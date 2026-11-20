//! Integration tests for the pure sound-library / group model and its config
//! round-trip.
//!
//! No audio device, no real files: every path here is a fake Windows path.

use std::path::{Path, PathBuf};

use soundshared::config::{Config, GroupConfig, LibraryConfig, SoundConfigEntry};
use soundshared::gui::groups::{GroupFilter, Library, MAX_GROUP_NAME_LEN};

const HIT: &str = r"C:\fake\hit.wav";
const WIND: &str = r"C:\fake\wind.ogg";
const LASER: &str = r"C:\fake\laser.flac";

fn p(path: &str) -> PathBuf {
    PathBuf::from(path)
}

fn library_with_sounds() -> Library {
    let mut lib = Library::new();
    assert!(lib.insert_sound(Path::new(HIT), "Hit", 1.0, None));
    assert!(lib.insert_sound(Path::new(WIND), "Wind", 0.5, None));
    assert!(lib.insert_sound(Path::new(LASER), "Laser", 0.25, None));
    lib
}

// ---------------------------------------------------------------- groups ----

#[test]
fn add_group_returns_id_and_preserves_order() {
    let mut lib = Library::new();
    let combat = lib.add_group("Combat").expect("combat");
    let ambience = lib.add_group("Ambience").expect("ambience");

    assert_ne!(combat, ambience);
    assert_eq!(lib.groups().len(), 2);
    assert_eq!(lib.groups()[0].name, "Combat");
    assert_eq!(lib.groups()[1].name, "Ambience");
    assert_eq!(lib.group(combat).map(|g| g.name.as_str()), Some("Combat"));
    assert_eq!(lib.group_name(ambience), Some("Ambience"));
}

#[test]
fn add_group_trims_whitespace() {
    let mut lib = Library::new();
    let id = lib.add_group("  Combat  ").expect("group");
    assert_eq!(lib.group_name(id), Some("Combat"));
}

#[test]
fn add_group_rejects_empty_and_whitespace_only_names() {
    let mut lib = Library::new();
    assert_eq!(lib.add_group(""), None);
    assert_eq!(lib.add_group("   "), None);
    assert_eq!(lib.add_group("\t\n"), None);
    assert!(lib.groups().is_empty());
}

#[test]
fn add_group_rejects_too_long_names() {
    let mut lib = Library::new();
    let max = "a".repeat(MAX_GROUP_NAME_LEN);
    assert!(lib.add_group(&max).is_some());
    let over = "b".repeat(MAX_GROUP_NAME_LEN + 1);
    assert_eq!(lib.add_group(&over), None);
    // The limit counts characters, not bytes: 32 multibyte chars are allowed.
    let multibyte = "д".repeat(MAX_GROUP_NAME_LEN);
    assert!(lib.add_group(&multibyte).is_some());
}

#[test]
fn add_group_rejects_duplicate_names_case_insensitively() {
    let mut lib = Library::new();
    assert!(lib.add_group("Combat").is_some());
    assert_eq!(lib.add_group("Combat"), None);
    assert_eq!(lib.add_group("COMBAT"), None);
    assert_eq!(lib.add_group(" combat "), None);
    assert_eq!(lib.groups().len(), 1);
}

#[test]
fn rename_group_updates_name() {
    let mut lib = Library::new();
    let id = lib.add_group("Combat").expect("group");
    assert!(lib.rename_group(id, "Fight"));
    assert_eq!(lib.group_name(id), Some("Fight"));
}

#[test]
fn rename_group_rejects_unknown_id() {
    let mut lib = Library::new();
    assert!(!lib.rename_group(uuid::Uuid::new_v4(), "Nothing"));
    assert!(lib.is_empty());
}

#[test]
fn rename_group_rejects_invalid_and_duplicate_names() {
    let mut lib = Library::new();
    let combat = lib.add_group("Combat").expect("combat");
    let ambience = lib.add_group("Ambience").expect("ambience");

    assert!(!lib.rename_group(combat, ""));
    assert!(!lib.rename_group(combat, "   "));
    assert!(!lib.rename_group(combat, &"x".repeat(MAX_GROUP_NAME_LEN + 1)));
    assert!(!lib.rename_group(combat, "Ambience"));
    assert!(!lib.rename_group(combat, "AMBIENCE"));
    // Failed renames leave the name untouched.
    assert_eq!(lib.group_name(combat), Some("Combat"));
    assert_eq!(lib.group_name(ambience), Some("Ambience"));
}

#[test]
fn rename_group_to_own_name_is_allowed() {
    let mut lib = Library::new();
    let id = lib.add_group("Combat").expect("group");
    assert!(lib.rename_group(id, "Combat"));
    assert!(lib.rename_group(id, "combat"));
    assert_eq!(lib.group_name(id), Some("combat"));
}

