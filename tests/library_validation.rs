//! Silent-failure checks of the library model: hand-written ids are remapped
//! with their references, and scenes that would start nothing are reported
//! (once) instead of failing quietly.
//!
//! No audio device; paths are fake or live in a per-test temp directory.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use soundshared::config::{
    Config, EmitterConfig, HotkeyAction, HotkeyConfig, LibraryConfig, PlaylistConfig, SceneConfig,
    SceneItemConfig, SoundConfigEntry,
};
use soundshared::gui::groups::{
    Emitter, Library, SceneItem, SceneWarning, SceneWarningKind, WarningScope,
};

const RAIN: &str = "/fake/rain.ogg";
const CROW: &str = "/fake/crow.wav";

fn sound(id: &str, path: &str) -> SoundConfigEntry {
    SoundConfigEntry {
        id: id.to_string(),
        path: path.to_string(),
        name: path.to_string(),
        volume: 1.0,
        ..SoundConfigEntry::default()
    }
}

fn item(sound: &str) -> SceneItemConfig {
    SceneItemConfig {
        sound: sound.to_string(),
        volume: 1.0,
        looped: false,
        start: Default::default(),
        delay_ms: 0,
        pan: 0.0,
    }
}

/// A config whose ids are hand-written words instead of UUIDs.
fn hand_written() -> Config {
    Config {
        library: LibraryConfig {
            sounds: vec![sound("rain", RAIN), sound("crow", CROW)],
            scenes: vec![SceneConfig {
                id: "storm".into(),
                name: "Storm".into(),
                items: vec![item("rain")],
                emitters: vec![EmitterConfig {
                    sounds: vec!["crow".into(), " rain ".into()],
                    ..EmitterConfig::default()
                }],
                playlist: Some("music".into()),
                ..SceneConfig::default()
            }],
            playlists: vec![PlaylistConfig {
                id: "music".into(),
                name: "Music".into(),
                items: vec!["rain".into(), "crow".into()],
                ..PlaylistConfig::default()
            }],
            ..LibraryConfig::default()
        },
        hotkeys: vec![
            HotkeyConfig {
                key: "F1".into(),
                action: HotkeyAction::Sound {
                    target: "crow".into(),
                },
            },
            HotkeyConfig {
                key: "F2".into(),
                action: HotkeyAction::Scene {
                    target: "storm".into(),
                },
            },
            HotkeyConfig {
                key: "F3".into(),
                action: HotkeyAction::Playlist {
                    target: "music".into(),
                },
            },
        ],
        ..Config::default()
    }
}

fn is_uuid(text: &str) -> bool {
    uuid::Uuid::parse_str(text).is_ok()
}

/// A scratch directory that is removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("bardwave-validation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ------------------------------------------------------------------ remap ----

#[test]
fn ensure_ids_rewrites_references_to_replaced_ids() {
    let mut config = hand_written();
    assert!(config.ensure_ids());

    let lib = &config.library;
    let rain = lib.sounds[0].id.clone();
    let crow = lib.sounds[1].id.clone();
    assert!(is_uuid(&rain) && is_uuid(&crow) && rain != crow);
    let scene = &lib.scenes[0];
    let playlist = &lib.playlists[0];
    assert!(is_uuid(&scene.id) && is_uuid(&playlist.id));

    assert_eq!(scene.items[0].sound, rain);
    assert_eq!(scene.emitters[0].sounds, vec![crow.clone(), rain.clone()]);
    assert_eq!(scene.playlist.as_deref(), Some(playlist.id.as_str()));
    assert_eq!(playlist.items, vec![rain, crow.clone()]);

    let targets: Vec<&str> = config
        .hotkeys
        .iter()
        .map(|h| h.action.target().unwrap())
        .collect();
    assert_eq!(
        targets,
        vec![crow.as_str(), scene.id.as_str(), playlist.id.as_str()]
    );

    // Idempotent: the second run finds nothing to do.
    assert!(!config.ensure_ids());
}

