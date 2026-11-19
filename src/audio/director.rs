//! The Director: one control core for the console and the headless server.
//!
//! Before it, the console (`AudioCommand` handled in the GUI) and the headless
//! control page (`Board`) each talked to the audio engine directly, so every new
//! feature (scenes, hotkeys, a phone remote) would have had to be written
//! twice. Now both go through a [`Director`]:
//!
//! ```text
//!   console  --AudioCommand-->  Director  --Playback-->  AudioEngine
//!   /control --ControlAction--> Director  --Playback-->  (or MockPlayback)
//! ```
//!
//! # What it does
//!
//! * **Scenes.** [`Director::activate_scene`] computes the difference between
//!   what plays now and what the scene wants ([`ActivationPlan`]: start, retune,
//!   stop) and executes it with the scene's `fade_in_ms` / `fade_out_ms` /
//!   `curve`. Activating again in the middle of a transition continues from the
//!   current gains: a sound that is already (fading) in is only retuned, never
//!   restarted, and the mixer starts every fade from the track's present gain.
//! * **Snapshots.** [`Director::snapshot_to_scene`] turns the playing mix into
//!   a new scene.
//! * **Stingers.** [`Director::play_stinger`] layers a one-shot over everything.
//!   It does not touch the active scene, and later scene changes spare it.
//! * **Delays.** `delay_ms` of a scene item is a pending start kept inside the
//!   Director and released by [`Director::tick`]; nothing sleeps.
//!   `start = "manual"` items are never started automatically.
//! * **State.** [`Director::subscribe`] hands out a `tokio::sync::watch`
//!   receiver of [`DirectorState`] (active scene, pending starts, playing
//!   sounds), the feed for `/ws/control` and for any UI.
//!
//! # Threading
//!
//! The engine is `!Send` (its cpal stream), so the Director does not own it and
//! does not spawn a thread that touches it. Every operation takes the engine as
//! a `&impl Playback` argument and is called from the thread that owns the
//! engine. Time-based work (delayed starts) is driven by [`Director::tick`];
//! the owner calls it regularly and uses [`Director::next_deadline`] to know
//! when it must wake up. The state itself ([`DirectorState`] behind a `watch`
//! channel) is `Send` and can be read from anywhere.
//!
//! # Testing without sound
//!
//! [`Playback`] is the small set of engine operations the Director needs.
//! [`AudioEngine`] implements it; [`mock::MockPlayback`] is an in-memory
//! implementation with a simulated gain per track, used by the tests.
//!
//! # Extension points
//!
//! * Emitters, playlists, hotkeys, ducking: take `&mut Director` (or send it a
//!   [`DirectorCommand`] through [`Director::execute`]) and call the public
//!   operations; follow the active scene through [`Director::subscribe`].
//!   Ducking uses [`Playback::duck_bus`] / [`Playback::release_duck`].
//! * Loudness normalization: [`CatalogSound::gain_db`] is applied in one place,
//!   [`CatalogSound::level`].

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::watch;

use crate::audio::director_audio::AudioControl;
use crate::audio::engine::{AudioEngine, EngineError, SoundId};
use crate::audio::mixer::{Bus as MixerBus, DuckSettings, FadeCurve as MixerCurve};
use crate::audio::playlist::{MusicLayer, MusicPlayback, PlaylistCommand};
use crate::config::{Bus, FadeCurve, SceneConfig, SceneItemConfig, SceneStart};

/// Levels closer than this count as equal (no retune needed).
const LEVEL_EPSILON: f32 = 1e-3;

/// How often [`Director::tick`] refreshes the published state when nothing
/// else happened (sounds that end by themselves).
const REFRESH_EVERY: Duration = Duration::from_millis(200);

/// Result of a [`Playback`] call.
pub type PlaybackResult = Result<(), EngineError>;

/// A fade: how long and which shape. `ms == 0` means "no fade" (the engine
/// still applies its short click-protection ramp where it needs one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FadeSpec {
    pub ms: u32,
    pub curve: FadeCurve,
}

impl FadeSpec {
    /// No fade.
    pub const NONE: FadeSpec = FadeSpec {
        ms: 0,
        curve: FadeCurve::EqualPower,
    };

    /// A fade of `ms` along `curve`.
    pub fn new(ms: u32, curve: FadeCurve) -> Self {
        Self { ms, curve }
    }
}

/// The engine operations the Director needs.
///
/// Everything is keyed by the engine's [`SoundId`]. Methods take `&self`: the
/// engine is internally synchronized, and the mock uses a mutex.
pub trait Playback {
    /// Sounds that are audibly playing right now (a sound that is only fading
    /// out its tail does not count).
    fn playing_sounds(&self) -> Vec<SoundId>;
    /// Whether the sound is playing (see [`playing_sounds`](Self::playing_sounds)).
    fn is_playing(&self, sound: SoundId) -> bool;
    /// The sound's level (the level an ongoing fade heads for), `None` when the
    /// engine does not know the sound.
    fn volume(&self, sound: SoundId) -> Option<f32>;
    /// Whether the sound repeats.
    fn is_looped(&self, sound: SoundId) -> bool;
    /// Set the level at once (a short ramp, no long fade).
    fn set_volume(&self, sound: SoundId, volume: f32) -> PlaybackResult;
    /// Multiply the level by `ratio` without cutting a fade in progress short.
    /// Default: `set_volume` of the scaled level (mocks).
    fn scale_volume(&self, sound: SoundId, ratio: f32) -> PlaybackResult {
        match self.volume(sound) {
            Some(current) => self.set_volume(sound, (current * ratio).clamp(0.0, 1.0)),
            None => Ok(()),
        }
    }
    /// Turn repeating on or off (live for a playing sound).
    fn set_looped(&self, sound: SoundId, looped: bool) -> PlaybackResult;
    /// Pan, `-1.0..=1.0`.
    fn set_pan(&self, sound: SoundId, pan: f32) -> PlaybackResult;
    /// Route the sound to a bus.
    fn set_bus(&self, sound: SoundId, bus: Bus) -> PlaybackResult;
    /// Start the sound from the beginning, fading in. `volume` (when given)
    /// becomes the sound's level first; `None` keeps the remembered one.
    fn play(
        &self,
        sound: SoundId,
        looped: bool,
        volume: Option<f32>,
        fade_in: FadeSpec,
    ) -> PlaybackResult;
    /// Layer a one-shot on top of whatever plays (a stinger). `volume` is
    /// applied only when the sound is not already playing, so a stinger never
    /// changes the level of the same sound inside the scene.
    fn play_one_shot(&self, sound: SoundId, volume: f32) -> PlaybackResult;
    /// Stop the sound, fading out. It reports as stopped at once.
    fn stop(&self, sound: SoundId, fade_out: FadeSpec) -> PlaybackResult;
    /// Fade the sound's level to `volume`, from the gain it has right now.
    fn fade_to(&self, sound: SoundId, volume: f32, fade: FadeSpec) -> PlaybackResult;
    /// Stop everything, fading out.
    fn stop_all(&self, fade_out: FadeSpec) -> PlaybackResult;
    /// Master volume.
    fn master_volume(&self) -> f32;
    /// Set the master volume.
    fn set_master_volume(&self, volume: f32) -> PlaybackResult;
    /// Length of the sound in milliseconds, `None` when unknown. Emitters use
    /// it to know when an instance they started has ended.
    fn duration_ms(&self, _sound: SoundId) -> Option<u32> {
        None
    }
    /// Turn a bus down under something else. Default: nothing (mocks).
    fn duck_bus(&self, _bus: Bus, _settings: DuckSettings) {}
    /// Bring a ducked bus back. Default: nothing (mocks).
    fn release_duck(&self, _bus: Bus) {}
    /// Volume of a bus. Default: nothing (mocks).
    fn set_bus_volume(&self, _bus: Bus, _volume: f32) -> PlaybackResult {
        Ok(())
    }
    /// Position, seek, pause and "track ended" for the music layer
    /// (playlists). Default: not offered.
    fn music(&self) -> Option<&dyn MusicPlayback> {
        None
    }
}

fn mixer_curve(curve: FadeCurve) -> MixerCurve {
    match curve {
        FadeCurve::EqualPower => MixerCurve::EqualPower,
        FadeCurve::Linear => MixerCurve::Linear,
    }
}

fn mixer_bus(bus: Bus) -> MixerBus {
    match bus {
        Bus::Music => MixerBus::Music,
        Bus::Ambience => MixerBus::Ambience,
        Bus::Sfx => MixerBus::Sfx,
    }
}

impl Playback for AudioEngine {
    fn playing_sounds(&self) -> Vec<SoundId> {
        self.list_sounds()
            .into_iter()
            .filter(|sound| self.is_sound_playing(*sound).unwrap_or(false))
            .collect()
    }

    fn is_playing(&self, sound: SoundId) -> bool {
        self.is_sound_playing(sound).unwrap_or(false)
    }

    fn volume(&self, sound: SoundId) -> Option<f32> {
        self.sound_volume(sound).ok()
    }

    fn is_looped(&self, sound: SoundId) -> bool {
        self.is_sound_looped(sound).unwrap_or(false)
    }

