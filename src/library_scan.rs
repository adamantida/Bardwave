//! Bulk adding of sounds: recursive folder scan with duplicate detection.
//!
//! [`start_scan`] walks a folder on its own thread and reports through a
//! channel, so the GUI never blocks:
//!
//! ```no_run
//! # use soundshared::library_scan::*;
//! # use std::path::PathBuf;
//! let handle = start_scan(PathBuf::from("/sounds"), Vec::new(), ScanOptions::default())
//!     .expect("spawn scan thread");
//! // every frame:
//! while let Ok(event) = handle.events().try_recv() {
//!     match event {
//!         ScanEvent::Progress(stats) => println!("{} found", stats.found),
//!         ScanEvent::Done(report) => println!("{} new sounds", report.files.len()),
//!     }
//! }
//! // "Cancel" button: handle.cancel();
//! ```
//!
//! The scan itself changes nothing. The finished [`ScanReport`] lists the new
//! files; [`ScanReport::apply_to_library`] adds them to a
//! [`Library`](crate::gui::groups::Library) (one group per first-level
//! sub-folder, on request).
//!
//! # Rules
//!
//! * Audio files are those with the extension `wav`, `flac`, `mp3`, `ogg`,
//!   `m4a` or `aac`, in any letter case ([`is_audio_file`]). AppleDouble
//!   companions (`._name.mp3`) and empty files are ignored.
//! * Entries are visited in sorted order (a folder's own files first, then its
//!   sub-folders, depth first), so the result does not depend on the file
//!   system's listing order, and "the first of two identical files wins" is
//!   predictable. Files reached through a symbolic link come last, so a link
//!   never wins over the file it points to.
//! * Duplicates are skipped by **path** (the file is already in the library)
//!   and by **content**: same size and same hash of the first and last
//!   [`FINGERPRINT_CHUNK`] bytes ([`fingerprint_file`]). Content is compared
//!   only between files of equal size, and also against the files already in
//!   the library, so the same sound copied to another folder is not added
//!   twice.
//! * Symbolic links are followed only while the target stays inside the
//!   scanned folder: a link to a file or folder outside it (or a broken one)
//!   is skipped, and a folder is never entered twice, so link loops end.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};

use crate::config::default_sound_volume;
use crate::gui::groups::{Library, MAX_GROUP_NAME_LEN};

/// Extensions (lower-case, no dot) the scanner treats as audio.
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "flac", "mp3", "ogg", "m4a", "aac"];

/// How many bytes at the start and at the end of a file the content hash
/// covers. Files up to twice this size are hashed completely.
pub const FINGERPRINT_CHUNK: u64 = 64 * 1024;

/// Most error messages kept in a [`ScanReport`] (the count is exact anyway).
const MAX_ERRORS_KEPT: usize = 200;

/// Minimum time between two progress events.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Is `path` named like an audio file ([`AUDIO_EXTENSIONS`], any letter case)?
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// Cheap identity of a file's content: its size plus a hash of its first and
/// last [`FINGERPRINT_CHUNK`] bytes. Equal fingerprints mean "almost certainly
/// the same file"; it is a duplicate filter, not a cryptographic guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    /// Size in bytes.
    pub size: u64,
    /// FNV-1a hash of the sampled bytes.
    pub hash: u64,
}

/// Compute the [`Fingerprint`] of a file.
pub fn fingerprint_file(path: &Path) -> io::Result<Fingerprint> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    Ok(Fingerprint {
        size,
        hash: hash_sampled(&mut file, size)?,
    })
}

/// FNV-1a over the first and last chunk of a file of `size` bytes.
fn hash_sampled(file: &mut File, size: u64) -> io::Result<u64> {
    let mut hash = FNV_OFFSET;
    let mut buffer = vec![0u8; FINGERPRINT_CHUNK as usize];
    if size <= 2 * FINGERPRINT_CHUNK {
        file.seek(SeekFrom::Start(0))?;
        let mut left = size;
        while left > 0 {
            let want = left.min(FINGERPRINT_CHUNK) as usize;
            file.read_exact(&mut buffer[..want])?;
            hash = fnv1a(hash, &buffer[..want]);
            left -= want as u64;
        }
    } else {
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut buffer)?;
        hash = fnv1a(hash, &buffer);
        file.seek(SeekFrom::Start(size - FINGERPRINT_CHUNK))?;
        file.read_exact(&mut buffer)?;
        hash = fnv1a(hash, &buffer);
    }
    Ok(hash)
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// The paths of every sound already in `library`, to pass to [`start_scan`] /
/// [`scan_dir`] as the "already known" set.
pub fn known_paths(library: &Library) -> Vec<PathBuf> {
    library.sounds().iter().map(|s| s.path.clone()).collect()
}

