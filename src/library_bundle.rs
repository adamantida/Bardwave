//! Library bundles: share a sound set as one `.zip` file.
//!
//! A bundle holds `bundle.toml` (groups, sounds with their volumes and tags,
//! scenes, playlists) and, optionally, the audio files themselves under
//! `audio/`. Hotkeys are a console setting and are not part of a bundle.
//!
//! ```text
//! bundle.toml
//! audio/Tavern/crowd.ogg        (only when exported with audio)
//! audio/Tavern/fire.wav
//! ```
//!
//! # Export
//!
//! [`export_bundle`] writes a bundle of the whole [`Library`]. A sound is
//! stored under a *relative path*: its location below the folder that all the
//! exported sounds share (so `~/Music/Tavern/a.ogg` and `~/Music/Fx/b.wav`
//! become `Tavern/a.ogg` and `Fx/b.wav`). When the sounds have nothing in
//! common (other drives) only the file names are kept. Components that are
//! illegal on some platform are replaced, so the result imports anywhere.
//! A sound whose file is missing is exported without audio and listed in
//! [`ExportReport::missing_audio`]. The archive is written to a temporary file
//! and renamed, so a failed export never leaves a broken bundle.
//!
//! # Import
//!
//! [`import_bundle`] merges a bundle into a library:
//!
//! * **ids are preserved**: groups, sounds, scenes and playlists keep the ids
//!   they had when exported, so scenes and playlists keep pointing at the
//!   right sounds. An id is replaced only when the [`ConflictPolicy`] says
//!   to keep both copies of an entity (`Rename`);
//! * **paths are rewritten**: a sound lives at
//!   `ImportOptions::target_dir / relative path`. With audio in the bundle the
//!   file is written there; without it the sound points there and shows up as
//!   missing until the file is found (see `Library::relink`);
//! * **conflicts follow an explicit policy**, applied per entity:
//!   an entity conflicts with an existing one of the same *id*, or of the same
//!   *name* (groups, scenes, playlists, compared case-insensitively), or, for
//!   sounds, at the same *path*; for a sound with audio also when the target
//!   *file already exists on disk*.
//!   - [`ConflictPolicy::Skip`]: keep what is there. The bundle's entity is not
//!     imported and everything in the bundle that referred to it is re-pointed
//!     at the existing one (a scene still gets its sounds). A sound whose only
//!     conflict is an existing file on disk is registered at that file, which
//!     is not overwritten.
//!   - [`ConflictPolicy::Rename`]: keep both. The imported entity gets a free
//!     name (`"Tavern (2)"`, `"hit (2).wav"`) and, when its id is taken, a new
//!     id. Importing the same bundle twice with `Rename` therefore creates
//!     copies, by request.
//!   - [`ConflictPolicy::Replace`]: the bundle wins. The existing entity takes
//!     the bundle's content but **keeps its own id**, so hotkeys and scenes
//!     that use it stay valid. Existing files are overwritten.
//! * the library is changed only after every file was written, in one step.
//!   If the import fails or is cancelled, the files it created are removed
//!   (a file replaced under `Replace` cannot be restored) and the library is
//!   untouched.
//!
//! [`ImportOptions::dry_run`] computes the report (what would be added,
//! renamed, replaced, skipped) without writing anything.
//!
//! # Safety of untrusted archives
//!
//! A bundle may come from anyone, so the importer treats it as hostile:
//!
//! * **zip-slip**: every entry name is checked before anything is read or
//!   written; an absolute path, a `..` or `.` component, an empty component, a
//!   backslash, a `:` (drive letters, streams), a control character or a
//!   symbolic-link entry rejects the whole archive ([`BundleError::UnsafePath`]).
//!   The paths of the sounds inside `bundle.toml` are validated the same strict
//!   way. Files are written only below `target_dir`, and the real (symlink
//!   resolved) location of each parent folder is checked to be inside it;
//!   only archive entries named by the manifest are extracted.
//! * **zip bombs**: [`ImportLimits`] bound the number of entries, the manifest
//!   size, the size of one file and the total extracted size. Sizes declared by
//!   the archive are checked up front, and the bytes actually read are
//!   counted, so a lying header does not help.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::config::{
    default_sound_volume, Bus, GroupConfig, LibraryConfig, PlaylistConfig, SceneConfig,
    SoundConfigEntry,
};
use crate::gui::groups::{Library, LibrarySound, MAX_GROUP_NAME_LEN, MAX_NAME_LEN};

/// Name of the manifest inside the archive.
pub const BUNDLE_MANIFEST: &str = "bundle.toml";
/// Value of `format` in the manifest.
pub const BUNDLE_FORMAT: &str = "bardwave-bundle";
/// Manifest version this build writes and the newest it reads.
pub const BUNDLE_VERSION: u32 = 1;
/// Folder of the audio files inside the archive.
pub const AUDIO_DIR: &str = "audio";

/// Extensions (lower case, without the dot) of audio files an import may write
/// to disk. Anything else the archive carries for a sound is skipped with a
/// warning, so a bundle cannot drop scripts or executables into the library.
pub const ALLOWED_AUDIO_EXTENSIONS: [&str; 6] = ["wav", "flac", "mp3", "ogg", "m4a", "aac"];

/// Whether `rel` ends in one of [`ALLOWED_AUDIO_EXTENSIONS`] (case-insensitive).
fn has_audio_extension(rel: &str) -> bool {
    let name = rel.rsplit(['/', '\\']).next().unwrap_or(rel);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            let ext = ext.to_ascii_lowercase();
            ALLOWED_AUDIO_EXTENSIONS.contains(&ext.as_str())
        }
        _ => false,
    }
}

/// Longest accepted sound display name, in characters.
const MAX_SOUND_NAME_LEN: usize = 256;
/// Longest accepted component of a relative path, in bytes.
const MAX_COMPONENT_BYTES: usize = 200;
/// Longest accepted relative path, in bytes.
const MAX_REL_PATH_BYTES: usize = 1024;
/// Deepest accepted relative path.
const MAX_REL_DEPTH: usize = 32;
/// Copy buffer size.
const CHUNK: usize = 128 * 1024;

/// What can go wrong with a bundle.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    /// File system error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// The file is not a readable zip archive.
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// The archive has no `bundle.toml`.
    #[error("the archive has no bundle.toml")]
    MissingManifest,
    /// `bundle.toml` is unreadable or inconsistent.
    #[error("invalid bundle.toml: {0}")]
    Manifest(String),
    /// The bundle was written by a newer version of the format.
    #[error("unsupported bundle version {0}")]
    UnsupportedVersion(u32),
    /// An entry name or a path in the manifest could escape the target folder.
    #[error("unsafe path in the bundle: {0}")]
    UnsafePath(String),
    /// The archive is bigger than the configured [`ImportLimits`].
    #[error("bundle too large: {0}")]
    LimitExceeded(String),
    /// The caller set the cancel flag.
    #[error("cancelled")]
    Cancelled,
}

/// Progress of an export or import, in files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleProgress {
    /// Files finished.
    pub done: usize,
    /// Files in total.
    pub total: usize,
    /// What is being processed (a relative path).
    pub current: String,
}

// ---------------------------------------------------------------- manifest --

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    app_version: String,
    #[serde(default)]
    groups: Vec<GroupConfig>,
    #[serde(default)]
    sounds: Vec<BundleSound>,
    #[serde(default)]
    scenes: Vec<SceneConfig>,
    #[serde(default)]
    playlists: Vec<PlaylistConfig>,
}

