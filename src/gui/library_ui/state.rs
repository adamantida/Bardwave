//! State of the library UI: the search filter, the open dialog, the "file not
//! found" knowledge, the local preview and the loader queue.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use uuid::Uuid;

use crate::audio::SoundId;
use crate::library_bundle::{
    BundleInfo, BundleProgress, ConflictPolicy, ExportReport, ImportReport,
};
use crate::library_scan::{ScanHandle, ScanReport, ScanStats};

use super::logic::{RemapPreview, ScanSummary, ViewFilter};
use super::tasks::{LibEvent, Tasks};

/// How long a status message stays under the toolbar.
const TOAST_LIFETIME: Duration = Duration::from_secs(7);

/// How often the files of the library are looked at again.
pub(crate) const RECHECK_EVERY: Duration = Duration::from_secs(20);

/// Files handed to the loader at once; the rest wait their turn here so a big
/// folder does not start thousands of waiting threads.
pub(crate) const LOAD_WINDOW: usize = 24;

/// A load that has not finished after this long stops holding its slot in the
/// load window (a file the decoder rejects never shows up as loaded; the
/// console also frees the slot at once on a decode error). Short on purpose:
/// a slot freed early only means one more load runs alongside.
pub(crate) const LOAD_TIMEOUT: Duration = Duration::from_secs(12);

/// A short message for the operator.
#[derive(Debug, Clone)]
pub struct Toast {
    /// The text.
    pub text: String,
    /// True for a problem (drawn in the warning colour).
    pub error: bool,
    pub(crate) at: Instant,
}

/// The sound the operator is auditioning on this machine only.
#[derive(Debug, Clone)]
pub struct PreviewTrack {
    /// Engine id of the sound.
    pub sound: SoundId,
    /// Its path (to light the right row).
    pub path: PathBuf,
    /// The local-only track.
    pub track: Uuid,
}

/// Bookkeeping of the background "which files are gone" check.
#[derive(Debug, Default)]
pub(crate) struct CheckState {
    pub in_flight: bool,
    pub queued: bool,
    pub reload: HashSet<PathBuf>,
    pub last: Option<Instant>,
}

/// Everything the library panel keeps between frames.
pub struct LibraryUi {
    /// Search box and lit tag chips.
    pub filter: ViewFilter,
    /// The open dialog, if any.
    pub dialog: LibDialog,
    /// Registered paths whose file was not found at the last check.
    pub missing: HashSet<PathBuf>,
    pub(crate) tasks: Tasks,
    pub(crate) events: Receiver<LibEvent>,
    token: u64,
    pub(crate) check: CheckState,
    /// The running local preview.
    pub preview: Option<PreviewTrack>,
    toast: Option<Toast>,
    pub(crate) load_queue: VecDeque<PathBuf>,
    pub(crate) load_in_flight: HashMap<PathBuf, Instant>,
}

impl LibraryUi {
    /// A fresh state; `ctx` is woken by background jobs.
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, events) = crossbeam_channel::unbounded();
        Self {
            filter: ViewFilter::default(),
            dialog: LibDialog::None,
            missing: HashSet::new(),
            tasks: Tasks::new(tx, ctx),
            events,
            token: 0,
            check: CheckState::default(),
            preview: None,
            toast: None,
            load_queue: VecDeque::new(),
            load_in_flight: HashMap::new(),
        }
    }

    /// A new dialog identity; answers meant for an older dialog are ignored.
    pub(crate) fn next_token(&mut self) -> u64 {
        self.token += 1;
        self.token
    }

    /// Show a status message.
    pub fn notify(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: false,
            at: Instant::now(),
        });
    }

    /// Show a problem message.
    pub fn notify_error(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: true,
            at: Instant::now(),
        });
    }

    /// The message to show now (none once it has expired).
    pub fn toast(&self) -> Option<&Toast> {
        self.toast
            .as_ref()
            .filter(|toast| toast.at.elapsed() < TOAST_LIFETIME)
    }

    /// Drop an expired message; true while one is still showing.
    pub(crate) fn expire_toast(&mut self) -> bool {
        if self.toast().is_none() {
            self.toast = None;
        }
        self.toast.is_some()
    }

    /// Check the files again soon; `reload` lists paths that should be loaded
    /// into the engine if their file turns out to exist (after a relink).
    pub fn request_recheck(&mut self, reload: impl IntoIterator<Item = PathBuf>) {
        self.check.queued = true;
        self.check.reload.extend(reload);
    }

    /// Queue files for the engine loader.
    pub(crate) fn enqueue_loads(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        // Membership of the queue is checked through a set built once per call:
        // a linear `VecDeque::contains` per path is quadratic for a big folder.
        let mut queued: HashSet<PathBuf> = self.load_queue.iter().cloned().collect();
        for path in paths {
            if !self.load_in_flight.contains_key(&path) && queued.insert(path.clone()) {
                self.load_queue.push_back(path);
            }
        }
    }

    /// True while a dialog is open.
    pub fn dialog_open(&self) -> bool {
        !matches!(self.dialog, LibDialog::None)
    }

    /// Is `sound` the one being auditioned?
    pub fn is_previewing(&self, sound: SoundId) -> bool {
        self.preview.as_ref().is_some_and(|p| p.sound == sound)
    }
}

