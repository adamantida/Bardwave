//! Buses, ducking and loudness normalization through the Director.
//!
//! The Director drives a real [`AudioMixer`] (through a small `Playback`
//! adapter), so every assertion is about samples that came out of the mix, not
//! about calls that were recorded. No sound card, no real config, no shared
//! port: the HTTP test binds port 0.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use soundshared::audio::director::mock::MockPlayback;
use soundshared::audio::director_audio::{apply_gain_updates, level_for, PLAYLIST_BUS};
use soundshared::audio::loudness;
use soundshared::audio::{
    AudioEngine, AudioMixer, Bus as MixerBus, CatalogSound, DecodedAudio, Director, DuckMode,
    DuckSettings, DuckTarget, EngineError, FadeSpec, MixControl, Playback, SoundId,
};
use soundshared::config::{AudioConfig, Bus, LibraryConfig, SoundConfigEntry};
use soundshared::network::control::{router, Board, BoardState, ControlAction, ControlCommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const RATE: u32 = 48_000;

fn mixer_bus(bus: Bus) -> MixerBus {
    match bus {
        Bus::Music => MixerBus::Music,
        Bus::Ambience => MixerBus::Ambience,
        Bus::Sfx => MixerBus::Sfx,
    }
}

/// A `Playback` on a real mixer: one looping track per sound.
struct MixerRig {
    mixer: AudioMixer,
    tracks: Mutex<HashMap<SoundId, Uuid>>,
    levels: Mutex<HashMap<SoundId, f32>>,
}

impl MixerRig {
    fn new() -> Self {
        Self {
            mixer: AudioMixer::new(),
            tracks: Mutex::new(HashMap::new()),
            levels: Mutex::new(HashMap::new()),
        }
    }

    fn add_audio(&self, audio: DecodedAudio) -> SoundId {
        let id = SoundId::new();
        let track = self.mixer.add_track(Arc::new(audio), true);
        self.tracks.lock().insert(id, track);
        self.levels.lock().insert(id, 1.0);
        id
    }

    /// A sound that is a constant `value`, so the mix output is its gain.
    fn add_dc(&self, value: f32) -> SoundId {
        self.add_audio(DecodedAudio::new(vec![value; RATE as usize * 2], RATE, 2))
    }

    fn track(&self, sound: SoundId) -> Result<Uuid, EngineError> {
        self.tracks
            .lock()
            .get(&sound)
            .copied()
            .ok_or(EngineError::SoundNotFound(sound))
    }

    /// Mix `frames` frames in device-sized blocks (the bus gains ramp once per
    /// block, as in the real callback) and return the interleaved buffer.
    fn render(&self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        for block in out.chunks_mut(480 * 2) {
            self.mixer.mix_into(block);
        }
        out
    }

    /// Let `ms` of audio go by and return the level at the end (left sample).
    fn settle(&self, ms: usize) -> f32 {
        let out = self.render(RATE as usize * ms / 1000);
        out[out.len() - 2]
    }
}

type R = Result<(), EngineError>;

impl Playback for MixerRig {
    fn playing_sounds(&self) -> Vec<SoundId> {
        let tracks = self.tracks.lock();
        tracks
            .iter()
            .filter(|(_, t)| self.mixer.is_track_playing(**t).unwrap_or(false))
            .map(|(s, _)| *s)
            .collect()
    }
    fn is_playing(&self, sound: SoundId) -> bool {
        self.track(sound)
            .and_then(|t| Ok(self.mixer.is_track_playing(t)?))
            .unwrap_or(false)
    }
    fn volume(&self, sound: SoundId) -> Option<f32> {
        self.levels.lock().get(&sound).copied()
    }
    fn is_looped(&self, sound: SoundId) -> bool {
        self.track(sound)
            .and_then(|t| Ok(self.mixer.is_track_looped(t)?))
            .unwrap_or(false)
    }
    fn set_volume(&self, sound: SoundId, volume: f32) -> R {
        let track = self.track(sound)?;
        self.mixer.set_track_volume(track, volume)?;
        self.levels.lock().insert(sound, volume);
        Ok(())
    }
    fn scale_volume(&self, sound: SoundId, ratio: f32) -> R {
        let track = self.track(sound)?;
        self.mixer.scale_track_volume(track, ratio)?;
        let mut levels = self.levels.lock();
        let level = levels.entry(sound).or_insert(1.0);
        *level = (*level * ratio).clamp(0.0, 1.0);
        Ok(())
    }
    fn set_looped(&self, sound: SoundId, looped: bool) -> R {
        Ok(self.mixer.set_track_looped(self.track(sound)?, looped)?)
    }
    fn set_pan(&self, sound: SoundId, pan: f32) -> R {
        Ok(self.mixer.set_track_pan(self.track(sound)?, pan)?)
    }
    fn set_bus(&self, sound: SoundId, bus: Bus) -> R {
        Ok(self
            .mixer
            .set_track_bus(self.track(sound)?, mixer_bus(bus))?)
    }
    fn play(&self, sound: SoundId, looped: bool, volume: Option<f32>, _fade: FadeSpec) -> R {
        if let Some(volume) = volume {
            self.set_volume(sound, volume)?;
        }
        let track = self.track(sound)?;
        self.mixer.set_track_looped(track, looped)?;
        Ok(self.mixer.play_track(track)?)
    }
    fn play_one_shot(&self, sound: SoundId, volume: f32) -> R {
        if !self.is_playing(sound) {
            self.set_volume(sound, volume)?;
        }
        Ok(self.mixer.play_track(self.track(sound)?)?)
    }
    fn stop(&self, sound: SoundId, _fade: FadeSpec) -> R {
        Ok(self.mixer.stop_track(self.track(sound)?)?)
    }
    fn fade_to(&self, sound: SoundId, volume: f32, _fade: FadeSpec) -> R {
        self.set_volume(sound, volume)
    }
    fn stop_all(&self, fade: FadeSpec) -> R {
        for sound in self.playing_sounds() {
            self.stop(sound, fade)?;
        }
        Ok(())
    }
    fn master_volume(&self) -> f32 {
        self.mixer.master_volume()
    }
    fn set_master_volume(&self, volume: f32) -> R {
        Ok(self.mixer.set_master_volume(volume)?)
    }
    fn duck_bus(&self, bus: Bus, settings: DuckSettings) {
        self.mixer.duck_bus(mixer_bus(bus), settings);
    }
    fn release_duck(&self, bus: Bus) {
        self.mixer.release_duck(mixer_bus(bus));
    }
    fn set_bus_volume(&self, bus: Bus, volume: f32) -> R {
        Ok(self.mixer.set_bus_volume(mixer_bus(bus), volume)?)
    }
}

/// Director with the library sounds registered (volume 1.0).
fn director_with(sounds: &[(&str, SoundId, Bus)], audio: &AudioConfig) -> Director {
    let mut director = Director::new();
    let catalog = sounds
        .iter()
        .map(|(id, sound, bus)| {
            let mut entry = CatalogSound::new(*id, *sound);
            entry.bus = *bus;
            entry.looped = true;
            entry
        })
        .collect();
    director.sync_library(Vec::new(), catalog);
    director.configure_audio(audio);
    director
}

fn quick_audio() -> AudioConfig {
    AudioConfig {
        duck_amount: 0.3,
        duck_attack_ms: 20,
        duck_release_ms: 40,
        ..Default::default()
    }
}

fn approx(a: f32, b: f32, tol: f32) {
    assert!((a - b).abs() <= tol, "{a} != {b} (±{tol})");
}

// ------------------------------------------------------------- ducking ----

#[test]
fn ducking_returns_exactly_the_original_volume() {
    let rig = MixerRig::new();
    let music = rig.add_dc(0.4);
    let mut director = director_with(&[("m", music, Bus::Music)], &quick_audio());
    director.set_bus_volume(&rig, Bus::Music, 0.5).unwrap();
    director.play_sound(&rig, music, true).unwrap();
    approx(rig.settle(100), 0.2, 1e-5);

    director
        .duck(&rig, DuckTarget::Bus(Bus::Music), 0.25, 50, 100)
        .unwrap();
    assert!(director.is_ducked(Bus::Music));
    approx(rig.settle(200), 0.05, 1e-5);
    // The fader was not touched by the duck.
    assert_eq!(director.bus_volume(Bus::Music), 0.5);

    director
        .duck_release(&rig, DuckTarget::Bus(Bus::Music))
        .unwrap();
    assert!(!director.is_ducked(Bus::Music));
    approx(rig.settle(300), 0.2, 1e-6);
    assert_eq!(rig.mixer.bus_gain(MixerBus::Music), 0.5);
}

#[test]
fn ducking_ramps_instead_of_jumping() {
    let rig = MixerRig::new();
    let music = rig.add_dc(0.4);
    let mut director = director_with(&[("m", music, Bus::Music)], &quick_audio());
    director.play_sound(&rig, music, true).unwrap();
    approx(rig.settle(50), 0.4, 1e-5);
    director
        .duck(&rig, DuckTarget::Bus(Bus::Music), 0.0, 100, 100)
        .unwrap();
    let out = rig.render(RATE as usize / 10); // 100 ms attack
    let mid = out[(out.len() / 2) & !1];
    assert!(mid > 0.05 && mid < 0.35, "mid-attack level {mid}");
    // The step between neighbouring frames stays tiny (no zipper noise).
    let max_step = out
        .chunks(2)
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).abs())
        .fold(0.0f32, f32::max);
    assert!(max_step < 0.01, "step {max_step}");
}