/// A sound as stored in `bundle.toml`: everything of a library sound except
/// its local path, which is replaced by a relative one.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BundleSound {
    id: String,
    #[serde(default)]
    name: String,
    /// Location below the target folder, `/`-separated.
    rel_path: String,
    /// The audio file is inside the archive (`audio/<rel_path>`).
    #[serde(default)]
    audio: bool,
    #[serde(default = "default_sound_volume")]
    volume: f32,
    #[serde(default)]
    looped: bool,
    #[serde(default)]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default)]
    gain_db: f32,
    #[serde(default)]
    bus: Bus,
    #[serde(default)]
    oneshot_stinger: bool,
    #[serde(default)]
    pan: f32,
}

// ------------------------------------------------------------ path hygiene --

/// Make one path component legal on every platform: forbidden characters
/// become `_`, trailing dots and spaces go, an empty result and Windows
/// reserved device names (`CON`, `NUL`, `COM1`...) are replaced or prefixed, and
/// the length is capped. Idempotent; a component is valid exactly when this
/// returns it unchanged.
fn sanitize_component(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut name = replaced.trim_end_matches([' ', '.']).to_string();
    if name.is_empty() {
        name.push('_');
    }
    if is_reserved_device_name(&name) {
        name.insert(0, '_');
    }
    if name.len() > MAX_COMPONENT_BYTES {
        let mut cut = MAX_COMPONENT_BYTES;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name.truncate(cut);
        name = name.trim_end_matches([' ', '.']).to_string();
        if name.is_empty() {
            name.push('_');
        }
    }
    name
}

fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix)
                .is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'9'))
        })
}

/// Strictly validate a manifest `rel_path` and split it into components.
fn validate_rel_path(rel: &str) -> Result<Vec<&str>, BundleError> {
    let bad = || BundleError::UnsafePath(format!("sound path {rel:?}"));
    if rel.is_empty() || rel.len() > MAX_REL_PATH_BYTES {
        return Err(bad());
    }
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() > MAX_REL_DEPTH || parts.iter().any(|p| sanitize_component(p) != *p) {
        return Err(bad());
    }
    Ok(parts)
}

/// Check the raw name of a zip entry. Directory entries may end with `/`.
fn validate_entry_name(name: &str) -> Result<(), BundleError> {
    let bad = |why: &str| BundleError::UnsafePath(format!("{name:?} ({why})"));
    if name.is_empty() || name.len() > 2 * MAX_REL_PATH_BYTES {
        return Err(bad("bad length"));
    }
    if name.starts_with('/') {
        return Err(bad("absolute path"));
    }
    if name.contains('\\') {
        return Err(bad("backslash"));
    }
    if name.contains(':') {
        return Err(bad("drive or stream marker"));
    }
    if name.chars().any(char::is_control) {
        return Err(bad("control character"));
    }
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    for part in trimmed.split('/') {
        match part {
            "" => return Err(bad("empty component")),
            "." | ".." => return Err(bad("dot component")),
            _ => {}
        }
    }
    Ok(())
}

/// A path component, platform independent.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    /// Drive or root.
    Anchor(String),
    /// A real name.
    Name(String),
}

fn split_parts(path: &Path) -> Vec<Part> {
    let text = path.to_string_lossy();
    // A Windows-style path (from a library written on Windows) read on
    // another system: split it by hand.
    if !cfg!(windows) && text.contains('\\') && !text.contains('/') {
        let mut parts = Vec::new();
        for (i, piece) in text.split('\\').enumerate() {
            if piece.is_empty() {
                continue;
            }
            let is_drive = i == 0 && piece.len() == 2 && piece.ends_with(':');
            parts.push(if is_drive {
                Part::Anchor(piece.to_string())
            } else {
                Part::Name(piece.to_string())
            });
        }
        return parts;
    }
    path.components()
        .filter_map(|c| match c {
            Component::Prefix(p) => {
                Some(Part::Anchor(p.as_os_str().to_string_lossy().into_owned()))
            }
            Component::RootDir => Some(Part::Anchor("/".into())),
            Component::CurDir => None,
            Component::ParentDir => Some(Part::Name("..".into())),
            Component::Normal(s) => Some(Part::Name(s.to_string_lossy().into_owned())),
        })
        .collect()
}

/// Relative path (`/`-separated, valid per [`validate_rel_path`]) of every
/// sound, unique case-insensitively.
fn plan_rel_paths(sounds: &[LibrarySound]) -> Vec<String> {
    let parts: Vec<Vec<Part>> = sounds.iter().map(|s| split_parts(&s.path)).collect();

    // Longest folder prefix shared by all sounds.
    let mut common: Option<Vec<Part>> = None;
    for sound_parts in &parts {
        let dir = &sound_parts[..sound_parts.len().saturating_sub(1)];
        common = Some(match common {
            None => dir.to_vec(),
            Some(prefix) => prefix
                .iter()
                .zip(dir)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a.clone())
                .collect(),
        });
    }
    let common = common.unwrap_or_default();
    // Only keep the structure when the shared prefix is a real folder, not
    // just "/" or a drive.
    let keep_structure = common.iter().any(|p| matches!(p, Part::Name(_)));
    let skip = if keep_structure { common.len() } else { 0 };

    let mut used: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(sounds.len());
    for sound_parts in &parts {
        let names: Vec<String> = if keep_structure {
            sound_parts[skip..]
                .iter()
                .filter_map(|p| match p {
                    Part::Name(n) => Some(sanitize_component(n)),
                    Part::Anchor(_) => None,
                })
                .collect()
        } else {
            sound_parts
                .iter()
                .rev()
                .find_map(|p| match p {
                    Part::Name(n) => Some(vec![sanitize_component(n)]),
                    Part::Anchor(_) => None,
                })
                .unwrap_or_else(|| vec!["_".to_string()])
        };
        let mut names = if names.is_empty() {
            vec!["_".to_string()]
        } else {
            names
        };
        names.truncate(MAX_REL_DEPTH);
        let mut rel = names.join("/");
        let mut counter = 2;
        while !used.insert(rel.to_lowercase()) {
            let last = names.last().cloned().unwrap_or_default();
            let numbered = numbered_name(&last, counter);
            counter += 1;
            let mut candidate = names.clone();
            if let Some(slot) = candidate.last_mut() {
                *slot = numbered;
            }
            rel = candidate.join("/");
        }
        out.push(rel);
    }
    out
}

/// `"hit.wav"`, 2 -> `"hit (2).wav"`; always a valid component.
fn numbered_name(name: &str, n: usize) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    sanitize_component(&format!("{stem} ({n}){ext}"))
}

// ------------------------------------------------------------------ export --

/// Export settings.
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Put the audio files into the archive (otherwise the bundle only
    /// carries the library data and the importer must have the sound files).
    pub include_audio: bool,
    /// Optional title stored in the manifest.
    pub name: String,
}

/// What an export did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    /// Sounds listed in the manifest.
    pub sounds: usize,
    /// Audio files stored in the archive.
    pub audio_files: usize,
    /// Their total size in bytes.
    pub audio_bytes: u64,
    /// Sounds whose file could not be found, exported without audio (only
    /// with `include_audio`).
    pub missing_audio: Vec<PathBuf>,
}

/// Export `library` to the zip file `dest` (replaced if it exists).
pub fn export_bundle(
    library: &Library,
    dest: &Path,
    options: &ExportOptions,
) -> Result<ExportReport, BundleError> {
    export_bundle_with(library, dest, options, &AtomicBool::new(false), &mut |_| {})
}

