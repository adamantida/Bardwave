//! Where Bardwave keeps its files (config, library, log, downloaded tools).
//!
//! Up to v1.4.0 everything lived next to the executable. That is fine for a
//! portable Windows/Linux folder, but wrong for a macOS `.app` bundle (the
//! bundle is read-only or signed, and edits break its signature) and for
//! installs into `Program Files` or `/usr/bin`. [`data_dir`] keeps the old
//! behaviour wherever it still works and falls back to the per-user system
//! location otherwise:
//!
//! | situation                                                         | directory                                   |
//! |-------------------------------------------------------------------|---------------------------------------------|
//! | `config.toml` already next to the executable (on Windows only if that folder is writable) | the executable's directory (v1.4.0 layout)  |
//! | a `portable.flag` file next to the executable                     | the executable's directory                  |
//! | executable directory is writable and not inside a macOS `.app`    | the executable's directory                  |
//! | otherwise, macOS                                                  | `~/Library/Application Support/Bardwave`    |
//! | otherwise, Windows                                                | `%APPDATA%\Bardwave`                        |
//! | otherwise, Linux / other Unix                                     | `$XDG_DATA_HOME/bardwave` or `~/.local/share/bardwave` |
//!
//! That directory is the *root*. With `--profile <name>` the config and the
//! library move to `<root>/profiles/<name>/` (see [`crate::profiles`]); the log,
//! the downloaded tools and `recent.toml` stay in the root and are shared.
//!
//! The decision itself is a pure function ([`choose_data_dir`]) so every OS
//! branch is unit-tested on any host.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// File name of the main configuration.
pub const CONFIG_FILE: &str = "config.toml";
/// Marker file that forces portable mode (data next to the executable).
pub const PORTABLE_FLAG: &str = "portable.flag";
/// Name of the log file inside the data directory.
pub const LOG_FILE: &str = "bardwave.log";
/// Name of the downloaded-audio library folder inside the data directory.
pub const LIBRARY_DIR: &str = "bardwave-library";
/// Name of the folder for downloaded helper tools (yt-dlp, ffmpeg).
pub const TOOLS_DIR: &str = "tools";

/// Operating-system family, as far as the directory layout cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    /// Linux and every other Unix.
    Unix,
}

impl Os {
    /// The OS this binary was compiled for.
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Unix
        }
    }
}

/// The environment variables [`system_data_dir`] looks at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    /// `HOME` (macOS, Linux).
    pub home: Option<PathBuf>,
    /// `APPDATA` (Windows).
    pub appdata: Option<PathBuf>,
    /// `XDG_DATA_HOME` (Linux).
    pub xdg_data_home: Option<PathBuf>,
}

impl Env {
    /// Read the variables from the real process environment. Empty values
    /// count as unset, as the XDG spec requires.
    pub fn from_process() -> Env {
        let get = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Env {
            home: get("HOME").or_else(|| get("USERPROFILE")),
            appdata: get("APPDATA"),
            xdg_data_home: get("XDG_DATA_HOME"),
        }
    }
}

/// Facts about the executable directory that influence the choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExeDirProbe {
    /// `config.toml` already exists there (an existing v1.4.0 install).
    pub has_config: bool,
    /// `portable.flag` exists there.
    pub has_portable_flag: bool,
    /// A file can be created there.
    pub writable: bool,
}

/// True when `dir` lies inside a macOS application bundle (`Foo.app/...`).
pub fn inside_app_bundle(dir: &Path) -> bool {
    dir.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|name| name.len() > 4 && name.to_ascii_lowercase().ends_with(".app"))
    })
}

/// The per-user system location for `os`, or None when the needed variables
/// are missing.
pub fn system_data_dir(os: Os, env: &Env) -> Option<PathBuf> {
    match os {
        Os::MacOs => env.home.as_ref().map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("Bardwave")
        }),
        Os::Windows => env.appdata.as_ref().map(|dir| dir.join("Bardwave")),
        Os::Unix => match &env.xdg_data_home {
            Some(xdg) => Some(xdg.join("bardwave")),
            None => env
                .home
                .as_ref()
                .map(|home| home.join(".local").join("share").join("bardwave")),
        },
    }
}

