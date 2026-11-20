//! Backwards compatibility of `config.toml`: a file written by v1.4.0 must load
//! into the v1.5 schema without errors or losses, get ids on load, and survive
//! being saved and loaded again.
//!
//! Every file lives in a unique directory under the system temp dir; the real
//! config of the user is never touched.

use std::path::{Path, PathBuf};

use soundshared::config::{
    canonical_hotkey, Bus, Config, FadeCurve, HotkeyAction, HotkeyConfig, RepeatMode, SceneStart,
};
use soundshared::gui::groups::Library;

/// A config exactly as v1.4.0 writes it: README sections, a `[library]` with
/// groups and sounds, no ids, no scenes, no `gm_token`.
const V140: &str = r#"
[server]
host = "0.0.0.0"
port = 8080
upnp = true
access_token = "k3y0fabcdef12345"

[audio]
buffer_size = 960
sample_rate = 48000
channels = 2
master_volume = 0.65

[logging]
level = "info"
file_logging = false

[gui]
reduce_motion = true

[[library.groups]]
id = "6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22"
name = "Nature"

[[library.groups]]
id = "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f"
name = "Combat"

[[library.sounds]]
path = 'C:\sounds\rain.ogg'
name = "Rain"
volume = 0.4
looped = true
group = "6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22"

[[library.sounds]]
path = '/home/gm/sounds/sword hit.wav'
name = "Sword"
volume = 1.0
looped = false
group = "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f"

[[library.sounds]]
path = 'D:\music\tavern.mp3'
"#;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bardwave-compat-{}-{}-{}",
        name,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(&path, text).expect("write config");
    path
}

