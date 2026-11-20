//! Test helpers (used by the unit tests and `tests/hotkeys.rs`; not API).

use parking_lot::Mutex;

use crate::audio::director::mock::MockPlayback;
use crate::audio::director::{FadeSpec, Playback, PlaybackResult};
use crate::audio::engine::SoundId;
use crate::audio::mixer::DuckSettings;
use crate::config::Bus;

/// A [`MockPlayback`] that also records `duck_bus` / `release_duck` calls
/// (the mock ignores them).
#[derive(Default)]
pub struct SpyPlayback {
    /// The wrapped mock: add sounds, read calls and gains from here.
    pub inner: MockPlayback,
    ducks: Mutex<Vec<String>>,
}

impl SpyPlayback {
    /// A spy over a fresh mock.
    pub fn new() -> Self {
        Self {
            inner: MockPlayback::new(),
            ducks: Mutex::new(Vec::new()),
        }
    }

    /// The duck calls so far: `"duck Music hold=None"`, `"release Music"`.
    pub fn duck_log(&self) -> Vec<String> {
        self.ducks.lock().clone()
    }

    /// Forget the duck calls.
    pub fn clear_duck_log(&self) {
        self.ducks.lock().clear();
    }
}

impl Playback for SpyPlayback {
    fn playing_sounds(&self) -> Vec<SoundId> {
        self.inner.playing_sounds()
    }
    fn is_playing(&self, sound: SoundId) -> bool {
        self.inner.is_playing(sound)
    }
    fn volume(&self, sound: SoundId) -> Option<f32> {
        self.inner.volume(sound)
    }
    fn is_looped(&self, sound: SoundId) -> bool {
        self.inner.is_looped(sound)
    }
    fn set_volume(&self, sound: SoundId, volume: f32) -> PlaybackResult {
        self.inner.set_volume(sound, volume)
    }
    fn set_looped(&self, sound: SoundId, looped: bool) -> PlaybackResult {
        self.inner.set_looped(sound, looped)
    }
    fn set_pan(&self, sound: SoundId, pan: f32) -> PlaybackResult {
        self.inner.set_pan(sound, pan)
    }
    fn set_bus(&self, sound: SoundId, bus: Bus) -> PlaybackResult {
        self.inner.set_bus(sound, bus)
    }
    fn play(
        &self,
        sound: SoundId,
        looped: bool,
        volume: Option<f32>,
        fade_in: FadeSpec,
    ) -> PlaybackResult {
        self.inner.play(sound, looped, volume, fade_in)
    }
    fn play_one_shot(&self, sound: SoundId, volume: f32) -> PlaybackResult {
        self.inner.play_one_shot(sound, volume)
    }
    fn stop(&self, sound: SoundId, fade_out: FadeSpec) -> PlaybackResult {
        self.inner.stop(sound, fade_out)
    }
    fn fade_to(&self, sound: SoundId, volume: f32, fade: FadeSpec) -> PlaybackResult {
        self.inner.fade_to(sound, volume, fade)
    }
    fn stop_all(&self, fade_out: FadeSpec) -> PlaybackResult {
        self.inner.stop_all(fade_out)
    }
    fn master_volume(&self) -> f32 {
        self.inner.master_volume()
    }
    fn set_master_volume(&self, volume: f32) -> PlaybackResult {
        self.inner.set_master_volume(volume)
    }
    fn duck_bus(&self, bus: Bus, settings: DuckSettings) {
        self.ducks
            .lock()
            .push(format!("duck {bus:?} hold={:?}", settings.hold_ms));
    }
    fn release_duck(&self, bus: Bus) {
        self.ducks.lock().push(format!("release {bus:?}"));
    }
}
