//! The phone panel in console mode: the window and the phone drive one and the
//! same app.
//!
//! A headless console (the real `SoundSharedApp`, driven frame by frame with a
//! bare egui context and an audio engine without a sound card) sits behind the
//! real `/control` routes of the server router. The HTTP clients run on a
//! tokio runtime in this process; the test thread plays the render loop while
//! they wait, which is exactly the shape of the real application (axum on its
//! own thread, commands executed in the frame).
//!
//! Everything binds port 0 on loopback. The data directory is redirected to a
//! temporary folder before the first use, so nothing here can touch the real
//! `config.toml`.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use eframe::egui;
use futures::{SinkExt, StreamExt};
use soundshared::audio::AudioEngine;
use soundshared::config::{Config, SceneConfig, SceneItemConfig, SceneStart, SoundConfigEntry};
use soundshared::gui::app::SoundSharedApp;
use soundshared::gui::gm_bridge::GmPanelLink;
use soundshared::gui::types::AppState;
use soundshared::network::broadcast::{AudioBroadcaster, BroadcasterConfig};
use soundshared::network::control::{BoardState, ControlContext, ControlFeed};
use soundshared::network::server::{build_router_with_control, ServerState};
use soundshared::network::websocket::WebSocketConfig;
use soundshared::network::{AccessGuard, GmAuth, GmGuard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const GM: &str = "gm-secret-token";
const WIND_ID: &str = "11111111-1111-4111-8111-111111111111";
const GONG_ID: &str = "22222222-2222-4222-8222-222222222222";
const STORM_ID: &str = "33333333-3333-4333-8333-333333333333";
const HDR: (&str, &str) = ("X-Bardwave-Control", "1");

/// Point every per-user data location at a temporary folder, once, before
/// anything asks for the data directory.
fn isolate_data_dir() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bardwave-gm-gui-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp home");
        for name in ["HOME", "USERPROFILE", "APPDATA", "XDG_DATA_HOME"] {
            std::env::set_var(name, &dir);
        }
    });
}

/// A silent 16-bit mono 8 kHz WAV of `secs` seconds.
fn write_wav(path: &std::path::Path, secs: u32) {
    let data_len = secs * 8_000 * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&8_000u32.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(bytes.len() + data_len as usize, 0);
    std::fs::write(path, bytes).expect("write wav");
}

#[derive(Default)]
struct Options {
    gm_token: Option<&'static str>,
    /// Treat every peer as a remote one.
    remote: bool,
}

struct Console {
    app: SoundSharedApp,
    ctx: egui::Context,
    rt: tokio::runtime::Runtime,
    router: axum::Router,
    shutdown: tokio::sync::broadcast::Sender<()>,
    server: Option<(SocketAddr, tokio::task::JoinHandle<()>)>,
    remote: Arc<AtomicBool>,
    feed: ControlFeed,
    dir: PathBuf,
    _audio_rx: crossbeam_channel::Receiver<soundshared::gui::types::AudioCommand>,
    _server_rx: crossbeam_channel::Receiver<soundshared::gui::types::ServerCommand>,
}

impl Drop for Console {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn console(options: Options) -> Console {
    isolate_data_dir();
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bardwave-gm-gui-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let wind = dir.join("wind.wav");
    let gong = dir.join("gong.wav");
    write_wav(&wind, 20);
    write_wav(&gong, 20);

    let mut config = Config::default();
    config.library.sounds = vec![
        SoundConfigEntry {
            id: WIND_ID.into(),
            path: wind.to_string_lossy().into(),
            name: "Wind".into(),
            volume: 0.8,
            ..Default::default()
        },
        SoundConfigEntry {
            id: GONG_ID.into(),
            path: gong.to_string_lossy().into(),
            name: "Gong".into(),
            volume: 1.0,
            ..Default::default()
        },
    ];
    config.library.scenes = vec![SceneConfig {
        id: STORM_ID.into(),
        name: "Storm".into(),
        items: vec![SceneItemConfig {
            sound: WIND_ID.into(),
            volume: 0.7,
            looped: true,
            start: SceneStart::OnEnter,
            delay_ms: 0,
        }],
        ..Default::default()
    }];

