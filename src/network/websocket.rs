//! WebSocket session handling for audio streaming

use axum::extract::ws::{Message, WebSocket};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::network::broadcast::{format_message, BroadcasterHandle};

/// Configuration for WebSocket sessions
#[derive(Debug, Clone)]
pub struct WebSocketConfig {
    /// Maximum message size (bytes)
    pub max_message_size: usize,
    /// Ping interval for keepalive
    pub ping_interval: Duration,
    /// Client send buffer size
    pub send_buffer_size: usize,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            max_message_size: 16 * 1024 * 1024, // 16 MB
            ping_interval: Duration::from_secs(30),
            send_buffer_size: 100,
        }
    }
}

/// Errors that can occur in WebSocket handling
#[derive(Debug, thiserror::Error)]
pub enum WebSocketError {
    #[error("WebSocket protocol error: {0}")]
    Protocol(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Client disconnected")]
    Disconnected,
    #[error("Invalid message format: {0}")]
    InvalidMessage(String),
    #[error("Send channel closed")]
    SendClosed,
}

/// A WebSocket session representing a connected listener
pub struct WebSocketSession {
    id: Uuid,
    ws: WebSocket,
    broadcaster: BroadcasterHandle,
    config: WebSocketConfig,
    /// Fires when the server stops. The session closes its socket instead of
    /// holding the server shutdown open indefinitely.
    shutdown_rx: broadcast::Receiver<()>,
    /// Channel to send messages to this client
    msg_tx: mpsc::Sender<Message>,
    /// Receiver for messages to send
    msg_rx: mpsc::Receiver<Message>,
    /// Access-key changes, and the key this listener connected with: when the
    /// GM issues a new key, sessions on the old one are closed.
    revocation: Option<(watch::Receiver<Option<String>>, Option<String>)>,
}

impl WebSocketSession {
    /// Create a new WebSocket session
    pub fn new(
        ws: WebSocket,
        broadcaster: BroadcasterHandle,
        config: WebSocketConfig,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Self {
        let (msg_tx, msg_rx) = mpsc::channel(config.send_buffer_size);
        let id = Uuid::new_v4();

        Self {
            id,
            ws,
            broadcaster,
            config,
            shutdown_rx,
            msg_tx,
            msg_rx,
            revocation: None,
        }
    }

    /// Close this session when the access key changes away from `own_key`.
    pub fn with_revocation(
        mut self,
        keys: watch::Receiver<Option<String>>,
        own_key: Option<String>,
    ) -> Self {
        self.revocation = Some((keys, own_key));
        self
    }

    /// Get session ID
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Run the session - handles incoming/outgoing messages.
    ///
    /// The listener is always removed from the broadcaster before returning,
    /// including when the socket fails: a leaked entry would otherwise keep the
    /// listener count (and the console readout) wrong for the rest of the run.
    pub async fn run(mut self) -> Result<(), WebSocketError> {
        info!("🔌 WebSocket session started: {}", self.id);

        // Register with broadcaster
        let msg_tx = self.msg_tx.clone();
        let client_id = self
            .broadcaster
            .register_client(msg_tx)
            .await
            .map_err(|e| WebSocketError::Protocol(e.to_string()))?;
        info!("🔌 Client registered with ID: {}", client_id);

        let outcome = self.session_loop(client_id).await;

        if let Err(e) = self.broadcaster.unregister_client(client_id).await {
            // The broadcaster is gone (shutdown); the count is moot by then.
            debug!("🔌 Could not unregister client {}: {}", client_id, e);
        }
        info!("🔌 WebSocket session ended: {}", self.id);

        outcome
    }

    /// The session's message pump.
    async fn session_loop(&mut self, client_id: Uuid) -> Result<(), WebSocketError> {
        // Send welcome message
        let welcome = serde_json::json!({
            "type": "welcome",
            "data": {
                "client_id": client_id.to_string(),
                "message": "Connected to the Bardwave audio stream"
            }
        });
        let _ = self.msg_tx.try_send(Message::Text(welcome.to_string()));
        info!("🔌 Welcome message sent to client");

        // A listener that connects while audio is already flowing never sees the
        // first frame that established the format, so the format is replayed here
        // (the browser uses it to label the stream in its info panel).
        if let Some(info) = self.broadcaster.format_info().await {
            let _ = self.msg_tx.try_send(Message::Text(format_message(&info)));
        }

        // Note: axum's WebSocket doesn't have split() so we handle both in one loop
        let mut interval = tokio::time::interval(self.config.ping_interval);
        let mut revocation = self.revocation.take();

        loop {
            tokio::select! {
                // The GM issued a new access key: a listener on the old key is
                // disconnected (with a code the page explains) instead of
                // keeping a leaked link alive until it reconnects.
                changed = async {
                    match revocation.as_mut() {
                        Some((keys, _)) => keys.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let Some((keys, own)) = revocation.as_ref() else { continue };
                    if changed.is_err() {
                        revocation = None;
                        continue;
                    }
                    let current = keys.borrow().clone();
                    if current.is_some() && current != *own {
                        info!("🔌 Access key changed, closing session {}", self.id);
                        self.close_gracefully(Some(axum::extract::ws::CloseFrame {
                            code: crate::network::access::CLOSE_KEY_REVOKED,
                            reason: "access key changed".into(),
                        }))
                        .await;
                        break;
                    }
                }
                // Outgoing: messages from broadcaster -> WebSocket
                Some(msg) = self.msg_rx.recv() => {
                    debug!("🔌 Sending message to client: {:?}", msg);
                    if let Err(e) = self.ws.send(msg).await {
                        warn!("🔌 Failed to send WebSocket message to {}: {}", self.id, e);
                        break;
                    }
                }
                // Incoming: WebSocket messages
                Some(msg_result) = self.ws.recv() => {
                    match msg_result {
                        Ok(Message::Text(text)) => {
                            debug!("🔌 Received text from {}: {}", self.id, text);
                            if let Err(e) = self.handle_text_message(&text).await {
                                warn!("🔌 Error handling text message from {}: {}", self.id, e);
                            }
                        }
                        Ok(Message::Binary(data)) => {
                            debug!("🔌 Received binary from {}: {} bytes", self.id, data.len());
                            // Ignore binary from client (we only send audio)
                        }
                        Ok(Message::Ping(data)) => {
                            debug!("🔌 Ping from {}, sending pong", self.id);
                            let _ = self.msg_tx.try_send(Message::Pong(data));
                        }
                        Ok(Message::Pong(_)) => {
                            debug!("🔌 Pong from {}", self.id);
                            // Keepalive response
                        }
                        Ok(Message::Close(reason)) => {
                            info!("🔌 Client {} requested close: {:?}", self.id, reason);
                            break;
                        }
                        Err(e) => {
                            error!("🔌 WebSocket error for {}: {}", self.id, e);
                            return Err(WebSocketError::Protocol(e.to_string()));
                        }
                    }
                }
                // Periodic ping
                _ = interval.tick() => {
                    debug!("🔌 Sending ping to {}", self.id);
                    let _ = self.msg_tx.try_send(Message::Ping(Vec::new()));
                }
                // The server is stopping: close the socket so the browser shows
                // "off air" and the graceful shutdown of the accept loop can
                // actually finish.
                _ = self.shutdown_rx.recv() => {
                    info!("🔌 Server shutting down, closing session {}", self.id);
                    self.close_gracefully(None).await;
                    break;
                }
            }
        }

        Ok(())
    }

    /// Send a Close frame and wait (briefly) for the client's reply.
    ///
    /// Dropping the socket right after sending Close resets the connection on
    /// Windows, and the reset discards the Close frame on its way to the
    /// browser — the page then sees a plain network error instead of the code
    /// that tells it why (e.g. a revoked access key).
    async fn close_gracefully(&mut self, frame: Option<axum::extract::ws::CloseFrame<'static>>) {
        if self.ws.send(Message::Close(frame)).await.is_err() {
            return;
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(Ok(message)) = self.ws.recv().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        })
        .await;
    }

    /// Handle incoming text messages (control messages from client)
    async fn handle_text_message(&mut self, text: &str) -> Result<(), WebSocketError> {
        debug!("Received text from {}: {}", self.id, text);

        // Parse as JSON for control messages
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(msg_type) = value.get("type").and_then(|v| v.as_str()) {
                match msg_type {
                    "ping" => {
                        let pong = serde_json::json!({"type": "pong"});
                        let _ = self.msg_tx.try_send(Message::Text(pong.to_string()));
                    }
                    "get_format" => {
                        // Client asks for the current audio format. Answering it
                        // used to be a no-op, so a listener that joined late had
                        // no format at all: the browser was left showing whatever
                        // its info panel guessed at startup.
                        match self.broadcaster.format_info().await {
                            Some(info) => {
                                let _ = self.msg_tx.try_send(Message::Text(format_message(&info)));
                            }
                            None => {
                                // No audio has been mixed yet; the format is
                                // announced with the first frame instead.
                                debug!("🔌 No audio format yet for {}", self.id);
                            }
                        }
                    }
                    _ => {
                        debug!("Unknown message type from {}: {}", self.id, msg_type);
                    }
                }
            }
        }

        Ok(())
    }
}

/// Upgrade HTTP request to WebSocket and spawn session
pub async fn handle_websocket_upgrade(
    ws: WebSocket,
    broadcaster: BroadcasterHandle,
    config: WebSocketConfig,
    shutdown_rx: broadcast::Receiver<()>,
    revocation: Option<(watch::Receiver<Option<String>>, Option<String>)>,
) {
    info!("🔌 New WebSocket connection from client");
    let mut session = WebSocketSession::new(ws, broadcaster, config, shutdown_rx);
    if let Some((keys, own)) = revocation {
        session = session.with_revocation(keys, own);
    }

    if let Err(e) = session.run().await {
        warn!("🔌 WebSocket session error: {}", e);
    } else {
        info!("🔌 WebSocket session ended cleanly");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_websocket_config_default() {
        let config = WebSocketConfig::default();
        assert_eq!(config.max_message_size, 16 * 1024 * 1024);
        assert_eq!(config.ping_interval, Duration::from_secs(30));
        assert_eq!(config.send_buffer_size, 100);
    }
}
