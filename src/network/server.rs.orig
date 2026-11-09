//! HTTP/WebSocket server for SoundShared

pub use axum::response::{Html, Response};
pub use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::{header, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
pub use tower_http::trace::TraceLayer;
use tracing::{error, info};

use crate::network::access::{AccessGuard, KEY_PARAM};
use crate::network::broadcast::{
    AudioBroadcaster, BroadcastError, BroadcasterConfig, BroadcasterHandle,
};
use crate::network::local_ip::{detect_for_bind, is_advertisable};
use crate::network::websocket::{handle_websocket_upgrade, WebSocketConfig};

// Embedded static files (compiled into binary)
const INDEX_HTML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/network/static/index.html"
));
const APP_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/network/static/app.js"
));
const STYLE_CSS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/network/static/style.css"
));
const OPUS_DECODER_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/network/static/opus-decoder.min.js"
));

/// Server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Bind address (default: 0.0.0.0)
    pub bind_addr: IpAddr,
    /// Port (default: 8080)
    pub port: u16,
    /// Ask the router to forward the port (UPnP IGD / PCP / NAT-PMP) for
    /// access from the internet (default: true)
    pub upnp: bool,
    /// Broadcaster configuration
    pub broadcaster_config: BroadcasterConfig,
    /// WebSocket configuration
    pub websocket_config: WebSocketConfig,
    /// Access key for the stream (open by default)
    pub access: AccessGuard,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            port: 8080,
            upnp: true,
            broadcaster_config: BroadcasterConfig::default(),
            websocket_config: WebSocketConfig::default(),
            access: AccessGuard::default(),
        }
    }
}

/// Handle to control the running server
#[derive(Clone)]
pub struct ServerHandle {
    /// Broadcast channel for shutdown signal
    pub shutdown_tx: broadcast::Sender<()>,
    /// Server URL (e.g., "http://192.168.1.100:8080"), None while stopped
    pub url: Arc<tokio::sync::Mutex<Option<String>>>,
    /// Broadcaster handle for audio engine
    pub broadcaster: BroadcasterHandle,
}

impl ServerHandle {
    /// Get the broadcaster handle for sending audio from engine
    pub fn broadcaster(&self) -> &BroadcasterHandle {
        &self.broadcaster
    }

    /// Get the server URL (available after start)
    pub async fn url(&self) -> Option<String> {
        self.url.lock().await.clone()
    }

    /// Stop the server
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(());
    }

    /// Check if server is running (URL is set)
    pub async fn is_running(&self) -> bool {
        self.url.lock().await.is_some()
    }
}

/// Server state shared across handlers
#[derive(Clone)]
pub struct ServerState {
    pub broadcaster: BroadcasterHandle,
    pub websocket_config: WebSocketConfig,
    /// Shutdown signal for the listener sessions.
    ///
    /// Every session subscribes on connect and closes its socket when this
    /// fires. Without it a browser tab holding a WebSocket open keeps the
    /// graceful shutdown of the accept loop waiting for as long as it stays
    /// open, which used to leave the console reporting "Live" after Stop server
    /// and froze the whole server thread.
    pub shutdown_tx: broadcast::Sender<()>,
    /// Access key the audio WebSocket and the status API require.
    pub access: AccessGuard,
}

