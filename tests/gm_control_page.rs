//! The phone panel's files (`control.html`, `control.js`, `control.css`): that
//! they are served with the right types, that no secret ends up in them, that
//! the local panel still opens without a token, and that the commands the
//! script sends are the ones the server speaks.
//!
//! Servers bind port 0 on loopback and the "engine thread" is a `Board` on a
//! mock playback: no sound card, no shared port, no real config.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use soundshared::audio::director::mock::MockPlayback;
use soundshared::audio::SoundId;
use soundshared::config::{
    LibraryConfig, SceneConfig, SceneItemConfig, SceneStart, SoundConfigEntry,
};
use soundshared::network::broadcast::{AudioBroadcaster, BroadcasterConfig};
use soundshared::network::control::{
    Board, ControlCommand, ControlContext, ControlFeed, CONTROL_HTML, PANEL_ASSETS,
};
use soundshared::network::server::{build_router_with_control, ServerState};
use soundshared::network::websocket::WebSocketConfig;
use soundshared::network::{AccessGuard, GmAuth, GmGuard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const GM: &str = "gm-secret-token-4711";
const PLAYER: &str = "player-key-0815";

const JS: &str = include_str!("../src/network/static/control.js");
const CSS: &str = include_str!("../src/network/static/control.css");

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn entry(id: &str, name: &str, stinger: bool) -> SoundConfigEntry {
    SoundConfigEntry {
        id: id.into(),
        path: format!("/fake/{id}.wav"),
        name: name.into(),
        volume: 0.8,
        oneshot_stinger: stinger,
        ..Default::default()
    }
}

fn library() -> LibraryConfig {
    LibraryConfig {
        sounds: vec![
            entry("wind", "Wind", false),
            entry("gong", "Gong", true),
            entry("rain", "<img src=x onerror=alert(1)>", false),
        ],
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

async fn start(remote: bool) -> Harness {
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

    let remote = Arc::new(AtomicBool::new(remote));
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

async fn get(addr: SocketAddr, path: &str) -> Reply {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
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

/// Decode a `Transfer-Encoding: chunked` body (axum may use it for a string).
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let Ok(len) = usize::from_str_radix(size.trim(), 16) else {
            return body.to_string();
        };
        if len == 0 {
            break;
        }
        out.push_str(&tail[..len]);
        rest = &tail[len + 2..];
    }
    out
}

fn text_of(reply: &Reply) -> String {
    if reply
        .header("transfer-encoding")
        .is_some_and(|v| v.contains("chunked"))
    {
        dechunk(&reply.body)
    } else {
        reply.body.clone()
    }
}

async fn next_json(ws: &mut Ws) -> serde_json::Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("no message within 5 s")
            .expect("socket closed")
            .expect("frame");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).expect("json");
        }
    }
}

async fn next_of_type(ws: &mut Ws, kind: &str) -> serde_json::Value {
    for _ in 0..20 {
        let message = next_json(ws).await;
        if message["type"] == kind {
            return message;
        }
    }
    panic!("no {kind} message arrived");
}

// ------------------------------------------------------------ the files -----