    // The server side: the panel's queue and feed, shared with the console.
    let (control_tx, control_rx) = crossbeam_channel::unbounded();
    let feed = ControlFeed::new();
    let gm = GmGuard::new(options.gm_token.map(str::to_string));
    let remote = Arc::new(AtomicBool::new(options.remote));
    let auth = GmAuth::new(gm.clone()).with_locality({
        let remote = remote.clone();
        Arc::new(move |peer: &SocketAddr| {
            !remote.load(Ordering::Relaxed) && peer.ip().is_loopback()
        })
    });
    let context = ControlContext::new(control_tx, auth, feed.clone());

    let (audio_tx, audio_rx) = bounded(64);
    let (server_tx, server_rx) = bounded(64);
    let state = AppState::new(
        audio_tx,
        server_tx,
        Arc::new(Mutex::new(0)),
        Arc::new(Mutex::new(config)),
    )
    .with_gm(gm, true)
    .with_gm_panel(GmPanelLink {
        commands: control_rx,
        feed: feed.clone(),
    });
    assert!(state.gm_panel_served, "wiring the panel marks it served");

    let ctx = egui::Context::default();
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = Arc::new(Mutex::new(Some(
        AudioEngine::new().expect("audio engine without a device"),
    )));
    let app = SoundSharedApp::with_context(&ctx, state, engine, audio_rx.clone());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let (shutdown, _) = tokio::sync::broadcast::channel(1);
    let router = rt.block_on(async {
        let (broadcaster, task) = AudioBroadcaster::new(BroadcasterConfig::default());
        tokio::spawn(task.run());
        let state = ServerState {
            broadcaster,
            websocket_config: WebSocketConfig::default(),
            shutdown_tx: shutdown.clone(),
            access: AccessGuard::new(Some("player-key".into())),
        };
        build_router_with_control(state, Some(context))
    });

    let mut console = Console {
        app,
        ctx,
        rt,
        router,
        shutdown,
        server: None,
        remote,
        feed,
        dir,
        _audio_rx: audio_rx,
        _server_rx: server_rx,
    };
    // The saved library is loaded by the decoder worker, frame by frame.
    console.pump_until(|c| c.app.get_sounds().len() == 2, "library to load");
    console
}

impl Console {
    /// One frame of the render loop.
    fn frame(&mut self) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let app = &mut self.app;
        let _ = self.ctx.run(input, |ctx| app.update(ctx));
    }

    fn pump_until(&mut self, done: impl Fn(&Console) -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.frame();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Run `work` on the runtime while this thread plays the render loop.
    fn drive<T: Send + 'static>(&mut self, work: impl Future<Output = T> + Send + 'static) -> T {
        let handle = self.rt.spawn(work);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !handle.is_finished() {
            assert!(Instant::now() < deadline, "the request never finished");
            self.frame();
            std::thread::sleep(Duration::from_millis(2));
        }
        self.rt.block_on(handle).expect("task")
    }

    /// Bind the listener, as the server thread does on Start.
    fn start_server(&mut self) -> SocketAddr {
        assert!(self.server.is_none(), "already started");
        let router = self.router.clone();
        let mut stop = self.shutdown.subscribe();
        let (addr, task) = self.rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            let task = tokio::spawn(async move {
                let _ = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = stop.recv().await;
                })
                .await;
            });
            (addr, task)
        });
        self.server = Some((addr, task));
        addr
    }

    /// Stop the listener, as the server thread does on Stop.
    fn stop_server(&mut self) {
        let (_, task) = self.server.take().expect("running");
        let _ = self.shutdown.send(());
        self.rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("the listener shuts down")
                .expect("join");
        });
    }

    fn sound_index(&self, name: &str) -> usize {
        self.app
            .get_sounds()
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| panic!("no sound {name}"))
    }

    fn window_playing(&self, name: &str) -> bool {
        self.app
            .get_sounds()
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| panic!("no sound {name}"))
            .playing
    }
}

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn state(&self) -> BoardState {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
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
        body: body.to_string(),
    }
}

/// A local call: loopback needs no token, only the control header.
async fn local(addr: SocketAddr, method: &'static str, path: String) -> Reply {
    http(addr, method, &path, &[HDR]).await
}

// ------------------------------------------------------------ commands ------

