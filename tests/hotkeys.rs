//! Hotkeys end to end: `[[hotkeys]]` from a config file -> bindings -> window
//! key events (egui) -> dispatcher -> Director on a mock playback.
//!
//! No sound card, no real config, no system-wide shortcuts are registered:
//! egui events are fed by hand and playback is `MockPlayback`.

use egui::{Event, Key as EKey, Modifiers, RawInput};
use soundshared::audio::director::mock::MockPlayback;
use soundshared::audio::{CatalogSound, Director, Playback};
use soundshared::config::{
    canonical_hotkey, Config, HotkeyAction, HotkeyConfig, SceneConfig, SceneItemConfig, SceneStart,
};
use soundshared::hotkeys::testing::SpyPlayback;
use soundshared::hotkeys::{
    find_conflicts_for, poll_local, Binding, DispatchOutcome, HotkeyDispatcher, HotkeyMap,
    HotkeyService, NoExtras, Platform, ProblemKind, ServiceStatus,
};

const CONFIG: &str = r##"
[[hotkeys]]
key = "Ctrl+1"
action = { kind = "scene", target = "storm" }

[[hotkeys]]
key = "F5"
[hotkeys.action]
kind = "stop_all"

[[hotkeys]]
key = "control+1"
[hotkeys.action]
kind = "master_up"

[[hotkeys]]
key = "Ctrl+Shift"
[hotkeys.action]
kind = "master_down"

[[hotkeys]]
key = "Ctrl+D"
[hotkeys.action]
kind = "duck_hold"

[[hotkeys]]
key = "alt+r"
action = { kind = "sound", target = "rain" }
"##;

fn config() -> Config {
    toml::from_str(CONFIG).expect("parse")
}

fn rig() -> (Director, SpyPlayback) {
    let pb = SpyPlayback::new();
    let rain = pb.inner.add_sound();
    let mut director = Director::new();
    director.register_sound(CatalogSound::new("rain", rain));
    director.set_scenes(vec![SceneConfig {
        id: "storm".into(),
        name: "Storm".into(),
        fade_in_ms: 0,
        fade_out_ms: 0,
        items: vec![SceneItemConfig {
            sound: "rain".into(),
            volume: 0.7,
            looped: true,
            start: SceneStart::OnEnter,
            delay_ms: 0,
        }],
        ..Default::default()
    }]);
    (director, pb)
}

fn key(key: EKey, pressed: bool, ctrl: bool, alt: bool) -> Event {
    Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers: Modifiers {
            ctrl,
            alt,
            command: ctrl,
            ..Modifiers::NONE
        },
    }
}

/// Run one egui frame, collect the local hotkeys, and return them with the
/// number of key events left for the widgets.
fn local_frame(
    ctx: &egui::Context,
    map: &HotkeyMap,
    events: Vec<Event>,
) -> (Vec<soundshared::hotkeys::HotkeyEvent>, usize) {
    let mut out = None;
    let raw = RawInput {
        events,
        ..RawInput::default()
    };
    let _ = ctx.run(raw, |ctx| {
        let fired = poll_local(ctx, map, None);
        let left = ctx.input(|i| {
            i.events
                .iter()
                .filter(|e| matches!(e, Event::Key { .. }))
                .count()
        });
        out = Some((fired, left));
    });
    out.unwrap()
}

#[test]
fn config_file_to_map_and_conflicts() {
    let config = config();
    assert_eq!(config.hotkeys.len(), 6);
    let map = HotkeyMap::build_for(&config.hotkeys, Platform::Other);
    // Ctrl+1 wins over control+1; Ctrl+Shift has no key.
    let indices: Vec<usize> = map.entries().iter().map(|e| e.index).collect();
    assert_eq!(indices, vec![0, 1, 4, 5]);
    assert!(map
        .problems()
        .iter()
        .any(|p| p.index == 2 && p.kind == ProblemKind::Shadowed { by: 0 }));
    assert!(map
        .problems()
        .iter()
        .any(|p| p.index == 3 && matches!(p.kind, ProblemKind::Invalid(_))));

    let conflicts = find_conflicts_for(&config.hotkeys, Platform::Other);
    assert_eq!(conflicts.len(), 1);
    assert_eq!((conflicts[0].first, conflicts[0].second), (0, 2));
    assert_eq!(conflicts[0].key, "Ctrl+1");
}

