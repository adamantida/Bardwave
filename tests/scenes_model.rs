//! Integration tests for the scene / playlist / hotkey / tag part of the pure
//! library model, and its config round trip.
//!
//! No audio device, no real files: every path here is a fake path.

use std::path::Path;

use soundshared::config::{
    Bus, Config, EmitterConfig, FadeCurve, HotkeyAction, LibraryConfig, PlaylistConfig, RepeatMode,
    SceneConfig, SceneItemConfig, SceneStart, SoundConfigEntry,
};
use soundshared::gui::groups::{
    Emitter, HotkeyError, Library, LibraryWarning, SceneItem, SoundId, MAX_NAME_LEN,
};

const RAIN: &str = "/fake/rain.ogg";
const CROW: &str = "/fake/crow.wav";
const CROW2: &str = "/fake/crow2.wav";
const THEME: &str = "/fake/theme.flac";

fn lib() -> Library {
    let mut lib = Library::new();
    for (path, name) in [
        (RAIN, "Rain"),
        (CROW, "Crow"),
        (CROW2, "Crow 2"),
        (THEME, "Theme"),
    ] {
        assert!(lib.insert_sound(Path::new(path), name, 1.0, None));
    }
    lib
}

fn id(lib: &Library, path: &str) -> SoundId {
    lib.id_of(Path::new(path)).expect("registered sound")
}

// ------------------------------------------------------------------- ids ----

#[test]
fn sounds_have_unique_stable_ids_with_an_id_path_index() {
    let mut lib = lib();
    let rain = id(&lib, RAIN);
    let crow = id(&lib, CROW);
    assert_ne!(rain, crow);
    assert_eq!(lib.path_of(rain), Some(Path::new(RAIN)));
    assert_eq!(lib.sound_by_id(crow).unwrap().name, "Crow");

    // Removing an earlier sound shifts storage positions, not ids.
    assert!(lib.remove_sound(Path::new(RAIN)));
    assert_eq!(lib.sound_by_id(rain), None);
    assert_eq!(lib.path_of(crow), Some(Path::new(CROW)));
    assert_eq!(lib.id_of(Path::new(CROW)), Some(crow));
}

#[test]
fn relocating_a_sound_keeps_its_id_and_references() {
    let mut lib = lib();
    let rain = id(&lib, RAIN);
    let scene = lib.add_scene("Storm").unwrap();
    assert!(lib.set_item(scene, SceneItem::new(rain)));

    assert!(lib.relocate_sound(Path::new(RAIN), Path::new("/moved/rain.ogg")));
    assert_eq!(lib.id_of(Path::new("/moved/rain.ogg")), Some(rain));
    assert!(lib.sound(Path::new(RAIN)).is_none());
    assert_eq!(lib.scene(scene).unwrap().items[0].sound, rain);
    assert!(lib.validate().is_empty());

    // Cannot relocate onto an occupied path or from an unknown one.
    assert!(!lib.relocate_sound(Path::new("/moved/rain.ogg"), Path::new(CROW)));
    assert!(!lib.relocate_sound(Path::new("/nope"), Path::new("/x")));
}

#[test]
fn insert_with_id_keeps_the_id_and_refuses_duplicates() {
    let mut lib = Library::new();
    let wanted = uuid::Uuid::new_v4();
    assert!(lib.insert_sound_with_id(wanted, Path::new(RAIN), "Rain", 1.0, None));
    assert_eq!(lib.id_of(Path::new(RAIN)), Some(wanted));
    assert!(!lib.insert_sound_with_id(wanted, Path::new(CROW), "Other", 1.0, None));
    assert!(!lib.insert_sound_with_id(
        uuid::Uuid::new_v4(),
        Path::new(RAIN),
        "Same path",
        1.0,
        None
    ));
    assert_eq!(lib.len(), 1);
}

// ------------------------------------------------------------- migration ----