#[test]
fn library_built_from_hand_written_ids_keeps_every_reference() {
    let lib = Library::from_full_config(&hand_written());

    let rain = lib.id_of(Path::new(RAIN)).unwrap();
    let crow = lib.id_of(Path::new(CROW)).unwrap();
    let scene = &lib.scenes()[0];
    assert_eq!(scene.items.len(), 1);
    assert_eq!(scene.items[0].sound, rain);
    assert_eq!(scene.emitters[0].sounds, vec![crow, rain]);
    assert_eq!(scene.playlist, Some(lib.playlists()[0].id));
    assert_eq!(lib.playlists()[0].items, vec![rain, crow]);
    assert_eq!(lib.hotkeys().len(), 3);

    // Nothing dangles: the checks that read the model find no missing id.
    let missing = lib.validate();
    assert!(missing.is_empty(), "{missing:?}");
    let unknown = lib
        .all_warnings_with(|_| true)
        .into_iter()
        .filter(|w| !matches!(w.kind, SceneWarningKind::MissingFile { .. }))
        .collect::<Vec<_>>();
    assert!(unknown.is_empty(), "{unknown:?}");
}

#[test]
fn from_config_alone_also_remaps_library_references() {
    let lib = Library::from_config(&hand_written().library);
    let rain = lib.id_of(Path::new(RAIN)).unwrap();
    assert_eq!(lib.scenes()[0].items[0].sound, rain);
    assert_eq!(lib.playlists()[0].items.len(), 2);
}

#[test]
fn load_from_file_remaps_and_persists_ids() {
    let dir = TempDir::new();
    let path = dir.0.join("config.toml");
    let sound_a = "[[library.sounds]]\nid = \"rain\"\npath = \"/fake/rain.ogg\"\nname = \"Rain\"\nvolume = 1.0\nlooped = false\n";
    let scene = "[[library.scenes]]\nid = \"storm\"\nname = \"Storm\"\n[[library.scenes.items]]\nsound = \"rain\"\n";
    let hotkey =
        "[[hotkeys]]\nkey = \"F2\"\n[hotkeys.action]\nkind = \"scene\"\ntarget = \"storm\"\n";
    std::fs::File::create(&path)
        .unwrap()
        .write_all(format!("{sound_a}\n{scene}\n{hotkey}").as_bytes())
        .unwrap();

    let loaded = Config::load_from(&path).unwrap();
    let rain = &loaded.library.sounds[0].id;
    assert!(is_uuid(rain));
    assert_eq!(&loaded.library.scenes[0].items[0].sound, rain);
    assert_eq!(
        loaded.hotkeys[0].action.target(),
        Some(loaded.library.scenes[0].id.as_str())
    );

    // Saved and reloaded: the new ids are stable, references still match.
    loaded.save_to(&path).unwrap();
    let again = Config::load_from(&path).unwrap();
    assert_eq!(again.library.sounds[0].id, *rain);
    assert_eq!(again.library.scenes[0].items[0].sound, *rain);
}

#[test]
fn unknown_non_uuid_references_are_still_dropped() {
    // "ghost" names no sound, so there is nothing to remap it to.
    let mut config = hand_written();
    config.library.scenes[0].items.push(item("ghost"));
    let lib = Library::from_full_config(&config);
    assert_eq!(lib.scenes()[0].items.len(), 1);
}

// --------------------------------------------------------------- warnings ----

/// A library with the sounds RAIN and CROW and nothing else.
fn base() -> (Library, uuid::Uuid, uuid::Uuid) {
    let mut lib = Library::new();
    assert!(lib.insert_sound(Path::new(RAIN), "Rain", 1.0, None));
    assert!(lib.insert_sound(Path::new(CROW), "Crow", 1.0, None));
    let rain = lib.id_of(Path::new(RAIN)).unwrap();
    let crow = lib.id_of(Path::new(CROW)).unwrap();
    (lib, rain, crow)
}

fn kinds(warnings: &[SceneWarning]) -> Vec<&SceneWarningKind> {
    warnings.iter().map(|w| &w.kind).collect()
}