/// Scan settings.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Deepest folder level visited (the scanned folder is level 0). Guards
    /// against absurdly deep trees; the default is generous.
    pub max_depth: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { max_depth: 64 }
    }
}

/// A new audio file found by the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    /// Path as walked: the scanned folder joined with the relative path (links
    /// are not resolved, the scanned folder is not canonicalized).
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Name of the first-level sub-folder the file lives under (at any depth),
    /// or `None` for a file directly in the scanned folder.
    pub folder: Option<String>,
}

/// Running totals, sent as progress and kept in the final report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// Folders entered.
    pub dirs_visited: usize,
    /// Audio files examined.
    pub audio_seen: usize,
    /// New files found so far.
    pub found: usize,
    /// Skipped: the path is already in the library.
    pub skipped_existing: usize,
    /// Skipped: same content as another file (in the library or earlier in
    /// this scan).
    pub skipped_duplicates: usize,
    /// Skipped: empty file.
    pub skipped_empty: usize,
    /// Skipped: symbolic link that is broken or leaves the scanned folder.
    pub skipped_links: usize,
    /// Entries that could not be read (see [`ScanReport::errors`]).
    pub errors: usize,
}

/// One entry that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanIssue {
    /// The path concerned.
    pub path: PathBuf,
    /// What went wrong.
    pub message: String,
}

/// The outcome of a scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// The folder that was scanned, as given.
    pub root: PathBuf,
    /// New files, in visiting order (see the module docs).
    pub files: Vec<ScannedFile>,
    /// Final totals.
    pub stats: ScanStats,
    /// The first few unreadable entries.
    pub errors: Vec<ScanIssue>,
    /// True when the scan was stopped by [`ScanHandle::cancel`]: `files` is
    /// then what was found until that moment.
    pub cancelled: bool,
}

/// What a scan thread sends.
#[derive(Debug, Clone)]
pub enum ScanEvent {
    /// Totals so far (at most one per 100 ms, and some may be dropped when the
    /// receiver falls behind).
    Progress(ScanStats),
    /// The scan is over; always the last event.
    Done(ScanReport),
}

