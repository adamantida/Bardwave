//! Shared types for GUI integration
//! These types are used by both the GUI and main.rs for communication

use crate::audio::{DuckTarget, SoundId};
use crate::config::Bus;
use crossbeam_channel::Sender;
use std::fmt::Debug;

/// Commands from GUI to audio engine
#[derive(Debug, Clone)]
pub enum AudioCommand {
    /// Load a sound file and return its SoundInfo
    LoadSound {
        path: String,
        response: Sender<anyhow::Result<SoundInfo>>,
    },
    /// Play a sound by ID
    PlaySound {
        id: SoundId,
        response: Sender<anyhow::Result<()>>,
    },
    /// Stop a sound by ID
    StopSound {
        id: SoundId,
        response: Sender<anyhow::Result<()>>,
    },
    /// Set per-sound volume (track volume)
    SetVolume {
        id: SoundId,
        volume: f32,
        response: Sender<anyhow::Result<()>>,
    },
    /// Turn repeating on or off for a sound
    SetLoop {
        id: SoundId,
        looped: bool,
        response: Sender<anyhow::Result<()>>,
    },
    /// Set master volume
    SetMasterVolume {
        volume: f32,
        response: Sender<anyhow::Result<()>>,
    },
    /// Remove a sound from the library
    RemoveSound {
        id: SoundId,
        response: Sender<anyhow::Result<()>>,
    },
    /// Stop all playback
    StopAll {
        response: Sender<anyhow::Result<()>>,
    },
    /// Switch to a scene (library scene id, a UUID string): the Director
    /// starts its sounds, retunes common ones and fades out the rest.
    ActivateScene {
        id: String,
        response: Sender<anyhow::Result<()>>,
    },
    /// Fade out the active scene's sounds.
    StopScene {
        response: Sender<anyhow::Result<()>>,
    },
    /// Save the mix that plays now as a new scene. The reply is the scene as
    /// stored in the library.
    SnapshotScene {
        name: String,
        response: Sender<anyhow::Result<crate::config::SceneConfig>>,
    },
    /// Play a sound once over everything, independent of the active scene.
    PlayStinger {
        id: SoundId,
        response: Sender<anyhow::Result<()>>,
    },
    /// Fade everything out over `fade_ms` milliseconds.
    StopAllFade {
        fade_ms: u32,
        response: Sender<anyhow::Result<()>>,
    },
    /// Music playlist: skip to the next track.
    PlaylistNext {
        response: Sender<anyhow::Result<()>>,
    },
    /// Music playlist: previous track (or restart the current one).
    PlaylistPrev {
        response: Sender<anyhow::Result<()>>,
    },
    /// Music playlist: jump to a position (milliseconds) in the track.
    PlaylistSeek {
        ms: u64,
        response: Sender<anyhow::Result<()>>,
    },
    /// Music playlist: pause.
    PlaylistPause {
        response: Sender<anyhow::Result<()>>,
    },
    /// Music playlist: resume after a pause.
    PlaylistResume {
        response: Sender<anyhow::Result<()>>,
    },
    /// Music playlist: shuffle on or off.
    PlaylistSetShuffle {
        shuffle: bool,
        response: Sender<anyhow::Result<()>>,
    },
    /// Music playlist: end-of-list behaviour.
    PlaylistSetRepeat {
        repeat: crate::config::RepeatMode,
        response: Sender<anyhow::Result<()>>,
    },
    /// Volume of a mixer bus (`0.0..=1.0`).
    SetBusVolume {
        bus: Bus,
        volume: f32,
        response: Sender<anyhow::Result<()>>,
    },
    /// Duck buses until `DuckRelease`; `None` parameters use `[audio] duck_*`.
    Duck {
        target: DuckTarget,
        amount: Option<f32>,
        attack_ms: Option<u32>,
        release_ms: Option<u32>,
        response: Sender<anyhow::Result<()>>,
    },
    /// Release a latched duck.
    DuckRelease {
        target: DuckTarget,
        response: Sender<anyhow::Result<()>>,
    },
    /// Hold mode: duck while a key is held (`active`), release on key-up.
    DuckHold {
        target: DuckTarget,
        active: bool,
        response: Sender<anyhow::Result<()>>,
    },
    /// Duck when not ducked, release when ducked.
    DuckToggle {
        target: DuckTarget,
        response: Sender<anyhow::Result<()>>,
    },
    /// Switch loudness normalization on or off (and set its target, LUFS).
    /// Turning it on measures the loaded sounds that have no gain yet.
    SetNormalize {
        enabled: bool,
        target_lufs: Option<f32>,
        response: Sender<anyhow::Result<()>>,
    },
}

impl AudioCommand {
    /// The music command and the reply channel of a `Playlist*` command.
    pub fn into_playlist(
        self,
    ) -> Result<(crate::audio::PlaylistCommand, Sender<anyhow::Result<()>>), Self> {
        use crate::audio::PlaylistCommand as C;
        Ok(match self {
            Self::PlaylistNext { response } => (C::Next, response),
            Self::PlaylistPrev { response } => (C::Prev, response),
            Self::PlaylistSeek { ms, response } => (C::Seek(ms), response),
            Self::PlaylistPause { response } => (C::Pause, response),
            Self::PlaylistResume { response } => (C::Resume, response),
            Self::PlaylistSetShuffle { shuffle, response } => (C::SetShuffle(shuffle), response),
            Self::PlaylistSetRepeat { repeat, response } => (C::SetRepeat(repeat), response),
            other => return Err(other),
        })
    }
}