#[test]
fn from_config_gives_missing_and_duplicate_ids_fresh_uuids() {
    let shared = uuid::Uuid::new_v4().to_string();
    let config = LibraryConfig {
        sounds: vec![
            SoundConfigEntry {
                path: RAIN.into(),
                ..Default::default()
            },
            SoundConfigEntry {
                id: shared.clone(),
                path: CROW.into(),
                ..Default::default()
            },
            SoundConfigEntry {
                id: shared.clone(),
                path: CROW2.into(),
                ..Default::default()
            },
            SoundConfigEntry {
                id: "garbage".into(),
                path: THEME.into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let lib = Library::from_config(&config);
    assert_eq!(lib.len(), 4);
    let ids: std::collections::HashSet<_> = lib.sounds().iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), 4, "every sound ends up with its own id");
    // The first holder of a valid id keeps it.
    assert_eq!(
        lib.id_of(Path::new(CROW)).map(|i| i.to_string()),
        Some(shared)
    );
}

#[test]
fn ensure_ids_reports_whether_it_changed_anything() {
    let mut config = LibraryConfig {
        sounds: vec![SoundConfigEntry {
            path: RAIN.into(),
            ..Default::default()
        }],
        scenes: vec![SceneConfig {
            name: "No id".into(),
            ..Default::default()
        }],
        playlists: vec![PlaylistConfig::default()],
        ..Default::default()
    };
    assert!(config.ensure_ids());
    assert!(config.sounds[0].id.len() == 36);
    assert!(config.scenes[0].id.len() == 36);
    assert!(config.playlists[0].id.len() == 36);
    let snapshot = config.clone();
    assert!(!config.ensure_ids(), "second pass finds nothing to fix");
    assert_eq!(config, snapshot);
}

// ---------------------------------------------------------------- scenes ----

#[test]
fn scene_crud_add_rename_reorder_remove() {
    let mut lib = Library::new();
    let tavern = lib.add_scene("  Tavern ").expect("tavern");
    let forest = lib.add_scene("Forest").expect("forest");
    let cave = lib.add_scene("Cave").expect("cave");

    assert_eq!(lib.scene(tavern).unwrap().name, "Tavern", "trimmed");
    let defaults = lib.scene(tavern).unwrap();
    assert_eq!((defaults.fade_in_ms, defaults.fade_out_ms), (2000, 2000));
    assert_eq!(defaults.curve, FadeCurve::EqualPower);
    assert!(defaults.stop_others);

    // Invalid and duplicate names.
    assert_eq!(lib.add_scene("   "), None);
    assert_eq!(lib.add_scene("tavern"), None, "case-insensitive duplicate");
    assert_eq!(lib.add_scene(&"x".repeat(MAX_NAME_LEN + 1)), None);
    assert!(lib.add_scene(&"x".repeat(MAX_NAME_LEN)).is_some());

    assert!(lib.rename_scene(forest, "Deep Forest"));
    assert!(!lib.rename_scene(forest, "CAVE"), "taken by another scene");
    assert!(lib.rename_scene(forest, "deep forest"), "own name is fine");
    assert!(!lib.rename_scene(uuid::Uuid::new_v4(), "Ghost"));

    assert!(lib.reorder_scene(cave, 0));
    let order: Vec<_> = lib.scenes().iter().map(|s| s.id).collect();
    assert_eq!(&order[..3], &[cave, tavern, forest]);
    assert!(lib.reorder_scene(cave, 999), "clamped to the end");
    assert_eq!(lib.scenes().last().unwrap().id, cave);
    assert!(!lib.reorder_scene(uuid::Uuid::new_v4(), 0));

    assert!(lib.remove_scene(tavern));
    assert!(!lib.remove_scene(tavern));
    assert!(lib.scene(tavern).is_none());
}

#[test]
fn scene_settings_are_validated() {
    let mut lib = lib();
    let scene = lib.add_scene("S").unwrap();

    assert!(lib.set_scene_color(scene, Some("#a04a4a")));
    assert!(lib.set_scene_color(scene, Some("#abc")));
    assert!(!lib.set_scene_color(scene, Some("red")));
    assert!(!lib.set_scene_color(scene, Some("#12345")));
    assert_eq!(lib.scene(scene).unwrap().color.as_deref(), Some("#abc"));
    assert!(lib.set_scene_color(scene, None));
    assert_eq!(lib.scene(scene).unwrap().color, None);

    assert!(lib.set_scene_fades(scene, 500, 4000));
    assert!(lib.set_scene_curve(scene, FadeCurve::Linear));
    assert!(lib.set_scene_stop_others(scene, false));
    let s = lib.scene(scene).unwrap();
    assert_eq!((s.fade_in_ms, s.fade_out_ms), (500, 4000));
    assert_eq!(s.curve, FadeCurve::Linear);
    assert!(!s.stop_others);

    // Playlist must exist.
    assert!(!lib.set_scene_playlist(scene, Some(uuid::Uuid::new_v4())));
    let playlist = lib.add_playlist("Music").unwrap();
    assert!(lib.set_scene_playlist(scene, Some(playlist)));
    assert_eq!(lib.scene(scene).unwrap().playlist, Some(playlist));
    assert!(lib.set_scene_playlist(scene, None));
}

#[test]
fn set_item_inserts_replaces_clamps_and_rejects_unknown_sounds() {
    let mut lib = lib();
    let rain = id(&lib, RAIN);
    let crow = id(&lib, CROW);
    let scene = lib.add_scene("Storm").unwrap();

    assert!(lib.set_item(scene, SceneItem::new(rain)));
    assert!(lib.set_item(scene, SceneItem::new(crow)));
    // Same sound again replaces the item in place (no duplicates).
    assert!(lib.set_item(
        scene,
        SceneItem {
            volume: 7.0, // clamped
            looped: true,
            start: SceneStart::Manual,
            delay_ms: 1500,
            ..SceneItem::new(rain)
        }
    ));
    let items = &lib.scene(scene).unwrap().items;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].sound, rain, "position kept");
    assert_eq!(items[0].volume, 1.0);
    assert!(items[0].looped);
    assert_eq!(items[0].start, SceneStart::Manual);
    assert_eq!(items[0].delay_ms, 1500);

    assert!(!lib.set_item(scene, SceneItem::new(uuid::Uuid::new_v4())));
    assert!(!lib.set_item(uuid::Uuid::new_v4(), SceneItem::new(rain)));

    assert!(lib.reorder_item(scene, crow, 0));
    assert_eq!(lib.scene(scene).unwrap().items[0].sound, crow);

    assert!(lib.remove_item(scene, rain));
    assert!(!lib.remove_item(scene, rain), "already gone");
    assert_eq!(lib.scene(scene).unwrap().items.len(), 1);
}