/// A running background scan. Dropping the handle cancels the scan.
pub struct ScanHandle {
    events: Receiver<ScanEvent>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ScanHandle {
    /// The event channel (poll it with `try_recv` from the UI thread).
    pub fn events(&self) -> &Receiver<ScanEvent> {
        &self.events
    }

    /// Ask the scan to stop; it ends with a [`ScanEvent::Done`] whose report
    /// has `cancelled == true`.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// True once the scan thread has ended (the final event may still be
    /// waiting in the channel).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Block until the scan is over and return its report (`None` only if the
    /// scan thread died). Skips progress events; for tests and non-UI callers.
    pub fn wait(mut self) -> Option<ScanReport> {
        let report = loop {
            match self.events.recv() {
                Ok(ScanEvent::Done(report)) => break Some(report),
                Ok(ScanEvent::Progress(_)) => continue,
                Err(_) => break None,
            }
        };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        report
    }
}

impl Drop for ScanHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Start scanning `root` on a new thread.
///
/// `known` are the paths of the sounds already in the library
/// ([`known_paths`]); they and their content are never reported again.
/// Fails only if the thread cannot be spawned.
pub fn start_scan(
    root: PathBuf,
    known: Vec<PathBuf>,
    options: ScanOptions,
) -> io::Result<ScanHandle> {
    let (tx, rx) = bounded::<ScanEvent>(256);
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let thread = std::thread::Builder::new()
        .name("library-scan".into())
        .spawn(move || run_scan_thread(&root, &known, &options, &flag, &tx))?;
    Ok(ScanHandle {
        events: rx,
        cancel,
        thread: Some(thread),
    })
}

fn run_scan_thread(
    root: &Path,
    known: &[PathBuf],
    options: &ScanOptions,
    cancel: &AtomicBool,
    tx: &Sender<ScanEvent>,
) {
    let report = scan_dir(root, known, options, cancel, |stats| {
        // A slow receiver loses progress events, never blocks the scan.
        if let Err(TrySendError::Disconnected(_)) = tx.try_send(ScanEvent::Progress(*stats)) {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    let _ = tx.send(ScanEvent::Done(report));
}

/// Scan `root` on the calling thread (what [`start_scan`] runs).
///
/// `on_progress` is called at most every 100 ms. `cancel` is checked for every
/// directory entry.
pub fn scan_dir(
    root: &Path,
    known: &[PathBuf],
    options: &ScanOptions,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&ScanStats),
) -> ScanReport {
    let mut sink = ScanSink {
        known_paths: known.iter().map(PathBuf::as_path).collect(),
        dups: DupIndex::seeded(known),
        report: ScanReport {
            root: root.to_path_buf(),
            ..ScanReport::default()
        },
        on_progress: &mut on_progress,
        last_progress: Instant::now(),
    };
    let finished = walk(root, options.max_depth, cancel, &mut sink);
    let mut report = sink.report;
    report.cancelled = !finished;
    report
}

struct ScanSink<'a> {
    known_paths: HashSet<&'a Path>,
    dups: DupIndex,
    report: ScanReport,
    on_progress: &'a mut dyn FnMut(&ScanStats),
    last_progress: Instant,
}

impl WalkSink for ScanSink<'_> {
    fn dir(&mut self, _dir: &Path) {
        self.report.stats.dirs_visited += 1;
        self.maybe_progress();
    }

    fn file(&mut self, file: WalkFile) {
        let stats = &mut self.report.stats;
        stats.audio_seen += 1;
        if self.known_paths.contains(file.path.as_path()) {
            stats.skipped_existing += 1;
        } else if self.dups.is_duplicate(&file.path, file.size) {
            stats.skipped_duplicates += 1;
        } else {
            stats.found += 1;
            self.report.files.push(ScannedFile {
                path: file.path,
                size: file.size,
                folder: file.folder,
            });
        }
        self.maybe_progress();
    }

    fn empty_file(&mut self) {
        self.report.stats.skipped_empty += 1;
    }

    fn link_skipped(&mut self) {
        self.report.stats.skipped_links += 1;
    }

    fn error(&mut self, path: &Path, error: &io::Error) {
        self.report.stats.errors += 1;
        if self.report.errors.len() < MAX_ERRORS_KEPT {
            self.report.errors.push(ScanIssue {
                path: path.to_path_buf(),
                message: error.to_string(),
            });
        }
    }
}

impl ScanSink<'_> {
    fn maybe_progress(&mut self) {
        if self.last_progress.elapsed() >= PROGRESS_INTERVAL {
            self.last_progress = Instant::now();
            (self.on_progress)(&self.report.stats);
        }
    }
}

/// Files seen so far, by size, with lazily computed content hashes.
struct DupIndex {
    by_size: HashMap<u64, Vec<Indexed>>,
}

struct Indexed {
    path: PathBuf,
    hash: Option<u64>,
}

impl DupIndex {
    /// An index holding the files already in the library (those still on disk).
    fn seeded(known: &[PathBuf]) -> Self {
        let mut by_size: HashMap<u64, Vec<Indexed>> = HashMap::new();
        for path in known {
            if let Ok(meta) = fs::metadata(path) {
                if meta.is_file() && meta.len() > 0 {
                    by_size.entry(meta.len()).or_default().push(Indexed {
                        path: path.clone(),
                        hash: None,
                    });
                }
            }
        }
        Self { by_size }
    }

    /// Is a file with this content already indexed? If not, index it. Hashes
    /// are only computed when two files have the same size. An unreadable file
    /// is never a duplicate.
    fn is_duplicate(&mut self, path: &Path, size: u64) -> bool {
        let entries = self.by_size.entry(size).or_default();
        if entries.is_empty() {
            entries.push(Indexed {
                path: path.to_path_buf(),
                hash: None,
            });
            return false;
        }
        let Ok(hash) = hash_path(path, size) else {
            return false;
        };
        for other in entries.iter_mut() {
            if other.hash.is_none() {
                other.hash = hash_path(&other.path, size).ok();
            }
            if other.hash == Some(hash) {
                return true;
            }
        }
        entries.push(Indexed {
            path: path.to_path_buf(),
            hash: Some(hash),
        });
        false
    }
}

fn hash_path(path: &Path, size: u64) -> io::Result<u64> {
    let mut file = File::open(path)?;
    hash_sampled(&mut file, size)
}

