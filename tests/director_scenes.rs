//! Scenes through the Director: the headless board (`Board`) on a mock
//! playback, the control API over HTTP, and the real engine without a device.
//!
//! No sound card, no shared port (the HTTP server binds port 0), no real
//! config: sounds are generated into a temporary directory.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use soundshared::audio::director::mock::{MockCall, MockPlayback};
use soundshared::audio::{
    AudioEngine, CatalogSound, Director, DirectorState, FadeSpec, Playback, SoundId,
};
use soundshared::config::{
    Bus, FadeCurve, LibraryConfig, SceneConfig, SceneItemConfig, SceneStart, SoundConfigEntry,
};
use soundshared::network::control::{
    router, Board, BoardState, ControlAction, ControlCommand, ScenesResponse,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn entry(id: &str, name: &str, volume: f32, looped: bool) -> SoundConfigEntry {
    SoundConfigEntry {
        id: id.into(),
        path: format!("/fake/{id}.wav"),
        name: name.into(),
        volume,
        looped,
        ..Default::default()
    }
}

fn item(sound: &str, volume: f32, looped: bool) -> SceneItemConfig {
    SceneItemConfig {
        sound: sound.into(),
        volume,
        looped,
        start: SceneStart::OnEnter,
        delay_ms: 0,
        pan: 0.0,
    }
}

fn scene(id: &str, name: &str, items: Vec<SceneItemConfig>) -> SceneConfig {
    SceneConfig {
        id: id.into(),
        name: name.into(),
        fade_in_ms: 1000,
        fade_out_ms: 2000,
        items,
        ..Default::default()
    }
}

/// rain, wind, crow, door (stinger); scenes "Storm" and "Calm".
fn library() -> LibraryConfig {
    let mut door = entry("door", "Door", 0.9, false);
    door.oneshot_stinger = true;
    door.bus = Bus::Sfx;
    LibraryConfig {
        sounds: vec![
            entry("rain", "Rain", 1.0, true),
            entry("wind", "Wind", 1.0, true),
            entry("crow", "Crow", 1.0, false),
            door,
        ],
        scenes: vec![
            scene(
                "storm",
                "Storm",
                vec![item("rain", 0.8, true), item("wind", 0.4, true)],
            ),
            scene(
                "calm",
                "Calm",
                vec![item("rain", 0.3, true), item("crow", 1.0, false)],
            ),
        ],
        ..Default::default()
    }
}

struct Rig {
    pb: MockPlayback,
    board: Board,
    ids: Vec<SoundId>,
}

fn rig() -> Rig {
    let pb = MockPlayback::new();
    let library = library();
    let ids: Vec<SoundId> = library.sounds.iter().map(|_| pb.add_sound()).collect();
    let loaded: Vec<(usize, SoundId)> = ids.iter().copied().enumerate().collect();
    let board = Board::assemble(&library, &loaded);
    Rig { pb, board, ids }
}

// ------------------------------------------------------- headless Board ----

#[test]
fn board_lists_scenes_and_the_active_one() {
    let mut r = rig();
    let state = r.board.handle(&r.pb, ControlAction::List).unwrap();
    assert_eq!(state.scenes.len(), 2);
    assert_eq!(state.scenes[0].name, "Storm");
    assert_eq!(state.scenes[0].sounds, 2);
    assert_eq!(state.scenes[1].uuid, "calm");
    assert_eq!(state.active_scene, None);

    let state = r
        .board
        .handle(&r.pb, ControlAction::ActivateScene(1))
        .unwrap();
    assert_eq!(state.active_scene, Some(1));
    assert!(state.scenes[1].active && !state.scenes[0].active);
}

#[test]
fn board_activates_a_scene_through_the_director() {
    let mut r = rig();
    let state = r
        .board
        .handle(&r.pb, ControlAction::ActivateScene(0))
        .unwrap();
    // rain (0) and wind (1) play, crow (2) does not.
    assert!(state.sounds[0].playing && state.sounds[1].playing);
    assert!(!state.sounds[2].playing);
    r.pb.advance(1000);
    assert!((r.pb.gain(r.ids[0]) - 0.8).abs() < 1e-4);
    assert!((r.pb.gain(r.ids[1]) - 0.4).abs() < 1e-4);
    assert!(r.pb.is_looped(r.ids[0]));

    // Crossfade to Calm: crow in, wind fades out, rain retuned.
    let state = r
        .board
        .handle(&r.pb, ControlAction::ActivateScene(1))
        .unwrap();
    assert_eq!(state.active_scene, Some(1));
    assert!(state.sounds[2].playing);
    assert!(!state.sounds[1].playing, "wind reports stopped at once");
    assert!(r.pb.gain(r.ids[1]) > 0.0, "but its tail still sounds");
    r.pb.advance(2000);
    assert_eq!(r.pb.gain(r.ids[1]), 0.0);
    assert!((r.pb.gain(r.ids[0]) - 0.3).abs() < 1e-4);

    let state = r.board.handle(&r.pb, ControlAction::StopScene).unwrap();
    assert_eq!(state.active_scene, None);
    assert!(!state.sounds[0].playing && !state.sounds[2].playing);
}

#[test]
fn board_rejects_an_unknown_scene() {
    let mut r = rig();
    assert!(r
        .board
        .handle(&r.pb, ControlAction::ActivateScene(9))
        .is_err());
}

#[test]
fn board_plays_a_stinger_over_the_scene() {
    let mut r = rig();
    r.board
        .handle(&r.pb, ControlAction::ActivateScene(0))
        .unwrap();
    r.pb.advance(1000);
    r.pb.clear_calls();

    // Sound 3 is flagged as a stinger: Play layers it, twice if pressed twice.
    let state = r.board.handle(&r.pb, ControlAction::Play(3)).unwrap();
    assert!(state.sounds[3].playing);
    r.board.handle(&r.pb, ControlAction::Play(3)).unwrap();
    assert_eq!(r.pb.track_count(r.ids[3]), 2);
    assert!(r
        .pb
        .calls()
        .iter()
        .all(|c| !matches!(c, MockCall::Stop { .. } | MockCall::StopAll { .. })));
    assert_eq!(r.pb.bus_of(r.ids[3]), Some(Bus::Sfx));

    // The scene is untouched and the next one spares the stinger.
    assert_eq!(r.board.director().active_scene(), Some("storm"));
    let state = r
        .board
        .handle(&r.pb, ControlAction::ActivateScene(1))
        .unwrap();
    assert!(state.sounds[3].playing);
}

#[test]
fn board_stop_all_with_fade_clears_the_scene() {
    let mut r = rig();
    r.board
        .handle(&r.pb, ControlAction::ActivateScene(0))
        .unwrap();
    r.pb.advance(1000);
    let state = r
        .board
        .handle(&r.pb, ControlAction::StopAllFade(1500))
        .unwrap();
    assert_eq!(state.active_scene, None);
    assert!(state.sounds.iter().all(|s| !s.playing));
    assert!(r.pb.calls().contains(&MockCall::StopAll { fade_ms: 1500 }));
}

#[test]
fn board_keeps_the_old_controls_working() {
    let mut r = rig();
    let state = r.board.handle(&r.pb, ControlAction::Play(2)).unwrap();
    assert!(state.sounds[2].playing);
    let state = r
        .board
        .handle(&r.pb, ControlAction::SetVolume(2, 0.25))
        .unwrap();
    assert_eq!(state.sounds[2].volume, 0.25);
    let state = r
        .board
        .handle(&r.pb, ControlAction::SetLoop(2, true))
        .unwrap();
    assert!(state.sounds[2].looped);
    let state = r
        .board
        .handle(&r.pb, ControlAction::SetMaster(0.5))
        .unwrap();
    assert_eq!(state.master_volume, 0.5);
    let state = r.board.handle(&r.pb, ControlAction::Stop(2)).unwrap();
    assert!(!state.sounds[2].playing);
    assert!(r.board.handle(&r.pb, ControlAction::Play(77)).is_err());
}

#[test]
fn board_delayed_items_start_on_tick() {
    let pb = MockPlayback::new();
    let mut library = library();
    let mut late = item("wind", 0.5, true);
    late.delay_ms = 400;
    library.scenes = vec![scene("late", "Late", vec![item("rain", 1.0, true), late])];
    let ids: Vec<SoundId> = library.sounds.iter().map(|_| pb.add_sound()).collect();
    let loaded: Vec<(usize, SoundId)> = ids.iter().copied().enumerate().collect();
    let mut board = Board::assemble(&library, &loaded);

    let state = board.handle(&pb, ControlAction::ActivateScene(0)).unwrap();
    assert!(state.sounds[0].playing && !state.sounds[1].playing);
    let due = board.next_deadline().expect("a delayed start is waiting");
    assert!(due > Instant::now());

    board.tick(&pb); // not due yet
    assert!(!pb.is_playing(ids[1]));
    std::thread::sleep(Duration::from_millis(450));
    board.tick(&pb);
    assert!(pb.is_playing(ids[1]));
    assert_eq!(board.next_deadline(), None);
}

#[test]
fn board_publishes_state_changes() {
    let mut r = rig();
    let mut rx = r.board.subscribe();
    rx.borrow_and_update();
    r.board
        .handle(&r.pb, ControlAction::ActivateScene(0))
        .unwrap();
    assert!(rx.has_changed().unwrap());
    let state: DirectorState = rx.borrow_and_update().clone();
    assert_eq!(state.active_scene.as_deref(), Some("storm"));
    assert_eq!(state.playing, vec!["rain".to_string(), "wind".to_string()]);
}

// ------------------------------------------------------------ snapshots ----

#[test]
fn a_snapshot_plays_the_same_mix_back() {
    let mut r = rig();
    r.board
        .handle(&r.pb, ControlAction::ActivateScene(0))
        .unwrap();
    r.pb.advance(1000);
    // Touch the mix by hand: wind a bit louder.
    r.board
        .handle(&r.pb, ControlAction::SetVolume(1, 0.6))
        .unwrap();

    // Snapshot through a standalone Director on the same playback.
    let mut director = Director::new();
    let library = library();
    let catalog = library
        .sounds
        .iter()
        .zip(&r.ids)
        .map(|(e, id)| CatalogSound::new(&e.id, *id))
        .collect();
    director.sync_library(library.scenes.clone(), catalog);
    let snap = director.snapshot_to_scene(&r.pb, "Copy").unwrap();
    assert_eq!(snap.items.len(), 2);
    let wind = snap.items.iter().find(|i| i.sound == "wind").unwrap();
    assert!((wind.volume - 0.6).abs() < 1e-4);

    director.stop_all_with_fade(&r.pb, 0).unwrap();
    director.activate_scene(&r.pb, &snap.id).unwrap();
    r.pb.advance(2000);
    assert!((r.pb.gain(r.ids[0]) - 0.8).abs() < 1e-4);
    assert!((r.pb.gain(r.ids[1]) - 0.6).abs() < 1e-4);
    assert!(!r.pb.is_playing(r.ids[2]));
}

// -------------------------------------------------------- the real engine ----

fn write_wav(path: &Path) {
    // 2 s of a quiet 48 kHz stereo tone, 16-bit PCM.
    let frames = 96_000u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + frames * 4).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&48_000u32.to_le_bytes());
    bytes.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 4).to_le_bytes());
    for i in 0..frames {
        let v = ((i as f32 * 0.05).sin() * 3000.0) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bardwave-director-{tag}-{}-{}",
        std::process::id(),
        SoundId::new().0
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn scenes_drive_the_real_engine() {
    let dir = temp_dir("engine");
    let mut library = LibraryConfig::default();
    for name in ["a", "b", "c"] {
        let path = dir.join(format!("{name}.wav"));
        write_wav(&path);
        library.sounds.push(SoundConfigEntry {
            id: name.into(),
            path: path.to_string_lossy().into_owned(),
            name: name.to_uppercase(),
            ..Default::default()
        });
    }
    library.scenes = vec![
        scene(
            "one",
            "One",
            vec![item("a", 0.5, true), item("b", 0.7, false)],
        ),
        scene(
            "two",
            "Two",
            vec![item("b", 0.2, true), item("c", 1.0, false)],
        ),
    ];

    let engine = AudioEngine::new().unwrap();
    let mut board = Board::load(&engine, &library);
    assert_eq!(board.len(), 3);

    let state = board
        .handle(&engine, ControlAction::ActivateScene(0))
        .unwrap();
    assert!(state.sounds[0].playing && state.sounds[1].playing && !state.sounds[2].playing);
    // Engine order is a hash order: check by volume instead.
    let volumes: Vec<f32> = engine
        .list_sounds()
        .into_iter()
        .filter(|s| engine.is_sound_playing(*s).unwrap())
        .map(|s| engine.sound_volume(s).unwrap())
        .collect();
    assert!(volumes.contains(&0.5) && volumes.contains(&0.7));

    // Scene two: "a" fades out, "b" is retuned and repeats now, "c" starts.
    let state = board
        .handle(&engine, ControlAction::ActivateScene(1))
        .unwrap();
    assert!(!state.sounds[0].playing, "a is on its way out");
    assert!(state.sounds[1].playing && state.sounds[2].playing);
    let b_looped = engine
        .list_sounds()
        .into_iter()
        .filter(|s| engine.is_sound_looped(*s).unwrap())
        .count();
    assert_eq!(b_looped, 1, "only b repeats");
    let b_volume = engine
        .list_sounds()
        .into_iter()
        .filter(|s| engine.is_sound_looped(*s).unwrap())
        .map(|s| engine.sound_volume(s).unwrap())
        .next()
        .unwrap();
    assert!(
        (b_volume - 0.2).abs() < 1e-6,
        "the fade target is remembered"
    );

    let state = board.handle(&engine, ControlAction::StopScene).unwrap();
    assert!(state.sounds.iter().all(|s| !s.playing));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn engine_playback_trait_matches_the_engine() {
    let dir = temp_dir("trait");
    let wav = dir.join("t.wav");
    write_wav(&wav);
    let engine = AudioEngine::new().unwrap();
    let sound = engine.load_sound(&wav).unwrap();
    let pb: &dyn Playback = &engine;

    assert!(!pb.is_playing(sound));
    pb.play(
        sound,
        true,
        Some(0.4),
        FadeSpec::new(500, FadeCurve::Linear),
    )
    .unwrap();
    assert!(pb.is_playing(sound) && pb.is_looped(sound));
    assert_eq!(pb.playing_sounds(), vec![sound]);
    assert_eq!(pb.volume(sound), Some(0.4));
    pb.fade_to(sound, 0.9, FadeSpec::new(500, FadeCurve::EqualPower))
        .unwrap();
    assert_eq!(pb.volume(sound), Some(0.9));
    assert!(pb.fade_to(sound, 1.5, FadeSpec::NONE).is_err());
    pb.stop(sound, FadeSpec::new(1000, FadeCurve::EqualPower))
        .unwrap();
    assert!(!pb.is_playing(sound));
    assert_eq!(pb.volume(SoundId::new()), None);
    let _ = std::fs::remove_dir_all(dir);
}

// -------------------------------------------------------------- HTTP API ----

struct Reply {
    status: u16,
    body: String,
}

async fn http(addr: SocketAddr, method: &str, path: &str, header: bool) -> Reply {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let extra = if header {
        "X-Bardwave-Control: 1\r\n"
    } else {
        ""
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("response");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status");
    // The body may be chunked; for these small JSON bodies take the JSON part.
    let body = match (body.find(['{', '[']), body.rfind(['}', ']'])) {
        (Some(a), Some(b)) if b >= a => body[a..=b].to_string(),
        _ => body.to_string(),
    };
    Reply { status, body }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_api_serves_scenes_and_runs_them() {
    let pb = Arc::new(MockPlayback::new());
    let library = library();
    let ids: Vec<SoundId> = library.sounds.iter().map(|_| pb.add_sound()).collect();
    let loaded: Vec<(usize, SoundId)> = ids.iter().copied().enumerate().collect();

    // The "main thread" of the headless server: owns the board, runs commands.
    let (tx, rx) = crossbeam_channel::unbounded::<ControlCommand>();
    let pump_pb = pb.clone();
    let pump = std::thread::spawn(move || {
        let mut board = Board::assemble(&library, &loaded);
        while let Ok(command) = rx.recv_timeout(Duration::from_secs(10)) {
            let result = board.handle(&*pump_pb, command.action);
            let _ = command.reply.send(result);
        }
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(tx);
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    // The scene list.
    let reply = http(addr, "GET", "/api/control/scenes", true).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let scenes: ScenesResponse = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(scenes.scenes.len(), 2);
    assert_eq!(scenes.scenes[0].name, "Storm");
    assert_eq!(scenes.active_scene, None);

    // Without the control header nothing happens.
    let reply = http(addr, "POST", "/api/control/scene/activate?id=0", false).await;
    assert_eq!(reply.status, 403);
    assert!(pb.playing_sounds().is_empty());

    // Activate.
    let reply = http(addr, "POST", "/api/control/scene/activate?id=0", true).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let state: BoardState = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(state.active_scene, Some(0));
    assert!(state.sounds[0].playing && state.sounds[1].playing);

    // The state endpoint carries scenes and the active one too.
    let reply = http(addr, "GET", "/api/control/state", true).await;
    let state: BoardState = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(state.scenes.len(), 2);
    assert!(state.scenes[0].active);

    // A bad scene is a 400, not a crash.
    let reply = http(addr, "POST", "/api/control/scene/activate?id=7", true).await;
    assert_eq!(reply.status, 400);

    // Stop the scene.
    let reply = http(addr, "POST", "/api/control/scene/stop", true).await;
    assert_eq!(reply.status, 200);
    let state: BoardState = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(state.active_scene, None);
    assert!(state.sounds.iter().all(|s| !s.playing));

    // stop_all with a fade.
    http(addr, "POST", "/api/control/scene/activate?id=1", true).await;
    let reply = http(addr, "POST", "/api/control/stop_all?fade_ms=800", true).await;
    assert_eq!(reply.status, 200);
    assert!(pb.calls().contains(&MockCall::StopAll { fade_ms: 800 }));

    // The page has the scene buttons.
    let reply = http(addr, "GET", "/control", false).await;
    assert_eq!(reply.status, 200);

    drop(pump); // the pump ends on its own when the sender is gone / times out
}