#[test]
fn hold_mode_ducks_while_held_and_releases_on_key_up() {
    let rig = MixerRig::new();
    let music = rig.add_dc(0.4);
    let mut director = director_with(&[("m", music, Bus::Music)], &quick_audio());
    director.play_sound(&rig, music, true).unwrap();
    approx(rig.settle(50), 0.4, 1e-5);

    director.duck_start(&rig, DuckTarget::Configured).unwrap();
    approx(rig.settle(100), 0.12, 1e-5);
    // As long as the key is held the duck stays, however long that is.
    approx(rig.settle(3000), 0.12, 1e-5);
    // A repeated key-down (key repeat) changes nothing.
    director.duck_start(&rig, DuckTarget::Configured).unwrap();
    approx(rig.settle(100), 0.12, 1e-5);

    director.duck_end(&rig, DuckTarget::Configured).unwrap();
    approx(rig.settle(200), 0.4, 1e-6);
    assert!(!director.is_ducked(Bus::Music));

    // A key-up without a key-down is ignored.
    director.duck_end(&rig, DuckTarget::Configured).unwrap();
    approx(rig.settle(50), 0.4, 1e-6);
}

#[test]
fn toggle_and_hold_do_not_cancel_each_other() {
    let rig = MixerRig::new();
    let music = rig.add_dc(0.4);
    let mut director = director_with(&[("m", music, Bus::Music)], &quick_audio());
    director.play_sound(&rig, music, true).unwrap();

    assert!(director.duck_toggle(&rig, DuckTarget::Configured).unwrap());
    approx(rig.settle(200), 0.12, 1e-5);

    // Hold and let go while the toggle is on: still ducked.
    director.duck_start(&rig, DuckTarget::Configured).unwrap();
    director.duck_end(&rig, DuckTarget::Configured).unwrap();
    approx(rig.settle(200), 0.12, 1e-5);

    // Toggle off while a key is held: stays down until the key goes up.
    director.duck_start(&rig, DuckTarget::Configured).unwrap();
    assert!(!director.duck_toggle(&rig, DuckTarget::Configured).unwrap());
    approx(rig.settle(200), 0.12, 1e-5);
    director.duck_end(&rig, DuckTarget::Configured).unwrap();
    approx(rig.settle(200), 0.4, 1e-6);
}

