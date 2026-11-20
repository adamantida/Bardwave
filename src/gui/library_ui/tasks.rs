//! Background work of the library UI.
//!
//! Native file dialogs, file-system checks and bundle import/export all block,
//! so none of them runs inside a frame. Each one runs on its own short-lived
//! thread and reports a [`LibEvent`] through one channel; the UI drains the
//! channel once per frame (see `ops::poll`) and the worker wakes the window with
//! `request_repaint`. Events that belong to a dialog carry its `token`, so the
//! answer of a dialog the operator already closed is ignored.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::config::LibraryConfig;
use crate::gui::groups::Library;
use crate::library_bundle::{
    export_bundle_with, import_bundle_with, inspect_bundle, BundleError, BundleInfo,
    BundleProgress, ExportOptions, ExportReport, ImportLimits, ImportOptions, ImportReport,
};
use crate::library_scan::AUDIO_EXTENSIONS;

use super::logic::{check_missing, MissingCheck};

/// Longest gap between two progress events of a bundle job.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(60);

/// What a worker reports to the UI.
#[derive(Debug)]
pub enum LibEvent {
    /// "Add folder" dialog answered.
    FolderPicked { token: u64, path: Option<PathBuf> },
    /// "Locate" dialog answered for the sound at `old`.
    RelinkPicked { old: PathBuf, new: Option<PathBuf> },
    /// "Browse" in the replace-prefix dialog answered.
    PrefixPicked { token: u64, path: Option<PathBuf> },
    /// Export destination chosen.
    ExportDestPicked { token: u64, path: Option<PathBuf> },
    /// Bundle to import chosen.
    ImportZipPicked { token: u64, path: Option<PathBuf> },
    /// Import target folder chosen.
    ImportTargetPicked { token: u64, path: Option<PathBuf> },
    /// "Add Sound" dialog answered.
    FilesPicked(Vec<PathBuf>),
    /// A "which files are gone" check finished.
    Missing(MissingCheck),
    /// A bundle was opened and validated.
    BundleInspected {
        token: u64,
        result: Result<BundleInfo, BundleError>,
    },
    /// A dry-run import finished.
    ImportPreview {
        token: u64,
        seq: u64,
        result: Result<ImportReport, BundleError>,
    },
    /// Export or import progress.
    Progress {
        token: u64,
        progress: BundleProgress,
    },
    /// Export finished.
    ExportDone {
        token: u64,
        result: Result<ExportReport, BundleError>,
    },
    /// Import finished: the report and the library content before and after
    /// the import, for the UI to merge into the live library.
    ImportDone {
        token: u64,
        result: Result<(ImportReport, Box<ImportedConfig>), BundleError>,
    },
}

/// The library content around an import, as seen by the import worker.
#[derive(Debug, Clone)]
pub struct ImportedConfig {
    /// The library the worker started from (a snapshot taken when the import
    /// began).
    pub base: LibraryConfig,
    /// That library after the bundle was merged into it.
    pub imported: LibraryConfig,
}

/// Starts background jobs; cheap to clone.
#[derive(Clone)]
pub struct Tasks {
    tx: Sender<LibEvent>,
    ctx: egui::Context,
}

impl Tasks {
    /// Jobs report into `tx` and wake `ctx`.
    pub fn new(tx: Sender<LibEvent>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    fn send(&self, event: LibEvent) {
        // The receiver lives as long as the app; a failed send only happens
        // while it shuts down.
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }

    fn spawn(&self, name: &str, job: impl FnOnce(&Tasks) + Send + 'static) -> bool {
        let tasks = self.clone();
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || job(&tasks))
            .is_ok()
    }

    /// Run a native dialog off the UI thread and report its answer through
    /// `done` (`None` when the operator cancelled or no thread could start).
    fn dialog(
        &self,
        name: &str,
        pick: impl FnOnce() -> Option<PathBuf> + Send + 'static,
        done: impl FnOnce(Option<PathBuf>) -> LibEvent + Send + 'static,
    ) {
        // `done` must survive a failed spawn, so both sides share it.
        let slot = Arc::new(std::sync::Mutex::new(Some(done)));
        let for_thread = Arc::clone(&slot);
        let spawned = self.spawn(name, move |tasks| {
            let picked = pick();
            if let Some(done) = take(&for_thread) {
                tasks.send(done(picked));
            }
        });
        if !spawned {
            if let Some(done) = take(&slot) {
                self.send(done(None));
            }
        }
    }