#[test]
fn a_healthy_scene_has_no_warnings() {
    let (mut lib, rain, crow) = base();
    let scene = lib.add_scene("Storm").unwrap();
    assert!(lib.set_item(scene, SceneItem::new(rain)));
    assert!(lib.set_emitters(scene, vec![Emitter::new(vec![crow])]));
    assert!(lib.scene_warnings_with(scene, |_| true).is_empty());
    assert!(lib.all_warnings_with(|_| true).is_empty());
}

#[test]
fn unknown_scene_has_no_warnings() {
    let (lib, ..) = base();
    assert!(lib.scene_warnings(uuid::Uuid::new_v4()).is_empty());
}

#[test]
fn empty_scene_is_reported() {
    let (mut lib, ..) = base();
    let scene = lib.add_scene("Silence").unwrap();
    let warnings = lib.scene_warnings_with(scene, |_| true);
    assert_eq!(kinds(&warnings), vec![&SceneWarningKind::EmptyScene]);
    assert_eq!(warnings[0].scope, WarningScope::Scene(scene));
}

#[test]
fn a_scene_with_only_a_playlist_is_not_empty() {
    let (mut lib, ..) = base();
    let playlist = lib.add_playlist("Music").unwrap();
    let scene = lib.add_scene("Music only").unwrap();
    assert!(lib.set_scene_playlist(scene, Some(playlist)));
    assert!(lib.scene_warnings_with(scene, |_| true).is_empty());
}

#[test]
fn unknown_sound_in_an_item_and_in_an_emitter_is_reported() {
    let ghost = uuid::Uuid::new_v4();
    let (_, rain, _) = base();
    let config = LibraryConfig {
        sounds: vec![sound(&rain.to_string(), RAIN)],
        scenes: vec![SceneConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Haunted".into(),
            items: vec![item(&ghost.to_string())],
            emitters: vec![EmitterConfig {
                sounds: vec![rain.to_string(), ghost.to_string()],
                ..EmitterConfig::default()
            }],
            ..SceneConfig::default()
        }],
        ..LibraryConfig::default()
    };
    let lib = Library::from_config(&config);
    let scene = lib.scenes()[0].id;
    let warnings = lib.scene_warnings_with(scene, |_| true);
    assert_eq!(
        kinds(&warnings),
        vec![
            &SceneWarningKind::UnknownSound {
                sound: ghost,
                emitter: None
            },
            &SceneWarningKind::UnknownSound {
                sound: ghost,
                emitter: Some(0)
            },
        ]
    );
}

#[test]
fn missing_file_is_reported_with_its_path() {
    let (mut lib, rain, crow) = base();
    let scene = lib.add_scene("Storm").unwrap();
    assert!(lib.set_item(scene, SceneItem::new(rain)));
    assert!(lib.set_emitters(scene, vec![Emitter::new(vec![crow])]));

    // Only the crow file "exists".
    let warnings = lib.scene_warnings_with(scene, |p| p == Path::new(CROW));
    assert_eq!(
        kinds(&warnings),
        vec![&SceneWarningKind::MissingFile {
            sound: rain,
            path: PathBuf::from(RAIN),
            emitter: None
        }]
    );
    // The default check really looks at the disk: the fake paths do not exist.
    assert_eq!(lib.scene_warnings(scene).len(), 2);
}

#[test]
fn missing_file_is_found_on_a_real_disk() {
    let dir = TempDir::new();
    let present = dir.0.join("present.wav");
    std::fs::write(&present, b"x").unwrap();
    let gone = dir.0.join("gone.wav");

    let mut lib = Library::new();
    assert!(lib.insert_sound(&present, "Present", 1.0, None));
    assert!(lib.insert_sound(&gone, "Gone", 1.0, None));
    let scene = lib.add_scene("Disk").unwrap();
    assert!(lib.set_item(scene, SceneItem::new(lib.id_of(&present).unwrap())));
    assert!(lib.set_item(scene, SceneItem::new(lib.id_of(&gone).unwrap())));

    let warnings = lib.scene_warnings(scene);
    assert_eq!(warnings.len(), 1);
    assert!(matches!(
        &warnings[0].kind,
        SceneWarningKind::MissingFile { path, .. } if *path == gone
    ));
}

