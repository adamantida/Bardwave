//! The remote game-master panel end to end: who may open `/api/control/*` and
//! `/ws/control`, the attempt limiter, and the live state on `/ws/control`.
//!
//! Every server binds port 0 on loopback. A remote client is simulated by
//! replacing the server's "is this peer local" test (the sockets themselves are
//! loopback either way), and the "engine thread" is a `Board` on a mock
//! playback, so nothing here needs a sound card, a shared port or a real
//! config.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use soundshared::audio::director::mock::MockPlayback;
use soundshared::audio::{Playback, SoundId};
use soundshared::config::{
    LibraryConfig, SceneConfig, SceneItemConfig, SceneStart, SoundConfigEntry,
};
use soundshared::network::broadcast::{AudioBroadcaster, BroadcasterConfig};
use soundshared::network::control::{
    Board, BoardState, ControlCommand, ControlContext, ControlFeed,
};
use soundshared::network::server::{build_router_with_control, ServerState};
use soundshared::network::websocket::WebSocketConfig;
use soundshared::network::{AccessGuard, GmAuth, GmGuard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const GM: &str = "gm-secret-token";
const PLAYER: &str = "player-key";

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn entry(id: &str, name: &str) -> SoundConfigEntry {
    SoundConfigEntry {
        id: id.into(),
        path: format!("/fake/{id}.wav"),
        name: name.into(),
        volume: 0.8,
        ..Default::default()
    }
}

fn library() -> LibraryConfig {
    LibraryConfig {
        sounds: vec![entry("wind", "Wind"), entry("gong", "Gong")],
        scenes: vec![SceneConfig {
            id: "storm".into(),
            name: "Storm".into(),
            items: vec![SceneItemConfig {
                sound: "wind".into(),
                volume: 0.7,
                looped: true,
                start: SceneStart::OnEnter,
                delay_ms: 0,
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

struct Harness {
    addr: SocketAddr,
    /// While true the server treats every peer as a remote one.
    remote: Arc<AtomicBool>,
    pb: Arc<MockPlayback>,
    gm: GmGuard,
    shutdown: tokio::sync::broadcast::Sender<()>,
    stop: Arc<AtomicBool>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.shutdown.send(());
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
    }
}

struct Options {
    gm_token: Option<&'static str>,
    remote: bool,
    max_clients: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            gm_token: Some(GM),
            remote: true,
            max_clients: None,
        }
    }
}

async fn start(options: Options) -> Harness {
    let library = library();
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

    let remote = Arc::new(AtomicBool::new(options.remote));
    let gm = GmGuard::new(options.gm_token.map(str::to_string));
    let auth = GmAuth::new(gm.clone()).with_locality({
        let remote = remote.clone();
        Arc::new(move |peer: &SocketAddr| {
            !remote.load(Ordering::Relaxed) && peer.ip().is_loopback()
        })
    });
    let mut ctx = ControlContext::new(tx, auth, feed);
    if let Some(max) = options.max_clients {
        ctx = ctx.with_max_clients(max);
    }

    let (broadcaster, task) = AudioBroadcaster::new(BroadcasterConfig::default());
    tokio::spawn(task.run());
    let (shutdown, _) = tokio::sync::broadcast::channel(1);
    let state = ServerState {
        broadcaster,
        websocket_config: WebSocketConfig::default(),
        shutdown_tx: shutdown.clone(),
        access: AccessGuard::new(Some(PLAYER.into())),
    };
    let app = build_router_with_control(state, Some(ctx));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    Harness {
        addr,
        remote,
        pb,
        gm,
        shutdown,
        stop,
        pump: Some(pump),
    }
}

struct Reply {
    status: u16,
    headers: String,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<String> {
        let prefix = format!("{}:", name.to_ascii_lowercase());
        self.headers.lines().find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with(&prefix)
                .then(|| line[prefix.len()..].trim().to_string())
        })
    }
}

async fn http(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)]) -> Reply {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: 0\r\n"
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status line");
    Reply {
        status,
        headers: head.to_string(),
        body: body.to_string(),
    }
}

const HDR: (&str, &str) = ("X-Bardwave-Control", "1");

