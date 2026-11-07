//! GUI application state and logic

use crate::audio::{AudioDecoder, AudioEngine, DecodedAudio, LocalOutputStatus, SoundId};
use crate::gui::groups::{GroupFilter, GroupId, Library};
use crate::gui::types::{
    AppState, AudioCommand, ServerCommand, ServerStatus as AppServerStatus, SoundInfo,
};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use egui::Context;
use rfd::FileDialog;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// Sound entry in the library (for GUI display)
#[derive(Debug, Clone)]
pub struct SoundEntry {
    pub id: SoundId,
    pub name: String,
    pub path: PathBuf,
    pub playing: bool,
    pub volume: f32,
    /// Whether playback restarts from the beginning when the sound ends.
    pub looped: bool,
    /// Group membership mirrored from [`Library`], or `None` when ungrouped.
    pub group: Option<GroupId>,
    /// Duration in seconds; `0.0` until the decoder reports it.
    pub duration: f32,
    /// Source sample rate in Hz; `0` means "same as the mixer".
    pub sample_rate: u32,
}

/// Which modal the console is currently showing.
///
/// Dialogs live in the app state rather than in the widgets so a frame that
/// paints a panel can still open one, and so the modal survives repaints.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    /// Nothing open.
    None,
    /// Create a group. Carries the in-progress name field and its error.
    NewGroup { name: String, error: Option<String> },
    /// Rename an existing group.
    RenameGroup {
        id: GroupId,
        name: String,
        error: Option<String>,
    },
    /// Confirm deletion of a group; its sounds become ungrouped.
    DeleteGroup { id: GroupId },
    /// Assign a sound to a group (or clear its membership).
    AssignSound { sound_id: SoundId, name: String },
    /// Add a sound from a YouTube link. Holds the in-progress link field and
    /// its validation error; the download itself runs on a worker thread and
    /// reports into [`SoundSharedApp::youtube_result_rx`].
    AddYouTube { url: String, error: Option<String> },
}

/// How long the library may stay unsaved before it is flushed to disk.
const LIBRARY_FLUSH: Duration = Duration::from_millis(400);

/// How long the `SAVED` confirmation stays under the CONFIGURATION fields.
const CONFIG_SAVED_HOLD: Duration = Duration::from_millis(1500);

/// Longest text the PORT well may hold before it can never be a valid port.
pub const PORT_INPUT_MAX_CHARS: usize = 5;

/// Longest text the BIND ADDRESS well may hold. Fits a full IPv6 literal
/// (`ffff:ffff:ffff:ffff:ffff:ffff:255.255.255.255`, 45 characters).
pub const BIND_INPUT_MAX_CHARS: usize = 45;

/// Why the PORT well was rejected, phrased for the inline error line.
pub const PORT_ERROR: &str = "Port must be 1–65535";

/// Why the BIND ADDRESS well was rejected, phrased for the inline error line.
pub const BIND_ERROR: &str = "Bind must be an IP address";

/// Why a group name was rejected, phrased for the modal's error line.
fn group_name_error(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Enter a group name".to_string()
    } else if trimmed.chars().count() > crate::gui::groups::MAX_GROUP_NAME_LEN {
        format!(
            "Maximum {} characters",
            crate::gui::groups::MAX_GROUP_NAME_LEN
        )
    } else {
        "A group with this name already exists".to_string()
    }
}

/// Internal server status for GUI
#[derive(Debug, Clone, PartialEq)]
pub enum ServerStatus {
    Stopped,
    Starting,
    Running,
    Error(String),
}

/// Server state snapshot for GUI
#[derive(Debug, Clone)]
pub struct ServerState {
    pub status: ServerStatus,
    pub bind_url: Option<String>,
    /// Loopback URL of the same server, for this machine.
    ///
    /// The advertised URL carries the LAN address so it can be sent to the
    /// players; the operator checking the page locally does not have to go
    /// through the network for it.
    pub local_url: Option<String>,
    /// Router port forwarding: the internet URL once the router grants it.
    pub upnp: crate::network::upnp::UpnpStatus,
    /// Warnings about what keeps players out (VPN, firewall).
    pub notes: Vec<String>,
    pub listener_count: usize,
    /// Most listeners seen at once since the server started.
    pub peak_listeners: usize,
}

/// Main GUI application state
pub struct SoundSharedApp {
    // Reference to shared app state
    app_state: AppState,

    // Audio engine (must stay on main thread due to cpal::Stream not being Send)
    audio_engine: Arc<Mutex<Option<AudioEngine>>>,

    // Audio command receiver (for processing commands on main thread)
    audio_cmd_rx: Receiver<AudioCommand>,

    // Local sound cache (synced from audio engine)
    sounds: Arc<Mutex<HashMap<SoundId, SoundEntry>>>,
    sound_order: Arc<Mutex<Vec<SoundId>>>,

    // Master volume cache
    master_volume: Arc<Mutex<f32>>,

    // Server state cache
    server_state: Arc<Mutex<ServerState>>,

    // UI feedback
    copied_feedback: Arc<Mutex<bool>>,
    copied_feedback_timer: Arc<Mutex<Option<Instant>>>,

    /// Library files still waiting for room in the audio command channel.
    ///
    /// That channel is short and only the GUI thread drains it, so a board with
    /// more files than the channel holds has to be fed frame by frame. Dropping
    /// the excess (the old behaviour) silently lost part of the saved library on
    /// every start.
    pending_loads: VecDeque<PathBuf>,

    // Channel for receiving server status updates
    server_status_rx: Receiver<AppServerStatus>,
    server_status_tx: Sender<AppServerStatus>,

    // Sound library + groups (the operator's saved collection)
    library: Library,
    /// Which group the library panel is filtered to.
    group_filter: GroupFilter,
    /// Name shown in the group-name field of the currently open modal.
    dialog: Dialog,
    /// Set when the library changed and needs writing to config.toml.
    library_dirty: bool,
    /// Last time the library was flushed to disk.
    last_persist: Instant,
    /// Master volume stashed while muted.
    muted: Option<f32>,

    /// Last known state of the desktop output (`Running`, `Headless`, `Failed`).
    ///
    /// Refreshed once per frame from the engine so the status strip can explain a
    /// silent desktop: the web listeners keep playing in every one of these
    /// states, which is exactly the asymmetry the strip has to make visible.
    local_output: LocalOutputStatus,

    /// Whether the live-dot pulse and the backdrop scanline are switched off.
    ///
    /// Resolved once at startup from `[gui] reduce_motion` and the
    /// `SOUNDSHARED_REDUCED_MOTION` override — see [`crate::gui::motion`].
    reduced_motion: bool,

