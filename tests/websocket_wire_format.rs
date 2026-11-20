//! End-to-end WebSocket wire-format check.
//!
//! Guards against the regression that caused browser crackle: the server must
//! send exactly one 8-byte header (4-byte sequence + 4-byte server stream
//! time) followed by one Opus packet per frame.

use std::time::Duration;

use futures::StreamExt;
use soundshared::network::broadcast::{AudioBroadcaster, AudioFrame, BroadcasterConfig};
use soundshared::network::server::{build_router, ServerState};
use soundshared::network::websocket::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn websocket_audio_frame_has_eight_byte_header_and_opus_payload() {
    let (broadcaster, broadcaster_task) = AudioBroadcaster::new(BroadcasterConfig::default());
    tokio::spawn(broadcaster_task.run());

    let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
    let state = ServerState {
        broadcaster: broadcaster.clone(),
        websocket_config: WebSocketConfig::default(),
        shutdown_tx,
        access: Default::default(),
    };

    // The router the app actually serves, so this test covers the real routes.
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let url = format!("ws://{}/ws/audio", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("websocket connect");

    // Give the session task time to register with the broadcaster.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut samples = vec![0.0f32; 960];
    samples[0] = 0.5;
    samples[1] = -0.5;
    broadcaster
        .send_audio(AudioFrame::new(samples, 48_000, 2, 0))
        .expect("queue frame");

    let mut binary = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                binary = Some(data);
                break;
            }
            Ok(Some(Ok(_))) => continue, // welcome/format text frames
            Ok(Some(Err(e))) => panic!("websocket error: {e}"),
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    let data = binary.expect("expected an audio frame");
    assert!(data.len() > 8, "8-byte header + Opus payload");
    // A 10 ms stereo frame at 96 kbit/s is a couple hundred bytes; any
    // regression back to raw PCM would be ~3840 bytes right away.
    assert!(
        data.len() < 600,
        "wire frame must be Opus-compressed, got {} bytes",
        data.len()
    );

    let seq = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    assert_eq!(seq, 0);

    // The server stream time must be present in the header (any value; the
    // epoch is created moments before, so it is a small count of milliseconds).
    let _server_ms = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

    // The Opus TOC byte: a CELT-mode stereo frame does not have to decode in
    // the test, but the payload must look like an Opus packet (its TOC byte
    // has config=15 as its top bits for full-band VBR music... we stay softer:
    // nonzero TOC, and decodeability checked by the roundtrip unit test).
    assert_ne!(data[8], 0, "opus payload must be present");

    let _ = ws.close(None).await;
}

/// A listener that connects *after* audio started must still be told the stream
/// format: the announcement is replayed from the broadcaster's remembered
/// format instead of being prepared and quietly dropped.
#[tokio::test]
async fn late_listener_receives_the_stream_format() {
    let (broadcaster, broadcaster_task) = AudioBroadcaster::new(BroadcasterConfig::default());
    tokio::spawn(broadcaster_task.run());

    // Establish the format the way the engine does: mixed 10 ms blocks. The wire
    // frame is 20 ms, so two blocks complete the first Opus packet.
    for seq in 0..2 {
        broadcaster
            .send_audio(AudioFrame::new(vec![0.0f32; 960], 48_000, 2, seq))
            .expect("queue frame");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
    let state = ServerState {
        broadcaster: broadcaster.clone(),
        websocket_config: WebSocketConfig::default(),
        shutdown_tx,
        access: Default::default(),
    };
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/audio", addr))
        .await
        .expect("websocket connect");

    let mut format = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && format.is_none() {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let value: serde_json::Value = serde_json::from_str(&text).expect("json");
                if value.get("type").and_then(|t| t.as_str()) == Some("format") {
                    format = Some(value);
                }
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => panic!("websocket error: {e}"),
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    let format = format.expect("a late listener must be sent the stream format");
    assert_eq!(format["data"]["sample_rate"], 48_000);
    assert_eq!(format["data"]["channels"], 2);
    assert_eq!(format["data"]["format"], "opus");

    let _ = ws.close(None).await;
}

/// Stopping the server closes the listener socket. Before this, every session
/// stayed open and the graceful shutdown never completed, which left the
/// server thread blocked forever on "Stop server".
#[tokio::test]
async fn server_shutdown_closes_listener_sessions() {
    let (broadcaster, broadcaster_task) = AudioBroadcaster::new(BroadcasterConfig::default());
    tokio::spawn(broadcaster_task.run());

    let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
    let state = ServerState {
        broadcaster: broadcaster.clone(),
        websocket_config: WebSocketConfig::default(),
        shutdown_tx: shutdown_tx.clone(),
        access: Default::default(),
    };
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/audio", addr))
        .await
        .expect("websocket connect");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // What main.rs's Stop Server sends.
    shutdown_tx.send(()).expect("shutdown signal");

    let closed = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(message) = ws.next().await {
            match message {
                Ok(Message::Close(_)) | Err(_) => return true,
                Ok(_) => continue,
            }
        }
        true
    })
    .await
    .unwrap_or(false);

    assert!(closed, "the session must close when the server stops");
    let _ = ws.close(None).await;
    // The plain test server keeps accepting, so it is detached, not awaited.
    server.abort();
}

