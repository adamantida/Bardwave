//! The presentation logic of the scene editor against the real library model
//! and the real `config.toml` round trip. No window, no audio device; every file
//! lives in a unique directory under the system temp dir.

use std::path::{Path, PathBuf};

use soundshared::config::{Config, FadeCurve, SceneStart};
use soundshared::gui::groups::{Emitter, Library};
use soundshared::gui::scenes::{
    add_sound_to_scene, apply_scene, duplicate_scene, AddToScene, SceneForm,
};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bardwave-gui-scenes-{}-{}-{}",
        name,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn library() -> Library {
    let mut lib = Library::new();
    for (path, name) in [
        ("/fake/rain.ogg", "Rain"),
        ("/fake/fire.ogg", "Fire"),
        ("/fake/crow.wav", "Crow"),
    ] {
        assert!(lib.insert_sound(Path::new(path), name, 0.7, None));
    }
    lib
}

/// Save the library through the real config writer and read it back.
fn through_disk(lib: &Library, name: &str) -> Library {
    let dir = temp_dir(name);
    let path = dir.join("config.toml");
    let config = Config {
        library: lib.to_config(),
        ..Config::default()
    };
    config.save_to(&path).expect("save");
    let loaded = Config::load_from(&path).expect("load");
    let _ = std::fs::remove_dir_all(&dir);
    Library::from_config(&loaded.library)
}

#[test]
fn an_edited_scene_survives_a_config_round_trip() {
    let mut lib = library();
    let rain = lib.id_of(Path::new("/fake/rain.ogg")).unwrap();
    let fire = lib.id_of(Path::new("/fake/fire.ogg")).unwrap();

    let mut form = SceneForm::blank(&lib);
    form.name = "Tavern".into();
    form.color = "#A04A4A".into();
    form.fade_in = "3 s".into();
    form.fade_out = "1500".into();
    form.curve = FadeCurve::Linear;
    form.stop_others = false;
    assert!(form.add_sound(&lib, rain));
    assert!(form.add_sound(&lib, fire));
    form.items[0].looped = true;
    form.items[0].volume = 0.4;
    form.items[1].manual = true;
    form.items[1].delay = "2500".into();

    let valid = form.validate(&lib).expect("valid");
    let id = apply_scene(&mut lib, None, &valid).expect("apply");

    let reloaded = through_disk(&lib, "roundtrip");
    let scene = reloaded.scene(id).expect("scene persisted under its id");
    assert_eq!(scene.name, "Tavern");
    assert_eq!(scene.color.as_deref(), Some("#a04a4a"));
    assert_eq!((scene.fade_in_ms, scene.fade_out_ms), (3000, 1500));
    assert_eq!(scene.curve, FadeCurve::Linear);
    assert!(!scene.stop_others);
    assert_eq!(scene.items.len(), 2);
    assert!(scene.items[0].looped);
    assert!((scene.items[0].volume - 0.4).abs() < 1e-6);
    assert_eq!(scene.items[1].start, SceneStart::Manual);
    assert_eq!(scene.items[1].delay_ms, 2500);

    // Re-opening the editor on the reloaded scene gives an equivalent form.
    let again = SceneForm::from_scene(&reloaded, scene);
    assert_eq!(again.validate(&reloaded).expect("valid"), valid);
}

#[test]
fn editing_in_the_gui_never_drops_emitters_or_the_playlist() {
    let mut lib = library();
    let crow = lib.id_of(Path::new("/fake/crow.wav")).unwrap();
    let rain = lib.id_of(Path::new("/fake/rain.ogg")).unwrap();
    let scene = lib.add_scene("Forest").unwrap();
    lib.set_item(scene, soundshared::gui::groups::SceneItem::new(rain));
    assert!(lib.set_emitters(scene, vec![Emitter::new(vec![crow])]));
    let music = lib.add_playlist("Music").unwrap();
    assert!(lib.set_scene_playlist(scene, Some(music)));

    let mut form = SceneForm::from_scene(&lib, lib.scene(scene).unwrap());
    form.name = "Deep forest".into();
    form.items[0].volume = 0.1;
    let valid = form.validate(&lib).unwrap();
    apply_scene(&mut lib, Some(scene), &valid).unwrap();

    let reloaded = through_disk(&lib, "keep-extras");
    let after = reloaded.scene(scene).unwrap();
    assert_eq!(after.name, "Deep forest");
    assert_eq!(after.emitters.len(), 1);
    assert_eq!(after.emitters[0].sounds, vec![crow]);
    assert_eq!(after.playlist, Some(music));

    // A duplicate carries them too.
    let mut lib = reloaded;
    let copy = duplicate_scene(&mut lib, scene).unwrap();
    assert_eq!(lib.scene(copy).unwrap().emitters.len(), 1);
    assert_eq!(lib.scene(copy).unwrap().playlist, Some(music));
}

#[test]
fn a_library_without_scenes_opens_the_editor_on_a_blank_form() {
    // What a v1.4.0 config looks like once loaded: sounds, no scenes.
    let lib = through_disk(&library(), "v140-like");
    assert!(lib.scenes().is_empty());
    let form = SceneForm::blank(&lib);
    assert_eq!(form.name, "Scene 1");
    assert_eq!(form.available_sounds(&lib).len(), 3);
    assert!(form.validate(&lib).is_ok());
}

#[test]
fn context_menu_add_goes_through_the_same_model_as_the_editor() {
    let mut lib = library();
    let crow = lib.id_of(Path::new("/fake/crow.wav")).unwrap();
    let scene = lib.add_scene("Night").unwrap();
    assert_eq!(add_sound_to_scene(&mut lib, scene, crow), AddToScene::Added);
    assert_eq!(
        add_sound_to_scene(&mut lib, scene, crow),
        AddToScene::AlreadyPresent
    );

    // The editor sees the sound as already part of the scene.
    let form = SceneForm::from_scene(&lib, lib.scene(scene).unwrap());
    assert_eq!(form.items.len(), 1);
    assert!(form
        .available_sounds(&lib)
        .iter()
        .all(|(id, _)| *id != crow));
}

#[test]
fn removing_a_library_sound_is_reflected_in_a_reopened_editor() {
    let mut lib = library();
    let rain = lib.id_of(Path::new("/fake/rain.ogg")).unwrap();
    let scene = lib.add_scene("Rainy").unwrap();
    assert_eq!(add_sound_to_scene(&mut lib, scene, rain), AddToScene::Added);
    lib.remove_sound(Path::new("/fake/rain.ogg"));
    let form = SceneForm::from_scene(&lib, lib.scene(scene).unwrap());
    assert!(
        form.items.is_empty(),
        "the library already cleaned the scene"
    );
}