impl ScanReport {
    /// Add the new files to `library`.
    ///
    /// The sound name is the file name without extension and the volume the
    /// default. With `auto_groups`, files under a first-level sub-folder go
    /// into a group of that name (created when missing, reusing an existing
    /// group with the same name regardless of case; names are cut to
    /// [`MAX_GROUP_NAME_LEN`] characters); other files stay ungrouped.
    /// A path that is already in the library is not added again.
    pub fn apply_to_library(&self, library: &mut Library, auto_groups: bool) -> ApplyStats {
        let mut stats = ApplyStats::default();
        for file in &self.files {
            let group = match (&file.folder, auto_groups) {
                (Some(folder), true) => group_for_folder(library, folder, &mut stats),
                _ => None,
            };
            let name = file
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if library.insert_sound(&file.path, &name, default_sound_volume(), group) {
                stats.added += 1;
            } else {
                stats.already_present += 1;
            }
        }
        stats
    }
}

/// What [`ScanReport::apply_to_library`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyStats {
    /// Sounds added.
    pub added: usize,
    /// Files that were already registered (nothing changed for them).
    pub already_present: usize,
    /// Groups created for sub-folders.
    pub groups_created: usize,
}

fn group_for_folder(
    library: &mut Library,
    folder: &str,
    stats: &mut ApplyStats,
) -> Option<crate::gui::groups::GroupId> {
    let name: String = folder.trim().chars().take(MAX_GROUP_NAME_LEN).collect();
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let lowered = name.to_lowercase();
    if let Some(group) = library
        .groups()
        .iter()
        .find(|g| g.name.to_lowercase() == lowered)
    {
        return Some(group.id);
    }
    let id = library.add_group(name)?;
    stats.groups_created += 1;
    Some(id)
}

/// Look for moved files: for every path in `wanted` (typically
/// [`Library::missing_sounds`]), list the audio files below `root` with the same
/// file name (compared case-insensitively). Paths with no candidate are absent
/// from the result; a path with several candidates lists all of them, in
/// visiting order, and the caller decides. Symbolic links follow the rules of
/// the module docs. Runs on the calling thread; returns what was found so far
/// when `cancel` is set.
pub fn locate_files(
    root: &Path,
    wanted: &[PathBuf],
    options: &ScanOptions,
    cancel: &AtomicBool,
) -> HashMap<PathBuf, Vec<PathBuf>> {
    let mut by_name: HashMap<String, Vec<&PathBuf>> = HashMap::new();
    for path in wanted {
        if let Some(name) = path.file_name() {
            by_name
                .entry(name.to_string_lossy().to_lowercase())
                .or_default()
                .push(path);
        }
    }
    let mut sink = LocateSink {
        by_name,
        found: HashMap::new(),
    };
    walk(root, options.max_depth, cancel, &mut sink);
    sink.found
}

struct LocateSink<'a> {
    by_name: HashMap<String, Vec<&'a PathBuf>>,
    found: HashMap<PathBuf, Vec<PathBuf>>,
}

impl WalkSink for LocateSink<'_> {
    fn file(&mut self, file: WalkFile) {
        let Some(name) = file.path.file_name() else {
            return;
        };
        if let Some(olds) = self.by_name.get(&name.to_string_lossy().to_lowercase()) {
            for old in olds {
                self.found
                    .entry((*old).clone())
                    .or_default()
                    .push(file.path.clone());
            }
        }
    }
}

// ------------------------------------------------------------------ walker --

/// An audio file the walker found.
struct WalkFile {
    path: PathBuf,
    size: u64,
    folder: Option<String>,
}

/// Receives what [`walk`] finds.
trait WalkSink {
    fn dir(&mut self, _dir: &Path) {}
    fn file(&mut self, file: WalkFile);
    fn empty_file(&mut self) {}
    fn link_skipped(&mut self) {}
    fn error(&mut self, _path: &Path, _error: &io::Error) {}
}

struct Pending {
    dir: PathBuf,
    depth: usize,
    folder: Option<String>,
}