#[test]
fn set_emitters_sanitizes_and_is_atomic() {
    let mut lib = lib();
    let crow = id(&lib, CROW);
    let crow2 = id(&lib, CROW2);
    let scene = lib.add_scene("Forest").unwrap();

    let wild = Emitter {
        sounds: vec![crow, crow2, crow], // duplicate collapsed
        min_interval_ms: 9000,
        max_interval_ms: 3000, // reversed
        volume: 3.0,
        volume_jitter: -1.0,
        pan_jitter: f32::NAN,
        max_overlap: 0,
    };
    assert!(lib.set_emitters(scene, vec![wild]));
    let e = &lib.scene(scene).unwrap().emitters[0];
    assert_eq!(e.sounds, vec![crow, crow2]);
    assert_eq!((e.min_interval_ms, e.max_interval_ms), (3000, 9000));
    assert_eq!(e.volume, 1.0);
    assert_eq!(e.volume_jitter, 0.0);
    assert_eq!(e.pan_jitter, 0.0);
    assert_eq!(e.max_overlap, 1);

    // A bad emitter refuses the whole call and leaves the old list in place.
    let before = lib.scene(scene).unwrap().emitters.clone();
    let ghost = Emitter::new(vec![uuid::Uuid::new_v4()]);
    assert!(!lib.set_emitters(scene, vec![Emitter::new(vec![crow]), ghost]));
    assert!(!lib.set_emitters(scene, vec![Emitter::new(vec![])]));
    assert!(!lib.set_emitters(uuid::Uuid::new_v4(), vec![]));
    assert_eq!(lib.scene(scene).unwrap().emitters, before);

    // Clearing is allowed.
    assert!(lib.set_emitters(scene, vec![]));
    assert!(lib.scene(scene).unwrap().emitters.is_empty());
}

// ------------------------------------------------------------- playlists ----

