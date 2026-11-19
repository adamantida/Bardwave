//! `[gui] ui_scale`: old configs keep loading at 100 %, bad values are repaired
//! on read, and a chosen scale survives a save and a load.
//!
//! Every file lives in a unique directory under the system temp dir; the real
//! config of the user is never touched.

use std::path::{Path, PathBuf};

use soundshared::config::{clamp_ui_scale, Config, UI_SCALE_MAX, UI_SCALE_MIN};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bardwave-uiscale-{}-{}-{}",
        name,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn load(dir: &Path, text: &str) -> Config {
    let path = dir.join("config.toml");
    std::fs::write(&path, text).expect("write config");
    Config::load_from(&path).expect("config loads")
}

#[test]
fn a_config_without_the_key_is_at_one_hundred_percent() {
    let dir = temp_dir("missing");
    let config = load(&dir, "[gui]\nreduce_motion = true\n");
    assert_eq!(config.gui.scale(), 1.0);
    assert!(config.gui.reduce_motion, "the other gui keys are kept");

    let no_gui = load(&dir, "[server]\nport = 8080\n");
    assert_eq!(no_gui.gui.scale(), 1.0);
    assert_eq!(Config::default().gui.scale(), 1.0);
}

#[test]
fn a_chosen_scale_is_read_and_saved() {
    let dir = temp_dir("roundtrip");
    let mut config = load(&dir, "[gui]\nui_scale = 1.3\n");
    assert!((config.gui.scale() - 1.3).abs() < 1e-6);

    config.gui.ui_scale = 1.45;
    let path = dir.join("saved.toml");
    config.save_to(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read back");
    assert!(text.contains("ui_scale"), "{text}");
    let again = Config::load_from(&path).expect("reload");
    assert!((again.gui.scale() - 1.45).abs() < 1e-6);
}

#[test]
fn the_default_scale_is_not_written() {
    let dir = temp_dir("default");
    let path = dir.join("saved.toml");
    Config::default().save_to(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read back");
    assert!(
        !text.contains("ui_scale"),
        "a stock config stays free of new keys: {text}"
    );
}

#[test]
fn nonsense_is_repaired_when_read() {
    let dir = temp_dir("nonsense");
    for (value, expected) in [
        ("9.0", UI_SCALE_MAX),
        ("0.1", UI_SCALE_MIN),
        ("-2.0", UI_SCALE_MIN),
        ("nan", 1.0),
        ("inf", 1.0),
    ] {
        let config = load(&dir, &format!("[gui]\nui_scale = {value}\n"));
        assert_eq!(config.gui.scale(), expected, "ui_scale = {value}");
    }
    assert_eq!(clamp_ui_scale(1.6), 1.6);
    assert_eq!(clamp_ui_scale(0.8), 0.8);
}

#[test]
fn the_range_matches_the_documented_limits() {
    assert_eq!(UI_SCALE_MIN, 0.8);
    assert_eq!(UI_SCALE_MAX, 1.6);
}