/// Sound information for GUI display
#[derive(Debug, Clone)]
pub struct SoundInfo {
    pub id: SoundId,
    pub name: String,
    pub path: String,
    pub duration: f32,
    pub is_playing: bool,
    pub volume: f32,
}

/// Commands from GUI to network server
#[derive(Debug, Clone)]
pub enum ServerCommand {
    /// Start the network server
    Start {
        response: Sender<anyhow::Result<String>>,
    },
    /// Stop the network server
    Stop {
        response: Sender<anyhow::Result<()>>,
    },
    /// Point the server at a new bind address / port for the next start.
    ///
    /// Editing the CONFIGURATION fields must take effect without restarting the
    /// app, so the server thread keeps these values instead of the bind address
    /// captured when the process started. The server refuses the change while it
    /// is running (the GUI greys the fields out in that state anyway) and
    /// re-validates both values before accepting them.
    SetBind {
        /// Bind address as typed, e.g. `0.0.0.0`, `127.0.0.1`, `::1`
        host: String,
        /// Port as typed, validated to `1..=65535`
        port: u16,
        /// `Ok` once the new values are in force, `Err` with a displayable reason
        response: Sender<Result<(), String>>,
    },
    /// Get current server status
    GetStatus { response: Sender<ServerStatus> },
}

/// Network server status for GUI display
#[derive(Debug, Clone)]
pub struct ServerStatus {
    pub running: bool,
    pub address: Option<String>,
    pub listener_count: usize,
}

/// Shared application state for GUI integration
#[derive(Clone)]
pub struct AppState {
    /// Channel to send commands from GUI to audio engine
    pub audio_cmd_tx: Sender<AudioCommand>,

    /// Channel to send commands from GUI to network server
    pub server_cmd_tx: Sender<ServerCommand>,

    /// Current connected listener count (for display)
    pub listener_count: std::sync::Arc<std::sync::Mutex<usize>>,

    /// Application configuration
    pub config: std::sync::Arc<std::sync::Mutex<crate::config::Config>>,

    /// Router port forwarding state, written by the server thread
    pub upnp_status: std::sync::Arc<std::sync::Mutex<crate::network::upnp::UpnpStatus>>,

    /// Problems that keep players out (VPN route, firewall), written by the
    /// server thread on Start and cleared on Stop.
    pub network_notes: std::sync::Arc<std::sync::Mutex<Vec<String>>>,

    /// Access key of the listener stream, shared with the server: the
    /// console puts it into every link it copies and can issue a new one.
    pub access: crate::network::AccessGuard,

    /// Game-master token of the remote control panel, shared with the server:
    /// the console shows the link and can replace the token (the old one stops
    /// working at once).
    pub gm: crate::network::GmGuard,

    /// Whether this process actually serves `/control` (today only with
    /// `--server-only`). The console says so next to the game-master link.
    pub gm_panel_served: bool,

    /// Where the phone panel's commands arrive and its state goes, when the
    /// console serves the panel (see `gui::gm_bridge`).
    pub gm_panel: Option<crate::gui::gm_bridge::GmPanelLink>,
    /// Scene / track titles the console publishes for the listeners' page
    /// (shared with the broadcaster, see `BroadcasterConfig::now_playing`).
    pub now_playing: crate::network::broadcast::NowPlayingFeed,
}

impl AppState {
    /// Create a new AppState with the given channels and config
    pub fn new(
        audio_cmd_tx: Sender<AudioCommand>,
        server_cmd_tx: Sender<ServerCommand>,
        listener_count: std::sync::Arc<std::sync::Mutex<usize>>,
        config: std::sync::Arc<std::sync::Mutex<crate::config::Config>>,
    ) -> Self {
        Self {
            audio_cmd_tx,
            server_cmd_tx,
            listener_count,
            config,
            upnp_status: Default::default(),
            network_notes: Default::default(),
            access: Default::default(),
            gm: Default::default(),
            gm_panel_served: false,
            gm_panel: None,
            now_playing: Default::default(),
        }
    }

    /// Use `feed` as the listeners' "now playing" feed.
    pub fn with_now_playing(mut self, feed: crate::network::broadcast::NowPlayingFeed) -> Self {
        self.now_playing = feed;
        self
    }

    /// Use `gm` as the remote panel's token guard; `served` says whether the
    /// `/control` routes are mounted in this process.
    pub fn with_gm(mut self, gm: crate::network::GmGuard, served: bool) -> Self {
        self.gm = gm;
        self.gm_panel_served = served;
        self
    }

    /// Serve the phone panel from this console: its commands are run by the
    /// window's own loop and its state is the window's state.
    pub fn with_gm_panel(mut self, link: crate::gui::gm_bridge::GmPanelLink) -> Self {
        self.gm_panel = Some(link);
        self.gm_panel_served = true;
        self
    }

    /// Use `access` as the stream's access key guard.
    pub fn with_access(mut self, access: crate::network::AccessGuard) -> Self {
        self.access = access;
        self
    }
}
