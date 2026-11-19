//! Profiles: several campaigns, each with its own config and library.
//!
//! The *default* profile is the root data directory ([`crate::paths::root_dir`]),
//! exactly where Bardwave has always kept `config.toml` and `bardwave-library/`;
//! nothing is migrated and existing users notice no change. Every other profile
//! lives in `<root>/profiles/<name>/` and holds its own `config.toml` and
//! `bardwave-library/`. Downloaded helper tools and the log stay shared in the
//! root.
//!
//! A running process serves exactly one profile (chosen once by `--profile
//! <name>`, see [`parse_profile_arg`]). Switching is a restart: the GUI asks for
//! a switch with [`request_switch`], the process shuts down normally and `main`
//! then calls [`launch_pending_switch`], so the old server has released its port
//! before the new process binds it.
//!
//! Everything here is a pure function of a root directory (no globals except the
//! pending-switch slot), so the whole layout is tested against temp directories.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Sub-directory of the root that holds the non-default profiles.
pub const PROFILES_DIR: &str = "profiles";
/// File (in the root) that remembers the recently used profiles.
pub const RECENT_FILE: &str = "recent.toml";
/// How many recent profiles are remembered.
pub const MAX_RECENT: usize = 8;
/// Longest accepted profile name, in characters.
pub const MAX_NAME_CHARS: usize = 32;
/// Name that stands for the default (root) profile on the command line and in
/// `recent.toml`. It cannot be used as a name of a regular profile.
pub const DEFAULT_PROFILE: &str = "default";

/// Why a profile name or operation was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// The name is empty after trimming.
    Empty,
    /// More than [`MAX_NAME_CHARS`] characters.
    TooLong,
    /// A character outside letters, digits, space, `-` and `_` (this also
    /// rules out path separators and `..`).
    BadChar(char),
    /// A name the operating system treats specially (`CON`, `NUL`, ...).
    Reserved,
    /// `default` is the root profile, not a folder under `profiles/`.
    DefaultName,
    /// A profile with this name (compared case-insensitively) already exists.
    Exists,
    /// No profile with this name.
    Missing,
    /// The operation is not allowed on the profile that is running.
    Active,
    /// A non-empty profile is deleted without the typed confirmation.
    NotConfirmed,
    /// The profile directory is not a plain directory (e.g. a symlink).
    NotADirectory,
    /// A file system error.
    Io(String),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::Empty => write!(f, "Enter a profile name"),
            ProfileError::TooLong => write!(f, "Up to {MAX_NAME_CHARS} characters"),
            ProfileError::BadChar(c) => write!(
                f,
                "Only letters, digits, space, - and _ are allowed (not {c:?})"
            ),
            ProfileError::Reserved => write!(f, "This name is reserved by the system"),
            ProfileError::DefaultName => write!(f, "\"default\" is the built-in profile"),
            ProfileError::Exists => write!(f, "A profile with this name already exists"),
            ProfileError::Missing => write!(f, "That profile does not exist"),
            ProfileError::Active => write!(f, "The running profile cannot be changed"),
            ProfileError::NotConfirmed => write!(f, "Type the profile name to confirm"),
            ProfileError::NotADirectory => write!(f, "The profile folder is not a plain folder"),
            ProfileError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl From<io::Error> for ProfileError {
    fn from(e: io::Error) -> Self {
        ProfileError::Io(e.to_string())
    }
}

/// Names Windows refuses as file names regardless of extension.
fn is_windows_reserved(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit()
            && upper.as_bytes()[3] != b'0')
}

/// Validate a profile name for use as a directory name and return it trimmed.
///
/// Letters, digits, single spaces inside, `-` and `_`; 1..=32 characters; no
/// path separators, dots (so no `..`), and no names reserved by Windows or
/// equal to `default`. This is the only gate between user input and a path.
pub fn validate_profile_name(raw: &str) -> Result<String, ProfileError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ProfileError::Empty);
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ProfileError::TooLong);
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_'))
    {
        return Err(ProfileError::BadChar(bad));
    }
    if name.eq_ignore_ascii_case(DEFAULT_PROFILE) {
        return Err(ProfileError::DefaultName);
    }
    if is_windows_reserved(name) {
        return Err(ProfileError::Reserved);
    }
    Ok(name.to_string())
}

