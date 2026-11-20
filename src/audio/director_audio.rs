//! Buses, ducking and loudness normalization of the [`Director`].
//!
//! This file is an `impl Director` extension: the state lives in
//! [`AudioControl`] (one private field of the Director), the operations are
//! methods of the Director, so `director.rs` stays small.
//!
//! # Buses and ducking
//!
//! Every sound plays on a bus ([`Bus`]: music / ambience / sfx), taken from
//! [`CatalogSound::bus`] when it starts. A bus has its own volume
//! ([`Director::set_bus_volume`]) and can be ducked: turned down to a level for
//! a while and brought back, with smooth ramps done by the mixer.
//!
//! * [`Director::duck`] ducks until [`Director::duck_release`] (a latch).
//! * [`Director::duck_start`] / [`Director::duck_end`] are the hold mode: the
//!   bus is down while a key is held.
//! * [`Director::duck_toggle`] flips the latch (press once / press again).
//!
//! A bus is down while it is latched OR held, so a toggle and a held key do not
//! cancel each other. The defaults (level, attack, release, which buses) come
//! from `[audio] duck_*` ([`Director::configure_audio`]). The "amount" is the
//! LEVEL the bus is lowered to (`0.3` is about -10 dB, `1.0` no ducking), the
//! same meaning as `audio.duck_amount`. The duck never touches the bus volume:
//! after the release the bus is back at exactly its fader level.
//!
//! # Loudness normalization
//!
//! [`CatalogSound::gain_db`] is the fixed correction of a sound. The engine
//! level of a sound is `volume * 10^(gain_db / 20)` (clamped to `0.0..=1.0`,
//! see [`CatalogSound::level`]): the user's volume is never rewritten, the gain
//! is a separate multiplier, and the two are multiplied in one place.
//!
//! With `audio.normalize` on, a loaded sound whose `gain_db` is `0.0` (not
//! measured) is measured in the background ([`Director::normalize_sound`]): one
//! worker thread decodes nothing (it measures the PCM the engine already
//! holds), so neither the GUI nor the audio callback waits for it. The result
//! comes back through [`Director::poll_normalization`] (called from
//! [`Director::tick`]): the catalog gain and the sound's engine level are
//! updated, and a [`GainUpdate`] is queued for the owner of the library
//! ([`Director::take_gain_updates`]) to store in the library / config
//! ([`apply_gain_updates`] does that for a [`LibraryConfig`]).
//!
//! Limits, on purpose: sounds that are streamed from disk (longer than the
//! streaming threshold) are not measured; the engine volume is capped at
//! `1.0`, so a positive gain is only audible when the user's volume is below
//! `1.0` (the typical correction of loud material is negative); a gain that
//! already is in the library keeps being applied when normalization is turned
//! off (it is the sound's own setting then).

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::audio::decoder::DecodedAudio;
use crate::audio::director::{CatalogSound, Director, DirectorError, Playback, PlaybackResult};
use crate::audio::engine::{AudioEngine, SoundId};
use crate::audio::loudness::{self, LoudnessInfo, MAX_GAIN_DB, MIN_GAIN_DB};
use crate::audio::mixer::DuckSettings;
use crate::config::{AudioConfig, Bus, LibraryConfig};

/// Every bus, in the order of [`bus_index`].
pub const BUSES: [Bus; 3] = [Bus::Music, Bus::Ambience, Bus::Sfx];

/// The bus a playlist (the music layer of a scene) plays on unless the
/// playlist says otherwise.
pub const PLAYLIST_BUS: Bus = Bus::Music;

fn bus_index(bus: Bus) -> usize {
    match bus {
        Bus::Music => 0,
        Bus::Ambience => 1,
        Bus::Sfx => 2,
    }
}

/// Lower-case name of a bus (`music`, `ambience`, `sfx`), as used by the
/// control API.
pub fn bus_name(bus: Bus) -> &'static str {
    match bus {
        Bus::Music => "music",
        Bus::Ambience => "ambience",
        Bus::Sfx => "sfx",
    }
}