    fn set_volume(&self, sound: SoundId, volume: f32) -> PlaybackResult {
        self.set_sound_volume(sound, volume)
    }

    fn scale_volume(&self, sound: SoundId, ratio: f32) -> PlaybackResult {
        self.scale_sound_volume(sound, ratio)
    }

    fn set_looped(&self, sound: SoundId, looped: bool) -> PlaybackResult {
        self.set_sound_looped(sound, looped)
    }

    fn set_pan(&self, sound: SoundId, pan: f32) -> PlaybackResult {
        self.set_sound_pan(sound, pan)
    }

    fn set_bus(&self, sound: SoundId, bus: Bus) -> PlaybackResult {
        self.set_sound_bus(sound, mixer_bus(bus))
    }

    fn play(
        &self,
        sound: SoundId,
        looped: bool,
        volume: Option<f32>,
        fade_in: FadeSpec,
    ) -> PlaybackResult {
        if let Some(volume) = volume {
            self.set_sound_volume(sound, volume)?;
        }
        if fade_in.ms == 0 {
            // Keeps the attack of a hard start, like the plain play button.
            self.play_with_loop(sound, looped)?;
        } else {
            self.play_sound_fade(sound, looped, fade_in.ms, mixer_curve(fade_in.curve))?;
        }
        Ok(())
    }

    fn play_one_shot(&self, sound: SoundId, volume: f32) -> PlaybackResult {
        if !self.is_sound_playing(sound).unwrap_or(false) {
            self.set_sound_volume(sound, volume)?;
        }
        self.play_with_loop(sound, false)?;
        Ok(())
    }

    fn stop(&self, sound: SoundId, fade_out: FadeSpec) -> PlaybackResult {
        if fade_out.ms == 0 {
            self.stop_sound(sound)
        } else {
            self.stop_sound_fade(sound, fade_out.ms, mixer_curve(fade_out.curve))
        }
    }

    fn fade_to(&self, sound: SoundId, volume: f32, fade: FadeSpec) -> PlaybackResult {
        if !(0.0..=1.0).contains(&volume) {
            return Err(EngineError::Mixer(
                crate::audio::mixer::MixerError::InvalidVolume(volume),
            ));
        }
        // Existence check (and the sound's registry entry) first.
        self.sound_info(sound)?;
        // The fade starts from the gain the tracks have right now, so a
        // transition that is already running is continued, not restarted.
        self.fade_sound(sound, volume, fade.ms, mixer_curve(fade.curve))?;
        // `fade_sound` leaves the remembered level alone (it is meant for
        // fade-outs); a scene's level has to be remembered for the next play
        // and for `volume()`. Done after the fade: `set_sound_volume` would
        // cancel it.
        self.inner.sound_volumes.write().insert(sound, volume);
        Ok(())
    }

    fn stop_all(&self, fade_out: FadeSpec) -> PlaybackResult {
        if fade_out.ms == 0 {
            AudioEngine::stop_all(self)
        } else {
            self.stop_all_fade(fade_out.ms, mixer_curve(fade_out.curve))
        }
    }

    fn master_volume(&self) -> f32 {
        AudioEngine::master_volume(self)
    }

    fn set_master_volume(&self, volume: f32) -> PlaybackResult {
        AudioEngine::set_master_volume(self, volume)
    }

    fn duration_ms(&self, sound: SoundId) -> Option<u32> {
        let (secs, _, _) = self.sound_info(sound).ok()?;
        (secs.is_finite() && secs > 0.0).then(|| (secs * 1000.0).round() as u32)
    }

    fn duck_bus(&self, bus: Bus, settings: DuckSettings) {
        AudioEngine::duck_bus(self, mixer_bus(bus), settings);
    }

    fn release_duck(&self, bus: Bus) {
        AudioEngine::release_duck(self, mixer_bus(bus));
    }

    fn set_bus_volume(&self, bus: Bus, volume: f32) -> PlaybackResult {
        AudioEngine::set_bus_volume(self, mixer_bus(bus), volume)
    }

    fn music(&self) -> Option<&dyn MusicPlayback> {
        Some(self)
    }
}

/// What can go wrong in a Director call.
#[derive(Debug, Error)]
pub enum DirectorError {
    #[error("no scene with id {0}")]
    UnknownScene(String),
    #[error("the scene name is empty")]
    EmptyName,
    #[error("no sound from the library is playing")]
    NothingPlaying,
    #[error("invalid value: {0}")]
    Invalid(String),
    #[error("{0}")]
    Playback(#[from] EngineError),
}

/// A library sound as the Director sees it: the stable library id (what scenes
/// refer to) next to the engine's id (what [`Playback`] takes), and the
/// per-sound settings that matter for playback.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogSound {
    /// Library id (UUID string), the one `[[library.scenes.items]]` use.
    pub id: String,
    /// The engine's id of the loaded sound.
    pub sound: SoundId,
    /// The sound's own level, `0.0..=1.0`.
    pub volume: f32,
    /// The sound's own repeat flag.
    pub looped: bool,
    /// Fixed correction in dB on top of every level (loudness normalization).
    pub gain_db: f32,
    /// Mixer bus the sound is routed to when it starts.
    pub bus: Bus,
    /// A stinger: one-shot over everything, spared by scene changes.
    pub stinger: bool,
    /// The sound's own stereo position, `-1.0..=1.0` (see `director_pan.rs`).
    pub pan: f32,
}

impl CatalogSound {
    /// A catalog entry with neutral settings.
    pub fn new(id: impl Into<String>, sound: SoundId) -> Self {
        Self {
            id: id.into(),
            sound,
            volume: 1.0,
            looped: false,
            gain_db: 0.0,
            bus: Bus::default(),
            stinger: false,
            pan: 0.0,
        }
    }

    /// The engine level for a wanted level: `volume` plus the fixed gain,
    /// clamped to the engine's `0.0..=1.0`.
    pub fn level(&self, volume: f32) -> f32 {
        let volume = if volume.is_finite() { volume } else { 1.0 };
        let gain = 10f32.powf(self.gain_db / 20.0);
        (volume.clamp(0.0, 1.0) * gain).clamp(0.0, 1.0)
    }

    /// Inverse of [`level`](Self::level): the wanted level behind an engine
    /// level.
    fn unlevel(&self, level: f32) -> f32 {
        let gain = 10f32.powf(self.gain_db / 20.0);
        if gain <= f32::EPSILON {
            return level.clamp(0.0, 1.0);
        }
        (level / gain).clamp(0.0, 1.0)
    }
}

/// One sound a scene activation starts.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedStart {
    pub sound: SoundId,
    /// Library id of the sound.
    pub library_id: String,
    /// Engine level (item volume with the sound's fixed gain).
    pub volume: f32,
    pub looped: bool,
    /// Delay after activation; `0` starts at once.
    pub delay_ms: u32,
}

/// A sound that already plays and only has to change level or repeat flag.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRetune {
    pub sound: SoundId,
    /// `(from, to)` engine level, `None` when the level already matches.
    pub volume: Option<(f32, f32)>,
    /// New repeat flag, `None` when it already matches.
    pub looped: Option<bool>,
}

/// The difference between what plays now and what a scene wants.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivationPlan {
    pub scene: String,
    pub fade_in: FadeSpec,
    pub fade_out: FadeSpec,
    /// Sounds to start (with the scene's fade-in).
    pub starts: Vec<PlannedStart>,
    /// Common sounds: level / repeat changes.
    pub retunes: Vec<PlannedRetune>,
    /// Sounds to fade out (`stop_others` only).
    pub stops: Vec<SoundId>,
    /// Library ids of scene items that are not loaded (skipped).
    pub missing: Vec<String>,
}

/// What an operation did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneReport {
    pub scene: String,
    pub started: Vec<SoundId>,
    /// Started later, after their `delay_ms`.
    pub scheduled: Vec<SoundId>,
    pub retuned: Vec<SoundId>,
    pub stopped: Vec<SoundId>,
    pub missing: Vec<String>,
    /// Per-sound failures; the rest of the operation still ran.
    pub errors: Vec<String>,
}

/// State published to subscribers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectorState {
    /// Grows by one on every published change.
    pub revision: u64,
    /// Grows when the set of scenes was replaced with different content.
    pub scenes_revision: u64,
    /// Id of the active scene.
    pub active_scene: Option<String>,
    /// Delayed starts still waiting.
    pub pending_starts: usize,
    /// Library ids of the playing catalog sounds.
    pub playing: Vec<String>,
}

/// A request to the Director, for callers that do not hold a typed handle
/// (hotkeys, the network). [`Director::execute`] runs it.
#[derive(Debug, Clone, PartialEq)]
pub enum DirectorCommand {
    ActivateScene(String),
    StopScene,
    SnapshotToScene(String),
    PlayStinger(SoundId),
    StopAllFade(u32),
    PlaySound(SoundId),
    StopSound(SoundId),
    SetVolume(SoundId, f32),
    SetLoop(SoundId, bool),
    /// A sound's own pan, `-1.0..=1.0` (see `director_pan.rs`).
    SetPan(SoundId, f32),
    SetMaster(f32),
    StopAll,
    /// Control the music layer (playlists).
    Playlist(PlaylistCommand),
}

