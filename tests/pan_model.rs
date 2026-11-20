//! Pan of library sounds and scene items: the config schema (old configs keep
//! loading, nothing is written for a centred sound, values are clamped), the
//! override rule, and the Director applying pans through a mock playback.
//!
//! No sound card, no real config, no network.

use std::path::Path;
use std::time::{Duration, Instant};

use soundshared::audio::director::mock::MockPlayback;
use soundshared::audio::director_pan::{resolve_pan, sanitize_pan};
use soundshared::audio::{CatalogSound, Director, DirectorCommand, Playback, SoundId};
use soundshared::config::SceneStart;
use soundshared::config::{Config, LibraryConfig, SceneConfig, SceneItemConfig, SoundConfigEntry};
use soundshared::gui::groups::{clamp_pan, Library, SceneItem};

// ------------------------------------------------------------ config ----

const OLD_CONFIG: &str = r#"
[[library.sounds]]
id = "5b3f0c1e-8a54-4d36-9d4a-1d9c6b2a7e01"
path = "/fake/rain.ogg"
name = "Rain"
volume = 0.5

[[library.scenes]]
id = "0f6d6b7a-3c1e-4b86-8f2a-7a9e5d4c3b21"
name = "Storm"

[[library.scenes.items]]
sound = "5b3f0c1e-8a54-4d36-9d4a-1d9c6b2a7e01"
volume = 0.7
"#;

fn parse(text: &str) -> Config {
    toml::from_str(text).expect("parse")
}

#[test]
fn old_configs_load_with_a_centred_pan_and_write_no_pan_key() {
    let config = parse(OLD_CONFIG);
    assert_eq!(config.library.sounds[0].pan, 0.0);
    assert_eq!(config.library.scenes[0].items[0].pan, 0.0);

    let lib = Library::from_config(&config.library);
    assert_eq!(lib.sounds()[0].pan, 0.0);
    assert_eq!(lib.scenes()[0].items[0].pan, 0.0);

    let out = toml::to_string_pretty(&Config {
        library: lib.to_config(),
        ..Config::default()
    })
    .expect("serialize");
    assert!(
        !out.contains("pan"),
        "a never-panned library stays as v1.4/1.5 wrote it:\n{out}"
    );
}

#[test]
fn pan_round_trips_through_toml_and_the_library_model() {
    let text = OLD_CONFIG
        .replace("volume = 0.5", "volume = 0.5\npan = -0.75")
        .replace("volume = 0.7", "volume = 0.7\npan = 0.4");
    let config = parse(&text);
    assert_eq!(config.library.sounds[0].pan, -0.75);
    assert_eq!(config.library.scenes[0].items[0].pan, 0.4);

    let lib = Library::from_config(&config.library);
    assert_eq!(lib.sounds()[0].pan, -0.75);
    assert_eq!(lib.scenes()[0].items[0].pan, 0.4);

    let saved = toml::to_string_pretty(&Config {
        library: lib.to_config(),
        ..Config::default()
    })
    .expect("serialize");
    let back = parse(&saved);
    assert_eq!(back.library.sounds[0].pan, -0.75);
    assert_eq!(back.library.scenes[0].items[0].pan, 0.4);
}

#[test]
fn out_of_range_pans_are_clamped_on_load() {
    let text = OLD_CONFIG
        .replace("volume = 0.5", "volume = 0.5\npan = -9.0")
        .replace("volume = 0.7", "volume = 0.7\npan = 3.5");
    let lib = Library::from_config(&parse(&text).library);
    assert_eq!(lib.sounds()[0].pan, -1.0);
    assert_eq!(lib.scenes()[0].items[0].pan, 1.0);

    let nan = OLD_CONFIG.replace("volume = 0.5", "volume = 0.5\npan = nan");
    let lib = Library::from_config(&parse(&nan).library);
    assert_eq!(lib.sounds()[0].pan, 0.0, "NaN folds to the centre");
}

#[test]
fn library_setters_clamp() {
    let mut lib = Library::new();
    let path = Path::new("/fake/wind.ogg");
    assert!(lib.insert_sound(path, "Wind", 1.0, None));
    assert!(lib.set_pan(path, 7.0));
    assert_eq!(lib.sound(path).unwrap().pan, 1.0);
    assert!(lib.set_pan(path, -0.3));
    assert_eq!(lib.sound(path).unwrap().pan, -0.3);
    assert!(lib.set_pan(path, f32::NAN));
    assert_eq!(lib.sound(path).unwrap().pan, 0.0);
    assert!(!lib.set_pan(Path::new("/fake/unknown.ogg"), 0.5));

    let id = lib.id_of(path).unwrap();
    let scene = lib.add_scene("Gale").unwrap();
    assert!(lib.set_item(
        scene,
        SceneItem {
            pan: -4.0,
            ..SceneItem::new(id)
        }
    ));
    assert_eq!(lib.scene(scene).unwrap().items[0].pan, -1.0);
    assert_eq!(
        SceneItem::new(id).pan,
        0.0,
        "a new item defers to the sound"
    );
}

