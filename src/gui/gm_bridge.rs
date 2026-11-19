//! The phone panel (`/control`, `/api/control/*`, `/ws/control`) in console
//! mode.
//!
//! In `--server-only` the `Board` plays the library. With a window there is
//! only one set of sounds, scenes and levels: the console's. So the panel's
//! [`ControlCommand`]s are executed here, on the GUI thread, through the same
//! command path as a click in the window (`AudioCommand` -> Director ->
//! engine), and the console's state is published into the [`ControlFeed`] that
//! `/ws/control` follows. The window and the phone therefore always show the
//! same thing.
//!
//! Nothing here blocks: commands are taken with `try_recv`, and a forwarder
//! thread only wakes the render loop when one arrives (the loop otherwise
//! sleeps up to half a second while idle).

use std::sync::{Arc, Weak};
use std::time::Duration;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};

use crate::audio::director_audio::{DuckMode, DuckTarget, MixControl};
use crate::audio::SoundId;
use crate::config::Bus;
use crate::gui::app::SoundSharedApp;
use crate::gui::types::AudioCommand;
use crate::network::control::{
    BoardState, ControlAction, ControlBuses, ControlCommand, ControlFeed, ControlPlaylist,
    ControlPlaylistState, ControlScene, ControlSound,
};

/// How many panel commands one frame runs at most, so a flood from a phone
/// cannot stall the render loop.
const MAX_COMMANDS_PER_FRAME: usize = 32;

/// How often the forwarder thread checks whether the app is still there.
const FORWARDER_POLL: Duration = Duration::from_millis(500);

/// What the server side hands the console so it can serve the panel: the queue
/// the HTTP handlers write to and the feed `/ws/control` reads from.
#[derive(Clone)]
pub struct GmPanelLink {
    pub commands: Receiver<ControlCommand>,
    pub feed: ControlFeed,
}

/// The console's end of the panel: a queue of commands and the state feed.
pub struct GmBridge {
    queue: Receiver<ControlCommand>,
    feed: ControlFeed,
    /// Dropped with the app: stops the forwarder thread.
    _alive: Arc<()>,
}

impl GmBridge {
    /// Start following `link`. The forwarder thread wakes `ctx` whenever a
    /// command arrives, so an idle console answers the phone at once.
    pub fn attach(ctx: &egui::Context, link: GmPanelLink) -> Self {
        let alive = Arc::new(());
        let (tx, queue) = unbounded();
        spawn_forwarder(ctx.clone(), link.commands, tx, Arc::downgrade(&alive));
        Self {
            queue,
            feed: link.feed,
            _alive: alive,
        }
    }
}