/// The stream (and the status API) require the access key from the shared
/// link; a new key disconnects listeners that joined with the old one.
#[tokio::test]
async fn access_key_guards_the_stream_and_revokes_old_links() {
    use soundshared::network::AccessGuard;

    let (broadcaster, broadcaster_task) = AudioBroadcaster::new(BroadcasterConfig::default());
    tokio::spawn(broadcaster_task.run());
    let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
    let access = AccessGuard::new(Some("secret42".into()));
    let state = ServerState {
        broadcaster,
        websocket_config: WebSocketConfig::default(),
        shutdown_tx,
        access: access.clone(),
    };
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // No key, wrong key: refused before the upgrade.
    for url in [
        format!("ws://{addr}/ws/audio"),
        format!("ws://{addr}/ws/audio?k=wrong"),
    ] {
        match tokio_tungstenite::connect_async(url).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), 401)
            }
            other => panic!("expected 401, got {:?}", other.map(|_| ())),
        }
    }

    // The right key connects.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/audio?k=secret42"))
        .await
        .expect("connect with key");
    tokio::time::sleep(Duration::from_millis(100)).await;

    // A new key closes that session with the "key revoked" code.
    access.set(Some("fresh77".into()));
    let mut close_code = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Close(frame)))) => {
                close_code = frame.map(|f| u16::from(f.code));
                break;
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => continue,
        }
    }
    assert_eq!(close_code, Some(4001));

    server.abort();
}

// ------------------------------------------------------------ now playing ----

mod now_playing {
    use super::*;
    use futures::SinkExt;
    use serde_json::Value;
    use soundshared::audio::playlist::mock::MusicMock;
    use soundshared::audio::{CatalogSound, Director};
    use soundshared::config::{Bus, PlaylistConfig, RepeatMode, SceneConfig};
    use soundshared::network::broadcast::{
        BroadcasterHandle, NowPlaying, NowPlayingFeed, NOW_PLAYING_MIN_INTERVAL,
    };
    use std::net::SocketAddr;
    use std::time::Instant;

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// A real router on port 0 with the broadcaster's feed returned.
    async fn serve() -> (BroadcasterHandle, NowPlayingFeed, SocketAddr) {
        let feed = NowPlayingFeed::new();
        let config = BroadcasterConfig {
            now_playing: feed.clone(),
            ..BroadcasterConfig::default()
        };
        let (broadcaster, task) = AudioBroadcaster::new(config);
        tokio::spawn(task.run());
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let state = ServerState {
            broadcaster: broadcaster.clone(),
            websocket_config: WebSocketConfig::default(),
            shutdown_tx,
            access: Default::default(),
        };
        let app = build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (broadcaster, feed, addr)
    }

    async fn connect(addr: SocketAddr) -> Socket {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/audio"))
            .await
            .expect("websocket connect");
        ws
    }