/// [`export_bundle`] with cancellation (checked between chunks of audio, the
/// result is then [`BundleError::Cancelled`] and nothing is written) and a
/// progress callback.
pub fn export_bundle_with(
    library: &Library,
    dest: &Path,
    options: &ExportOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&BundleProgress),
) -> Result<ExportReport, BundleError> {
    let tmp = sibling_temp(dest);
    let result = write_bundle(library, &tmp, options, cancel, progress);
    match result {
        Ok(report) => {
            if let Err(error) = fs::rename(&tmp, dest) {
                let _ = fs::remove_file(&tmp);
                return Err(error.into());
            }
            Ok(report)
        }
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(error)
        }
    }
}

/// A hidden temporary name next to `path`, short enough for any file system.
fn sibling_temp(path: &Path) -> PathBuf {
    let name = format!(".bwtmp-{}", Uuid::new_v4().simple());
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

fn write_bundle(
    library: &Library,
    tmp: &Path,
    options: &ExportOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&BundleProgress),
) -> Result<ExportReport, BundleError> {
    let config = library.to_config();
    let rels = plan_rel_paths(library.sounds());
    let mut report = ExportReport {
        sounds: library.sounds().len(),
        ..ExportReport::default()
    };

    // Which sounds can carry audio.
    let mut with_audio = vec![false; rels.len()];
    if options.include_audio {
        for (i, sound) in library.sounds().iter().enumerate() {
            match fs::metadata(&sound.path) {
                Ok(meta) if meta.is_file() => with_audio[i] = true,
                _ => report.missing_audio.push(sound.path.clone()),
            }
        }
    }

    let manifest = Manifest {
        format: BUNDLE_FORMAT.to_string(),
        version: BUNDLE_VERSION,
        name: options.name.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        groups: config.groups,
        sounds: config
            .sounds
            .iter()
            .zip(&rels)
            .zip(&with_audio)
            .map(|((sound, rel), audio)| bundle_sound(sound, rel, *audio))
            .collect(),
        scenes: config.scenes,
        playlists: config.playlists,
    };
    let manifest_text =
        toml::to_string_pretty(&manifest).map_err(|e| BundleError::Manifest(e.to_string()))?;

    let file = File::create(tmp)?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    zip.start_file(
        BUNDLE_MANIFEST,
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644),
    )?;
    zip.write_all(manifest_text.as_bytes())?;

    let total = rels.len();
    for (i, sound) in library.sounds().iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(BundleError::Cancelled);
        }
        progress(&BundleProgress {
            done: i,
            total,
            current: rels[i].clone(),
        });
        if !with_audio[i] {
            continue;
        }
        let mut source = File::open(&sound.path)?;
        let size = source.metadata()?.len();
        // Audio is already compressed: store it as is.
        zip.start_file(
            format!("{AUDIO_DIR}/{}", rels[i]),
            SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .large_file(size >= u64::from(u32::MAX))
                .unix_permissions(0o644),
        )?;
        let mut buffer = vec![0u8; CHUNK];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(BundleError::Cancelled);
            }
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            zip.write_all(&buffer[..read])?;
            report.audio_bytes += read as u64;
        }
        report.audio_files += 1;
    }
    progress(&BundleProgress {
        done: total,
        total,
        current: String::new(),
    });
    let mut writer = zip.finish()?;
    writer.flush()?;
    Ok(report)
}

fn bundle_sound(sound: &SoundConfigEntry, rel: &str, audio: bool) -> BundleSound {
    BundleSound {
        id: sound.id.clone(),
        name: sound.name.clone(),
        rel_path: rel.to_string(),
        audio,
        volume: sound.volume,
        looped: sound.looped,
        group: sound.group.clone(),
        tags: sound.tags.clone(),
        gain_db: sound.gain_db,
        bus: sound.bus,
        oneshot_stinger: sound.oneshot_stinger,
        pan: sound.pan,
    }
}

// ------------------------------------------------------------------ import --

/// What to do when an imported entity collides with an existing one. See the
/// module docs for the exact rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// Keep the existing entity, ignore the bundle's (the safe default).
    #[default]
    Skip,
    /// Keep both: give the imported one a free name (and a new id if needed).
    Rename,
    /// The bundle's content replaces the existing one (which keeps its id).
    Replace,
}

/// Bounds against hostile or broken archives.
#[derive(Debug, Clone)]
pub struct ImportLimits {
    /// Entries in the archive.
    pub max_entries: usize,
    /// Groups, sounds, scenes and playlists in the manifest, together.
    pub max_items: usize,
    /// Uncompressed size of `bundle.toml`.
    pub max_manifest_bytes: u64,
    /// Uncompressed size of one audio file.
    pub max_file_bytes: u64,
    /// Uncompressed size of everything that gets extracted.
    pub max_total_bytes: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_entries: 20_000,
            max_items: 100_000,
            max_manifest_bytes: 16 * 1024 * 1024,
            max_file_bytes: 2 * 1024 * 1024 * 1024,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
        }
    }
}

/// Import settings.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Folder the sounds go to (created if audio has to be written).
    pub target_dir: PathBuf,
    /// Conflict resolution.
    pub policy: ConflictPolicy,
    /// Size bounds.
    pub limits: ImportLimits,
    /// Compute the report only: write nothing, leave the library unchanged.
    pub dry_run: bool,
}

impl ImportOptions {
    /// Default limits, no dry run.
    pub fn new(target_dir: impl Into<PathBuf>, policy: ConflictPolicy) -> Self {
        Self {
            target_dir: target_dir.into(),
            policy,
            limits: ImportLimits::default(),
            dry_run: false,
        }
    }
}

/// Counts for one kind of entity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntityStats {
    /// Newly added (no conflict).
    pub added: usize,
    /// Replaced an existing entity (`Replace`).
    pub replaced: usize,
    /// Added as a copy under a new name (`Rename`).
    pub renamed: usize,
    /// Not imported because an existing one was kept (`Skip`).
    pub skipped: usize,
}

/// Where each bundle entity ended up: bundle id -> id in the library. Equal
/// for everything imported as is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdMap {
    /// Groups.
    pub groups: HashMap<Uuid, Uuid>,
    /// Sounds.
    pub sounds: HashMap<Uuid, Uuid>,
    /// Playlists.
    pub playlists: HashMap<Uuid, Uuid>,
    /// Scenes.
    pub scenes: HashMap<Uuid, Uuid>,
}

/// What an import did (or, for a dry run, would do).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Groups.
    pub groups: EntityStats,
    /// Sounds.
    pub sounds: EntityStats,
    /// Playlists.
    pub playlists: EntityStats,
    /// Scenes.
    pub scenes: EntityStats,
    /// Audio files written.
    pub files_written: usize,
    /// Their total size.
    pub bytes_written: u64,
    /// Things worth telling the user (an audio file the manifest promised but
    /// the archive lacks, a name that had to be shortened...).
    pub warnings: Vec<String>,
    /// Bundle id -> library id.
    pub id_map: IdMap,
    /// The report is a prediction; nothing was written.
    pub dry_run: bool,
}

/// What a bundle contains (see [`inspect_bundle`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BundleInfo {
    /// Title from the manifest (may be empty).
    pub name: String,
    /// Version of the program that exported it (may be empty).
    pub app_version: String,
    /// Groups in the manifest.
    pub groups: usize,
    /// Sounds in the manifest.
    pub sounds: usize,
    /// Scenes in the manifest.
    pub scenes: usize,
    /// Playlists in the manifest.
    pub playlists: usize,
    /// Audio files stored in the archive.
    pub audio_files: usize,
    /// Their uncompressed size.
    pub audio_bytes: u64,
}