    /// Text in the CONFIGURATION `PORT` well. It may hold an invalid value while
    /// the operator is typing; only valid text is ever committed to the config.
    port_input: String,
    /// Text in the CONFIGURATION `BIND ADDRESS` well.
    bind_input: String,
    /// Inline error under the CONFIGURATION wells, e.g. `PORT MUST BE 1–65535`.
    config_error: Option<String>,
    /// When the last valid server-configuration edit was accepted, for the
    /// brief `SAVED` label.
    config_saved_at: Option<Instant>,
    /// Replies from the server thread for pushed `ServerCommand::SetBind`s.
    bind_result_rx: Receiver<Result<(), String>>,
    bind_result_tx: Sender<Result<(), String>>,
    /// Files waiting for the decoder worker.
    decode_tx: Sender<DecodeJob>,
    /// Decoded files coming back from the worker, finished on the GUI thread.
    decoded_rx: Receiver<DecodedJob>,
    /// Results from the (at most one) YouTube download worker. The sender half
    /// lives in the worker thread; a new download simply replaces the receiver.
    youtube_result_rx: Receiver<YouTubeJob>,
    /// Progress text the YouTube worker updates for the modal to read.
    youtube_status: Arc<Mutex<String>>,
    /// True while a YouTube download is in flight (the modal disables itself).
    youtube_busy: bool,
    /// MASTER OUTPUT meter position (0..1, dB scale with peak decay).
    output_meter: f32,
}

/// One file for the background decoder: the path and where to report.
type DecodeJob = (String, Sender<anyhow::Result<SoundInfo>>);
/// A finished decode: the job plus the decoded PCM (or the error text).
type DecodedJob = (
    String,
    Sender<anyhow::Result<SoundInfo>>,
    Result<DecodedAudio, String>,
);

/// One finished YouTube download: the file ready for the decoder, or the reason.
type YouTubeJob = Result<PathBuf, String>;