#[test]
fn buses_are_independent() {
    let rig = MixerRig::new();
    let music = rig.add_dc(0.1);
    let amb = rig.add_dc(0.1);
    let sfx = rig.add_dc(0.1);
    let mut director = director_with(
        &[
            ("m", music, Bus::Music),
            ("a", amb, Bus::Ambience),
            ("s", sfx, Bus::Sfx),
        ],
        &quick_audio(),
    );
    for sound in [music, amb, sfx] {
        director.play_sound(&rig, sound, true).unwrap();
    }
    approx(rig.settle(100), 0.3, 1e-5);

    director.set_bus_volume(&rig, Bus::Music, 0.5).unwrap();
    approx(rig.settle(100), 0.25, 1e-5);
    assert_eq!(rig.mixer.bus_volume(MixerBus::Ambience), 1.0);
    assert_eq!(rig.mixer.bus_volume(MixerBus::Sfx), 1.0);

    director
        .duck(&rig, DuckTarget::Bus(Bus::Sfx), 0.0, 20, 20)
        .unwrap();
    approx(rig.settle(100), 0.15, 1e-5);
    assert!(director.is_ducked(Bus::Sfx));
    assert!(!director.is_ducked(Bus::Music));
    assert!(!director.is_ducked(Bus::Ambience));
    approx(rig.mixer.bus_gain(MixerBus::Music), 0.5, 1e-6);
    approx(rig.mixer.bus_gain(MixerBus::Ambience), 1.0, 1e-6);

    director.set_bus_volume(&rig, Bus::Ambience, 0.0).unwrap();
    approx(rig.settle(100), 0.05, 1e-5);
    assert_eq!(director.bus_volume(Bus::Music), 0.5);
    assert_eq!(director.bus_volume(Bus::Ambience), 0.0);
    assert_eq!(director.bus_volume(Bus::Sfx), 1.0);
}