async fn ws_connect(
    addr: SocketAddr,
    query: &str,
    headers: &[(&str, &str)],
) -> Result<Ws, Box<tokio_tungstenite::tungstenite::Error>> {
    let mut request = format!("ws://{addr}/ws/control{query}")
        .into_client_request()
        .expect("request");
    for (name, value) in headers {
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    connect_async(request)
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

/// One JSON message from the socket (pings and the like skipped).
async fn next_json(ws: &mut Ws) -> Option<serde_json::Value> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("no message within 5 s")?;
        match message {
            Ok(Message::Text(text)) => return Some(serde_json::from_str(&text).expect("json")),
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(other) => panic!("unexpected frame {other:?}"),
        }
    }
}

/// The next `snapshot`, with its revision and state.
async fn next_snapshot(ws: &mut Ws) -> (u64, BoardState) {
    loop {
        let message = next_json(ws).await.expect("socket closed");
        if message["type"] == "snapshot" {
            let state = serde_json::from_value(message["state"].clone()).expect("state");
            return (message["rev"].as_u64().expect("rev"), state);
        }
    }
}

/// Snapshots until one satisfies `done`.
async fn snapshot_where(ws: &mut Ws, done: impl Fn(&BoardState) -> bool) -> (u64, BoardState) {
    for _ in 0..20 {
        let (rev, state) = next_snapshot(ws).await;
        if done(&state) {
            return (rev, state);
        }
    }
    panic!("the wanted state never arrived");
}

fn send_text(text: &str) -> Message {
    Message::Text(text.to_string())
}