/// Decode files one by one off the GUI thread.
///
/// Decoding (and resampling) a long track takes a noticeable time; it used to
/// run inside the frame update, freezing the whole console while a library
/// loaded. The worker decodes, hands the PCM back and wakes the UI.
fn spawn_decoder(ctx: egui::Context) -> (Sender<DecodeJob>, Receiver<DecodedJob>) {
    let (job_tx, job_rx) = crossbeam_channel::unbounded::<DecodeJob>();
    let (done_tx, done_rx) = crossbeam_channel::unbounded::<DecodedJob>();
    std::thread::Builder::new()
        .name("decoder".into())
        .spawn(move || {
            for (path, response) in job_rx {
                let result = AudioDecoder::decode_file(&path).map_err(|e| e.to_string());
                if done_tx.send((path, response, result)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        })
        .expect("spawn decoder thread");
    (job_tx, done_rx)
}

impl SoundSharedApp {
    /// Create a new GUI app state with shared AppState and audio engine
    pub fn new(
        app_state: AppState,
        audio_engine: Arc<Mutex<Option<AudioEngine>>>,
        audio_cmd_rx: Receiver<AudioCommand>,
        cc: &eframe::CreationContext<'_>,
    ) -> Self {
        let (decode_tx, decoded_rx) = spawn_decoder(cc.egui_ctx.clone());
        // Create channels for receiving updates from background threads
        let (server_status_tx, server_status_rx) = bounded(1);
        let (bind_result_tx, bind_result_rx) = bounded(4);

        // Restore the saved library (groups + sounds) before the first frame,
        // so the console opens on the operator's actual collection. The two
        // CONFIGURATION wells start on the values currently in the config.
        let (initial_master_vol, library, initial_host, initial_port, reduced_motion) = {
            let config = app_state.config.lock().unwrap();
            (
                config.audio.master_volume,
                Library::from_config(&config.library),
                config.server.host.clone(),
                config.server.port,
                crate::gui::motion::reduced_motion(config.gui.reduce_motion),
            )
        };

        let mut app = Self {
            app_state,
            audio_engine,
            audio_cmd_rx,
            sounds: Arc::new(Mutex::new(HashMap::new())),
            sound_order: Arc::new(Mutex::new(Vec::new())),
            master_volume: Arc::new(Mutex::new(initial_master_vol)),
            server_state: Arc::new(Mutex::new(ServerState {
                status: ServerStatus::Stopped,
                bind_url: None,
                local_url: None,
                upnp: Default::default(),
                notes: Vec::new(),
                listener_count: 0,
                peak_listeners: 0,
            })),
            copied_feedback: Arc::new(Mutex::new(false)),
            copied_feedback_timer: Arc::new(Mutex::new(None)),
            pending_loads: VecDeque::new(),
            server_status_rx,
            server_status_tx,
            library,
            group_filter: GroupFilter::All,
            dialog: Dialog::None,
            library_dirty: false,
            last_persist: Instant::now(),
            muted: None,
            local_output: LocalOutputStatus::stopped(),
            reduced_motion,
            port_input: initial_port.to_string(),
            bind_input: initial_host,
            config_error: None,
            config_saved_at: None,
            bind_result_rx,
            bind_result_tx,
            decode_tx,
            decoded_rx,
            youtube_result_rx: crossbeam_channel::unbounded::<YouTubeJob>().1,
            youtube_status: Arc::new(Mutex::new(String::new())),
            youtube_busy: false,
            output_meter: 0.0,
        };

        // Request initial server status
        app.request_server_status();

        // Put the saved board back on the desk before the first frame.
        app.restore_library_sounds();

        app
    }

    // ===== Public API =====

    /// Queue a sound file to be handed to the audio engine.
    ///
    /// The command is dropped in immediately when the channel has room; a full
    /// channel (the startup restore of a large library) parks the path in
    /// [`Self::pending_loads`] and [`Self::pump_pending_loads`] feeds it as soon
    /// as the main thread has drained a slot.
    pub fn request_load_sound(&mut self, path: PathBuf) {
        let (response_tx, response_rx) = bounded(1);
        let cmd = AudioCommand::LoadSound {
            path: path.to_string_lossy().to_string(),
            response: response_tx,
        };

        if let Err(e) = self.app_state.audio_cmd_tx.try_send(cmd) {
            debug!("Load channel full, queueing {}: {}", path.display(), e);
            self.pending_loads.push_back(path);
            return;
        }

        self.await_load_response(response_rx);
    }

    /// Report the outcome of one `LoadSound` command without blocking the GUI.
    ///
    /// The sound entry itself is inserted by [`Self::process_audio_commands`] on
    /// the main thread; this thread only logs the result.
    fn await_load_response(&self, response_rx: Receiver<anyhow::Result<SoundInfo>>) {
        std::thread::spawn(move || match response_rx.recv() {
            Ok(Ok(sound_info)) => {
                info!("Loaded sound: {} ({})", sound_info.name, sound_info.id);
            }
            Ok(Err(e)) => error!("Failed to load sound: {}", e),
            Err(_) => error!("Audio command channel closed"),
        });
    }

    /// Feed queued library files to the engine while the channel has room.
    fn pump_pending_loads(&mut self) {
        while let Some(path) = self.pending_loads.pop_front() {
            let (response_tx, response_rx) = bounded(1);
            let cmd = AudioCommand::LoadSound {
                path: path.to_string_lossy().to_string(),
                response: response_tx,
            };
            if let Err(e) = self.app_state.audio_cmd_tx.try_send(cmd) {
                // Still full: put it back and try again on a later frame.
                debug!(
                    "Load channel still full, {} stays queued: {}",
                    path.display(),
                    e
                );
                self.pending_loads.push_front(path);
                break;
            }
            self.await_load_response(response_rx);
        }
    }

    /// Re-open every sound saved in the library that is still on disk.
    ///
    /// The audio engine always starts empty, so without this the console would
    /// come back with a blank board while the group rail still reported the
    /// saved counts. Missing files are skipped rather than reported as errors,
    /// because a library entry may legitimately point at an unmounted volume.
    fn restore_library_sounds(&mut self) {
        let paths: Vec<PathBuf> = self
            .library
            .sounds()
            .iter()
            .map(|sound| sound.path.clone())
            .collect();

        for path in paths {
            if path.exists() {
                self.request_load_sound(path);
            } else {
                debug!(
                    "Library sound missing on disk, skipping: {}",
                    path.display()
                );
            }
        }
    }

    /// Request playing a sound
    pub fn request_play_sound(&self, id: SoundId) {
        let sounds = self.sounds.clone();
        let (response_tx, response_rx) = bounded(1);
        let cmd = AudioCommand::PlaySound {
            id,
            response: response_tx,
        };

        if let Err(e) = self.app_state.audio_cmd_tx.try_send(cmd) {
            error!("Failed to send play sound command (channel full): {}", e);
            return;
        }

        std::thread::spawn(move || {
            if response_rx
                .recv()
                .unwrap_or(Err(anyhow::anyhow!("Failed")))
                .is_ok()
            {
                if let Some(entry) = sounds.lock().unwrap().get_mut(&id) {
                    entry.playing = true;
                }
            }
        });
    }

    /// Request stopping a sound
    pub fn request_stop_sound(&self, id: SoundId) {
        let sounds = self.sounds.clone();
        let (response_tx, response_rx) = bounded(1);
        let cmd = AudioCommand::StopSound {
            id,
            response: response_tx,
        };

        if let Err(e) = self.app_state.audio_cmd_tx.try_send(cmd) {
            error!("Failed to send stop sound command (channel full): {}", e);
            return;
        }

        std::thread::spawn(move || {
            if response_rx
                .recv()
                .unwrap_or(Err(anyhow::anyhow!("Failed")))
                .is_ok()
            {
                if let Some(entry) = sounds.lock().unwrap().get_mut(&id) {
                    entry.playing = false;
                }
            }
        });
    }

    /// Request setting sound volume
    pub fn request_set_volume(&self, id: SoundId, volume: f32) {
        let sounds = self.sounds.clone();
        let volume = volume.clamp(0.0, 1.0);
        let (response_tx, response_rx) = bounded(1);
        let cmd = AudioCommand::SetVolume {
            id,
            volume,
            response: response_tx,
        };

        if let Err(e) = self.app_state.audio_cmd_tx.try_send(cmd) {
            error!("Failed to send set volume command (channel full): {}", e);
            return;
        }

        std::thread::spawn(move || {
            if response_rx
                .recv()
                .unwrap_or(Err(anyhow::anyhow!("Failed")))
                .is_ok()
            {
                if let Some(entry) = sounds.lock().unwrap().get_mut(&id) {
                    entry.volume = volume;
                }
            }
        });
    }

    /// Set master volume
    pub fn set_master_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        *self.master_volume.lock().unwrap() = volume;

        let (response_tx, _response_rx) = bounded(1);
        let cmd = AudioCommand::SetMasterVolume {
            volume,
            response: response_tx,
        };

        if let Err(e) = self.app_state.audio_cmd_tx.try_send(cmd) {
            error!(
                "Failed to send set master volume command (channel full): {}",
                e
            );
        }
    }

    /// Get master volume
    pub fn get_master_volume(&self) -> f32 {
        *self.master_volume.lock().unwrap()
    }

    /// Request removing a sound
    pub fn request_remove_sound(&self, id: SoundId) {
        let sounds = self.sounds.clone();
        let sound_order = self.sound_order.clone();
        let (response_tx, response_rx) = bounded(1);
        let cmd = AudioCommand::RemoveSound {
            id,
            response: response_tx,
        };

        if let Err(e) = self.app_state.audio_cmd_tx.try_send(cmd) {
            error!("Failed to send remove sound command (channel full): {}", e);
            return;
        }

        std::thread::spawn(move || {
            if response_rx
                .recv()
                .unwrap_or(Err(anyhow::anyhow!("Failed")))
                .is_ok()
            {
                sounds.lock().unwrap().remove(&id);
                sound_order.lock().unwrap().retain(|&x| x != id);
            }
        });
    }

    /// Every loaded sound, regardless of the active group filter.
    pub fn sounds(&self) -> Vec<SoundEntry> {
        self.get_sounds()
    }

    /// Get all sounds for display
    pub fn get_sounds(&self) -> Vec<SoundEntry> {
        let sounds = self.sounds.lock().unwrap();
        let order = self.sound_order.lock().unwrap();
        order
            .iter()
            .filter_map(|id| sounds.get(id).cloned())
            .collect()
    }

    /// Request starting the server
    pub fn request_start_server(&self) {
        // Double-click protection: don't start if already starting or running
        {
            let state = self.server_state.lock().unwrap();
            if matches!(state.status, ServerStatus::Starting | ServerStatus::Running) {
                return;
            }
        }

        // Update local state to starting
        {
            let mut state = self.server_state.lock().unwrap();
            state.status = ServerStatus::Starting;
            state.bind_url = None;
            state.listener_count = 0;
        }

        let (response_tx, response_rx) = bounded(1);
        let cmd = ServerCommand::Start {
            response: response_tx,
        };

        // Non-blocking send to avoid freezing GUI if channel is full
        if let Err(e) = self.app_state.server_cmd_tx.try_send(cmd) {
            error!("Failed to send start server command (channel full): {}", e);
            let mut state = self.server_state.lock().unwrap();
            state.status = ServerStatus::Error("Server command channel full".to_string());
            return;
        }

        let server_state = self.server_state.clone();
        let server_status_tx = self.server_status_tx.clone();

        std::thread::spawn(move || {
            // Wait for response with timeout to avoid hanging forever
            match response_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(url)) => {
                    let mut state = server_state.lock().unwrap();
                    state.status = ServerStatus::Running;
                    state.bind_url = Some(url.clone());
                    info!("Server started successfully at {}", url);
                    let _ = server_status_tx.try_send(AppServerStatus {
                        running: true,
                        address: Some(url),
                        listener_count: state.listener_count,
                    });
                }
                Ok(Err(e)) => {
                    error!("Failed to start server: {}", e);
                    let mut state = server_state.lock().unwrap();
                    state.status = ServerStatus::Error(e.to_string());
                    let _ = server_status_tx.try_send(AppServerStatus {
                        running: false,
                        address: None,
                        listener_count: 0,
                    });
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    error!("Server start timed out after 10 seconds");
                    let mut state = server_state.lock().unwrap();
                    state.status = ServerStatus::Error("Server start timed out".to_string());
                    let _ = server_status_tx.try_send(AppServerStatus {
                        running: false,
                        address: None,
                        listener_count: 0,
                    });
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    error!("Server command channel closed");
                    let mut state = server_state.lock().unwrap();
                    state.status = ServerStatus::Error("Channel closed".to_string());
                }
            }
        });
    }

    /// Request stopping the server
    pub fn request_stop_server(&self) {
        let server_state = self.server_state.clone();
        let server_status_tx = self.server_status_tx.clone();

        let (response_tx, response_rx) = bounded(1);
        let cmd = ServerCommand::Stop {
            response: response_tx,
        };

        // Non-blocking send to avoid freezing GUI if channel is full
        if let Err(e) = self.app_state.server_cmd_tx.try_send(cmd) {
            error!("Failed to send stop server command (channel full): {}", e);
            return;
        }

        std::thread::spawn(
            move || match response_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(())) => {
                    let mut state = server_state.lock().unwrap();
                    state.status = ServerStatus::Stopped;
                    state.bind_url = None;
                    state.listener_count = 0;
                    let _ = server_status_tx.try_send(AppServerStatus {
                        running: false,
                        address: None,
                        listener_count: 0,
                    });
                }
                Ok(Err(e)) => {
                    error!("Failed to stop server: {}", e);
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    error!("Server stop timed out after 10 seconds");
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    error!("Server command channel closed");
                }
            },
        );
    }

    /// Get server state
    pub fn get_server_state(&self) -> ServerState {
        // Check for updates from channel
        self.poll_server_status();
        let mut state = self.server_state.lock().unwrap();
        // Read real-time listener count from the shared counter (updated by server thread)
        state.listener_count = *self.app_state.listener_count.lock().unwrap();
        // PEAK used to print the current count; keep the real maximum.
        state.peak_listeners = if state.status == ServerStatus::Running {
            state.peak_listeners.max(state.listener_count)
        } else {
            0
        };
        // Derived, never stored separately: it follows the advertised URL and
        // disappears with it when the server stops.
        state.local_url = state
            .bind_url
            .as_deref()
            .and_then(crate::network::server::loopback_url);
        state.upnp = self.app_state.upnp_status.lock().unwrap().clone();
        state.notes = self.app_state.network_notes.lock().unwrap().clone();
        state.clone()
    }

    /// Poll server status from channel
    fn poll_server_status(&self) {
        match self.server_status_rx.try_recv() {
            Ok(status) => {
                let mut state = self.server_state.lock().unwrap();
                state.listener_count = status.listener_count;
                if status.running && state.status != ServerStatus::Running {
                    state.status = ServerStatus::Running;
                    state.bind_url = status.address;
                } else if !status.running && state.status == ServerStatus::Running {
                    state.status = ServerStatus::Stopped;
                    state.bind_url = None;
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                debug!("Server status channel disconnected");
            }
        }
    }

    /// Request server status
    pub fn request_server_status(&self) {
        let (response_tx, response_rx) = bounded(1);
        let cmd = ServerCommand::GetStatus {
            response: response_tx,
        };

        // Non-blocking send to avoid freezing GUI if channel is full
        if let Err(e) = self.app_state.server_cmd_tx.try_send(cmd) {
            error!("Failed to send get status command (channel full): {}", e);
            return;
        }

        let server_status_tx = self.server_status_tx.clone();
        std::thread::spawn(move || {
            if let Ok(status) = response_rx.recv_timeout(Duration::from_secs(5)) {
                let _ = server_status_tx.try_send(status);
            }
        });
    }

    /// Get server port from config — the last *valid* value in force.
    pub fn get_server_port(&self) -> u16 {
        self.app_state.config.lock().unwrap().server.port
    }

    /// Get server bind address from config — the last *valid* value in force.
    pub fn get_server_bind_addr(&self) -> String {
        self.app_state.config.lock().unwrap().server.host.clone()
    }

    // ===== LOCAL OUTPUT STATUS =====

    /// Latest desktop-output state, for the status strip.
    ///
    /// `Running` means the speakers are being fed; `Headless` and `Failed` mean
    /// the network keeps streaming while the desktop stays silent.
    pub fn local_output(&self) -> &LocalOutputStatus {
        &self.local_output
    }

    /// Read the engine's current local-output state. Cheap: one `RwLock` read.
    fn poll_local_output(&mut self) {
        if let Some(engine) = self.audio_engine.lock().unwrap().as_ref() {
            self.local_output = engine.local_output_status();

            // -48 dBFS .. 0 dBFS onto the 20-cell strip, with a falling peak.
            let peak = engine.take_output_peak();
            let level = if peak > 0.0 {
                ((20.0 * peak.log10() + 48.0) / 48.0).clamp(0.0, 1.0)
            } else {
                0.0
            };
            self.output_meter = level.max(self.output_meter * 0.82);
            if self.output_meter < 0.01 {
                self.output_meter = 0.0;
            }
        }
    }

    /// `url` as players need it: with the access key appended.
    pub fn share_url(&self, url: &str) -> String {
        self.app_state.access.share_url(url)
    }

    /// The stream's access key, if one is required.
    pub fn access_key(&self) -> Option<String> {
        self.app_state.access.current()
    }

    /// Issue a new access key: the old link stops working at once and its
    /// listeners are disconnected. Saved with the next config flush.
    pub fn regenerate_access_key(&mut self) {
        let key = crate::network::generate_key();
        self.app_state.access.set(Some(key.clone()));
        self.app_state.config.lock().unwrap().server.access_token = Some(key);
        self.library_dirty = true;
        info!("Issued a new listener access key");
    }

    /// Live output level for the MASTER OUTPUT strip (0..1).
    pub fn output_meter(&self) -> f32 {
        self.output_meter
    }

    // ===== MOTION =====

    /// True when the operator switched off decorative motion.
    ///
    /// Panels gate their pulses on this: the state a dot reports is always
    /// shown, only the opacity breath is dropped.
    pub fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    // ===== CONFIGURATION fields (PORT / BIND ADDRESS) =====

    /// Text currently in the CONFIGURATION port well.
    pub fn port_input_mut(&mut self) -> &mut String {
        &mut self.port_input
    }

    /// Text currently in the CONFIGURATION bind-address well.
    pub fn bind_input_mut(&mut self) -> &mut String {
        &mut self.bind_input
    }

    /// Inline error under the CONFIGURATION wells, when the last edit was invalid.
    pub fn config_error(&self) -> Option<&str> {
        self.config_error.as_deref()
    }

    /// Whether the last accepted edit should still show its `SAVED` label.
    pub fn config_saved(&self) -> bool {
        self.config_saved_at
            .is_some_and(|at| at.elapsed() < CONFIG_SAVED_HOLD)
    }

    /// Called after the operator typed in the PORT well.
    ///
    /// Valid text is committed — config, `config.toml` and the server thread.
    /// Invalid text leaves the last valid port in force and raises an inline
    /// error instead of applying garbage.
    pub fn on_port_edited(&mut self) {
        match crate::config::parse_port(&self.port_input) {
            Some(port) => {
                let host = self.get_server_bind_addr();
                self.commit_server_config(host, port);
            }
            None => self.config_error = Some(PORT_ERROR.to_string()),
        }
    }

    /// Called after the operator typed in the BIND ADDRESS well.
    ///
    /// Same contract as [`Self::on_port_edited`].
    pub fn on_bind_edited(&mut self) {
        match crate::config::parse_bind_host(&self.bind_input) {
            Some(ip) => {
                let port = self.get_server_port();
                self.commit_server_config(ip.to_string(), port);
            }
            None => self.config_error = Some(BIND_ERROR.to_string()),
        }
    }

    /// Put a validated host/port in force without restarting the app.
    ///
    /// The value goes to `AppState::config`, is handed to the server thread
    /// (whose next `Start` binds it), and marks the config dirty so the existing
    /// debounced flush — [`Self::persist_library`] — writes the whole config,
    /// `[server]` section included, to `config.toml`.
    fn commit_server_config(&mut self, host: String, port: u16) {
        {
            let mut config = self.app_state.config.lock().unwrap();
            config.server.host = host.clone();
            config.server.port = port;
        }

        self.push_bind_to_server(host, port);
        self.library_dirty = true;
    }

    /// Ask the server thread to use `host:port` for the next start.
    fn push_bind_to_server(&mut self, host: String, port: u16) {
        let (response_tx, response_rx) = bounded(1);
        let cmd = ServerCommand::SetBind {
            host,
            port,
            response: response_tx,
        };

        // Non-blocking send to avoid freezing GUI if channel is full
        if let Err(e) = self.app_state.server_cmd_tx.try_send(cmd) {
            error!(
                "Failed to send bind address to server (channel full): {}",
                e
            );
            self.config_error = Some("SERVER BUSY".to_string());
            return;
        }

        let results = self.bind_result_tx.clone();
        std::thread::spawn(move || {
            if let Ok(result) = response_rx.recv_timeout(Duration::from_secs(5)) {
                let _ = results.try_send(result);
            }
        });
    }

    /// Collect bind-change replies from the server thread.
    fn poll_bind_results(&mut self) {
        while let Ok(result) = self.bind_result_rx.try_recv() {
            match result {
                Ok(()) => {
                    self.config_error = None;
                    self.config_saved_at = Some(Instant::now());
                }
                Err(message) => self.config_error = Some(message.to_uppercase()),
            }
        }
    }

    /// Copied feedback
    pub fn set_copied_feedback(&self, value: bool) {
        *self.copied_feedback.lock().unwrap() = value;
        if value {
            *self.copied_feedback_timer.lock().unwrap() = Some(Instant::now());
        }
    }

    pub fn copied_feedback(&self) -> bool {
        let feedback = *self.copied_feedback.lock().unwrap();
        if feedback {
            if let Some(timer) = *self.copied_feedback_timer.lock().unwrap() {
                if timer.elapsed() > Duration::from_secs(2) {
                    *self.copied_feedback.lock().unwrap() = false;
                    *self.copied_feedback_timer.lock().unwrap() = None;
                    return false;
                }
            }
        }
        feedback
    }

    /// Open native file dialog to add sound
    pub fn open_file_dialog(&mut self) {
        if let Some(path) = FileDialog::new()
            .add_filter("Audio Files", &["mp3", "wav", "ogg", "flac", "opus"])
            .set_title("Select Audio File")
            .pick_file()
        {
            self.request_load_sound(path);
        }
    }

    // ===== YouTube links =====

    /// Open the "add from YouTube" modal.
    pub fn open_add_youtube(&mut self) {
        if self.youtube_busy {
            return; // one download at a time; the modal is already reporting it
        }
        self.dialog = Dialog::AddYouTube {
            url: String::new(),
            error: None,
        };
    }

    /// Whether a YouTube download is currently in flight.
    pub fn youtube_busy(&self) -> bool {
        self.youtube_busy
    }

    /// The YouTube worker's latest progress line.
    pub fn youtube_status(&self) -> String {
        self.youtube_status.lock().unwrap().clone()
    }

    /// Apply the open "add from YouTube" modal: validate the link and hand the
    /// download to a worker thread. Validation failures stay in the modal.
    pub fn submit_add_youtube(&mut self) {
        let url = match &self.dialog {
            Dialog::AddYouTube { url, .. } => url.clone(),
            _ => return,
        };
        let Some(video_id) = crate::audio::youtube::extract_video_id(&url) else {
            if let Dialog::AddYouTube { error, .. } = &mut self.dialog {
                *error = Some("Not a YouTube video link".to_string());
            }
            return;
        };

        let (result_tx, result_rx) = bounded(1);
        self.youtube_result_rx = result_rx;
        *self.youtube_status.lock().unwrap() = "Connecting…".to_string();
        self.youtube_busy = true;

        let status = self.youtube_status.clone();
        let sink: crate::audio::youtube::StatusSink = Arc::new(move |line: &str| {
            if let Ok(mut status) = status.lock() {
                *status = line.to_string();
            }
        });
        std::thread::Builder::new()
            .name("youtube-download".into())
            .spawn(move || {
                let result = crate::audio::youtube::download_audio(&url, &video_id, &sink);
                // The channel is closed with the app; failing to report simply
                // means nobody is listening for the result any more.
                let _ = result_tx.send(result);
            })
            .expect("spawn youtube download worker");
    }

    /// Collect YouTube download outcomes from the worker.
    ///
    /// Success hands the file to the normal loader (the row appears as soon as
    /// it is decoded, sharing every existing path). Failure lands in the modal
    /// if it is still open, and in the log in any case.
    fn poll_youtube_results(&mut self) {
        while let Ok(result) = self.youtube_result_rx.try_recv() {
            let result = match result {
                Ok(path) => path,
                Err(message) => {
                    error!("YouTube download failed: {}", message);
                    if matches!(self.dialog, Dialog::AddYouTube { .. }) {
                        if let Dialog::AddYouTube { error, .. } = &mut self.dialog {
                            *error = Some(message);
                        }
                    }
                    self.youtube_busy = false;
                    continue;
                }
            };

            info!("YouTube download finished: {}", result.display());
            if matches!(self.dialog, Dialog::AddYouTube { .. }) {
                self.dialog = Dialog::None;
            }
            self.youtube_busy = false;
            self.request_load_sound(result);
        }
    }

    // ===== Library, groups and filters =====

    /// The saved library: groups plus their sounds, in display order.
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// Which group the library panel is currently focused on.
    pub fn group_filter(&self) -> GroupFilter {
        self.group_filter
    }

    /// Focus the library on a group, or on everything.
    pub fn set_group_filter(&mut self, filter: GroupFilter) {
        self.group_filter = filter;
    }

    /// Number of named groups.
    pub fn group_count(&self) -> usize {
        self.library.groups().len()
    }

    /// Display name of a group, or `None` when the id is unknown.
    pub fn group_name(&self, id: GroupId) -> Option<String> {
        self.library.group_name(id).map(str::to_string)
    }

    /// Group membership of a sound, as stored in the library.
    pub fn sound_group(&self, path: &Path) -> Option<GroupId> {
        self.library.sound(path).and_then(|sound| sound.group)
    }

    /// Sounds to render, honouring the active group filter.
    ///
    /// Order is the load order (the mixer's insertion order), so rows never jump
    /// when a group changes.
    pub fn visible_sounds(&self) -> Vec<SoundEntry> {
        let filter = self.group_filter;
        self.get_sounds()
            .into_iter()
            .filter(|entry| self.library.matches(&entry.path, filter))
            .collect()
    }

    /// How many sounds a given filter would show (used by the rail badges).
    pub fn count_for(&self, filter: GroupFilter) -> usize {
        self.library.count_for(filter)
    }

    // ===== Modals =====

    /// The modal currently open, if any.
    pub fn dialog(&self) -> &Dialog {
        &self.dialog
    }

    /// Mutable access to the open modal, for its text field.
    pub fn dialog_mut(&mut self) -> &mut Dialog {
        &mut self.dialog
    }

    /// Dismiss the open modal without applying it.
    pub fn close_dialog(&mut self) {
        self.dialog = Dialog::None;
    }

    /// Open the "new group" modal.
    pub fn open_new_group(&mut self) {
        self.dialog = Dialog::NewGroup {
            name: String::new(),
            error: None,
        };
    }

    /// Open the "rename group" modal, pre-filled with the current name.
    pub fn open_rename_group(&mut self, id: GroupId) {
        let name = self.group_name(id).unwrap_or_default();
        self.dialog = Dialog::RenameGroup {
            id,
            name,
            error: None,
        };
    }

    /// Open the delete confirmation for a group.
    pub fn open_delete_group(&mut self, id: GroupId) {
        self.dialog = Dialog::DeleteGroup { id };
    }

    /// Open the "move sound to group" modal.
    pub fn open_assign_sound(&mut self, sound_id: SoundId, name: impl Into<String>) {
        self.dialog = Dialog::AssignSound {
            sound_id,
            name: name.into(),
        };
    }

    /// Apply the open "new group" modal. Validation failures stay in the modal.
    pub fn submit_new_group(&mut self) {
        let outcome = match &self.dialog {
            Dialog::NewGroup { name, .. } => Some(self.library.add_group(name)),
            _ => None,
        };
        match outcome {
            Some(Some(id)) => {
                self.library_dirty = true;
                self.group_filter = GroupFilter::Group(id);
                self.dialog = Dialog::None;
            }
            Some(None) => {
                if let Dialog::NewGroup { name, error } = &mut self.dialog {
                    *error = Some(group_name_error(name));
                }
            }
            None => {}
        }
    }

    /// Apply the open "rename group" modal.
    pub fn submit_rename_group(&mut self) {
        let outcome = match &self.dialog {
            Dialog::RenameGroup { id, name, .. } => Some(self.library.rename_group(*id, name)),
            _ => None,
        };
        match outcome {
            Some(true) => {
                self.library_dirty = true;
                self.dialog = Dialog::None;
            }
            Some(false) => {
                if let Dialog::RenameGroup { name, error, .. } = &mut self.dialog {
                    *error = Some(group_name_error(name));
                }
            }
            None => {}
        }
    }

    /// Delete the group named in the open confirmation modal.
    pub fn confirm_delete_group(&mut self) {
        let id = match &self.dialog {
            Dialog::DeleteGroup { id } => Some(*id),
            _ => None,
        };
        let Some(id) = id else { return };
        if self.library.remove_group(id) {
            self.library_dirty = true;
            if self.group_filter == GroupFilter::Group(id) {
                self.group_filter = GroupFilter::All;
            }
            self.sync_group_membership();
        }
        self.dialog = Dialog::None;
    }

    /// Move the sound named in the open modal into `group` (`None` = ungrouped).
    pub fn assign_sound_group(&mut self, group: Option<GroupId>) {
        let sound_id = match &self.dialog {
            Dialog::AssignSound { sound_id, .. } => Some(*sound_id),
            _ => None,
        };
        let Some(sound_id) = sound_id else { return };
        let path = self
            .sounds
            .lock()
            .unwrap()
            .get(&sound_id)
            .map(|entry| entry.path.clone());
        if let Some(path) = path {
            if self.library.assign(&path, group) {
                self.library_dirty = true;
                if let Some(entry) = self.sounds.lock().unwrap().get_mut(&sound_id) {
                    entry.group = group;
                }
            }
        }
        self.dialog = Dialog::None;
    }

    // ===== Transport =====

    /// Stop every playing sound at once (the command-bar panic control).
    pub fn stop_all_sounds(&mut self) {
        let (response_tx, _response_rx) = bounded(1);
        let cmd = AudioCommand::StopAll {
            response: response_tx,
        };
        if let Err(e) = self.app_state.audio_cmd_tx.try_send(cmd) {
            error!("Failed to send stop all command (channel full): {}", e);
            return;
        }
        for entry in self.sounds.lock().unwrap().values_mut() {
            entry.playing = false;
        }
    }

    /// Set a sound's volume and mirror it into the library so it persists.
    pub fn request_set_sound_volume(&mut self, id: SoundId, path: &Path, volume: f32) {
        self.request_set_volume(id, volume);
        if self.library.set_volume(path, volume) {
            self.library_dirty = true;
        }
    }

    /// Toggle a sound's "repeat" flag.
    ///
    /// The flag is mirrored into the library (so it persists) and pushed to the
    /// engine, where it takes effect on the live track immediately.
    pub fn request_set_sound_loop(&mut self, id: SoundId, path: &Path, looped: bool) {
        // Update the cache first so the row redraws lit on this same frame.
        if let Some(entry) = self.sounds.lock().unwrap().get_mut(&id) {
            entry.looped = looped;
        }
        if self.library.set_looped(path, looped) {
            self.library_dirty = true;
        }

        let (response_tx, _response_rx) = bounded(1);
        let cmd = AudioCommand::SetLoop {
            id,
            looped,
            response: response_tx,
        };
        if let Err(e) = self.app_state.audio_cmd_tx.try_send(cmd) {
            error!("Failed to send set loop command (channel full): {}", e);
        }
    }

    /// True while anything needs a repaint to animate (pulse, breathing dot).
    pub fn tooltip_requested(&self) -> bool {
        let playing = self.sounds.lock().unwrap().values().any(|e| e.playing);
        let animating = matches!(
            self.server_state.lock().unwrap().status,
            ServerStatus::Running | ServerStatus::Starting
        );
        playing || animating
    }

    /// Mute / unmute the master bus, remembering the previous level.
    pub fn toggle_mute(&mut self) {
        match self.muted {
            Some(previous) => {
                self.set_master_volume(previous);
                self.muted = None;
            }
            None => {
                self.muted = Some(self.get_master_volume());
                self.set_master_volume(0.0);
            }
        }
    }

    /// Is the master bus currently muted?
    pub fn is_muted(&self) -> bool {
        self.muted.is_some()
    }

    // ===== Persistence =====

    /// Write the library into `config.toml`. Safe to call at any time.
    pub fn persist_library(&mut self) {
        let snapshot = self.library.to_config();
        let groups = snapshot.groups.len();
        let sounds = snapshot.sounds.len();

        let mut config = self.app_state.config.lock().unwrap();
        config.library = snapshot;
        if let Err(e) = config.save() {
            error!("Failed to save library to config.toml: {}", e);
            return;
        }
        drop(config);

        self.library_dirty = false;
        self.last_persist = Instant::now();
        debug!("Library saved ({} groups, {} sounds)", groups, sounds);
    }

    /// Flush the library at most every `LIBRARY_FLUSH` while it is dirty.
    fn maybe_persist(&mut self) {
        if self.library_dirty && self.last_persist.elapsed() >= LIBRARY_FLUSH {
            self.persist_library();
        }
    }

    /// Copy group membership from the library into the cached sound entries.
    fn sync_group_membership(&mut self) {
        let mut sounds = self.sounds.lock().unwrap();
        for entry in sounds.values_mut() {
            entry.group = self
                .library
                .sound(&entry.path)
                .and_then(|sound| sound.group);
        }
    }

    /// Update function called each frame - process audio commands on main thread
    pub fn update(&mut self, ctx: &Context) {
        // Process pending audio commands (runs on main thread where AudioEngine lives)
        self.process_audio_commands();
        self.finish_decoded();
        self.poll_youtube_results();

        // Auto-detect tracks that finished playing (mixer marks them stopped but keeps them)
        // so the GUI button switches from "Stop" back to "Play" and the sound can be removed.
        if let Ok(mut engine) = self.audio_engine.lock() {
            if let Some(engine) = engine.as_mut() {
                let sounds = self.sounds.lock().unwrap();
                let finished: Vec<SoundId> = sounds
                    .iter()
                    .filter(|(_, entry)| entry.playing)
                    .filter_map(|(id, _entry)| match engine.is_sound_playing(*id) {
                        Ok(false) => Some(*id),
                        Ok(true) | Err(_) => None,
                    })
                    .collect();
                drop(sounds);
                if !finished.is_empty() {
                    let mut sounds = self.sounds.lock().unwrap();
                    for id in finished {
                        if let Some(entry) = sounds.get_mut(&id) {
                            entry.playing = false;
                            debug!("Auto-stopped finished sound {}", id);
                        }
                    }
                }
            }
        }

        // Feed the engine the library files that did not fit the channel on
        // the previous pass.
        self.pump_pending_loads();

        // Poll server status
        self.poll_server_status();

        // Collect replies for CONFIGURATION edits pushed to the server thread
        self.poll_bind_results();

        // Keep the status strip's "local audio" readout current
        self.poll_local_output();

        // Flush the library if it changed and the debounce window elapsed.
        self.maybe_persist();

        // Request repaint for live updates: the level meter needs a steady
        // frame rate while anything plays (or the meter is still falling), and
        // the YouTube modal reads its worker's progress line.
        let animating = self.output_meter > 0.0
            || self.youtube_busy
            || self.sounds.lock().unwrap().values().any(|s| s.playing);
        ctx.request_repaint_after(Duration::from_millis(if self.youtube_busy {
            200
        } else if animating {
            33
        } else {
            500
        }));
    }

    /// Register the files the decoder worker has finished.
    fn finish_decoded(&mut self) {
        while let Ok((path, response, decoded)) = self.decoded_rx.try_recv() {
            let engine_guard = self.audio_engine.lock().unwrap();
            let Some(engine) = engine_guard.as_ref() else {
                let _ = response.send(Err(anyhow::anyhow!("audio engine stopped")));
                continue;
            };
            match decoded {
                Ok(audio) => {
                    let sound_id = engine.load_sound_from_data(audio);
                    let (duration, sample_rate, _) =
                        engine.sound_info(sound_id).unwrap_or((0.0, 48000, 2));
                    // YouTube downloads arrive as `<title> [<id>]` on disk;
                    // their ` [id]` stamp is trimmed so the row shows the
                    // title. Any other file keeps its plain stem.
                    let name = crate::audio::youtube::display_name(std::path::Path::new(&path));

                    // A sound that was saved at a lower level must come
                    // back at that level: the engine starts every sound
                    // at 1.0, so the saved value is pushed into it here
                    // rather than being silently forgotten on restart.
                    let path_buf = PathBuf::from(&path);
                    let saved_volume = self
                        .library
                        .sound(&path_buf)
                        .map(|sound| sound.volume)
                        .unwrap_or(1.0);
                    if let Err(e) = engine.set_sound_volume(sound_id, saved_volume) {
                        warn!(
                            "Could not apply saved volume {:.2} to {}: {}",
                            saved_volume,
                            path_buf.display(),
                            e
                        );
                    }

                    let info = SoundInfo {
                        id: sound_id,
                        name: name.clone(),
                        path: path.clone(),
                        duration,
                        is_playing: false,
                        volume: saved_volume,
                    };

                    // Register the sound in the saved library. When the
                    // operator is focused on a group, the new sound lands
                    // in that group — the RPG-sounds workflow.
                    let target_group = match self.group_filter {
                        GroupFilter::Group(id) => Some(id),
                        _ => None,
                    };
                    if self
                        .library
                        .insert_sound(&path_buf, &name, saved_volume, target_group)
                    {
                        self.library_dirty = true;
                    }
                    let group = self.library.sound(&path_buf).and_then(|s| s.group);
                    // A sound saved with "repeat" comes back repeating.
                    let looped = self
                        .library
                        .sound(&path_buf)
                        .map(|s| s.looped)
                        .unwrap_or(false);

                    // Update local cache
                    let entry = SoundEntry {
                        id: sound_id,
                        name,
                        path: path_buf,
                        playing: false,
                        volume: saved_volume,
                        looped,
                        group,
                        duration,
                        sample_rate,
                    };
                    self.sounds.lock().unwrap().insert(sound_id, entry);
                    self.sound_order.lock().unwrap().push(sound_id);
                    let _ = response.send(Ok(info));
                }
                Err(e) => {
                    let _ = response.send(Err(anyhow::anyhow!("{path}: {e}")));
                }
            }
        }
    }

    /// Process pending audio commands on the main thread
    fn process_audio_commands(&mut self) {
        // Try to get commands without blocking
        while let Ok(cmd) = self.audio_cmd_rx.try_recv() {
            if let Some(engine) = self.audio_engine.lock().unwrap().as_mut() {
                match cmd {
                    AudioCommand::LoadSound { path, response } => {
                        // Decoded off the GUI thread; finished in `finish_decoded`.
                        if let Err(e) = self.decode_tx.send((path, response)) {
                            let (_, response) = e.into_inner();
                            let _ = response.send(Err(anyhow::anyhow!("decoder stopped")));
                        }
                    }
                    AudioCommand::PlaySound { id, response } => {
                        // Play through the sound's "repeat" flag so a looping
                        // sound keeps going without the UI re-triggering it.
                        let looped = self
                            .sounds
                            .lock()
                            .unwrap()
                            .get(&id)
                            .map(|entry| entry.looped)
                            .unwrap_or(false);
                        let result = engine.play_with_loop(id, looped);
                        if result.is_ok() {
                            if let Some(entry) = self.sounds.lock().unwrap().get_mut(&id) {
                                entry.playing = true;
                            }
                        }
                        // Convert TrackHandle to () for response
                        let response_result = result.map(|_| ()).map_err(|e| e.into());
                        let _ = response.send(response_result);
                    }
                    AudioCommand::StopSound { id, response } => {
                        let result = engine.stop_sound(id);
                        if result.is_ok() {
                            if let Some(entry) = self.sounds.lock().unwrap().get_mut(&id) {
                                entry.playing = false;
                            }
                        }
                        let _ = response.send(result.map_err(|e| e.into()));
                    }
                    AudioCommand::SetVolume {
                        id,
                        volume,
                        response,
                    } => {
                        // Per-sound level: it scales this sound only. Routing it to
                        // `set_master_volume` made one row's slider drag the whole
                        // mix, so the other sounds moved with it.
                        let result = engine.set_sound_volume(id, volume);
                        if result.is_ok() {
                            if let Some(entry) = self.sounds.lock().unwrap().get_mut(&id) {
                                entry.volume = volume;
                            }
                        }
                        let _ = response.send(result.map_err(|e| e.into()));
                    }
                    AudioCommand::SetMasterVolume { volume, response } => {
                        let result = engine.set_master_volume(volume);
                        if result.is_ok() {
                            *self.master_volume.lock().unwrap() = volume;
                        }
                        let _ = response.send(result.map_err(|e| e.into()));
                    }
                    AudioCommand::SetLoop {
                        id,
                        looped,
                        response,
                    } => {
                        // Applies to the live track, so flipping the toggle on a
                        // sound that is already playing takes effect immediately.
                        let result = engine.set_sound_looped(id, looped);
                        if result.is_ok() {
                            if let Some(entry) = self.sounds.lock().unwrap().get_mut(&id) {
                                entry.looped = looped;
                            }
                        }
                        let _ = response.send(result.map_err(|e| e.into()));
                    }
                    AudioCommand::RemoveSound { id, response } => {
                        let path = self
                            .sounds
                            .lock()
                            .unwrap()
                            .get(&id)
                            .map(|entry| entry.path.clone());
                        let result = engine.unload_sound(id);
                        if result.is_ok() {
                            self.sounds.lock().unwrap().remove(&id);
                            self.sound_order.lock().unwrap().retain(|&x| x != id);
                            if let Some(path) = path {
                                if self.library.remove_sound(&path) {
                                    self.library_dirty = true;
                                }
                            }
                        }
                        let _ = response.send(result.map_err(|e| e.into()));
                    }
                    AudioCommand::StopAll { response } => {
                        let result = engine.stop_all();
                        if result.is_ok() {
                            for entry in self.sounds.lock().unwrap().values_mut() {
                                entry.playing = false;
                            }
                        }
                        let _ = response.send(result.map_err(|e| e.into()));
                    }
                }
            } else {
                // Engine not available - handle each variant separately due to different response types
                match cmd {
                    AudioCommand::LoadSound { response, .. } => {
                        let _ = response.send(Err(anyhow::anyhow!("Engine not running")));
                    }
                    AudioCommand::PlaySound { response, .. } => {
                        let _ = response.send(Err(anyhow::anyhow!("Engine not running")));
                    }
                    AudioCommand::StopSound { response, .. } => {
                        let _ = response.send(Err(anyhow::anyhow!("Engine not running")));
                    }
                    AudioCommand::SetVolume { response, .. } => {
                        let _ = response.send(Err(anyhow::anyhow!("Engine not running")));
                    }
                    AudioCommand::SetLoop { response, .. } => {
                        let _ = response.send(Err(anyhow::anyhow!("Engine not running")));
                    }
                    AudioCommand::SetMasterVolume { response, .. } => {
                        let _ = response.send(Err(anyhow::anyhow!("Engine not running")));
                    }
                    AudioCommand::RemoveSound { response, .. } => {
                        let _ = response.send(Err(anyhow::anyhow!("Engine not running")));
                    }
                    AudioCommand::StopAll { response } => {
                        let _ = response.send(Ok(()));
                    }
                };
            }
        }
    }
}