/// Bus by name (case-insensitive), `None` when unknown.
pub fn parse_bus(name: &str) -> Option<Bus> {
    match name.trim().to_ascii_lowercase().as_str() {
        "music" => Some(Bus::Music),
        "ambience" | "ambient" => Some(Bus::Ambience),
        "sfx" | "fx" => Some(Bus::Sfx),
        _ => None,
    }
}

/// The engine level for a user volume and a fixed gain: what
/// [`CatalogSound::level`] computes, for callers that have no catalog entry
/// (the console pushes it into the engine when a file has been decoded).
pub fn level_for(volume: f32, gain_db: f32) -> f32 {
    let volume = if volume.is_finite() { volume } else { 1.0 };
    let gain_db = if gain_db.is_finite() { gain_db } else { 0.0 };
    (volume.clamp(0.0, 1.0) * 10f32.powf(gain_db / 20.0)).clamp(0.0, 1.0)
}

/// Which buses a duck acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DuckTarget {
    /// The buses of `audio.duck_buses` (music and ambience by default).
    #[default]
    Configured,
    /// Every bus.
    All,
    /// One bus.
    Bus(Bus),
}

impl DuckTarget {
    /// `default` / `configured` (or empty), `all`, or a bus name.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "default" | "configured" => Some(Self::Configured),
            "all" => Some(Self::All),
            other => parse_bus(other).map(Self::Bus),
        }
    }
}

/// How a duck request behaves (the `mode` of `/api/control/duck`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuckMode {
    /// Duck until released.
    On,
    /// Release the latched duck.
    Off,
    /// Latch on when off, release when on.
    Toggle,
    /// Hold mode, key down.
    HoldStart,
    /// Hold mode, key up.
    HoldEnd,
}

impl DuckMode {
    /// `on`, `off`, `toggle`, `start` (`hold`), `end` (`release_hold`).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "on" | "duck" => Some(Self::On),
            "off" | "release" => Some(Self::Off),
            "toggle" => Some(Self::Toggle),
            "start" | "hold" | "down" => Some(Self::HoldStart),
            "end" | "up" => Some(Self::HoldEnd),
            _ => None,
        }
    }
}

/// One bus / ducking request, the common currency of the console
/// (`AudioCommand`), the control API (`ControlAction`) and hotkeys.
/// [`Director::run_mix_control`] runs it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MixControl {
    /// Volume of a bus, `0.0..=1.0`.
    SetBusVolume(Bus, f32),
    /// Duck. `None` parameters fall back to the `[audio] duck_*` defaults.
    Duck {
        target: DuckTarget,
        mode: DuckMode,
        amount: Option<f32>,
        attack_ms: Option<u32>,
        release_ms: Option<u32>,
    },
}

/// A finished measurement for the owner of the library to store.
#[derive(Debug, Clone, PartialEq)]
pub struct GainUpdate {
    /// Library id of the sound (`SoundConfigEntry::id`).
    pub library_id: String,
    /// The correction to store as `gain_db`.
    pub gain_db: f32,
    /// Measured integrated loudness, LUFS.
    pub lufs: f32,
}

/// Normalization switch and target, see [`Director::normalization`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizeSettings {
    pub enabled: bool,
    pub target_lufs: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct DuckSlot {
    /// Ducked by `duck` / `duck_toggle` until released.
    latched: bool,
    /// Ducked while a key is held.
    held: bool,
}

impl DuckSlot {
    fn active(&self) -> bool {
        self.latched || self.held
    }
}

/// A sound waiting for the measuring worker.
struct MeasureJob {
    library_id: String,
    audio: Arc<DecodedAudio>,
}

/// What the worker found out.
struct MeasureResult {
    library_id: String,
    info: Option<LoudnessInfo>,
}