#[test]
fn playlist_crud_and_track_editing() {
    let mut lib = lib();
    let rain = id(&lib, RAIN);
    let theme = id(&lib, THEME);
    let travel = lib.add_playlist("Travel").unwrap();
    let battle = lib.add_playlist("Battle").unwrap();

    let p = lib.playlist(travel).unwrap();
    assert_eq!(p.repeat, RepeatMode::All);
    assert_eq!(p.bus, Bus::Music);
    assert_eq!(p.crossfade_ms, 2000);
    assert!(!p.shuffle);

    assert_eq!(lib.add_playlist("travel"), None);
    assert!(lib.rename_playlist(battle, "Boss fight"));
    assert!(!lib.rename_playlist(battle, "Travel"));

    assert!(lib.add_playlist_item(travel, theme, None));
    assert!(lib.add_playlist_item(travel, rain, Some(0)));
    assert!(!lib.add_playlist_item(travel, uuid::Uuid::new_v4(), None));
    assert_eq!(lib.playlist(travel).unwrap().items, vec![rain, theme]);
    assert!(lib.remove_playlist_item(travel, 0));
    assert!(!lib.remove_playlist_item(travel, 5));
    assert_eq!(lib.playlist(travel).unwrap().items, vec![theme]);

    assert!(lib.set_playlist_items(travel, vec![theme, rain, theme]));
    assert!(!lib.set_playlist_items(travel, vec![theme, uuid::Uuid::new_v4()]));
    assert_eq!(lib.playlist(travel).unwrap().items.len(), 3, "unchanged");

    assert!(lib.set_playlist_mode(travel, true, RepeatMode::Off));
    assert!(lib.set_playlist_crossfade(travel, 0));
    assert!(lib.set_playlist_bus(travel, Bus::Ambience));
    let p = lib.playlist(travel).unwrap();
    assert!(p.shuffle && p.repeat == RepeatMode::Off);
    assert_eq!((p.crossfade_ms, p.bus), (0, Bus::Ambience));

    assert!(lib.reorder_playlist(battle, 0));
    assert_eq!(lib.playlists()[0].id, battle);
    assert!(lib.remove_playlist(battle));
    assert!(!lib.remove_playlist(battle));
}

#[test]
fn removing_a_playlist_detaches_it_from_scenes_and_hotkeys() {
    let mut lib = lib();
    let playlist = lib.add_playlist("Travel").unwrap();
    let scene = lib.add_scene("Road").unwrap();
    assert!(lib.set_scene_playlist(scene, Some(playlist)));
    lib.add_hotkey(
        "F2",
        HotkeyAction::Playlist {
            target: playlist.to_string(),
        },
    )
    .unwrap();
    lib.add_hotkey("F3", HotkeyAction::NextTrack).unwrap();

    assert!(lib.remove_playlist(playlist));
    assert_eq!(lib.scene(scene).unwrap().playlist, None);
    assert_eq!(lib.hotkeys().len(), 1, "only the untargeted binding stays");
    assert!(lib.validate().is_empty());
}

// ------------------------------------------------- cleanup on sound removal ----

#[test]
fn removing_a_sound_cleans_scenes_emitters_playlists_and_hotkeys() {
    let mut lib = lib();
    let rain = id(&lib, RAIN);
    let crow = id(&lib, CROW);
    let crow2 = id(&lib, CROW2);
    let theme = id(&lib, THEME);

    let a = lib.add_scene("A").unwrap();
    let b = lib.add_scene("B").unwrap();
    assert!(lib.set_item(a, SceneItem::new(crow)));
    assert!(lib.set_item(a, SceneItem::new(rain)));
    assert!(lib.set_item(b, SceneItem::new(crow)));
    // Emitter 0 keeps crow2 after the removal, emitter 1 has only crow.
    assert!(lib.set_emitters(
        a,
        vec![Emitter::new(vec![crow, crow2]), Emitter::new(vec![crow])]
    ));
    let list = lib.add_playlist("List").unwrap();
    assert!(lib.set_playlist_items(list, vec![theme, crow, rain, crow]));
    lib.add_hotkey(
        "Ctrl+1",
        HotkeyAction::Sound {
            target: crow.to_string(),
        },
    )
    .unwrap();
    lib.add_hotkey(
        "Ctrl+2",
        HotkeyAction::Sound {
            target: rain.to_string(),
        },
    )
    .unwrap();
    lib.add_hotkey(
        "Ctrl+3",
        HotkeyAction::Scene {
            target: a.to_string(),
        },
    )
    .unwrap();

    let report = lib.remove_sound_report(Path::new(CROW)).expect("known");
    assert_eq!(report.scene_items, 2);
    assert_eq!(report.emitter_refs, 2);
    assert_eq!(report.emitters_dropped, 1);
    assert_eq!(report.playlist_items, 2);
    assert_eq!(report.hotkeys, 1);

    assert_eq!(lib.scene(a).unwrap().items.len(), 1);
    assert!(lib.scene(b).unwrap().items.is_empty());
    assert_eq!(lib.scene(a).unwrap().emitters.len(), 1);
    assert_eq!(lib.scene(a).unwrap().emitters[0].sounds, vec![crow2]);
    assert_eq!(lib.playlist(list).unwrap().items, vec![theme, rain]);
    assert_eq!(lib.hotkeys().len(), 2);
    assert!(lib.find_hotkey("Ctrl+1").is_none());
    assert!(lib.find_hotkey("Ctrl+2").is_some());
    assert!(lib.find_hotkey("Ctrl+3").is_some(), "scene hotkey stays");
    assert!(lib.validate().is_empty());

    assert_eq!(lib.remove_sound_report(Path::new(CROW)), None);
    assert!(!lib.remove_sound(Path::new(CROW)));
}