#[test]
fn the_panel_files_are_the_ones_on_disk() {
    let routes: Vec<&str> = PANEL_ASSETS.iter().map(|(path, _, _)| *path).collect();
    assert_eq!(routes, ["/control.js", "/control.css"]);
    assert_eq!(PANEL_ASSETS[0].2, JS);
    assert_eq!(PANEL_ASSETS[1].2, CSS);
    assert!(CONTROL_HTML.starts_with("<!doctype html>"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_routes_serve_the_files_with_the_right_content_types() {
    // Remote and without any token: the static files carry no secret and a
    // <script src> tag cannot send one.
    let h = start(true).await;
    let js = get(h.addr, "/control.js").await;
    assert_eq!(js.status, 200);
    assert_eq!(
        js.header("content-type").as_deref(),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(text_of(&js), JS);

    let css = get(h.addr, "/control.css").await;
    assert_eq!(css.status, 200);
    assert_eq!(
        css.header("content-type").as_deref(),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(text_of(&css), CSS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_local_panel_opens_without_a_token() {
    let h = start(false).await;
    let page = get(h.addr, "/control").await;
    assert_eq!(page.status, 200, "{}", page.body);
    let content_type = page.header("content-type").unwrap_or_default();
    assert!(
        content_type.starts_with("text/html"),
        "content type {content_type}"
    );
    let body = text_of(&page);
    assert_eq!(body, CONTROL_HTML);
    assert!(body.contains(r#"<script src="/control.js">"#));
    assert!(body.contains(r#"href="/control.css""#));
    // The new page replaced the temporary one.
    assert!(!body.contains("Server-only mode"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_page_needs_the_token_remotely_and_never_contains_it() {
    let h = start(true).await;
    assert_eq!(get(h.addr, "/control").await.status, 401);
    assert_eq!(
        get(h.addr, &format!("/control?g={PLAYER}")).await.status,
        401
    );

    let page = get(h.addr, &format!("/control?g={GM}")).await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(page.header("cache-control").as_deref(), Some("no-store"));
    assert_eq!(
        page.header("referrer-policy").as_deref(),
        Some("no-referrer")
    );
    let body = text_of(&page);
    for secret in [GM, PLAYER] {
        assert!(!body.contains(secret), "the page leaks {secret}");
        assert!(!JS.contains(secret) && !CSS.contains(secret));
    }
    // The page is static: it does not echo the query back.
    assert_eq!(body, CONTROL_HTML);
}

// ---------------------------------------------------- client-side hygiene ---

#[test]
fn the_script_never_builds_html_from_server_data() {
    for banned in [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
        "srcdoc",
    ] {
        assert!(!JS.contains(banned), "control.js uses {banned}");
    }
    assert!(JS.contains("textContent"));
}

#[test]
fn the_page_is_csp_friendly_and_self_contained() {
    let page = CONTROL_HTML;
    assert!(page.contains("Content-Security-Policy"));
    assert!(page.contains("script-src 'self'"));
    assert!(!page.contains("unsafe-inline"));
    // No inline scripts, styles or handlers.
    assert!(!page.contains("<style"));
    assert!(!page.contains("style=\""));
    for attr in ["onclick=", "oninput=", "onchange=", "onload=", "onerror="] {
        assert!(!page.contains(attr), "inline handler {attr}");
    }
    assert!(!page.contains("<script>"), "inline script");
    // Nothing is loaded from another host (and no link could carry the token).
    for text in [page, JS, CSS] {
        assert!(!text.contains("http://") && !text.contains("https://"));
        assert!(!text.contains("@import"));
    }
    assert!(!page.contains("<a "));
    assert!(page.contains(r#"name="referrer" content="no-referrer""#));
    assert!(page.contains("width=device-width"));
}

#[test]
fn the_token_is_removed_from_the_address_and_kept_out_of_logs() {
    assert!(JS.contains("history.replaceState"));
    assert!(JS.contains("sessionStorage"));
    assert!(JS.contains("searchParams.delete('g')"));
    assert!(!JS.contains("console."), "the script must not log");
    assert!(!JS.contains("localStorage"));
}

#[test]
fn the_page_is_accessible_and_themed() {
    assert!(CONTROL_HTML.contains(r#"lang="en""#));
    assert!(CONTROL_HTML.contains(r#"aria-pressed="false""#));
    assert!(CONTROL_HTML.contains(r#"role="status""#));
    assert!(JS.contains("aria-pressed"));
    assert!(CSS.contains(":focus-visible"));
    assert!(CSS.contains("prefers-reduced-motion"));
    assert!(CSS.contains("prefers-color-scheme: dark"));
    assert!(CSS.contains("--target: 44px"));
    assert!(CSS.contains("min-width: 320px"));
}

#[test]
fn every_element_the_script_looks_up_exists_in_the_page() {
    let mut ids = Vec::new();
    for part in JS.split("$('").skip(1) {
        if let Some((id, _)) = part.split_once("')") {
            ids.push(id.to_string());
        }
    }
    assert!(ids.len() > 10, "found only {ids:?}");
    for id in ids {
        assert!(
            CONTROL_HTML.contains(&format!(r#"id="{id}""#)),
            "control.js uses #{id}, which control.html does not have"
        );
    }
}

// -------------------------------------------------- the script's protocol ---

/// The commands control.js sends, as the server must accept them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_commands_the_script_sends_are_accepted_by_the_server() {
    let h = start(true).await;
    let url = format!("ws://{}/ws/control?g={GM}", h.addr);
    let request = url.into_client_request().expect("request");
    let (mut ws, _) = connect_async(request).await.expect("connect");

    // The snapshot has every field the script reads.
    let first = next_of_type(&mut ws, "snapshot").await;
    assert!(first["rev"].is_number());
    let state = &first["state"];
    assert!(state["master_volume"].is_number());
    assert!(state["scenes"].is_array());
    assert!(state["playlists"].is_array());
    assert!(state["ducked"].is_boolean());
    let sounds = state["sounds"].as_array().expect("sounds");
    assert_eq!(sounds.len(), 3);
    for field in ["id", "name", "playing", "looped", "volume", "stinger"] {
        assert!(
            !sounds[0][field].is_null(),
            "snapshot lacks sounds[].{field}"
        );
    }
    assert_eq!(sounds[1]["stinger"], true, "the gong is a stinger");
    // A hostile name arrives as plain data (the page shows it as text).
    assert_eq!(sounds[2]["name"], "<img src=x onerror=alert(1)>");
    assert!(state["scenes"][0]["name"].is_string());
    assert!(state["scenes"][0]["sounds"].is_number());

    let commands = [
        r#"{"cmd":"play","id":0,"req":1}"#,
        r#"{"cmd":"loop","id":0,"on":true,"req":2}"#,
        r#"{"cmd":"volume","id":0,"value":0.5,"req":3}"#,
        r#"{"cmd":"stop","id":0,"req":4}"#,
        r#"{"cmd":"stinger","id":1,"req":5}"#,
        r#"{"cmd":"scene","id":0,"req":6}"#,
        r#"{"cmd":"stop_scene","req":7}"#,
        r#"{"cmd":"master","value":0.4,"req":8}"#,
        r#"{"cmd":"bus","bus":"music","value":0.3,"req":9}"#,
        r#"{"cmd":"bus","bus":"ambience","value":0.3,"req":10}"#,
        r#"{"cmd":"bus","bus":"sfx","value":0.3,"req":11}"#,
        r#"{"cmd":"duck","on":true,"req":12}"#,
        r#"{"cmd":"duck","on":false,"req":13}"#,
        r#"{"cmd":"stop_all","req":14}"#,
        r#"{"cmd":"ping","req":"p"}"#,
    ];
    for (i, command) in commands.iter().enumerate() {
        ws.send(Message::Text((*command).to_string()))
            .await
            .expect("send");
        let reply = loop {
            let message = next_json(&mut ws).await;
            if message["type"] != "snapshot" {
                break message;
            }
        };
        let expected = if i == commands.len() - 1 {
            "pong"
        } else {
            "ack"
        };
        assert_eq!(reply["type"], expected, "{command} -> {reply}");
    }
    // The server's cap on a frame and on a request id are respected by the
    // script's request counter (a small integer).
    assert!(JS.contains("++reqSeq"));
}