/// What [`Director::execute`] returns.
#[derive(Debug, Clone, PartialEq)]
pub enum DirectorOutcome {
    Done,
    Scene(SceneReport),
    Snapshot(SceneConfig),
}

/// A start waiting for its `delay_ms`.
#[derive(Debug, Clone)]
struct PendingStart {
    scene: String,
    sound: SoundId,
    volume: f32,
    looped: bool,
    fade: FadeSpec,
    due: Instant,
}

/// The control core. See the module docs.
pub struct Director {
    pub(crate) catalog: Vec<CatalogSound>,
    scenes: Vec<SceneConfig>,
    active: Option<String>,
    pending: Vec<PendingStart>,
    /// Sounds started as stingers that still sound.
    stingers: HashSet<SoundId>,
    state_tx: watch::Sender<DirectorState>,
    last_refresh: Option<Instant>,
    /// Random / periodic sounds of the active scene (see `audio::emitter`).
    pub(crate) emitters: crate::audio::emitter::EmitterHost,
    /// Playlists and the music player (see `audio::playlist`).
    pub(crate) music: MusicLayer,
    /// Buses, ducking and normalization (see `director_audio.rs`).
    pub(crate) audio_ctl: AudioControl,
}

impl Default for Director {
    fn default() -> Self {
        Self::new()
    }
}

impl Director {
    /// An empty Director: no scenes, no catalog.
    pub fn new() -> Self {
        let (state_tx, _) = watch::channel(DirectorState::default());
        Self {
            catalog: Vec::new(),
            scenes: Vec::new(),
            active: None,
            pending: Vec::new(),
            stingers: HashSet::new(),
            state_tx,
            last_refresh: None,
            emitters: crate::audio::emitter::EmitterHost::new(),
            music: MusicLayer::default(),
            audio_ctl: AudioControl::default(),
        }
    }

    // ----------------------------------------------------------- state ----

    /// Follow state changes: the receiver always holds the latest
    /// [`DirectorState`] and wakes (`changed().await`) on every new revision.
    /// Cheap to clone, `Send`, usable from the server's async runtime.
    pub fn subscribe(&self) -> watch::Receiver<DirectorState> {
        self.state_tx.subscribe()
    }

    /// The latest published state.
    pub fn state(&self) -> DirectorState {
        self.state_tx.borrow().clone()
    }

    /// Library id of the active scene.
    pub fn active_scene(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// The scenes the Director knows.
    pub fn scenes(&self) -> &[SceneConfig] {
        &self.scenes
    }

    /// One scene by library id.
    pub fn scene(&self, id: &str) -> Option<&SceneConfig> {
        self.scenes.iter().find(|scene| scene.id == id)
    }

    /// The registered library sounds.
    pub fn catalog(&self) -> &[CatalogSound] {
        &self.catalog
    }

    /// The catalog entry of an engine sound.
    pub fn catalog_sound(&self, sound: SoundId) -> Option<&CatalogSound> {
        self.catalog.iter().find(|entry| entry.sound == sound)
    }

    fn by_library_id(&self, id: &str) -> Option<&CatalogSound> {
        self.catalog.iter().find(|entry| entry.id == id)
    }

    fn is_stinger(&self, sound: SoundId) -> bool {
        self.stingers.contains(&sound) || self.catalog_sound(sound).is_some_and(|e| e.stinger)
    }

    // ------------------------------------------------------- library ----

    /// Replace the scenes and the catalog (what the owner of the library calls
    /// whenever either changed, or right before a scene command). The active
    /// scene stays active only while it still exists; pending starts of
    /// vanished scenes are dropped.
    pub fn sync_library(&mut self, scenes: Vec<SceneConfig>, sounds: Vec<CatalogSound>) {
        self.catalog = sounds;
        self.set_scenes(scenes);
    }

    /// Replace the scenes.
    pub fn set_scenes(&mut self, scenes: Vec<SceneConfig>) {
        let changed = self.scenes != scenes;
        self.scenes = scenes;
        if let Some(active) = &self.active {
            if self.scenes.iter().all(|scene| &scene.id != active) {
                self.active = None;
            }
        }
        let scenes = &self.scenes;
        self.pending
            .retain(|p| scenes.iter().any(|scene| scene.id == p.scene));
        let active = self.active.clone();
        let pending = self.pending.len();
        self.state_tx.send_if_modified(|state| {
            if !changed && state.active_scene == active && state.pending_starts == pending {
                return false;
            }
            if changed {
                state.scenes_revision += 1;
            }
            state.active_scene = active;
            state.pending_starts = pending;
            state.revision += 1;
            true
        });
    }

    /// Add or replace one catalog sound (by library id).
    pub fn register_sound(&mut self, entry: CatalogSound) {
        match self.catalog.iter_mut().find(|e| e.id == entry.id) {
            Some(existing) => *existing = entry,
            None => self.catalog.push(entry),
        }
    }

    /// Forget a sound (it was removed from the library or unloaded).
    pub fn unregister_sound(&mut self, sound: SoundId) {
        self.catalog.retain(|entry| entry.sound != sound);
        self.pending.retain(|p| p.sound != sound);
        self.stingers.remove(&sound);
    }

    // --------------------------------------------------------- scenes ----

    /// What activating a scene would do right now, without doing it.
    pub fn plan_scene<P: Playback + ?Sized>(
        &self,
        pb: &P,
        scene_id: &str,
    ) -> Result<ActivationPlan, DirectorError> {
        let scene = self
            .scene(scene_id)
            .ok_or_else(|| DirectorError::UnknownScene(scene_id.to_string()))?;
        let playing = pb.playing_sounds();
        let mut plan = ActivationPlan {
            scene: scene.id.clone(),
            fade_in: FadeSpec::new(scene.fade_in_ms, scene.curve),
            fade_out: FadeSpec::new(scene.fade_out_ms, scene.curve),
            starts: Vec::new(),
            retunes: Vec::new(),
            stops: Vec::new(),
            missing: Vec::new(),
        };

        let mut in_scene: HashSet<SoundId> = HashSet::new();
        for item in &scene.items {
            let Some(entry) = self.by_library_id(&item.sound) else {
                plan.missing.push(item.sound.clone());
                continue;
            };
            if !in_scene.insert(entry.sound) {
                continue; // a sound appears once: the first item wins
            }
            if item.start == SceneStart::Manual {
                // Listed in the scene, but only the operator starts it. If it
                // plays it is part of the scene (spared by `stop_others`).
                continue;
            }
            let volume = entry.level(item.volume);
            if playing.contains(&entry.sound) {
                let current = pb.volume(entry.sound).unwrap_or(volume);
                let volume_change =
                    ((current - volume).abs() > LEVEL_EPSILON).then_some((current, volume));
                let loop_change = (pb.is_looped(entry.sound) != item.looped).then_some(item.looped);
                if volume_change.is_some() || loop_change.is_some() {
                    plan.retunes.push(PlannedRetune {
                        sound: entry.sound,
                        volume: volume_change,
                        looped: loop_change,
                    });
                }
            } else {
                plan.starts.push(PlannedStart {
                    sound: entry.sound,
                    library_id: entry.id.clone(),
                    volume,
                    looped: item.looped,
                    delay_ms: item.delay_ms,
                });
            }
        }

        if scene.stop_others {
            plan.stops = playing
                .into_iter()
                .filter(|sound| {
                    !in_scene.contains(sound)
                        && !self.is_stinger(*sound)
                        && !self.emitter_instance_of(scene_id, *sound)
                        && !self.music_reserves(scene, *sound)
                })
                .collect();
        }
        Ok(plan)
    }

    /// Switch to a scene. See the module docs; time is `Instant::now()`.
    pub fn activate_scene<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        scene_id: &str,
    ) -> Result<SceneReport, DirectorError> {
        self.activate_scene_at(pb, scene_id, Instant::now())
    }

