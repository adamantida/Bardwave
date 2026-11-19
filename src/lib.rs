//! SoundShared - Local Network Audio Streaming
//!
//! A Rust application for streaming audio from a desktop to browsers
//! on the local network via WebSocket.

pub mod audio;
pub mod config;
pub mod error;
pub mod gui;
pub mod hotkeys;
pub mod library_bundle;
pub mod library_query;
pub mod library_scan;
pub mod network;
pub mod paths;
pub mod profiles;

/// SoundShared version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Re-export commonly used types
pub use config::Config;
pub use error::{Result, SoundSharedError};
pub use gui::app::ServerStatus as GuiServerStatus;
pub use gui::app::SoundSharedApp;
pub use gui::types::{AppState, AudioCommand, ServerCommand, SoundInfo};
pub use network::broadcast::{
    AudioBroadcaster, AudioFrame, BroadcastError, BroadcasterConfig, BroadcasterHandle,
};
pub use network::server::{NetworkServer, ServerConfig, ServerError, ServerHandle};
pub use network::websocket::WebSocketConfig;
