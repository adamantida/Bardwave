//! The game-master panel's music and mixer side: the playlist, the bus levels
//! and the ducking state in the control snapshot, the playlist and bus
//! commands over `/ws/control` and REST, and who may fetch the page shell
//! (everybody) versus its data (token or local peer only).
//!
//! The "engine thread" is a `Board` on a mock playback with positions, so
//! nothing here needs a sound card, a shared port or a real config. Servers
//! bind port 0 on loopback; a remote client is simulated by replacing the
//! server's "is this peer local" test.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use soundshared::audio::playlist::mock::MusicMock;
use soundshared::audio::MixControl;
use soundshared::audio::{PlaylistCommand, PlaylistStatus, SoundId, TrackMeta};
use soundshared::config::{Bus, LibraryConfig, PlaylistConfig, RepeatMode, SoundConfigEntry};
use soundshared::network::broadcast::{AudioBroadcaster, BroadcasterConfig};
use soundshared::network::control::{
    Board, BoardState, ControlAction, ControlCommand, ControlContext, ControlFeed, CONTROL_HTML,
    PANEL_ASSETS,
};
use soundshared::network::server::{build_router_with_control, ServerState};
use soundshared::network::websocket::WebSocketConfig;
use soundshared::network::{AccessGuard, GmAuth, GmGuard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const GM: &str = "gm-remote-state-token";
const PLAYER: &str = "player-remote-state-key";
const TRACK_MS: u64 = 10_000;

const JS: &str = include_str!("../src/network/static/control.js");
const CSS: &str = include_str!("../src/network/static/control.css");

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn entry(id: &str, name: &str) -> SoundConfigEntry {
    SoundConfigEntry {
        id: id.into(),
        path: format!("/fake/{id}.mp3"),
        name: name.into(),
        volume: 0.8,
        ..Default::default()
    }
}

fn library() -> LibraryConfig {
    LibraryConfig {
        sounds: vec![
            entry("t0", "Alpha"),
            entry("t1", "Beta"),
            entry("t2", "Gamma"),
        ],
        playlists: vec![
            PlaylistConfig {
                id: "p1".into(),
                name: "Tavern".into(),
                items: vec!["t0".into(), "t1".into(), "t2".into()],
                shuffle: false,
                repeat: RepeatMode::All,
                crossfade_ms: 0,
                bus: Bus::Music,
            },
            PlaylistConfig {
                id: "p2".into(),
                name: "Empty".into(),
                items: Vec::new(),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

/// A mock with three tracks: tags on the first, a title only on the third.
fn tracks(pb: &MusicMock) -> Vec<(usize, SoundId)> {
    let metas = [
        TrackMeta {
            title: Some("Hearth".into()),
            artist: Some("The Bards".into()),
            album: None,
        },
        TrackMeta::default(),
        TrackMeta {
            title: Some("Last Call".into()),
            artist: None,
            album: None,
        },
    ];
    metas
        .into_iter()
        .enumerate()
        .map(|(i, meta)| (i, pb.add_track_with_meta(TRACK_MS, meta)))
        .collect()
}

fn playlist_of(state: &BoardState) -> &soundshared::network::control::ControlPlaylistState {
    state
        .playlist
        .as_ref()
        .expect("the board reports its playlist")
}

// ------------------------------------------------------------ the state ----

#[test]
fn the_state_reports_playlist_buses_and_ducking() {
    let pb = MusicMock::new();
    let loaded = tracks(&pb);
    let mut board = Board::assemble(&library(), &loaded);

    // Before anything plays: the music layer is there but idle, every bus is
    // wide open, nothing is ducked.
    let state = board.handle(&pb, ControlAction::List).unwrap();
    assert_eq!(state.playlists.len(), 2);
    let pl = playlist_of(&state);
    assert_eq!(pl.state.status, PlaylistStatus::Stopped);
    assert_eq!(pl.label, None);
    assert_eq!(
        (state.buses.music, state.buses.ambience, state.buses.sfx),
        (1.0, 1.0, 1.0)
    );
    assert!(!state.ducked && state.ducked_buses.is_empty());

    // Start: the track, its tags, length, shuffle and repeat arrive.
    let state = board
        .handle(
            &pb,
            ControlAction::Playlist(PlaylistCommand::Start("p1".into())),
        )
        .unwrap();
    let pl = playlist_of(&state);
    assert_eq!(pl.state.status, PlaylistStatus::Playing);
    assert_eq!(pl.state.playlist.as_deref(), Some("p1"));
    assert_eq!(pl.state.name.as_deref(), Some("Tavern"));
    assert_eq!(pl.state.track_index, Some(0));
    assert_eq!(pl.state.track_count, 3);
    assert_eq!(pl.state.title.as_deref(), Some("Hearth"));
    assert_eq!(pl.state.artist.as_deref(), Some("The Bards"));
    assert_eq!(pl.label.as_deref(), Some("The Bards - Hearth"));
    assert_eq!(pl.state.duration_ms, Some(TRACK_MS));
    assert!(!pl.state.shuffle);
    assert_eq!(pl.state.repeat, RepeatMode::All);

    // The position moves with the clock but is published per second.
    pb.advance(3_700);
    board.tick(&pb);
    let state = board.handle(&pb, ControlAction::List).unwrap();
    assert_eq!(playlist_of(&state).state.position_ms, 3_000);

    // Next: a track without tags is labelled with its sound's name.
    let state = board
        .handle(&pb, ControlAction::Playlist(PlaylistCommand::Next))
        .unwrap();
    let pl = playlist_of(&state);
    assert_eq!(pl.state.track_index, Some(1));
    assert_eq!(pl.label.as_deref(), Some("Beta"));
    // ...and one with a title only by the title.
    let state = board
        .handle(&pb, ControlAction::Playlist(PlaylistCommand::Next))
        .unwrap();
    assert_eq!(playlist_of(&state).label.as_deref(), Some("Last Call"));
    let state = board
        .handle(&pb, ControlAction::Playlist(PlaylistCommand::Prev))
        .unwrap();
    assert_eq!(playlist_of(&state).state.track_index, Some(1));

    // Pause, resume, seek.
    let state = board
        .handle(&pb, ControlAction::Playlist(PlaylistCommand::Pause))
        .unwrap();
    assert_eq!(playlist_of(&state).state.status, PlaylistStatus::Paused);
    let state = board
        .handle(&pb, ControlAction::Playlist(PlaylistCommand::Resume))
        .unwrap();
    assert_eq!(playlist_of(&state).state.status, PlaylistStatus::Playing);
    let state = board
        .handle(&pb, ControlAction::Playlist(PlaylistCommand::Seek(6_400)))
        .unwrap();
    assert_eq!(playlist_of(&state).state.position_ms, 6_000);

    // Shuffle and repeat.
    let state = board
        .handle(
            &pb,
            ControlAction::Playlist(PlaylistCommand::SetShuffle(true)),
        )
        .unwrap();
    assert!(playlist_of(&state).state.shuffle);
    let state = board
        .handle(
            &pb,
            ControlAction::Playlist(PlaylistCommand::SetRepeat(RepeatMode::One)),
        )
        .unwrap();
    assert_eq!(playlist_of(&state).state.repeat, RepeatMode::One);

    // Bus levels are the real ones, and ducking names its buses.
    let state = board
        .handle(
            &pb,
            ControlAction::Mix(MixControl::SetBusVolume(Bus::Music, 0.4)),
        )
        .unwrap();
    assert_eq!(state.buses.music, 0.4);
    assert_eq!(state.buses.sfx, 1.0);
    let state = board.handle(&pb, ControlAction::Duck(true)).unwrap();
    assert!(state.ducked);
    assert!(
        !state.ducked_buses.is_empty()
            && state
                .ducked_buses
                .iter()
                .all(|b| ["music", "ambience", "sfx"].contains(&b.as_str())),
        "{:?}",
        state.ducked_buses
    );
    let state = board.handle(&pb, ControlAction::Duck(false)).unwrap();
    assert!(!state.ducked && state.ducked_buses.is_empty());

    // Stop.
    let state = board
        .handle(&pb, ControlAction::Playlist(PlaylistCommand::Stop))
        .unwrap();
    assert_ne!(playlist_of(&state).state.status, PlaylistStatus::Playing);
}

#[test]
fn playlist_commands_that_cannot_run_are_errors_not_panics() {
    let pb = MusicMock::new();
    let loaded = tracks(&pb);
    let mut board = Board::assemble(&library(), &loaded);
    let play = |id: &str| ControlAction::Playlist(PlaylistCommand::Start(id.into()));

    // Nothing plays yet.
    for command in [
        PlaylistCommand::Next,
        PlaylistCommand::Prev,
        PlaylistCommand::Seek(1_000),
        PlaylistCommand::Pause,
    ] {
        assert!(
            board.handle(&pb, ControlAction::Playlist(command)).is_err(),
            "nothing is playing"
        );
    }
    assert!(board.handle(&pb, play("nope")).is_err());
    assert!(board.handle(&pb, play("p2")).is_err(), "no tracks");
    // Out-of-range bus values are refused by the director, never applied.
    assert!(board
        .handle(
            &pb,
            ControlAction::Mix(MixControl::SetBusVolume(Bus::Music, 7.0))
        )
        .is_err());
    // The board still works afterwards.
    let state = board.handle(&pb, play("p1")).unwrap();
    assert_eq!(playlist_of(&state).state.status, PlaylistStatus::Playing);
    // A seek past the end is pulled back into the track.
    let state = board
        .handle(
            &pb,
            ControlAction::Playlist(PlaylistCommand::Seek(u64::MAX)),
        )
        .unwrap();
    assert!(playlist_of(&state).state.position_ms < TRACK_MS);
}

#[test]
fn an_old_state_without_the_new_fields_still_parses() {
    let old = r#"{"sounds":[],"master_volume":1.0,"scenes":[],"playlists":[],"ducked":false}"#;
    let state: BoardState = serde_json::from_str(old).expect("v1.4 state");
    assert!(state.playlist.is_none());
    assert_eq!(state.buses.music, 1.0);
    assert!(state.ducked_buses.is_empty());
}

// ------------------------------------------------------------- server ------

struct Harness {
    addr: SocketAddr,
    remote: Arc<AtomicBool>,
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

async fn start(remote_peer: bool) -> Harness {
    let library = library();
    let pb = Arc::new(MusicMock::new());
    let loaded = tracks(&pb);
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

    let remote = Arc::new(AtomicBool::new(remote_peer));
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
        remote,
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

/// Decode a `Transfer-Encoding: chunked` body.
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            return body.to_string();
        };
        if size == 0 {
            break;
        }
        if tail.len() < size {
            return body.to_string();
        }
        out.push_str(&tail[..size]);
        rest = tail[size..].trim_start_matches("\r\n");
    }
    out
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
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    Reply {
        status,
        headers: head.to_string(),
        body: if chunked {
            dechunk(body)
        } else {
            body.to_string()
        },
    }
}

const HDR: (&str, &str) = ("X-Bardwave-Control", "1");

async fn ws_connect(
    addr: SocketAddr,
    query: &str,
) -> Result<Ws, Box<tokio_tungstenite::tungstenite::Error>> {
    let request = format!("ws://{addr}/ws/control{query}")
        .into_client_request()
        .expect("request");
    connect_async(request)
        .await
        .map(|(ws, _)| ws)
        .map_err(Box::new)
}

async fn next_json(ws: &mut Ws) -> serde_json::Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("no message within 5 s")
            .expect("socket closed")
            .expect("socket error");
        match message {
            Message::Text(text) => return serde_json::from_str(&text).expect("json"),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

/// Send one command and return its reply (snapshots skipped).
async fn command(ws: &mut Ws, text: &str) -> serde_json::Value {
    ws.send(Message::Text(text.to_string()))
        .await
        .expect("send");
    loop {
        let message = next_json(ws).await;
        if message["type"] != "snapshot" {
            return message;
        }
    }
}

/// Snapshots until one satisfies `done`.
async fn snapshot_where(ws: &mut Ws, done: impl Fn(&BoardState) -> bool) -> BoardState {
    for _ in 0..20 {
        let message = next_json(ws).await;
        if message["type"] == "snapshot" {
            let state: BoardState =
                serde_json::from_value(message["state"].clone()).expect("state");
            if done(&state) {
                return state;
            }
        }
    }
    panic!("the wanted state never arrived");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playlist_and_bus_commands_work_over_the_socket() {
    let h = start(true).await;
    let mut ws = ws_connect(h.addr, &format!("?g={GM}"))
        .await
        .expect("connect");

    // The first snapshot already carries the new fields.
    let first = next_json(&mut ws).await;
    assert_eq!(first["type"], "snapshot");
    let state = &first["state"];
    assert_eq!(state["playlist"]["status"], "stopped");
    assert_eq!(state["buses"]["music"], 1.0);
    assert!(state["ducked_buses"].is_array());

    let reply = command(&mut ws, r#"{"cmd":"playlist_start","id":"p1","req":1}"#).await;
    assert_eq!(
        (reply["type"].as_str(), reply["req"].as_i64()),
        (Some("ack"), Some(1))
    );
    let state = snapshot_where(&mut ws, |s| {
        s.playlist
            .as_ref()
            .is_some_and(|p| p.state.status == PlaylistStatus::Playing)
    })
    .await;
    assert_eq!(
        playlist_of(&state).label.as_deref(),
        Some("The Bards - Hearth")
    );

    // The alias `playlist_play` starts a playlist too.
    let reply = command(&mut ws, r#"{"cmd":"playlist_play","id":"p1","req":2}"#).await;
    assert_eq!(reply["type"], "ack");

    for (text, check) in [
        (r#"{"cmd":"playlist_next","req":3}"#, "next"),
        (r#"{"cmd":"playlist_prev","req":4}"#, "prev"),
        (r#"{"cmd":"playlist_pause","req":5}"#, "pause"),
        (r#"{"cmd":"playlist_resume","req":6}"#, "resume"),
        (r#"{"cmd":"playlist_toggle","req":7}"#, "toggle"),
        (r#"{"cmd":"playlist_toggle","req":8}"#, "toggle"),
        (r#"{"cmd":"playlist_seek","ms":4500,"req":9}"#, "seek"),
        (
            r#"{"cmd":"playlist_shuffle","on":true,"req":10}"#,
            "shuffle",
        ),
        (
            r#"{"cmd":"playlist_repeat","mode":"one","req":11}"#,
            "repeat",
        ),
    ] {
        let reply = command(&mut ws, text).await;
        assert_eq!(reply["type"], "ack", "{check}: {reply}");
    }
    let state = snapshot_where(&mut ws, |s| {
        s.playlist.as_ref().is_some_and(|p| {
            p.state.shuffle && p.state.repeat == RepeatMode::One && p.state.position_ms == 4_000
        })
    })
    .await;
    assert_eq!(playlist_of(&state).state.status, PlaylistStatus::Playing);

    // Bus fader and ducking.
    let reply = command(
        &mut ws,
        r#"{"cmd":"bus","bus":"ambience","value":0.25,"req":12}"#,
    )
    .await;
    assert_eq!(reply["type"], "ack");
    snapshot_where(&mut ws, |s| s.buses.ambience == 0.25).await;
    // An out-of-range value is refused, not applied.
    let reply = command(&mut ws, r#"{"cmd":"bus","bus":"sfx","value":3,"req":13}"#).await;
    assert_eq!(reply["type"], "error", "{reply}");
    let reply = command(&mut ws, r#"{"cmd":"duck","on":true,"req":14}"#).await;
    assert_eq!(reply["type"], "ack");
    let state = snapshot_where(&mut ws, |s| s.ducked).await;
    assert!(!state.ducked_buses.is_empty());
    let reply = command(&mut ws, r#"{"cmd":"duck","on":false,"req":15}"#).await;
    assert_eq!(reply["type"], "ack");
    snapshot_where(&mut ws, |s| !s.ducked && s.ducked_buses.is_empty()).await;

    let reply = command(&mut ws, r#"{"cmd":"playlist_stop","req":16}"#).await;
    assert_eq!(reply["type"], "ack");
    snapshot_where(&mut ws, |s| {
        s.playlist
            .as_ref()
            .is_some_and(|p| p.state.status != PlaylistStatus::Playing)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_and_malformed_commands_are_refused_and_the_socket_lives_on() {
    let h = start(true).await;
    let mut ws = ws_connect(h.addr, &format!("?g={GM}"))
        .await
        .expect("connect");
    let _ = next_json(&mut ws).await;

    for (text, why) in [
        (r#"{"cmd":"playlist_explode","req":1}"#, "unknown command"),
        (r#"{"cmd":"playlist_start","req":2}"#, "no id"),
        (r#"{"cmd":"playlist_start","id":7,"req":3}"#, "numeric id"),
        (
            r#"{"cmd":"playlist_seek","ms":-5,"req":4}"#,
            "negative position",
        ),
        (
            r#"{"cmd":"playlist_seek","ms":"soon","req":5}"#,
            "text position",
        ),
        (
            r#"{"cmd":"playlist_repeat","mode":"forever","req":6}"#,
            "unknown repeat",
        ),
        (
            r#"{"cmd":"playlist_shuffle","on":"yes","req":7}"#,
            "non-boolean",
        ),
        (
            r#"{"cmd":"bus","bus":"master","value":0.5,"req":8}"#,
            "unknown bus",
        ),
        (
            r#"{"cmd":"bus","bus":"music","value":"loud","req":9}"#,
            "text volume",
        ),
    ] {
        let reply = command(&mut ws, text).await;
        assert_eq!(reply["type"], "error", "{why}: {reply}");
        assert!(reply["message"].is_string());
    }

    // Commands the engine refuses come back as errors too.
    for text in [
        r#"{"cmd":"playlist_start","id":"nope","req":20}"#,
        r#"{"cmd":"playlist_start","id":"p2","req":21}"#,
        r#"{"cmd":"playlist_next","req":22}"#,
        r#"{"cmd":"playlist_seek","ms":1000,"req":23}"#,
    ] {
        let reply = command(&mut ws, text).await;
        assert_eq!(reply["type"], "error", "{text}: {reply}");
    }

    let reply = command(&mut ws, r#"{"cmd":"ping","req":"p"}"#).await;
    assert_eq!(reply["type"], "pong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playlist_commands_over_rest() {
    let h = start(true).await;
    let auth = format!("Bearer {GM}");
    let post = |path: &'static str| {
        let auth = auth.clone();
        let addr = h.addr;
        async move { http(addr, "POST", path, &[("Authorization", &auth)]).await }
    };

    let reply = post("/api/control/playlist/start?id=p1").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let state: BoardState = serde_json::from_str(&reply.body).expect("state");
    assert_eq!(playlist_of(&state).state.status, PlaylistStatus::Playing);

    for path in [
        "/api/control/playlist/next",
        "/api/control/playlist/prev",
        "/api/control/playlist/pause",
        "/api/control/playlist/resume",
        "/api/control/playlist/toggle",
        "/api/control/playlist/toggle",
        "/api/control/playlist/seek?ms=2500",
        "/api/control/playlist/shuffle?on=true",
        "/api/control/playlist/repeat?mode=OFF",
    ] {
        let reply = post(path).await;
        assert_eq!(reply.status, 200, "{path}: {}", reply.body);
    }
    let reply = post("/api/control/playlist/repeat?mode=all").await;
    let state: BoardState = serde_json::from_str(&reply.body).expect("state");
    let pl = playlist_of(&state);
    assert!(pl.state.shuffle);
    assert_eq!(pl.state.repeat, RepeatMode::All);

    // Bad requests are 400s with a message, not panics.
    for path in [
        "/api/control/playlist/explode",
        "/api/control/playlist/start",
        "/api/control/playlist/start?id=nope",
        "/api/control/playlist/seek",
        "/api/control/playlist/seek?ms=-1",
        "/api/control/playlist/shuffle",
        "/api/control/playlist/repeat?mode=sometimes",
    ] {
        let reply = post(path).await;
        assert!(
            reply.status == 400,
            "{path}: {} {}",
            reply.status,
            reply.body
        );
    }

    let reply = post("/api/control/bus?bus=sfx&value=0.3").await;
    assert_eq!(reply.status, 200);
    let state: BoardState = serde_json::from_str(&reply.body).expect("state");
    assert_eq!(state.buses.sfx, 0.3);
    let reply = post("/api/control/bus?bus=sfx&value=9").await;
    assert_eq!(reply.status, 400, "an overshoot is refused");
    let reply = post("/api/control/bus?bus=sfx&value=0.3").await;
    let state: BoardState = serde_json::from_str(&reply.body).expect("state");
    assert_eq!(state.buses.sfx, 0.3, "and changes nothing");
}

// ------------------------------------------------------- shell vs. data ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shell_is_open_but_its_data_needs_the_token() {
    let h = start(true).await;

    // A remote phone that reloaded the page (no token in the address) gets
    // the shell...
    let page = http(h.addr, "GET", "/control", &[]).await;
    assert_eq!(page.status, 200);
    assert_eq!(page.body, CONTROL_HTML);
    assert_eq!(page.header("cache-control").as_deref(), Some("no-store"));
    assert_eq!(
        page.header("referrer-policy").as_deref(),
        Some("no-referrer")
    );
    for &(path, _, body) in PANEL_ASSETS {
        let reply = http(h.addr, "GET", path, &[]).await;
        assert_eq!(reply.status, 200, "{path}");
        assert_eq!(reply.body, body, "{path}");
    }
    // ...even with a query that carries somebody's token: it is not echoed.
    let with_token = http(h.addr, "GET", &format!("/control?g={GM}"), &[]).await;
    assert_eq!(with_token.status, 200);
    for secret in [GM, PLAYER] {
        assert!(!with_token.body.contains(secret), "the page leaks a secret");
        assert!(!page.body.contains(secret));
        assert!(!JS.contains(secret) && !CSS.contains(secret));
    }

    // ...but none of the data or commands.
    for (method, path) in [
        ("GET", "/api/control/state"),
        ("GET", "/api/control/scenes"),
        ("POST", "/api/control/play?id=0"),
        ("POST", "/api/control/bus?bus=music&value=0.5"),
        ("POST", "/api/control/duck?on=true"),
        ("POST", "/api/control/playlist/start?id=p1"),
        ("POST", "/api/control/playlist/next"),
        ("POST", "/api/control/playlist/seek?ms=1"),
        ("POST", "/api/control/playlist/explode"),
    ] {
        // A fresh server each time: the failed-attempt limiter would answer
        // 429 after a handful of refusals from one address.
        let fresh = start(true).await;
        let reply = http(fresh.addr, method, path, &[HDR]).await;
        assert_eq!(reply.status, 401, "{method} {path}: {}", reply.body);
        let wrong = http(
            fresh.addr,
            method,
            path,
            &[HDR, ("Authorization", "Bearer wrong")],
        )
        .await;
        assert_eq!(wrong.status, 401, "{method} {path}");
    }
    let error = ws_connect(h.addr, "")
        .await
        .expect_err("socket needs the token");
    assert!(matches!(
        *error,
        tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status().as_u16() == 401
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_client_needs_no_token_as_before() {
    let h = start(false).await;
    assert!(!h.remote.load(Ordering::Relaxed));

    let state = http(h.addr, "GET", "/api/control/state", &[HDR]).await;
    assert_eq!(state.status, 200, "{}", state.body);
    let state: BoardState = serde_json::from_str(&state.body).expect("state");
    assert!(state.playlist.is_some());

    let reply = http(h.addr, "POST", "/api/control/playlist/start?id=p1", &[HDR]).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    // The CSRF header is still demanded of a token-less local action.
    let reply = http(h.addr, "POST", "/api/control/playlist/next", &[]).await;
    assert_eq!(reply.status, 403);

    let mut ws = ws_connect(h.addr, "").await.expect("local socket");
    let first = next_json(&mut ws).await;
    assert_eq!(first["type"], "snapshot");
    assert_eq!(first["state"]["playlist"]["status"], "playing");
}

// ------------------------------------------------------------ the script ---

#[test]
fn the_script_uses_the_new_commands_and_fields() {
    for needle in [
        "playlist_start",
        "playlist_pause",
        "playlist_resume",
        "playlist_next",
        "playlist_prev",
        "playlist_stop",
        "playlist_seek",
        "playlist_shuffle",
        "playlist_repeat",
        "ducked_buses",
        "server.buses",
        "duration_ms",
        "position_ms",
    ] {
        assert!(JS.contains(needle), "control.js lacks {needle}");
    }
}

#[test]
fn the_script_keeps_the_token_in_session_storage_and_explains_a_missing_one() {
    // The token survives a reload through sessionStorage (in try/catch)...
    assert!(JS.contains("sessionStorage.getItem(TOKEN_KEY)"));
    assert!(JS.contains("sessionStorage.setItem(TOKEN_KEY, token)"));
    assert!(JS.contains("searchParams.delete('g')"));
    assert!(!JS.contains("localStorage"));
    // ...and without one the panel says what to do instead of staying dead.
    assert!(JS.contains("Open the link from the console"));
}

#[test]
fn the_script_parses() {
    // Skipped where Node is not installed.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/network/static/control.js");
    match std::process::Command::new("node")
        .arg("--check")
        .arg(path)
        .output()
    {
        Ok(output) => assert!(
            output.status.success(),
            "node --check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(_) => eprintln!("node not found: syntax check skipped"),
    }
}