#[test]
fn clamp_helpers_agree() {
    for value in [
        -5.0,
        -1.0,
        -0.25,
        0.0,
        0.5,
        1.0,
        9.0,
        f32::NAN,
        f32::INFINITY,
    ] {
        let a = clamp_pan(value);
        let b = sanitize_pan(value);
        assert!((-1.0..=1.0).contains(&a));
        assert!((-1.0..=1.0).contains(&b));
    }
    assert_eq!(sanitize_pan(f32::INFINITY), 0.0);
    assert_eq!(clamp_pan(f32::NEG_INFINITY), -1.0);
}

#[test]
fn a_scene_item_overrides_the_sound_pan_only_when_non_zero() {
    assert_eq!(resolve_pan(-0.5, None), -0.5);
    assert_eq!(resolve_pan(-0.5, Some(0.0)), -0.5);
    assert_eq!(resolve_pan(-0.5, Some(0.8)), 0.8);
    assert_eq!(resolve_pan(0.0, Some(-0.2)), -0.2);
    assert_eq!(resolve_pan(9.0, None), 1.0);
    assert_eq!(resolve_pan(0.0, Some(-9.0)), -1.0);
}

// ---------------------------------------------------------- Director ----

struct Rig {
    pb: MockPlayback,
    director: Director,
    rain: SoundId,
    bell: SoundId,
}

fn item(sound: &str, pan: f32, start: SceneStart, delay_ms: u32) -> SceneItemConfig {
    SceneItemConfig {
        sound: sound.into(),
        volume: 1.0,
        looped: false,
        start,
        delay_ms,
        pan,
    }
}

fn scene(id: &str, items: Vec<SceneItemConfig>) -> SceneConfig {
    SceneConfig {
        id: id.into(),
        name: id.into(),
        fade_in_ms: 0,
        fade_out_ms: 0,
        items,
        ..Default::default()
    }
}

/// `rain` (own pan -0.6) and `bell` (own pan 0.0, a stinger flagged one).
/// Scenes: `plain` uses the sounds' pans, `wide` overrides rain to +0.9,
/// `late` starts rain after a delay with an override, `manual` lists rain
/// as a hand-started item with an override.
fn rig() -> Rig {
    let pb = MockPlayback::new();
    let rain = pb.add_sound();
    let bell = pb.add_sound();
    let mut director = Director::new();
    let mut rain_entry = CatalogSound::new("rain", rain);
    rain_entry.pan = -0.6;
    let mut bell_entry = CatalogSound::new("bell", bell);
    bell_entry.stinger = true;
    director.sync_library(
        vec![
            scene("plain", vec![item("rain", 0.0, SceneStart::OnEnter, 0)]),
            scene("wide", vec![item("rain", 0.9, SceneStart::OnEnter, 0)]),
            scene("late", vec![item("rain", -0.3, SceneStart::OnEnter, 500)]),
            scene("manual", vec![item("rain", 0.7, SceneStart::Manual, 0)]),
            scene("empty", vec![]),
        ],
        vec![rain_entry, bell_entry],
    );
    Rig {
        pb,
        director,
        rain,
        bell,
    }
}

fn near(a: Option<f32>, b: f32) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-6)
}

#[test]
fn starting_a_scene_applies_the_sound_pan() {
    let mut r = rig();
    r.director.activate_scene(&r.pb, "plain").unwrap();
    assert!(near(r.pb.pan_of(r.rain), -0.6));
    assert!(r.pb.is_playing(r.rain));
}

#[test]
fn a_scene_item_overrides_the_sound_pan_and_switching_scenes_moves_it() {
    let mut r = rig();
    r.director.activate_scene(&r.pb, "wide").unwrap();
    assert!(near(r.pb.pan_of(r.rain), 0.9));

    // Rain keeps playing across the switch: it is retuned to the new scene's
    // pan, here the sound's own (the item's pan is centre).
    r.director.activate_scene(&r.pb, "plain").unwrap();
    assert!(near(r.pb.pan_of(r.rain), -0.6));
}

#[test]
fn a_delayed_start_is_panned_when_it_starts() {
    let mut r = rig();
    let t0 = Instant::now();
    r.director.activate_scene_at(&r.pb, "late", t0).unwrap();
    assert!(!r.pb.is_playing(r.rain));
    r.director.tick_at(&r.pb, t0 + Duration::from_millis(600));
    assert!(r.pb.is_playing(r.rain));
    assert!(near(r.pb.pan_of(r.rain), -0.3));
}