#[test]
fn configured_ducking_takes_levels_and_buses_from_audio_config() {
    let rig = MixerRig::new();
    let music = rig.add_dc(0.1);
    let amb = rig.add_dc(0.1);
    let sfx = rig.add_dc(0.1);
    let audio = AudioConfig {
        duck_amount: 0.4,
        duck_attack_ms: 10,
        duck_release_ms: 10,
        duck_buses: vec![Bus::Music, Bus::Sfx],
        ..Default::default()
    };
    let mut director = director_with(
        &[
            ("m", music, Bus::Music),
            ("a", amb, Bus::Ambience),
            ("s", sfx, Bus::Sfx),
        ],
        &audio,
    );
    for sound in [music, amb, sfx] {
        director.play_sound(&rig, sound, true).unwrap();
    }
    director
        .run_mix_control(
            &rig,
            MixControl::Duck {
                target: DuckTarget::Configured,
                mode: DuckMode::On,
                amount: None,
                attack_ms: None,
                release_ms: None,
            },
        )
        .unwrap();
    // music and sfx at 0.4, ambience untouched.
    approx(rig.settle(100), 0.1 * 0.4 + 0.1 + 0.1 * 0.4, 1e-5);

    // An explicit amount and the "all buses" target override the defaults.
    director
        .run_mix_control(
            &rig,
            MixControl::Duck {
                target: DuckTarget::All,
                mode: DuckMode::On,
                amount: Some(0.5),
                attack_ms: Some(10),
                release_ms: Some(10),
            },
        )
        .unwrap();
    approx(rig.settle(100), 0.15, 1e-5);
    director
        .run_mix_control(
            &rig,
            MixControl::Duck {
                target: DuckTarget::All,
                mode: DuckMode::Off,
                amount: None,
                attack_ms: None,
                release_ms: None,
            },
        )
        .unwrap();
    approx(rig.settle(100), 0.3, 1e-6);
}

#[test]
fn bad_values_are_refused() {
    let rig = MixerRig::new();
    let mut director = Director::new();
    assert!(director.set_bus_volume(&rig, Bus::Music, 1.5).is_err());
    assert!(director.set_bus_volume(&rig, Bus::Music, f32::NAN).is_err());
    assert!(director.duck(&rig, DuckTarget::All, -0.1, 10, 10).is_err());
    assert!(director
        .duck(&rig, DuckTarget::All, f32::NAN, 10, 10)
        .is_err());
    assert_eq!(director.bus_volume(Bus::Music), 1.0);
    assert!(!director.is_ducked(Bus::Music));
}