/// Read and validate a bundle without importing it (for an import dialog).
pub fn inspect_bundle(zip_path: &Path, limits: &ImportLimits) -> Result<BundleInfo, BundleError> {
    let loaded = open_bundle(zip_path, limits)?;
    Ok(BundleInfo {
        name: loaded.manifest.name.clone(),
        app_version: loaded.manifest.app_version.clone(),
        groups: loaded.manifest.groups.len(),
        sounds: loaded.manifest.sounds.len(),
        scenes: loaded.manifest.scenes.len(),
        playlists: loaded.manifest.playlists.len(),
        audio_files: loaded.sounds.iter().filter(|s| s.entry.is_some()).count(),
        audio_bytes: loaded
            .sounds
            .iter()
            .filter_map(|s| s.entry.map(|(_, size)| size))
            .sum(),
    })
}

/// A manifest sound after validation.
struct CheckedSound {
    id: Uuid,
    rel: String,
    /// Archive index and declared size of its audio, when present.
    entry: Option<(usize, u64)>,
}

struct Loaded {
    archive: ZipArchive<File>,
    manifest: Manifest,
    group_ids: Vec<Uuid>,
    sounds: Vec<CheckedSound>,
    playlist_ids: Vec<Uuid>,
    scene_ids: Vec<Uuid>,
    warnings: Vec<String>,
}

/// Open the archive and check everything that can be checked without
/// extracting: entry names, limits, manifest syntax, ids, relative paths.
fn open_bundle(zip_path: &Path, limits: &ImportLimits) -> Result<Loaded, BundleError> {
    let mut archive = ZipArchive::new(File::open(zip_path)?)?;
    if archive.len() > limits.max_entries {
        return Err(BundleError::LimitExceeded(format!(
            "{} entries (limit {})",
            archive.len(),
            limits.max_entries
        )));
    }

    let mut index_of: HashMap<String, usize> = HashMap::new();
    let mut sizes: Vec<u64> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i)?;
        let name = entry.name().to_string();
        validate_entry_name(&name)?;
        if let Some(mode) = entry.unix_mode() {
            // S_IFLNK: never accept links, even though nothing here would
            // create one.
            if mode & 0o170000 == 0o120000 {
                return Err(BundleError::UnsafePath(format!("{name:?} (symbolic link)")));
            }
        }
        if let Some(rest) = name.strip_prefix("audio/") {
            if !entry.is_dir() {
                validate_rel_path(rest)?;
            }
        }
        sizes.push(entry.size());
        if index_of.insert(name.clone(), i).is_some() {
            return Err(BundleError::Manifest(format!("duplicate entry {name:?}")));
        }
    }

    let manifest_index = *index_of
        .get(BUNDLE_MANIFEST)
        .ok_or(BundleError::MissingManifest)?;
    if sizes[manifest_index] > limits.max_manifest_bytes {
        return Err(BundleError::LimitExceeded(format!(
            "bundle.toml is {} bytes (limit {})",
            sizes[manifest_index], limits.max_manifest_bytes
        )));
    }
    let mut text = String::new();
    {
        let entry = archive.by_index(manifest_index)?;
        let mut limited = entry.take(limits.max_manifest_bytes + 1);
        limited
            .read_to_string(&mut text)
            .map_err(|e| BundleError::Manifest(e.to_string()))?;
    }
    if text.len() as u64 > limits.max_manifest_bytes {
        return Err(BundleError::LimitExceeded(
            "bundle.toml is too large".into(),
        ));
    }
    let manifest: Manifest =
        toml::from_str(&text).map_err(|e| BundleError::Manifest(e.to_string()))?;
    if manifest.format != BUNDLE_FORMAT {
        return Err(BundleError::Manifest("not a Bardwave bundle".into()));
    }
    if manifest.version == 0 || manifest.version > BUNDLE_VERSION {
        return Err(BundleError::UnsupportedVersion(manifest.version));
    }
    let items = manifest.groups.len()
        + manifest.sounds.len()
        + manifest.scenes.len()
        + manifest.playlists.len();
    if items > limits.max_items {
        return Err(BundleError::LimitExceeded(format!(
            "{items} items in bundle.toml (limit {})",
            limits.max_items
        )));
    }

    let group_ids = parse_unique_ids(manifest.groups.iter().map(|g| g.id.as_str()), "group")?;
    let playlist_ids =
        parse_unique_ids(manifest.playlists.iter().map(|p| p.id.as_str()), "playlist")?;
    let scene_ids = parse_unique_ids(manifest.scenes.iter().map(|s| s.id.as_str()), "scene")?;
    let sound_ids = parse_unique_ids(manifest.sounds.iter().map(|s| s.id.as_str()), "sound")?;

    let mut warnings = Vec::new();
    let mut sounds = Vec::with_capacity(manifest.sounds.len());
    let mut total: u64 = 0;
    for (sound, id) in manifest.sounds.iter().zip(sound_ids) {
        validate_rel_path(&sound.rel_path)?;
        let mut entry = None;
        if sound.audio && !has_audio_extension(&sound.rel_path) {
            warnings.push(format!(
                "{:?} was not extracted: only audio files ({}) are imported; the sound is imported without it",
                sound.rel_path,
                ALLOWED_AUDIO_EXTENSIONS.join(", ")
            ));
        } else if sound.audio {
            match index_of.get(&format!("{AUDIO_DIR}/{}", sound.rel_path)) {
                Some(&i) => {
                    let size = sizes[i];
                    if size > limits.max_file_bytes {
                        return Err(BundleError::LimitExceeded(format!(
                            "{} is {size} bytes (limit {})",
                            sound.rel_path, limits.max_file_bytes
                        )));
                    }
                    total = total.saturating_add(size);
                    entry = Some((i, size));
                }
                None => warnings.push(format!(
                    "the archive has no audio for {:?}; the sound is imported without it",
                    sound.rel_path
                )),
            }
        }
        sounds.push(CheckedSound {
            id,
            rel: sound.rel_path.clone(),
            entry,
        });
    }
    if total > limits.max_total_bytes {
        return Err(BundleError::LimitExceeded(format!(
            "{total} bytes of audio (limit {})",
            limits.max_total_bytes
        )));
    }

    Ok(Loaded {
        archive,
        manifest,
        group_ids,
        sounds,
        playlist_ids,
        scene_ids,
        warnings,
    })
}

fn parse_unique_ids<'a>(
    ids: impl Iterator<Item = &'a str>,
    kind: &str,
) -> Result<Vec<Uuid>, BundleError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in ids {
        let id = Uuid::parse_str(raw.trim())
            .map_err(|_| BundleError::Manifest(format!("bad {kind} id {raw:?}")))?;
        if !seen.insert(id) {
            return Err(BundleError::Manifest(format!("duplicate {kind} id {id}")));
        }
        out.push(id);
    }
    Ok(out)
}

/// Import a bundle into `library`. See the module docs for the rules.
pub fn import_bundle(
    library: &mut Library,
    zip_path: &Path,
    options: &ImportOptions,
) -> Result<ImportReport, BundleError> {
    import_bundle_with(
        library,
        zip_path,
        options,
        &AtomicBool::new(false),
        &mut |_| {},
    )
}

/// [`import_bundle`] with cancellation (checked between chunks, result
/// [`BundleError::Cancelled`], nothing changed) and a progress callback.
pub fn import_bundle_with(
    library: &mut Library,
    zip_path: &Path,
    options: &ImportOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&BundleProgress),
) -> Result<ImportReport, BundleError> {
    let mut loaded = open_bundle(zip_path, &options.limits)?;
    let target =
        std::path::absolute(&options.target_dir).unwrap_or_else(|_| options.target_dir.clone());

    let mut planner = Planner::new(library, &loaded, options.policy, target.clone());
    planner.plan_groups();
    planner.plan_sounds()?;
    planner.plan_playlists();
    planner.plan_scenes();
    let Planned {
        config,
        mut report,
        writes,
    } = planner.finish();
    report.warnings.extend(loaded.warnings.iter().cloned());
    report.dry_run = options.dry_run;
    report.files_written = writes.len();
    report.bytes_written = writes.iter().map(|w| w.size).sum();

    if options.dry_run {
        return Ok(report);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(BundleError::Cancelled);
    }

    extract_all(
        &mut loaded.archive,
        &writes,
        &target,
        options,
        cancel,
        progress,
    )?;
    library.apply_library_config(&config);
    Ok(report)
}

