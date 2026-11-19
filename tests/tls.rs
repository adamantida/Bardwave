//! Optional HTTPS (`server.tls`) end to end.
//!
//! Every server binds port 0 on loopback and keeps its certificate in a
//! temporary directory, so nothing here touches the real data directory or a
//! shared port. The clients trust the generated certificate explicitly (it is
//! their only trust anchor), the way a phone user would after comparing the
//! fingerprint.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use soundshared::audio::director::mock::MockPlayback;
use soundshared::audio::SoundId;
use soundshared::config::{LibraryConfig, SoundConfigEntry};
use soundshared::network::broadcast::{AudioBroadcaster, BroadcasterConfig};
use soundshared::network::control::{Board, ControlCommand, ControlContext, ControlFeed};
use soundshared::network::server::{
    build_router_with_control, listen_url_with_scheme, loopback_url, NetworkServer, ServerConfig,
    ServerError, ServerState,
};
use soundshared::network::tls::{self, SanSet, TlsSettings};
use soundshared::network::websocket::WebSocketConfig;
use soundshared::network::{AccessGuard, GmAuth, GmGuard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const KEY: &str = "player-key";
const GM: &str = "gm-secret-token";

/// A unique directory removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bardwave-tls-it-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tls_settings(dir: &TempDir) -> TlsSettings {
    TlsSettings {
        enabled: true,
        dir: Some(dir.path().join("tls")),
        ..Default::default()
    }
}

/// The generated certificate, as a client that trusts exactly it.
fn trusted_cert(settings: &TlsSettings) -> CertificateDer<'static> {
    CertificateDer::from_pem_file(settings.cert_dir().join(tls::CERT_FILE)).expect("certificate")
}

fn connector(cert: CertificateDer<'static>) -> tokio_rustls::TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).expect("trust anchor");
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(config))
}

async fn tls_stream(
    addr: SocketAddr,
    cert: CertificateDer<'static>,
) -> std::io::Result<TlsStream<tokio::net::TcpStream>> {
    let tcp = tokio::net::TcpStream::connect(addr).await?;
    connector(cert)
        .connect(ServerName::IpAddress(addr.ip().into()), tcp)
        .await
}

struct Reply {
    status: u16,
    body: String,
}