#[test]
fn playlist_sounds_route_to_the_music_bus() {
    let rig = MixerRig::new();
    let track = rig.add_dc(0.2);
    let director = Director::new();
    director.route_music_layer(&rig, track).unwrap();
    assert_eq!(PLAYLIST_BUS, Bus::Music);
    assert_eq!(
        rig.mixer.track_bus(rig.track(track).unwrap()).unwrap(),
        MixerBus::Music
    );
}

#[test]
fn a_sound_plays_on_the_bus_of_its_library_entry() {
    let rig = MixerRig::new();
    let door = rig.add_dc(0.2);
    let mut director = director_with(&[("door", door, Bus::Sfx)], &quick_audio());
    director.play_sound(&rig, door, false).unwrap();
    assert_eq!(
        rig.mixer.track_bus(rig.track(door).unwrap()).unwrap(),
        MixerBus::Sfx
    );
}

// --------------------------------------------------------- normalization ----

fn sine(amplitude: f32, secs: f32) -> DecodedAudio {
    let frames = (RATE as f32 * secs) as usize;
    let mut samples = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let v = amplitude * (TAU * 1000.0 * i as f32 / RATE as f32).sin();
        samples.push(v);
        samples.push(v);
    }
    DecodedAudio::new(samples, RATE, 2)
}

fn wait_for_measurements(director: &mut Director, rig: &impl Playback) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while director.measuring_count() > 0 {
        assert!(Instant::now() < deadline, "measurement did not finish");
        director.tick(rig);
        std::thread::sleep(Duration::from_millis(5));
    }
    director.tick(rig);
}

fn rendered_lufs(rig: &MixerRig, sound: SoundId, director: &Director) -> f32 {
    // Play only this sound.
    for other in rig.playing_sounds() {
        rig.stop(other, FadeSpec::NONE).unwrap();
    }
    rig.render(480);
    // Same level as the real chain: volume x gain from the Director's catalog.
    let entry = director.catalog_sound(sound).unwrap().clone();
    rig.set_volume(sound, entry.level(entry.volume)).unwrap();
    rig.play(sound, true, None, FadeSpec::NONE).unwrap();
    let out = rig.render(RATE as usize * 2);
    loudness::measure_lufs(&DecodedAudio::new(out, RATE, 2)).expect("measurable")
}

#[test]
fn normalization_brings_two_files_to_the_target() {
    let target = -28.0;
    let rig = MixerRig::new();
    let loud_audio = sine(0.5, 2.0);
    let quiet_audio = sine(0.05, 2.0);
    let loud = rig.add_audio(loud_audio.clone());
    let quiet = rig.add_audio(quiet_audio.clone());
    let audio = AudioConfig {
        normalize: true,
        normalize_target_lufs: target,
        ..Default::default()
    };
    let mut director = director_with(
        &[("loud", loud, Bus::Music), ("quiet", quiet, Bus::Music)],
        &audio,
    );

    // The two files really differ by far more than the tolerance.
    let raw_loud = loudness::measure_lufs(&loud_audio).unwrap();
    let raw_quiet = loudness::measure_lufs(&quiet_audio).unwrap();
    assert!(raw_loud - raw_quiet > 15.0);

    assert!(director.needs_normalization(loud));
    assert!(director.start_normalization("loud", Arc::new(loud_audio)));
    assert!(director.start_normalization("quiet", Arc::new(quiet_audio)));
    // Already queued: not started twice.
    assert!(!director.start_normalization("loud", Arc::new(sine(0.5, 1.0))));
    wait_for_measurements(&mut director, &rig);

    let updates = director.take_gain_updates();
    assert_eq!(updates.len(), 2, "{updates:?}");
    assert!(director.take_gain_updates().is_empty());
    for update in &updates {
        assert!(update.gain_db < 0.0 && update.gain_db >= loudness::MIN_GAIN_DB);
    }

    // Through the real chain (user volume x gain, mixer) both land on the target.
    let l = rendered_lufs(&rig, loud, &director);
    let q = rendered_lufs(&rig, quiet, &director);
    approx(l, target, 1.0);
    approx(q, target, 1.0);
    assert!((l - q).abs() < 1.0);

    // Measured once: not asked for again, and the user's volume is untouched.
    assert!(!director.needs_normalization(loud));
    assert_eq!(director.catalog_sound(loud).unwrap().volume, 1.0);

    // The gains go into a library config the way the headless server stores them.
    let mut library = LibraryConfig::default();
    for id in ["loud", "quiet"] {
        library.sounds.push(SoundConfigEntry {
            id: id.into(),
            ..Default::default()
        });
    }
    assert!(apply_gain_updates(&mut library, &updates));
    assert!(library.sounds.iter().all(|s| s.gain_db < 0.0));
}