    /// Folder chooser (answers with `None` when cancelled).
    pub fn pick_folder(
        &self,
        title: &str,
        start: Option<PathBuf>,
        done: impl FnOnce(Option<PathBuf>) -> LibEvent + Send + 'static,
    ) {
        let title = title.to_string();
        self.dialog(
            "library-folder-dialog",
            move || {
                let mut dialog = rfd::FileDialog::new().set_title(title);
                if let Some(dir) = start {
                    dialog = dialog.set_directory(dir);
                }
                dialog.pick_folder()
            },
            done,
        );
    }

    /// Audio-file chooser for relinking one sound.
    pub fn pick_audio_file(
        &self,
        title: &str,
        start: Option<PathBuf>,
        done: impl FnOnce(Option<PathBuf>) -> LibEvent + Send + 'static,
    ) {
        self.pick_file_with(title, start, "Audio files", AUDIO_EXTENSIONS, done);
    }

    /// Zip chooser for importing a bundle.
    pub fn pick_zip(
        &self,
        title: &str,
        done: impl FnOnce(Option<PathBuf>) -> LibEvent + Send + 'static,
    ) {
        self.pick_file_with(title, None, "Bardwave bundle", &["zip"], done);
    }

    fn pick_file_with(
        &self,
        title: &str,
        start: Option<PathBuf>,
        filter_name: &'static str,
        extensions: &'static [&'static str],
        done: impl FnOnce(Option<PathBuf>) -> LibEvent + Send + 'static,
    ) {
        let title = title.to_string();
        self.dialog(
            "library-file-dialog",
            move || {
                let mut dialog = rfd::FileDialog::new()
                    .set_title(title)
                    .add_filter(filter_name, extensions);
                if let Some(dir) = start {
                    dialog = dialog.set_directory(dir);
                }
                dialog.pick_file()
            },
            done,
        );
    }

    /// Multi-file chooser of the "Add Sound" button; the answer is a
    /// [`LibEvent::FilesPicked`] (nothing is sent when it is cancelled).
    pub fn pick_sound_files(&self) {
        self.spawn("library-add-files-dialog", |tasks| {
            let picked = rfd::FileDialog::new()
                .add_filter("Audio Files", &["mp3", "wav", "ogg", "flac", "opus"])
                .set_title("Select Audio Files")
                .pick_files();
            if let Some(files) = picked {
                tasks.send(LibEvent::FilesPicked(files));
            }
        });
    }

    /// "Save as" chooser for the export file.
    pub fn pick_save_zip(
        &self,
        title: &str,
        file_name: &str,
        done: impl FnOnce(Option<PathBuf>) -> LibEvent + Send + 'static,
    ) {
        let title = title.to_string();
        let file_name = file_name.to_string();
        self.dialog(
            "library-save-dialog",
            move || {
                rfd::FileDialog::new()
                    .set_title(title)
                    .set_file_name(file_name)
                    .add_filter("Bardwave bundle", &["zip"])
                    .save_file()
            },
            done,
        );
    }

    /// Look at every path on disk (see [`check_missing`]); the answer is a
    /// [`LibEvent::Missing`]. Returns false when no thread could be started.
    pub fn check_files(&self, paths: Vec<PathBuf>, reload: HashSet<PathBuf>) -> bool {
        self.spawn("library-file-check", move |tasks| {
            let result = check_missing(&paths, &reload, |path| path.is_file());
            tasks.send(LibEvent::Missing(result));
        })
    }

    /// Open and validate a bundle.
    pub fn inspect(&self, token: u64, zip: PathBuf) {
        let spawned = self.spawn("library-bundle-inspect", {
            let zip = zip.clone();
            move |tasks| {
                let result = inspect_bundle(&zip, &ImportLimits::default());
                tasks.send(LibEvent::BundleInspected { token, result });
            }
        });
        if !spawned {
            self.send(LibEvent::BundleInspected {
                token,
                result: Err(BundleError::Io(std::io::Error::other(
                    "could not start a worker thread",
                ))),
            });
        }
    }

    /// Dry-run an import against a private copy of the library.
    pub fn preview_import(
        &self,
        token: u64,
        seq: u64,
        mut library: Library,
        zip: PathBuf,
        mut options: ImportOptions,
    ) {
        options.dry_run = true;
        self.spawn("library-import-preview", move |tasks| {
            let result = import_bundle_with(
                &mut library,
                &zip,
                &options,
                &AtomicBool::new(false),
                &mut |_| {},
            );
            tasks.send(LibEvent::ImportPreview { token, seq, result });
        });
    }

    /// Write a bundle of `library` to `dest`.
    pub fn export(
        &self,
        token: u64,
        library: Library,
        dest: PathBuf,
        options: ExportOptions,
        cancel: Arc<AtomicBool>,
    ) {
        let spawned = self.spawn("library-export", move |tasks| {
            let mut throttle = Throttle::new();
            let result = export_bundle_with(&library, &dest, &options, &cancel, &mut |progress| {
                if throttle.ready(progress) {
                    tasks.send(LibEvent::Progress {
                        token,
                        progress: progress.clone(),
                    });
                }
            });
            tasks.send(LibEvent::ExportDone { token, result });
        });
        if !spawned {
            self.send(LibEvent::ExportDone {
                token,
                result: Err(BundleError::Io(std::io::Error::other(
                    "could not start a worker thread",
                ))),
            });
        }
    }

    /// Import `zip` into a private copy of `library` and report the library
    /// before and after. The UI merges the difference into the live library
    /// by id (see `merge_import` in `ops.rs`), so edits made while the import
    /// ran survive, and a failed or cancelled import leaves it untouched.
    pub fn import(
        &self,
        token: u64,
        mut library: Library,
        zip: PathBuf,
        options: ImportOptions,
        cancel: Arc<AtomicBool>,
    ) {
        let spawned = self.spawn("library-import", move |tasks| {
            let base = library.to_config();
            let mut throttle = Throttle::new();
            let result =
                import_bundle_with(&mut library, &zip, &options, &cancel, &mut |progress| {
                    if throttle.ready(progress) {
                        tasks.send(LibEvent::Progress {
                            token,
                            progress: progress.clone(),
                        });
                    }
                })
                .map(|report| {
                    let imported = library.to_config();
                    (report, Box::new(ImportedConfig { base, imported }))
                });
            tasks.send(LibEvent::ImportDone { token, result });
        });
        if !spawned {
            self.send(LibEvent::ImportDone {
                token,
                result: Err(BundleError::Io(std::io::Error::other(
                    "could not start a worker thread",
                ))),
            });
        }
    }
}