async fn https_get(addr: SocketAddr, cert: &CertificateDer<'static>, path: &str) -> Reply {
    let mut stream = tls_stream(addr, cert.clone()).await.expect("TLS connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    // A missing close_notify at the very end is not a failure of the reply.
    let _ = stream.read_to_end(&mut raw).await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status line in {text:?}"));
    Reply {
        status,
        body: body.to_string(),
    }
}

fn address_of(url: &str) -> SocketAddr {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .parse()
        .unwrap_or_else(|e| panic!("bad server url {url:?}: {e}"))
}

async fn start_tls_server(dir: &TempDir) -> soundshared::network::server::ServerHandle {
    let config = ServerConfig {
        bind_addr: "127.0.0.1".parse().unwrap(),
        port: 0,
        upnp: false,
        access: AccessGuard::new(Some(KEY.into())),
        tls: tls_settings(dir),
        ..Default::default()
    };
    NetworkServer::new(config).start().await.expect("start")
}

// ---------------------------------------------------------------- HTTPS -----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_answers_the_status_api_and_keeps_its_key() {
    let dir = TempDir::new("status");
    let handle = start_tls_server(&dir).await;
    let url = handle.url().await.expect("url");
    assert!(url.starts_with("https://127.0.0.1:"), "{url}");
    let addr = address_of(&url);
    let cert = trusted_cert(&tls_settings(&dir));

    let ok = https_get(addr, &cert, &format!("/api/status?k={KEY}")).await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    let json: serde_json::Value = serde_json::from_str(&ok.body).expect("json");
    assert_eq!(json["status"], "running");

    let denied = https_get(addr, &cert, "/api/status").await;
    assert_eq!(denied.status, 401);

    // The listener page is served too.
    let page = https_get(addr, &cert, "/").await;
    assert_eq!(page.status, 200);

    handle.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_http_is_not_served_on_the_https_port() {
    let dir = TempDir::new("plain");
    let handle = start_tls_server(&dir).await;
    let addr = address_of(&handle.url().await.expect("url"));

    let mut tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
    tcp.write_all(b"GET /api/status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), tcp.read_to_end(&mut raw)).await;
    let text = String::from_utf8_lossy(&raw);
    assert!(
        !text.contains("200 OK"),
        "plain HTTP must not be answered: {text:?}"
    );
    assert!(!text.contains("running"), "{text:?}");

    handle.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_does_not_trust_the_certificate_is_refused() {
    let dir = TempDir::new("untrusted");
    let handle = start_tls_server(&dir).await;
    let addr = address_of(&handle.url().await.expect("url"));

    // Trust some other self-signed certificate: the handshake must fail.
    let other = TempDir::new("other");
    let other_settings = tls_settings(&other);
    tls::load_or_create(&other_settings, &SanSet::detect()).expect("other cert");
    let result = tls_stream(addr, trusted_cert(&other_settings)).await;
    assert!(
        result.is_err(),
        "an unknown certificate must not be trusted"
    );

    handle.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_self_signed_certificate_is_created_once_and_reused() {
    let dir = TempDir::new("reuse");
    let settings = tls_settings(&dir);
    assert!(!settings.cert_dir().exists());

    let first = start_tls_server(&dir).await;
    let fp_first = tls::stored_info(&settings.cert_dir())
        .expect("stored")
        .fingerprint_sha256;
    // The running server reports the same certificate to the GUI.
    assert!(tls::active_info().is_some());
    first.stop();

    let second = start_tls_server(&dir).await;
    let fp_second = tls::stored_info(&settings.cert_dir())
        .expect("stored")
        .fingerprint_sha256;
    assert_eq!(fp_first, fp_second, "the certificate must be reused");
    assert_eq!(fp_first.split(':').count(), 32, "{fp_first}");
    second.stop();

    // ...and the fingerprint is that of the file a client would trust.
    let cert = trusted_cert(&settings);
    assert_eq!(tls::fingerprint_sha256(&cert), fp_first);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_private_key_is_readable_by_its_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("keymode");
    let handle = start_tls_server(&dir).await;
    let key = tls_settings(&dir).cert_dir().join(tls::KEY_FILE);
    let mode = std::fs::metadata(&key).expect("key").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");
    handle.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_certificate_is_a_clear_error() {
    let dir = TempDir::new("missing");
    let config = ServerConfig {
        bind_addr: "127.0.0.1".parse().unwrap(),
        port: 0,
        upnp: false,
        tls: TlsSettings {
            enabled: true,
            cert: Some(dir.path().join("no-such-cert.pem")),
            key: Some(dir.path().join("no-such-key.pem")),
            dir: Some(dir.path().join("tls")),
        },
        ..Default::default()
    };
    let error = match NetworkServer::new(config).start().await {
        Ok(_) => panic!("must not start"),
        Err(e) => e,
    };
    assert!(matches!(error, ServerError::Config(_)), "{error}");
    assert!(error.to_string().contains("no-such-cert.pem"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_http_mode_is_unchanged() {
    let config = ServerConfig {
        bind_addr: "127.0.0.1".parse().unwrap(),
        port: 0,
        upnp: false,
        ..Default::default()
    };
    assert!(!config.tls.enabled, "HTTPS is off by default");
    let handle = NetworkServer::new(config).start().await.expect("start");
    let url = handle.url().await.expect("url");
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");

    let mut tcp = tokio::net::TcpStream::connect(address_of(&url))
        .await
        .expect("connect");
    tcp.write_all(b"GET /api/status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut raw = Vec::new();
    tcp.read_to_end(&mut raw).await.expect("read");
    assert!(String::from_utf8_lossy(&raw).contains("200 OK"));
    handle.stop();
}

#[test]
fn links_follow_the_scheme() {
    let bind = "0.0.0.0".parse().unwrap();
    let actual: SocketAddr = "0.0.0.0:8080".parse().unwrap();
    let detected = Some("192.168.1.20".parse().unwrap());
    assert_eq!(
        listen_url_with_scheme(bind, actual, detected, true),
        "https://192.168.1.20:8080"
    );
    assert_eq!(
        listen_url_with_scheme(bind, actual, detected, false),
        "http://192.168.1.20:8080"
    );
    assert_eq!(
        loopback_url("https://192.168.1.20:8080").as_deref(),
        Some("https://127.0.0.1:8080")
    );
    assert_eq!(
        loopback_url("http://192.168.1.20:8080").as_deref(),
        Some("http://127.0.0.1:8080")
    );
}

// ------------------------------------------------------------------ wss -----

struct ControlHarness {
    addr: SocketAddr,
    cert: CertificateDer<'static>,
    remote: Arc<AtomicBool>,
    shutdown: tokio::sync::broadcast::Sender<()>,
    stop: Arc<AtomicBool>,
    pump: Option<std::thread::JoinHandle<()>>,
    _dir: TempDir,
}

impl Drop for ControlHarness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.shutdown.send(());
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
    }
}

/// The router with the game-master panel, served over TLS by `tls::serve`.
async fn start_control_over_tls() -> ControlHarness {
    let dir = TempDir::new("wss");
    let settings = tls_settings(&dir);
    let acceptor = tls::prepare(&settings).expect("acceptor");
    let cert = trusted_cert(&settings);

    let library = LibraryConfig {
        sounds: vec![SoundConfigEntry {
            id: "wind".into(),
            path: "/fake/wind.wav".into(),
            name: "Wind".into(),
            volume: 0.8,
            ..Default::default()
        }],
        ..Default::default()
    };
    let pb = Arc::new(MockPlayback::new());
    let ids: Vec<SoundId> = library.sounds.iter().map(|_| pb.add_sound()).collect();
    let loaded: Vec<(usize, SoundId)> = ids.iter().copied().enumerate().collect();
    let feed = ControlFeed::new();
    let (tx, rx) = crossbeam_channel::unbounded::<ControlCommand>();
    let stop = Arc::new(AtomicBool::new(false));
    let pump = {
        let (pb, feed, stop) = (pb.clone(), feed.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut board = Board::assemble(&library, &loaded);
            board.attach_feed(&*pb, feed);
            while !stop.load(Ordering::Relaxed) {
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(command) => {
                        let result = board.handle(&*pb, command.action);
                        let _ = command.reply.send(result);
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
                board.tick(&*pb);
            }
        })
    };

    let remote = Arc::new(AtomicBool::new(true));
    let auth = GmAuth::new(GmGuard::new(Some(GM.to_string()))).with_locality({
        let remote = remote.clone();
        Arc::new(move |peer: &SocketAddr| {
            !remote.load(Ordering::Relaxed) && peer.ip().is_loopback()
        })
    });
    let ctx = ControlContext::new(tx, auth, feed);

    let (broadcaster, task) = AudioBroadcaster::new(BroadcasterConfig::default());
    tokio::spawn(task.run());
    let (shutdown, _) = tokio::sync::broadcast::channel(1);
    let state = ServerState {
        broadcaster,
        websocket_config: WebSocketConfig::default(),
        shutdown_tx: shutdown.clone(),
        access: AccessGuard::new(Some(KEY.into())),
    };
    let app = build_router_with_control(state, Some(ctx));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mut stop_rx = shutdown.subscribe();
    tokio::spawn(tls::serve(listener, app, acceptor, async move {
        let _ = stop_rx.recv().await;
    }));

    ControlHarness {
        addr,
        cert,
        remote,
        shutdown,
        stop,
        pump: Some(pump),
        _dir: dir,
    }
}

type Wss = tokio_tungstenite::WebSocketStream<TlsStream<tokio::net::TcpStream>>;

async fn wss_connect(
    h: &ControlHarness,
    query: &str,
) -> Result<Wss, Box<tokio_tungstenite::tungstenite::Error>> {
    let request = format!("wss://{}/ws/control{query}", h.addr)
        .into_client_request()
        .expect("request");
    let stream = tls_stream(h.addr, h.cert.clone()).await.expect("TLS");
    tokio_tungstenite::client_async(request, stream)
        .await
        .map(|(ws, _)| ws)
        .map_err(Box::new)
}

fn status_of(error: &tokio_tungstenite::tungstenite::Error) -> Option<u16> {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => Some(response.status().as_u16()),
        _ => None,
    }
}

async fn first_snapshot(ws: &mut Wss) -> serde_json::Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("no message within 5 s")
            .expect("socket closed")
            .expect("frame");
        if let Message::Text(text) = message {
            let json: serde_json::Value = serde_json::from_str(&text).expect("json");
            if json["type"] == "snapshot" {
                return json;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_control_socket_works_over_wss_with_the_token() {
    let h = start_control_over_tls().await;
    let mut ws = wss_connect(&h, &format!("?g={GM}"))
        .await
        .expect("wss with the token");
    let snapshot = first_snapshot(&mut ws).await;
    assert_eq!(
        snapshot["state"]["sounds"].as_array().map(Vec::len),
        Some(1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_control_socket_refuses_a_remote_client_without_the_token() {
    let h = start_control_over_tls().await;
    for query in ["", "?g=", "?g=wrong-token", "?g=player-key"] {
        let error = wss_connect(&h, query).await.expect_err("must be refused");
        assert_eq!(status_of(&error), Some(401), "query {query:?}");
    }
    // The REST side of the panel is closed to it as well.
    let reply = https_get(h.addr, &h.cert, "/api/control/state").await;
    assert_eq!(reply.status, 401);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_peer_address_reaches_the_panel_through_tls() {
    // Locality is decided from the peer address, so a loopback peer needs no
    // token once the server treats this computer as local: that only works
    // when the TLS accept loop passes the real peer on.
    let h = start_control_over_tls().await;
    h.remote.store(false, Ordering::Relaxed);
    let mut ws = wss_connect(&h, "").await.expect("local peer, no token");
    first_snapshot(&mut ws).await;
}

// --------------------------------------------------------------- config -----

#[test]
fn old_configs_load_with_https_off_and_new_fields_round_trip() {
    let old: soundshared::config::Config =
        toml::from_str("[server]\nport = 9000\nhost = \"0.0.0.0\"\n").expect("old config");
    assert!(!old.server.tls);
    assert_eq!(old.server.tls_cert, None);
    assert_eq!(old.server.tls_key, None);
    assert!(!TlsSettings::from_config(&old.server).enabled);

    let new: soundshared::config::Config = toml::from_str(
        "[server]\ntls = true\ntls_cert = \"/etc/ssl/bard.pem\"\ntls_key = \"/etc/ssl/bard.key\"\n",
    )
    .expect("new config");
    let settings = TlsSettings::from_config(&new.server);
    assert!(settings.enabled);
    assert_eq!(settings.cert, Some(PathBuf::from("/etc/ssl/bard.pem")));
    assert_eq!(settings.key, Some(PathBuf::from("/etc/ssl/bard.key")));

    let text = toml::to_string(&new).expect("serialize");
    let again: soundshared::config::Config = toml::from_str(&text).expect("reload");
    assert!(again.server.tls);
    assert_eq!(again.server.tls_key.as_deref(), Some("/etc/ssl/bard.key"));
}
