//! Running a [`HotkeyAction`] against the Director.
//!
//! [`HotkeyDispatcher`] is the only place that knows what each action means.
//! It holds the little state some actions need (the volume before a mute,
//! whether a duck is latched, whether a hold-key is down) and is otherwise a
//! thin layer over `Director` + [`Playback`], so it can be driven by
//! `MockPlayback` in tests.
//!
//! Actions the Director does not own (playlists: start, next, previous,
//! pause) are forwarded to a [`HotkeyExtras`] implementation supplied by
//! whoever owns the playlists; [`NoExtras`] refuses them with
//! [`DispatchError::Unsupported`].

use thiserror::Error;

use crate::audio::director::{Director, DirectorError, Playback, SceneReport};
use crate::audio::mixer::DuckSettings;
use crate::config::{Bus, HotkeyAction};

use super::map::{HotkeyEvent, KeyPhase};

/// What a dispatch can fail with.
#[derive(Debug, Error)]
pub enum DispatchError {
    /// The action's target id is not in the library (sound deleted, stale
    /// config).
    #[error("no sound with id {0} in the library")]
    UnknownSound(String),
    /// The Director refused (unknown scene, engine error).
    #[error("{0}")]
    Director(#[from] DirectorError),
    /// Nothing handles this action in this build (see [`HotkeyExtras`]).
    #[error("the action {0} is not available yet")]
    Unsupported(&'static str),
    /// A [`HotkeyExtras`] implementation failed.
    #[error("{0}")]
    Extras(String),
}

/// What a dispatch did, for a status line or a log.
#[derive(Debug, Clone, PartialEq)]
pub enum DispatchOutcome {
    /// The action ran.
    Done,
    /// A scene was activated.
    Scene(SceneReport),
    /// Nothing to do (a key release of an action that only acts on press, a
    /// repeated press of a held key).
    Ignored,
}

/// Playlist operations. The Director has no playlists, so the owner of the
/// playlist engine implements this and passes it to
/// [`HotkeyDispatcher::handle`]. Every method defaults to "unsupported".
pub trait HotkeyExtras {
    /// Start (or restart) a playlist by library id.
    fn start_playlist(&mut self, _id: &str) -> Result<(), DispatchError> {
        Err(DispatchError::Unsupported("playlist"))
    }
    /// Skip to the next track.
    fn next_track(&mut self) -> Result<(), DispatchError> {
        Err(DispatchError::Unsupported("next_track"))
    }
    /// Go back to the previous track.
    fn prev_track(&mut self) -> Result<(), DispatchError> {
        Err(DispatchError::Unsupported("prev_track"))
    }
    /// Pause / resume the music.
    fn toggle_pause_music(&mut self) -> Result<(), DispatchError> {
        Err(DispatchError::Unsupported("pause_music"))
    }
}

/// A [`HotkeyExtras`] without playlists.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoExtras;

impl HotkeyExtras for NoExtras {}

/// Master volume change of one `MasterUp` / `MasterDown` press.
pub const DEFAULT_MASTER_STEP: f32 = 0.05;
/// Master volume restored by an unmute when the volume before the mute is
/// unknown (the mute came from somewhere else).
const FALLBACK_UNMUTED_VOLUME: f32 = 0.8;

/// Runs hotkey actions. Create one per console and keep it: it remembers the
/// pre-mute volume and the duck latch between presses.
#[derive(Debug, Clone)]
pub struct HotkeyDispatcher {
    /// Master volume step of `MasterUp` / `MasterDown` (`0.0..=1.0`).
    pub master_step: f32,
    /// How the background buses are ducked by `DuckToggle` / `DuckHold`.
    pub duck: DuckSettings,
    /// The buses that duck (the background: music and ambience).
    pub duck_buses: Vec<Bus>,
    muted_from: Option<f32>,
    duck_latched: bool,
    hold_down: bool,
}

impl Default for HotkeyDispatcher {
    fn default() -> Self {
        Self {
            master_step: DEFAULT_MASTER_STEP,
            duck: DuckSettings::default(),
            duck_buses: vec![Bus::Music, Bus::Ambience],
            muted_from: None,
            duck_latched: false,
            hold_down: false,
        }
    }
}

impl HotkeyDispatcher {
    /// A dispatcher with the default step and duck.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `DuckToggle` currently holds the background down.
    pub fn is_duck_latched(&self) -> bool {
        self.duck_latched
    }

    /// Run the action of an event.
    pub fn handle<P: Playback + ?Sized>(
        &mut self,
        director: &mut Director,
        pb: &P,
        extras: &mut dyn HotkeyExtras,
        event: &HotkeyEvent,
    ) -> Result<DispatchOutcome, DispatchError> {
        self.dispatch(director, pb, extras, &event.action, event.phase)
    }

    /// Run `action` for a key `phase`. Everything except `DuckHold` acts on
    /// the press and ignores the release.
    pub fn dispatch<P: Playback + ?Sized>(
        &mut self,
        director: &mut Director,
        pb: &P,
        extras: &mut dyn HotkeyExtras,
        action: &HotkeyAction,
        phase: KeyPhase,
    ) -> Result<DispatchOutcome, DispatchError> {
        if let HotkeyAction::DuckHold = action {
            return Ok(self.duck_hold(pb, phase));
        }
        if phase == KeyPhase::Released {
            return Ok(DispatchOutcome::Ignored);
        }
        match action {
            HotkeyAction::Sound { target } => {
                let entry = director
                    .catalog()
                    .iter()
                    .find(|entry| &entry.id == target)
                    .cloned()
                    .ok_or_else(|| DispatchError::UnknownSound(target.clone()))?;
                if entry.stinger {
                    director.play_stinger(pb, entry.sound)?;
                } else if pb.is_playing(entry.sound) {
                    director.stop_sound(pb, entry.sound)?;
                } else {
                    director.play_sound(pb, entry.sound, entry.looped)?;
                }
            }
            HotkeyAction::Scene { target } => {
                return Ok(DispatchOutcome::Scene(director.activate_scene(pb, target)?));
            }
            HotkeyAction::Playlist { target } => extras.start_playlist(target)?,
            HotkeyAction::StopAll => {
                let fade_ms = director
                    .active_scene()
                    .and_then(|id| director.scene(id))
                    .map_or(0, |scene| scene.fade_out_ms);
                director.stop_all_with_fade(pb, fade_ms)?;
            }
            HotkeyAction::StopScene => {
                director.stop_scene(pb)?;
            }
            HotkeyAction::MasterUp => self.step_master(director, pb, self.master_step)?,
            HotkeyAction::MasterDown => self.step_master(director, pb, -self.master_step)?,
            HotkeyAction::MasterMute => self.toggle_mute(director, pb)?,
            HotkeyAction::DuckToggle => {
                if self.duck_latched {
                    self.release_duck(pb);
                } else {
                    self.apply_duck(pb);
                }
                self.duck_latched = !self.duck_latched;
            }
            HotkeyAction::DuckHold => unreachable!("handled above"),
            HotkeyAction::NextTrack => extras.next_track()?,
            HotkeyAction::PrevTrack => extras.prev_track()?,
            HotkeyAction::PauseMusic => extras.toggle_pause_music()?,
        }
        Ok(DispatchOutcome::Done)
    }

    fn step_master<P: Playback + ?Sized>(
        &mut self,
        director: &mut Director,
        pb: &P,
        delta: f32,
    ) -> Result<(), DispatchError> {
        let next = (pb.master_volume() + delta).clamp(0.0, 1.0);
        // Hundredths: repeated +/-0.05 steps must not drift away from 0.0/1.0.
        let next = (next * 100.0).round() / 100.0;
        director.set_master_volume(pb, next)?;
        if next > 0.0 {
            self.muted_from = None;
        }
        Ok(())
    }

    fn toggle_mute<P: Playback + ?Sized>(
        &mut self,
        director: &mut Director,
        pb: &P,
    ) -> Result<(), DispatchError> {
        let current = pb.master_volume();
        if current > 0.0 {
            self.muted_from = Some(current);
            director.set_master_volume(pb, 0.0)?;
        } else {
            let restore = self.muted_from.take().unwrap_or(FALLBACK_UNMUTED_VOLUME);
            director.set_master_volume(pb, restore)?;
        }
        Ok(())
    }

    fn duck_hold<P: Playback + ?Sized>(&mut self, pb: &P, phase: KeyPhase) -> DispatchOutcome {
        match phase {
            KeyPhase::Pressed if !self.hold_down => {
                self.hold_down = true;
                self.apply_duck(pb);
                DispatchOutcome::Done
            }
            KeyPhase::Released if self.hold_down => {
                self.hold_down = false;
                // A latched toggle keeps the background down.
                if !self.duck_latched {
                    self.release_duck(pb);
                }
                DispatchOutcome::Done
            }
            _ => DispatchOutcome::Ignored,
        }
    }

    fn apply_duck<P: Playback + ?Sized>(&self, pb: &P) {
        // Held until released: no automatic release timer.
        let settings = DuckSettings {
            hold_ms: None,
            ..self.duck
        };
        for bus in &self.duck_buses {
            pb.duck_bus(*bus, settings);
        }
    }

    fn release_duck<P: Playback + ?Sized>(&self, pb: &P) {
        for bus in &self.duck_buses {
            pb.release_duck(*bus);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::director::mock::MockCall;
    use crate::audio::director::CatalogSound;
    use crate::config::{SceneConfig, SceneItemConfig, SceneStart};
    use crate::hotkeys::testing::SpyPlayback as Spy;
    use crate::hotkeys::Source;

    #[derive(Default)]
    struct Recorder {
        calls: Vec<String>,
    }

    impl HotkeyExtras for Recorder {
        fn start_playlist(&mut self, id: &str) -> Result<(), DispatchError> {
            self.calls.push(format!("playlist {id}"));
            Ok(())
        }
        fn next_track(&mut self) -> Result<(), DispatchError> {
            self.calls.push("next".into());
            Ok(())
        }
        fn prev_track(&mut self) -> Result<(), DispatchError> {
            self.calls.push("prev".into());
            Ok(())
        }
        fn toggle_pause_music(&mut self) -> Result<(), DispatchError> {
            self.calls.push("pause".into());
            Ok(())
        }
    }

    fn rain_item() -> SceneItemConfig {
        SceneItemConfig {
            sound: "rain".into(),
            volume: 1.0,
            looped: false,
            start: SceneStart::OnEnter,
            delay_ms: 0,
        }
    }

    fn setup() -> (Director, Spy, crate::audio::engine::SoundId) {
        let pb = Spy::new();
        let sound = pb.inner.add_sound();
        let mut director = Director::new();
        director.register_sound(CatalogSound::new("rain", sound));
        (director, pb, sound)
    }

    fn press(
        d: &mut HotkeyDispatcher,
        dir: &mut Director,
        pb: &Spy,
        action: HotkeyAction,
    ) -> Result<DispatchOutcome, DispatchError> {
        d.dispatch(dir, pb, &mut NoExtras, &action, KeyPhase::Pressed)
    }

    #[test]
    fn sound_toggles() {
        let (mut dir, pb, sound) = setup();
        let mut d = HotkeyDispatcher::new();
        let action = HotkeyAction::Sound {
            target: "rain".into(),
        };
        press(&mut d, &mut dir, &pb, action.clone()).unwrap();
        assert!(pb.is_playing(sound));
        press(&mut d, &mut dir, &pb, action).unwrap();
        assert!(!pb.is_playing(sound));
    }

    #[test]
    fn stinger_sound_fires_one_shot_and_never_toggles_off() {
        let pb = Spy::new();
        let sound = pb.inner.add_sound();
        let mut dir = Director::new();
        let mut entry = CatalogSound::new("boom", sound);
        entry.stinger = true;
        dir.register_sound(entry);
        let mut d = HotkeyDispatcher::new();
        let action = HotkeyAction::Sound {
            target: "boom".into(),
        };
        pb.inner.clear_calls();
        press(&mut d, &mut dir, &pb, action.clone()).unwrap();
        assert!(pb
            .inner
            .calls()
            .iter()
            .any(|c| matches!(c, MockCall::OneShot { .. })));
        press(&mut d, &mut dir, &pb, action).unwrap();
        assert!(pb.is_playing(sound), "a stinger is layered, not toggled");
    }

    #[test]
    fn unknown_sound_and_scene_are_errors() {
        let (mut dir, pb, _) = setup();
        let mut d = HotkeyDispatcher::new();
        let err = press(
            &mut d,
            &mut dir,
            &pb,
            HotkeyAction::Sound {
                target: "nope".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, DispatchError::UnknownSound(_)));
        let err = press(
            &mut d,
            &mut dir,
            &pb,
            HotkeyAction::Scene {
                target: "nope".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DispatchError::Director(DirectorError::UnknownScene(_))
        ));
    }

    #[test]
    fn scene_activation_and_stop_all_use_the_scene_fade() {
        let (mut dir, pb, sound) = setup();
        let scene = SceneConfig {
            id: "tavern".into(),
            name: "Tavern".into(),
            fade_in_ms: 0,
            fade_out_ms: 1500,
            items: vec![rain_item()],
            ..SceneConfig::default()
        };
        dir.set_scenes(vec![scene]);
        let mut d = HotkeyDispatcher::new();
        let out = press(
            &mut d,
            &mut dir,
            &pb,
            HotkeyAction::Scene {
                target: "tavern".into(),
            },
        )
        .unwrap();
        assert!(matches!(out, DispatchOutcome::Scene(_)));
        assert_eq!(dir.active_scene(), Some("tavern"));
        assert!(pb.is_playing(sound));

        pb.inner.clear_calls();
        press(&mut d, &mut dir, &pb, HotkeyAction::StopAll).unwrap();
        assert_eq!(dir.active_scene(), None);
        assert!(pb
            .inner
            .calls()
            .contains(&MockCall::StopAll { fade_ms: 1500 }));
    }

    #[test]
    fn stop_scene_clears_the_active_scene() {
        let (mut dir, pb, _) = setup();
        let scene = SceneConfig {
            id: "s".into(),
            name: "S".into(),
            items: vec![rain_item()],
            ..SceneConfig::default()
        };
        dir.set_scenes(vec![scene]);
        let mut d = HotkeyDispatcher::new();
        press(
            &mut d,
            &mut dir,
            &pb,
            HotkeyAction::Scene { target: "s".into() },
        )
        .unwrap();
        press(&mut d, &mut dir, &pb, HotkeyAction::StopScene).unwrap();
        assert_eq!(dir.active_scene(), None);
    }

    #[test]
    fn master_steps_clamp_and_mute_restores() {
        let (mut dir, pb, _) = setup();
        let mut d = HotkeyDispatcher::new();
        pb.set_master_volume(0.5).unwrap();
        press(&mut d, &mut dir, &pb, HotkeyAction::MasterUp).unwrap();
        assert!((pb.master_volume() - 0.55).abs() < 1e-6);
        press(&mut d, &mut dir, &pb, HotkeyAction::MasterDown).unwrap();
        press(&mut d, &mut dir, &pb, HotkeyAction::MasterDown).unwrap();
        assert!((pb.master_volume() - 0.45).abs() < 1e-6);
        for _ in 0..30 {
            press(&mut d, &mut dir, &pb, HotkeyAction::MasterDown).unwrap();
        }
        assert_eq!(pb.master_volume(), 0.0);
        for _ in 0..30 {
            press(&mut d, &mut dir, &pb, HotkeyAction::MasterUp).unwrap();
        }
        assert_eq!(pb.master_volume(), 1.0);

        pb.set_master_volume(0.6).unwrap();
        press(&mut d, &mut dir, &pb, HotkeyAction::MasterMute).unwrap();
        assert_eq!(pb.master_volume(), 0.0);
        press(&mut d, &mut dir, &pb, HotkeyAction::MasterMute).unwrap();
        assert!((pb.master_volume() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn unmute_without_memory_uses_a_fallback() {
        let (mut dir, pb, _) = setup();
        let mut d = HotkeyDispatcher::new();
        pb.set_master_volume(0.0).unwrap();
        press(&mut d, &mut dir, &pb, HotkeyAction::MasterMute).unwrap();
        assert!(pb.master_volume() > 0.0);
    }

    #[test]
    fn duck_toggle_latches() {
        let (mut dir, pb, _) = setup();
        let mut d = HotkeyDispatcher::new();
        press(&mut d, &mut dir, &pb, HotkeyAction::DuckToggle).unwrap();
        assert!(d.is_duck_latched());
        assert_eq!(
            pb.duck_log(),
            vec!["duck Music hold=None", "duck Ambience hold=None"]
        );
        pb.clear_duck_log();
        press(&mut d, &mut dir, &pb, HotkeyAction::DuckToggle).unwrap();
        assert!(!d.is_duck_latched());
        assert_eq!(pb.duck_log(), vec!["release Music", "release Ambience"]);
    }

    #[test]
    fn duck_hold_follows_the_key() {
        let (mut dir, pb, _) = setup();
        let mut d = HotkeyDispatcher::new();
        let hold = HotkeyAction::DuckHold;
        let run = |d: &mut HotkeyDispatcher, dir: &mut Director, phase| {
            d.dispatch(dir, &pb, &mut NoExtras, &hold, phase).unwrap()
        };
        assert_eq!(
            run(&mut d, &mut dir, KeyPhase::Pressed),
            DispatchOutcome::Done
        );
        // Key repeat: no second duck.
        assert_eq!(
            run(&mut d, &mut dir, KeyPhase::Pressed),
            DispatchOutcome::Ignored
        );
        assert_eq!(pb.duck_log().len(), 2);
        assert_eq!(
            run(&mut d, &mut dir, KeyPhase::Released),
            DispatchOutcome::Done
        );
        assert_eq!(pb.duck_log().len(), 4);
        assert!(pb.duck_log()[3].starts_with("release"));
        // A stray release does nothing.
        assert_eq!(
            run(&mut d, &mut dir, KeyPhase::Released),
            DispatchOutcome::Ignored
        );
    }

    #[test]
    fn hold_release_keeps_a_latched_duck() {
        let (mut dir, pb, _) = setup();
        let mut d = HotkeyDispatcher::new();
        press(&mut d, &mut dir, &pb, HotkeyAction::DuckToggle).unwrap();
        pb.clear_duck_log();
        d.dispatch(
            &mut dir,
            &pb,
            &mut NoExtras,
            &HotkeyAction::DuckHold,
            KeyPhase::Pressed,
        )
        .unwrap();
        d.dispatch(
            &mut dir,
            &pb,
            &mut NoExtras,
            &HotkeyAction::DuckHold,
            KeyPhase::Released,
        )
        .unwrap();
        assert!(pb.duck_log().iter().all(|c| c.starts_with("duck")));
    }

    #[test]
    fn release_of_a_press_action_is_ignored() {
        let (mut dir, pb, sound) = setup();
        let mut d = HotkeyDispatcher::new();
        let out = d
            .dispatch(
                &mut dir,
                &pb,
                &mut NoExtras,
                &HotkeyAction::Sound {
                    target: "rain".into(),
                },
                KeyPhase::Released,
            )
            .unwrap();
        assert_eq!(out, DispatchOutcome::Ignored);
        assert!(!pb.is_playing(sound));
    }

    #[test]
    fn playlist_actions_go_to_extras() {
        let (mut dir, pb, _) = setup();
        let mut d = HotkeyDispatcher::new();
        let mut rec = Recorder::default();
        for action in [
            HotkeyAction::Playlist {
                target: "pl".into(),
            },
            HotkeyAction::NextTrack,
            HotkeyAction::PrevTrack,
            HotkeyAction::PauseMusic,
        ] {
            d.dispatch(&mut dir, &pb, &mut rec, &action, KeyPhase::Pressed)
                .unwrap();
        }
        assert_eq!(rec.calls, vec!["playlist pl", "next", "prev", "pause"]);
    }

    #[test]
    fn without_extras_playlist_actions_are_unsupported() {
        let (mut dir, pb, _) = setup();
        let mut d = HotkeyDispatcher::new();
        let err = press(&mut d, &mut dir, &pb, HotkeyAction::NextTrack).unwrap_err();
        assert!(matches!(err, DispatchError::Unsupported("next_track")));
        assert!(err.to_string().contains("not available"));
    }

    #[test]
    fn handle_runs_an_event() {
        let (mut dir, pb, sound) = setup();
        let mut d = HotkeyDispatcher::new();
        let event = HotkeyEvent {
            index: 0,
            action: HotkeyAction::Sound {
                target: "rain".into(),
            },
            phase: KeyPhase::Pressed,
            source: Source::Local,
        };
        d.handle(&mut dir, &pb, &mut NoExtras, &event).unwrap();
        assert!(pb.is_playing(sound));
    }
}
