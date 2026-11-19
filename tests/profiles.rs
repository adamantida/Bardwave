//! Profiles: names, directory choice, create / copy / rename / delete, the
//! recent list and the `--profile` argument. Everything runs in temp
//! directories; the real config, library and port are never touched.

use std::path::{Path, PathBuf};

use soundshared::gui::profiles_ui::{ProfileDialog, ProfileEnv, ProfilesUi};
use soundshared::profiles::*;

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bardwave-profiles-it-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A default profile with a config that points into its own library.
fn seed_default(root: &Path) {
    let lib = root.join("bardwave-library");
    std::fs::create_dir_all(lib.join("sub")).unwrap();
    std::fs::write(lib.join("a.ogg"), b"aaaa").unwrap();
    std::fs::write(lib.join("sub").join("b.ogg"), b"bbbb").unwrap();
    let config = format!(
        "[server]\nport = 8080\n\n[[library.sounds]]\npath = '{}'\n",
        lib.join("a.ogg").display()
    );
    std::fs::write(root.join("config.toml"), config).unwrap();
}

#[test]
fn path_traversal_names_are_refused_everywhere() {
    let root = temp_root("traversal");
    for bad in [
        "..", "../evil", "a/b", "a\\b", ".", "/etc", "C:\\x", "a..b", "~/x",
    ] {
        assert!(validate_profile_name(bad).is_err(), "{bad}");
        assert!(profile_dir(&root, Some(bad)).is_err(), "{bad}");
        assert!(create_profile(&root, bad, None).is_err(), "{bad}");
        assert!(parse_profile_name(bad).is_err(), "{bad}");
    }
    // Nothing was created, in particular nothing outside `profiles/`.
    assert!(list_profiles(&root).is_empty());
    assert!(!root.join("evil").exists());
    assert!(!root.parent().unwrap().join("evil").exists());
    // Deleting / renaming by a traversal name finds no profile either.
    assert_eq!(
        delete_profile(&root, "..", "..", None),
        Err(ProfileError::Missing)
    );
    assert_eq!(
        rename_profile(&root, "..", "x", None),
        Err(ProfileError::Missing)
    );
    assert!(root.is_dir());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn data_dir_choice_per_profile() {
    let root = Path::new("/data/bardwave");
    assert_eq!(profile_dir(root, None).unwrap(), root);
    assert_eq!(
        profile_dir(root, Some("Curse of Strahd")).unwrap(),
        root.join("profiles").join("Curse of Strahd")
    );
}

#[test]
fn no_arguments_means_the_default_profile() {
    // The existing way of starting the app is unchanged.
    assert_eq!(parse_profile_arg(&args(&["bardwave"])), Ok(None));
    assert_eq!(
        parse_profile_arg(&args(&["bardwave", "--server-only"])),
        Ok(None)
    );
    assert_eq!(
        parse_profile_arg(&args(&["bardwave", "--profile", "default"])),
        Ok(None)
    );
    assert_eq!(
        parse_profile_arg(&args(&["bardwave", "--server-only", "--profile", "Camp"])),
        Ok(Some("Camp".to_string()))
    );
    assert_eq!(
        parse_profile_arg(&args(&["bardwave", "--profile=Camp 2"])),
        Ok(Some("Camp 2".to_string()))
    );
    assert!(parse_profile_arg(&args(&["bardwave", "--profile"])).is_err());
    assert!(parse_profile_arg(&args(&["bardwave", "--profile", "../x"])).is_err());
}

#[test]
fn bad_profile_argument_stops_the_binary_before_anything_runs() {
    // `--server-only` keeps it from opening a dialog; the name is rejected
    // before any config or port is looked at.
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_soundshared"))
        .args(["--server-only", "--profile", "../x"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run the binary");
    // It must exit at once; never leave a child running if it does not.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the binary did not reject the profile name");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(2));
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(stderr.contains("invalid profile name"), "{stderr}");
}

#[test]
fn create_empty_profile_and_list() {
    let root = temp_root("create");
    let dir = create_profile(&root, "Alpha", None).unwrap();
    assert_eq!(dir, root.join("profiles").join("Alpha"));
    assert!(dir.is_dir());
    assert!(!dir.join("config.toml").exists());
    assert_eq!(list_profiles(&root), vec!["Alpha".to_string()]);
    assert!(profile_exists(&root, Some("alpha")));
    assert!(profile_exists(&root, None));
    assert!(!profile_exists(&root, Some("Nope")));
    assert_eq!(
        create_profile(&root, "alpha", None),
        Err(ProfileError::Exists)
    );
    // Stray files and invalid folder names are not profiles.
    std::fs::write(root.join("profiles").join("file.txt"), b"x").unwrap();
    std::fs::create_dir_all(root.join("profiles").join("bad.name")).unwrap();
    assert_eq!(list_profiles(&root), vec!["Alpha".to_string()]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn copying_the_current_profile_repoints_library_paths() {
    let root = temp_root("copy");
    seed_default(&root);
    let dir = create_profile(&root, "Copy", Some(&root)).unwrap();
    assert_eq!(
        std::fs::read(dir.join("bardwave-library/sub/b.ogg")).unwrap(),
        b"bbbb"
    );
    let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    let own = dir.join("bardwave-library").join("a.ogg");
    assert!(config.contains(&own.display().to_string()), "{config}");
    let old = root.join("bardwave-library").join("a.ogg");
    assert!(!config.contains(&old.display().to_string()));
    // The profiles folder itself is not copied into the copy.
    assert!(!dir.join("profiles").exists());
    // The source is untouched.
    assert!(root.join("bardwave-library/a.ogg").is_file());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rename_moves_files_and_follows_library_paths() {
    let root = temp_root("rename");
    seed_default(&root);
    create_profile(&root, "Old", Some(&root)).unwrap();
    rename_profile(&root, "Old", "New", Some("Other")).unwrap();
    assert_eq!(list_profiles(&root), vec!["New".to_string()]);
    let dir = profile_dir(&root, Some("New")).unwrap();
    let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(config.contains(&dir.join("bardwave-library").display().to_string()));
    assert!(!config.contains("profiles/Old") && !config.contains("profiles\\Old"));
    assert_eq!(
        rename_profile(&root, "New", "Z", Some("new")),
        Err(ProfileError::Active)
    );
    assert_eq!(
        rename_profile(&root, "Missing", "Z", None),
        Err(ProfileError::Missing)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn delete_needs_the_typed_name_unless_empty() {
    let root = temp_root("delete");
    seed_default(&root);
    create_profile(&root, "Empty", None).unwrap();
    create_profile(&root, "Full", Some(&root)).unwrap();
    assert!(profile_is_empty(&root, "Empty"));
    assert!(!profile_is_empty(&root, "Full"));

    // Empty: no confirmation needed.
    delete_profile(&root, "Empty", "", None).unwrap();
    // Full: refused without, or with a wrong, name.
    assert_eq!(
        delete_profile(&root, "Full", "", None),
        Err(ProfileError::NotConfirmed)
    );
    assert_eq!(
        delete_profile(&root, "Full", "full ", None),
        Err(ProfileError::NotConfirmed)
    );
    // The running profile is never deleted.
    assert_eq!(
        delete_profile(&root, "Full", "Full", Some("Full")),
        Err(ProfileError::Active)
    );
    assert_eq!(list_profiles(&root), vec!["Full".to_string()]);
    delete_profile(&root, "Full", "Full", None).unwrap();
    assert!(list_profiles(&root).is_empty());
    // The default profile's files survived all of it.
    assert!(root.join("config.toml").is_file());
    assert!(root.join("bardwave-library/a.ogg").is_file());
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn a_symlinked_profile_is_neither_listed_nor_deleted() {
    let root = temp_root("symlink");
    let outside = temp_root("symlink-outside");
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("profiles").join("Link")).unwrap();
    assert!(list_profiles(&root).is_empty());
    assert_eq!(
        delete_profile(&root, "Link", "Link", None),
        Err(ProfileError::Missing)
    );
    assert!(outside.join("keep.txt").is_file());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn recent_profiles_round_trip_through_recent_toml() {
    let root = temp_root("recent");
    assert!(RecentProfiles::load(&root).recent.is_empty());
    record_launch(&root, Some("One"));
    record_launch(&root, None);
    record_launch(&root, Some("Two"));
    record_launch(&root, Some("one"));
    let recent = RecentProfiles::load(&root);
    assert_eq!(recent.recent, vec!["one", "Two", DEFAULT_PROFILE]);
    assert!(root.join("recent.toml").is_file());

    // Only the profiles that still exist are offered.
    create_profile(&root, "Two", None).unwrap();
    assert_eq!(recent.existing(&root), vec![Some("Two".to_string()), None]);

    // Capped at eight.
    for i in 0..20 {
        record_launch(&root, Some(&format!("P{i}")));
    }
    let recent = RecentProfiles::load(&root);
    assert_eq!(recent.recent.len(), MAX_RECENT);
    assert_eq!(recent.recent[0], "P19");

    // A corrupt or hostile file is an empty list, not an error.
    std::fs::write(root.join("recent.toml"), "recent = [\"../x\", 5").unwrap();
    assert!(RecentProfiles::load(&root).recent.is_empty());
    std::fs::write(root.join("recent.toml"), "recent = [\"../x\", \"Ok\"]\n").unwrap();
    assert_eq!(RecentProfiles::load(&root).recent, vec!["Ok"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn switch_request_slot_is_taken_once() {
    // Only this test touches the slot in this process.
    assert_eq!(take_pending_switch(), None);
    request_switch(Some("Camp".into()));
    assert_eq!(take_pending_switch(), Some(Some("Camp".to_string())));
    assert_eq!(take_pending_switch(), None);
    request_switch(None);
    assert_eq!(take_pending_switch(), Some(None));
}

#[test]
fn switch_command_has_no_shell() {
    let cmd = switch_command(Path::new("/x/bardwave"), Some("a b; rm -rf ~"));
    assert_eq!(cmd.get_program(), "/x/bardwave");
    let a: Vec<_> = cmd
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    assert_eq!(a, ["--profile", "a b; rm -rf ~"]);
}

fn env_for(root: &Path, active: Option<&str>) -> ProfileEnv {
    ProfileEnv {
        root: root.to_path_buf(),
        data_dir: match active {
            Some(n) => profile_dir(root, Some(n)).unwrap(),
            None => root.to_path_buf(),
        },
        active: active.map(str::to_string),
    }
}

#[test]
fn dialog_flow_new_rename_delete_switch() {
    let root = temp_root("flow");
    seed_default(&root);
    let env = env_for(&root, None);
    let mut ui = ProfilesUi::default();
    ui.refresh(&env);
    assert_eq!(ui.menu_entries(), vec![None]);

    // New (no copy): created, then offers the switch.
    ui.open_new();
    if let ProfileDialog::New { name, .. } = &mut ui.dialog {
        *name = " Camp ".into();
    }
    ui.submit_new(&env);
    assert_eq!(
        ui.dialog,
        ProfileDialog::Switch {
            target: Some("Camp".into()),
            created: true
        }
    );
    assert_eq!(ui.names(), ["Camp"]);

    // A bad name keeps the dialog and reports.
    ui.open_new();
    if let ProfileDialog::New { name, .. } = &mut ui.dialog {
        *name = "../x".into();
    }
    ui.submit_new(&env);
    assert!(matches!(
        &ui.dialog,
        ProfileDialog::New { error: Some(_), .. }
    ));

    // New with copy runs on a worker; poll until done.
    ui.open_new();
    if let ProfileDialog::New { name, copy, .. } = &mut ui.dialog {
        *name = "Clone".into();
        *copy = true;
    }
    ui.submit_new(&env);
    assert!(ui.is_busy());
    for _ in 0..200 {
        ui.poll_job(&env);
        if !ui.is_busy() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(!ui.is_busy());
    assert_eq!(
        ui.dialog,
        ProfileDialog::Switch {
            target: Some("Clone".into()),
            created: true
        }
    );
    assert!(root.join("profiles/Clone/bardwave-library/a.ogg").is_file());

    // Rename.
    ui.open_rename("Camp");
    if let ProfileDialog::Rename { name, .. } = &mut ui.dialog {
        *name = "Camp2".into();
    }
    ui.submit_rename(&env);
    assert!(matches!(ui.dialog, ProfileDialog::Manage { .. }));
    assert_eq!(ui.names(), ["Camp2", "Clone"]);

    // Delete: the non-empty clone needs its name.
    ui.open_delete(&env, "Clone");
    ui.submit_delete(&env);
    assert!(matches!(
        &ui.dialog,
        ProfileDialog::Delete { error: Some(_), .. }
    ));
    if let ProfileDialog::Delete { typed, .. } = &mut ui.dialog {
        *typed = "Clone".into();
    }
    ui.submit_delete(&env);
    assert_eq!(ui.names(), ["Camp2"]);
    // The empty one goes without typing.
    ui.open_delete(&env, "Camp2");
    assert!(matches!(
        &ui.dialog,
        ProfileDialog::Delete { empty: true, .. }
    ));
    ui.submit_delete(&env);
    assert!(ui.names().is_empty());

    // Switch to a profile that vanished is dropped, to the default is allowed.
    ui.open_switch(Some("Gone".into()));
    assert_eq!(ui.confirmed_switch(&env), None);
    ui.open_switch(None);
    assert_eq!(ui.confirmed_switch(&env), Some(None));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_running_profile_cannot_be_renamed_or_deleted_from_the_dialogs() {
    let root = temp_root("running");
    create_profile(&root, "Live", None).unwrap();
    let env = env_for(&root, Some("Live"));
    let mut ui = ProfilesUi::default();
    ui.refresh(&env);
    ui.open_rename("Live");
    if let ProfileDialog::Rename { name, .. } = &mut ui.dialog {
        *name = "Other".into();
    }
    ui.submit_rename(&env);
    assert!(matches!(
        &ui.dialog,
        ProfileDialog::Rename { error: Some(_), .. }
    ));
    ui.open_delete(&env, "Live");
    ui.submit_delete(&env);
    assert!(matches!(
        &ui.dialog,
        ProfileDialog::Delete { error: Some(_), .. }
    ));
    assert_eq!(list_profiles(&root), vec!["Live".to_string()]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn menu_lists_recent_first_then_default_then_the_rest() {
    let root = temp_root("menu");
    create_profile(&root, "B", None).unwrap();
    create_profile(&root, "A", None).unwrap();
    create_profile(&root, "C", None).unwrap();
    record_launch(&root, Some("C"));
    let env = env_for(&root, None);
    let mut ui = ProfilesUi::default();
    ui.refresh(&env);
    assert_eq!(
        ui.menu_entries(),
        vec![
            Some("C".to_string()),
            None,
            Some("A".to_string()),
            Some("B".to_string())
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}