    /// [`activate_scene`](Self::activate_scene) with an explicit clock, for tests.
    pub fn activate_scene_at<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        scene_id: &str,
        now: Instant,
    ) -> Result<SceneReport, DirectorError> {
        let plan = self.plan_scene(pb, scene_id)?;
        let mut report = SceneReport {
            scene: plan.scene.clone(),
            missing: plan.missing.clone(),
            ..Default::default()
        };

        // Delayed starts already waiting for this very scene keep their time:
        // pressing the scene button again must not push them back.
        let old_pending = std::mem::take(&mut self.pending);

        for start in &plan.starts {
            if start.delay_ms == 0 {
                match self.start_sound(
                    pb,
                    Some(&plan.scene),
                    start.sound,
                    start.looped,
                    start.volume,
                    plan.fade_in,
                ) {
                    Ok(()) => report.started.push(start.sound),
                    Err(e) => report.errors.push(format!("{}: {e}", start.library_id)),
                }
            } else {
                let due = old_pending
                    .iter()
                    .find(|p| p.scene == plan.scene && p.sound == start.sound)
                    .map(|p| p.due)
                    .unwrap_or_else(|| now + Duration::from_millis(u64::from(start.delay_ms)));
                self.pending.push(PendingStart {
                    scene: plan.scene.clone(),
                    sound: start.sound,
                    volume: start.volume,
                    looped: start.looped,
                    fade: plan.fade_in,
                    due,
                });
                report.scheduled.push(start.sound);
            }
        }

        for retune in &plan.retunes {
            let mut ok = true;
            if let Some((from, to)) = retune.volume {
                let fade = if to > from {
                    plan.fade_in
                } else {
                    plan.fade_out
                };
                if let Err(e) = pb.fade_to(retune.sound, to, fade) {
                    report.errors.push(format!("{}: {e}", retune.sound));
                    ok = false;
                }
            }
            if let Some(looped) = retune.looped {
                if let Err(e) = pb.set_looped(retune.sound, looped) {
                    report.errors.push(format!("{}: {e}", retune.sound));
                    ok = false;
                }
            }
            if ok {
                report.retuned.push(retune.sound);
            }
        }

        self.apply_scene_pans(pb, &plan.scene);

        for sound in &plan.stops {
            match pb.stop(*sound, plan.fade_out) {
                Ok(()) => report.stopped.push(*sound),
                Err(e) => report.errors.push(format!("{sound}: {e}")),
            }
        }

        self.active = Some(plan.scene);
        self.sync_emitters(pb, now);
        self.music_on_scene(pb, &mut report);
        self.last_refresh = Some(now);
        self.publish(pb);
        Ok(report)
    }

    /// Fade out the active scene's sounds and leave no scene active. Stingers
    /// and sounds that are not part of the scene keep playing.
    pub fn stop_scene<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
    ) -> Result<SceneReport, DirectorError> {
        let mut report = SceneReport::default();
        self.pending.clear();
        if let Some(scene) = self.active.take().and_then(|id| self.scene(&id).cloned()) {
            report.scene = scene.id.clone();
            let fade = FadeSpec::new(scene.fade_out_ms, scene.curve);
            self.fade_out_emitters(pb, fade);
            let mut seen = HashSet::new();
            for item in &scene.items {
                let Some(entry) = self.by_library_id(&item.sound) else {
                    continue;
                };
                if !seen.insert(entry.sound) || !pb.is_playing(entry.sound) {
                    continue;
                }
                match pb.stop(entry.sound, fade) {
                    Ok(()) => report.stopped.push(entry.sound),
                    Err(e) => report.errors.push(format!("{}: {e}", entry.id)),
                }
            }
            self.music_on_scene_stopped(pb, fade.ms);
        }
        self.publish(pb);
        Ok(report)
    }

    /// Save the mix that plays now as a new scene (default fades, "stop
    /// others"). Stingers are left out. The scene is added to the Director's
    /// list (activatable at once); the owner of the library has to store it
    /// there too, or the next [`sync_library`](Self::sync_library) drops it.
    pub fn snapshot_to_scene<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        name: &str,
    ) -> Result<SceneConfig, DirectorError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DirectorError::EmptyName);
        }
        let mut items = Vec::new();
        for entry in &self.catalog {
            if !pb.is_playing(entry.sound) || self.is_stinger(entry.sound) {
                continue;
            }
            let level = pb.volume(entry.sound).unwrap_or_else(|| entry.level(1.0));
            items.push(SceneItemConfig {
                sound: entry.id.clone(),
                volume: entry.unlevel(level),
                looped: pb.is_looped(entry.sound),
                start: SceneStart::OnEnter,
                delay_ms: 0,
                pan: 0.0,
            });
        }
        if items.is_empty() {
            return Err(DirectorError::NothingPlaying);
        }
        let scene = SceneConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            items,
            ..Default::default()
        };
        self.scenes.push(scene.clone());
        self.state_tx.send_modify(|state| {
            state.scenes_revision += 1;
            state.revision += 1;
        });
        Ok(scene)
    }

    // -------------------------------------------------------- stingers ----

    /// Play a sound once over everything. The active scene is untouched; later
    /// scene changes (`stop_others`) spare the stinger while it sounds.
    pub fn play_stinger<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        sound: SoundId,
    ) -> Result<(), DirectorError> {
        let volume = match self.catalog_sound(sound) {
            Some(entry) => {
                pb.set_bus(sound, entry.bus)?;
                entry.level(entry.volume)
            }
            None => pb.volume(sound).unwrap_or(1.0),
        };
        self.pan_before_start(pb, None, sound)?;
        pb.play_one_shot(sound, volume)?;
        self.stingers.insert(sound);
        self.publish(pb);
        Ok(())
    }

    /// Fade out everything (the active scene, stingers and manual sounds
    /// alike) over `ms` and leave no scene active.
    pub fn stop_all_with_fade<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        ms: u32,
    ) -> Result<(), DirectorError> {
        let curve = self
            .active
            .as_deref()
            .and_then(|id| self.scene(id))
            .map(|scene| scene.curve)
            .unwrap_or_default();
        self.pending.clear();
        self.active = None;
        self.clear_emitters();
        self.music_halt();
        let result = pb.stop_all(FadeSpec::new(ms, curve));
        self.publish(pb);
        Ok(result?)
    }

    // ---------------------------------------------------- manual sounds ----

    /// Start a sound from the beginning (restarting it when it already plays),
    /// at its own level, repeating when `looped`. A stinger-flagged sound is layered as a
    /// stinger instead. Without a fade, like the play button always did.
    pub fn play_sound<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        sound: SoundId,
        looped: bool,
    ) -> Result<(), DirectorError> {
        if self.catalog_sound(sound).is_some_and(|entry| entry.stinger) {
            return self.play_stinger(pb, sound);
        }
        // A scene may have left the engine at another level for this sound:
        // a hand-started sound always plays at its own level.
        let level = match self.catalog_sound(sound) {
            Some(entry) => {
                pb.set_bus(sound, entry.bus)?;
                Some(entry.level(entry.volume))
            }
            None => None,
        };
        self.pan_before_start(pb, self.active_scene(), sound)?;
        pb.stop(sound, FadeSpec::NONE)?;
        pb.play(sound, looped, level, FadeSpec::NONE)?;
        self.publish(pb);
        Ok(())
    }

    /// Stop a sound (the short click-protection ramp only).
    pub fn stop_sound<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        sound: SoundId,
    ) -> Result<(), DirectorError> {
        pb.stop(sound, FadeSpec::NONE)?;
        self.publish(pb);
        Ok(())
    }

    /// Set a sound's own level (`0.0..=1.0`), remembered in the catalog.
    pub fn set_sound_volume<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        sound: SoundId,
        volume: f32,
    ) -> Result<(), DirectorError> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(DirectorError::Invalid(format!("volume {volume}")));
        }
        let level = match self.catalog.iter_mut().find(|e| e.sound == sound) {
            Some(entry) => {
                entry.volume = volume;
                entry.level(volume)
            }
            None => volume,
        };
        pb.set_volume(sound, level)?;
        Ok(())
    }

    /// Turn repeating on or off for a sound, remembered in the catalog.
    pub fn set_sound_looped<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        sound: SoundId,
        looped: bool,
    ) -> Result<(), DirectorError> {
        pb.set_looped(sound, looped)?;
        if let Some(entry) = self.catalog.iter_mut().find(|e| e.sound == sound) {
            entry.looped = looped;
        }
        Ok(())
    }

    /// Set the master volume (`0.0..=1.0`).
    pub fn set_master_volume<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        volume: f32,
    ) -> Result<(), DirectorError> {
        pb.set_master_volume(volume)?;
        Ok(())
    }

    /// Stop everything at once (short ramp) and leave no scene active.
    pub fn stop_all<P: Playback + ?Sized>(&mut self, pb: &P) -> Result<(), DirectorError> {
        self.stop_all_with_fade(pb, 0)
    }

    // ------------------------------------------------------ scheduling ----

    /// When [`tick`](Self::tick) next has work (the earliest delayed start).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .iter()
            .map(|p| p.due)
            .chain(self.emitter_deadline())
            .chain(self.music_deadline())
            .min()
    }

    /// Run what is due. Call it regularly from the engine's thread (once per
    /// GUI frame, or when [`next_deadline`](Self::next_deadline) passes).
    pub fn tick<P: Playback + ?Sized>(&mut self, pb: &P) {
        self.tick_at(pb, Instant::now());
    }

    /// [`tick`](Self::tick) with an explicit clock, for tests.
    pub fn tick_at<P: Playback + ?Sized>(&mut self, pb: &P, now: Instant) {
        let mut ran = self.music_tick(pb, now);
        self.poll_normalization(pb);
        if self.pending.iter().any(|p| p.due <= now) {
            let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
                .into_iter()
                .partition(|p| p.due <= now);
            self.pending = rest;
            for start in due {
                // The scene may have been left or the sound played by hand
                // meanwhile; only start what is still wanted.
                if self.active.as_deref() != Some(start.scene.as_str())
                    || pb.is_playing(start.sound)
                {
                    continue;
                }
                if let Err(e) = self.start_sound(
                    pb,
                    Some(&start.scene),
                    start.sound,
                    start.looped,
                    start.volume,
                    start.fade,
                ) {
                    tracing::warn!("Delayed start of {} failed: {e}", start.sound);
                }
                ran = true;
            }
        }
        ran |= self.run_emitters(pb, now);
        let stale = self
            .last_refresh
            .is_none_or(|last| now.saturating_duration_since(last) >= REFRESH_EVERY);
        if ran || stale {
            self.last_refresh = Some(now);
            self.publish(pb);
        }
    }

    // -------------------------------------------------------- generic ----

    /// Run a [`DirectorCommand`].
    pub fn execute<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        command: DirectorCommand,
    ) -> Result<DirectorOutcome, DirectorError> {
        Ok(match command {
            DirectorCommand::ActivateScene(id) => {
                DirectorOutcome::Scene(self.activate_scene(pb, &id)?)
            }
            DirectorCommand::StopScene => DirectorOutcome::Scene(self.stop_scene(pb)?),
            DirectorCommand::SnapshotToScene(name) => {
                DirectorOutcome::Snapshot(self.snapshot_to_scene(pb, &name)?)
            }
            DirectorCommand::PlayStinger(sound) => {
                self.play_stinger(pb, sound)?;
                DirectorOutcome::Done
            }
            DirectorCommand::StopAllFade(ms) => {
                self.stop_all_with_fade(pb, ms)?;
                DirectorOutcome::Done
            }
            DirectorCommand::PlaySound(sound) => {
                let looped = self.catalog_sound(sound).is_some_and(|e| e.looped);
                self.play_sound(pb, sound, looped)?;
                DirectorOutcome::Done
            }
            DirectorCommand::StopSound(sound) => {
                self.stop_sound(pb, sound)?;
                DirectorOutcome::Done
            }
            DirectorCommand::SetVolume(sound, volume) => {
                self.set_sound_volume(pb, sound, volume)?;
                DirectorOutcome::Done
            }
            DirectorCommand::SetLoop(sound, looped) => {
                self.set_sound_looped(pb, sound, looped)?;
                DirectorOutcome::Done
            }
            DirectorCommand::SetPan(sound, pan) => {
                self.set_sound_pan(pb, sound, pan)?;
                DirectorOutcome::Done
            }
            DirectorCommand::SetMaster(volume) => {
                self.set_master_volume(pb, volume)?;
                DirectorOutcome::Done
            }
            DirectorCommand::StopAll => {
                self.stop_all(pb)?;
                DirectorOutcome::Done
            }
            DirectorCommand::Playlist(command) => {
                self.playlist_command(pb, command)?;
                DirectorOutcome::Done
            }
        })
    }

    // -------------------------------------------------------- private ----

    /// Start a catalog (or plain) sound for a scene.
    fn start_sound<P: Playback + ?Sized>(
        &self,
        pb: &P,
        scene: Option<&str>,
        sound: SoundId,
        looped: bool,
        volume: f32,
        fade_in: FadeSpec,
    ) -> PlaybackResult {
        if let Some(entry) = self.catalog_sound(sound) {
            pb.set_bus(sound, entry.bus)?;
        }
        self.pan_before_start(pb, scene, sound)?;
        pb.play(sound, looped, Some(volume), fade_in)
    }

    /// Publish the current state if it differs from the last one.
    fn publish<P: Playback + ?Sized>(&mut self, pb: &P) {
        self.stingers.retain(|sound| pb.is_playing(*sound));
        let playing: Vec<String> = self
            .catalog
            .iter()
            .filter(|entry| pb.is_playing(entry.sound))
            .map(|entry| entry.id.clone())
            .collect();
        let active = self.active.clone();
        let pending = self.pending.len();
        self.state_tx.send_if_modified(|state| {
            if state.active_scene == active
                && state.pending_starts == pending
                && state.playing == playing
            {
                return false;
            }
            state.active_scene = active;
            state.pending_starts = pending;
            state.playing = playing;
            state.revision += 1;
            true
        });
    }
}