/// Depth-first walk in sorted order. Returns false when cancelled.
fn walk(root: &Path, max_depth: usize, cancel: &AtomicBool, sink: &mut dyn WalkSink) -> bool {
    let canonical_root = match fs::canonicalize(root) {
        Ok(path) if path.is_dir() => path,
        Ok(_) => {
            sink.error(
                root,
                &io::Error::new(io::ErrorKind::InvalidInput, "not a folder"),
            );
            return true;
        }
        Err(error) => {
            sink.error(root, &error);
            return true;
        }
    };
    let mut visited: HashSet<PathBuf> = HashSet::new();
    visited.insert(canonical_root.clone());
    // Files reached through a symbolic link are reported last, so that a
    // real file wins over a link to it.
    let mut linked_files: Vec<WalkFile> = Vec::new();
    let mut stack = vec![Pending {
        dir: root.to_path_buf(),
        depth: 0,
        folder: None,
    }];

    while let Some(pending) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        sink.dir(&pending.dir);
        let mut entries: Vec<(std::ffi::OsString, fs::FileType)> = Vec::new();
        match fs::read_dir(&pending.dir) {
            Ok(read) => {
                for entry in read {
                    match entry.and_then(|e| e.file_type().map(|t| (e.file_name(), t))) {
                        Ok(item) => entries.push(item),
                        Err(error) => sink.error(&pending.dir, &error),
                    }
                }
            }
            Err(error) => {
                sink.error(&pending.dir, &error);
                continue;
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut subdirs: Vec<Pending> = Vec::new();
        for (name, file_type) in entries {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let path = pending.dir.join(&name);
            let is_link = file_type.is_symlink();
            let (is_dir, is_file, size) = if is_link {
                match resolve_link(&path, &canonical_root) {
                    Ok(Some(meta)) => (meta.is_dir(), meta.is_file(), meta.len()),
                    Ok(None) => {
                        sink.link_skipped();
                        continue;
                    }
                    Err(error) => {
                        sink.error(&path, &error);
                        continue;
                    }
                }
            } else if file_type.is_dir() {
                (true, false, 0)
            } else if file_type.is_file() {
                match fs::metadata(&path) {
                    Ok(meta) => (false, true, meta.len()),
                    Err(error) => {
                        sink.error(&path, &error);
                        continue;
                    }
                }
            } else {
                continue;
            };

            if is_dir {
                if pending.depth >= max_depth {
                    continue;
                }
                match fs::canonicalize(&path) {
                    Ok(canonical) => {
                        if !visited.insert(canonical) {
                            continue;
                        }
                    }
                    Err(error) => {
                        sink.error(&path, &error);
                        continue;
                    }
                }
                let folder = match (&pending.folder, pending.depth) {
                    (Some(folder), _) => Some(folder.clone()),
                    (None, 0) => Some(name.to_string_lossy().into_owned()),
                    (None, _) => None,
                };
                subdirs.push(Pending {
                    dir: path,
                    depth: pending.depth + 1,
                    folder,
                });
            } else if is_file {
                if name.to_string_lossy().starts_with("._") || !is_audio_file(&path) {
                    continue;
                }
                if size == 0 {
                    sink.empty_file();
                    continue;
                }
                let found = WalkFile {
                    path,
                    size,
                    folder: pending.folder.clone(),
                };
                if is_link {
                    linked_files.push(found);
                } else {
                    sink.file(found);
                }
            }
        }
        // Sorted order: the first sub-folder must be popped first.
        stack.extend(subdirs.into_iter().rev());
    }
    for found in linked_files {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        sink.file(found);
    }
    true
}

/// Resolve a symbolic link: `Ok(Some(metadata of the target))` when it leads to
/// a file or folder inside `canonical_root`, `Ok(None)` when it is broken or
/// leaves the root.
fn resolve_link(link: &Path, canonical_root: &Path) -> io::Result<Option<fs::Metadata>> {
    let target = match fs::canonicalize(link) {
        Ok(target) => target,
        // Dangling link or a loop: not an error worth reporting.
        Err(_) => return Ok(None),
    };
    if !target.starts_with(canonical_root) {
        return Ok(None);
    }
    fs::metadata(&target).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_extension_is_case_insensitive() {
        for name in [
            "a.wav", "a.WAV", "a.Flac", "a.mp3", "a.OGG", "a.m4a", "a.aac",
        ] {
            assert!(is_audio_file(Path::new(name)), "{name}");
        }
        for name in ["a.txt", "a", "wav", "a.wav.bak", ".wav", "a.opus"] {
            // ".wav" has no extension in Rust's view, "a.opus" is not listed.
            assert!(!is_audio_file(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn fnv_is_stable() {
        // Known FNV-1a 64 value of "a".
        assert_eq!(fnv1a(FNV_OFFSET, b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