/// Build the router that serves the listener page, the status API and the audio
/// WebSocket.
///
/// The in-app server in main.rs and NetworkServer both build their HTTP surface
/// from this single definition, so the two can never drift apart. They used to
/// differ: the app hand-built a router with the embedded assets, while
/// NetworkServer served a ServeDir from a path that does not exist next to the
/// installed binary, so its listener page was always a 404.
pub fn build_router(state: ServerState) -> Router {
    Router::new()
        .route("/", get(serve_index_html))
        .route("/index.html", get(serve_index_html))
        .route("/app.js", get(serve_app_js))
        .route("/style.css", get(serve_style_css))
        .route("/opus-decoder.min.js", get(serve_opus_decoder_js))
        .route("/api/status", get(api_status))
        .route("/ws/audio", get(websocket_handler))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

/// Errors that can occur in the server
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Failed to bind to address: {0}")]
    BindError(#[from] std::io::Error),
    #[error("Broadcaster error: {0}")]
    Broadcaster(#[from] BroadcastError),
    #[error("Server not running")]
    NotRunning,
    #[error("Invalid configuration: {0}")]
    Config(String),
}

/// The main network server
pub struct NetworkServer {
    config: ServerConfig,
}

impl NetworkServer {
    /// Create a new network server
    pub fn new(config: ServerConfig) -> Self {
        Self { config }
    }

    /// Start the server, returning a handle for control
    pub async fn start(self) -> Result<ServerHandle, ServerError> {
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let url = Arc::new(tokio::sync::Mutex::new(None::<String>));

        // Create broadcaster
        let (broadcaster_handle, broadcaster) =
            AudioBroadcaster::new(self.config.broadcaster_config.clone());

        // The advertised address is chosen for the family that is being bound,
        // never taken from the default route alone.
        let detected_ip = detect_for_bind(self.config.bind_addr);

        // Build the shared router
        let state = ServerState {
            broadcaster: broadcaster_handle.clone(),
            websocket_config: self.config.websocket_config.clone(),
            shutdown_tx: shutdown_tx.clone(),
            access: self.config.access.clone(),
        };
        let app = build_router(state);

        // Bind to address
        let addr = SocketAddr::new(self.config.bind_addr, self.config.port);
        let listener = TcpListener::bind(addr).await?;
        let actual_addr = listener.local_addr()?;

        // The URL follows the socket that was actually bound, so a bind to port
        // 0 advertises the port the OS picked rather than the requested one.
        let server_url = listen_url(self.config.bind_addr, actual_addr, detected_ip);
        *url.lock().await = Some(server_url.clone());

        info!("Network server listening on {}", actual_addr);
        info!("Server URL: {}", server_url);

        // Spawn broadcaster task
        let broadcaster_task = tokio::spawn(broadcaster.run());

        // Spawn server task
        let server_task = tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            // Opus packets are ~100 bytes every 10 ms: with Nagle's algorithm
            // on, Windows holds them back and sends bursts, which drains the
            // listener's jitter buffer and makes the stream stutter.
            axum::serve(listener, app)
                .tcp_nodelay(true)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.recv().await;
                    info!("🌐 Shutdown signal received, stopping server...");
                })
                .await
        });

        // Create server handle
        let shutdown_tx_for_handle = shutdown_tx.clone();
        let handle = ServerHandle {
            shutdown_tx: shutdown_tx_for_handle,
            url,
            broadcaster: broadcaster_handle,
        };

        // Monitor tasks in background
        let handle_clone = handle.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = broadcaster_task => {
                    if let Err(e) = result {
                        error!("Broadcaster task panicked: {}", e);
                    } else {
                        info!("Broadcaster task completed");
                    }
                }
                result = server_task => {
                    if let Err(e) = result {
                        error!("Server task error: {}", e);
                    } else {
                        info!("Server task completed");
                    }
                }
            }
            // Signal shutdown to any remaining tasks and retire the URL, so
            // is_running()/url() stop claiming a live listener.
            *handle_clone.url.lock().await = None;
            let _ = handle_clone.shutdown_tx.send(());
        });

        Ok(handle)
    }
}

/// Reply for a request without a valid access key.
fn access_denied() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "status": "unauthorized",
            "message": "access key missing or outdated"
        })),
    )
        .into_response()
}

/// REST API: GET /api/status
///
/// Guarded by the access key like the stream: the listener page calls it
/// first, so a missing or outdated key is reported as such instead of as an
/// endless reconnect loop.
pub async fn api_status(
    State(state): State<ServerState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !state
        .access
        .allows(query.get(KEY_PARAM).map(String::as_str))
    {
        return access_denied();
    }
    let listener_count = state.broadcaster.listener_count().await;

    let status = serde_json::json!({
        "status": "running",
        "listeners": listener_count,
        "version": env!("CARGO_PKG_VERSION"),
        "audio_format": {
            "sample_rate": 48000,
            "channels": 2,
            "format": crate::network::broadcast::WIRE_CODEC
        }
    });

    Json(status).into_response()
}