#[test]
fn a_manual_item_uses_the_scene_override_when_played_by_hand() {
    let mut r = rig();
    r.director.activate_scene(&r.pb, "manual").unwrap();
    assert!(
        !r.pb.is_playing(r.rain),
        "manual items wait for the operator"
    );
    r.director.play_sound(&r.pb, r.rain, false).unwrap();
    assert!(near(r.pb.pan_of(r.rain), 0.7));

    // Without the scene the sound's own pan is used again.
    r.director.activate_scene(&r.pb, "empty").unwrap();
    r.director.play_sound(&r.pb, r.rain, false).unwrap();
    assert!(near(r.pb.pan_of(r.rain), -0.6));
}

#[test]
fn a_stinger_plays_at_its_own_pan() {
    let mut r = rig();
    r.director
        .set_sound_pan(&r.pb, r.bell, 0.35)
        .expect("known sound");
    r.director.play_stinger(&r.pb, r.bell).unwrap();
    assert!(near(r.pb.pan_of(r.bell), 0.35));
}

#[test]
fn changing_the_pan_while_playing_calls_set_pan_at_once() {
    let mut r = rig();
    r.director.play_sound(&r.pb, r.rain, true).unwrap();
    assert!(near(r.pb.pan_of(r.rain), -0.6));

    r.director.set_sound_pan(&r.pb, r.rain, 0.5).unwrap();
    assert!(near(r.pb.pan_of(r.rain), 0.5));
    assert!(near(r.director.catalog_sound(r.rain).map(|e| e.pan), 0.5));

    // Out of range is clamped, not rejected; NaN is rejected.
    r.director.set_sound_pan(&r.pb, r.rain, 5.0).unwrap();
    assert!(near(r.pb.pan_of(r.rain), 1.0));
    assert!(r.director.set_sound_pan(&r.pb, r.rain, f32::NAN).is_err());
    assert!(near(r.pb.pan_of(r.rain), 1.0));

    // An unknown engine sound is an error, not a panic.
    assert!(r
        .director
        .set_sound_pan(&r.pb, SoundId::new(), 0.1)
        .is_err());
}

#[test]
fn the_scene_override_keeps_winning_over_a_live_sound_pan_change() {
    let mut r = rig();
    r.director.activate_scene(&r.pb, "wide").unwrap();
    r.director.set_sound_pan(&r.pb, r.rain, -1.0).unwrap();
    assert!(
        near(r.pb.pan_of(r.rain), 0.9),
        "the item's pan is what is heard in this scene"
    );
    r.director.activate_scene(&r.pb, "plain").unwrap();
    assert!(near(r.pb.pan_of(r.rain), -1.0), "now the sound's own pan");
}

#[test]
fn apply_pans_follows_a_library_edit_of_the_active_scene() {
    let mut r = rig();
    r.director.activate_scene(&r.pb, "wide").unwrap();
    assert!(near(r.pb.pan_of(r.rain), 0.9));

    // The scene editor saved another override: the library is synced into the
    // Director and the pans are re-applied.
    let mut rain_entry = CatalogSound::new("rain", r.rain);
    rain_entry.pan = -0.6;
    r.director.sync_library(
        vec![scene(
            "wide",
            vec![item("rain", -0.2, SceneStart::OnEnter, 0)],
        )],
        vec![rain_entry],
    );
    r.director.apply_pans(&r.pb);
    assert!(near(r.pb.pan_of(r.rain), -0.2));
}

#[test]
fn the_pan_command_goes_through_execute() {
    let mut r = rig();
    r.director
        .execute(&r.pb, DirectorCommand::SetPan(r.rain, -0.25))
        .unwrap();
    assert!(near(r.pb.pan_of(r.rain), -0.25));
}

#[test]
fn a_centred_library_leaves_every_sound_untouched() {
    let pb = MockPlayback::new();
    let sound = pb.add_sound();
    let mut director = Director::new();
    director.sync_library(
        vec![scene("s", vec![item("a", 0.0, SceneStart::OnEnter, 0)])],
        vec![CatalogSound::new("a", sound)],
    );
    director.activate_scene(&pb, "s").unwrap();
    assert!(near(pb.pan_of(sound), 0.0));
}

#[test]
fn the_catalog_entry_defaults_to_centre() {
    assert_eq!(CatalogSound::new("x", SoundId::new()).pan, 0.0);
    // The schema default is the same.
    assert_eq!(SoundConfigEntry::default().pan, 0.0);
    let _ = LibraryConfig::default();
}