/// The dialogs of the library UI.
#[derive(Default)]
pub enum LibDialog {
    /// Nothing open.
    #[default]
    None,
    /// Add a folder of sounds.
    AddFolder(AddFolder),
    /// Replace a path prefix.
    Remap(Remap),
    /// Export a bundle.
    Export(Export),
    /// Import a bundle.
    Import(Import),
    /// Edit the tags of one sound.
    Tags(TagEdit),
}

/// "Add folder" dialog.
pub struct AddFolder {
    /// Identity of this dialog.
    pub token: u64,
    /// Create one group per first-level sub-folder.
    pub auto_groups: bool,
    /// Where it stands.
    pub phase: AddFolderPhase,
}

/// Steps of the "Add folder" dialog.
pub enum AddFolderPhase {
    /// The native folder chooser is open.
    Picking,
    /// The scan runs; dropping the handle cancels it.
    Scanning {
        root: PathBuf,
        handle: ScanHandle,
        stats: ScanStats,
    },
    /// The scan is over; the operator decides.
    Ready {
        report: ScanReport,
        summary: ScanSummary,
    },
    /// The scan could not start.
    Failed(String),
}

/// "Replace path prefix" dialog.
pub struct Remap {
    /// Identity of this dialog.
    pub token: u64,
    /// The prefix to replace.
    pub old: String,
    /// The prefix to put instead.
    pub new: String,
    /// Preview for the typed values: `(old, new, result)`.
    pub preview: Option<(String, String, RemapPreview)>,
}

/// "Export bundle" dialog.
pub struct Export {
    /// Identity of this dialog.
    pub token: u64,
    /// Title stored in the bundle and used as the file name.
    pub name: String,
    /// Put the audio files into the zip.
    pub include_audio: bool,
    /// Where it stands.
    pub phase: ExportPhase,
}

/// Steps of the export dialog.
pub enum ExportPhase {
    /// Choosing options.
    Idle,
    /// The save dialog is open.
    Picking,
    /// Writing the zip.
    Running {
        dest: PathBuf,
        cancel: Arc<AtomicBool>,
        progress: Option<BundleProgress>,
    },
    /// Finished.
    Done { dest: PathBuf, report: ExportReport },
    /// Failed.
    Failed { message: String, safety: bool },
}

/// "Import bundle" dialog.
pub struct Import {
    /// Identity of this dialog.
    pub token: u64,
    /// The chosen zip.
    pub zip: Option<PathBuf>,
    /// What the zip contains.
    pub info: Option<BundleInfo>,
    /// What to do on conflicts.
    pub policy: ConflictPolicy,
    /// Folder the sounds go to, as text.
    pub target: String,
    /// Sequence number of the latest dry run (older answers are ignored).
    pub preview_seq: u64,
    /// The `(policy, target)` the dry run was asked for.
    pub preview_key: Option<(ConflictPolicy, String)>,
    /// Result of the dry run.
    pub preview: ImportPreview,
    /// Where it stands.
    pub phase: ImportPhase,
}

/// The dry-run result shown in the import dialog.
pub enum ImportPreview {
    /// Not asked yet.
    None,
    /// Running.
    Computing,
    /// Done.
    Ready(Box<ImportReport>),
    /// The bundle would be refused.
    Failed { message: String, safety: bool },
}

/// Steps of the import dialog.
pub enum ImportPhase {
    /// The native file chooser is open.
    Picking,
    /// The zip is being read and validated.
    Inspecting,
    /// Choosing options.
    Ready,
    /// Importing.
    Running {
        cancel: Arc<AtomicBool>,
        progress: Option<BundleProgress>,
    },
    /// Finished.
    Done(Box<ImportReport>),
    /// Refused or failed.
    Failed { message: String, safety: bool },
}

/// Tag editor of one sound.
pub struct TagEdit {
    /// The sound.
    pub path: PathBuf,
    /// Text in the tag field.
    pub input: String,
    /// Why the last input was refused.
    pub error: Option<String>,
    /// Put the focus into the field on the first frame.
    pub focus: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui() -> LibraryUi {
        LibraryUi::new(egui::Context::default())
    }

    #[test]
    fn enqueue_loads_skips_duplicates_and_in_flight() {
        let mut ui = ui();
        ui.load_in_flight
            .insert(PathBuf::from("busy.wav"), Instant::now());
        ui.enqueue_loads(["a.wav", "b.wav", "a.wav", "busy.wav"].map(PathBuf::from));
        ui.enqueue_loads(["b.wav", "c.wav"].map(PathBuf::from));
        let queued: Vec<_> = ui.load_queue.iter().cloned().collect();
        assert_eq!(queued, ["a.wav", "b.wav", "c.wav"].map(PathBuf::from));
    }

    #[test]
    fn enqueue_loads_is_not_quadratic_for_a_big_folder() {
        let mut ui = ui();
        let start = Instant::now();
        ui.enqueue_loads((0..60_000).map(|i| PathBuf::from(format!("/samples/dir/{i}.wav"))));
        assert_eq!(ui.load_queue.len(), 60_000);
        // A linear scan per file takes tens of seconds here; a set takes ms.
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