/// State of buses, ducking and normalization inside a [`Director`].
pub(crate) struct AudioControl {
    duck_amount: f32,
    duck_attack_ms: u32,
    duck_release_ms: u32,
    duck_buses: Vec<Bus>,
    normalize: bool,
    target_lufs: f32,
    bus_volumes: [f32; 3],
    ducks: [DuckSlot; 3],
    /// Library ids handed to the worker and not answered yet.
    measuring: HashSet<String>,
    /// Library ids that were measured (or found unmeasurable) this session:
    /// not measured again until the target changes.
    attempted: HashSet<String>,
    jobs_tx: Option<Sender<MeasureJob>>,
    results_tx: Sender<MeasureResult>,
    results_rx: Receiver<MeasureResult>,
    completed: Vec<GainUpdate>,
}

impl Default for AudioControl {
    fn default() -> Self {
        let defaults = AudioConfig::default();
        let (results_tx, results_rx) = unbounded();
        Self {
            duck_amount: defaults.duck_amount,
            duck_attack_ms: defaults.duck_attack_ms,
            duck_release_ms: defaults.duck_release_ms,
            duck_buses: defaults.duck_buses.clone(),
            normalize: defaults.normalize,
            target_lufs: defaults.normalize_target_lufs,
            bus_volumes: [1.0; 3],
            ducks: [DuckSlot::default(); 3],
            measuring: HashSet::new(),
            attempted: HashSet::new(),
            jobs_tx: None,
            results_tx,
            results_rx,
            completed: Vec::new(),
        }
    }
}

impl AudioControl {
    fn default_settings(&self) -> DuckSettings {
        DuckSettings {
            target: self.duck_amount,
            attack_ms: self.duck_attack_ms,
            release_ms: self.duck_release_ms,
            hold_ms: None,
        }
    }

    fn targets(&self, target: DuckTarget) -> Vec<Bus> {
        match target {
            DuckTarget::Configured => self.duck_buses.clone(),
            DuckTarget::All => BUSES.to_vec(),
            DuckTarget::Bus(bus) => vec![bus],
        }
    }

    /// The single worker that measures sounds one at a time (a library of
    /// hundreds of sounds must not start hundreds of threads).
    fn worker(&mut self) -> Option<&Sender<MeasureJob>> {
        if self.jobs_tx.is_none() {
            let (jobs_tx, jobs_rx) = unbounded::<MeasureJob>();
            let results_tx = self.results_tx.clone();
            let spawned = thread::Builder::new()
                .name("normalize-measure".into())
                .spawn(move || {
                    // Ends when the Director (the sender) is dropped.
                    for job in jobs_rx {
                        let info = loudness::analyze(&job.audio);
                        if results_tx
                            .send(MeasureResult {
                                library_id: job.library_id,
                                info,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            if spawned.is_ok() {
                self.jobs_tx = Some(jobs_tx);
            }
        }
        self.jobs_tx.as_ref()
    }
}

fn invalid(what: &str, value: impl std::fmt::Display) -> DirectorError {
    DirectorError::Invalid(format!("{what} {value}"))
}

impl Director {
    // ---------------------------------------------------------- config ----

    /// Take the ducking defaults and the normalization switch from `[audio]`.
    /// Cheap and idempotent: call it whenever the configuration may have
    /// changed (every frame is fine). Nonsense values are replaced by the
    /// defaults.
    pub fn configure_audio(&mut self, audio: &AudioConfig) {
        let defaults = AudioConfig::default();
        let ctl = &mut self.audio_ctl;
        ctl.duck_amount = if audio.duck_amount.is_finite() {
            audio.duck_amount.clamp(0.0, 1.0)
        } else {
            defaults.duck_amount
        };
        ctl.duck_attack_ms = audio.duck_attack_ms;
        ctl.duck_release_ms = audio.duck_release_ms;
        ctl.duck_buses = audio.duck_buses.clone();
        let target = if audio.normalize_target_lufs.is_finite() {
            audio.normalize_target_lufs.clamp(-60.0, 0.0)
        } else {
            defaults.normalize_target_lufs
        };
        self.set_normalize(audio.normalize, target);
    }

    // ----------------------------------------------------------- buses ----

    /// Set the volume of a bus (`0.0..=1.0`). Independent of every other bus
    /// and of ducking.
    pub fn set_bus_volume<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        bus: Bus,
        volume: f32,
    ) -> Result<(), DirectorError> {
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            return Err(invalid("bus volume", volume));
        }
        pb.set_bus_volume(bus, volume)?;
        self.audio_ctl.bus_volumes[bus_index(bus)] = volume;
        Ok(())
    }

    /// The volume last set on a bus (1.0 before any change).
    pub fn bus_volume(&self, bus: Bus) -> f32 {
        self.audio_ctl.bus_volumes[bus_index(bus)]
    }

    /// Route a sound of the music layer (a playlist track) to the music bus.
    /// Playlists call it when they start a track, unless the playlist is
    /// routed elsewhere (`PlaylistConfig::bus`, default [`PLAYLIST_BUS`]).
    pub fn route_music_layer<P: Playback + ?Sized>(
        &self,
        pb: &P,
        sound: SoundId,
    ) -> PlaybackResult {
        pb.set_bus(sound, PLAYLIST_BUS)
    }

    // --------------------------------------------------------- ducking ----

    /// Duck the buses of `target` down to the level `amount` (`0.0..=1.0`,
    /// 1.0 = no ducking) over `attack_ms`, until [`duck_release`](Self::duck_release).
    pub fn duck<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        target: DuckTarget,
        amount: f32,
        attack_ms: u32,
        release_ms: u32,
    ) -> Result<(), DirectorError> {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return Err(invalid("duck amount", amount));
        }
        if amount >= 1.0 {
            return self.duck_release(pb, target);
        }
        let settings = DuckSettings {
            target: amount,
            attack_ms,
            release_ms,
            hold_ms: None,
        };
        for bus in self.audio_ctl.targets(target) {
            self.audio_ctl.ducks[bus_index(bus)].latched = true;
            pb.duck_bus(bus, settings);
        }
        Ok(())
    }

    /// Duck with the `[audio] duck_*` defaults until released.
    pub fn duck_default<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        target: DuckTarget,
    ) -> Result<(), DirectorError> {
        let s = self.audio_ctl.default_settings();
        self.duck(pb, target, s.target, s.attack_ms, s.release_ms)
    }