// ---------------------------------------------------------------- planning --

/// How a sound's audio reaches the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    /// Nothing to write.
    No,
    /// Write; the destination is free.
    Create,
    /// Write over an existing file.
    Overwrite,
}

struct PlannedWrite {
    entry_index: usize,
    entry_name: String,
    dest: PathBuf,
    overwrite: bool,
    size: u64,
}

struct Planned {
    config: LibraryConfig,
    report: ImportReport,
    writes: Vec<PlannedWrite>,
}

/// State of one import while the merged configuration is built.
struct Planner<'a> {
    loaded: &'a Loaded,
    policy: ConflictPolicy,
    target: PathBuf,
    config: LibraryConfig,
    report: ImportReport,
    writes: Vec<PlannedWrite>,
    sound_by_id: HashMap<Uuid, usize>,
    sound_by_path: HashMap<String, usize>,
    /// Lower-cased paths handed out by this import.
    claimed: HashSet<String>,
}

impl<'a> Planner<'a> {
    fn new(library: &Library, loaded: &'a Loaded, policy: ConflictPolicy, target: PathBuf) -> Self {
        let config = library.to_config();
        let mut sound_by_id = HashMap::new();
        let mut sound_by_path = HashMap::new();
        for (i, sound) in config.sounds.iter().enumerate() {
            if let Ok(id) = Uuid::parse_str(&sound.id) {
                sound_by_id.insert(id, i);
            }
            sound_by_path.insert(sound.path.clone(), i);
        }
        Self {
            loaded,
            policy,
            target,
            config,
            report: ImportReport::default(),
            writes: Vec::new(),
            sound_by_id,
            sound_by_path,
            claimed: HashSet::new(),
        }
    }

    fn finish(self) -> Planned {
        Planned {
            config: self.config,
            report: self.report,
            writes: self.writes,
        }
    }

    // ------------------------------------------------------------ groups --

    fn plan_groups(&mut self) {
        let loaded = self.loaded;
        for (group, &gid) in loaded.manifest.groups.iter().zip(&loaded.group_ids) {
            let name = clean_name(&group.name, MAX_GROUP_NAME_LEN);
            if name.is_empty() {
                self.report
                    .warnings
                    .push(format!("group {gid} has no name and was ignored"));
                continue;
            }
            let by_id = position_by_id(self.config.groups.iter().map(|g| g.id.as_str()), gid);
            let by_name = self
                .config
                .groups
                .iter()
                .position(|g| g.name.to_lowercase() == name.to_lowercase());
            let stats = &mut self.report.groups;
            match by_id.or(by_name) {
                None => {
                    self.config.groups.push(GroupConfig {
                        id: gid.to_string(),
                        name,
                    });
                    self.report.id_map.groups.insert(gid, gid);
                    stats.added += 1;
                }
                Some(i) => match self.policy {
                    ConflictPolicy::Skip => {
                        self.report
                            .id_map
                            .groups
                            .insert(gid, parse_or(&self.config.groups[i].id, gid));
                        stats.skipped += 1;
                    }
                    ConflictPolicy::Replace => {
                        let taken_elsewhere =
                            self.config.groups.iter().enumerate().any(|(j, g)| {
                                j != i && g.name.to_lowercase() == name.to_lowercase()
                            });
                        if taken_elsewhere {
                            self.report.warnings.push(format!(
                                "group {:?} keeps its name: another group already uses it",
                                self.config.groups[i].name
                            ));
                        } else {
                            self.config.groups[i].name = name;
                        }
                        self.report
                            .id_map
                            .groups
                            .insert(gid, parse_or(&self.config.groups[i].id, gid));
                        stats.replaced += 1;
                    }
                    ConflictPolicy::Rename => {
                        let id = if by_id.is_some() { Uuid::new_v4() } else { gid };
                        let name = unique_name(&name, MAX_GROUP_NAME_LEN, |candidate| {
                            self.config
                                .groups
                                .iter()
                                .any(|g| g.name.to_lowercase() == candidate.to_lowercase())
                        });
                        self.config.groups.push(GroupConfig {
                            id: id.to_string(),
                            name,
                        });
                        self.report.id_map.groups.insert(gid, id);
                        stats.renamed += 1;
                    }
                },
            }
        }
    }

    // ------------------------------------------------------------ sounds --

    fn plan_sounds(&mut self) -> Result<(), BundleError> {
        let loaded = self.loaded;
        for (sound, checked) in loaded.manifest.sounds.iter().zip(&loaded.sounds) {
            self.plan_sound(sound, checked)?;
        }
        Ok(())
    }

    fn plan_sound(
        &mut self,
        sound: &BundleSound,
        checked: &CheckedSound,
    ) -> Result<(), BundleError> {
        let sid = checked.id;
        let mut local = self.target.clone();
        for part in validate_rel_path(&checked.rel)? {
            local.push(part);
        }
        let want_audio = checked.entry.is_some();
        let on_disk = path_kind(&local);
        let local_key = path_key(&local);

        let by_id = self.sound_by_id.get(&sid).copied();
        let by_path = self.sound_by_path.get(&local_string(&local)).copied();
        let claimed = self.claimed.contains(&local_key);
        // An existing directory can never be written over, whatever the policy.
        let dir_in_the_way = on_disk == PathKind::Other;
        let disk_file = on_disk == PathKind::File;

        let group = sound
            .group
            .as_deref()
            .and_then(|raw| Uuid::parse_str(raw.trim()).ok())
            .and_then(|gid| self.report.id_map.groups.get(&gid).copied())
            .map(|gid| gid.to_string());
        let name: String = sound
            .name
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_SOUND_NAME_LEN)
            .collect();

        let make_entry = |id: Uuid, path: &Path| SoundConfigEntry {
            id: id.to_string(),
            path: local_string(path),
            name: name.clone(),
            volume: sound.volume,
            looped: sound.looped,
            group: group.clone(),
            tags: sound.tags.clone(),
            gain_db: sound.gain_db,
            bus: sound.bus,
            oneshot_stinger: sound.oneshot_stinger,
            pan: crate::gui::groups::clamp_pan(sound.pan),
        };

        let conflict = by_id.or(by_path);
        // (final id, final path, write mode) for something that is inserted.
        let mut insert: Option<(Uuid, PathBuf, WriteMode)> = None;
        // The sound is not inserted.
        let mut skipped_to: Option<Uuid> = None;
        let mut replaced: Option<(usize, PathBuf, WriteMode)> = None;
        let mut renamed = false;