fn take<T>(slot: &Arc<std::sync::Mutex<Option<T>>>) -> Option<T> {
    slot.lock().ok().and_then(|mut guard| guard.take())
}

/// Lets at most one progress event through per [`PROGRESS_INTERVAL`] (and the
/// last one), so a bundle of thousands of small files does not flood the UI.
struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    fn new() -> Self {
        Self { last: None }
    }

    fn ready(&mut self, progress: &BundleProgress) -> bool {
        let due = self.last.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL);
        if due || progress.done >= progress.total {
            self.last = Some(Instant::now());
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_passes_first_and_last() {
        let mut throttle = Throttle::new();
        let step = |done| BundleProgress {
            done,
            total: 100,
            current: String::new(),
        };
        assert!(throttle.ready(&step(0)));
        // Immediately after: held back.
        assert!(!throttle.ready(&step(1)));
        assert!(!throttle.ready(&step(50)));
        // The final event always passes.
        assert!(throttle.ready(&step(100)));
    }

    #[test]
    fn file_check_worker_reports_through_the_channel() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let tasks = Tasks::new(tx, egui::Context::default());
        let dir = std::env::temp_dir().join(format!("bardwave-ui-check-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let present = dir.join("a.wav");
        std::fs::write(&present, b"x").unwrap();
        let gone = dir.join("gone.wav");
        let reload: HashSet<PathBuf> = [present.clone()].into_iter().collect();
        assert!(tasks.check_files(vec![present.clone(), gone.clone()], reload));
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            LibEvent::Missing(check) => {
                assert!(check.missing.contains(&gone));
                assert!(!check.missing.contains(&present));
                assert_eq!(check.reload, [present]);
            }
            other => panic!("unexpected event {other:?}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