/// Which profile a name on the command line (or in `recent.toml`) means:
/// `None` for the default profile, otherwise the validated name.
pub fn parse_profile_name(raw: &str) -> Result<Option<String>, ProfileError> {
    if raw.trim().eq_ignore_ascii_case(DEFAULT_PROFILE) {
        return Ok(None);
    }
    validate_profile_name(raw).map(Some)
}

/// Pull `--profile <name>` / `--profile=<name>` out of the process arguments
/// (the first one is the executable). `Ok(None)` when absent or `default`.
pub fn parse_profile_arg(args: &[String]) -> Result<Option<String>, String> {
    let mut found: Option<String> = None;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let value = if arg == "--profile" {
            match iter.next() {
                Some(v) => v.clone(),
                None => return Err("--profile needs a profile name".to_string()),
            }
        } else if let Some(v) = arg.strip_prefix("--profile=") {
            v.to_string()
        } else {
            continue;
        };
        found = Some(value);
    }
    match found {
        None => Ok(None),
        Some(raw) => parse_profile_name(&raw).map_err(|e| format!("invalid profile name: {e}")),
    }
}

/// `<root>/profiles`.
pub fn profiles_root(root: &Path) -> PathBuf {
    root.join(PROFILES_DIR)
}

/// The data directory of a profile: the root itself for `None`.
///
/// The name is validated again here, so no caller can build a path outside
/// `<root>/profiles/` even by mistake.
pub fn profile_dir(root: &Path, profile: Option<&str>) -> Result<PathBuf, ProfileError> {
    match profile {
        None => Ok(root.to_path_buf()),
        Some(name) => {
            let valid = validate_profile_name(name)?;
            Ok(profiles_root(root).join(valid))
        }
    }
}