/// Copy a scene (for example a [`Director::snapshot_to_scene`] result) into the
/// GUI's [`Library`](crate::gui::groups::Library) model. Returns the scene as
/// the library stored it (its own new id), or `None` when the name is invalid
/// or already used.
pub fn import_scene(
    library: &mut crate::gui::groups::Library,
    scene: &SceneConfig,
) -> Option<SceneConfig> {
    use crate::gui::groups::SceneItem;
    let id = library.add_scene(&scene.name)?;
    library.set_scene_color(id, scene.color.as_deref());
    library.set_scene_fades(id, scene.fade_in_ms, scene.fade_out_ms);
    library.set_scene_curve(id, scene.curve);
    library.set_scene_stop_others(id, scene.stop_others);
    for item in &scene.items {
        let Ok(sound) = uuid::Uuid::parse_str(&item.sound) else {
            continue;
        };
        library.set_item(
            id,
            SceneItem {
                sound,
                volume: item.volume,
                looped: item.looped,
                start: item.start,
                delay_ms: item.delay_ms,
                pan: item.pan,
            },
        );
    }
    library
        .to_config()
        .scenes
        .into_iter()
        .find(|s| s.id == id.to_string())
}

/// Scenes and catalog for [`Director::sync_library`], taken from the GUI's
/// library model. `resolve` maps a sound's path to the engine id it was loaded
/// under (sounds that are not loaded are left out of the catalog, so scene items
/// pointing at them are reported as missing).
pub fn sync_from_library(
    director: &mut Director,
    library: &crate::gui::groups::Library,
    resolve: impl Fn(&std::path::Path) -> Option<SoundId>,
) {
    let config = library.to_config();
    let sounds = library
        .sounds()
        .iter()
        .filter_map(|sound| {
            let engine_id = resolve(&sound.path)?;
            Some(CatalogSound {
                id: sound.id.to_string(),
                sound: engine_id,
                volume: sound.volume,
                looped: sound.looped,
                gain_db: sound.gain_db,
                bus: sound.bus,
                stinger: sound.oneshot_stinger,
                pan: sound.pan,
            })
        })
        .collect();
    director.sync_library(config.scenes, sounds);
    director.set_playlists(config.playlists);
}

/// An in-memory [`Playback`] with a simulated gain per track. Used by the unit
/// and integration tests; it makes no sound and needs no device.
pub mod mock {
    use std::collections::HashMap;

    use parking_lot::Mutex;

    use super::*;

    /// One call the mock received (volumes and fade times as given).
    #[derive(Debug, Clone, PartialEq)]
    pub enum MockCall {
        Play {
            sound: SoundId,
            looped: bool,
            volume: Option<f32>,
            fade_ms: u32,
        },
        OneShot {
            sound: SoundId,
            volume: f32,
        },
        Stop {
            sound: SoundId,
            fade_ms: u32,
        },
        FadeTo {
            sound: SoundId,
            volume: f32,
            fade_ms: u32,
        },
        SetVolume(SoundId, f32),
        SetLooped(SoundId, bool),
        SetBus(SoundId, Bus),
        StopAll {
            fade_ms: u32,
        },
        SetMaster(f32),
    }

    #[derive(Debug, Clone)]
    struct Sound {
        volume: f32,
        bus: Bus,
        pan: f32,
        duration_ms: Option<u32>,
    }

    #[derive(Debug, Clone)]
    struct Track {
        sound: SoundId,
        looped: bool,
        gain: f32,
        from: f32,
        target: f32,
        elapsed_ms: f32,
        total_ms: f32,
        /// Fading out to be dropped (reports as stopped at once).
        halting: bool,
    }

    #[derive(Default)]
    struct State {
        sounds: HashMap<SoundId, Sound>,
        tracks: Vec<Track>,
        master: f32,
        calls: Vec<MockCall>,
        fail: HashSet<SoundId>,
    }

    /// See the module docs.
    #[derive(Default)]
    pub struct MockPlayback {
        state: Mutex<State>,
    }

    impl MockPlayback {
        pub fn new() -> Self {
            let mock = Self::default();
            mock.state.lock().master = 1.0;
            mock
        }

        /// Register a sound (volume 1.0, not looped) and return its id.
        pub fn add_sound(&self) -> SoundId {
            let id = SoundId::new();
            self.state.lock().sounds.insert(
                id,
                Sound {
                    volume: 1.0,
                    bus: Bus::default(),
                    pan: 0.0,
                    duration_ms: None,
                },
            );
            id
        }

        /// Set the length [`Playback::duration_ms`] reports for the sound.
        pub fn set_duration(&self, sound: SoundId, duration_ms: Option<u32>) {
            if let Some(entry) = self.state.lock().sounds.get_mut(&sound) {
                entry.duration_ms = duration_ms;
            }
        }

        /// The pan most recently set for the sound (`0.0` when never set).
        pub fn pan_of(&self, sound: SoundId) -> Option<f32> {
            self.state.lock().sounds.get(&sound).map(|s| s.pan)
        }

        /// How many tracks of the sound play (fading-out tails excluded).
        pub fn playing_count(&self, sound: SoundId) -> usize {
            self.state
                .lock()
                .tracks
                .iter()
                .filter(|t| t.sound == sound && !t.halting)
                .count()
        }