    /// Release the latched duck of `target`. A bus that is still held down
    /// stays ducked until [`duck_end`](Self::duck_end).
    pub fn duck_release<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        target: DuckTarget,
    ) -> Result<(), DirectorError> {
        for bus in self.audio_ctl.targets(target) {
            let slot = &mut self.audio_ctl.ducks[bus_index(bus)];
            slot.latched = false;
            if !slot.held {
                pb.release_duck(bus);
            }
        }
        Ok(())
    }

    /// Hold mode, key down: duck with the defaults for as long as the key is
    /// held. Pressing again while held changes nothing.
    pub fn duck_start<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        target: DuckTarget,
    ) -> Result<(), DirectorError> {
        let settings = self.audio_ctl.default_settings();
        for bus in self.audio_ctl.targets(target) {
            let slot = &mut self.audio_ctl.ducks[bus_index(bus)];
            if slot.held {
                continue;
            }
            slot.held = true;
            if !slot.latched {
                pb.duck_bus(bus, settings);
            }
        }
        Ok(())
    }

    /// Hold mode, key up: bring the buses back (unless a latched duck keeps
    /// them down). A key-up without a key-down does nothing.
    pub fn duck_end<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        target: DuckTarget,
    ) -> Result<(), DirectorError> {
        for bus in self.audio_ctl.targets(target) {
            let slot = &mut self.audio_ctl.ducks[bus_index(bus)];
            if !slot.held {
                continue;
            }
            slot.held = false;
            if !slot.latched {
                pb.release_duck(bus);
            }
        }
        Ok(())
    }

    /// Latch the duck on when no bus of `target` is latched, off otherwise.
    /// Returns whether the buses are now latched down.
    pub fn duck_toggle<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        target: DuckTarget,
    ) -> Result<bool, DirectorError> {
        let any_latched = self
            .audio_ctl
            .targets(target)
            .into_iter()
            .any(|bus| self.audio_ctl.ducks[bus_index(bus)].latched);
        if any_latched {
            self.duck_release(pb, target)?;
            Ok(false)
        } else {
            self.duck_default(pb, target)?;
            Ok(true)
        }
    }

    /// Whether a bus is ducked by a latch or a held key (it may still be on
    /// its way down or back up).
    pub fn is_ducked(&self, bus: Bus) -> bool {
        self.audio_ctl.ducks[bus_index(bus)].active()
    }

    /// Run a [`MixControl`].
    pub fn run_mix_control<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        control: MixControl,
    ) -> Result<(), DirectorError> {
        match control {
            MixControl::SetBusVolume(bus, volume) => self.set_bus_volume(pb, bus, volume),
            MixControl::Duck {
                target,
                mode,
                amount,
                attack_ms,
                release_ms,
            } => match mode {
                DuckMode::On => {
                    let d = self.audio_ctl.default_settings();
                    self.duck(
                        pb,
                        target,
                        amount.unwrap_or(d.target),
                        attack_ms.unwrap_or(d.attack_ms),
                        release_ms.unwrap_or(d.release_ms),
                    )
                }
                DuckMode::Off => self.duck_release(pb, target),
                DuckMode::Toggle => self.duck_toggle(pb, target).map(|_| ()),
                DuckMode::HoldStart => self.duck_start(pb, target),
                DuckMode::HoldEnd => self.duck_end(pb, target),
            },
        }
    }

    // --------------------------------------------------- normalization ----

    /// The normalization switch and target.
    pub fn normalization(&self) -> NormalizeSettings {
        NormalizeSettings {
            enabled: self.audio_ctl.normalize,
            target_lufs: self.audio_ctl.target_lufs,
        }
    }

    /// Switch normalization on or off and set its target (LUFS). A new target
    /// makes the sounds measured so far eligible again; it does not touch the
    /// gains already stored.
    pub fn set_normalize(&mut self, enabled: bool, target_lufs: f32) {
        let ctl = &mut self.audio_ctl;
        if (ctl.target_lufs - target_lufs).abs() > f32::EPSILON {
            ctl.attempted.clear();
        }
        ctl.normalize = enabled;
        ctl.target_lufs = target_lufs;
    }

    /// Whether the catalog sound would be measured now: normalization is on,
    /// its gain is not set (`0.0`) and no measurement is running or done.
    pub fn needs_normalization(&self, sound: SoundId) -> bool {
        let ctl = &self.audio_ctl;
        ctl.normalize
            && self.catalog_sound(sound).is_some_and(|entry| {
                entry.gain_db == 0.0
                    && !ctl.measuring.contains(&entry.id)
                    && !ctl.attempted.contains(&entry.id)
            })
    }

    /// Measure a loaded catalog sound in the background when
    /// [`needs_normalization`](Self::needs_normalization) says so. Returns
    /// whether a measurement was started. Streamed sounds (not held in memory)
    /// are skipped.
    pub fn normalize_sound(&mut self, engine: &AudioEngine, sound: SoundId) -> bool {
        if !self.needs_normalization(sound) {
            return false;
        }
        let Some(audio) = engine.sound_audio(sound) else {
            return false; // streamed or unknown
        };
        let Some(id) = self.catalog_sound(sound).map(|e| e.id.clone()) else {
            return false;
        };
        self.start_normalization(&id, audio)
    }

    /// [`normalize_sound`](Self::normalize_sound) for the whole catalog (the
    /// operator switched normalization on). Returns how many measurements
    /// were started.
    pub fn normalize_catalog(&mut self, engine: &AudioEngine) -> usize {
        let sounds: Vec<SoundId> = self.catalog().iter().map(|entry| entry.sound).collect();
        sounds
            .into_iter()
            .filter(|sound| self.normalize_sound(engine, *sound))
            .count()
    }

    /// Hand PCM of the library sound `library_id` to the measuring worker.
    /// Returns false when it is already queued or the worker is not available.
    pub fn start_normalization(&mut self, library_id: &str, audio: Arc<DecodedAudio>) -> bool {
        let ctl = &mut self.audio_ctl;
        if ctl.measuring.contains(library_id) {
            return false;
        }
        let job = MeasureJob {
            library_id: library_id.to_string(),
            audio,
        };
        let sent = ctl.worker().is_some_and(|worker| worker.send(job).is_ok());
        if sent {
            ctl.measuring.insert(library_id.to_string());
        }
        sent
    }

    /// Number of sounds being measured.
    pub fn measuring_count(&self) -> usize {
        self.audio_ctl.measuring.len()
    }

    /// Collect finished measurements: set the catalog gain, bring the engine
    /// level of the sound along and queue a [`GainUpdate`]. Returns how many
    /// gains were applied. Never blocks; [`Director::tick`] calls it.
    pub fn poll_normalization<P: Playback + ?Sized>(&mut self, pb: &P) -> usize {
        let mut applied = 0;
        while let Ok(result) = self.audio_ctl.results_rx.try_recv() {
            let ctl = &mut self.audio_ctl;
            ctl.measuring.remove(&result.library_id);
            ctl.attempted.insert(result.library_id.clone());
            // Switched off while the worker was busy: keep the library as is.
            let Some(info) = result.info.filter(|_| ctl.normalize) else {
                continue;
            };
            let gain = info
                .gain_db_for_target(ctl.target_lufs)
                .clamp(MIN_GAIN_DB, MAX_GAIN_DB);
            if self.set_sound_gain_db(pb, &result.library_id, gain) {
                self.audio_ctl.completed.push(GainUpdate {
                    library_id: result.library_id,
                    gain_db: gain,
                    lufs: info.lufs,
                });
                applied += 1;
            }
        }
        applied
    }

    /// The gains measured since the last call, for the owner of the library to
    /// store (`gain_db`) and save.
    pub fn take_gain_updates(&mut self) -> Vec<GainUpdate> {
        std::mem::take(&mut self.audio_ctl.completed)
    }

    /// Set the fixed gain of a catalog sound (by library id) and bring its
    /// engine level along: a sound that plays keeps its scene level (scaled by
    /// the change), a stopped one is set to `volume x gain`. The user's volume
    /// stays as it is. Returns false when the sound is not in the catalog.
    pub fn set_sound_gain_db<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        library_id: &str,
        gain_db: f32,
    ) -> bool {
        let gain_db = if gain_db.is_finite() {
            gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
        } else {
            0.0
        };
        let Some(entry) = self.catalog_entry_mut(library_id) else {
            return false;
        };
        let old = entry.gain_db;
        entry.gain_db = gain_db;
        let (sound, level_now) = (entry.sound, entry.level(entry.volume));
        let result = if pb.is_playing(sound) && pb.volume(sound).is_some() {
            // Scale in place: setting the level would drop the fade the sound
            // is in the middle of (a scene crossfade, a fade-in).
            let ratio = 10f32.powf((gain_db - old) / 20.0);
            pb.scale_volume(sound, ratio)
        } else {
            pb.set_volume(sound, level_now)
        };
        if let Err(e) = result {
            tracing::warn!("Could not apply the gain of {library_id}: {e}");
        }
        true
    }

    /// Push the engine level (`volume x gain`) of a catalog sound into the
    /// engine. The console calls it right after a sound was loaded so a saved
    /// gain is audible from the first play.
    pub fn apply_sound_level<P: Playback + ?Sized>(
        &self,
        pb: &P,
        sound: SoundId,
    ) -> PlaybackResult {
        match self.catalog_sound(sound) {
            Some(entry) => pb.set_volume(sound, entry.level(entry.volume)),
            None => Ok(()),
        }
    }

    fn catalog_entry_mut(&mut self, library_id: &str) -> Option<&mut CatalogSound> {
        self.catalog.iter_mut().find(|e| e.id == library_id)
    }
}