#[test]
fn v140_config_loads_without_loss() {
    let dir = temp_dir("load");
    let path = write(&dir, V140);

    let config = Config::load_from(&path).expect("v1.4.0 config must load");

    assert_eq!(config.server.port, 8080);
    assert_eq!(
        config.server.access_token.as_deref(),
        Some("k3y0fabcdef12345")
    );
    assert_eq!(config.audio.master_volume, 0.65);
    assert!(config.gui.reduce_motion);

    assert_eq!(config.library.groups.len(), 2);
    assert_eq!(config.library.sounds.len(), 3);
    let rain = &config.library.sounds[0];
    assert_eq!(rain.path, r"C:\sounds\rain.ogg");
    assert_eq!(rain.name, "Rain");
    assert_eq!(rain.volume, 0.4);
    assert!(rain.looped);
    assert_eq!(
        rain.group.as_deref(),
        Some("6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22")
    );
    // The bare entry keeps the old defaults.
    let bare = &config.library.sounds[2];
    assert_eq!(bare.volume, 1.0);
    assert!(!bare.looped && bare.group.is_none() && bare.name.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn v140_config_gets_new_fields_at_their_defaults() {
    let dir = temp_dir("defaults");
    let config = Config::load_from(&write(&dir, V140)).expect("load");

    // New sound fields.
    for sound in &config.library.sounds {
        assert!(sound.tags.is_empty());
        assert_eq!(sound.gain_db, 0.0);
        assert_eq!(sound.bus, Bus::Ambience);
        assert!(!sound.oneshot_stinger);
    }
    // No scenes, playlists or hotkeys appear out of nowhere.
    assert!(config.library.scenes.is_empty());
    assert!(config.library.playlists.is_empty());
    assert!(config.hotkeys.is_empty());

    // Audio extras: normalization off, documented defaults.
    assert!(!config.audio.normalize);
    assert_eq!(config.audio.normalize_target_lufs, -18.0);
    assert_eq!(config.audio.duck_amount, 0.3);
    assert_eq!(config.audio.duck_attack_ms, 200);
    assert_eq!(config.audio.duck_release_ms, 800);
    assert_eq!(config.audio.duck_buses, vec![Bus::Music, Bus::Ambience]);

    // GUI: global hotkeys are opt-in; the old option is kept.
    assert!(!config.gui.global_hotkeys);
    assert!(config.gui.reduce_motion);

    // gm_token is not invented by the loader (main.rs generates it).
    assert_eq!(config.server.gm_token, None);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ids_are_minted_on_load_and_become_permanent_on_save() {
    let dir = temp_dir("ids");
    let path = write(&dir, V140);

    let first = Config::load_from(&path).expect("load");
    let ids: Vec<String> = first.library.sounds.iter().map(|s| s.id.clone()).collect();
    assert_eq!(ids.len(), 3);
    for id in &ids {
        assert!(uuid::Uuid::parse_str(id).is_ok(), "{id:?} is not a UUID");
    }
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "ids must be unique");

    // Until saved, every load would mint different ids; after a save they stay.
    first.save_to(&path).expect("save");
    let second = Config::load_from(&path).expect("reload");
    let again: Vec<String> = second.library.sounds.iter().map(|s| s.id.clone()).collect();
    assert_eq!(ids, again);

    // And the library model sees the same ids.
    let lib = Library::from_config(&second.library);
    for (sound, id) in lib.sounds().iter().zip(&ids) {
        assert_eq!(sound.id.to_string(), *id);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn v140_config_round_trips_through_the_library_model() {
    let dir = temp_dir("model");
    let path = write(&dir, V140);
    let loaded = Config::load_from(&path).expect("load");

    let lib = Library::from_full_config(&loaded);
    assert!(lib.validate().is_empty());
    let mut saved = loaded.clone();
    lib.write_config(&mut saved);

    assert_eq!(saved.library, loaded.library);
    assert_eq!(saved.hotkeys, loaded.hotkeys);

    saved.save_to(&path).expect("save");
    let reloaded = Config::load_from(&path).expect("reload");
    assert_eq!(reloaded.library, loaded.library);
    assert_eq!(reloaded.server.port, loaded.server.port);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_saved_v15_config_does_not_leak_empty_new_sections() {
    // A v1.4.0 user who never touches scenes must not get scenes/hotkeys
    // clutter in the file (and a v1.4.0 binary reading it back keeps working,
    // since unknown keys are ignored there).
    let dir = temp_dir("clean");
    let path = write(&dir, V140);
    let config = Config::load_from(&path).expect("load");
    config.save_to(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read");

    assert!(!text.contains("scenes"));
    assert!(!text.contains("playlists"));
    assert!(!text.contains("[[hotkeys]]"));
    assert!(!text.contains("\nhotkeys = "));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn full_v15_config_round_trips_through_toml() {
    let text = r##"
[server]
port = 9000
gm_token = "gmsecret"

[audio]
normalize = true
normalize_target_lufs = -16.0
duck_amount = 0.5
duck_attack_ms = 100
duck_release_ms = 1500
duck_buses = ["music"]

[gui]
global_hotkeys = true

[[library.sounds]]
id = "11111111-1111-4111-8111-111111111111"
path = "/s/crow.wav"
tags = ["bird", "forest"]
gain_db = -3.5
bus = "sfx"
oneshot_stinger = true

[[library.sounds]]
id = "22222222-2222-4222-8222-222222222222"
path = "/s/theme.flac"
bus = "music"

[[library.scenes]]
id = "33333333-3333-4333-8333-333333333333"
name = "Forest"
color = "#336633"
fade_in_ms = 3000
fade_out_ms = 1500
curve = "linear"
stop_others = false
playlist = "44444444-4444-4444-8444-444444444444"

[[library.scenes.items]]
sound = "11111111-1111-4111-8111-111111111111"
volume = 0.4
looped = true
start = "manual"
delay_ms = 250

[[library.scenes.emitters]]
sounds = ["11111111-1111-4111-8111-111111111111"]
min_interval_ms = 5000
max_interval_ms = 9000
volume = 0.5
volume_jitter = 0.25
pan_jitter = 0.5
max_overlap = 3

[[library.playlists]]
id = "44444444-4444-4444-8444-444444444444"
name = "Travel"
items = ["22222222-2222-4222-8222-222222222222"]
shuffle = true
repeat = "one"
crossfade_ms = 4000
bus = "music"

[[hotkeys]]
key = "Ctrl+1"
action = { kind = "scene", target = "33333333-3333-4333-8333-333333333333" }

[[hotkeys]]
key = "Cmd+K"
[hotkeys.action]
kind = "stop_all"
"##;
    let config: Config = toml::from_str(text).expect("parse");

    let crow = &config.library.sounds[0];
    assert_eq!(crow.tags, ["bird", "forest"]);
    assert_eq!(crow.gain_db, -3.5);
    assert_eq!(crow.bus, Bus::Sfx);
    assert!(crow.oneshot_stinger);

    let scene = &config.library.scenes[0];
    assert_eq!(scene.curve, FadeCurve::Linear);
    assert!(!scene.stop_others);
    assert_eq!(scene.fade_in_ms, 3000);
    assert_eq!(scene.items[0].start, SceneStart::Manual);
    assert_eq!(scene.items[0].delay_ms, 250);
    assert_eq!(scene.emitters[0].max_overlap, 3);
    assert_eq!(config.library.playlists[0].repeat, RepeatMode::One);
    assert!(config.library.playlists[0].shuffle);

    assert_eq!(config.server.gm_token.as_deref(), Some("gmsecret"));
    assert_eq!(config.audio.duck_buses, vec![Bus::Music]);
    assert!(config.gui.global_hotkeys);
    assert_eq!(config.hotkeys.len(), 2);
    assert_eq!(
        config.hotkeys[0].action,
        HotkeyAction::Scene {
            target: "33333333-3333-4333-8333-333333333333".into()
        }
    );
    assert_eq!(config.hotkeys[1].action, HotkeyAction::StopAll);

    // Serialize and parse again: identical.
    let out = toml::to_string_pretty(&config).expect("serialize");
    let back: Config = toml::from_str(&out).expect("reparse");
    assert_eq!(back.library, config.library);
    assert_eq!(back.hotkeys, config.hotkeys);
    assert_eq!(back.audio.duck_buses, config.audio.duck_buses);
    assert_eq!(back.server.gm_token, config.server.gm_token);

    // Through the library model too (hotkeys included).
    let lib = Library::from_full_config(&config);
    assert!(lib.validate().is_empty(), "{:?}", lib.validate());
    let mut rebuilt = Config::default();
    lib.write_config(&mut rebuilt);
    assert_eq!(rebuilt.library, config.library);
    assert_eq!(rebuilt.hotkeys, config.hotkeys);
}

#[test]
fn every_hotkey_action_round_trips_through_toml() {
    let target = "33333333-3333-4333-8333-333333333333".to_string();
    let actions = vec![
        HotkeyAction::Sound {
            target: target.clone(),
        },
        HotkeyAction::Scene {
            target: target.clone(),
        },
        HotkeyAction::Playlist { target },
        HotkeyAction::StopAll,
        HotkeyAction::StopScene,
        HotkeyAction::MasterUp,
        HotkeyAction::MasterDown,
        HotkeyAction::MasterMute,
        HotkeyAction::DuckToggle,
        HotkeyAction::DuckHold,
        HotkeyAction::NextTrack,
        HotkeyAction::PrevTrack,
        HotkeyAction::PauseMusic,
    ];
    let config = Config {
        hotkeys: actions
            .iter()
            .enumerate()
            .map(|(i, action)| HotkeyConfig {
                key: format!("F{}", i + 1),
                action: action.clone(),
            })
            .collect(),
        ..Config::default()
    };
    let text = toml::to_string_pretty(&config).expect("serialize");
    let back: Config = toml::from_str(&text).expect("parse");
    assert_eq!(back.hotkeys, config.hotkeys, "{text}");
    // The serialized tag names are the documented snake_case ones.
    assert!(text.contains("kind = \"stop_all\""));
    assert!(text.contains("kind = \"duck_hold\""));
    assert!(text.contains("kind = \"pause_music\""));
}

#[test]
fn gm_token_is_generated_once_and_empty_means_local_only() {
    let mut config = Config::default();
    assert_eq!(config.server.gm_token, None);

    assert!(config.server.ensure_gm_token(), "first call generates");
    let token = config.server.gm_token.clone().expect("token");
    assert!(token.len() >= 12);
    assert_ne!(Some(&token), config.server.access_token.as_ref());
    assert!(!config.server.ensure_gm_token(), "second call keeps it");
    assert_eq!(config.server.gm_token.as_deref(), Some(token.as_str()));

    // An explicit empty value is a decision, not a gap: never regenerated.
    config.server.gm_token = Some(String::new());
    assert!(!config.server.ensure_gm_token());
    assert_eq!(config.server.gm_token.as_deref(), Some(""));

    // And it survives a save/load as "".
    let dir = temp_dir("gm");
    let path = dir.join("config.toml");
    config.save_to(&path).expect("save");
    let back = Config::load_from(&path).expect("load");
    assert_eq!(back.server.gm_token.as_deref(), Some(""));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hotkey_strings_are_canonicalized_for_comparison() {
    assert_eq!(canonical_hotkey("ctrl+1").as_deref(), Some("Ctrl+1"));
    assert_eq!(
        canonical_hotkey("Shift + control + a").as_deref(),
        Some("Ctrl+Shift+A")
    );
    assert_eq!(canonical_hotkey("cmd+k").as_deref(), Some("Cmd+K"));
    assert_eq!(canonical_hotkey("Command+K").as_deref(), Some("Cmd+K"));
    assert_eq!(canonical_hotkey("f5").as_deref(), Some("F5"));
    assert_eq!(canonical_hotkey("alt+SPACE").as_deref(), Some("Alt+Space"));
    assert_eq!(canonical_hotkey("esc").as_deref(), Some("Escape"));
    assert_eq!(canonical_hotkey("Ctrl+Ctrl+1").as_deref(), Some("Ctrl+1"));

    for bad in ["", "+", "Ctrl", "Ctrl+", "Ctrl+A+B", "Ctrl++1", "  "] {
        assert_eq!(canonical_hotkey(bad), None, "{bad:?} must be rejected");
    }
}