#[test]
fn remove_group_ungroups_its_members_and_keeps_other_groups() {
    let mut lib = library_with_sounds();
    let combat = lib.add_group("Combat").expect("combat");
    let ambience = lib.add_group("Ambience").expect("ambience");
    assert!(lib.assign(Path::new(HIT), Some(combat)));
    assert!(lib.assign(Path::new(LASER), Some(combat)));
    assert!(lib.assign(Path::new(WIND), Some(ambience)));

    assert!(lib.remove_group(combat));
    assert!(lib.group(combat).is_none());
    assert_eq!(lib.groups().len(), 1);

    assert_eq!(lib.sound(Path::new(HIT)).unwrap().group, None);
    assert_eq!(lib.sound(Path::new(LASER)).unwrap().group, None);
    assert_eq!(lib.sound(Path::new(WIND)).unwrap().group, Some(ambience));

    assert_eq!(lib.count_for(GroupFilter::Ungrouped), 2);
    assert_eq!(lib.count_for(GroupFilter::Group(ambience)), 1);
    assert_eq!(lib.count_for(GroupFilter::Group(combat)), 0);
    assert_eq!(lib.count_for(GroupFilter::All), 3);
}

#[test]
fn remove_group_rejects_unknown_id() {
    let mut lib = Library::new();
    assert!(!lib.remove_group(uuid::Uuid::new_v4()));
}

// ---------------------------------------------------------------- sounds ----

#[test]
fn insert_sound_adds_and_reports_new() {
    let mut lib = Library::new();
    assert!(lib.is_empty());
    assert_eq!(lib.len(), 0);
    assert!(lib.insert_sound(Path::new(HIT), "Hit", 1.0, None));
    assert!(!lib.is_empty());
    assert_eq!(lib.len(), 1);

    let sound = lib.sound(Path::new(HIT)).expect("sound");
    assert_eq!(sound.name, "Hit");
    assert_eq!(sound.volume, 1.0);
    assert_eq!(sound.group, None);
    assert_eq!(lib.sounds()[0].path, p(HIT));
}

#[test]
fn insert_sound_duplicate_is_ignored_and_leaves_fields_untouched() {
    let mut lib = Library::new();
    let combat = lib.add_group("Combat").expect("combat");
    assert!(lib.insert_sound(Path::new(HIT), "Hit", 1.0, None));
    assert!(lib.assign(Path::new(HIT), Some(combat)));
    assert!(lib.set_volume(Path::new(HIT), 0.3));

    // Second insert with different data must change nothing.
    assert!(!lib.insert_sound(Path::new(HIT), "Renamed", 0.9, None));
    let sound = lib.sound(Path::new(HIT)).expect("sound");
    assert_eq!(sound.name, "Hit");
    assert_eq!(sound.volume, 0.3);
    assert_eq!(sound.group, Some(combat));
    assert_eq!(lib.len(), 1);
}

#[test]
fn insert_sound_with_unknown_group_becomes_ungrouped() {
    let mut lib = Library::new();
    let ghost = uuid::Uuid::new_v4();
    assert!(lib.insert_sound(Path::new(HIT), "Hit", 1.0, Some(ghost)));
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().group, None);
}

#[test]
fn insert_sound_keeps_insertion_order() {
    let lib = library_with_sounds();
    let order: Vec<&str> = lib.sounds().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(order, ["Hit", "Wind", "Laser"]);
}

#[test]
fn remove_sound_removes_and_keeps_lookups_consistent() {
    let mut lib = library_with_sounds();
    assert!(lib.remove_sound(Path::new(WIND)));
    assert!(lib.sound(Path::new(WIND)).is_none());
    assert_eq!(lib.len(), 2);
    // Lookups still resolve correctly after an index shift.
    assert_eq!(lib.sound(Path::new(LASER)).unwrap().name, "Laser");
    assert!(lib.assign(Path::new(LASER), None));
    assert!(!lib.remove_sound(Path::new(WIND)));
}

#[test]
fn assign_and_clear_group() {
    let mut lib = library_with_sounds();
    let combat = lib.add_group("Combat").expect("combat");

    assert!(lib.assign(Path::new(HIT), Some(combat)));
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().group, Some(combat));

    assert!(lib.assign(Path::new(HIT), None));
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().group, None);
}

#[test]
fn assign_unknown_group_clears_membership() {
    let mut lib = library_with_sounds();
    let combat = lib.add_group("Combat").expect("combat");
    assert!(lib.assign(Path::new(HIT), Some(combat)));

    assert!(lib.assign(Path::new(HIT), Some(uuid::Uuid::new_v4())));
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().group, None);
}