#[test]
fn emitter_without_sounds_is_reported() {
    let ghost = uuid::Uuid::new_v4().to_string();
    let config = LibraryConfig {
        scenes: vec![SceneConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Hollow".into(),
            items: vec![item(&ghost)],
            emitters: vec![EmitterConfig::default()],
            ..SceneConfig::default()
        }],
        ..LibraryConfig::default()
    };
    let lib = Library::from_config(&config);
    let warnings = lib.scene_warnings_with(lib.scenes()[0].id, |_| true);
    assert!(kinds(&warnings).contains(&&SceneWarningKind::EmitterWithoutSounds { emitter: 0 }));
}

#[test]
fn reversed_emitter_interval_is_swapped_on_load_and_never_reported_after() {
    let (_, rain, _) = base();
    let config = LibraryConfig {
        sounds: vec![sound(&rain.to_string(), RAIN)],
        scenes: vec![SceneConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Backwards".into(),
            emitters: vec![EmitterConfig {
                sounds: vec![rain.to_string()],
                min_interval_ms: 9000,
                max_interval_ms: 3000,
                ..EmitterConfig::default()
            }],
            ..SceneConfig::default()
        }],
        ..LibraryConfig::default()
    };
    let lib = Library::from_config(&config);
    let e = &lib.scenes()[0].emitters[0];
    assert_eq!((e.min_interval_ms, e.max_interval_ms), (3000, 9000));
    assert!(lib
        .scene_warnings_with(lib.scenes()[0].id, |_| true)
        .is_empty());
}

#[test]
fn unknown_playlist_is_reported() {
    let ghost = uuid::Uuid::new_v4();
    let (_, rain, _) = base();
    let config = LibraryConfig {
        sounds: vec![sound(&rain.to_string(), RAIN)],
        scenes: vec![SceneConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Lost music".into(),
            items: vec![item(&rain.to_string())],
            playlist: Some(ghost.to_string()),
            ..SceneConfig::default()
        }],
        ..LibraryConfig::default()
    };
    let lib = Library::from_config(&config);
    let warnings = lib.scene_warnings_with(lib.scenes()[0].id, |_| true);
    assert_eq!(
        kinds(&warnings),
        vec![&SceneWarningKind::UnknownPlaylist { playlist: ghost }]
    );
}

#[test]
fn all_warnings_cover_scenes_playlists_and_hotkeys() {
    let ghost = uuid::Uuid::new_v4();
    let (_, rain, _) = base();
    let config = Config {
        library: LibraryConfig {
            sounds: vec![sound(&rain.to_string(), RAIN)],
            playlists: vec![PlaylistConfig {
                id: uuid::Uuid::new_v4().to_string(),
                name: "P".into(),
                items: vec![ghost.to_string(), rain.to_string(), ghost.to_string()],
                ..PlaylistConfig::default()
            }],
            scenes: vec![SceneConfig {
                id: uuid::Uuid::new_v4().to_string(),
                name: "Empty".into(),
                ..SceneConfig::default()
            }],
            ..LibraryConfig::default()
        },
        hotkeys: vec![
            HotkeyConfig {
                key: "F1".into(),
                action: HotkeyAction::Sound {
                    target: ghost.to_string(),
                },
            },
            HotkeyConfig {
                key: "F2".into(),
                action: HotkeyAction::Scene {
                    target: "nonsense".into(),
                },
            },
            HotkeyConfig {
                key: "F3".into(),
                action: HotkeyAction::Scene {
                    target: ghost.to_string(),
                },
            },
        ],
        ..Config::default()
    };
    let lib = Library::from_full_config(&config);
    let all = lib.all_warnings_with(|_| false);

    let has = |scope: WarningScope, kind: &SceneWarningKind| {
        all.iter().any(|w| w.scope == scope && &w.kind == kind)
    };
    let playlist = lib.playlists()[0].id;
    assert!(has(
        WarningScope::Scene(lib.scenes()[0].id),
        &SceneWarningKind::EmptyScene
    ));
    // The repeated ghost track is reported once.
    assert_eq!(
        all.iter()
            .filter(|w| w.scope == WarningScope::Playlist(playlist)
                && matches!(w.kind, SceneWarningKind::UnknownSound { .. }))
            .count(),
        1
    );
    assert!(has(
        WarningScope::Playlist(playlist),
        &SceneWarningKind::MissingFile {
            sound: rain,
            path: PathBuf::from(RAIN),
            emitter: None
        }
    ));
    assert!(has(
        WarningScope::Hotkey(0),
        &SceneWarningKind::UnknownSound {
            sound: ghost,
            emitter: None
        }
    ));
    assert!(has(
        WarningScope::Hotkey(1),
        &SceneWarningKind::UnknownTarget {
            target: "nonsense".into()
        }
    ));
    assert!(has(
        WarningScope::Hotkey(2),
        &SceneWarningKind::UnknownScene { scene: ghost }
    ));
}