        match conflict {
            Some(existing) => match self.policy {
                ConflictPolicy::Skip => {
                    skipped_to = Some(parse_or(&self.config.sounds[existing].id, sid));
                }
                ConflictPolicy::Replace => {
                    let owner = by_path;
                    let path_free =
                        owner.is_none_or(|o| o == existing) && !claimed && !dir_in_the_way;
                    let current = PathBuf::from(&self.config.sounds[existing].path);
                    let (path, mode) = if !path_free {
                        if want_audio {
                            self.report.warnings.push(format!(
                                "audio of {:?} was not extracted: its path belongs to another sound",
                                checked.rel
                            ));
                        }
                        (current, WriteMode::No)
                    } else if want_audio {
                        (
                            local.clone(),
                            if disk_file {
                                WriteMode::Overwrite
                            } else {
                                WriteMode::Create
                            },
                        )
                    } else if disk_file || current == local {
                        (local.clone(), WriteMode::No)
                    } else {
                        (current, WriteMode::No)
                    };
                    replaced = Some((existing, path, mode));
                }
                ConflictPolicy::Rename => {
                    renamed = true;
                    let id = if by_id.is_some() { Uuid::new_v4() } else { sid };
                    let path_taken =
                        by_path.is_some() || claimed || dir_in_the_way || (want_audio && disk_file);
                    let path = if path_taken {
                        self.unique_path(&local)
                    } else {
                        local.clone()
                    };
                    let mode = if want_audio {
                        WriteMode::Create
                    } else {
                        WriteMode::No
                    };
                    insert = Some((id, path, mode));
                }
            },
            None => {
                if claimed || dir_in_the_way {
                    insert = Some((
                        sid,
                        self.unique_path(&local),
                        if want_audio {
                            WriteMode::Create
                        } else {
                            WriteMode::No
                        },
                    ));
                } else if want_audio && disk_file {
                    insert = Some(match self.policy {
                        ConflictPolicy::Skip => (sid, local.clone(), WriteMode::No),
                        ConflictPolicy::Replace => (sid, local.clone(), WriteMode::Overwrite),
                        ConflictPolicy::Rename => {
                            (sid, self.unique_path(&local), WriteMode::Create)
                        }
                    });
                    renamed = self.policy == ConflictPolicy::Rename;
                } else {
                    insert = Some((
                        sid,
                        local.clone(),
                        if want_audio {
                            WriteMode::Create
                        } else {
                            WriteMode::No
                        },
                    ));
                }
            }
        }

        let stats = &mut self.report.sounds;
        if let Some(existing_id) = skipped_to {
            stats.skipped += 1;
            self.report.id_map.sounds.insert(sid, existing_id);
            return Ok(());
        }
        if let Some((index, path, mode)) = replaced {
            stats.replaced += 1;
            let id = parse_or(&self.config.sounds[index].id, sid);
            let old_path = self.config.sounds[index].path.clone();
            self.config.sounds[index] = make_entry(id, &path);
            self.sound_by_path.remove(&old_path);
            self.sound_by_path.insert(local_string(&path), index);
            self.claimed.insert(path_key(&path));
            self.report.id_map.sounds.insert(sid, id);
            self.queue_write(checked, &path, mode);
            return Ok(());
        }
        if let Some((id, path, mode)) = insert {
            if renamed {
                stats.renamed += 1;
            } else {
                stats.added += 1;
            }
            let index = self.config.sounds.len();
            self.config.sounds.push(make_entry(id, &path));
            self.sound_by_id.insert(id, index);
            self.sound_by_path.insert(local_string(&path), index);
            self.claimed.insert(path_key(&path));
            self.report.id_map.sounds.insert(sid, id);
            self.queue_write(checked, &path, mode);
        }
        Ok(())
    }

    fn queue_write(&mut self, checked: &CheckedSound, dest: &Path, mode: WriteMode) {
        if mode == WriteMode::No {
            return;
        }
        let Some((entry_index, size)) = checked.entry else {
            return;
        };
        self.writes.push(PlannedWrite {
            entry_index,
            entry_name: format!("{AUDIO_DIR}/{}", checked.rel),
            dest: dest.to_path_buf(),
            overwrite: mode == WriteMode::Overwrite,
            size,
        });
    }

    /// `"a (2).wav"`, `"a (3).wav"`... the first variant of `wanted` that no
    /// sound, no earlier import step and no file on disk uses.
    fn unique_path(&self, wanted: &Path) -> PathBuf {
        let dir = wanted.parent().map(Path::to_path_buf).unwrap_or_default();
        let name = wanted
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for n in 2..100_000 {
            let candidate = dir.join(numbered_name(&name, n));
            if !self.claimed.contains(&path_key(&candidate))
                && !self.sound_by_path.contains_key(&local_string(&candidate))
                && path_kind(&candidate) == PathKind::Missing
            {
                return candidate;
            }
        }
        // Practically unreachable; a random name still never collides.
        dir.join(format!("{}-{}", Uuid::new_v4().simple(), name))
    }

    // --------------------------------------------------------- playlists --

    fn plan_playlists(&mut self) {
        let loaded = self.loaded;
        for (playlist, &pid) in loaded.manifest.playlists.iter().zip(&loaded.playlist_ids) {
            let mut name = clean_name(&playlist.name, MAX_NAME_LEN);
            if name.is_empty() {
                name = "Playlist".to_string();
            }
            let items: Vec<String> = playlist
                .items
                .iter()
                .map(|raw| self.map_sound_id(raw))
                .collect();
            let build = |id: String, name: String| PlaylistConfig {
                id,
                name,
                items: items.clone(),
                shuffle: playlist.shuffle,
                repeat: playlist.repeat,
                crossfade_ms: playlist.crossfade_ms,
                bus: playlist.bus,
            };
            let by_id = position_by_id(self.config.playlists.iter().map(|p| p.id.as_str()), pid);
            let by_name = self
                .config
                .playlists
                .iter()
                .position(|p| p.name.to_lowercase() == name.to_lowercase());
            let stats = &mut self.report.playlists;
            match by_id.or(by_name) {
                None => {
                    self.config.playlists.push(build(pid.to_string(), name));
                    self.report.id_map.playlists.insert(pid, pid);
                    stats.added += 1;
                }
                Some(i) => match self.policy {
                    ConflictPolicy::Skip => {
                        self.report
                            .id_map
                            .playlists
                            .insert(pid, parse_or(&self.config.playlists[i].id, pid));
                        stats.skipped += 1;
                    }
                    ConflictPolicy::Replace => {
                        let existing = self.config.playlists[i].id.clone();
                        self.config.playlists[i] = build(existing.clone(), name);
                        self.report
                            .id_map
                            .playlists
                            .insert(pid, parse_or(&existing, pid));
                        stats.replaced += 1;
                    }
                    ConflictPolicy::Rename => {
                        let id = if by_id.is_some() { Uuid::new_v4() } else { pid };
                        let name = unique_name(&name, MAX_NAME_LEN, |candidate| {
                            self.config
                                .playlists
                                .iter()
                                .any(|p| p.name.to_lowercase() == candidate.to_lowercase())
                        });
                        self.config.playlists.push(build(id.to_string(), name));
                        self.report.id_map.playlists.insert(pid, id);
                        stats.renamed += 1;
                    }
                },
            }
        }
    }

    // ------------------------------------------------------------ scenes --

    fn plan_scenes(&mut self) {
        let loaded = self.loaded;
        for (scene, &sid) in loaded.manifest.scenes.iter().zip(&loaded.scene_ids) {
            let mut name = clean_name(&scene.name, MAX_NAME_LEN);
            if name.is_empty() {
                name = "Scene".to_string();
            }
            let mut mapped = scene.clone();
            for item in &mut mapped.items {
                item.sound = self.map_sound_id(&item.sound);
            }
            for emitter in &mut mapped.emitters {
                for sound in &mut emitter.sounds {
                    *sound = self.map_sound_id(sound);
                }
            }
            mapped.playlist =
                scene
                    .playlist
                    .as_deref()
                    .map(|raw| match Uuid::parse_str(raw.trim()) {
                        Ok(id) => self
                            .report
                            .id_map
                            .playlists
                            .get(&id)
                            .copied()
                            .unwrap_or(id)
                            .to_string(),
                        Err(_) => raw.to_string(),
                    });

            let by_id = position_by_id(self.config.scenes.iter().map(|s| s.id.as_str()), sid);
            let by_name = self
                .config
                .scenes
                .iter()
                .position(|s| s.name.to_lowercase() == name.to_lowercase());
            let stats = &mut self.report.scenes;
            match by_id.or(by_name) {
                None => {
                    mapped.id = sid.to_string();
                    mapped.name = name;
                    self.config.scenes.push(mapped);
                    self.report.id_map.scenes.insert(sid, sid);
                    stats.added += 1;
                }
                Some(i) => match self.policy {
                    ConflictPolicy::Skip => {
                        self.report
                            .id_map
                            .scenes
                            .insert(sid, parse_or(&self.config.scenes[i].id, sid));
                        stats.skipped += 1;
                    }
                    ConflictPolicy::Replace => {
                        let existing = self.config.scenes[i].id.clone();
                        mapped.id = existing.clone();
                        mapped.name = name;
                        self.config.scenes[i] = mapped;
                        self.report
                            .id_map
                            .scenes
                            .insert(sid, parse_or(&existing, sid));
                        stats.replaced += 1;
                    }
                    ConflictPolicy::Rename => {
                        let id = if by_id.is_some() { Uuid::new_v4() } else { sid };
                        mapped.id = id.to_string();
                        mapped.name = unique_name(&name, MAX_NAME_LEN, |candidate| {
                            self.config
                                .scenes
                                .iter()
                                .any(|s| s.name.to_lowercase() == candidate.to_lowercase())
                        });
                        self.config.scenes.push(mapped);
                        self.report.id_map.scenes.insert(sid, id);
                        stats.renamed += 1;
                    }
                },
            }
        }
    }

    /// The library id for a sound id written in the bundle; ids the bundle
    /// does not define are kept as they are.
    fn map_sound_id(&self, raw: &str) -> String {
        match Uuid::parse_str(raw.trim()) {
            Ok(id) => self
                .report
                .id_map
                .sounds
                .get(&id)
                .copied()
                .unwrap_or(id)
                .to_string(),
            Err(_) => raw.to_string(),
        }
    }
}