    /// The next text message, with the time it arrived.
    async fn next_text(ws: &mut Socket, wait: Duration) -> Option<(Value, Instant)> {
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(100), ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    return Some((serde_json::from_str(&text).expect("json"), Instant::now()))
                }
                Ok(Some(Ok(_))) | Err(_) => continue,
                Ok(Some(Err(_))) | Ok(None) => return None,
            }
        }
        None
    }

    /// The next text message of `kind`.
    async fn next_of_kind(ws: &mut Socket, kind: &str, wait: Duration) -> Option<(Value, Instant)> {
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            let left = deadline - tokio::time::Instant::now();
            let (value, at) = next_text(ws, left).await?;
            if value["type"] == kind {
                return Some((value, at));
            }
        }
        None
    }

    /// A director with scene "Storm" whose playlist plays track `t0` (no tags:
    /// it is labelled with the sound's name) on a mock output.
    fn rig() -> (Director, MusicMock) {
        let pb = MusicMock::new();
        let sound = pb.add_track(60_000);
        let mut director = Director::new();
        let scene = SceneConfig {
            id: "storm".into(),
            name: "Storm".into(),
            playlist: Some("pl".into()),
            ..Default::default()
        };
        director.sync_library(vec![scene], vec![CatalogSound::new("t0", sound)]);
        director.set_playlists(vec![PlaylistConfig {
            id: "pl".into(),
            name: "Weather".into(),
            items: vec!["t0".into()],
            shuffle: false,
            repeat: RepeatMode::All,
            crossfade_ms: 0,
            bus: Bus::Music,
        }]);
        (director, pb)
    }

    fn names(id: &str) -> Option<String> {
        (id == "t0").then(|| "Rain loop".to_string())
    }

    #[tokio::test]
    async fn listener_gets_now_playing_after_a_scene_is_activated() {
        let (_broadcaster, feed, addr) = serve().await;
        let mut ws = connect(addr).await;
        // Joined in silence: the state at connect is all nulls.
        let (first, _) = next_of_kind(&mut ws, "now_playing", Duration::from_secs(3))
            .await
            .expect("now_playing at connect");
        assert_eq!(first["scene"], Value::Null);
        assert_eq!(first["track"], Value::Null);

        let (mut director, pb) = rig();
        director.activate_scene(&pb, "storm").expect("activate");
        feed.publish(NowPlaying::from_director(&director, names));

        let (msg, _) = next_of_kind(&mut ws, "now_playing", Duration::from_secs(3))
            .await
            .expect("now_playing after activation");
        assert_eq!(msg["scene"], "Storm");
        assert_eq!(msg["track"], "Rain loop");
        // Names only: nothing but the three keys goes over the wire.
        assert_eq!(msg.as_object().unwrap().len(), 3);

        // Stopping the scene silences it.
        director.stop_scene(&pb).expect("stop");
        feed.publish(NowPlaying::from_director(&director, names));
        let (msg, _) = next_of_kind(&mut ws, "now_playing", Duration::from_secs(3))
            .await
            .expect("now_playing after stop");
        assert_eq!(msg["scene"], Value::Null);
        assert_eq!(msg["track"], Value::Null);
        let _ = ws.close(None).await;
    }

    #[tokio::test]
    async fn late_listener_gets_the_current_state_right_after_welcome() {
        let (broadcaster, feed, addr) = serve().await;
        feed.publish(NowPlaying::new(Some("Tavern"), Some("Fiddle - Jig")));
        assert_eq!(
            broadcaster.now_playing(),
            NowPlaying::new(Some("Tavern"), Some("Fiddle - Jig"))
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut ws = connect(addr).await;
        let mut kinds = Vec::new();
        let mut state = None;
        while let Some((value, _)) = next_text(&mut ws, Duration::from_secs(3)).await {
            let kind = value["type"].as_str().unwrap_or("").to_string();
            if kind == "now_playing" {
                state = Some(value);
                break;
            }
            kinds.push(kind);
        }
        assert_eq!(kinds.first().map(String::as_str), Some("welcome"));
        let state = state.expect("late listener gets now_playing");
        assert_eq!(state["scene"], "Tavern");
        assert_eq!(state["track"], "Fiddle - Jig");
        let _ = ws.close(None).await;
    }

    #[tokio::test]
    async fn changes_are_throttled_and_the_last_state_wins() {
        let (_broadcaster, feed, addr) = serve().await;
        let mut ws = connect(addr).await;
        next_of_kind(&mut ws, "now_playing", Duration::from_secs(3))
            .await
            .expect("state at connect");

        // Read on a task of its own so the arrival times are not skewed by
        // the burst below.
        let reader = tokio::spawn(async move {
            let mut got = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_millis(1200);
            while tokio::time::Instant::now() < deadline {
                let left = deadline - tokio::time::Instant::now();
                if let Some((value, at)) = next_of_kind(&mut ws, "now_playing", left).await {
                    got.push((value, at));
                }
            }
            let _ = ws.close(None).await;
            got
        });

        // A burst of 20 changes in a few tens of milliseconds.
        for i in 0..20 {
            feed.publish(NowPlaying::new(Some(&format!("Scene {i}")), None));
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        let got = reader.await.expect("reader task");
        let arrivals: Vec<Instant> = got.iter().map(|(_, at)| *at).collect();
        let last = got.last().map(|(value, _)| value.clone());
        assert!(
            arrivals.len() <= 3,
            "20 quick changes must collapse, got {} messages",
            arrivals.len()
        );
        assert_eq!(last.expect("final state")["scene"], "Scene 19");
        // Allow for timer granularity; never the ~2 ms of the burst.
        for pair in arrivals.windows(2) {
            assert!(
                pair[1] - pair[0] >= NOW_PLAYING_MIN_INTERVAL - Duration::from_millis(40),
                "messages {:?} apart",
                pair[1] - pair[0]
            );
        }
    }

    #[tokio::test]
    async fn every_listener_is_told_and_other_messages_are_unchanged() {
        let (_broadcaster, feed, addr) = serve().await;
        let mut a = connect(addr).await;
        let mut b = connect(addr).await;
        for ws in [&mut a, &mut b] {
            next_of_kind(ws, "now_playing", Duration::from_secs(3))
                .await
                .expect("state at connect");
        }
        feed.publish(NowPlaying::new(Some("Dungeon"), None));
        for ws in [&mut a, &mut b] {
            let (msg, _) = next_of_kind(ws, "now_playing", Duration::from_secs(3))
                .await
                .expect("broadcast to every listener");
            assert_eq!(msg["scene"], "Dungeon");
            assert_eq!(msg["track"], Value::Null);
        }
        // ping/pong still works next to the new message.
        a.send(Message::Text(r#"{"type":"ping"}"#.into()))
            .await
            .expect("send ping");
        assert!(next_of_kind(&mut a, "pong", Duration::from_secs(3))
            .await
            .is_some());
        let _ = a.close(None).await;
        let _ = b.close(None).await;
    }
}