#[test]
fn nothing_changes_while_normalization_is_off() {
    let rig = MixerRig::new();
    let sound = rig.add_audio(sine(0.5, 1.0));
    let mut director = director_with(&[("s", sound, Bus::Ambience)], &AudioConfig::default());
    assert!(!director.normalization().enabled);
    assert!(!director.needs_normalization(sound));

    // Even a measurement that was started by hand is not applied.
    assert!(director.start_normalization("s", Arc::new(sine(0.5, 1.0))));
    wait_for_measurements(&mut director, &rig);
    assert!(director.take_gain_updates().is_empty());
    let entry = director.catalog_sound(sound).unwrap();
    assert_eq!(entry.gain_db, 0.0);
    assert_eq!(entry.level(0.7), 0.7);

    director.set_sound_volume(&rig, sound, 0.7).unwrap();
    assert_eq!(rig.volume(sound), Some(0.7));
    director.play_sound(&rig, sound, true).unwrap();
    assert_eq!(rig.volume(sound), Some(0.7));
}

#[test]
fn engine_sounds_are_measured_only_when_enabled_and_unmeasured() {
    let engine = AudioEngine::new().unwrap();
    let sound = engine.load_sound_from_data(sine(0.5, 1.5));
    let mut director = Director::new();
    director.sync_library(Vec::new(), vec![CatalogSound::new("s", sound)]);

    // Off by default: nothing starts, the engine level stays.
    assert!(!director.normalize_sound(&engine, sound));
    assert_eq!(director.normalize_catalog(&engine), 0);
    assert_eq!(director.measuring_count(), 0);
    assert_eq!(engine.sound_volume(sound).unwrap(), 1.0);

    // On: the sound is measured in the background and the engine follows.
    director.configure_audio(&AudioConfig {
        normalize: true,
        normalize_target_lufs: -20.0,
        ..Default::default()
    });
    assert_eq!(director.normalize_catalog(&engine), 1);
    assert_eq!(director.normalize_catalog(&engine), 0, "already running");
    let deadline = Instant::now() + Duration::from_secs(20);
    while director.measuring_count() > 0 {
        assert!(Instant::now() < deadline);
        director.tick(&engine);
        std::thread::sleep(Duration::from_millis(5));
    }
    director.tick(&engine);
    let updates = director.take_gain_updates();
    assert_eq!(updates.len(), 1);
    let gain = updates[0].gain_db;
    assert!(gain < 0.0);
    approx(
        engine.sound_volume(sound).unwrap(),
        level_for(1.0, gain),
        1e-6,
    );
    assert_eq!(director.catalog_sound(sound).unwrap().volume, 1.0);

    // A sound that already has a gain is left alone.
    let other = engine.load_sound_from_data(sine(0.5, 1.5));
    let mut entry = CatalogSound::new("o", other);
    entry.gain_db = -3.0;
    director.register_sound(entry);
    assert!(!director.needs_normalization(other));
    assert!(!director.normalize_sound(&engine, other));
}

