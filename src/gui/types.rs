//! Shared types for GUI integration
//! These types are used by both the GUI and main.rs for communication

use crate::audio::SoundId;
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
        }
    }

    /// Use `access` as the stream's access key guard.
    pub fn with_access(mut self, access: crate::network::AccessGuard) -> Self {
        self.access = access;
        self
    }
}
