//! Timer actions on the [`Director`]: what a fired
//! [`TimerEvent`](crate::audio::timeline::TimerEvent) actually does.
//!
//! Every [`TimerAction`] is mapped onto an existing Director call, so a timer
//! behaves exactly like the operator pressing the same button. A timer that
//! points at something that is gone (a deleted scene, an unloaded sound)
//! must never panic or stop the others: the problem comes back as a warning in
//! [`TimerRun`] and the caller logs it.

use crate::audio::director::{Director, Playback};
use crate::audio::playlist::PlaylistCommand;
use crate::config::TimerAction;

/// The result of running one timer action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimerRun {
    /// What did not work. Empty when the action ran cleanly.
    pub warnings: Vec<String>,
}

impl TimerRun {
    /// Whether everything worked.
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }

    fn warn(message: impl Into<String>) -> Self {
        Self {
            warnings: vec![message.into()],
        }
    }
}

/// Ids are UUID strings; ignore case and surrounding spaces when comparing.
fn same_id(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

impl Director {
    /// Run a timer action. Never panics; failures are warnings in the result.
    pub fn run_timer_action<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        action: &TimerAction,
    ) -> TimerRun {
        match action {
            TimerAction::ActivateScene { scene } => {
                let Some(id) = self
                    .scenes()
                    .iter()
                    .find(|candidate| same_id(&candidate.id, scene))
                    .map(|found| found.id.clone())
                else {
                    return TimerRun::warn(format!("scene {scene} no longer exists"));
                };
                match self.activate_scene(pb, &id) {
                    Ok(report) => {
                        let mut run = TimerRun::default();
                        if !report.missing.is_empty() {
                            run.warnings.push(format!(
                                "scene {}: {} sound(s) not loaded",
                                report.scene,
                                report.missing.len()
                            ));
                        }
                        run.warnings.extend(report.errors);
                        run
                    }
                    Err(error) => TimerRun::warn(format!("scene {scene}: {error}")),
                }
            }
            TimerAction::StopScene => self.finish("stop scene", |d| d.stop_scene(pb).map(|_| ())),
            TimerAction::PlayStinger { sound } => {
                let Some(engine_id) = self
                    .catalog()
                    .iter()
                    .find(|entry| same_id(&entry.id, sound))
                    .map(|entry| entry.sound)
                else {
                    return TimerRun::warn(format!("sound {sound} is not loaded"));
                };
                self.finish("stinger", |d| d.play_stinger(pb, engine_id))
            }
            TimerAction::StopAll { fade_ms } => {
                let fade = *fade_ms;
                self.finish("stop all", |d| d.stop_all_with_fade(pb, fade))
            }
            TimerAction::StartPlaylist { playlist } => {
                self.music_command(pb, PlaylistCommand::Start(playlist.clone()), "playlist")
            }
            TimerAction::StopPlaylist => self.music_command(pb, PlaylistCommand::Stop, "music"),
            TimerAction::NextTrack => self.music_command(pb, PlaylistCommand::Next, "next track"),
            TimerAction::PauseMusic => self.music_command(pb, PlaylistCommand::Pause, "pause"),
            TimerAction::ResumeMusic => self.music_command(pb, PlaylistCommand::Resume, "resume"),
        }
    }

    fn music_command<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        command: PlaylistCommand,
        what: &str,
    ) -> TimerRun {
        self.finish(what, |d| d.playlist_command(pb, command))
    }

    fn finish(
        &mut self,
        what: &str,
        call: impl FnOnce(&mut Self) -> Result<(), crate::audio::director::DirectorError>,
    ) -> TimerRun {
        match call(self) {
            Ok(()) => TimerRun::default(),
            Err(error) => TimerRun::warn(format!("{what}: {error}")),
        }
    }
}