#[test]
fn user_volume_and_gain_multiply() {
    let rig = MixerRig::new();
    let sound = rig.add_dc(0.4);
    let mut director = director_with(&[("s", sound, Bus::Ambience)], &quick_audio());
    let gain = 10f32.powf(-6.0 / 20.0);

    assert!(director.set_sound_gain_db(&rig, "s", -6.0));
    assert_eq!(director.catalog_sound(sound).unwrap().gain_db, -6.0);
    approx(rig.volume(sound).unwrap(), gain, 1e-6);

    // The slider moves, the gain stays: the two multiply.
    director.set_sound_volume(&rig, sound, 0.5).unwrap();
    approx(rig.volume(sound).unwrap(), 0.5 * gain, 1e-6);
    director.play_sound(&rig, sound, true).unwrap();
    approx(rig.settle(100), 0.4 * 0.5 * gain, 1e-5);

    // The user's own volume is what the library keeps.
    assert_eq!(director.catalog_sound(sound).unwrap().volume, 0.5);

    // A gain change while the sound plays rescales its current level.
    director.set_sound_gain_db(&rig, "s", -12.0);
    approx(
        rig.volume(sound).unwrap(),
        0.5 * 10f32.powf(-12.0 / 20.0),
        1e-6,
    );

    // A boost is audible below full volume and capped at 1.0 (the engine range).
    director.set_sound_gain_db(&rig, "s", 6.0);
    director.set_sound_volume(&rig, sound, 0.25).unwrap();
    approx(
        rig.volume(sound).unwrap(),
        0.25 * 10f32.powf(6.0 / 20.0),
        1e-6,
    );
    director.set_sound_volume(&rig, sound, 1.0).unwrap();
    assert_eq!(rig.volume(sound), Some(1.0));

    assert!(!director.set_sound_gain_db(&rig, "nope", -3.0));
}

#[test]
fn a_measured_gain_does_not_reset_a_fade_in_progress() {
    let rig = MixerRig::new();
    let sound = rig.add_dc(0.5);
    let mut director = director_with(&[("s", sound, Bus::Ambience)], &quick_audio());
    director.play_sound(&rig, sound, true).unwrap();
    rig.settle(50);

    // A one second fade from full level down to 0.2 is running ...
    let track = rig.track(sound).unwrap();
    rig.mixer
        .fade_track_to(track, 0.2, 1000, soundshared::audio::FadeCurve::Linear)
        .unwrap();
    rig.render(RATE as usize / 4);
    let before = rig.mixer.track_gain(track).unwrap();
    assert!(before > 0.3 && before < 0.9, "mid-fade gain {before}");

    // ... when the loudness measurement comes back with -6 dB.
    assert!(director.set_sound_gain_db(&rig, "s", -6.0));
    assert!(
        rig.mixer.is_track_fading(track).unwrap(),
        "the fade must keep running"
    );
    let out = rig.render(RATE as usize / 8);
    let first = out[0] / 0.5;
    assert!(
        (first - before).abs() < 0.02,
        "no jump at the hand-over: {first} vs {before}"
    );
    // The fade still ends where it was heading, scaled by the gain.
    rig.render(RATE as usize);
    let end = rig.mixer.track_gain(track).unwrap();
    approx(end, 0.2 * 10f32.powf(-6.0 / 20.0), 1e-4);
    assert!(!rig.mixer.is_track_fading(track).unwrap());
}

#[test]
fn apply_sound_level_pushes_volume_times_gain() {
    let pb = MockPlayback::new();
    let sound = pb.add_sound();
    let mut director = Director::new();
    let mut entry = CatalogSound::new("s", sound);
    entry.volume = 0.8;
    entry.gain_db = -6.0;
    director.register_sound(entry.clone());
    director.apply_sound_level(&pb, sound).unwrap();
    approx(pb.volume(sound).unwrap(), entry.level(0.8), 1e-6);
    // An unknown sound is not an error.
    director.apply_sound_level(&pb, SoundId::new()).unwrap();
}

// -------------------------------------------------------------- control ----