#[test]
fn a_config_without_hotkeys_is_an_empty_map() {
    let config: Config = toml::from_str("[server]\nport = 8080\n").unwrap();
    assert!(config.hotkeys.is_empty());
    let map = HotkeyMap::build(&config.hotkeys);
    assert!(map.is_empty());
    assert!(map.problems().is_empty());
    assert!(soundshared::hotkeys::find_conflicts(&config.hotkeys).is_empty());
}

#[test]
fn window_keypress_activates_a_scene_and_is_consumed() {
    let config = config();
    let map = HotkeyMap::build_for(&config.hotkeys, Platform::current());
    let (mut director, pb) = rig();
    let mut dispatcher = HotkeyDispatcher::new();
    let ctx = egui::Context::default();

    let (fired, left) = local_frame(&ctx, &map, vec![key(EKey::Num1, true, true, false)]);
    assert_eq!(left, 0, "the key must not reach the widgets");
    assert_eq!(fired.len(), 1);
    let out = dispatcher
        .handle(&mut director, &pb, &mut NoExtras, &fired[0])
        .unwrap();
    assert!(matches!(out, DispatchOutcome::Scene(_)));
    assert_eq!(director.active_scene(), Some("storm"));

    // F5 stops everything.
    let (fired, _) = local_frame(&ctx, &map, vec![key(EKey::F5, true, false, false)]);
    dispatcher
        .handle(&mut director, &pb, &mut NoExtras, &fired[0])
        .unwrap();
    assert_eq!(director.active_scene(), None);
    assert!(pb.playing_sounds().is_empty());
}

#[test]
fn key_without_a_binding_is_left_alone() {
    let config = config();
    let map = HotkeyMap::build_for(&config.hotkeys, Platform::current());
    let ctx = egui::Context::default();
    let (fired, left) = local_frame(&ctx, &map, vec![key(EKey::Num7, true, true, false)]);
    assert!(fired.is_empty());
    assert_eq!(left, 1);
}

#[test]
fn duck_hold_from_key_down_to_key_up() {
    let config = config();
    let map = HotkeyMap::build_for(&config.hotkeys, Platform::current());
    let (mut director, pb) = rig();
    let mut dispatcher = HotkeyDispatcher::new();
    let ctx = egui::Context::default();

    let (down, _) = local_frame(&ctx, &map, vec![key(EKey::D, true, true, false)]);
    dispatcher
        .handle(&mut director, &pb, &mut NoExtras, &down[0])
        .unwrap();
    assert_eq!(pb.duck_log().len(), 2);
    assert!(pb.duck_log()[0].starts_with("duck"));

    let (up, _) = local_frame(&ctx, &map, vec![key(EKey::D, false, false, false)]);
    dispatcher
        .handle(&mut director, &pb, &mut NoExtras, &up[0])
        .unwrap();
    assert_eq!(pb.duck_log().len(), 4);
    assert!(pb.duck_log()[3].starts_with("release"));
}

#[test]
fn sound_hotkey_toggles_the_sound() {
    let config = config();
    let map = HotkeyMap::build_for(&config.hotkeys, Platform::current());
    let (mut director, pb) = rig();
    let rain = director.catalog()[0].sound;
    let mut dispatcher = HotkeyDispatcher::new();
    let ctx = egui::Context::default();

    // Alt+R twice (released in between so it is not an auto-repeat).
    for expect_playing in [true, false] {
        let (fired, _) = local_frame(&ctx, &map, vec![key(EKey::R, true, false, true)]);
        dispatcher
            .handle(&mut director, &pb, &mut NoExtras, &fired[0])
            .unwrap();
        assert_eq!(pb.is_playing(rain), expect_playing);
        let _ = local_frame(&ctx, &map, vec![key(EKey::R, false, false, false)]);
    }
}