#[test]
fn phone_commands_change_the_console_and_reach_the_feed() {
    let mut c = console(Options::default());
    let addr = c.start_server();
    let wind = c.sound_index("Wind");
    let gong = c.sound_index("Gong");

    // The first snapshot is the console's library, before anything is touched.
    let reply = c.drive(local(addr, "GET", "/api/control/state".into()));
    assert_eq!(reply.status, 200, "{}", reply.body);
    let state = reply.state();
    assert_eq!(state.sounds.len(), 2);
    assert_eq!(state.scenes.len(), 1);
    assert_eq!(state.scenes[0].name, "Storm");
    assert_eq!(state.active_scene, None);
    assert!(state.sounds.iter().all(|s| !s.playing));

    // Play from the phone: the window's row lights up, the answer and the feed
    // both say so.
    let reply = c.drive(local(addr, "POST", format!("/api/control/play?id={gong}")));
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(reply.state().sounds[gong].playing);
    assert!(c.window_playing("Gong"), "the window row follows the phone");
    assert!(c.feed.latest().state.sounds[gong].playing);

    // Activate a scene: same Director as the window, so the active scene and
    // its sounds are what both sides see.
    let reply = c.drive(local(
        addr,
        "POST",
        "/api/control/scene/activate?id=0".into(),
    ));
    assert_eq!(reply.status, 200, "{}", reply.body);
    let state = reply.state();
    assert_eq!(state.active_scene, Some(0));
    assert!(state.scenes[0].active);
    assert!(
        c.window_playing("Wind"),
        "the scene's sound plays in the window"
    );
    let published = c.feed.latest();
    assert_eq!(published.state.active_scene, Some(0));
    assert!(published.state.sounds[wind].playing);
    assert!(
        published.rev > 0,
        "the feed carries revisions for /ws/control"
    );

    // The master level is the console's master level.
    let reply = c.drive(local(addr, "POST", "/api/control/master?value=0.25".into()));
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!((c.app.get_master_volume() - 0.25).abs() < 1e-6);
    assert!((c.feed.latest().state.master_volume - 0.25).abs() < 1e-6);

    // Stop all: nothing keeps playing, in the window or on the phone.
    let reply = c.drive(local(addr, "POST", "/api/control/stop_all".into()));
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(reply.state().sounds.iter().all(|s| !s.playing));
    assert!(!c.window_playing("Wind") && !c.window_playing("Gong"));
    assert!(c.feed.latest().state.sounds.iter().all(|s| !s.playing));
}

#[test]
fn a_click_in_the_window_is_published_to_the_phone() {
    let mut c = console(Options::default());
    let gong_id = c
        .app
        .get_sounds()
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case("Gong"))
        .expect("gong")
        .id;
    let gong = c.sound_index("Gong");

    assert!(!c.feed.latest().state.sounds[gong].playing);
    c.app.request_play_sound(gong_id);
    c.pump_until(
        |c| c.feed.latest().state.sounds[gong].playing,
        "the window's play to reach the feed",
    );
    assert!(c.window_playing("Gong"));

    c.app.stop_all_sounds();
    c.pump_until(
        |c| !c.feed.latest().state.sounds[gong].playing,
        "the window's stop to reach the feed",
    );
}

#[test]
fn the_live_socket_follows_the_console() {
    let mut c = console(Options::default());
    let addr = c.start_server();
    let gong = c.sound_index("Gong");

    let seen = c.drive(async move {
        let (mut ws, _) = connect_async(format!("ws://{addr}/ws/control"))
            .await
            .expect("local socket needs no token");
        // First message: a full snapshot with a revision.
        let first = next_snapshot(&mut ws).await;
        assert!(first.sounds.iter().all(|s| !s.playing));
        ws.send(Message::Text(format!(
            r#"{{"cmd":"play","id":{gong},"req":"p1"}}"#
        )))
        .await
        .expect("send");
        for _ in 0..20 {
            let state = next_snapshot(&mut ws).await;
            if state.sounds[gong].playing {
                return true;
            }
        }
        false
    });
    assert!(seen, "the playing state arrived on the socket");
    assert!(c.window_playing("Gong"));
}

async fn next_snapshot<S>(ws: &mut S) -> BoardState
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no message within 10 s")
            .expect("socket closed")
            .expect("socket error");
        if let Message::Text(text) = message {
            let json: serde_json::Value = serde_json::from_str(&text).expect("json");
            if json["type"] == "snapshot" {
                return serde_json::from_value(json["state"].clone()).expect("state");
            }
        }
    }
}