#[test]
fn removing_a_scene_removes_its_hotkeys() {
    let mut lib = lib();
    let scene = lib.add_scene("A").unwrap();
    lib.add_hotkey(
        "Ctrl+1",
        HotkeyAction::Scene {
            target: scene.to_string(),
        },
    )
    .unwrap();
    assert!(lib.remove_scene(scene));
    assert!(lib.hotkeys().is_empty());
}

// ---------------------------------------------------------------- hotkeys ----

#[test]
fn hotkey_crud_validates_key_and_target() {
    let mut lib = lib();
    let rain = id(&lib, RAIN);
    let scene = lib.add_scene("A").unwrap();

    let index = lib
        .add_hotkey(
            "ctrl+shift+1",
            HotkeyAction::Scene {
                target: scene.to_string().to_uppercase(),
            },
        )
        .expect("added");
    assert_eq!(index, 0);
    let stored = &lib.hotkeys()[0];
    assert_eq!(stored.key, "Ctrl+Shift+1", "stored canonically");
    assert_eq!(
        stored.action,
        HotkeyAction::Scene {
            target: scene.to_string()
        },
        "target re-spelled lower-case"
    );

    assert_eq!(
        lib.add_hotkey("Ctrl+", HotkeyAction::StopAll),
        Err(HotkeyError::InvalidKey("Ctrl+".into()))
    );
    let ghost = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        lib.add_hotkey(
            "F1",
            HotkeyAction::Sound {
                target: ghost.clone()
            }
        ),
        Err(HotkeyError::UnknownTarget(ghost))
    );
    // A scene id is not a sound id.
    assert!(matches!(
        lib.add_hotkey(
            "F1",
            HotkeyAction::Sound {
                target: scene.to_string()
            }
        ),
        Err(HotkeyError::UnknownTarget(_))
    ));

    assert_eq!(
        lib.set_hotkey(
            0,
            "F9",
            HotkeyAction::Sound {
                target: rain.to_string()
            }
        ),
        Ok(())
    );
    assert_eq!(lib.hotkeys()[0].key, "F9");
    assert_eq!(
        lib.set_hotkey(4, "F9", HotkeyAction::StopAll),
        Err(HotkeyError::NoSuchHotkey(4))
    );
    assert!(lib.remove_hotkey(0));
    assert!(!lib.remove_hotkey(0));
}

#[test]
fn hotkey_conflicts_are_reported_by_validate() {
    let mut lib = lib();
    lib.add_hotkey("Ctrl+1", HotkeyAction::StopAll).unwrap();
    lib.add_hotkey("F5", HotkeyAction::MasterUp).unwrap();
    // Same combination spelled differently: still a conflict.
    lib.add_hotkey("control+1", HotkeyAction::MasterDown)
        .unwrap();
    assert_eq!(lib.find_hotkey("CTRL+1"), Some(0));

    assert_eq!(lib.hotkey_conflicts(), vec![("Ctrl+1".into(), vec![0, 2])]);
    assert_eq!(
        lib.validate(),
        vec![LibraryWarning::HotkeyConflict {
            key: "Ctrl+1".into(),
            indices: vec![0, 2]
        }]
    );

    assert!(lib.remove_hotkey(2));
    assert!(lib.validate().is_empty());
}

#[test]
fn modifier_order_does_not_hide_conflicts() {
    let mut lib = lib();
    lib.add_hotkey("Shift+Ctrl+A", HotkeyAction::StopAll)
        .unwrap();
    lib.add_hotkey("ctrl+shift+a", HotkeyAction::StopScene)
        .unwrap();
    lib.add_hotkey("Ctrl+A", HotkeyAction::MasterMute).unwrap();
    assert_eq!(lib.hotkey_conflicts().len(), 1);
    assert_eq!(lib.hotkey_conflicts()[0].1, vec![0, 1]);
}