/// Whether `dir` is a plain directory (not a symlink to somewhere else).
fn is_plain_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir)
        .map(|m| m.is_dir() && !m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Names of the existing profiles (directories under `<root>/profiles/` with a
/// valid name), sorted case-insensitively. The default profile is not listed.
pub fn list_profiles(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(profiles_root(root)) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| is_plain_dir(&e.path()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| validate_profile_name(n).is_ok_and(|v| v == *n))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// Existing profile whose name equals `name` ignoring case, if any.
fn find_existing(root: &Path, name: &str) -> Option<String> {
    list_profiles(root)
        .into_iter()
        .find(|n| n.eq_ignore_ascii_case(name))
}

/// Whether a profile exists. `None` (the default profile) always does.
pub fn profile_exists(root: &Path, profile: Option<&str>) -> bool {
    match profile {
        None => true,
        Some(name) => find_existing(root, name).is_some(),
    }
}

/// True when the profile has no config and no library content, so deleting it
/// loses nothing.
pub fn profile_is_empty(root: &Path, name: &str) -> bool {
    let Ok(dir) = profile_dir(root, Some(name)) else {
        return false;
    };
    fn has_files(dir: &Path) -> bool {
        match std::fs::read_dir(dir) {
            Ok(entries) => entries.filter_map(|e| e.ok()).any(|e| {
                let p = e.path();
                if is_plain_dir(&p) {
                    has_files(&p)
                } else {
                    true
                }
            }),
            Err(_) => false,
        }
    }
    !has_files(&dir)
}

/// Recursively copy `from` into `to` (created if needed). Symlinks are skipped
/// rather than followed, so a link cannot pull in files from elsewhere.
pub fn copy_dir_all(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_dir_all(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

/// Replace every occurrence of the path `from` with `to` inside the config at
/// `config`, so a copied or renamed profile keeps pointing at its own library
/// instead of the old one. Handles the TOML-escaped form of Windows paths too.
fn rewrite_library_paths(config: &Path, from: &Path, to: &Path) -> io::Result<()> {
    let text = match std::fs::read_to_string(config) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let (from_s, to_s) = (from.to_string_lossy(), to.to_string_lossy());
    let mut out = text.replace(from_s.as_ref(), to_s.as_ref());
    let (from_e, to_e) = (from_s.replace('\\', "\\\\"), to_s.replace('\\', "\\\\"));
    if from_e != from_s {
        out = out.replace(&from_e, &to_e);
    }
    if out != text {
        std::fs::write(config, out)?;
    }
    Ok(())
}

/// Create `<root>/profiles/<name>/`. With `copy_from` (the data directory of
/// the profile to clone) its `config.toml` and `bardwave-library/` are copied
/// in and the library paths inside the config are re-pointed at the copy.
///
/// Fails with [`ProfileError::Exists`] when the name is taken (compared
/// case-insensitively, since macOS and Windows file systems are). A partially
/// built profile is removed again when the copy fails.
pub fn create_profile(
    root: &Path,
    name: &str,
    copy_from: Option<&Path>,
) -> Result<PathBuf, ProfileError> {
    let name = validate_profile_name(name)?;
    if find_existing(root, &name).is_some() {
        return Err(ProfileError::Exists);
    }
    let dir = profile_dir(root, Some(&name))?;
    std::fs::create_dir_all(profiles_root(root))?;
    // `create_dir` (not `_all`) so a race with another creator is an error.
    std::fs::create_dir(&dir).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            ProfileError::Exists
        } else {
            e.into()
        }
    })?;
    if let Some(src) = copy_from {
        if let Err(e) = copy_profile_content(src, &dir) {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e.into());
        }
    }
    Ok(dir)
}

fn copy_profile_content(src: &Path, dir: &Path) -> io::Result<()> {
    let src_config = src.join(crate::paths::CONFIG_FILE);
    if src_config.is_file() {
        std::fs::copy(&src_config, dir.join(crate::paths::CONFIG_FILE))?;
    }
    let src_lib = src.join(crate::paths::LIBRARY_DIR);
    if is_plain_dir(&src_lib) {
        let dst_lib = dir.join(crate::paths::LIBRARY_DIR);
        copy_dir_all(&src_lib, &dst_lib)?;
        rewrite_library_paths(&dir.join(crate::paths::CONFIG_FILE), &src_lib, &dst_lib)?;
    }
    Ok(())
}

/// Rename a profile directory. The running profile (`active`) cannot be
/// renamed: its config and library are open. Library paths inside the renamed
/// profile's config follow the new location.
pub fn rename_profile(
    root: &Path,
    old: &str,
    new: &str,
    active: Option<&str>,
) -> Result<(), ProfileError> {
    let old_name = find_existing(root, old).ok_or(ProfileError::Missing)?;
    if active.is_some_and(|a| a.eq_ignore_ascii_case(&old_name)) {
        return Err(ProfileError::Active);
    }
    let new_name = validate_profile_name(new)?;
    // A change of letter case only is the same folder on a case-insensitive
    // file system: allowed, the rename below handles it.
    if !old_name.eq_ignore_ascii_case(&new_name) && find_existing(root, &new_name).is_some() {
        return Err(ProfileError::Exists);
    }
    let from = profile_dir(root, Some(&old_name))?;
    let to = profile_dir(root, Some(&new_name))?;
    if !is_plain_dir(&from) {
        return Err(ProfileError::NotADirectory);
    }
    std::fs::rename(&from, &to)?;
    rewrite_library_paths(
        &to.join(crate::paths::CONFIG_FILE),
        &from.join(crate::paths::LIBRARY_DIR),
        &to.join(crate::paths::LIBRARY_DIR),
    )?;
    Ok(())
}

/// Delete a profile directory for good (there is no trash).
///
/// An empty profile goes without questions; otherwise `confirm` must be the
/// profile's exact name. The running profile can never be deleted.
pub fn delete_profile(
    root: &Path,
    name: &str,
    confirm: &str,
    active: Option<&str>,
) -> Result<(), ProfileError> {
    let name = find_existing(root, name).ok_or(ProfileError::Missing)?;
    if active.is_some_and(|a| a.eq_ignore_ascii_case(&name)) {
        return Err(ProfileError::Active);
    }
    if !profile_is_empty(root, &name) && confirm.trim() != name {
        return Err(ProfileError::NotConfirmed);
    }
    let dir = profile_dir(root, Some(&name))?;
    if !is_plain_dir(&dir) {
        return Err(ProfileError::NotADirectory);
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Recently used profiles, newest first, kept in `<root>/recent.toml`.
///
/// The default profile is stored as [`DEFAULT_PROFILE`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProfiles {
    #[serde(default)]
    pub recent: Vec<String>,
}

impl RecentProfiles {
    /// Read `<root>/recent.toml`. A missing or unreadable file is an empty
    /// list: this is a convenience, never a reason to refuse to start.
    pub fn load(root: &Path) -> Self {
        let mut recent: RecentProfiles = std::fs::read_to_string(root.join(RECENT_FILE))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        recent.normalize();
        recent
    }

    /// Write `<root>/recent.toml` (temp file + rename, so a crash cannot leave
    /// half a file).
    pub fn save(&self, root: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let path = root.join(RECENT_FILE);
        let tmp = root.join(format!("{RECENT_FILE}.tmp"));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
    }

    /// Drop invalid and duplicate entries and cap the list.
    fn normalize(&mut self) {
        let mut seen: Vec<String> = Vec::new();
        self.recent.retain(|entry| {
            let Ok(parsed) = parse_profile_name(entry) else {
                return false;
            };
            let key = parsed.unwrap_or_else(|| DEFAULT_PROFILE.to_string());
            if seen.iter().any(|s| s.eq_ignore_ascii_case(&key)) {
                return false;
            }
            seen.push(key);
            true
        });
        self.recent.truncate(MAX_RECENT);
    }

    /// Move `profile` (`None` = default) to the front.
    pub fn touch(&mut self, profile: Option<&str>) {
        let key = profile.unwrap_or(DEFAULT_PROFILE).to_string();
        self.recent.retain(|e| !e.eq_ignore_ascii_case(&key));
        self.recent.insert(0, key);
        self.normalize();
    }

    /// Forget a profile (after it was deleted or renamed).
    pub fn forget(&mut self, name: &str) {
        self.recent.retain(|e| !e.eq_ignore_ascii_case(name));
    }

    /// The remembered profiles that still exist, newest first, as `None` for
    /// the default profile and `Some(name)` for the others.
    pub fn existing(&self, root: &Path) -> Vec<Option<String>> {
        self.recent
            .iter()
            .filter_map(|e| parse_profile_name(e).ok())
            .filter(|p| profile_exists(root, p.as_deref()))
            .collect()
    }
}

/// Remember at startup that `profile` was opened. Errors are ignored: the
/// recent list is a convenience.
pub fn record_launch(root: &Path, profile: Option<&str>) {
    let mut recent = RecentProfiles::load(root);
    recent.touch(profile);
    let _ = recent.save(root);
}

/// The one slot between the GUI ("switch to X") and `main` (which spawns the
/// new process once this one has shut down). `Some(None)` = default profile.
static PENDING_SWITCH: Mutex<Option<Option<String>>> = Mutex::new(None);

/// Ask for a restart into `target` (`None` = default profile) after shutdown.
pub fn request_switch(target: Option<String>) {
    if let Ok(mut slot) = PENDING_SWITCH.lock() {
        *slot = Some(target);
    }
}

/// Take the requested switch, if any. `Some(None)` means "the default profile".
pub fn take_pending_switch() -> Option<Option<String>> {
    PENDING_SWITCH.lock().ok().and_then(|mut slot| slot.take())
}

/// The command that starts `exe` on `target`. No shell is involved: the name
/// is a single argument.
pub fn switch_command(exe: &Path, target: Option<&str>) -> std::process::Command {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--profile").arg(target.unwrap_or(DEFAULT_PROFILE));
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Start the new process if a switch was requested. Call it last, after the
/// server and the audio engine have been stopped. Returns what was launched.
pub fn launch_pending_switch() -> Option<io::Result<Option<String>>> {
    let target = take_pending_switch()?;
    let result = std::env::current_exe().and_then(|exe| {
        // Detached: the child outlives this process, so it is not waited on.
        switch_command(&exe, target.as_deref()).spawn().map(drop)
    });
    Some(result.map(|()| target))
}

/// Open `dir` in the system file manager (no shell; fire and forget).
pub fn open_in_file_manager(dir: &Path) -> io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bardwave-profiles-unit-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_are_trimmed_and_checked() {
        assert_eq!(
            validate_profile_name("  Curse of Strahd "),
            Ok("Curse of Strahd".into())
        );
        assert_eq!(validate_profile_name("a-b_c 1"), Ok("a-b_c 1".into()));
        assert_eq!(validate_profile_name("Кампания"), Ok("Кампания".into()));
        assert_eq!(validate_profile_name("   "), Err(ProfileError::Empty));
        assert_eq!(
            validate_profile_name(&"x".repeat(33)),
            Err(ProfileError::TooLong)
        );
        assert!(validate_profile_name(&"x".repeat(32)).is_ok());
    }

    #[test]
    fn path_tricks_are_rejected() {
        for bad in [
            "..", ".", "../x", "a/b", "a\\b", "x/..", "/abs", "C:\\x", "a.b", "~", "a\0b", "a:b",
            "a\nb",
        ] {
            assert!(
                matches!(
                    validate_profile_name(bad),
                    Err(ProfileError::BadChar(_)) | Err(ProfileError::Empty)
                ),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn reserved_and_default_names_are_rejected() {
        assert_eq!(
            validate_profile_name("default"),
            Err(ProfileError::DefaultName)
        );
        assert_eq!(
            validate_profile_name("DEFAULT"),
            Err(ProfileError::DefaultName)
        );
        for name in ["con", "NUL", "com1", "LPT9", "Aux"] {
            assert_eq!(
                validate_profile_name(name),
                Err(ProfileError::Reserved),
                "{name}"
            );
        }
        assert!(validate_profile_name("com0").is_ok());
        assert!(validate_profile_name("console").is_ok());
    }

    #[test]
    fn profile_arg_forms() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_profile_arg(&args(&["bw"])), Ok(None));
        assert_eq!(parse_profile_arg(&args(&["bw", "--server-only"])), Ok(None));
        assert_eq!(
            parse_profile_arg(&args(&["bw", "--profile", "Camp 1"])),
            Ok(Some("Camp 1".into()))
        );
        assert_eq!(
            parse_profile_arg(&args(&["bw", "--profile=Camp"])),
            Ok(Some("Camp".into()))
        );
        assert_eq!(
            parse_profile_arg(&args(&["bw", "--profile", "default"])),
            Ok(None)
        );
        assert!(parse_profile_arg(&args(&["bw", "--profile"])).is_err());
        assert!(parse_profile_arg(&args(&["bw", "--profile", "../x"])).is_err());
    }

    #[test]
    fn directories_follow_the_layout() {
        let root = Path::new("/data");
        assert_eq!(profile_dir(root, None).unwrap(), root);
        assert_eq!(
            profile_dir(root, Some("Camp")).unwrap(),
            root.join("profiles").join("Camp")
        );
        assert!(profile_dir(root, Some("../x")).is_err());
    }

    #[test]
    fn create_list_rename_delete_round_trip() {
        let root = temp_root("round");
        assert!(list_profiles(&root).is_empty());
        create_profile(&root, "Beta", None).unwrap();
        create_profile(&root, "alpha", None).unwrap();
        assert_eq!(list_profiles(&root), vec!["alpha", "Beta"]);
        assert_eq!(
            create_profile(&root, "ALPHA", None),
            Err(ProfileError::Exists)
        );

        rename_profile(&root, "alpha", "Gamma", None).unwrap();
        assert_eq!(list_profiles(&root), vec!["Beta", "Gamma"]);
        assert_eq!(
            rename_profile(&root, "Gamma", "beta", None),
            Err(ProfileError::Exists)
        );
        assert_eq!(
            rename_profile(&root, "Gamma", "X", Some("gamma")),
            Err(ProfileError::Active)
        );

        delete_profile(&root, "Beta", "", None).unwrap();
        assert_eq!(list_profiles(&root), vec!["Gamma"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recent_list_is_capped_and_deduplicated() {
        let mut r = RecentProfiles::default();
        for i in 0..12 {
            r.touch(Some(&format!("P{i}")));
        }
        assert_eq!(r.recent.len(), MAX_RECENT);
        assert_eq!(r.recent[0], "P11");
        r.touch(Some("p8"));
        assert_eq!(r.recent[0], "p8");
        assert_eq!(
            r.recent
                .iter()
                .filter(|e| e.eq_ignore_ascii_case("p8"))
                .count(),
            1
        );
        r.touch(None);
        assert_eq!(r.recent[0], DEFAULT_PROFILE);
    }

    #[test]
    fn switch_command_passes_the_name_as_one_argument() {
        let cmd = switch_command(Path::new("/bin/bardwave"), Some("a b; rm -rf"));
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["--profile", "a b; rm -rf"]);
        let cmd = switch_command(Path::new("/bin/bardwave"), None);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["--profile", "default"]);
    }
}