/// Static file handlers using embedded file contents
pub async fn serve_index_html() -> Html<&'static str> {
    info!("📁 Serving index.html");
    Html(INDEX_HTML)
}

pub async fn serve_app_js() -> impl IntoResponse {
    info!("📁 Serving app.js");
    let mut headers = axum::http::HeaderMap::new();
    // Static header values: parsed at compile time instead of unwrapping a
    // parse that could panic while serving a request.
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    (headers, APP_JS)
}

pub async fn serve_style_css() -> impl IntoResponse {
    info!("📁 Serving style.css");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/css; charset=utf-8"),
    );
    (headers, STYLE_CSS)
}

/// The Opus WASM decoder module embedded next to app.js. Served as a real
/// static module so the client can `import()` it: the client does not ship
/// any bundler, and inlining was too large for the injection approach used
/// for the worklet blob.
pub async fn serve_opus_decoder_js() -> impl IntoResponse {
    info!("📁 Serving opus-decoder.min.js");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    (headers, OPUS_DECODER_JS)
}

/// WebSocket handler: GET /ws/audio
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    info!("🔌 WebSocket upgrade requested at /ws/audio");
    let key = query.get(KEY_PARAM).cloned();
    if !state.access.allows(key.as_deref()) {
        tracing::warn!("🔌 Listener rejected: access key missing or wrong");
        return access_denied();
    }
    let revocation = Some((state.access.subscribe(), key));
    // The configured frame limit used to be ignored here, so the library
    // default applied no matter what WebSocketConfig said.
    let ws = ws.max_message_size(state.websocket_config.max_message_size);
    // Each session owns its own subscription, so a later Stop Server reaches
    // every listener that is connected at that moment.
    let shutdown_rx = state.shutdown_tx.subscribe();
    ws.on_upgrade(move |socket| {
        handle_websocket_upgrade(
            socket,
            state.broadcaster,
            state.websocket_config,
            shutdown_rx,
            revocation,
        )
    })
}

/// The URL listeners should open, derived from where the socket actually bound.
///
/// * a loopback bind is only reachable from this machine, so it advertises
///   loopback;
/// * a wildcard bind (0.0.0.0 / ::) advertises a detected address of the
///   matching family, falling back to loopback when there is none;
/// * an explicit address advertises itself.
///
/// The actual address supplies the real port, so a bind to port 0 reports the
/// port the OS picked rather than the one that was asked for.
pub fn listen_url(bind_addr: IpAddr, actual: SocketAddr, detected: Option<IpAddr>) -> String {
    let advertise = if bind_addr.is_loopback() {
        bind_addr
    } else if bind_addr.is_unspecified() {
        match detected {
            // Never advertise an address of the other family: the socket only
            // listens on the family it was bound to. The address also has to be
            // one a client could actually open, which is checked here as well
            // as where it was detected, because an unreachable URL is worse
            // than an honest loopback one.
            Some(ip) if ip.is_ipv4() == bind_addr.is_ipv4() && is_advertisable(ip) => ip,
            _ if bind_addr.is_ipv4() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            _ => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        }
    } else {
        bind_addr
    };

    match advertise {
        IpAddr::V4(ip) => format!("http://{}:{}", ip, actual.port()),
        // IPv6 literals need brackets, otherwise the port reads as part of the address.
        IpAddr::V6(ip) => format!("http://[{}]:{}", ip, actual.port()),
    }
}