// ---------------------------------------------------------------- REST ------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_players_key_does_not_open_the_control_panel() {
    let h = start(Options::default()).await;
    let bearer = format!("Bearer {PLAYER}");
    let attempts: [(&str, &[(&str, &str)]); 4] = [
        ("/api/control/state", &[("Authorization", &bearer), HDR]),
        ("/api/control/state?k=player-key", &[HDR]),
        ("/api/control/state?g=player-key", &[HDR]),
        ("/control?k=player-key", &[]),
    ];
    for (path, headers) in attempts {
        let reply = http(h.addr, "GET", path, headers).await;
        assert_eq!(reply.status, 401, "{path}: {}", reply.body);
    }
    let reply = http(
        h.addr,
        "POST",
        "/api/control/play?id=0&k=player-key",
        &[("Authorization", &bearer), HDR],
    )
    .await;
    assert_eq!(reply.status, 401);
    assert!(h.pb.playing_sounds().is_empty(), "nothing may have played");

    // ...nor does the listener key open the WebSocket.
    let error = ws_connect(
        h.addr,
        &format!("?k={PLAYER}"),
        &[("Authorization", &bearer)],
    )
    .await
    .expect_err("handshake must fail");
    assert_eq!(status_of(&error), Some(401));

    // The listener side is untouched: its status API still takes its own key.
    let reply = http(h.addr, "GET", &format!("/api/status?k={PLAYER}"), &[]).await;
    assert_eq!(reply.status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gm_token_opens_the_panel_in_header_and_query() {
    let h = start(Options::default()).await;
    let bearer = format!("Bearer {GM}");

    let reply = http(
        h.addr,
        "GET",
        "/api/control/state",
        &[("Authorization", &bearer)],
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let state: BoardState = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(state.sounds.len(), 2);
    assert_eq!(state.scenes.len(), 1);

    // The token proves intent: no X-Bardwave-Control header needed.
    let reply = http(
        h.addr,
        "POST",
        "/api/control/play?id=0",
        &[("Authorization", &bearer)],
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let state: BoardState = serde_json::from_str(&reply.body).unwrap();
    assert!(state.sounds[0].playing);

    // The query form (links, QR codes), percent-encoding included.
    let reply = http(
        h.addr,
        "POST",
        &format!("/api/control/stop?id=0&g={GM}"),
        &[],
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let encoded = GM.replace('-', "%2D");
    let reply = http(
        h.addr,
        "GET",
        &format!("/api/control/scenes?g={encoded}"),
        &[],
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);

    // New actions of the remote panel.
    let reply = http(
        h.addr,
        "POST",
        "/api/control/stinger?id=1",
        &[("Authorization", &bearer)],
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let reply = http(
        h.addr,
        "POST",
        "/api/control/duck?on=true",
        &[("Authorization", &bearer)],
    )
    .await;
    let state: BoardState = serde_json::from_str(&reply.body).unwrap();
    assert!(state.ducked);

    // The page is served, and keeps the token out of caches and Referer.
    let reply = http(h.addr, "GET", &format!("/control?g={GM}"), &[]).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("cache-control").as_deref(), Some("no-store"));
    assert_eq!(
        reply.header("referrer-policy").as_deref(),
        Some("no-referrer")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_remote_client_without_a_token_gets_401_and_a_local_one_works() {
    let h = start(Options::default()).await;

    // Remote, no token / wrong token: the same answer.
    let none = http(h.addr, "GET", "/api/control/state", &[HDR]).await;
    let wrong = http(
        h.addr,
        "GET",
        "/api/control/state",
        &[HDR, ("Authorization", "Bearer nope")],
    )
    .await;
    assert_eq!(none.status, 401);
    assert_eq!(wrong.status, 401);
    assert_eq!(none.body, wrong.body, "must not reveal which one it was");
    let reply = http(h.addr, "GET", "/control", &[]).await;
    assert_eq!(reply.status, 401);

    // The same machine, as a local peer: no token needed...
    h.remote.store(false, Ordering::Relaxed);
    let reply = http(h.addr, "GET", "/api/control/state", &[HDR]).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let reply = http(h.addr, "POST", "/api/control/play?id=0", &[HDR]).await;
    assert_eq!(reply.status, 200);
    assert!(h.pb.playing_sounds().len() == 1);
    let reply = http(h.addr, "GET", "/control", &[]).await;
    assert_eq!(reply.status, 200);
    // ...but a local action still needs the CSRF header.
    let reply = http(h.addr, "POST", "/api/control/stop?id=0", &[]).await;
    assert_eq!(reply.status, 403);
    assert!(h.pb.playing_sounds().len() == 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxied_loopback_request_is_not_local() {
    // The peer is loopback, but a relay on this PC says it forwards someone.
    let h = start(Options {
        remote: false,
        ..Default::default()
    })
    .await;
    let reply = http(h.addr, "GET", "/api/control/state", &[HDR]).await;
    assert_eq!(reply.status, 200);
    for header in [
        "X-Forwarded-For",
        "X-Real-IP",
        "Forwarded",
        "CF-Connecting-IP",
        "Via",
        "X-Forwarded-Port",
    ] {
        let reply = http(
            h.addr,
            "GET",
            "/api/control/state",
            &[HDR, (header, "127.0.0.1")],
        )
        .await;
        assert_eq!(reply.status, 401, "{header}");
    }
    // The relayed client may still use the token.
    let reply = http(
        h.addr,
        "GET",
        &format!("/api/control/state?g={GM}"),
        &[("X-Forwarded-For", "203.0.113.5")],
    )
    .await;
    assert_eq!(reply.status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebound_host_name_is_not_local() {
    // DNS rebinding: the peer is loopback, but the page's own (attacker) name
    // is in Host and Origin, so the Origin == Host check alone would pass.
    let h = start(Options {
        remote: false,
        ..Default::default()
    })
    .await;
    let port = h.addr.port();
    let raw = |host: String, path: String| async move {
        let mut stream = tokio::net::TcpStream::connect(h.addr)
            .await
            .expect("connect");
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\n\
             X-Bardwave-Control: 1\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.expect("read");
        let text = String::from_utf8_lossy(&buf).into_owned();
        text.split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .expect("status line")
    };
    let state = "/api/control/state".to_string();
    assert_eq!(
        raw(format!("evil.example:{port}"), state.clone()).await,
        401
    );
    assert_eq!(raw(format!("localhost:{port}"), state.clone()).await, 200);
    assert_eq!(raw(format!("127.0.0.1:{port}"), state).await, 200);

    let error = ws_connect(
        h.addr,
        "",
        &[
            ("Host", &format!("evil.example:{port}")),
            ("Origin", &format!("http://evil.example:{port}")),
        ],
    )
    .await;
    // Either the handshake is refused (401) or it cannot be built at all.
    if let Err(error) = error {
        assert_eq!(status_of(&error), Some(401));
    } else {
        panic!("rebound host opened the control socket");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_gm_token_forbids_remote_access() {
    for token in [Some(""), None] {
        let h = start(Options {
            gm_token: token,
            ..Default::default()
        })
        .await;
        for (path, headers) in [
            ("/api/control/state", vec![HDR]),
            ("/api/control/state?g=", vec![HDR]),
            (
                "/api/control/state",
                vec![HDR, ("Authorization", "Bearer ")],
            ),
            (
                "/api/control/state?g=",
                vec![HDR, ("Authorization", "Bearer")],
            ),
            // The listener key is not a way in either.
            ("/api/control/state?g=player-key", vec![HDR]),
        ] {
            let reply = http(h.addr, "GET", path, &headers).await;
            assert_eq!(reply.status, 401, "token {token:?}, {path}");
        }
        let error = ws_connect(h.addr, "?g=", &[]).await.expect_err("refused");
        assert_eq!(status_of(&error), Some(401));

        // This computer keeps working.
        h.remote.store(false, Ordering::Relaxed);
        let reply = http(h.addr, "GET", "/api/control/state", &[HDR]).await;
        assert_eq!(reply.status, 200);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_wrong_tokens_are_rate_limited() {
    let h = start(Options::default()).await;
    let mut statuses = Vec::new();
    for _ in 0..10 {
        let reply = http(
            h.addr,
            "GET",
            "/api/control/state",
            &[("Authorization", "Bearer wrong")],
        )
        .await;
        statuses.push(reply.status);
    }
    assert!(statuses.iter().all(|s| *s == 401), "{statuses:?}");

    // The 11th is refused outright, even with the right token.
    let reply = http(
        h.addr,
        "GET",
        "/api/control/state",
        &[("Authorization", &format!("Bearer {GM}"))],
    )
    .await;
    assert_eq!(reply.status, 429, "{}", reply.body);
    let wait: u64 = reply
        .header("retry-after")
        .expect("Retry-After")
        .parse()
        .expect("seconds");
    assert!((1..=60).contains(&wait), "{wait}");
    // The WebSocket door is locked as well.
    let error = ws_connect(h.addr, &format!("?g={GM}"), &[])
        .await
        .expect_err("locked out");
    assert_eq!(status_of(&error), Some(429));

    // This computer is not subject to it.
    h.remote.store(false, Ordering::Relaxed);
    let reply = http(h.addr, "GET", "/api/control/state", &[HDR]).await;
    assert_eq!(reply.status, 200);
}

// ------------------------------------------------------------ WebSocket -----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_see_the_same_state() {
    let h = start(Options::default()).await;
    let query = format!("?g={GM}");
    let mut a = ws_connect(h.addr, &query, &[]).await.expect("client a");
    let mut b = ws_connect(h.addr, &query, &[]).await.expect("client b");

    // The first message is a full snapshot.
    let (rev_a, state_a) = next_snapshot(&mut a).await;
    let (rev_b, state_b) = next_snapshot(&mut b).await;
    assert_eq!(state_a.sounds.len(), 2);
    assert_eq!(state_a.scenes.len(), 1);
    assert!(state_a.sounds.iter().all(|s| !s.playing));
    assert_eq!((rev_a, &state_a), (rev_b, &state_b));

    // A presses a scene; both see it, and the same revision of it.
    a.send(send_text(r#"{"cmd":"scene","id":0,"req":"s1"}"#))
        .await
        .unwrap();
    let mut acked = false;
    let (rev_a2, state_a2) = loop {
        let message = next_json(&mut a).await.expect("open");
        if message["type"] == "ack" {
            assert_eq!(message["req"], "s1");
            assert_eq!(message["ok"], true);
            acked = true;
        } else if message["type"] == "snapshot" {
            let state: BoardState = serde_json::from_value(message["state"].clone()).unwrap();
            if state.active_scene == Some(0) {
                break (message["rev"].as_u64().unwrap(), state);
            }
        }
    };
    if !acked {
        loop {
            let message = next_json(&mut a).await.expect("open");
            if message["type"] == "ack" {
                break;
            }
        }
    }
    let (rev_b2, state_b2) = snapshot_where(&mut b, |s| s.active_scene == Some(0)).await;
    assert_eq!((rev_a2, &state_a2), (rev_b2, &state_b2));
    assert!(state_b2.sounds[0].playing && !state_b2.sounds[1].playing);
    assert!(rev_a2 > rev_a);

    // B moves a slider; A follows.
    b.send(send_text(r#"{"cmd":"volume","id":1,"value":0.25}"#))
        .await
        .unwrap();
    let (_, seen_by_a) = snapshot_where(&mut a, |s| (s.sounds[1].volume - 0.25).abs() < 1e-6).await;
    assert_eq!(seen_by_a.active_scene, Some(0));

    // A REST call from a third party shows up on the sockets too.
    let reply = http(
        h.addr,
        "POST",
        "/api/control/master?value=0.5",
        &[("Authorization", &format!("Bearer {GM}"))],
    )
    .await;
    assert_eq!(reply.status, 200);
    snapshot_where(&mut a, |s| (s.master_volume - 0.5).abs() < 1e-6).await;
    snapshot_where(&mut b, |s| (s.master_volume - 0.5).abs() < 1e-6).await;

    // Stinger, duck and stop all through the socket.
    a.send(send_text(r#"{"cmd":"stinger","id":1}"#))
        .await
        .unwrap();
    snapshot_where(&mut b, |s| s.sounds[1].playing).await;
    a.send(send_text(r#"{"cmd":"duck","on":true}"#))
        .await
        .unwrap();
    snapshot_where(&mut b, |s| s.ducked).await;
    a.send(send_text(r#"{"cmd":"stop_all","fade_ms":100}"#))
        .await
        .unwrap();
    snapshot_where(&mut b, |s| s.sounds.iter().all(|x| !x.playing)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn garbage_messages_do_not_take_the_server_down() {
    let h = start(Options::default()).await;
    let query = format!("?g={GM}");
    let mut ws = ws_connect(h.addr, &query, &[]).await.expect("connect");
    next_snapshot(&mut ws).await;

    // Text that is not a command: each one is answered, the socket stays up.
    let garbage = [
        "",
        "not json at all",
        "null",
        "[]",
        "42",
        r#"{"cmd":42}"#,
        r#"{"cmd":"launch_missiles"}"#,
        r#"{"cmd":"play"}"#,
        r#"{"cmd":"play","id":-5}"#,
        r#"{"cmd":"play","id":99999999999999999999}"#,
        r#"{"cmd":"volume","id":0,"value":"loud"}"#,
        r#"{"cmd":"play","id":999}"#,
        r#"{"cmd":"scene","id":999}"#,
        r#"{"cmd":"stinger","id":999,"req":{"nested":"object"}}"#,
        "{\"cmd\":\"\u{0}\"}",
    ];
    for text in garbage {
        ws.send(send_text(text)).await.unwrap();
        let reply = loop {
            let message = next_json(&mut ws).await.expect("still open after garbage");
            if message["type"] != "snapshot" {
                break message;
            }
        };
        assert_eq!(reply["type"], "error", "{text:?} -> {reply}");
    }
    ws.send(Message::Binary(vec![0, 1, 2, 3])).await.unwrap();
    let reply = loop {
        let message = next_json(&mut ws).await.expect("open after binary");
        if message["type"] != "snapshot" {
            break message;
        }
    };
    assert_eq!(reply["type"], "error");

    // The connection still works: ping/pong and a real command.
    ws.send(send_text(r#"{"cmd":"ping","req":7}"#))
        .await
        .unwrap();
    loop {
        let message = next_json(&mut ws).await.expect("open");
        if message["type"] == "pong" {
            assert_eq!(message["req"], 7);
            break;
        }
    }
    ws.send(send_text(r#"{"cmd":"play","id":0}"#))
        .await
        .unwrap();
    snapshot_where(&mut ws, |s| s.sounds[0].playing).await;

    // An oversized message ends that connection, not the server.
    let mut big = ws_connect(h.addr, &query, &[])
        .await
        .expect("second client");
    next_snapshot(&mut big).await;
    let _ = big.send(send_text(&"a".repeat(64 * 1024))).await;
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(message) = big.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                return true;
            }
        }
        true
    })
    .await
    .expect("the oversized sender is dropped");
    assert!(ended);

    // Everything else is still alive.
    let reply = http(
        h.addr,
        "GET",
        "/api/control/state",
        &[("Authorization", &format!("Bearer {GM}"))],
    )
    .await;
    assert_eq!(reply.status, 200);
    let mut fresh = ws_connect(h.addr, &query, &[]).await.expect("new client");
    let (_, state) = next_snapshot(&mut fresh).await;
    assert!(state.sounds[0].playing, "state survived the garbage");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_number_of_control_clients_is_limited() {
    let h = start(Options {
        max_clients: Some(2),
        ..Default::default()
    })
    .await;
    let query = format!("?g={GM}");
    let mut a = ws_connect(h.addr, &query, &[]).await.expect("a");
    let _b = ws_connect(h.addr, &query, &[]).await.expect("b");
    next_snapshot(&mut a).await;

    let error = ws_connect(h.addr, &query, &[])
        .await
        .expect_err("a third client is refused");
    assert_eq!(status_of(&error), Some(503));

    // A slot comes back when a client leaves.
    a.close(None).await.unwrap();
    drop(a);
    let mut again = None;
    for _ in 0..50 {
        match ws_connect(h.addr, &query, &[]).await {
            Ok(ws) => {
                again = Some(ws);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    assert!(again.is_some(), "the freed slot was never given back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_web_page_from_another_site_cannot_use_the_local_socket() {
    // A local browser needs no token, so the page's origin is what protects it.
    let h = start(Options {
        remote: false,
        ..Default::default()
    })
    .await;
    let error = ws_connect(h.addr, "", &[("Origin", "http://evil.example")])
        .await
        .expect_err("cross-origin handshake refused");
    assert_eq!(status_of(&error), Some(403));

    let own = format!("http://{}", h.addr);
    let mut ws = ws_connect(h.addr, "", &[("Origin", &own)])
        .await
        .expect("same origin");
    next_snapshot(&mut ws).await;
    let mut plain = ws_connect(h.addr, "", &[]).await.expect("not a browser");
    next_snapshot(&mut plain).await;

    // A remote page that knows the token is not a cross-site attack.
    h.remote.store(true, Ordering::Relaxed);
    let mut remote = ws_connect(
        h.addr,
        &format!("?g={GM}"),
        &[("Origin", "http://phone.example")],
    )
    .await
    .expect("token holder");
    next_snapshot(&mut remote).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacing_the_token_closes_remote_sessions() {
    let h = start(Options::default()).await;
    let mut ws = ws_connect(h.addr, &format!("?g={GM}"), &[])
        .await
        .expect("connect");
    next_snapshot(&mut ws).await;

    h.gm.set(Some("brand-new-token".into()));
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(message) = ws.next().await {
            if let Ok(Message::Close(frame)) = message {
                return frame.map(|f| u16::from(f.code));
            }
        }
        None
    })
    .await
    .expect("the session must end");
    assert_eq!(
        closed,
        Some(soundshared::network::access::CLOSE_GM_TOKEN_REVOKED)
    );

    // The old token is dead, the new one works.
    let reply = http(h.addr, "GET", &format!("/api/control/state?g={GM}"), &[]).await;
    assert_eq!(reply.status, 401);
    let reply = http(h.addr, "GET", "/api/control/state?g=brand-new-token", &[]).await;
    assert_eq!(reply.status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_server_closes_control_sessions() {
    let h = start(Options::default()).await;
    let mut ws = ws_connect(h.addr, &format!("?g={GM}"), &[])
        .await
        .expect("connect");
    next_snapshot(&mut ws).await;

    let _ = h.shutdown.send(());
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(message) = ws.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                return;
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the session outlived the server");
}