        /// Make every call that starts this sound fail.
        pub fn fail_on(&self, sound: SoundId) {
            self.state.lock().fail.insert(sound);
        }

        /// Let `ms` of audio time pass: fades advance, finished fade-outs drop.
        pub fn advance(&self, ms: u32) {
            let mut state = self.state.lock();
            for track in &mut state.tracks {
                if track.total_ms > 0.0 {
                    track.elapsed_ms += ms as f32;
                    let progress = (track.elapsed_ms / track.total_ms).min(1.0);
                    track.gain = track.from + (track.target - track.from) * progress;
                    if progress >= 1.0 {
                        track.total_ms = 0.0;
                        track.gain = track.target;
                    }
                }
            }
            state
                .tracks
                .retain(|t| !(t.halting && t.total_ms == 0.0 && t.gain <= 0.0));
        }

        /// The loudest gain among the sound's tracks (0.0 when none).
        pub fn gain(&self, sound: SoundId) -> f32 {
            self.state
                .lock()
                .tracks
                .iter()
                .filter(|t| t.sound == sound)
                .map(|t| t.gain)
                .fold(0.0, f32::max)
        }

        /// How many tracks of the sound exist (fading tails included).
        pub fn track_count(&self, sound: SoundId) -> usize {
            self.state
                .lock()
                .tracks
                .iter()
                .filter(|t| t.sound == sound)
                .count()
        }

        /// The bus the sound was routed to.
        pub fn bus_of(&self, sound: SoundId) -> Option<Bus> {
            self.state.lock().sounds.get(&sound).map(|s| s.bus)
        }

        /// Every call so far.
        pub fn calls(&self) -> Vec<MockCall> {
            self.state.lock().calls.clone()
        }

        /// Forget the recorded calls.
        pub fn clear_calls(&self) {
            self.state.lock().calls.clear();
        }

        fn check(state: &State, sound: SoundId) -> PlaybackResult {
            if !state.sounds.contains_key(&sound) || state.fail.contains(&sound) {
                return Err(EngineError::SoundNotFound(sound));
            }
            Ok(())
        }

        fn halt(track: &mut Track, fade_ms: u32) {
            track.halting = true;
            track.from = track.gain;
            track.target = 0.0;
            track.elapsed_ms = 0.0;
            track.total_ms = fade_ms as f32;
            if fade_ms == 0 {
                track.gain = 0.0;
            }
        }
    }

    impl Playback for MockPlayback {
        fn playing_sounds(&self) -> Vec<SoundId> {
            let state = self.state.lock();
            let mut out = Vec::new();
            for track in state.tracks.iter().filter(|t| !t.halting) {
                if !out.contains(&track.sound) {
                    out.push(track.sound);
                }
            }
            out
        }

        fn is_playing(&self, sound: SoundId) -> bool {
            self.state
                .lock()
                .tracks
                .iter()
                .any(|t| t.sound == sound && !t.halting)
        }

        fn volume(&self, sound: SoundId) -> Option<f32> {
            self.state.lock().sounds.get(&sound).map(|s| s.volume)
        }

        fn is_looped(&self, sound: SoundId) -> bool {
            self.state
                .lock()
                .tracks
                .iter()
                .any(|t| t.sound == sound && !t.halting && t.looped)
        }

        fn set_volume(&self, sound: SoundId, volume: f32) -> PlaybackResult {
            let mut state = self.state.lock();
            Self::check(&state, sound)?;
            state.calls.push(MockCall::SetVolume(sound, volume));
            state.sounds.get_mut(&sound).expect("checked").volume = volume;
            for track in state
                .tracks
                .iter_mut()
                .filter(|t| t.sound == sound && !t.halting)
            {
                track.gain = volume;
                track.target = volume;
                track.total_ms = 0.0;
            }
            Ok(())
        }

        fn set_looped(&self, sound: SoundId, looped: bool) -> PlaybackResult {
            let mut state = self.state.lock();
            Self::check(&state, sound)?;
            state.calls.push(MockCall::SetLooped(sound, looped));
            for track in state
                .tracks
                .iter_mut()
                .filter(|t| t.sound == sound && !t.halting)
            {
                track.looped = looped;
            }
            Ok(())
        }

        fn set_pan(&self, sound: SoundId, pan: f32) -> PlaybackResult {
            let mut state = self.state.lock();
            Self::check(&state, sound)?;
            state.sounds.get_mut(&sound).expect("checked").pan = pan;
            Ok(())
        }

        fn duration_ms(&self, sound: SoundId) -> Option<u32> {
            self.state.lock().sounds.get(&sound)?.duration_ms
        }

        fn set_bus(&self, sound: SoundId, bus: Bus) -> PlaybackResult {
            let mut state = self.state.lock();
            Self::check(&state, sound)?;
            state.calls.push(MockCall::SetBus(sound, bus));
            state.sounds.get_mut(&sound).expect("checked").bus = bus;
            Ok(())
        }

        fn play(
            &self,
            sound: SoundId,
            looped: bool,
            volume: Option<f32>,
            fade_in: FadeSpec,
        ) -> PlaybackResult {
            let mut state = self.state.lock();
            Self::check(&state, sound)?;
            state.calls.push(MockCall::Play {
                sound,
                looped,
                volume,
                fade_ms: fade_in.ms,
            });
            let entry = state.sounds.get_mut(&sound).expect("checked");
            if let Some(volume) = volume {
                entry.volume = volume;
            }
            let target = entry.volume;
            state.tracks.push(Track {
                sound,
                looped,
                gain: if fade_in.ms == 0 { target } else { 0.0 },
                from: 0.0,
                target,
                elapsed_ms: 0.0,
                total_ms: fade_in.ms as f32,
                halting: false,
            });
            Ok(())
        }

        fn play_one_shot(&self, sound: SoundId, volume: f32) -> PlaybackResult {
            let mut state = self.state.lock();
            Self::check(&state, sound)?;
            state.calls.push(MockCall::OneShot { sound, volume });
            let playing = state.tracks.iter().any(|t| t.sound == sound && !t.halting);
            let entry = state.sounds.get_mut(&sound).expect("checked");
            if !playing {
                entry.volume = volume;
            }
            let target = entry.volume;
            state.tracks.push(Track {
                sound,
                looped: false,
                gain: target,
                from: target,
                target,
                elapsed_ms: 0.0,
                total_ms: 0.0,
                halting: false,
            });
            Ok(())
        }

        fn stop(&self, sound: SoundId, fade_out: FadeSpec) -> PlaybackResult {
            let mut state = self.state.lock();
            Self::check(&state, sound)?;
            state.calls.push(MockCall::Stop {
                sound,
                fade_ms: fade_out.ms,
            });
            for track in state
                .tracks
                .iter_mut()
                .filter(|t| t.sound == sound && !t.halting)
            {
                Self::halt(track, fade_out.ms);
            }
            state
                .tracks
                .retain(|t| !(t.halting && t.total_ms == 0.0 && t.gain <= 0.0));
            Ok(())
        }

        fn fade_to(&self, sound: SoundId, volume: f32, fade: FadeSpec) -> PlaybackResult {
            let mut state = self.state.lock();
            Self::check(&state, sound)?;
            state.calls.push(MockCall::FadeTo {
                sound,
                volume,
                fade_ms: fade.ms,
            });
            state.sounds.get_mut(&sound).expect("checked").volume = volume;
            for track in state
                .tracks
                .iter_mut()
                .filter(|t| t.sound == sound && !t.halting)
            {
                track.from = track.gain;
                track.target = volume;
                track.elapsed_ms = 0.0;
                track.total_ms = fade.ms as f32;
                if fade.ms == 0 {
                    track.gain = volume;
                }
            }
            Ok(())
        }

        fn stop_all(&self, fade_out: FadeSpec) -> PlaybackResult {
            let mut state = self.state.lock();
            state.calls.push(MockCall::StopAll {
                fade_ms: fade_out.ms,
            });
            for track in state.tracks.iter_mut().filter(|t| !t.halting) {
                Self::halt(track, fade_out.ms);
            }
            state
                .tracks
                .retain(|t| !(t.halting && t.total_ms == 0.0 && t.gain <= 0.0));
            Ok(())
        }

        fn master_volume(&self) -> f32 {
            self.state.lock().master
        }