#[test]
fn validate_reports_dangling_references_from_a_damaged_config() {
    let ghost_sound = uuid::Uuid::new_v4();
    let ghost_playlist = uuid::Uuid::new_v4();
    let real = uuid::Uuid::new_v4();
    let config = Config {
        library: LibraryConfig {
            sounds: vec![SoundConfigEntry {
                id: real.to_string(),
                path: RAIN.into(),
                ..Default::default()
            }],
            scenes: vec![SceneConfig {
                name: "Broken".into(),
                items: vec![
                    SceneItemConfig {
                        sound: real.to_string(),
                        volume: 1.0,
                        looped: false,
                        start: SceneStart::OnEnter,
                        delay_ms: 0,
                        pan: 0.0,
                    },
                    SceneItemConfig {
                        sound: ghost_sound.to_string(),
                        volume: 1.0,
                        looped: false,
                        start: SceneStart::OnEnter,
                        delay_ms: 0,
                        pan: 0.0,
                    },
                    // Not a UUID at all: cannot be represented, dropped on load.
                    SceneItemConfig {
                        sound: "nonsense".into(),
                        volume: 1.0,
                        looped: false,
                        start: SceneStart::OnEnter,
                        delay_ms: 0,
                        pan: 0.0,
                    },
                ],
                emitters: vec![
                    EmitterConfig {
                        sounds: vec![ghost_sound.to_string()],
                        ..Default::default()
                    },
                    EmitterConfig::default(),
                ],
                playlist: Some(ghost_playlist.to_string()),
                ..Default::default()
            }],
            playlists: vec![PlaylistConfig {
                name: "P".into(),
                items: vec![ghost_sound.to_string()],
                ..Default::default()
            }],
            ..Default::default()
        },
        hotkeys: vec![
            soundshared::config::HotkeyConfig {
                key: "F1".into(),
                action: HotkeyAction::Sound {
                    target: ghost_sound.to_string(),
                },
            },
            soundshared::config::HotkeyConfig {
                key: "Ctrl+".into(),
                action: HotkeyAction::StopAll,
            },
            soundshared::config::HotkeyConfig {
                key: "F2".into(),
                action: HotkeyAction::Scene {
                    target: "not-a-uuid".into(),
                },
            },
        ],
        ..Config::default()
    };

    let lib = Library::from_full_config(&config);
    // Dangling-but-valid ids are preserved, not silently repaired...
    assert_eq!(lib.scenes()[0].items.len(), 2);
    assert_eq!(lib.hotkeys().len(), 3);
    // ...and reported.
    let warnings = lib.validate();
    let has = |f: &dyn Fn(&LibraryWarning) -> bool| warnings.iter().any(f);
    assert!(has(
        &|w| matches!(w, LibraryWarning::SceneMissingSound { sound, .. } if *sound == ghost_sound)
    ));
    assert!(has(&|w| matches!(
        w,
        LibraryWarning::EmitterMissingSound { emitter: 0, .. }
    )));
    assert!(has(&|w| matches!(
        w,
        LibraryWarning::EmitterWithoutSounds { emitter: 1, .. }
    )));
    assert!(has(
        &|w| matches!(w, LibraryWarning::SceneMissingPlaylist { playlist, .. } if *playlist == ghost_playlist)
    ));
    assert!(has(&|w| matches!(
        w,
        LibraryWarning::PlaylistMissingSound { .. }
    )));
    assert!(has(&|w| matches!(
        w,
        LibraryWarning::HotkeyMissingTarget { index: 0, .. }
    )));
    assert!(has(&|w| matches!(
        w,
        LibraryWarning::HotkeyInvalidKey { index: 1, .. }
    )));
    assert!(has(&|w| matches!(
        w,
        LibraryWarning::HotkeyMissingTarget { index: 2, .. }
    )));
    assert_eq!(warnings.len(), 8, "{warnings:#?}");
    // Every warning renders as readable text.
    for warning in &warnings {
        assert!(!warning.to_string().is_empty());
    }

    // Saving keeps the dangling references (nothing is lost behind the user's back).
    let mut out = Config::default();
    lib.write_config(&mut out);
    assert_eq!(out.library.scenes[0].items.len(), 2);
    assert_eq!(out.hotkeys.len(), 3);
}

// ------------------------------------------------------------------ tags ----