fn position_by_id<'a>(mut ids: impl Iterator<Item = &'a str>, wanted: Uuid) -> Option<usize> {
    ids.position(|raw| Uuid::parse_str(raw.trim()).ok() == Some(wanted))
}

fn parse_or(raw: &str, fallback: Uuid) -> Uuid {
    Uuid::parse_str(raw.trim()).unwrap_or(fallback)
}

/// Strip control characters, trim, and cut to `max` characters.
fn clean_name(raw: &str, max: usize) -> String {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let cut: String = cleaned.trim().chars().take(max).collect();
    cut.trim().to_string()
}

/// `"Tavern"` -> `"Tavern (2)"`... the first variant `taken` does not know,
/// within `max` characters.
fn unique_name(base: &str, max: usize, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    for n in 2..100_000 {
        let suffix = format!(" ({n})");
        let room = max.saturating_sub(suffix.chars().count());
        let stem: String = base.chars().take(room).collect();
        let candidate = format!("{}{suffix}", stem.trim_end());
        if !taken(&candidate) {
            return candidate;
        }
    }
    format!("{base} {}", Uuid::new_v4().simple())
}

fn local_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Key for "is this path taken", folding case because the target may be a
/// case-insensitive file system.
fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Missing,
    /// A file, or a symbolic link (not followed).
    File,
    /// A directory, or something that cannot be inspected.
    Other,
}

fn path_kind(path: &Path) -> PathKind {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => PathKind::Other,
        Ok(_) => PathKind::File,
        Err(e) if e.kind() == io::ErrorKind::NotFound => PathKind::Missing,
        Err(_) => PathKind::Other,
    }
}

// -------------------------------------------------------------- extraction --

/// Write every planned file: first all of them to temporary names, then
/// publish them under their final names. Failure or cancellation removes what
/// was created.
fn extract_all(
    archive: &mut ZipArchive<File>,
    writes: &[PlannedWrite],
    target: &Path,
    options: &ImportOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&BundleProgress),
) -> Result<(), BundleError> {
    if writes.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(target)?;
    let canonical_target = fs::canonicalize(target)?;

    let mut staged: Vec<(PathBuf, &PlannedWrite)> = Vec::with_capacity(writes.len());
    let outcome = stage_files(
        archive,
        writes,
        &canonical_target,
        &options.limits,
        cancel,
        progress,
        &mut staged,
    )
    .and_then(|()| publish_files(&mut staged));
    match outcome {
        Ok(()) => {
            progress(&BundleProgress {
                done: writes.len(),
                total: writes.len(),
                current: String::new(),
            });
            Ok(())
        }
        Err((error, created)) => {
            for (tmp, _) in &staged {
                let _ = fs::remove_file(tmp);
            }
            for path in &created {
                let _ = fs::remove_file(path);
            }
            for write in writes {
                remove_empty_parents(&write.dest, target);
            }
            Err(error)
        }
    }
}

/// Extract every planned entry into a temporary file next to its destination.
fn stage_files<'a>(
    archive: &mut ZipArchive<File>,
    writes: &'a [PlannedWrite],
    canonical_target: &Path,
    limits: &ImportLimits,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&BundleProgress),
    staged: &mut Vec<(PathBuf, &'a PlannedWrite)>,
) -> Result<(), (BundleError, Vec<PathBuf>)> {
    let fail = |error: BundleError| (error, Vec::new());
    let mut total_left = limits.max_total_bytes;
    for (done, write) in writes.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(fail(BundleError::Cancelled));
        }
        progress(&BundleProgress {
            done,
            total: writes.len(),
            current: write.entry_name.clone(),
        });
        let parent = write
            .dest
            .parent()
            .ok_or_else(|| fail(BundleError::UnsafePath(write.entry_name.clone())))?;
        fs::create_dir_all(parent).map_err(|e| fail(e.into()))?;
        let real_parent = fs::canonicalize(parent).map_err(|e| fail(e.into()))?;
        if !real_parent.starts_with(canonical_target) {
            return Err(fail(BundleError::UnsafePath(format!(
                "{:?} would be written outside the target folder",
                write.entry_name
            ))));
        }

        let tmp = parent.join(format!(".bwimport-{}.tmp", Uuid::new_v4().simple()));
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| fail(e.into()))?;
        staged.push((tmp.clone(), write));

        let entry = archive
            .by_index(write.entry_index)
            .map_err(|e| fail(e.into()))?;
        let allowed = limits.max_file_bytes.min(total_left);
        let mut reader = entry.take(allowed + 1);
        let mut buffer = vec![0u8; CHUNK];
        let mut copied: u64 = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(fail(BundleError::Cancelled));
            }
            let read = reader.read(&mut buffer).map_err(|e| fail(e.into()))?;
            if read == 0 {
                break;
            }
            copied += read as u64;
            if copied > allowed {
                return Err(fail(BundleError::LimitExceeded(format!(
                    "{:?} is larger than the allowed size",
                    write.entry_name
                ))));
            }
            out.write_all(&buffer[..read]).map_err(|e| fail(e.into()))?;
        }
        out.flush().map_err(|e| fail(e.into()))?;
        total_left -= copied;
    }
    Ok(())
}