// ----------------------------------------------------------- lifecycle ------

#[test]
fn the_routes_exist_between_start_and_stop_only() {
    let mut c = console(Options::default());
    let gong = c.sound_index("Gong");

    for round in 0..2 {
        let addr = c.start_server();
        let reply = c.drive(local(addr, "GET", "/api/control/state".into()));
        assert_eq!(reply.status, 200, "round {round}: {}", reply.body);
        let reply = c.drive(http(addr, "GET", "/control", &[]));
        assert_eq!(reply.status, 200, "round {round}: the panel page");
        let reply = c.drive(local(addr, "POST", format!("/api/control/play?id={gong}")));
        assert_eq!(reply.status, 200, "round {round}: {}", reply.body);
        assert!(c.window_playing("Gong"));
        let reply = c.drive(local(addr, "POST", "/api/control/stop_all".into()));
        assert_eq!(reply.status, 200);

        c.stop_server();
        // Nothing listens any more: the connection is refused.
        let refused = c.rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(addr))
                .await
                .expect("connect answers")
                .is_err()
        });
        assert!(refused, "round {round}: the port must be closed after Stop");
    }
}

// ---------------------------------------------------------------- access ----

#[test]
fn access_rules_are_the_remote_panel_rules() {
    let mut c = console(Options {
        gm_token: Some(GM),
        remote: true,
    });
    let addr = c.start_server();
    let gong = c.sound_index("Gong");

    // A remote peer without the token, or with a wrong one, is refused and the
    // console does not move.
    let play = format!("/api/control/play?id={gong}");
    for (path, headers) in [
        (play.clone(), vec![HDR]),
        (format!("{play}&g=wrong-token"), vec![HDR]),
        (format!("{play}&k=player-key"), vec![HDR]),
    ] {
        let reply = c.drive({
            let headers = headers.clone();
            async move { http(addr, "POST", &path, &headers).await }
        });
        assert_eq!(reply.status, 401, "{}", reply.body);
    }
    assert!(!c.window_playing("Gong"), "refused commands must not run");

    // With the game-master token (query or Bearer) it works.
    let reply = c.drive({
        let path = format!("{play}&g={GM}");
        async move { http(addr, "POST", &path, &[HDR]).await }
    });
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(c.window_playing("Gong"));
    let bearer = format!("Bearer {GM}");
    let reply = c.drive(async move {
        http(
            addr,
            "POST",
            "/api/control/stop_all",
            &[HDR, ("Authorization", &bearer)],
        )
        .await
    });
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(!c.window_playing("Gong"));

    // This computer needs no token.
    c.remote.store(false, Ordering::Relaxed);
    let reply = c.drive(local(addr, "GET", "/api/control/state".into()));
    assert_eq!(reply.status, 200);
}

#[test]
fn without_a_gm_token_only_this_computer_is_served() {
    let mut c = console(Options {
        gm_token: None,
        remote: true,
    });
    let addr = c.start_server();
    for token in ["", "anything"] {
        let reply = c.drive({
            let path = format!("/api/control/state?g={token}");
            async move { http(addr, "GET", &path, &[HDR]).await }
        });
        assert_eq!(reply.status, 401, "token {token:?}");
    }
    c.remote.store(false, Ordering::Relaxed);
    let reply = c.drive(local(addr, "GET", "/api/control/state".into()));
    assert_eq!(reply.status, 200);
}

#[test]
fn a_console_without_the_panel_runs_frames_as_before() {
    isolate_data_dir();
    let (audio_tx, audio_rx) = bounded(8);
    let (server_tx, _server_rx) = bounded(8);
    let state = AppState::new(
        audio_tx,
        server_tx,
        Arc::new(Mutex::new(0)),
        Arc::new(Mutex::new(Config::default())),
    );
    assert!(!state.gm_panel_served);
    let ctx = egui::Context::default();
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = Arc::new(Mutex::new(Some(AudioEngine::new().expect("engine"))));
    let mut app = SoundSharedApp::with_context(&ctx, state, engine, audio_rx);
    for _ in 0..3 {
        let _ = ctx.run(egui::RawInput::default(), |ctx| app.update(ctx));
    }
}