#[test]
fn tags_add_remove_normalize_and_list() {
    let mut lib = lib();
    assert!(lib.add_tag(Path::new(RAIN), "  Weather "));
    assert!(lib.add_tag(Path::new(RAIN), "loud"));
    assert!(!lib.add_tag(Path::new(RAIN), "WEATHER"), "already there");
    assert!(!lib.add_tag(Path::new(RAIN), "   "), "empty");
    assert!(!lib.add_tag(Path::new(RAIN), &"t".repeat(33)), "too long");
    assert!(!lib.add_tag(Path::new("/nope"), "x"));
    assert!(lib.add_tag(Path::new(CROW), "bird"));
    assert!(lib.add_tag(Path::new(CROW), "loud"));

    assert_eq!(
        lib.sound(Path::new(RAIN)).unwrap().tags,
        vec!["weather", "loud"]
    );
    assert_eq!(lib.all_tags(), vec!["bird", "loud", "weather"]);
    let loud: Vec<_> = lib
        .sounds_with_tag("LOUD")
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(loud, vec!["Rain", "Crow"]);
    assert!(lib.sounds_with_tag("").is_empty());

    assert!(lib.remove_tag(Path::new(RAIN), " Loud"));
    assert!(!lib.remove_tag(Path::new(RAIN), "loud"));
    assert_eq!(lib.sounds_with_tag("loud").len(), 1);

    assert!(lib.set_tags(Path::new(CROW), &["A", "a", "b", ""]));
    assert_eq!(lib.sound(Path::new(CROW)).unwrap().tags, vec!["a", "b"]);
    assert!(!lib.set_tags(Path::new("/nope"), &["x"]));

    // Tags vanish from the global list when the last holder is removed.
    assert!(lib.remove_sound(Path::new(CROW)));
    assert_eq!(lib.all_tags(), vec!["weather"]);
}

#[test]
fn sound_bus_gain_and_stinger_flags() {
    let mut lib = lib();
    let rain = Path::new(RAIN);
    assert_eq!(lib.sound(rain).unwrap().bus, Bus::Ambience, "default bus");
    assert!(lib.set_bus(rain, Bus::Sfx));
    assert!(lib.set_oneshot_stinger(rain, true));
    assert!(lib.set_gain_db(rain, -6.0));
    let s = lib.sound(rain).unwrap();
    assert_eq!(
        (s.bus, s.oneshot_stinger, s.gain_db),
        (Bus::Sfx, true, -6.0)
    );

    assert!(lib.set_gain_db(rain, 500.0));
    assert_eq!(lib.sound(rain).unwrap().gain_db, 24.0);
    assert!(lib.set_gain_db(rain, f32::NAN));
    assert_eq!(lib.sound(rain).unwrap().gain_db, 0.0);
    assert!(!lib.set_bus(Path::new("/nope"), Bus::Music));
}

// ------------------------------------------------------------- round trip ----

fn rich_library() -> Library {
    let mut lib = lib();
    let nature = lib.add_group("Nature").unwrap();
    assert!(lib.assign(Path::new(RAIN), Some(nature)));
    assert!(lib.set_looped(Path::new(RAIN), true));
    assert!(lib.set_volume(Path::new(RAIN), 0.4));
    assert!(lib.add_tag(Path::new(RAIN), "weather"));
    assert!(lib.set_gain_db(Path::new(RAIN), -2.5));
    assert!(lib.set_bus(Path::new(CROW), Bus::Sfx));
    assert!(lib.set_oneshot_stinger(Path::new(CROW), true));
    assert!(lib.set_bus(Path::new(THEME), Bus::Music));

    let rain = id(&lib, RAIN);
    let crow = id(&lib, CROW);
    let crow2 = id(&lib, CROW2);
    let theme = id(&lib, THEME);

    let list = lib.add_playlist("Travel").unwrap();
    assert!(lib.set_playlist_items(list, vec![theme]));
    assert!(lib.set_playlist_mode(list, true, RepeatMode::One));
    assert!(lib.set_playlist_crossfade(list, 3500));

    let scene = lib.add_scene("Forest").unwrap();
    assert!(lib.set_scene_color(scene, Some("#336633")));
    assert!(lib.set_scene_fades(scene, 3000, 1500));
    assert!(lib.set_scene_curve(scene, FadeCurve::Linear));
    assert!(lib.set_scene_stop_others(scene, false));
    assert!(lib.set_scene_playlist(scene, Some(list)));
    assert!(lib.set_item(
        scene,
        SceneItem {
            volume: 0.35,
            looped: true,
            start: SceneStart::OnEnter,
            delay_ms: 100,
            ..SceneItem::new(rain)
        }
    ));
    assert!(lib.set_item(
        scene,
        SceneItem {
            start: SceneStart::Manual,
            ..SceneItem::new(crow)
        }
    ));
    assert!(lib.set_emitters(
        scene,
        vec![Emitter {
            sounds: vec![crow, crow2],
            min_interval_ms: 5000,
            max_interval_ms: 15000,
            volume: 0.5,
            volume_jitter: 0.25,
            pan_jitter: 0.5,
            max_overlap: 3,
        }]
    ));
    let empty = lib.add_scene("Empty").unwrap();
    let _ = empty;

    lib.add_hotkey(
        "Ctrl+1",
        HotkeyAction::Scene {
            target: scene.to_string(),
        },
    )
    .unwrap();
    lib.add_hotkey(
        "F1",
        HotkeyAction::Sound {
            target: crow.to_string(),
        },
    )
    .unwrap();
    lib.add_hotkey("Cmd+K", HotkeyAction::DuckToggle).unwrap();
    lib
}