        fn set_master_volume(&self, volume: f32) -> PlaybackResult {
            let mut state = self.state.lock();
            state.calls.push(MockCall::SetMaster(volume));
            state.master = volume;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{MockCall, MockPlayback};
    use super::*;

    struct Rig {
        pb: MockPlayback,
        director: Director,
        rain: SoundId,
        wind: SoundId,
        crow: SoundId,
        door: SoundId,
    }

    fn scene(id: &str, items: Vec<SceneItemConfig>) -> SceneConfig {
        SceneConfig {
            id: id.to_string(),
            name: id.to_string(),
            fade_in_ms: 1000,
            fade_out_ms: 2000,
            items,
            ..Default::default()
        }
    }

    fn item(sound: &str, volume: f32, looped: bool) -> SceneItemConfig {
        SceneItemConfig {
            sound: sound.to_string(),
            volume,
            looped,
            start: SceneStart::OnEnter,
            delay_ms: 0,
            pan: 0.0,
        }
    }

    fn rig() -> Rig {
        let pb = MockPlayback::new();
        let (rain, wind, crow, door) = (
            pb.add_sound(),
            pb.add_sound(),
            pb.add_sound(),
            pb.add_sound(),
        );
        let mut director = Director::new();
        let mut stinger = CatalogSound::new("door", door);
        stinger.stinger = true;
        director.sync_library(
            vec![
                scene(
                    "storm",
                    vec![item("rain", 0.8, true), item("wind", 0.4, true)],
                ),
                scene(
                    "calm",
                    vec![item("rain", 0.3, true), item("crow", 1.0, false)],
                ),
            ],
            vec![
                CatalogSound::new("rain", rain),
                CatalogSound::new("wind", wind),
                CatalogSound::new("crow", crow),
                stinger,
            ],
        );
        Rig {
            pb,
            director,
            rain,
            wind,
            crow,
            door,
        }
    }

    #[test]
    fn activation_plan_is_start_retune_stop() {
        let mut r = rig();
        let first = r.director.plan_scene(&r.pb, "storm").unwrap();
        assert_eq!(first.starts.len(), 2);
        assert!(first.retunes.is_empty() && first.stops.is_empty());
        assert_eq!(first.fade_in, FadeSpec::new(1000, FadeCurve::EqualPower));

        r.director.activate_scene(&r.pb, "storm").unwrap();
        r.pb.advance(1000);

        let plan = r.director.plan_scene(&r.pb, "calm").unwrap();
        // crow starts, rain is common (level 0.8 -> 0.3), wind is not in calm.
        assert_eq!(
            plan.starts.iter().map(|s| s.sound).collect::<Vec<_>>(),
            vec![r.crow]
        );
        assert_eq!(plan.retunes.len(), 1);
        assert_eq!(plan.retunes[0].sound, r.rain);
        assert_eq!(plan.retunes[0].volume, Some((0.8, 0.3)));
        assert_eq!(plan.stops, vec![r.wind]);
    }

    #[test]
    fn activation_uses_the_scene_fades() {
        let mut r = rig();
        r.director.activate_scene(&r.pb, "storm").unwrap();
        r.pb.advance(2000);
        r.pb.clear_calls();
        r.director.activate_scene(&r.pb, "calm").unwrap();
        let calls = r.pb.calls();
        assert!(calls.contains(&MockCall::Play {
            sound: r.crow,
            looped: false,
            volume: Some(1.0),
            fade_ms: 1000
        }));
        assert!(calls.contains(&MockCall::Stop {
            sound: r.wind,
            fade_ms: 2000
        }));
        // rain gets quieter: that is a fade-out, so the fade-out time.
        assert!(calls.contains(&MockCall::FadeTo {
            sound: r.rain,
            volume: 0.3,
            fade_ms: 2000
        }));
    }

    #[test]
    fn stop_others_false_keeps_the_rest_playing() {
        let mut r = rig();
        let mut calm = r.director.scene("calm").unwrap().clone();
        calm.stop_others = false;
        let storm = r.director.scene("storm").unwrap().clone();
        r.director.set_scenes(vec![storm, calm]);
        r.director.activate_scene(&r.pb, "storm").unwrap();
        r.pb.advance(1000);
        let report = r.director.activate_scene(&r.pb, "calm").unwrap();
        assert!(report.stopped.is_empty());
        r.pb.advance(3000);
        assert!(r.pb.is_playing(r.wind));
    }

    #[test]
    fn reactivating_mid_transition_continues_from_the_current_gain() {
        let mut r = rig();
        r.director.activate_scene(&r.pb, "storm").unwrap();
        r.pb.advance(500); // half way through the 1 s fade-in
        let mid = r.pb.gain(r.rain);
        assert!(mid > 0.2 && mid < 0.5, "rain is half in: {mid}");

        // Switch away and back in the middle of the transition.
        r.director.activate_scene(&r.pb, "calm").unwrap();
        r.director.activate_scene(&r.pb, "storm").unwrap();
        // No restart for rain: still one track and the same gain.
        assert_eq!(r.pb.track_count(r.rain), 1);
        assert!((r.pb.gain(r.rain) - mid).abs() < 1e-4);
        r.pb.advance(2000);
        assert!((r.pb.gain(r.rain) - 0.8).abs() < 1e-4);
    }

    #[test]
    fn stinger_does_not_stop_the_scene_and_survives_the_next_one() {
        let mut r = rig();
        r.director.activate_scene(&r.pb, "storm").unwrap();
        r.pb.advance(1000);
        r.director.play_stinger(&r.pb, r.door).unwrap();
        assert!(r.pb.is_playing(r.door));
        assert!(r.pb.is_playing(r.rain) && r.pb.is_playing(r.wind));
        assert_eq!(r.director.active_scene(), Some("storm"));

        let report = r.director.activate_scene(&r.pb, "calm").unwrap();
        assert!(!report.stopped.contains(&r.door));
        assert!(r.pb.is_playing(r.door));
    }

    #[test]
    fn stinger_not_flagged_in_the_catalog_is_still_spared_while_it_sounds() {
        let mut r = rig();
        r.director.activate_scene(&r.pb, "calm").unwrap();
        // `wind` is not a stinger; layer it as one anyway.
        r.director.play_stinger(&r.pb, r.wind).unwrap();
        let report = r.director.activate_scene(&r.pb, "calm").unwrap();
        assert!(report.stopped.is_empty());
    }

    #[test]
    fn snapshot_captures_playing_sounds_and_replays_the_same_set() {
        let mut r = rig();
        r.director.activate_scene(&r.pb, "storm").unwrap();
        r.pb.advance(1000);
        r.director.play_stinger(&r.pb, r.door).unwrap();

        let snap = r.director.snapshot_to_scene(&r.pb, "  My mix ").unwrap();
        assert_eq!(snap.name, "My mix");
        let mut ids: Vec<_> = snap.items.iter().map(|i| i.sound.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["rain", "wind"], "the stinger is not part of it");
        let rain = snap.items.iter().find(|i| i.sound == "rain").unwrap();
        assert!((rain.volume - 0.8).abs() < 1e-4 && rain.looped);

        // Stop everything and play the snapshot: the same sounds come back.
        r.director.stop_all(&r.pb).unwrap();
        assert!(r.pb.playing_sounds().is_empty());
        r.director.activate_scene(&r.pb, &snap.id).unwrap();
        r.pb.advance(2000);
        let mut playing = r.pb.playing_sounds();
        playing.sort_by_key(|s| s.0);
        let mut expected = vec![r.rain, r.wind];
        expected.sort_by_key(|s| s.0);
        assert_eq!(playing, expected);
        assert!((r.pb.gain(r.rain) - 0.8).abs() < 1e-4);
        assert!((r.pb.gain(r.wind) - 0.4).abs() < 1e-4);
    }

    #[test]
    fn snapshot_needs_a_name_and_something_playing() {
        let mut r = rig();
        assert!(matches!(
            r.director.snapshot_to_scene(&r.pb, "  "),
            Err(DirectorError::EmptyName)
        ));
        assert!(matches!(
            r.director.snapshot_to_scene(&r.pb, "x"),
            Err(DirectorError::NothingPlaying)
        ));
    }

    #[test]
    fn manual_items_are_not_started_and_delays_wait_for_the_tick() {
        let mut r = rig();
        let mut manual = item("crow", 1.0, false);
        manual.start = SceneStart::Manual;
        let mut late = item("wind", 0.5, true);
        late.delay_ms = 1500;
        r.director.set_scenes(vec![scene(
            "mixed",
            vec![item("rain", 0.5, true), manual, late],
        )]);

        let t0 = Instant::now();
        let report = r.director.activate_scene_at(&r.pb, "mixed", t0).unwrap();
        assert_eq!(report.started, vec![r.rain]);
        assert_eq!(report.scheduled, vec![r.wind]);
        assert!(!r.pb.is_playing(r.crow), "manual: never automatic");
        assert!(!r.pb.is_playing(r.wind));
        assert_eq!(r.director.state().pending_starts, 1);
        assert_eq!(
            r.director.next_deadline(),
            Some(t0 + Duration::from_millis(1500))
        );

        r.director.tick_at(&r.pb, t0 + Duration::from_millis(1000));
        assert!(!r.pb.is_playing(r.wind), "not due yet");
        r.director.tick_at(&r.pb, t0 + Duration::from_millis(1500));
        assert!(r.pb.is_playing(r.wind));
        assert_eq!(r.director.state().pending_starts, 0);
        assert_eq!(r.director.next_deadline(), None);
    }

    #[test]
    fn a_manual_item_that_plays_is_spared_by_stop_others() {
        let mut r = rig();
        let mut manual = item("crow", 1.0, false);
        manual.start = SceneStart::Manual;
        r.director
            .set_scenes(vec![scene("a", vec![item("rain", 0.5, true), manual])]);
        r.pb.play(r.crow, false, None, FadeSpec::NONE).unwrap();
        let report = r.director.activate_scene(&r.pb, "a").unwrap();
        assert!(report.stopped.is_empty());
    }

    #[test]
    fn leaving_a_scene_cancels_its_pending_starts() {
        let mut r = rig();
        let mut late = item("wind", 0.5, true);
        late.delay_ms = 1000;
        r.director.set_scenes(vec![
            scene("late", vec![late]),
            scene("other", vec![item("rain", 1.0, false)]),
        ]);
        let t0 = Instant::now();
        r.director.activate_scene_at(&r.pb, "late", t0).unwrap();
        r.director.activate_scene_at(&r.pb, "other", t0).unwrap();
        r.director.tick_at(&r.pb, t0 + Duration::from_secs(5));
        assert!(!r.pb.is_playing(r.wind));
    }

    #[test]
    fn reactivating_keeps_the_original_delay() {
        let mut r = rig();
        let mut late = item("wind", 0.5, true);
        late.delay_ms = 1000;
        r.director.set_scenes(vec![scene("late", vec![late])]);
        let t0 = Instant::now();
        r.director.activate_scene_at(&r.pb, "late", t0).unwrap();
        r.director
            .activate_scene_at(&r.pb, "late", t0 + Duration::from_millis(600))
            .unwrap();
        assert_eq!(
            r.director.next_deadline(),
            Some(t0 + Duration::from_millis(1000))
        );
    }

    #[test]
    fn stop_scene_fades_the_scene_and_spares_the_rest() {
        let mut r = rig();
        r.director.activate_scene(&r.pb, "storm").unwrap();
        r.pb.advance(1000);
        r.pb.play(r.crow, false, None, FadeSpec::NONE).unwrap();
        r.director.play_stinger(&r.pb, r.door).unwrap();
        let report = r.director.stop_scene(&r.pb).unwrap();
        assert_eq!(report.stopped.len(), 2);
        assert_eq!(r.director.active_scene(), None);
        assert!(r.pb.is_playing(r.crow) && r.pb.is_playing(r.door));
        assert!(!r.pb.is_playing(r.rain));
        // The tail still sounds until the fade-out is over.
        assert!(r.pb.gain(r.rain) > 0.0);
        r.pb.advance(2000);
        assert_eq!(r.pb.gain(r.rain), 0.0);
    }

    #[test]
    fn stop_all_with_fade_takes_everything_down() {
        let mut r = rig();
        r.director.activate_scene(&r.pb, "storm").unwrap();
        r.pb.advance(1000);
        r.director.play_stinger(&r.pb, r.door).unwrap();
        r.director.stop_all_with_fade(&r.pb, 1500).unwrap();
        assert_eq!(r.director.active_scene(), None);
        assert!(r.pb.playing_sounds().is_empty());
        assert!(r.pb.gain(r.rain) > 0.0, "fading, not cut");
        r.pb.advance(1500);
        assert_eq!(r.pb.gain(r.rain), 0.0);
        assert!(r.pb.calls().contains(&MockCall::StopAll { fade_ms: 1500 }));
    }

    #[test]
    fn unknown_scene_and_unloaded_sounds_are_reported() {
        let mut r = rig();
        assert!(matches!(
            r.director.activate_scene(&r.pb, "nope"),
            Err(DirectorError::UnknownScene(_))
        ));
        r.director.set_scenes(vec![scene(
            "x",
            vec![item("rain", 1.0, false), item("ghost", 1.0, false)],
        )]);
        let report = r.director.activate_scene(&r.pb, "x").unwrap();
        assert_eq!(report.missing, vec!["ghost".to_string()]);
        assert_eq!(report.started, vec![r.rain]);
    }

    #[test]
    fn one_failing_sound_does_not_stop_the_scene() {
        let mut r = rig();
        r.pb.fail_on(r.wind);
        let report = r.director.activate_scene(&r.pb, "storm").unwrap();
        assert_eq!(report.started, vec![r.rain]);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(r.director.active_scene(), Some("storm"));
    }

    #[test]
    fn state_is_published_to_subscribers() {
        let mut r = rig();
        let mut rx = r.director.subscribe();
        let before = rx.borrow_and_update().revision;
        r.director.activate_scene(&r.pb, "storm").unwrap();
        assert!(rx.has_changed().unwrap());
        let state = rx.borrow_and_update().clone();
        assert!(state.revision > before);
        assert_eq!(state.active_scene.as_deref(), Some("storm"));
        assert_eq!(state.playing.len(), 2);

        // No change, no new revision.
        let revision = state.revision;
        r.director.tick(&r.pb);
        assert_eq!(r.director.state().revision, revision);

        r.director.stop_all(&r.pb).unwrap();
        let state = r.director.state();
        assert_eq!(state.active_scene, None);
        assert!(state.playing.is_empty());
    }

    #[test]
    fn deleting_the_active_scene_clears_it() {
        let mut r = rig();
        r.director.activate_scene(&r.pb, "storm").unwrap();
        let calm = r.director.scene("calm").unwrap().clone();
        r.director.set_scenes(vec![calm]);
        assert_eq!(r.director.active_scene(), None);
        assert_eq!(r.director.state().active_scene, None);
    }

    #[test]
    fn gain_db_is_applied_to_scene_levels_and_undone_in_snapshots() {
        let mut r = rig();
        let mut quiet = CatalogSound::new("rain", r.rain);
        quiet.gain_db = -6.0;
        r.director.register_sound(quiet);
        r.director.activate_scene(&r.pb, "storm").unwrap();
        r.pb.advance(1000);
        let expected = 0.8 * 10f32.powf(-6.0 / 20.0);
        assert!((r.pb.gain(r.rain) - expected).abs() < 1e-3);
        let snap = r.director.snapshot_to_scene(&r.pb, "s").unwrap();
        let rain = snap.items.iter().find(|i| i.sound == "rain").unwrap();
        assert!((rain.volume - 0.8).abs() < 1e-3);
    }

    #[test]
    fn manual_controls_track_the_catalog() {
        let mut r = rig();
        r.director.set_sound_volume(&r.pb, r.rain, 0.25).unwrap();
        assert_eq!(r.director.catalog_sound(r.rain).unwrap().volume, 0.25);
        assert!(r.director.set_sound_volume(&r.pb, r.rain, 1.5).is_err());
        r.director.set_sound_looped(&r.pb, r.rain, true).unwrap();
        assert!(r.director.catalog_sound(r.rain).unwrap().looped);

        r.director.play_sound(&r.pb, r.rain, true).unwrap();
        assert!(r.pb.is_playing(r.rain) && r.pb.is_looped(r.rain));
        // Restart, not layering.
        r.director.play_sound(&r.pb, r.rain, true).unwrap();
        assert_eq!(r.pb.track_count(r.rain), 1);
        // A stinger-flagged sound layers instead.
        r.director.play_sound(&r.pb, r.door, false).unwrap();
        r.director.play_sound(&r.pb, r.door, false).unwrap();
        assert_eq!(r.pb.track_count(r.door), 2);
        r.director.stop_sound(&r.pb, r.rain).unwrap();
        assert!(!r.pb.is_playing(r.rain));
    }

    #[test]
    fn execute_runs_commands() {
        let mut r = rig();
        let out = r
            .director
            .execute(&r.pb, DirectorCommand::ActivateScene("storm".into()))
            .unwrap();
        assert!(matches!(out, DirectorOutcome::Scene(rep) if rep.started.len() == 2));
        r.pb.advance(1000);
        let out = r
            .director
            .execute(&r.pb, DirectorCommand::SnapshotToScene("Copy".into()))
            .unwrap();
        assert!(matches!(out, DirectorOutcome::Snapshot(s) if s.items.len() == 2));
        r.director.execute(&r.pb, DirectorCommand::StopAll).unwrap();
        assert!(r.pb.playing_sounds().is_empty());
    }

    #[test]
    fn bus_is_applied_when_a_catalog_sound_starts() {
        let mut r = rig();
        let mut rain = CatalogSound::new("rain", r.rain);
        rain.bus = Bus::Music;
        r.director.register_sound(rain);
        r.director.activate_scene(&r.pb, "storm").unwrap();
        assert_eq!(r.pb.bus_of(r.rain), Some(Bus::Music));
    }

    #[test]
    fn import_scene_into_the_library_model() {
        use crate::gui::groups::Library;
        let mut lib = Library::new();
        lib.insert_sound(std::path::Path::new("/fake/a.wav"), "A", 1.0, None);
        let a = lib.id_of(std::path::Path::new("/fake/a.wav")).unwrap();
        let scene = SceneConfig {
            id: "x".into(),
            name: "Copy".into(),
            items: vec![item(&a.to_string(), 0.5, true)],
            ..Default::default()
        };
        let stored = import_scene(&mut lib, &scene).unwrap();
        assert_eq!(stored.items.len(), 1);
        assert_ne!(stored.id, "x");
        assert!(
            import_scene(&mut lib, &scene).is_none(),
            "name already used"
        );
    }
}
