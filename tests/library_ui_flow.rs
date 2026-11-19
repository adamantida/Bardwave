//! The library UI's logic against real files: scan a folder and summarize it,
//! find and repair missing files, export and import a bundle, and read the
//! error texts of hostile archives. No window, no real user config, no ports.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use soundshared::gui::groups::Library;
use soundshared::gui::library_ui::logic::{
    build_rows, check_missing, describe_bundle_error, import_report_lines, is_safety_error,
    remap_moves, summarize_scan, RowItem,
};
use soundshared::library_bundle::{
    export_bundle_with, import_bundle_with, inspect_bundle, BundleError, ConflictPolicy,
    ExportOptions, ImportLimits, ImportOptions,
};
use soundshared::library_query::LibraryQuery;
use soundshared::library_scan::{known_paths, start_scan, ScanOptions};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bardwave-ui-flow-{tag}-{}", uuid_like()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{nanos}-{}", std::process::id())
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn scan_summary_matches_apply_and_hides_duplicates() {
    let dir = temp_dir("scan");
    write(&dir.join("Tavern/crowd.wav"), "crowd-data");
    write(&dir.join("Tavern/fire.wav"), "fire-data");
    write(&dir.join("Combat/sword.ogg"), "sword-data");
    write(&dir.join("top.mp3"), "top-data");
    write(&dir.join("Combat/copy-of-top.mp3"), "top-data");
    write(&dir.join("notes.txt"), "ignored");

    let mut library = Library::new();
    library.add_group("tavern").unwrap();
    let report = start_scan(dir.clone(), known_paths(&library), ScanOptions::default())
        .unwrap()
        .wait()
        .unwrap();
    let summary = summarize_scan(&report, &library);
    assert_eq!(summary.new_sounds, 4, "the copy is a duplicate");
    assert_eq!(summary.skipped_duplicates, 1);
    assert_eq!(
        summary.reused_groups, 1,
        "Tavern matches the existing group"
    );
    assert_eq!(summary.new_groups, ["Combat"]);

    let groups_before = library.groups().len();
    let stats = report.apply_to_library(&mut library, true);
    assert_eq!(stats.added, 4);
    assert_eq!(
        library.groups().len(),
        groups_before + summary.new_groups.len()
    );

    // Scanning again finds nothing new: paths are known.
    let again = start_scan(dir.clone(), known_paths(&library), ScanOptions::default())
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(summarize_scan(&again, &library).new_sounds, 0);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn missing_files_are_found_relinked_and_remapped() {
    let old_dir = temp_dir("old");
    let new_dir = temp_dir("new");
    write(&old_dir.join("a.wav"), "aaa");
    write(&old_dir.join("sub/b.wav"), "bbb");
    let mut library = Library::new();
    for name in ["a.wav", "sub/b.wav"] {
        let path = old_dir.join(name);
        library.insert_sound(&path, name, 1.0, None);
    }
    let paths: Vec<PathBuf> = library.sounds().iter().map(|s| s.path.clone()).collect();

    // Everything present.
    let check = check_missing(&paths, &HashSet::new(), |p| p.is_file());
    assert!(check.missing.is_empty());

    // The folder moves away.
    fs::rename(&old_dir, new_dir.join("moved")).unwrap();
    let check = check_missing(&paths, &HashSet::new(), |p| p.is_file());
    assert_eq!(check.missing.len(), 2);

    let rows = build_rows(Vec::new(), &library, &check.missing, &LibraryQuery::new());
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| matches!(r, RowItem::Missing(_))));

    // Replace the prefix: the plan agrees with the result, and the files are
    // found again (reported for reload).
    let old_prefix = old_dir.to_string_lossy().into_owned();
    let new_prefix = new_dir.join("moved").to_string_lossy().into_owned();
    let moves = remap_moves(&library, &old_prefix, &new_prefix);
    assert_eq!(moves.len(), 2);
    assert_eq!(library.remap_prefix(&old_prefix, &new_prefix), 2);
    let new_paths: Vec<PathBuf> = library.sounds().iter().map(|s| s.path.clone()).collect();
    let reload: HashSet<PathBuf> = new_paths.iter().cloned().collect();
    let check = check_missing(&new_paths, &reload, |p| p.is_file());
    assert!(check.missing.is_empty());
    assert_eq!(check.reload.len(), 2);

    // Relink one sound by hand to a file that exists.
    let target = new_dir.join("elsewhere.wav");
    write(&target, "zzz");
    let first = new_paths[0].clone();
    let id = library.id_of(&first).unwrap();
    assert!(library.relink(&first, &target));
    assert_eq!(library.id_of(&target), Some(id), "relink keeps the id");
    fs::remove_dir_all(&new_dir).unwrap();
}