#[test]
fn stale_targets_report_errors_instead_of_panicking() {
    let hotkeys = vec![HotkeyConfig {
        key: "F1".into(),
        action: HotkeyAction::Sound {
            target: "deleted".into(),
        },
    }];
    let map = HotkeyMap::build(&hotkeys);
    let (mut director, pb) = rig();
    let mut dispatcher = HotkeyDispatcher::new();
    let event = soundshared::hotkeys::HotkeyEvent {
        index: 0,
        action: map.entries()[0].action.clone(),
        phase: soundshared::hotkeys::KeyPhase::Pressed,
        source: soundshared::hotkeys::Source::Local,
    };
    let err = dispatcher
        .handle(&mut director, &pb, &mut NoExtras, &event)
        .unwrap_err();
    assert!(err.to_string().contains("deleted"));
}

#[test]
fn cmd_or_ctrl_resolves_per_platform_in_a_config() {
    let hotkeys = vec![HotkeyConfig {
        key: "CmdOrCtrl+K".into(),
        action: HotkeyAction::StopAll,
    }];
    let mac = HotkeyMap::build_for(&hotkeys, Platform::MacOs);
    let other = HotkeyMap::build_for(&hotkeys, Platform::Other);
    assert_eq!(mac.entries()[0].binding.to_string(), "Cmd+K");
    assert_eq!(other.entries()[0].binding.to_string(), "Ctrl+K");
}

#[test]
fn canonical_form_agrees_with_the_config_module_for_single_word_keys() {
    for text in [
        "ctrl+shift+1",
        "Cmd+K",
        "alt+f5",
        "space",
        "NumPad1",
        "shift+ctrl+Up",
        "esc",
        "win+enter",
    ] {
        let mine = Binding::parse_for(text, Platform::Other)
            .unwrap()
            .to_string();
        assert_eq!(canonical_hotkey(text), Some(mine), "{text}");
    }
}

#[test]
fn mock_playback_is_enough_for_scene_hotkeys() {
    // The plain mock (no spy) works too: the dispatcher only needs `Playback`.
    let pb = MockPlayback::new();
    let rain = pb.add_sound();
    let mut director = Director::new();
    director.register_sound(CatalogSound::new("rain", rain));
    let mut dispatcher = HotkeyDispatcher::new();
    pb.set_master_volume(0.5).unwrap();
    dispatcher
        .dispatch(
            &mut director,
            &pb,
            &mut NoExtras,
            &HotkeyAction::MasterUp,
            soundshared::hotkeys::KeyPhase::Pressed,
        )
        .unwrap();
    assert!(pb.master_volume() > 0.5);
}

#[test]
fn global_service_off_by_default_and_never_panics() {
    let started = HotkeyService::start(false, &[], None);
    assert!(started.service.is_none());
    assert_eq!(started.status, ServiceStatus::Disabled);
}

/// Test threads are not the main thread, so on macOS starting the service must
/// come back as a readable status, not a crash.
#[cfg(target_os = "macos")]
#[test]
fn global_service_off_the_main_thread_reports_a_status() {
    let started = HotkeyService::start(true, &[], None);
    assert!(started.service.is_none());
    assert!(matches!(started.status, ServiceStatus::Unavailable(_)));
    assert!(!started.status.message().is_empty());
}

#[cfg(feature = "midi")]
#[test]
fn midi_note_runs_a_scene() {
    use soundshared::hotkeys::midi::MidiMap;
    let hotkeys = vec![HotkeyConfig {
        key: "Midi:Ch1:Note60".into(),
        action: HotkeyAction::Scene {
            target: "storm".into(),
        },
    }];
    let (mut director, pb) = rig();
    let mut dispatcher = HotkeyDispatcher::new();
    let mut midi = MidiMap::build(&hotkeys);
    let events = midi.on_message(&[0x90, 60, 100]);
    assert_eq!(events.len(), 1);
    dispatcher
        .handle(&mut director, &pb, &mut NoExtras, &events[0])
        .unwrap();
    assert_eq!(director.active_scene(), Some("storm"));
    // A keyboard-only map ignores the MIDI entry without complaint.
    assert!(HotkeyMap::build(&hotkeys).problems().is_empty());
}
