//! Integration tests for the library backend: bundle export/import (including
//! hostile archives), the recursive folder scan, composite search and the
//! missing-file helpers.
//!
//! Everything happens in unique temporary folders; no user config, no network
//! port, no audio device.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use soundshared::config::{Bus, RepeatMode};
use soundshared::gui::groups::{Emitter, GroupFilter, Library, SceneItem};
use soundshared::library_bundle::{
    export_bundle, export_bundle_with, import_bundle, import_bundle_with, inspect_bundle,
    BundleError, ConflictPolicy, ExportOptions, ImportLimits, ImportOptions,
};
use soundshared::library_query::LibraryQuery;
use soundshared::library_scan::{
    is_audio_file, known_paths, locate_files, scan_dir, start_scan, ScanEvent, ScanOptions,
};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

// ----------------------------------------------------------------- helpers --

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "bardwave-libtest-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        // Compare real locations (macOS: /var -> /private/var).
        Self(fs::canonicalize(&path).unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, part: &str) -> PathBuf {
        self.0.join(part)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_file(path: &Path, content: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Every regular file below `dir`, relative, sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(read) = fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(
                    path.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

struct Sample {
    library: Library,
    /// (path, relative path) of every sound.
    files: Vec<(PathBuf, &'static str)>,
}

/// A library with real files under `src`: two groups, three sounds with tags,
/// a scene (items + emitter + playlist) and a playlist.
fn sample(src: &Path) -> Sample {
    let mut lib = Library::new();
    let tavern = lib.add_group("Tavern").unwrap();
    let fx = lib.add_group("Fx").unwrap();

    let crowd = src.join("Tavern/crowd.ogg");
    let fire = src.join("Tavern/fire.wav");
    let hit = src.join("Fx/hit.wav");
    write_file(&crowd, b"crowd-bytes-OggS-0123456789");
    write_file(&fire, &vec![7u8; 300_000]);
    write_file(&hit, b"hit");

    assert!(lib.insert_sound(&crowd, "Гул толпы", 0.6, Some(tavern)));
    assert!(lib.insert_sound(&fire, "Fireplace", 0.4, Some(tavern)));
    assert!(lib.insert_sound(&hit, "", 0.9, Some(fx)));
    lib.set_looped(&crowd, true);
    lib.set_bus(&hit, Bus::Sfx);
    lib.set_gain_db(&fire, -3.5);
    lib.set_oneshot_stinger(&hit, true);
    lib.set_tags(&crowd, &["Loud", "indoor"]);
    lib.set_tags(&fire, &["warm"]);

    let crowd_id = lib.id_of(&crowd).unwrap();
    let fire_id = lib.id_of(&fire).unwrap();
    let hit_id = lib.id_of(&hit).unwrap();

    let playlist = lib.add_playlist("Tavern songs").unwrap();
    lib.add_playlist_item(playlist, crowd_id, None);
    lib.add_playlist_item(playlist, fire_id, None);
    lib.set_playlist_mode(playlist, true, RepeatMode::One);
    lib.set_playlist_crossfade(playlist, 1234);

    let scene = lib.add_scene("Evening").unwrap();
    lib.set_item(scene, SceneItem::new(crowd_id));
    lib.set_item(
        scene,
        SceneItem {
            volume: 0.25,
            looped: true,
            delay_ms: 500,
            ..SceneItem::new(fire_id)
        },
    );
    lib.set_emitters(scene, vec![Emitter::new(vec![hit_id])]);
    lib.set_scene_playlist(scene, Some(playlist));
    lib.set_scene_color(scene, Some("#a04a4a"));
    lib.set_scene_fades(scene, 111, 222);

    Sample {
        library: lib,
        files: vec![
            (crowd, "Tavern/crowd.ogg"),
            (fire, "Tavern/fire.wav"),
            (hit, "Fx/hit.wav"),
        ],
    }
}

/// The config of `lib` with every sound path replaced by `target/rel`.
fn config_with_paths_under(
    lib: &Library,
    sample: &Sample,
    target: &Path,
) -> soundshared::config::LibraryConfig {
    let mut cfg = lib.to_config();
    for sound in &mut cfg.sounds {
        let rel = sample
            .files
            .iter()
            .find(|(path, _)| path.to_string_lossy() == sound.path)
            .map(|(_, rel)| *rel)
            .unwrap();
        sound.path = target.join(rel).to_string_lossy().into_owned();
    }
    cfg
}

const SOUND_ID: &str = "11111111-1111-4111-8111-111111111111";

fn manifest_with(rel: &str, audio: bool) -> String {
    format!(
        "format = \"bardwave-bundle\"\nversion = 1\n\n[[sounds]]\nid = \"{SOUND_ID}\"\nname = \"x\"\nrel_path = \"{}\"\naudio = {audio}\n",
        rel.replace('\\', "\\\\")
    )
}

/// Write a zip with the given entries, in order, stored uncompressed.
fn build_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

fn import(
    lib: &mut Library,
    zip: &Path,
    target: &Path,
    policy: ConflictPolicy,
) -> Result<soundshared::library_bundle::ImportReport, BundleError> {
    import_bundle(lib, zip, &ImportOptions::new(target, policy))
}

// ------------------------------------------------------ export -> import ----

#[test]
fn round_trip_with_audio_gives_an_identical_library() {
    let work = TempDir::new("rt-audio");
    let src = work.join("src");
    let sample = sample(&src);
    let zip = work.join("set.zip");

    let exported = export_bundle(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            name: "Tavern set".into(),
        },
    )
    .unwrap();
    assert_eq!(exported.sounds, 3);
    assert_eq!(exported.audio_files, 3);
    assert!(exported.missing_audio.is_empty());

    let info = inspect_bundle(&zip, &ImportLimits::default()).unwrap();
    assert_eq!(info.name, "Tavern set");
    assert_eq!(
        (info.groups, info.sounds, info.scenes, info.playlists),
        (2, 3, 1, 1)
    );
    assert_eq!(info.audio_files, 3);
    assert_eq!(info.audio_bytes, exported.audio_bytes);

    let dest = work.join("player-machine/sounds");
    let mut imported = Library::new();
    let report = import(&mut imported, &zip, &dest, ConflictPolicy::Skip).unwrap();
    assert_eq!(report.sounds.added, 3);
    assert_eq!(report.groups.added, 2);
    assert_eq!(report.scenes.added, 1);
    assert_eq!(report.playlists.added, 1);
    assert_eq!(report.files_written, 3);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    // Same ids and content; only the sound paths moved to the target folder.
    assert_eq!(
        imported.to_config(),
        config_with_paths_under(&sample.library, &sample, &dest)
    );
    // The scene still resolves its sounds by id.
    let scene = &imported.scenes()[0];
    for item in &scene.items {
        assert!(imported.sound_by_id(item.sound).is_some());
    }
    // The audio really arrived, byte for byte, with no temporary leftovers.
    for (path, rel) in &sample.files {
        assert_eq!(fs::read(dest.join(rel)).unwrap(), fs::read(path).unwrap());
    }
    assert_eq!(
        tree(&dest),
        vec!["Fx/hit.wav", "Tavern/crowd.ogg", "Tavern/fire.wav"]
    );
    assert!(imported.missing_sounds().is_empty());
}

#[test]
fn round_trip_without_audio_points_sounds_at_the_target_folder() {
    let work = TempDir::new("rt-meta");
    let src = work.join("src");
    let sample = sample(&src);
    let zip = work.join("set.zip");
    export_bundle(&sample.library, &zip, &ExportOptions::default()).unwrap();

    let dest = work.join("elsewhere");
    let mut imported = Library::new();
    let report = import(&mut imported, &zip, &dest, ConflictPolicy::Skip).unwrap();
    assert_eq!(report.files_written, 0);
    assert!(!dest.exists(), "no audio, so nothing is created");
    assert_eq!(
        imported.to_config(),
        config_with_paths_under(&sample.library, &sample, &dest)
    );
    // The files are not there yet: all three show up as missing.
    assert_eq!(imported.missing_sounds().len(), 3);
}

#[test]
fn missing_audio_is_exported_without_it() {
    let work = TempDir::new("missing-audio");
    let src = work.join("src");
    let sample = sample(&src);
    fs::remove_file(&sample.files[2].0).unwrap();
    let zip = work.join("set.zip");
    let report = export_bundle(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(report.audio_files, 2);
    assert_eq!(report.missing_audio, vec![sample.files[2].0.clone()]);

    let dest = work.join("dest");
    let mut lib = Library::new();
    import(&mut lib, &zip, &dest, ConflictPolicy::Skip).unwrap();
    assert_eq!(lib.len(), 3);
    assert_eq!(lib.missing_sounds().len(), 1);
}

#[test]
fn hotkeys_stay_out_of_the_bundle_and_survive_an_import() {
    use soundshared::config::HotkeyAction;
    let work = TempDir::new("hotkeys");
    let src = work.join("src");
    let sample = sample(&src);
    let zip = work.join("set.zip");
    export_bundle(&sample.library, &zip, &ExportOptions::default()).unwrap();

    let mut lib = Library::new();
    lib.add_hotkey("F5", HotkeyAction::StopAll).unwrap();
    import(&mut lib, &zip, &work.join("d"), ConflictPolicy::Skip).unwrap();
    assert_eq!(lib.hotkeys().len(), 1);
    assert_eq!(lib.len(), 3);
}

// --------------------------------------------------------------- conflicts --

#[test]
fn importing_twice_with_skip_creates_no_duplicates() {
    let work = TempDir::new("skip-twice");
    let sample = sample(&work.join("src"));
    let zip = work.join("set.zip");
    export_bundle(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();

    let dest = work.join("dest");
    let mut lib = Library::new();
    import(&mut lib, &zip, &dest, ConflictPolicy::Skip).unwrap();
    let before = lib.to_config();
    let files_before = tree(&dest);

    let report = import(&mut lib, &zip, &dest, ConflictPolicy::Skip).unwrap();
    assert_eq!(report.sounds.skipped, 3);
    assert_eq!(report.sounds.added, 0);
    assert_eq!(report.groups.skipped, 2);
    assert_eq!(report.scenes.skipped, 1);
    assert_eq!(report.playlists.skipped, 1);
    assert_eq!(report.files_written, 0);
    assert_eq!(lib.to_config(), before);
    assert_eq!(tree(&dest), files_before);
}

#[test]
fn skip_repoints_references_at_the_existing_entities() {
    let work = TempDir::new("skip-remap");
    let sample = sample(&work.join("src"));
    let zip = work.join("set.zip");
    export_bundle(&sample.library, &zip, &ExportOptions::default()).unwrap();

    // The importer already has "tavern" (other id, other case) and the sound at
    // the very path the bundle would use, but no scene.
    let dest = work.join("dest");
    let mut lib = Library::new();
    let own_group = lib.add_group("tavern").unwrap();
    let own_path = dest.join("Tavern/crowd.ogg");
    assert!(lib.insert_sound(&own_path, "Mine", 0.1, None));
    let own_id = lib.id_of(&own_path).unwrap();

    let report = import(&mut lib, &zip, &dest, ConflictPolicy::Skip).unwrap();
    assert_eq!(report.groups.skipped, 1);
    assert_eq!(report.groups.added, 1);
    assert_eq!(report.sounds.skipped, 1);
    assert_eq!(report.sounds.added, 2);

    // The existing sound and group were left alone ...
    let mine = lib.sound(&own_path).unwrap();
    assert_eq!(mine.name, "Mine");
    assert_eq!(mine.volume, 0.1);
    // ... the bundle's Tavern sounds went into the existing group ...
    let fire = lib.sound(&dest.join("Tavern/fire.wav")).unwrap();
    assert_eq!(fire.group, Some(own_group));
    // ... and the scene's item for the skipped sound now points at it.
    let scene = lib.scenes().iter().find(|s| s.name == "Evening").unwrap();
    assert!(scene.items.iter().any(|item| item.sound == own_id));
    assert!(lib
        .validate()
        .iter()
        .all(|w| !w.to_string().contains("unknown")));
}

#[test]
fn rename_keeps_both_copies_and_fresh_ids() {
    let work = TempDir::new("rename");
    let sample = sample(&work.join("src"));
    let zip = work.join("set.zip");
    export_bundle(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();

    let dest = work.join("dest");
    let mut lib = Library::new();
    import(&mut lib, &zip, &dest, ConflictPolicy::Rename).unwrap();
    let report = import(&mut lib, &zip, &dest, ConflictPolicy::Rename).unwrap();
    assert_eq!(report.sounds.renamed, 3);
    assert_eq!(report.groups.renamed, 2);
    assert_eq!(report.scenes.renamed, 1);
    assert_eq!(report.playlists.renamed, 1);
    assert_eq!(lib.len(), 6);
    assert_eq!(lib.groups().len(), 4);
    assert_eq!(lib.scenes().len(), 2);

    // Names and files are unique; ids are unique; nothing was overwritten.
    let names: HashSet<String> = lib.scenes().iter().map(|s| s.name.to_lowercase()).collect();
    assert_eq!(names.len(), 2);
    assert!(lib.scenes().iter().any(|s| s.name == "Evening (2)"));
    assert!(lib.groups().iter().any(|g| g.name == "Tavern (2)"));
    let ids: HashSet<_> = lib.sounds().iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), 6);
    assert!(dest.join("Tavern/crowd (2).ogg").is_file());
    assert_eq!(tree(&dest).len(), 6);
    assert!(lib.missing_sounds().is_empty());

    // The second scene uses the second set of sounds, not the first.
    let second = lib
        .scenes()
        .iter()
        .find(|s| s.name == "Evening (2)")
        .unwrap();
    for item in &second.items {
        let path = lib.path_of(item.sound).unwrap();
        assert!(path.to_string_lossy().contains("(2)"), "{path:?}");
    }
}

#[test]
fn replace_overwrites_content_but_keeps_the_local_ids() {
    let work = TempDir::new("replace");
    let sample = sample(&work.join("src"));
    let zip = work.join("set.zip");
    export_bundle(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();

    // A local library with the same names but other ids, other volumes and an
    // older version of one file.
    let dest = work.join("dest");
    let mut lib = Library::new();
    let local_group = lib.add_group("Tavern").unwrap();
    let crowd = dest.join("Tavern/crowd.ogg");
    write_file(&crowd, b"old");
    assert!(lib.insert_sound(&crowd, "Old name", 0.05, Some(local_group)));
    let local_id = lib.id_of(&crowd).unwrap();
    let local_scene = lib.add_scene("evening").unwrap();

    let report = import(&mut lib, &zip, &dest, ConflictPolicy::Replace).unwrap();
    assert_eq!(report.sounds.replaced, 1);
    assert_eq!(report.sounds.added, 2);
    assert_eq!(report.groups.replaced, 1);
    assert_eq!(report.scenes.replaced, 1);

    let crowd_now = lib.sound(&crowd).unwrap();
    assert_eq!(crowd_now.id, local_id, "the local id survives");
    assert_eq!(crowd_now.name, "Гул толпы");
    assert_eq!(crowd_now.volume, 0.6);
    assert_eq!(crowd_now.group, Some(local_group));
    assert_eq!(
        fs::read(&crowd).unwrap(),
        fs::read(&sample.files[0].0).unwrap()
    );
    assert_eq!(lib.scenes().len(), 1);
    assert_eq!(lib.scenes()[0].id, local_scene);
    assert_eq!(lib.scenes()[0].items.len(), 2);
    assert!(lib.scenes()[0].items.iter().any(|i| i.sound == local_id));
    assert_eq!(lib.groups().len(), 2);
}

#[test]
fn an_existing_file_is_kept_by_skip_copied_by_rename_overwritten_by_replace() {
    let work = TempDir::new("disk-conflict");
    let sample = sample(&work.join("src"));
    let zip = work.join("set.zip");
    export_bundle(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();

    for (policy, expect_content, expect_files) in [
        (ConflictPolicy::Skip, &b"mine"[..], 3),
        (ConflictPolicy::Rename, &b"mine"[..], 4),
        (ConflictPolicy::Replace, &b"hit"[..], 3),
    ] {
        let dest = work.join(&format!("dest-{policy:?}"));
        write_file(&dest.join("Fx/hit.wav"), b"mine");
        let mut lib = Library::new();
        import(&mut lib, &zip, &dest, policy).unwrap();
        assert_eq!(lib.len(), 3, "{policy:?}");
        assert_eq!(fs::read(dest.join("Fx/hit.wav")).unwrap(), expect_content);
        assert_eq!(tree(&dest).len(), expect_files, "{policy:?}");
        assert!(lib.missing_sounds().is_empty(), "{policy:?}");
    }
}

#[test]
fn dry_run_predicts_without_writing_or_changing_anything() {
    let work = TempDir::new("dry-run");
    let sample = sample(&work.join("src"));
    let zip = work.join("set.zip");
    export_bundle(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let dest = work.join("dest");
    let mut lib = Library::new();
    let mut options = ImportOptions::new(&dest, ConflictPolicy::Skip);
    options.dry_run = true;
    let report = import_bundle(&mut lib, &zip, &options).unwrap();
    assert!(report.dry_run);
    assert_eq!(report.sounds.added, 3);
    assert_eq!(report.files_written, 3);
    assert!(lib.is_empty());
    assert!(!dest.exists());
}

#[test]
fn cancelled_export_and_import_leave_nothing_behind() {
    let work = TempDir::new("cancel");
    let sample = sample(&work.join("src"));
    let zip = work.join("set.zip");
    let cancel = AtomicBool::new(true);
    let result = export_bundle_with(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            ..ExportOptions::default()
        },
        &cancel,
        &mut |_| {},
    );
    assert!(matches!(result, Err(BundleError::Cancelled)));
    assert!(!zip.exists());
    assert!(tree(work.path()).iter().all(|f| !f.contains("bwtmp")));

    export_bundle(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let dest = work.join("dest");
    let mut lib = Library::new();
    let result = import_bundle_with(
        &mut lib,
        &zip,
        &ImportOptions::new(&dest, ConflictPolicy::Skip),
        &cancel,
        &mut |_| {},
    );
    assert!(matches!(result, Err(BundleError::Cancelled)));
    assert!(lib.is_empty());
    assert!(!dest.exists());
}

#[test]
fn export_to_a_missing_folder_fails_cleanly() {
    let work = TempDir::new("export-fail");
    let sample = sample(&work.join("src"));
    let result = export_bundle(
        &sample.library,
        &work.join("no/such/dir/set.zip"),
        &ExportOptions::default(),
    );
    assert!(matches!(result, Err(BundleError::Io(_))));
}

#[test]
fn exporting_over_an_existing_bundle_replaces_it() {
    let work = TempDir::new("export-over");
    let sample = sample(&work.join("src"));
    let zip = work.join("set.zip");
    fs::write(&zip, b"stale").unwrap();
    export_bundle(&sample.library, &zip, &ExportOptions::default()).unwrap();
    assert!(inspect_bundle(&zip, &ImportLimits::default()).is_ok());
    assert_eq!(
        tree(work.path())
            .iter()
            .filter(|f| f.ends_with(".zip"))
            .count(),
        1
    );
}

// --------------------------------------------------------- hostile archives --

fn assert_rejected_untouched(
    work: &TempDir,
    zip: &Path,
    expect: impl Fn(&BundleError) -> bool,
    what: &str,
) {
    let dest = work.join("dest");
    let mut lib = Library::new();
    lib.add_group("Keep").unwrap();
    let before = lib.to_config();
    let result = import(&mut lib, zip, &dest, ConflictPolicy::Replace);
    match result {
        Err(error) => assert!(expect(&error), "{what}: unexpected error {error:?}"),
        Ok(_) => panic!("{what}: hostile bundle was accepted"),
    }
    assert_eq!(lib.to_config(), before, "{what}: library changed");
    assert!(
        tree(&dest).is_empty(),
        "{what}: files were written: {:?}",
        tree(&dest)
    );
}

fn is_unsafe(error: &BundleError) -> bool {
    matches!(error, BundleError::UnsafePath(_))
}

#[test]
fn zip_slip_entry_names_are_rejected() {
    let work = TempDir::new("zipslip");
    let manifest = manifest_with("a.wav", true);
    let evil_names = [
        "../evil.txt",
        "audio/../../evil.wav",
        "audio/../evil.wav",
        "/tmp/bardwave-evil.wav",
        "..\\evil.wav",
        "audio\\..\\..\\evil.wav",
        "C:/evil.wav",
        "C:evil.wav",
        "audio/./a.wav",
        "audio//a.wav",
        "audio/sub/../../../evil.wav",
        "audio/a:stream.wav",
    ];
    for (i, evil) in evil_names.iter().enumerate() {
        let zip = work.join(&format!("evil-{i}.zip"));
        build_zip(
            &zip,
            &[
                ("bundle.toml", manifest.as_bytes()),
                ("audio/a.wav", b"RIFF"),
                (evil, b"pwned"),
            ],
        );
        assert_rejected_untouched(&work, &zip, is_unsafe, evil);
        // Nothing escaped next to the target either.
        assert!(!work.join("evil.txt").exists());
        assert!(!work.join("evil.wav").exists());
    }
    assert!(!Path::new("/tmp/bardwave-evil.wav").exists());
}

#[test]
fn manifest_paths_that_escape_are_rejected_even_without_audio() {
    let work = TempDir::new("zipslip-manifest");
    for (i, rel) in [
        "../evil.wav",
        "a/../../evil.wav",
        "/abs/evil.wav",
        "C:/evil.wav",
        "a\\..\\evil.wav",
        "a/./b.wav",
        "a//b.wav",
        "",
        "CON.wav",
    ]
    .iter()
    .enumerate()
    {
        for audio in [false, true] {
            let zip = work.join(&format!("m-{i}-{audio}.zip"));
            build_zip(
                &zip,
                &[("bundle.toml", manifest_with(rel, audio).as_bytes())],
            );
            assert_rejected_untouched(&work, &zip, is_unsafe, rel);
        }
    }
}

#[test]
fn symlink_entries_are_rejected() {
    let work = TempDir::new("symlink-entry");
    let zip = work.join("link.zip");
    let mut writer = ZipWriter::new(File::create(&zip).unwrap());
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer.start_file("bundle.toml", options).unwrap();
    writer
        .write_all(manifest_with("a.wav", false).as_bytes())
        .unwrap();
    writer
        .add_symlink("audio/a.wav", "/etc/passwd", options)
        .unwrap();
    writer.finish().unwrap();
    assert_rejected_untouched(&work, &zip, is_unsafe, "symlink entry");
}

#[cfg(unix)]
#[test]
fn a_symlinked_folder_inside_the_target_cannot_redirect_the_write() {
    let work = TempDir::new("symlink-target");
    let outside = work.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let dest = work.join("dest");
    fs::create_dir_all(&dest).unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("Tavern")).unwrap();

    let sample = sample(&work.join("src"));
    let zip = work.join("set.zip");
    export_bundle(
        &sample.library,
        &zip,
        &ExportOptions {
            include_audio: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();

    let mut lib = Library::new();
    let result = import(&mut lib, &zip, &dest, ConflictPolicy::Replace);
    assert!(
        matches!(result, Err(BundleError::UnsafePath(_))),
        "{result:?}"
    );
    assert!(lib.is_empty());
    assert!(tree(&outside).is_empty(), "escaped: {:?}", tree(&outside));
    // The harmless file that was staged first is cleaned up too.
    assert!(tree(&dest).is_empty(), "{:?}", tree(&dest));
}

#[test]
fn broken_bundles_are_errors_not_panics() {
    let work = TempDir::new("broken");
    type BrokenCase = (&'static str, Vec<(&'static str, Vec<u8>)>);
    let cases: Vec<BrokenCase> = vec![
        ("no-manifest", vec![("audio/a.wav", b"x".to_vec())]),
        (
            "bad-toml",
            vec![("bundle.toml", b"this is = = not toml".to_vec())],
        ),
        (
            "wrong-format",
            vec![("bundle.toml", b"format = \"other\"\nversion = 1\n".to_vec())],
        ),
        (
            "future-version",
            vec![(
                "bundle.toml",
                b"format = \"bardwave-bundle\"\nversion = 99\n".to_vec(),
            )],
        ),
        (
            "bad-id",
            vec![(
                "bundle.toml",
                manifest_with("a.wav", false)
                    .replace(SOUND_ID, "not-a-uuid")
                    .into_bytes(),
            )],
        ),
        (
            "duplicate-id",
            vec![(
                "bundle.toml",
                format!(
                    "{}\n[[sounds]]\nid = \"{SOUND_ID}\"\nrel_path = \"b.wav\"\n",
                    manifest_with("a.wav", false)
                )
                .into_bytes(),
            )],
        ),
        (
            "invalid-utf8",
            vec![("bundle.toml", vec![0xff, 0xfe, 0xfd])],
        ),
    ];
    for (name, entries) in cases {
        let zip = work.join(&format!("{name}.zip"));
        let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, d)| (*n, d.as_slice())).collect();
        build_zip(&zip, &refs);
        let mut lib = Library::new();
        let result = import(&mut lib, &zip, &work.join("dest"), ConflictPolicy::Skip);
        assert!(result.is_err(), "{name}");
        assert!(lib.is_empty());
    }

    let garbage = work.join("garbage.zip");
    fs::write(&garbage, b"definitely not a zip file").unwrap();
    let mut lib = Library::new();
    assert!(import(&mut lib, &garbage, &work.join("dest"), ConflictPolicy::Skip).is_err());
    assert!(import(
        &mut lib,
        &work.join("does-not-exist.zip"),
        &work.join("dest"),
        ConflictPolicy::Skip
    )
    .is_err());
}

#[test]
fn a_missing_audio_entry_only_warns() {
    let work = TempDir::new("promised-audio");
    let zip = work.join("a.zip");
    build_zip(
        &zip,
        &[("bundle.toml", manifest_with("a.wav", true).as_bytes())],
    );
    let mut lib = Library::new();
    let report = import(&mut lib, &zip, &work.join("dest"), ConflictPolicy::Skip).unwrap();
    assert_eq!(lib.len(), 1);
    assert_eq!(report.warnings.len(), 1);
}

#[test]
fn size_and_count_limits_stop_zip_bombs() {
    let work = TempDir::new("limits");
    let manifest = manifest_with("a.wav", true);
    let zip = work.join("big.zip");
    build_zip(
        &zip,
        &[
            ("bundle.toml", manifest.as_bytes()),
            ("audio/a.wav", &vec![0u8; 5000]),
        ],
    );
    let limited = |limits: ImportLimits| {
        let mut lib = Library::new();
        let mut options = ImportOptions::new(work.join("dest"), ConflictPolicy::Skip);
        options.limits = limits;
        (import_bundle(&mut lib, &zip, &options), lib)
    };
    let big = |f: &dyn Fn(&mut ImportLimits)| {
        let mut limits = ImportLimits::default();
        f(&mut limits);
        limits
    };

    let (result, lib) = limited(big(&|l| l.max_file_bytes = 1000));
    assert!(
        matches!(result, Err(BundleError::LimitExceeded(_))),
        "{result:?}"
    );
    assert!(lib.is_empty());
    let (result, _) = limited(big(&|l| l.max_total_bytes = 1000));
    assert!(matches!(result, Err(BundleError::LimitExceeded(_))));
    let (result, _) = limited(big(&|l| l.max_entries = 1));
    assert!(matches!(result, Err(BundleError::LimitExceeded(_))));
    let (result, _) = limited(big(&|l| l.max_manifest_bytes = 10));
    assert!(matches!(result, Err(BundleError::LimitExceeded(_))));
    let (result, _) = limited(big(&|l| l.max_items = 0));
    assert!(matches!(result, Err(BundleError::LimitExceeded(_))));
    assert!(tree(&work.join("dest")).is_empty());

    // Within the limits it imports.
    let (result, lib) = limited(big(&|l| l.max_file_bytes = 5000));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(lib.len(), 1);
}

#[test]
fn a_highly_compressed_entry_is_bounded_by_what_it_really_expands_to() {
    let work = TempDir::new("deflate-bomb");
    let zip = work.join("bomb.zip");
    let mut writer = ZipWriter::new(File::create(&zip).unwrap());
    writer
        .start_file(
            "bundle.toml",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
    writer
        .write_all(manifest_with("a.wav", true).as_bytes())
        .unwrap();
    writer
        .start_file(
            "audio/a.wav",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
    // 8 MB of zeros deflate to a few KB.
    let zeros = vec![0u8; 1024 * 1024];
    for _ in 0..8 {
        writer.write_all(&zeros).unwrap();
    }
    writer.finish().unwrap();
    assert!(fs::metadata(&zip).unwrap().len() < 100_000);

    let mut options = ImportOptions::new(work.join("dest"), ConflictPolicy::Skip);
    options.limits.max_file_bytes = 1024 * 1024;
    let mut lib = Library::new();
    let result = import_bundle(&mut lib, &zip, &options);
    assert!(
        matches!(result, Err(BundleError::LimitExceeded(_))),
        "{result:?}"
    );
    assert!(lib.is_empty());
    assert!(tree(&work.join("dest")).is_empty());
}

#[test]
fn a_lying_size_header_cannot_bypass_the_limit() {
    let work = TempDir::new("lying-header");
    let zip = work.join("lie.zip");
    build_zip(
        &zip,
        &[
            ("bundle.toml", manifest_with("a.wav", true).as_bytes()),
            ("audio/a.wav", &vec![1u8; 5000]),
        ],
    );
    // Patch the central directory so the audio entry claims 10 bytes.
    let mut bytes = fs::read(&zip).unwrap();
    let name = b"audio/a.wav";
    let mut patched = false;
    for at in 0..bytes.len().saturating_sub(46 + name.len()) {
        if bytes[at..at + 4] == [0x50, 0x4b, 0x01, 0x02]
            && &bytes[at + 46..at + 46 + name.len()] == name
        {
            bytes[at + 24..at + 28].copy_from_slice(&10u32.to_le_bytes());
            patched = true;
        }
    }
    assert!(patched);
    fs::write(&zip, &bytes).unwrap();

    let mut options = ImportOptions::new(work.join("dest"), ConflictPolicy::Skip);
    options.limits.max_file_bytes = 100;
    let mut lib = Library::new();
    let result = import_bundle(&mut lib, &zip, &options);
    assert!(result.is_err(), "{result:?}");
    assert!(lib.is_empty());
    assert!(tree(&work.join("dest")).is_empty());
}

// -------------------------------------------------------------------- scan --

/// ```text
/// root/Fx/hit.mp3  Fx/._hit.mp3  Fx/empty.wav
/// root/Tavern/crowd.OGG  Tavern/zz_copy.ogg (same bytes)  Tavern/deep/er/fire.flac
/// root/a.wav  b.TXT  alias.wav -> Tavern/deep/er/fire.flac  link_in -> Fx
/// root/loop -> .   escape -> ../outside   escape.wav -> ../outside/out.wav
/// ```
#[cfg(unix)]
fn scan_tree(work: &TempDir) -> PathBuf {
    use std::os::unix::fs::symlink;
    let root = work.join("root");
    write_file(&root.join("a.wav"), b"AAAA");
    write_file(&root.join("b.TXT"), b"not audio");
    write_file(&root.join("Tavern/crowd.OGG"), b"crowd-crowd-crowd");
    write_file(&root.join("Tavern/zz_copy.ogg"), b"crowd-crowd-crowd");
    write_file(&root.join("Tavern/deep/er/fire.flac"), b"fire!");
    write_file(&root.join("Fx/hit.mp3"), b"hit-hit");
    write_file(&root.join("Fx/._hit.mp3"), b"appledouble");
    write_file(&root.join("Fx/empty.wav"), b"");
    write_file(&work.join("outside/out.wav"), b"outside!");
    symlink(
        root.join("Tavern/deep/er/fire.flac"),
        root.join("alias.wav"),
    )
    .unwrap();
    symlink(root.join("Fx"), root.join("link_in")).unwrap();
    symlink(&root, root.join("loop")).unwrap();
    symlink(work.join("outside"), root.join("escape")).unwrap();
    symlink(work.join("outside/out.wav"), root.join("escape.wav")).unwrap();
    symlink(root.join("nowhere.wav"), root.join("dangling.wav")).unwrap();
    root
}

#[cfg(unix)]
#[test]
fn scan_walks_recursively_follows_only_inner_links_and_skips_duplicates() {
    let work = TempDir::new("scan");
    let root = scan_tree(&work);
    let report = scan_dir(
        &root,
        &[],
        &ScanOptions::default(),
        &AtomicBool::new(false),
        |_| {},
    );
    assert!(!report.cancelled);
    assert!(report.errors.is_empty(), "{:?}", report.errors);

    let found: Vec<(String, Option<String>)> = report
        .files
        .iter()
        .map(|f| {
            (
                f.path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                f.folder.clone(),
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            ("a.wav".to_string(), None),
            ("Fx/hit.mp3".to_string(), Some("Fx".to_string())),
            ("Tavern/crowd.OGG".to_string(), Some("Tavern".to_string())),
            (
                "Tavern/deep/er/fire.flac".to_string(),
                Some("Tavern".to_string())
            ),
        ]
    );
    assert_eq!(report.stats.found, 4);
    // zz_copy.ogg (same bytes as crowd) and alias.wav (link to fire.flac).
    assert_eq!(report.stats.skipped_duplicates, 2);
    assert_eq!(report.stats.skipped_empty, 1);
    // escape/ (folder outside the root) and escape.wav (file outside the root);
    // the dangling link is dropped without being counted as an error.
    assert!(report.stats.skipped_links >= 2);
    assert!(report
        .files
        .iter()
        .all(|f| !f.path.starts_with(work.join("outside"))));
    // The folder links to a visited folder and to the root itself were not
    // walked again.
    assert!(report
        .files
        .iter()
        .all(|f| !f.path.starts_with(root.join("link_in"))));
}

#[cfg(unix)]
#[test]
fn scan_skips_files_already_in_the_library_by_path_and_by_content() {
    let work = TempDir::new("scan-known");
    let root = scan_tree(&work);

    let mut lib = Library::new();
    // Known by path.
    lib.insert_sound(&root.join("a.wav"), "A", 1.0, None);
    // Known by content, stored somewhere else (same bytes as hit.mp3).
    let elsewhere = work.join("elsewhere/my-hit.mp3");
    write_file(&elsewhere, b"hit-hit");
    lib.insert_sound(&elsewhere, "Hit", 1.0, None);
    // A library entry whose file is gone must not break the scan.
    lib.insert_sound(&work.join("gone.wav"), "Gone", 1.0, None);

    let report = scan_dir(
        &root,
        &known_paths(&lib),
        &ScanOptions::default(),
        &AtomicBool::new(false),
        |_| {},
    );
    let names: Vec<String> = report
        .files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["crowd.OGG", "fire.flac"]);
    assert_eq!(report.stats.skipped_existing, 1);
    assert!(report.stats.skipped_duplicates >= 3);

    // Applying twice adds nothing the second time.
    let first = report.apply_to_library(&mut lib, true);
    assert_eq!(first.added, 2);
    let again = report.apply_to_library(&mut lib, true);
    assert_eq!(again.added, 0);
    assert_eq!(again.already_present, 2);
}

#[test]
fn scan_applies_first_level_folders_as_groups() {
    let work = TempDir::new("scan-groups");
    let root = work.join("root");
    write_file(&root.join("top.wav"), b"1");
    write_file(&root.join("Tavern/a.wav"), b"22");
    write_file(&root.join("Tavern/sub/b.WAV"), b"333");
    write_file(&root.join("Fx/c.mp3"), b"4444");
    write_file(&root.join("L".repeat(60)).join("d.aac"), b"55555");

    let report = scan_dir(
        &root,
        &[],
        &ScanOptions::default(),
        &AtomicBool::new(false),
        |_| {},
    );
    assert_eq!(report.files.len(), 5);

    let mut lib = Library::new();
    let existing = lib.add_group("fx").unwrap();
    let stats = report.apply_to_library(&mut lib, true);
    assert_eq!(stats.added, 5);
    // "Tavern" and the long name are new; "Fx" reused the existing "fx".
    assert_eq!(stats.groups_created, 2);
    assert_eq!(lib.groups().len(), 3);
    assert_eq!(
        lib.sound(&root.join("Fx/c.mp3")).unwrap().group,
        Some(existing)
    );
    assert_eq!(lib.sound(&root.join("top.wav")).unwrap().group, None);
    let tavern = lib.groups().iter().find(|g| g.name == "Tavern").unwrap().id;
    assert_eq!(lib.count_for(GroupFilter::Group(tavern)), 2);
    assert_eq!(lib.sound(&root.join("Tavern/a.wav")).unwrap().name, "a");

    // Without auto groups, everything is ungrouped.
    let mut flat = Library::new();
    report.apply_to_library(&mut flat, false);
    assert_eq!(flat.count_for(GroupFilter::Ungrouped), 5);
    assert!(flat.groups().is_empty());
}

#[test]
fn scan_runs_on_a_thread_reports_progress_and_can_be_cancelled() {
    let work = TempDir::new("scan-thread");
    let root = work.join("root");
    for i in 0..30 {
        write_file(
            &root.join(format!("d{}/f{i}.wav", i % 5)),
            format!("{i}").repeat(i + 1).as_bytes(),
        );
    }
    let handle = start_scan(root.clone(), Vec::new(), ScanOptions::default()).unwrap();
    let report = handle.wait().expect("report");
    assert_eq!(report.files.len(), 30);
    assert!(!report.cancelled);

    // Events end with exactly one Done.
    let handle = start_scan(root.clone(), Vec::new(), ScanOptions::default()).unwrap();
    let mut done = 0;
    let mut last_found = 0;
    while done == 0 {
        match handle.events().recv().unwrap() {
            ScanEvent::Progress(stats) => {
                assert!(stats.found >= last_found);
                last_found = stats.found;
            }
            ScanEvent::Done(report) => {
                done += 1;
                assert_eq!(report.stats.found, 30);
            }
        }
    }
    assert!(handle.events().recv().is_err(), "nothing after Done");

    // A scan cancelled before it starts walking returns an empty, flagged
    // report.
    let cancel = AtomicBool::new(true);
    let report = scan_dir(&root, &[], &ScanOptions::default(), &cancel, |_| {});
    assert!(report.cancelled);
    assert!(report.files.is_empty());
}

#[test]
fn scanning_a_file_or_a_missing_folder_reports_an_error() {
    let work = TempDir::new("scan-bad-root");
    write_file(&work.join("f.wav"), b"x");
    for root in [work.join("f.wav"), work.join("nope")] {
        let report = scan_dir(
            &root,
            &[],
            &ScanOptions::default(),
            &AtomicBool::new(false),
            |_| {},
        );
        assert!(report.files.is_empty());
        assert_eq!(report.stats.errors, 1);
    }
}

#[test]
fn scan_respects_the_depth_limit() {
    let work = TempDir::new("scan-depth");
    let root = work.join("root");
    write_file(&root.join("a/b/c/deep.wav"), b"d");
    write_file(&root.join("a/shallow.wav"), b"s");
    let options = ScanOptions { max_depth: 1 };
    let report = scan_dir(&root, &[], &options, &AtomicBool::new(false), |_| {});
    assert_eq!(report.files.len(), 1);
    assert!(report.files[0].path.ends_with("shallow.wav"));
}

#[test]
fn locate_files_finds_moved_sounds_by_name() {
    let work = TempDir::new("locate");
    let root = work.join("new-home");
    write_file(&root.join("Tavern/Crowd.OGG"), b"c");
    write_file(&root.join("Fx/hit.wav"), b"h");
    write_file(&root.join("Other/hit.wav"), b"h2");

    let wanted = vec![
        PathBuf::from("/old/place/crowd.ogg"),
        PathBuf::from("/old/place/hit.wav"),
        PathBuf::from("/old/place/lost.wav"),
    ];
    let found = locate_files(
        &root,
        &wanted,
        &ScanOptions::default(),
        &AtomicBool::new(false),
    );
    assert_eq!(found.len(), 2);
    assert_eq!(found[&wanted[0]], vec![root.join("Tavern/Crowd.OGG")]);
    assert_eq!(found[&wanted[1]].len(), 2);
    assert!(!found.contains_key(&wanted[2]));
}

#[test]
fn audio_extensions_are_recognized_in_any_case() {
    for name in ["x.WAV", "x.Flac", "x.mP3", "x.oGg", "x.M4A", "x.AAC"] {
        assert!(is_audio_file(Path::new(name)), "{name}");
    }
    assert!(!is_audio_file(Path::new("x.txt")));
}

// ---------------------------------------------------- missing files / remap --

#[test]
fn missing_sounds_relink_and_remap_keep_ids_and_references() {
    let work = TempDir::new("missing");
    let present = work.join("present.wav");
    write_file(&present, b"p");
    let gone = work.join("gone.wav");

    let mut lib = Library::new();
    lib.insert_sound(&present, "present", 1.0, None);
    lib.insert_sound(&gone, "gone", 1.0, None);
    let gone_id = lib.id_of(&gone).unwrap();
    let scene = lib.add_scene("S").unwrap();
    lib.set_item(scene, SceneItem::new(gone_id));

    let missing = lib.missing_sounds();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].path, gone);

    // Relink to the file's new location.
    let moved = work.join("moved/gone.wav");
    write_file(&moved, b"g");
    assert!(lib.relink(&gone, &moved));
    assert!(lib.missing_sounds().is_empty());
    assert_eq!(lib.id_of(&moved), Some(gone_id));
    assert!(lib.sound(&gone).is_none());
    assert_eq!(lib.scenes()[0].items[0].sound, gone_id);

    // Refusals.
    assert!(!lib.relink(&gone, &moved), "old path is unknown now");
    assert!(
        !lib.relink(&moved, &present),
        "target belongs to another sound"
    );
    assert!(!lib.relink(&moved, Path::new("")));
    assert!(lib.relink(&moved, &moved));
}

#[test]
fn remap_prefix_rewrites_paths_without_touching_ids() {
    let mut lib = Library::new();
    lib.insert_sound(Path::new("/old/music/a.wav"), "a", 1.0, None);
    lib.insert_sound(Path::new("/old/music/sub/b.ogg"), "b", 1.0, None);
    lib.insert_sound(Path::new("/old/music2/c.wav"), "c", 1.0, None);
    lib.insert_sound(Path::new("/elsewhere/d.wav"), "d", 1.0, None);
    let ids: Vec<_> = lib.sounds().iter().map(|s| s.id).collect();
    let scene = lib.add_scene("S").unwrap();
    lib.set_item(scene, SceneItem::new(ids[0]));

    assert_eq!(lib.remap_prefix_plan("/old/music", "/new").len(), 2);
    assert_eq!(lib.remap_prefix("/old/music", "/new"), 2);
    assert_eq!(lib.sounds()[0].path, Path::new("/new/a.wav"));
    assert_eq!(lib.sounds()[1].path, Path::new("/new/sub/b.ogg"));
    assert_eq!(lib.sounds()[2].path, Path::new("/old/music2/c.wav"));
    assert_eq!(lib.sounds()[3].path, Path::new("/elsewhere/d.wav"));
    let after: Vec<_> = lib.sounds().iter().map(|s| s.id).collect();
    assert_eq!(ids, after);
    assert_eq!(lib.id_of(Path::new("/new/sub/b.ogg")), Some(ids[1]));
    assert!(lib.sound(Path::new("/old/music/a.wav")).is_none());
    assert_eq!(lib.scenes()[0].items[0].sound, ids[0]);

    // Nothing left under the old prefix; unusable prefixes do nothing.
    assert_eq!(lib.remap_prefix("/old/music", "/new"), 0);
    assert_eq!(lib.remap_prefix("", "/x"), 0);
    assert_eq!(lib.remap_prefix("/new", ""), 0);
}

#[test]
fn remap_prefix_never_merges_two_sounds() {
    let mut lib = Library::new();
    lib.insert_sound(Path::new("/a/x.wav"), "moves", 1.0, None);
    lib.insert_sound(Path::new("/b/x.wav"), "stays", 1.0, None);
    // /a/x.wav would land on the path of the sound that stays: refused.
    assert_eq!(lib.remap_prefix("/a", "/b"), 0);
    assert_eq!(lib.sounds()[0].path, Path::new("/a/x.wav"));
    assert_eq!(lib.len(), 2);
}

#[test]
fn remap_prefix_converts_windows_libraries_to_unix_paths() {
    let mut lib = Library::new();
    lib.insert_sound(Path::new(r"C:\Sounds\Tavern\a.wav"), "a", 1.0, None);
    lib.insert_sound(Path::new(r"C:\Sounds2\b.wav"), "b", 1.0, None);
    let id = lib.sounds()[0].id;
    assert_eq!(lib.remap_prefix(r"C:\Sounds", "/Users/me/Sounds"), 1);
    assert_eq!(
        lib.path_of(id),
        Some(Path::new("/Users/me/Sounds/Tavern/a.wav"))
    );
    assert_eq!(lib.sounds()[1].path, Path::new(r"C:\Sounds2\b.wav"));
}

// ------------------------------------------------------------------ search --

#[test]
fn search_finds_unicode_names_and_tags_regardless_of_case() {
    let mut lib = Library::new();
    let group = lib.add_group("Таверна").unwrap();
    lib.insert_sound(Path::new("/s/1.ogg"), "Гул ТОЛПЫ", 1.0, Some(group));
    lib.insert_sound(Path::new("/s/2.wav"), "Straße", 1.0, None);
    lib.insert_sound(Path::new("/s/3.wav"), "École", 1.0, Some(group));
    lib.set_tags(Path::new("/s/1.ogg"), &["Ночь", "Шум"]);
    lib.set_tags(Path::new("/s/2.wav"), &["ночь"]);

    let count = |q: LibraryQuery| lib.count_matching(&q);
    assert_eq!(count(LibraryQuery::new().with_text("гул толпы")), 1);
    assert_eq!(count(LibraryQuery::new().with_text("ТОЛП")), 1);
    assert_eq!(count(LibraryQuery::new().with_text("straße")), 1);
    assert_eq!(count(LibraryQuery::new().with_text("éCOLE")), 1);
    assert_eq!(count(LibraryQuery::new().with_text("ecole")), 0);
    assert_eq!(count(LibraryQuery::new().with_tag("НОЧЬ")), 2);
    assert_eq!(
        count(LibraryQuery::new().with_tag("ночь").with_tag("шум")),
        1
    );
    assert_eq!(
        count(
            LibraryQuery::new()
                .with_group(GroupFilter::Group(group))
                .with_tag("ночь")
        ),
        1
    );
    assert_eq!(count(LibraryQuery::from(GroupFilter::Ungrouped)), 1);
    assert_eq!(
        lib.group_counts(&LibraryQuery::new().with_tag("ночь")).all,
        2
    );
}