#[test]
fn bundle_round_trip_through_the_ui_paths() {
    let dir = temp_dir("bundle");
    write(&dir.join("src/Tavern/crowd.wav"), "crowd-data");
    write(&dir.join("src/Tavern/fire.wav"), "fire-data");
    let mut library = Library::new();
    let group = library.add_group("Tavern").unwrap();
    for name in ["crowd", "fire"] {
        let path = dir.join(format!("src/Tavern/{name}.wav"));
        library.insert_sound(&path, name, 0.5, Some(group));
        library.add_tag(&path, "tavern");
    }

    let zip = dir.join("set.zip");
    let report = export_bundle_with(
        &library,
        &zip,
        &ExportOptions {
            include_audio: true,
            name: "Pack".into(),
        },
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(report.audio_files, 2);

    let info = inspect_bundle(&zip, &ImportLimits::default()).unwrap();
    assert_eq!((info.sounds, info.audio_files), (2, 2));

    // Dry run first (what the dialog previews), then the real import.
    let mut target_library = Library::new();
    let mut options = ImportOptions::new(dir.join("imported"), ConflictPolicy::Skip);
    options.dry_run = true;
    let dry = import_bundle_with(
        &mut target_library,
        &zip,
        &options,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert!(target_library.is_empty(), "a dry run changes nothing");
    assert_eq!(import_report_lines(&dry)[0], "Sounds: 2 new");
    assert!(!dir.join("imported").exists());

    options.dry_run = false;
    import_bundle_with(
        &mut target_library,
        &zip,
        &options,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(target_library.len(), 2);
    assert_eq!(target_library.all_tags(), ["tavern"]);
    assert!(target_library
        .sounds()
        .iter()
        .all(|s| s.path.starts_with(dir.join("imported")) && s.path.is_file()));

    // Importing again with "keep mine" adds nothing.
    let again = import_bundle_with(
        &mut target_library,
        &zip,
        &options,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(again.sounds.skipped, 2);
    assert_eq!(target_library.len(), 2);
    fs::remove_dir_all(&dir).unwrap();
}

fn zip_with(path: &Path, entries: &[(&str, &str)]) {
    let file = fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    for (name, content) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn hostile_and_broken_archives_get_plain_messages() {
    let dir = temp_dir("hostile");

    // zip-slip entry.
    let slip = dir.join("slip.zip");
    zip_with(
        &slip,
        &[
            ("bundle.toml", "format = \"bardwave-bundle\"\n"),
            ("../evil.txt", "x"),
        ],
    );
    let error = inspect_bundle(&slip, &ImportLimits::default()).unwrap_err();
    assert!(matches!(error, BundleError::UnsafePath(_)), "{error:?}");
    assert!(is_safety_error(&error));
    let text = describe_bundle_error(&error);
    assert!(text.contains("Nothing was imported"), "{text}");
    assert!(text.contains("evil.txt"), "{text}");

    // Too many entries for tiny limits.
    let many = dir.join("many.zip");
    zip_with(
        &many,
        &[
            ("bundle.toml", ""),
            ("audio/a.wav", "a"),
            ("audio/b.wav", "b"),
        ],
    );
    let limits = ImportLimits {
        max_entries: 1,
        ..ImportLimits::default()
    };
    let error = inspect_bundle(&many, &limits).unwrap_err();
    assert!(is_safety_error(&error), "{error:?}");
    assert!(describe_bundle_error(&error).contains("safety"));

    // A zip that is not a bundle.
    let plain = dir.join("plain.zip");
    zip_with(&plain, &[("readme.txt", "hi")]);
    let error = inspect_bundle(&plain, &ImportLimits::default()).unwrap_err();
    assert!(describe_bundle_error(&error).contains("not a Bardwave bundle"));

    // Not a zip at all.
    let junk = dir.join("junk.zip");
    write(&junk, "this is not a zip file");
    let error = inspect_bundle(&junk, &ImportLimits::default()).unwrap_err();
    assert!(!is_safety_error(&error));
    assert!(describe_bundle_error(&error).contains("zip"));
    fs::remove_dir_all(&dir).unwrap();
}