#[test]
fn board_runs_bus_and_duck_actions() {
    let rig = MixerRig::new();
    let music = rig.add_dc(0.4);
    let library = LibraryConfig {
        sounds: vec![SoundConfigEntry {
            id: "m".into(),
            path: "/fake/m.wav".into(),
            bus: Bus::Music,
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut board = Board::assemble(&library, &[(0, music)]);
    board.configure_audio(&quick_audio());
    board.handle(&rig, ControlAction::Play(0)).unwrap();
    approx(rig.settle(50), 0.4, 1e-5);

    board
        .handle(
            &rig,
            ControlAction::Mix(MixControl::SetBusVolume(Bus::Music, 0.5)),
        )
        .unwrap();
    approx(rig.settle(50), 0.2, 1e-5);
    assert!(board
        .handle(
            &rig,
            ControlAction::Mix(MixControl::SetBusVolume(Bus::Music, 3.0))
        )
        .is_err());

    let duck = |mode| {
        ControlAction::Mix(MixControl::Duck {
            target: DuckTarget::Configured,
            mode,
            amount: None,
            attack_ms: None,
            release_ms: None,
        })
    };
    board.handle(&rig, duck(DuckMode::Toggle)).unwrap();
    approx(rig.settle(100), 0.06, 1e-5);
    board.handle(&rig, duck(DuckMode::Toggle)).unwrap();
    approx(rig.settle(200), 0.2, 1e-6);
    board.handle(&rig, duck(DuckMode::HoldStart)).unwrap();
    approx(rig.settle(100), 0.06, 1e-5);
    board.handle(&rig, duck(DuckMode::HoldEnd)).unwrap();
    approx(rig.settle(200), 0.2, 1e-6);
}

struct Reply {
    status: u16,
    body: String,
}

async fn post(addr: SocketAddr, path: &str, header: bool) -> Reply {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let extra = if header {
        "X-Bardwave-Control: 1\r\n"
    } else {
        ""
    };
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("response");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status");
    let body = match (body.find(['{', '[']), body.rfind(['}', ']'])) {
        (Some(a), Some(b)) if b >= a => body[a..=b].to_string(),
        _ => body.to_string(),
    };
    Reply { status, body }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_api_sets_bus_volume_and_ducks() {
    let rig = Arc::new(MixerRig::new());
    let music = rig.add_dc(0.4);
    let library = LibraryConfig {
        sounds: vec![SoundConfigEntry {
            id: "m".into(),
            path: "/fake/m.wav".into(),
            bus: Bus::Music,
            ..Default::default()
        }],
        ..Default::default()
    };

    let (tx, rx) = crossbeam_channel::unbounded::<ControlCommand>();
    let pump_rig = rig.clone();
    let pump = std::thread::spawn(move || {
        let mut board = Board::assemble(&library, &[(0, music)]);
        board.configure_audio(&quick_audio());
        while let Ok(command) = rx.recv_timeout(Duration::from_secs(10)) {
            let result = board.handle(&*pump_rig, command.action);
            let _ = command.reply.send(result);
        }
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(tx);
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    // The control header is required.
    let reply = post(addr, "/api/control/bus?bus=music&value=0.5", false).await;
    assert_eq!(reply.status, 403);
    assert_eq!(rig.mixer.bus_volume(MixerBus::Music), 1.0);

    let reply = post(addr, "/api/control/bus?bus=music&value=0.5", true).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    serde_json::from_str::<BoardState>(&reply.body).unwrap();
    assert_eq!(rig.mixer.bus_volume(MixerBus::Music), 0.5);

    let reply = post(addr, "/api/control/bus?bus=voice&value=0.5", true).await;
    assert_eq!(reply.status, 400);
    let reply = post(addr, "/api/control/bus?bus=sfx&value=2", true).await;
    assert_eq!(reply.status, 400);

    let reply = post(
        addr,
        "/api/control/duck?mode=on&bus=music&amount=0.25",
        true,
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(rig.mixer.is_bus_ducked(MixerBus::Music));
    let reply = post(addr, "/api/control/duck?mode=off&bus=music", true).await;
    assert_eq!(reply.status, 200);
    let reply = post(addr, "/api/control/duck?mode=start", true).await;
    assert_eq!(reply.status, 200);
    assert!(rig.mixer.is_bus_ducked(MixerBus::Music));
    let reply = post(addr, "/api/control/duck?mode=end", true).await;
    assert_eq!(reply.status, 200);
    let reply = post(addr, "/api/control/duck?mode=sideways", true).await;
    assert_eq!(reply.status, 400);
    let reply = post(addr, "/api/control/duck?bus=nowhere", true).await;
    assert_eq!(reply.status, 400);

    drop(pump);
}