/// Give every staged file its final name. On failure, returns the error and
/// the final paths that were already created by this step (`staged` then holds
/// the files that still have their temporary name).
fn publish_files(
    staged: &mut Vec<(PathBuf, &PlannedWrite)>,
) -> Result<(), (BundleError, Vec<PathBuf>)> {
    let mut created: Vec<PathBuf> = Vec::new();
    let mut remaining = std::mem::take(staged).into_iter();
    while let Some((tmp, write)) = remaining.next() {
        match publish_one(&tmp, &write.dest, write.overwrite) {
            Ok(()) => {
                if !write.overwrite {
                    created.push(write.dest.clone());
                }
            }
            Err(error) => {
                staged.push((tmp, write));
                staged.extend(remaining);
                return Err((error.into(), created));
            }
        }
    }
    Ok(())
}

/// Move `tmp` to `dest`; without `overwrite` an existing `dest` is an error.
fn publish_one(tmp: &Path, dest: &Path, overwrite: bool) -> io::Result<()> {
    if overwrite {
        return fs::rename(tmp, dest);
    }
    match fs::hard_link(tmp, dest) {
        Ok(()) => fs::remove_file(tmp),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            // No hard links on this file system: fall back to check + rename.
            if fs::symlink_metadata(dest).is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            fs::rename(tmp, dest)
        }
    }
}

/// Remove the empty folders between `file` and `stop` (exclusive).
fn remove_empty_parents(file: &Path, stop: &Path) {
    let mut dir = file.parent();
    while let Some(current) = dir {
        if current == stop || !current.starts_with(stop) {
            break;
        }
        if fs::remove_dir(current).is_err() {
            break;
        }
        dir = current.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_is_idempotent_and_safe() {
        for raw in [
            "normal.wav",
            "a:b",
            "..",
            ".",
            "",
            "   ",
            "a. ",
            "CON",
            "con.txt",
            "COM1.wav",
            "LPT9",
            "COM0",
            "a/b\\c",
            "tab\tname",
            "Ünï.wav",
            &"x".repeat(500),
            &"я".repeat(300),
            "trailing.dot.",
        ] {
            let once = sanitize_component(raw);
            assert_eq!(sanitize_component(&once), once, "{raw:?}");
            assert!(!once.is_empty());
            assert!(once.len() <= MAX_COMPONENT_BYTES);
            assert!(validate_rel_path(&once).is_ok(), "{once:?}");
        }
        assert_eq!(sanitize_component("normal.wav"), "normal.wav");
        assert_eq!(sanitize_component("CON"), "_CON");
    }

    #[test]
    fn rel_path_validation_rejects_traversal() {
        for bad in [
            "",
            "/abs.wav",
            "../x.wav",
            "a/../x.wav",
            "a/./x.wav",
            "a//x.wav",
            "a\\x.wav",
            "C:/x.wav",
            "a/b:c.wav",
            "a/CON.wav",
            "a/",
        ] {
            assert!(validate_rel_path(bad).is_err(), "{bad:?}");
        }
        assert!(validate_rel_path("Tavern/crowd.ogg").is_ok());
        assert!(validate_rel_path("музыка/гул.ogg").is_ok());
    }

    #[test]
    fn entry_names_are_checked() {
        for bad in [
            "/etc/passwd",
            "../evil",
            "audio/../../evil",
            "audio/a/../b",
            "a\\b",
            "C:evil",
            "audio//x",
            "./x",
            "",
        ] {
            assert!(validate_entry_name(bad).is_err(), "{bad:?}");
        }
        for good in [
            "bundle.toml",
            "audio/",
            "audio/a/b.wav",
            "audio/Таверна/x.ogg",
        ] {
            assert!(validate_entry_name(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn numbered_names_keep_the_extension() {
        assert_eq!(numbered_name("hit.wav", 2), "hit (2).wav");
        assert_eq!(numbered_name("noext", 3), "noext (3)");
        assert_eq!(numbered_name(".hidden", 2), ".hidden (2)");
    }

    #[test]
    fn unique_names_stay_within_the_limit() {
        let taken = |c: &str| c == "Tavern" || c == "Tavern (2)";
        assert_eq!(unique_name("Tavern", 32, taken), "Tavern (3)");
        let long = "x".repeat(32);
        let result = unique_name(&long, 32, |c| c == long);
        assert!(result.chars().count() <= 32);
        assert!(result.ends_with(" (2)"));
    }

    #[test]
    fn rel_paths_keep_structure_below_the_common_folder() {
        use crate::gui::groups::Library;
        let mut lib = Library::new();
        lib.insert_sound(Path::new("/m/Music/Tavern/a.ogg"), "", 1.0, None);
        lib.insert_sound(Path::new("/m/Music/Fx/b.wav"), "", 1.0, None);
        let rels = plan_rel_paths(lib.sounds());
        assert_eq!(rels, vec!["Tavern/a.ogg", "Fx/b.wav"]);
    }

    #[test]
    fn rel_paths_flatten_without_a_shared_folder_and_stay_unique() {
        use crate::gui::groups::Library;
        let mut lib = Library::new();
        lib.insert_sound(Path::new("/one/hit.wav"), "", 1.0, None);
        lib.insert_sound(Path::new("/two/hit.wav"), "", 1.0, None);
        lib.insert_sound(Path::new("/three/HIT.wav"), "", 1.0, None);
        let rels = plan_rel_paths(lib.sounds());
        assert_eq!(rels, vec!["hit.wav", "hit (2).wav", "HIT (3).wav"]);
    }

    #[test]
    fn windows_style_paths_split_on_any_system() {
        let parts = split_parts(Path::new(r"C:\Snd\Tavern\a.wav"));
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], Part::Anchor("C:".into()));
        assert_eq!(parts[3], Part::Name("a.wav".into()));
    }

    #[test]
    fn only_audio_extensions_pass_the_whitelist() {
        for ok in ["a.wav", "x/b.FLAC", "c.Mp3", "d.ogg", "e.m4a", "f.aac"] {
            assert!(has_audio_extension(ok), "{ok}");
        }
        for bad in [
            "a.exe",
            "b.sh",
            "c",
            "x/.wav",
            "d.wav.bat",
            "e.toml",
            "f.",
            "g\\h.dll",
        ] {
            assert!(!has_audio_extension(bad), "{bad}");
        }
    }

    #[test]
    fn import_skips_non_audio_files_with_a_warning() {
        let dir = std::env::temp_dir().join(format!(
            "bardwave-bundle-ext-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("b.zip");
        let manifest = format!(
            "format = \"bardwave-bundle\"\nversion = 1\n\n\
             [[sounds]]\nid = \"{}\"\nname = \"ok\"\nrel_path = \"ok.wav\"\naudio = true\n\n\
             [[sounds]]\nid = \"{}\"\nname = \"bad\"\nrel_path = \"run.sh\"\naudio = true\n",
            Uuid::new_v4(),
            Uuid::new_v4()
        );
        {
            let mut zip = ZipWriter::new(File::create(&zip_path).unwrap());
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            for (name, data) in [
                ("bundle.toml", manifest.as_bytes()),
                ("audio/ok.wav", b"RIFF" as &[u8]),
                ("audio/run.sh", b"#!/bin/sh" as &[u8]),
            ] {
                zip.start_file(name, options).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        let target = dir.join("dest");
        let mut library = Library::new();
        let report = import_bundle(
            &mut library,
            &zip_path,
            &ImportOptions::new(&target, ConflictPolicy::Skip),
        )
        .unwrap();
        assert!(target.join("ok.wav").is_file());
        assert!(!target.join("run.sh").exists());
        assert_eq!(library.len(), 2);
        assert_eq!(report.files_written, 1);
        assert!(report.warnings.iter().any(|w| w.contains("run.sh")));
        let _ = fs::remove_dir_all(&dir);
    }
}
