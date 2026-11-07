//! Network module for SoundShared.
//! Handles HTTP server, WebSocket connections, audio broadcasting, and UPnP port forwarding.

pub mod access;
pub mod broadcast;
pub mod control;
pub mod local_ip;
pub mod opus_wire;
pub mod server;
pub mod upnp;
pub mod websocket;

pub use access::{generate_key, with_key, AccessGuard};
pub use broadcast::{
    format_message, AudioBroadcaster, AudioFormatInfo, AudioFrame, BroadcastError,
    BroadcasterConfig, BroadcasterHandle, WIRE_CODEC,
};
pub use local_ip::{candidates, detect_for_bind, is_advertisable, pick_advertised};
pub use opus_wire::{
    wire_encoder, WireEncoder, WIRE_BITRATE, WIRE_CHANNELS, WIRE_FRAME_FRAMES, WIRE_FRAME_SAMPLES,
    WIRE_SAMPLE_RATE,
};
pub use server::{
    api_status, build_router, get, listen_url, loopback_url, serve_app_js, serve_index_html,
    serve_opus_decoder_js, serve_style_css, websocket_handler, NetworkServer, Router, ServerConfig,
    ServerError, ServerHandle, ServerState, TraceLayer,
};
pub use websocket::{handle_websocket_upgrade, WebSocketConfig, WebSocketError, WebSocketSession};