#[test]
fn assign_unknown_sound_returns_false() {
    let mut lib = library_with_sounds();
    let combat = lib.add_group("Combat").expect("combat");
    assert!(!lib.assign(Path::new(r"C:\fake\missing.wav"), Some(combat)));
}

#[test]
fn volume_is_clamped_to_unit_range() {
    let mut lib = library_with_sounds();

    assert!(lib.set_volume(Path::new(HIT), 2.5));
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().volume, 1.0);

    assert!(lib.set_volume(Path::new(HIT), -1.0));
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().volume, 0.0);

    assert!(lib.set_volume(Path::new(HIT), 0.42));
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().volume, 0.42);

    assert!(lib.set_volume(Path::new(HIT), f32::NAN));
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().volume, 0.0);
}

#[test]
fn set_volume_unknown_sound_returns_false() {
    let mut lib = library_with_sounds();
    assert!(!lib.set_volume(Path::new(r"C:\fake\missing.wav"), 0.5));
}

#[test]
fn paths_are_matched_exactly_not_canonicalized() {
    let mut lib = Library::new();
    assert!(lib.insert_sound(Path::new(HIT), "Hit", 1.0, None));
    // A different casing / relative form is a different entry by contract.
    assert!(!lib.assign(Path::new(r"C:\FAKE\hit.wav"), None));
    assert!(!lib.set_volume(Path::new("hit.wav"), 0.5));
    assert!(lib.sound(Path::new(HIT)).is_some());
}

// --------------------------------------------------------------- filters ----

#[test]
fn filter_counts_and_lists() {
    let mut lib = library_with_sounds();
    let combat = lib.add_group("Combat").expect("combat");
    let ambience = lib.add_group("Ambience").expect("ambience");
    assert!(lib.assign(Path::new(HIT), Some(combat)));
    assert!(lib.assign(Path::new(LASER), Some(combat)));
    assert!(lib.assign(Path::new(WIND), Some(ambience)));

    assert_eq!(lib.count_for(GroupFilter::All), 3);
    assert_eq!(lib.count_for(GroupFilter::Ungrouped), 0);
    assert_eq!(lib.count_for(GroupFilter::Group(combat)), 2);
    assert_eq!(lib.count_for(GroupFilter::Group(ambience)), 1);
    assert_eq!(lib.count_for(GroupFilter::Group(uuid::Uuid::new_v4())), 0);

    let all: Vec<&str> = lib
        .sounds_for(GroupFilter::All)
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(all, ["Hit", "Wind", "Laser"]);

    let combat_sounds: Vec<&str> = lib
        .sounds_for(GroupFilter::Group(combat))
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(combat_sounds, ["Hit", "Laser"]);
}

#[test]
fn ungrouped_filter_lists_only_group_less_sounds() {
    let mut lib = library_with_sounds();
    let combat = lib.add_group("Combat").expect("combat");
    assert!(lib.assign(Path::new(WIND), Some(combat)));

    let ungrouped: Vec<&str> = lib
        .sounds_for(GroupFilter::Ungrouped)
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(ungrouped, ["Hit", "Laser"]);
    assert_eq!(lib.count_for(GroupFilter::Ungrouped), 2);
}

#[test]
fn matches_reports_membership() {
    let mut lib = library_with_sounds();
    let combat = lib.add_group("Combat").expect("combat");
    assert!(lib.assign(Path::new(HIT), Some(combat)));

    assert!(lib.matches(Path::new(HIT), GroupFilter::All));
    assert!(lib.matches(Path::new(HIT), GroupFilter::Group(combat)));
    assert!(!lib.matches(Path::new(HIT), GroupFilter::Ungrouped));
    assert!(!lib.matches(Path::new(WIND), GroupFilter::Group(combat)));
    // Unregistered paths match nothing, not even All.
    assert!(!lib.matches(Path::new(r"C:\fake\missing.wav"), GroupFilter::All));
}

#[test]
fn default_filter_is_all() {
    assert_eq!(GroupFilter::default(), GroupFilter::All);
}

// ----------------------------------------------------------------- config ----