fn spawn_forwarder(
    ctx: egui::Context,
    commands: Receiver<ControlCommand>,
    out: Sender<ControlCommand>,
    alive: Weak<()>,
) {
    let spawned = std::thread::Builder::new()
        .name("gm-panel-wake".into())
        .spawn(move || {
            while alive.upgrade().is_some() {
                match commands.recv_timeout(FORWARDER_POLL) {
                    Ok(command) => {
                        if out.send(command).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    if let Err(e) = spawned {
        tracing::error!("Could not start the phone panel forwarder: {e}");
    }
}

impl SoundSharedApp {
    /// Once per frame: run the panel's pending commands, then publish the
    /// console's state for `/ws/control`. A no-op without a panel.
    pub(crate) fn gm_frame(&mut self) {
        if self.gm.is_none() {
            return;
        }
        for _ in 0..MAX_COMMANDS_PER_FRAME {
            let Some(command) = self.gm.as_ref().and_then(|gm| gm.queue.try_recv().ok()) else {
                break;
            };
            let result = self.gm_run(command.action);
            // The handler may have timed out and gone away; that is fine.
            let _ = command.reply.send(result);
        }
        let state = self.gm_state();
        if let Some(gm) = &self.gm {
            gm.feed.publish(state);
        }
    }

    /// The console's state as the phone panel shows it. Sounds are numbered by
    /// their position in the console's list (the order the window shows them),
    /// scenes by their position in the library.
    pub(crate) fn gm_state(&self) -> BoardState {
        let sounds = self.get_sounds();
        let control_sounds = sounds
            .iter()
            .enumerate()
            .map(|(id, entry)| {
                let known = self.library.sound(&entry.path);
                ControlSound {
                    id,
                    name: entry.name.clone(),
                    group: entry
                        .group
                        .and_then(|group| self.library.group_name(group))
                        .map(str::to_string),
                    playing: entry.playing,
                    looped: entry.looped,
                    volume: entry.volume,
                    stinger: known.is_some_and(|sound| sound.oneshot_stinger),
                }
            })
            .collect();

        let active = self.director.active_scene();
        let scenes: Vec<ControlScene> = self
            .library
            .scenes()
            .iter()
            .enumerate()
            .map(|(id, scene)| {
                let uuid = scene.id.to_string();
                ControlScene {
                    id,
                    active: active == Some(uuid.as_str()),
                    uuid,
                    name: scene.name.clone(),
                    color: scene.color.clone(),
                    sounds: scene.items.len(),
                }
            })
            .collect();
        let active_scene = scenes.iter().position(|scene| scene.active);

        let playlists = self
            .library
            .playlists()
            .iter()
            .map(|list| ControlPlaylist {
                id: list.id.to_string(),
                name: list.name.clone(),
                tracks: list.items.len(),
            })
            .collect();

        let ducked_buses: Vec<String> = [Bus::Music, Bus::Ambience, Bus::Sfx]
            .into_iter()
            .filter(|bus| self.director.is_ducked(*bus))
            .map(|bus| crate::audio::director_audio::bus_name(bus).to_string())
            .collect();

        BoardState {
            sounds: control_sounds,
            master_volume: self.get_master_volume(),
            scenes,
            active_scene,
            playlists,
            ducked: !ducked_buses.is_empty(),
            ducked_buses,
            buses: ControlBuses {
                music: self.director.bus_volume(Bus::Music),
                ambience: self.director.bus_volume(Bus::Ambience),
                sfx: self.director.bus_volume(Bus::Sfx),
            },
            playlist: Some(self.gm_playlist_view()),
        }
    }

    /// The music layer for the panel (position rounded down to a second, so a
    /// playing track publishes once a second).
    fn gm_playlist_view(&self) -> ControlPlaylistState {
        let mut state = self.director.playlist_state();
        state.position_ms -= state.position_ms % 1000;
        if state.playlist.is_none() {
            // The player's idle defaults settle after its first tick; do not
            // let that read as a change of state.
            state.crossfade_ms = 0;
        }
        let label = state.track.as_ref().map(|track| {
            let fallback = uuid::Uuid::parse_str(track)
                .ok()
                .and_then(|id| self.library.sound_by_id(id))
                .map(crate::gui::music::sound_label)
                .unwrap_or_else(|| track.clone());
            state.label(&fallback)
        });
        ControlPlaylistState { state, label }
    }

    /// Run one panel action and report the state afterwards.
    fn gm_run(&mut self, action: ControlAction) -> Result<BoardState, String> {
        match action {
            ControlAction::List => {}
            ControlAction::Play(index) => {
                let id = self.gm_sound(index)?;
                self.gm_audio(|response| AudioCommand::PlaySound { id, response })?;
            }
            ControlAction::Stop(index) => {
                let id = self.gm_sound(index)?;
                self.gm_audio(|response| AudioCommand::StopSound { id, response })?;
            }
            ControlAction::StopAll => {
                // The panic control also silences the library preview.
                crate::gui::library_ui::stop_preview(self);
                self.gm_audio(|response| AudioCommand::StopAll { response })?;
            }
            ControlAction::StopAllFade(fade_ms) => {
                crate::gui::library_ui::stop_preview(self);
                self.gm_audio(|response| AudioCommand::StopAllFade { fade_ms, response })?;
            }
            ControlAction::SetVolume(index, volume) => {
                let id = self.gm_sound(index)?;
                let path = self.gm_path(id);
                let volume = volume.clamp(0.0, 1.0);
                self.gm_audio(|response| AudioCommand::SetVolume {
                    id,
                    volume,
                    response,
                })?;
                // Persisted like a slider drag in the window.
                if let Some(path) = path {
                    if self.library.set_volume(&path, volume) {
                        self.library_dirty = true;
                    }
                }
            }
            ControlAction::SetLoop(index, looped) => {
                let id = self.gm_sound(index)?;
                let path = self.gm_path(id);
                self.gm_audio(|response| AudioCommand::SetLoop {
                    id,
                    looped,
                    response,
                })?;
                if let Some(path) = path {
                    if self.library.set_looped(&path, looped) {
                        self.library_dirty = true;
                    }
                }
            }
            ControlAction::SetMaster(volume) => {
                let volume = volume.clamp(0.0, 1.0);
                self.gm_audio(|response| AudioCommand::SetMasterVolume { volume, response })?;
                // An explicit level replaces a remembered "muted" level.
                self.muted = None;
            }
            ControlAction::ActivateScene(index) => {
                let id = self
                    .library
                    .scenes()
                    .get(index)
                    .map(|scene| scene.id.to_string())
                    .ok_or_else(|| format!("no scene #{index}"))?;
                self.gm_audio(|response| AudioCommand::ActivateScene { id, response })?;
            }
            ControlAction::StopScene => {
                self.gm_audio(|response| AudioCommand::StopScene { response })?;
            }
            ControlAction::PlayStinger(index) => {
                let id = self.gm_sound(index)?;
                self.gm_audio(|response| AudioCommand::PlayStinger { id, response })?;
            }
            ControlAction::Duck(on) => {
                let mode = if on { DuckMode::On } else { DuckMode::Off };
                self.gm_mix(MixControl::Duck {
                    target: DuckTarget::Configured,
                    mode,
                    amount: None,
                    attack_ms: None,
                    release_ms: None,
                })?;
            }
            ControlAction::Mix(control) => self.gm_mix(control)?,
            ControlAction::Playlist(command) => self.gm_playlist(command)?,
        }
        Ok(self.gm_state())
    }

    /// The engine id of the sound at position `index` of the console's list.
    fn gm_sound(&self, index: usize) -> Result<SoundId, String> {
        self.get_sounds()
            .get(index)
            .map(|entry| entry.id)
            .ok_or_else(|| format!("no sound #{index}"))
    }

    /// The library path of an engine sound.
    fn gm_path(&self, id: SoundId) -> Option<std::path::PathBuf> {
        self.sounds
            .lock()
            .unwrap()
            .get(&id)
            .map(|entry| entry.path.clone())
    }

    /// Run `command` through the console's own audio command path, exactly as a
    /// click in the window would, and wait for its (immediate) answer.
    fn gm_audio(
        &mut self,
        make: impl FnOnce(Sender<anyhow::Result<()>>) -> AudioCommand,
    ) -> Result<(), String> {
        // Clear whatever the window queued first, so the channel has room and
        // the order of events is the order they happened in.
        self.process_audio_commands();
        let (response_tx, response_rx) = bounded(1);
        self.app_state
            .audio_cmd_tx
            .try_send(make(response_tx))
            .map_err(|_| "the console is busy, try again".to_string())?;
        self.process_audio_commands();
        match response_rx.try_recv() {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(_) => Err("the audio engine did not answer".to_string()),
        }
    }

    /// Music-layer commands go straight to the Director, as the console's own
    /// music controls do.
    fn gm_playlist(&mut self, command: crate::audio::PlaylistCommand) -> Result<(), String> {
        let guard = self.audio_engine.lock().unwrap();
        let engine = guard.as_ref().ok_or("audio engine stopped")?;
        crate::gui::app::sync_director(&mut self.director, &self.library, &self.sounds);
        let result = self.director.playlist_command(engine, command);
        crate::gui::app::mirror_playing(&self.sounds, engine);
        result.map_err(|e| e.to_string())
    }

    /// Bus volumes and ducking go straight to the Director, as the console's
    /// own mixer controls do.
    fn gm_mix(&mut self, control: MixControl) -> Result<(), String> {
        let guard = self.audio_engine.lock().unwrap();
        let engine = guard.as_ref().ok_or("audio engine stopped")?;
        self.director
            .run_mix_control(engine, control)
            .map_err(|e| e.to_string())
    }
}