/// Store measured gains in a library (the headless server's config). Entries
/// are matched by `id`; an entry without an id is matched by the
/// `sound-<index>` id the headless board gives it. Returns whether anything
/// changed.
pub fn apply_gain_updates(library: &mut LibraryConfig, updates: &[GainUpdate]) -> bool {
    let mut changed = false;
    for update in updates {
        for (index, entry) in library.sounds.iter_mut().enumerate() {
            let id_matches = if entry.id.is_empty() {
                update.library_id == format!("sound-{index}")
            } else {
                entry.id == update.library_id
            };
            if id_matches && (entry.gain_db - update.gain_db).abs() > f32::EPSILON {
                entry.gain_db = update.gain_db;
                changed = true;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers_accept_the_documented_names() {
        assert_eq!(parse_bus(" Music "), Some(Bus::Music));
        assert_eq!(parse_bus("sfx"), Some(Bus::Sfx));
        assert_eq!(parse_bus("voice"), None);
        assert_eq!(DuckTarget::parse(""), Some(DuckTarget::Configured));
        assert_eq!(DuckTarget::parse("ALL"), Some(DuckTarget::All));
        assert_eq!(
            DuckTarget::parse("ambience"),
            Some(DuckTarget::Bus(Bus::Ambience))
        );
        assert_eq!(DuckTarget::parse("nope"), None);
        assert_eq!(DuckMode::parse("hold"), Some(DuckMode::HoldStart));
        assert_eq!(DuckMode::parse("up"), Some(DuckMode::HoldEnd));
        assert_eq!(DuckMode::parse("sideways"), None);
        for bus in BUSES {
            assert_eq!(parse_bus(bus_name(bus)), Some(bus));
        }
    }

    #[test]
    fn level_for_matches_the_catalog_level() {
        let mut sound = CatalogSound::new("a", SoundId::new());
        for (volume, gain) in [(1.0, 0.0), (0.5, -6.0), (0.4, 6.0), (1.0, 12.0), (0.0, 3.0)] {
            sound.gain_db = gain;
            assert_eq!(level_for(volume, gain), sound.level(volume));
        }
        assert_eq!(level_for(f32::NAN, f32::NAN), 1.0);
    }

    #[test]
    fn configure_audio_takes_defaults_and_replaces_nonsense() {
        let mut director = Director::new();
        let mut audio = AudioConfig {
            duck_amount: 0.5,
            duck_attack_ms: 10,
            duck_release_ms: 20,
            duck_buses: vec![Bus::Sfx],
            normalize: true,
            normalize_target_lufs: -20.0,
            ..Default::default()
        };
        director.configure_audio(&audio);
        assert_eq!(
            director.normalization(),
            NormalizeSettings {
                enabled: true,
                target_lufs: -20.0
            }
        );
        assert_eq!(director.audio_ctl.duck_amount, 0.5);
        assert_eq!(director.audio_ctl.duck_buses, vec![Bus::Sfx]);
        audio.duck_amount = f32::NAN;
        audio.normalize_target_lufs = f32::INFINITY;
        director.configure_audio(&audio);
        assert_eq!(
            director.audio_ctl.duck_amount,
            AudioConfig::default().duck_amount
        );
        assert_eq!(
            director.normalization().target_lufs,
            AudioConfig::default().normalize_target_lufs
        );
    }

    #[test]
    fn gain_updates_find_entries_by_id_or_board_index() {
        let mut library = LibraryConfig::default();
        library.sounds.push(crate::config::SoundConfigEntry {
            id: "a".into(),
            ..Default::default()
        });
        library
            .sounds
            .push(crate::config::SoundConfigEntry::default());
        let updates = [
            GainUpdate {
                library_id: "a".into(),
                gain_db: -4.0,
                lufs: -14.0,
            },
            GainUpdate {
                library_id: "sound-1".into(),
                gain_db: -2.0,
                lufs: -16.0,
            },
        ];
        assert!(apply_gain_updates(&mut library, &updates));
        assert_eq!(library.sounds[0].gain_db, -4.0);
        assert_eq!(library.sounds[1].gain_db, -2.0);
        assert!(!apply_gain_updates(&mut library, &updates));
    }
}