/// Pure decision: which directory holds the data, given the facts.
///
/// Falls back to the executable directory when no system location can be
/// derived (no `HOME`/`APPDATA`), since that at least is a place we know.
pub fn choose_data_dir(os: Os, exe_dir: &Path, env: &Env, probe: ExeDirProbe) -> PathBuf {
    // On Windows a `config.toml` in a folder this user cannot write to is not a
    // portable install: it is what an elevated first run (the firewall-rule
    // step) leaves in `Program Files`, and pinning the data there would stop
    // a normal user from saving anything. Only `portable.flag` opts in then.
    let config_pins_exe_dir = probe.has_config && (os != Os::Windows || probe.writable);
    let keep_next_to_exe = config_pins_exe_dir
        || probe.has_portable_flag
        || (probe.writable && !(os == Os::MacOs && inside_app_bundle(exe_dir)));

    if keep_next_to_exe {
        return exe_dir.to_path_buf();
    }
    system_data_dir(os, env).unwrap_or_else(|| exe_dir.to_path_buf())
}

/// Whether a file can be created in `dir` (a real probe: an existing but
/// read-only folder such as `Program Files` accepts nothing).
pub fn dir_is_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".bardwave-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Directory of the running executable.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Gather the real facts for `exe_dir` and decide.
fn resolve(exe_dir: &Path) -> PathBuf {
    let probe = ExeDirProbe {
        has_config: exe_dir.join(CONFIG_FILE).is_file(),
        has_portable_flag: exe_dir.join(PORTABLE_FLAG).exists(),
        writable: dir_is_writable(exe_dir),
    };
    choose_data_dir(Os::current(), exe_dir, &Env::from_process(), probe)
}

/// The root data directory: where the default profile keeps its files and where
/// the shared log, the downloaded tools and `recent.toml` live.
///
/// Decided once per process and created on first use. If the chosen system
/// directory cannot be created the executable directory is used instead.
pub fn root_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = exe_dir();
        let chosen = match &exe {
            Some(dir) => resolve(dir),
            // No executable path at all: the working directory is the last resort.
            None => system_data_dir(Os::current(), &Env::from_process())
                .unwrap_or_else(|| PathBuf::from(".")),
        };
        match std::fs::create_dir_all(&chosen) {
            Ok(()) => chosen,
            Err(_) => exe.unwrap_or(chosen),
        }
    })
    .clone()
}

/// Profile chosen for this process: `None` is the default profile.
static ACTIVE_PROFILE: OnceLock<Option<String>> = OnceLock::new();

/// Choose the profile of this process (`--profile`). Must run before anything
/// asks for [`data_dir`]; returns false (and changes nothing) when a profile was
/// already fixed, either explicitly or by an earlier path lookup.
pub fn set_active_profile(profile: Option<String>) -> bool {
    ACTIVE_PROFILE.set(profile).is_ok()
}

/// The profile this process runs: `None` for the default profile. Fixed on the
/// first call, so a path can never change under a running app.
pub fn active_profile() -> Option<String> {
    ACTIVE_PROFILE.get_or_init(|| None).clone()
}

/// Resolve and create the data directory of `profile` under `root`: the root
/// itself for the default profile, `<root>/profiles/<name>` otherwise. Fails
/// (instead of quietly using the root) when the folder cannot be created, so a
/// profile never ends up writing over the default profile's data.
pub fn prepare_data_dir(root: &Path, profile: Option<&str>) -> Result<PathBuf, String> {
    let dir = crate::profiles::profile_dir(root, profile)
        .map_err(|e| format!("invalid profile name: {e}"))?;
    if profile.is_some() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create the profile folder {}: {e}", dir.display()))?;
    }
    Ok(dir)
}

/// Make sure the active profile's folder exists; call once at startup, before
/// anything is read or written, and exit with the message on `Err`.
pub fn ensure_data_dir() -> Result<(), String> {
    prepare_data_dir(&root_dir(), active_profile().as_deref()).map(|_| ())
}

/// The directory for config and library of the active profile: the root itself
/// for the default profile, `<root>/profiles/<name>` otherwise. Never falls back
/// to the root for a named profile; startup checks creation with
/// [`ensure_data_dir`] and exits on failure.
pub fn data_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = root_dir();
        let profile = active_profile();
        match prepare_data_dir(&root, profile.as_deref()) {
            Ok(dir) => dir,
            Err(e) => {
                // Startup already reported this and exited; keep the profile's
                // own path so writes fail loudly rather than hit the root.
                tracing::error!("{e}");
                crate::profiles::profile_dir(&root, profile.as_deref()).unwrap_or(root)
            }
        }
    })
    .clone()
}