#[test]
fn new_schema_round_trips_through_toml_without_loss() {
    let lib = rich_library();
    assert!(lib.validate().is_empty());

    let mut config = Config::default();
    lib.write_config(&mut config);
    let text = toml::to_string_pretty(&config).expect("serialize");
    let parsed: Config = toml::from_str(&text).expect("parse");
    assert_eq!(parsed.library, config.library);
    assert_eq!(parsed.hotkeys, config.hotkeys);

    let restored = Library::from_full_config(&parsed);
    assert_eq!(restored.sounds(), lib.sounds());
    assert_eq!(restored.groups(), lib.groups());
    assert_eq!(restored.scenes(), lib.scenes());
    assert_eq!(restored.playlists(), lib.playlists());
    assert_eq!(restored.hotkeys(), lib.hotkeys());
    // Stable under a second pass as well.
    assert_eq!(restored.to_config(), lib.to_config());
}

#[test]
fn new_schema_round_trips_through_a_file() {
    let dir = std::env::temp_dir().join(format!(
        "bardwave-scenes-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");

    let lib = rich_library();
    let mut config = Config::default();
    lib.write_config(&mut config);
    config.save_to(&path).expect("save");

    let loaded = Config::load_from(&path).expect("load");
    let restored = Library::from_full_config(&loaded);
    assert_eq!(restored.scenes(), lib.scenes());
    assert_eq!(restored.sounds(), lib.sounds(), "ids survive a restart");
    assert_eq!(restored.hotkeys(), lib.hotkeys());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn from_config_keeps_the_old_path_keyed_behavior() {
    // Library::from_config / to_config without hotkeys still works for callers
    // that only know LibraryConfig.
    let lib = rich_library();
    let restored = Library::from_config(&lib.to_config());
    assert_eq!(restored.scenes(), lib.scenes());
    assert!(restored.hotkeys().is_empty());
    assert_eq!(restored.to_config(), lib.to_config());
}

#[test]
fn damaged_scene_numbers_are_clamped_on_load() {
    let sound = uuid::Uuid::new_v4();
    let config = LibraryConfig {
        sounds: vec![SoundConfigEntry {
            id: sound.to_string(),
            path: RAIN.into(),
            gain_db: 900.0,
            tags: vec!["  A ".into(), "a".into(), "".into()],
            ..Default::default()
        }],
        scenes: vec![SceneConfig {
            name: "S".into(),
            color: Some("not a color".into()),
            items: vec![SceneItemConfig {
                sound: sound.to_string(),
                volume: 9.0,
                looped: false,
                start: SceneStart::OnEnter,
                delay_ms: 0,
                pan: 0.0,
            }],
            emitters: vec![EmitterConfig {
                sounds: vec![sound.to_string()],
                min_interval_ms: 10,
                max_interval_ms: 5,
                max_overlap: 0,
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let lib = Library::from_config(&config);
    assert_eq!(lib.sounds()[0].gain_db, 24.0);
    assert_eq!(lib.sounds()[0].tags, vec!["a"]);
    let scene = &lib.scenes()[0];
    assert_eq!(scene.color, None);
    assert_eq!(scene.items[0].volume, 1.0);
    assert_eq!(scene.emitters[0].min_interval_ms, 5);
    assert_eq!(scene.emitters[0].max_interval_ms, 10);
    assert_eq!(scene.emitters[0].max_overlap, 1);
}