#[test]
fn config_roundtrip_preserves_order_and_membership() {
    let mut lib = library_with_sounds();
    let _ambience = lib.add_group("Ambience").expect("ambience");
    let combat = lib.add_group("Combat").expect("combat");
    assert!(lib.assign(Path::new(HIT), Some(combat)));
    assert!(lib.assign(Path::new(LASER), Some(combat)));
    assert!(lib.set_volume(Path::new(WIND), 0.35));

    let config = lib.to_config();
    assert_eq!(config.groups.len(), 2);
    assert_eq!(config.groups[0].name, "Ambience");
    assert_eq!(config.groups[1].name, "Combat");
    assert_eq!(config.sounds.len(), 3);
    assert_eq!(config.sounds[0].path, HIT);

    // Through real TOML text, not just struct copies.
    let text = toml::to_string_pretty(&config).expect("serialize");
    let parsed: LibraryConfig = toml::from_str(&text).expect("deserialize");
    let restored = Library::from_config(&parsed);

    assert_eq!(restored.groups(), lib.groups());
    assert_eq!(restored.sounds(), lib.sounds());
    assert_eq!(restored.len(), 3);
    assert_eq!(restored.count_for(GroupFilter::Group(combat)), 2);
    assert_eq!(restored.count_for(GroupFilter::Ungrouped), 1);
    assert_eq!(restored.sound(Path::new(WIND)).unwrap().volume, 0.35);
}

#[test]
fn full_config_roundtrip_through_toml() {
    let mut lib = Library::new();
    let id = lib.add_group("Combat").expect("combat");
    assert!(lib.insert_sound(Path::new(HIT), "Hit", 0.8, Some(id)));
    // The repeat flag is part of the persisted library, so it must survive the
    // round trip along with volume and group membership.
    assert!(lib.set_looped(Path::new(HIT), true));

    let config = Config {
        library: lib.to_config(),
        ..Config::default()
    };

    let text = toml::to_string_pretty(&config).expect("serialize");
    let back: Config = toml::from_str(&text).expect("deserialize");
    assert_eq!(back.library, config.library);

    let restored = Library::from_config(&back.library);
    assert_eq!(restored.sounds(), lib.sounds());
    assert_eq!(restored.groups(), lib.groups());
}

#[test]
fn from_config_repairs_dangling_group_reference() {
    // The group list is empty, but the sound claims membership.
    let config = LibraryConfig {
        groups: vec![],
        sounds: vec![SoundConfigEntry {
            path: HIT.to_string(),
            name: "Hit".to_string(),
            volume: 1.0,
            looped: false,
            group: Some(uuid::Uuid::new_v4().to_string()),
            ..SoundConfigEntry::default()
        }],
        ..LibraryConfig::default()
    };

    let lib = Library::from_config(&config);
    assert_eq!(lib.len(), 1);
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().group, None);
    assert_eq!(lib.count_for(GroupFilter::Ungrouped), 1);
}

#[test]
fn from_config_drops_invalid_and_duplicate_group_ids() {
    let good = uuid::Uuid::new_v4();
    let config = LibraryConfig {
        groups: vec![
            GroupConfig {
                id: "not-a-uuid".to_string(),
                name: "Bad".to_string(),
            },
            GroupConfig {
                id: good.to_string(),
                name: "Combat".to_string(),
            },
            // Same id twice: the duplicate must be dropped, not shadow the first.
            GroupConfig {
                id: good.to_string(),
                name: "Duplicate".to_string(),
            },
            GroupConfig {
                id: "".to_string(),
                name: "EmptyId".to_string(),
            },
        ],
        sounds: vec![],
        ..LibraryConfig::default()
    };

    let lib = Library::from_config(&config);
    assert_eq!(lib.groups().len(), 1);
    assert_eq!(lib.group_name(good), Some("Combat"));
}

#[test]
fn from_config_drops_empty_paths_and_duplicate_paths() {
    let config = LibraryConfig {
        groups: vec![],
        sounds: vec![
            SoundConfigEntry {
                path: "".to_string(),
                name: "Empty".to_string(),
                volume: 1.0,
                looped: false,
                group: None,
                ..SoundConfigEntry::default()
            },
            SoundConfigEntry {
                path: HIT.to_string(),
                name: "Hit".to_string(),
                volume: 1.0,
                looped: false,
                group: None,
                ..SoundConfigEntry::default()
            },
            SoundConfigEntry {
                path: HIT.to_string(),
                name: "Hit again".to_string(),
                volume: 0.2,
                looped: false,
                group: None,
                ..SoundConfigEntry::default()
            },
        ],
        ..LibraryConfig::default()
    };

    let lib = Library::from_config(&config);
    assert_eq!(lib.len(), 1);
    assert_eq!(lib.sound(Path::new(HIT)).unwrap().name, "Hit");
}

#[test]
fn from_config_of_default_is_empty() {
    let lib = Library::from_config(&LibraryConfig::default());
    assert!(lib.is_empty());
    assert!(lib.groups().is_empty());
    assert_eq!(lib.count_for(GroupFilter::All), 0);
}

#[test]
fn to_config_of_empty_library_is_default() {
    let lib = Library::new();
    assert_eq!(lib.to_config(), LibraryConfig::default());
}