/// `config.toml` inside the data directory.
pub fn config_path() -> PathBuf {
    data_dir().join(CONFIG_FILE)
}

/// `bardwave.log`, shared by all profiles (in the root).
pub fn log_path() -> PathBuf {
    root_dir().join(LOG_FILE)
}

/// The downloaded-audio library folder (not created here).
pub fn library_dir() -> PathBuf {
    data_dir().join(LIBRARY_DIR)
}

/// Folder where Bardwave stores helper tools it downloaded itself, shared by
/// all profiles (in the root).
pub fn tools_dir() -> PathBuf {
    root_dir().join(TOOLS_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_all() -> Env {
        Env {
            home: Some(PathBuf::from("/home/u")),
            appdata: Some(PathBuf::from("C:/Users/u/AppData/Roaming")),
            xdg_data_home: None,
        }
    }

    const WRITABLE: ExeDirProbe = ExeDirProbe {
        has_config: false,
        has_portable_flag: false,
        writable: true,
    };
    const READ_ONLY: ExeDirProbe = ExeDirProbe {
        has_config: false,
        has_portable_flag: false,
        writable: false,
    };

    #[test]
    fn profile_dir_that_cannot_be_created_is_an_error_not_the_root() {
        let root = std::env::temp_dir().join(format!("bw-paths-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // `profiles` is a file, so <root>/profiles/Camp cannot be created.
        std::fs::write(root.join("profiles"), b"x").unwrap();
        assert!(prepare_data_dir(&root, Some("Camp")).is_err());
        // The default profile and a creatable profile still work.
        assert_eq!(prepare_data_dir(&root, None).unwrap(), root);
        std::fs::remove_file(root.join("profiles")).unwrap();
        let dir = prepare_data_dir(&root, Some("Camp")).unwrap();
        assert!(dir.is_dir() && dir != root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn writable_exe_dir_keeps_the_v1_4_layout_on_every_os() {
        for os in [Os::Windows, Os::MacOs, Os::Unix] {
            let exe = Path::new("/opt/bardwave");
            assert_eq!(choose_data_dir(os, exe, &env_all(), WRITABLE), exe);
        }
    }

    #[test]
    fn existing_config_wins_even_when_not_writable_or_in_a_bundle() {
        let probe = ExeDirProbe {
            has_config: true,
            ..READ_ONLY
        };
        let exe = Path::new("/Applications/Bardwave.app/Contents/MacOS");
        for os in [Os::MacOs, Os::Unix] {
            assert_eq!(choose_data_dir(os, exe, &env_all(), probe), exe);
        }
    }

    #[test]
    fn windows_config_left_by_an_elevated_run_does_not_pin_program_files() {
        let exe = Path::new("C:/Program Files/Bardwave");
        let appdata = PathBuf::from("C:/Users/u/AppData/Roaming").join("Bardwave");
        // Elevated first run: the folder is writable, so data goes there and
        // config.toml gets created.
        assert_eq!(choose_data_dir(Os::Windows, exe, &env_all(), WRITABLE), exe);
        // Later normal run: the config exists but the folder is read-only.
        let normal = ExeDirProbe {
            has_config: true,
            ..READ_ONLY
        };
        assert_eq!(
            choose_data_dir(Os::Windows, exe, &env_all(), normal),
            appdata
        );
        // A writable portable folder with a config keeps the v1.4.0 layout,
        // and portable.flag still forces it.
        let portable_cfg = ExeDirProbe {
            has_config: true,
            ..WRITABLE
        };
        assert_eq!(
            choose_data_dir(Os::Windows, exe, &env_all(), portable_cfg),
            exe
        );
        let flagged = ExeDirProbe {
            has_config: true,
            has_portable_flag: true,
            writable: false,
        };
        assert_eq!(choose_data_dir(Os::Windows, exe, &env_all(), flagged), exe);
    }

    #[test]
    fn portable_flag_forces_the_exe_dir() {
        let probe = ExeDirProbe {
            has_portable_flag: true,
            ..READ_ONLY
        };
        let exe = Path::new("/Applications/Bardwave.app/Contents/MacOS");
        assert_eq!(choose_data_dir(Os::MacOs, exe, &env_all(), probe), exe);
    }

    #[test]
    fn macos_bundle_uses_application_support_even_if_writable() {
        let exe = Path::new("/Applications/Bardwave.app/Contents/MacOS");
        assert_eq!(
            choose_data_dir(Os::MacOs, exe, &env_all(), WRITABLE),
            PathBuf::from("/home/u/Library/Application Support/Bardwave")
        );
    }

    #[test]
    fn app_suffix_outside_macos_is_just_a_folder_name() {
        let exe = Path::new("/opt/Thing.app/bin");
        assert_eq!(choose_data_dir(Os::Unix, exe, &env_all(), WRITABLE), exe);
        assert_eq!(choose_data_dir(Os::Windows, exe, &env_all(), WRITABLE), exe);
    }

    #[test]
    fn macos_plain_folder_is_not_a_bundle() {
        assert!(!inside_app_bundle(Path::new("/Users/u/bardwave")));
        assert!(!inside_app_bundle(Path::new("/Users/u/.app/x")));
        assert!(!inside_app_bundle(Path::new("/Users/u/myapp/x")));
        assert!(inside_app_bundle(Path::new("/Applications/B.APP/Contents")));
        let exe = Path::new("/Users/u/bardwave");
        assert_eq!(choose_data_dir(Os::MacOs, exe, &env_all(), WRITABLE), exe);
    }

    #[test]
    fn read_only_exe_dir_uses_the_system_location() {
        assert_eq!(
            choose_data_dir(
                Os::MacOs,
                Path::new("/usr/local/bin"),
                &env_all(),
                READ_ONLY
            ),
            PathBuf::from("/home/u/Library/Application Support/Bardwave")
        );
        assert_eq!(
            choose_data_dir(
                Os::Windows,
                Path::new("C:/Program Files/Bardwave"),
                &env_all(),
                READ_ONLY
            ),
            PathBuf::from("C:/Users/u/AppData/Roaming").join("Bardwave")
        );
        assert_eq!(
            choose_data_dir(Os::Unix, Path::new("/usr/bin"), &env_all(), READ_ONLY),
            PathBuf::from("/home/u/.local/share/bardwave")
        );
    }

    #[test]
    fn linux_prefers_xdg_data_home() {
        let env = Env {
            xdg_data_home: Some(PathBuf::from("/data/xdg")),
            ..env_all()
        };
        assert_eq!(
            choose_data_dir(Os::Unix, Path::new("/usr/bin"), &env, READ_ONLY),
            PathBuf::from("/data/xdg/bardwave")
        );
    }

    #[test]
    fn missing_environment_falls_back_to_the_exe_dir() {
        let exe = Path::new("/usr/bin");
        let empty = Env::default();
        for os in [Os::Windows, Os::MacOs, Os::Unix] {
            assert_eq!(choose_data_dir(os, exe, &empty, READ_ONLY), exe);
        }
        // Windows needs APPDATA specifically; HOME alone is not enough.
        let home_only = Env {
            home: Some(PathBuf::from("/home/u")),
            ..Env::default()
        };
        assert_eq!(system_data_dir(Os::Windows, &home_only), None);
    }

    fn unique_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bardwave-paths-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn writability_probe_is_real_and_leaves_nothing_behind() {
        let dir = unique_dir("probe");
        assert!(dir_is_writable(&dir));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert!(!dir_is_writable(&dir.join("missing")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_reads_the_real_flags_from_disk() {
        let dir = unique_dir("resolve");
        // Writable, no flags: stays next to the executable.
        assert_eq!(resolve(&dir), dir);
        // With a config it also stays, regardless of anything else.
        std::fs::write(dir.join(CONFIG_FILE), "").unwrap();
        assert_eq!(resolve(&dir), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn derived_paths_live_in_the_data_dir() {
        let base = data_dir();
        assert!(base.is_dir());
        assert_eq!(config_path(), base.join("config.toml"));
        assert_eq!(library_dir(), base.join("bardwave-library"));
        // Without `--profile` the active profile is the default one: the root.
        assert_eq!(active_profile(), None);
        assert_eq!(base, root_dir());
        assert_eq!(log_path(), base.join("bardwave.log"));
        assert_eq!(tools_dir(), base.join("tools"));
    }
}