// ---------------------------------------------------------------- logging ----

/// Collects everything the `tracing` subscriber formats.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn broken_library() -> (Library, uuid::Uuid) {
    let config = LibraryConfig {
        scenes: vec![SceneConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Haunted".into(),
            items: vec![item(&uuid::Uuid::new_v4().to_string())],
            ..SceneConfig::default()
        }],
        ..LibraryConfig::default()
    };
    let lib = Library::from_config(&config);
    let scene = lib.scenes()[0].id;
    (lib, scene)
}

#[test]
fn problems_are_logged_once_each() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let (lib, scene) = broken_library();

        assert_eq!(lib.log_scene_warnings(scene), 1);
        // Activating the same scene again, or the load-time pass, adds nothing.
        for _ in 0..50 {
            assert_eq!(lib.log_scene_warnings(scene), 0);
        }
        assert_eq!(lib.log_all_warnings(), 0);
        assert_eq!(lib.log_all_warnings(), 0);

        // A clone shares the memory of what was reported.
        assert_eq!(lib.clone().log_all_warnings(), 0);
    });

    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert_eq!(text.matches("WARN").count(), 1, "{text}");
    assert!(text.contains("Haunted"), "{text}");
    assert!(text.contains("unknown sound"), "{text}");
}

#[test]
fn load_logs_once_and_a_new_problem_is_logged_later() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        // Loading a damaged config logs at once...
        let ghost = uuid::Uuid::new_v4().to_string();
        let config = Config {
            library: LibraryConfig {
                scenes: vec![SceneConfig {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: "Haunted".into(),
                    items: vec![item(&ghost)],
                    ..SceneConfig::default()
                }],
                ..LibraryConfig::default()
            },
            ..Config::default()
        };
        let mut lib = Library::from_full_config(&config);
        // ...so an explicit pass finds nothing new.
        assert_eq!(lib.log_all_warnings(), 0);

        // The problem is fixed; what remains is a different one.
        let scene = lib.scenes()[0].id;
        let ghost_id: uuid::Uuid = ghost.parse().unwrap();
        assert!(lib.remove_item(scene, ghost_id));
        assert!(lib.add_playlist("Anything").is_some());
        // Gone: now only "empty scene" is left, which is a new problem.
        assert_eq!(lib.log_all_warnings(), 1);
    });

    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert_eq!(text.matches("WARN").count(), 2, "{text}");
}

#[test]
fn describe_warning_names_the_scene() {
    let (lib, scene) = broken_library();
    let warnings = lib.scene_warnings_with(scene, |_| true);
    let text = lib.describe_warning(&warnings[0]);
    assert!(text.starts_with("scene \u{201c}Haunted\u{201d}"), "{text}");
}

#[test]
fn version_comes_from_cargo() {
    assert_eq!(soundshared::VERSION, env!("CARGO_PKG_VERSION"));
}