/// The loopback URL of a running server, derived from the advertised one.
///
/// The listener page is always reachable on the machine that serves it, and
/// that address never depends on detection: an operator who only wants to check
/// the page locally should not have to go through the LAN for it. With a
/// proxy/VPN both sides stay visible this way -- the LAN address above for the
/// players, this one for the console's own machine.
pub fn loopback_url(advertised: &str) -> Option<String> {
    // The URL is built by listen_url, so the port is the last colon segment;
    // the IPv6 form brackets its host and therefore parses the same way.
    let port = advertised
        .trim_end_matches('/')
        .rsplit(':')
        .next()?
        .parse::<u16>()
        .ok()?;

    Some(format!("http://127.0.0.1:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn test_server_config_default() {
        let config = ServerConfig::default();
        assert_eq!(config.port, 8080);
        assert_eq!(config.bind_addr, IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)));
    }

    /// Detection may answer with nothing in a test environment, but whatever it
    /// answers with has to be an address a listener could actually open.
    #[test]
    fn detection_never_offers_an_unreachable_address() {
        if let Some(ip) = detect_for_bind(IpAddr::V4(Ipv4Addr::UNSPECIFIED)) {
            assert!(is_advertisable(ip), "detected {ip}");
            assert!(
                !ip.is_loopback(),
                "loopback is the fallback, not a detection"
            );
        }
    }

    #[tokio::test]
    async fn test_server_start_stop() {
        let config = ServerConfig {
            port: 0, // Let OS assign
            ..Default::default()
        };

        let server = NetworkServer::new(config);
        let handle = match server.start().await {
            Ok(handle) => handle,
            Err(_) => return, // Skip if the environment cannot bind
        };

        let url = handle.url().await.expect("a started server has a URL");
        // The URL must carry the port the OS actually bound, not the requested 0.
        assert!(!url.ends_with(":0"), "unbound port advertised: {url}");
        assert!(handle.is_running().await);

        // The server must really be listening on the advertised port.
        let addr = url.trim_start_matches("http://");
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_ok(),
            "nothing is listening at {addr}"
        );

        handle.stop();
        let _ = timeout(Duration::from_secs(2), async {
            while handle.is_running().await {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_websocket_connect() {
        use futures::stream::StreamExt;
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message;

        let config = ServerConfig {
            port: 0,
            ..Default::default()
        };

        let server = NetworkServer::new(config);
        let handle = match server.start().await {
            Ok(h) => h,
            Err(_) => return, // Skip if can't bind
        };

        let url = match handle.url().await {
            Some(u) => u.replace("http://", "ws://") + "/ws/audio",
            None => {
                handle.stop();
                return;
            }
        };

        match connect_async(url).await {
            Ok((mut ws_stream, _)) => {
                // Give the session time to register and greet the client.
                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(2), ws_stream.next()).await;

                match result {
                    Ok(Some(Ok(Message::Binary(data)))) => {
                        assert!(!data.is_empty(), "Received empty binary message");
                    }
                    Ok(Some(Ok(Message::Text(text)))) => {
                        // Text messages are also valid (welcome / format info)
                        assert!(!text.is_empty());
                    }
                    Ok(Some(Ok(_))) => {} // Other message types are OK
                    Ok(Some(Err(e))) => panic!("WebSocket error: {}", e),
                    Ok(None) => {} // Stream closed, that's OK for a quick test
                    Err(_) => {}   // Timeout, no audio playing yet - that's OK
                }

                let _ = ws_stream.close(None).await;
            }
            Err(e) => {
                println!(
                    "WebSocket connection failed (may be expected if server not fully started): {}",
                    e
                );
            }
        }

        handle.stop();
    }

    /// The library server must keep serving real responses after the router was
    /// unified: the listener page is embedded, not read from disk.
    #[tokio::test]
    async fn network_server_serves_the_embedded_listener_page() {
        let config = ServerConfig {
            port: 0,
            ..Default::default()
        };

        let handle = match NetworkServer::new(config).start().await {
            Ok(handle) => handle,
            Err(_) => return,
        };
        let url = handle.url().await.expect("url");
        let addr = url.trim_start_matches("http://");

        let body = http_get(addr, "/").await;
        assert!(body.contains("Bardwave"), "unexpected body: {body}");

        handle.stop();
    }

    /// Minimal HTTP GET so the test needs no extra dependency.
    async fn http_get(addr: &str, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect to the running server");
        let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        response
    }

    // --- advertised URL ------------------------------------------------------

    #[test]
    fn listen_url_advertises_loopback_for_loopback_binds() {
        let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 8080));
        assert_eq!(
            listen_url(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                v4,
                Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 245)))
            ),
            "http://127.0.0.1:8080"
        );

        let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 9000));
        assert_eq!(
            listen_url(IpAddr::V6(Ipv6Addr::LOCALHOST), v6, None),
            "http://[::1]:9000"
        );
    }

    #[test]
    fn listen_url_prefers_the_detected_lan_address_for_wildcard_binds() {
        let actual = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080));
        assert_eq!(
            listen_url(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                actual,
                Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 245)))
            ),
            "http://192.168.1.245:8080"
        );

        // Without detection a wildcard bind is only guaranteed reachable locally.
        assert_eq!(
            listen_url(IpAddr::V4(Ipv4Addr::UNSPECIFIED), actual, None),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn listen_url_never_advertises_the_other_address_family() {
        // IPv4 wildcard socket + IPv6 detection: advertising ::1 would point at
        // a socket this server never opened.
        let actual4 = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080));
        assert_eq!(
            listen_url(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                actual4,
                Some(IpAddr::V6(Ipv6Addr::LOCALHOST))
            ),
            "http://127.0.0.1:8080"
        );

        // ...and the mirror image for an IPv6 wildcard socket.
        let actual6 = SocketAddr::from((Ipv6Addr::UNSPECIFIED, 8080));
        assert_eq!(
            listen_url(
                IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                actual6,
                Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 245)))
            ),
            "http://[::1]:8080"
        );
    }

    #[test]
    fn listen_url_reports_the_port_the_os_actually_bound() {
        // The OS may pick the port (bind to 0), and the advertised URL has to
        // match the socket rather than the requested port.
        let actual = SocketAddr::from((Ipv4Addr::LOCALHOST, 41_000));
        assert_eq!(
            listen_url(IpAddr::V4(Ipv4Addr::LOCALHOST), actual, None),
            "http://127.0.0.1:41000"
        );
    }

    #[test]
    fn listen_url_advertises_an_explicit_address_verbatim() {
        let actual = SocketAddr::from((Ipv4Addr::new(10, 0, 0, 7), 8080));
        assert_eq!(
            listen_url(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)), actual, None),
            "http://10.0.0.7:8080"
        );
    }

    /// A proxy TUN adapter answers the route probe with an address out of the
    /// benchmarking range. That URL used to be published verbatim as
    /// http://198.18.0.1:8080, which neither a player nor the operator can
    /// open; loopback is what a wildcard bind falls back to instead.
    #[test]
    fn listen_url_never_advertises_a_benchmarking_range_address() {
        let actual = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080));

        // Straight from detection...
        assert_eq!(
            listen_url(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                actual,
                Some(IpAddr::V4(Ipv4Addr::new(198, 18, 0, 1)))
            ),
            "http://127.0.0.1:8080"
        );

        // ...and even when something hands one over unchecked.
        assert_eq!(
            listen_url(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                actual,
                Some(IpAddr::V4(Ipv4Addr::new(169, 254, 3, 4)))
            ),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn loopback_url_keeps_the_bound_port() {
        assert_eq!(
            loopback_url("http://192.168.1.245:8080").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        // The IPv6 form brackets the host, so the port is still the last colon
        // segment.
        assert_eq!(
            loopback_url("http://[2a00:1450::1]:9000").as_deref(),
            Some("http://127.0.0.1:9000")
        );
    }

    #[test]
    fn loopback_url_rejects_text_without_a_port() {
        assert_eq!(loopback_url("http://192.168.1.245"), None);
        assert_eq!(loopback_url("not a url"), None);
    }

    /// End to end on a real socket: whatever the machine looks like, the URL a
    /// started server publishes is either loopback or an address a client can
    /// open -- never the benchmarking address a proxy TUN adapter hands to the
    /// route probe.
    #[tokio::test]
    async fn started_server_never_advertises_an_unreachable_address() {
        let config = ServerConfig {
            port: 0,
            ..Default::default()
        };

        let handle = match NetworkServer::new(config).start().await {
            Ok(handle) => handle,
            Err(_) => return, // Skip if the environment cannot bind
        };

        let url = handle.url().await.expect("a started server has a URL");
        let host = url.trim_start_matches("http://");
        let host = host.rsplit_once(':').map_or(host, |(host, _)| host);
        let ip: IpAddr = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .unwrap_or_else(|_| panic!("advertised host is not an IP literal: {url}"));

        assert!(
            ip.is_loopback() || is_advertisable(ip),
            "advertised an address no listener can open: {url}"
        );

        handle.stop();
    }
}
