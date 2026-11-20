//! What the library UI does to the app: opening dialogs, applying their
//! results to the [`Library`](crate::gui::groups::Library), local preview, and
//! the per-frame [`poll`].
//!
//! Everything here runs on the UI thread and returns quickly; the slow parts
//! (file dialogs, scans, bundle jobs, `stat` calls) live in [`super::tasks`] and
//! come back as [`LibEvent`]s.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::audio::SoundId;
use crate::config::LibraryConfig;
use crate::gui::app::SoundSharedApp;
use crate::gui::groups::Library;
use crate::library_bundle::{ConflictPolicy, ExportOptions, ImportOptions};
use crate::library_query::LibraryQuery;
use crate::library_scan::{
    is_audio_file, known_paths, start_scan, ScanEvent, ScanOptions, ScanReport,
};

use super::logic::{
    build_rows, bundle_file_name, common_parent, default_import_dir, describe_bundle_error,
    is_safety_error, nearest_existing_ancestor, parse_tag_input, preview_remap, remap_moves,
    summarize_scan, with_zip_extension, RowItem,
};
use super::state::{
    AddFolder, AddFolderPhase, Export, ExportPhase, Import, ImportPhase, ImportPreview, LibDialog,
    PreviewTrack, Remap, TagEdit, LOAD_TIMEOUT, LOAD_WINDOW, RECHECK_EVERY,
};
use super::tasks::LibEvent;

/// Something a dialog asks for. The dialog code only records these while it
/// draws; they are carried out afterwards, when nothing is borrowed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Act {
    /// Close the dialog (a running scan is cancelled by dropping its handle).
    Close,
    /// Add the scanned files to the library.
    ApplyScan,
    /// Choose the replacement folder of the prefix dialog.
    RemapBrowse,
    /// Rewrite the prefix.
    RemapApply,
    /// Open the save dialog and start the export.
    ExportChoose,
    /// Stop the running export.
    ExportCancel,
    /// Choose another bundle.
    ImportPickAnother,
    /// Choose the target folder of the import.
    ImportBrowse,
    /// Dry-run the import again with the current options.
    ImportRefresh,
    /// Import for real.
    ImportStart,
    /// Stop the running import.
    ImportCancel,
    /// Add the typed tags to the sound.
    TagAdd(String),
    /// Remove a tag from the sound.
    TagRemove(String),
}

/// Per-frame work of the library UI. Call once per frame before drawing.
pub fn poll(state: &mut SoundSharedApp, ctx: &egui::Context) {
    let events: Vec<LibEvent> = state.lib_ui.events.try_iter().collect();
    for event in events {
        state.handle_lib_event(event);
    }
    state.poll_scan(ctx);
    state.poll_preview(ctx);
    state.schedule_file_check();
    state.feed_loads(ctx);
    if state.lib_ui.expire_toast() {
        ctx.request_repaint_after(Duration::from_millis(1000));
    }
}

/// Stop the local preview (used by the panic button too).
pub fn stop_preview(state: &mut SoundSharedApp) {
    state.stop_preview();
}

impl SoundSharedApp {
    // ------------------------------------------------------------- query ----

    /// The group, tag and text filter as one query.
    pub(crate) fn current_query(&self) -> LibraryQuery {
        self.lib_ui.filter.query(self.group_filter())
    }

    /// Rows of the sound list under the current filter.
    pub(crate) fn list_rows(&self) -> Vec<RowItem> {
        build_rows(
            self.get_sounds(),
            &self.library,
            &self.lib_ui.missing,
            &self.current_query(),
        )
    }

    /// Paths of the sounds loaded in the engine.
    pub(crate) fn loaded_paths(&self) -> HashSet<PathBuf> {
        self.sounds
            .lock()
            .unwrap()
            .values()
            .map(|entry| entry.path.clone())
            .collect()
    }

    // ----------------------------------------------------------- dialogs ----

    fn begin_dialog(&mut self) -> u64 {
        // One modal at a time: the older console modals give way.
        self.close_dialog();
        self.lib_ui.dialog = LibDialog::None;
        self.lib_ui.next_token()
    }

    /// "Add folder…": choose a folder, then scan it.
    pub fn open_add_folder(&mut self) {
        let token = self.begin_dialog();
        self.lib_ui.dialog = LibDialog::AddFolder(AddFolder {
            token,
            auto_groups: true,
            phase: AddFolderPhase::Picking,
        });
        self.lib_ui
            .tasks
            .pick_folder("Choose a folder with sounds", None, move |path| {
                LibEvent::FolderPicked { token, path }
            });
    }

    /// "Add Sound": choose audio files without blocking the frame.
    pub fn open_add_files(&mut self) {
        self.lib_ui.tasks.pick_sound_files();
    }

    /// "Export bundle…".
    pub fn open_export(&mut self) {
        let token = self.begin_dialog();
        self.lib_ui.dialog = LibDialog::Export(Export {
            token,
            name: "Bardwave set".to_string(),
            include_audio: true,
            phase: ExportPhase::Idle,
        });
    }

    /// "Import bundle…": choose a zip first.
    pub fn open_import(&mut self) {
        let token = self.begin_dialog();
        self.lib_ui.dialog = LibDialog::Import(Import {
            token,
            zip: None,
            info: None,
            policy: ConflictPolicy::Skip,
            target: String::new(),
            preview_seq: 0,
            preview_key: None,
            preview: ImportPreview::None,
            phase: ImportPhase::Picking,
        });
        self.lib_ui
            .tasks
            .pick_zip("Choose a bundle to import", move |path| {
                LibEvent::ImportZipPicked { token, path }
            });
    }

    /// "Replace path prefix…", pre-filled with the folder the missing files
    /// shared.
    pub fn open_remap(&mut self) {
        let missing: Vec<&Path> = self
            .library
            .sounds()
            .iter()
            .filter(|s| self.lib_ui.missing.contains(&s.path))
            .map(|s| s.path.as_path())
            .collect();
        let old = common_parent(&missing)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let token = self.begin_dialog();
        self.lib_ui.dialog = LibDialog::Remap(Remap {
            token,
            old,
            new: String::new(),
            preview: None,
        });
    }

    /// Edit the tags of the sound at `path`.
    pub fn open_tag_editor(&mut self, path: PathBuf) {
        if self.library.sound(&path).is_none() {
            return;
        }
        self.begin_dialog();
        self.lib_ui.dialog = LibDialog::Tags(TagEdit {
            path,
            input: String::new(),
            error: None,
            focus: true,
        });
    }

    /// Ask for the new location of a missing file.
    pub fn locate_sound(&mut self, path: PathBuf) {
        let start = nearest_existing_ancestor(&path, |dir| dir.is_dir());
        self.lib_ui
            .tasks
            .pick_audio_file("Locate the sound file", start, move |new| {
                LibEvent::RelinkPicked { old: path, new }
            });
    }

    /// Look at the files again now (the "Recheck" button).
    pub fn recheck_files(&mut self) {
        self.lib_ui.request_recheck(Vec::new());
    }

    /// Drop a missing sound from the library (and from the scenes that use it).
    pub fn remove_missing_sound(&mut self, path: &Path) {
        let name = self
            .library
            .sound(path)
            .map(|s| super::logic::display_name(&s.name, &s.path))
            .unwrap_or_default();
        if let Some(refs) = self.library.remove_sound_report(path) {
            self.library_dirty = true;
            self.lib_ui.missing.remove(path);
            let used = refs.scene_items + refs.playlist_items + refs.emitter_refs;
            if used > 0 {
                self.lib_ui.notify(format!(
                    "Removed \"{name}\" and {used} reference(s) to it in scenes and playlists"
                ));
            } else {
                self.lib_ui.notify(format!("Removed \"{name}\""));
            }
        }
    }

    // ------------------------------------------------------- applying -------

    /// Update the engine-side rows after the library moved sounds to new paths.
    pub(crate) fn apply_path_moves(&mut self, moves: &[(PathBuf, PathBuf)]) {
        if moves.is_empty() {
            return;
        }
        let map: HashMap<&Path, &PathBuf> = moves
            .iter()
            .map(|(old, new)| (old.as_path(), new))
            .collect();
        for entry in self.sounds.lock().unwrap().values_mut() {
            if let Some(new) = map.get(entry.path.as_path()) {
                entry.path = (*new).clone();
            }
        }
        for (old, _) in moves {
            self.lib_ui.missing.remove(old);
        }
    }

    /// Copy group membership from the library into the loaded rows.
    fn mirror_groups(&mut self) {
        for entry in self.sounds.lock().unwrap().values_mut() {
            entry.group = self.library.sound(&entry.path).and_then(|s| s.group);
        }
    }

    /// Replace the library content (an import) and bring the rows in line.
    fn replace_library(&mut self, config: &LibraryConfig) {
        let known: HashSet<PathBuf> = self
            .library
            .sounds()
            .iter()
            .map(|s| s.path.clone())
            .collect();
        let loaded: Vec<(PathBuf, Uuid)> = self
            .sounds
            .lock()
            .unwrap()
            .values()
            .filter_map(|e| self.library.id_of(&e.path).map(|id| (e.path.clone(), id)))
            .collect();
        self.library.apply_library_config(config);
        let moves: Vec<(PathBuf, PathBuf)> = loaded
            .into_iter()
            .filter_map(|(old, id)| {
                self.library
                    .path_of(id)
                    .filter(|new| *new != old.as_path())
                    .map(|new| (old, new.to_path_buf()))
            })
            .collect();
        self.apply_path_moves(&moves);
        self.mirror_groups();
        let loaded_now = self.loaded_paths();
        let fresh: Vec<PathBuf> = self
            .library
            .sounds()
            .iter()
            .map(|s| s.path.clone())
            .filter(|p| !known.contains(p) && !loaded_now.contains(p))
            .collect();
        let tags = self.library.tag_counts(&LibraryQuery::new());
        self.lib_ui.filter.retain_known(&tags);
        self.library_dirty = true;
        self.lib_ui.request_recheck(fresh);
    }

    fn apply_scan(&mut self, report: &ScanReport, auto_groups: bool) {
        let stats = report.apply_to_library(&mut self.library, auto_groups);
        self.library_dirty = true;
        self.lib_ui
            .enqueue_loads(report.files.iter().map(|f| f.path.clone()));
        let mut text = format!(
            "Added {} sound{}",
            stats.added,
            if stats.added == 1 { "" } else { "s" }
        );
        if stats.groups_created > 0 {
            text.push_str(&format!(
                ", {} new group{}",
                stats.groups_created,
                if stats.groups_created == 1 { "" } else { "s" }
            ));
        }
        self.lib_ui.notify(text);
    }

    // --------------------------------------------------------- acts ---------

    /// Carry out what a dialog asked for. Returns whether the dialog stays
    /// open.
    pub(crate) fn run_acts(&mut self, acts: Vec<Act>, dialog: &mut LibDialog) -> bool {
        let mut keep = true;
        for act in acts {
            match act {
                Act::Close => keep = false,
                Act::ApplyScan => {
                    if let LibDialog::AddFolder(d) = dialog {
                        let phase = std::mem::replace(&mut d.phase, AddFolderPhase::Picking);
                        if let AddFolderPhase::Ready { report, .. } = phase {
                            self.apply_scan(&report, d.auto_groups);
                            keep = false;
                        } else {
                            d.phase = phase;
                        }
                    }
                }
                Act::RemapBrowse => {
                    if let LibDialog::Remap(d) = dialog {
                        let token = d.token;
                        self.lib_ui
                            .tasks
                            .pick_folder("Choose the new folder", None, move |path| {
                                LibEvent::PrefixPicked { token, path }
                            });
                    }
                }
                Act::RemapApply => {
                    if let LibDialog::Remap(d) = dialog {
                        let moves = remap_moves(&self.library, &d.old, &d.new);
                        let count = self.library.remap_prefix(&d.old, &d.new);
                        if count > 0 {
                            self.apply_path_moves(&moves);
                            self.library_dirty = true;
                            self.lib_ui
                                .request_recheck(moves.iter().map(|(_, new)| new.clone()));
                            self.lib_ui.notify(format!(
                                "Rewrote {count} path{}",
                                if count == 1 { "" } else { "s" }
                            ));
                            keep = false;
                        } else {
                            self.lib_ui
                                .notify_error("No sound path starts with that prefix");
                        }
                    }
                }
                Act::ExportChoose => {
                    if let LibDialog::Export(d) = dialog {
                        let token = d.token;
                        d.phase = ExportPhase::Picking;
                        self.lib_ui.tasks.pick_save_zip(
                            "Export bundle",
                            &bundle_file_name(&d.name),
                            move |path| LibEvent::ExportDestPicked { token, path },
                        );
                    }
                }
                Act::ExportCancel => {
                    if let LibDialog::Export(Export {
                        phase: ExportPhase::Running { cancel, .. },
                        ..
                    }) = dialog
                    {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
                Act::ImportPickAnother => {
                    if let LibDialog::Import(d) = dialog {
                        let token = d.token;
                        d.phase = ImportPhase::Picking;
                        d.preview = ImportPreview::None;
                        self.lib_ui
                            .tasks
                            .pick_zip("Choose a bundle to import", move |path| {
                                LibEvent::ImportZipPicked { token, path }
                            });
                    }
                }
                Act::ImportBrowse => {
                    if let LibDialog::Import(d) = dialog {
                        let token = d.token;
                        self.lib_ui.tasks.pick_folder(
                            "Choose the folder for the sounds",
                            None,
                            move |path| LibEvent::ImportTargetPicked { token, path },
                        );
                    }
                }
                Act::ImportRefresh => {
                    if let LibDialog::Import(d) = dialog {
                        request_import_preview(d, &self.library, &self.lib_ui.tasks);
                    }
                }
                Act::ImportStart => {
                    if let LibDialog::Import(d) = dialog {
                        self.start_import(d);
                    }
                }
                Act::ImportCancel => {
                    if let LibDialog::Import(Import {
                        phase: ImportPhase::Running { cancel, .. },
                        ..
                    }) = dialog
                    {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
                Act::TagAdd(raw) => {
                    if let LibDialog::Tags(d) = dialog {
                        match parse_tag_input(&raw) {
                            Ok(tags) => {
                                for tag in tags {
                                    if self.library.add_tag(&d.path, &tag) {
                                        self.library_dirty = true;
                                    }
                                }
                                d.input.clear();
                                d.error = None;
                                d.focus = true;
                            }
                            Err(error) => d.error = Some(error.to_string()),
                        }
                    }
                }
                Act::TagRemove(tag) => {
                    if let LibDialog::Tags(d) = dialog {
                        if self.library.remove_tag(&d.path, &tag) {
                            self.library_dirty = true;
                            let known = self.library.tag_counts(&LibraryQuery::new());
                            self.lib_ui.filter.retain_known(&known);
                        }
                    }
                }
            }
        }
        keep
    }

    fn start_import(&mut self, d: &mut Import) {
        let (Some(zip), true) = (d.zip.clone(), !d.target.trim().is_empty()) else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        d.phase = ImportPhase::Running {
            cancel: Arc::clone(&cancel),
            progress: None,
        };
        let options = ImportOptions::new(PathBuf::from(d.target.trim()), d.policy);
        self.lib_ui
            .tasks
            .import(d.token, self.library.clone(), zip, options, cancel);
    }

    // -------------------------------------------------------- events --------

    fn handle_lib_event(&mut self, event: LibEvent) {
        match event {
            LibEvent::FolderPicked { token, path } => self.on_folder_picked(token, path),
            LibEvent::RelinkPicked { old, new } => self.on_relink_picked(old, new),
            LibEvent::PrefixPicked { token, path } => {
                if let (LibDialog::Remap(d), Some(path)) = (&mut self.lib_ui.dialog, path) {
                    if d.token == token {
                        d.new = path.to_string_lossy().into_owned();
                    }
                }
            }
            LibEvent::ExportDestPicked { token, path } => self.on_export_dest(token, path),
            LibEvent::ImportZipPicked { token, path } => self.on_import_zip(token, path),
            LibEvent::ImportTargetPicked { token, path } => {
                if let (LibDialog::Import(d), Some(path)) = (&mut self.lib_ui.dialog, path) {
                    if d.token == token {
                        d.target = path.to_string_lossy().into_owned();
                        request_import_preview(d, &self.library, &self.lib_ui.tasks);
                    }
                }
            }
            LibEvent::FilesPicked(files) => {
                for file in files {
                    self.request_load_sound(file);
                }
            }
            LibEvent::Missing(check) => {
                self.lib_ui.check.in_flight = false;
                self.lib_ui.check.last = Some(Instant::now());
                self.lib_ui.missing = check.missing;
                let loaded = self.loaded_paths();
                let load: Vec<PathBuf> = check
                    .reload
                    .into_iter()
                    .filter(|p| !loaded.contains(p))
                    .collect();
                self.lib_ui.enqueue_loads(load);
            }
            LibEvent::BundleInspected { token, result } => self.on_inspected(token, result),
            LibEvent::ImportPreview { token, seq, result } => {
                if let LibDialog::Import(d) = &mut self.lib_ui.dialog {
                    if d.token == token && d.preview_seq == seq {
                        d.preview = match result {
                            Ok(report) => ImportPreview::Ready(Box::new(report)),
                            Err(error) => ImportPreview::Failed {
                                safety: is_safety_error(&error),
                                message: describe_bundle_error(&error),
                            },
                        };
                    }
                }
            }
            LibEvent::Progress { token, progress } => match &mut self.lib_ui.dialog {
                LibDialog::Export(Export {
                    token: t,
                    phase: ExportPhase::Running { progress: p, .. },
                    ..
                }) if *t == token => *p = Some(progress),
                LibDialog::Import(Import {
                    token: t,
                    phase: ImportPhase::Running { progress: p, .. },
                    ..
                }) if *t == token => *p = Some(progress),
                _ => {}
            },
            LibEvent::ExportDone { token, result } => self.on_export_done(token, result),
            LibEvent::ImportDone { token, result } => self.on_import_done(token, result),
        }
    }

    fn on_folder_picked(&mut self, token: u64, path: Option<PathBuf>) {
        let LibDialog::AddFolder(d) = &mut self.lib_ui.dialog else {
            return;
        };
        if d.token != token || !matches!(d.phase, AddFolderPhase::Picking) {
            return;
        }
        let Some(root) = path else {
            self.lib_ui.dialog = LibDialog::None;
            return;
        };
        match start_scan(
            root.clone(),
            known_paths(&self.library),
            ScanOptions::default(),
        ) {
            Ok(handle) => {
                d.phase = AddFolderPhase::Scanning {
                    root,
                    handle,
                    stats: Default::default(),
                };
            }
            Err(error) => {
                d.phase = AddFolderPhase::Failed(format!("Could not start the scan: {error}"));
            }
        }
    }

    fn on_relink_picked(&mut self, old: PathBuf, new: Option<PathBuf>) {
        let Some(new) = new else {
            return;
        };
        if !is_audio_file(&new) {
            self.lib_ui
                .notify_error("That file is not a supported audio file");
            return;
        }
        let name = self
            .library
            .sound(&old)
            .map(|s| super::logic::display_name(&s.name, &s.path))
            .unwrap_or_default();
        if self.library.relink(&old, &new) {
            self.apply_path_moves(&[(old, new.clone())]);
            self.library_dirty = true;
            self.lib_ui.request_recheck([new]);
            self.lib_ui.notify(format!("Relinked \"{name}\""));
        } else {
            self.lib_ui
                .notify_error("That file is already used by another sound in the library");
        }
    }

    fn on_export_dest(&mut self, token: u64, path: Option<PathBuf>) {
        let LibDialog::Export(d) = &mut self.lib_ui.dialog else {
            return;
        };
        if d.token != token || !matches!(d.phase, ExportPhase::Picking) {
            return;
        }
        let Some(path) = path else {
            d.phase = ExportPhase::Idle;
            return;
        };
        let dest = with_zip_extension(&path);
        let cancel = Arc::new(AtomicBool::new(false));
        d.phase = ExportPhase::Running {
            dest: dest.clone(),
            cancel: Arc::clone(&cancel),
            progress: None,
        };
        let options = ExportOptions {
            include_audio: d.include_audio,
            name: d.name.trim().to_string(),
        };
        self.lib_ui
            .tasks
            .export(token, self.library.clone(), dest, options, cancel);
    }

    fn on_import_zip(&mut self, token: u64, path: Option<PathBuf>) {
        let LibDialog::Import(d) = &mut self.lib_ui.dialog else {
            return;
        };
        if d.token != token || !matches!(d.phase, ImportPhase::Picking) {
            return;
        }
        match path {
            None => {
                // Cancelled: back to the options, or close when nothing was
                // chosen yet.
                if d.info.is_some() {
                    d.phase = ImportPhase::Ready;
                } else {
                    self.lib_ui.dialog = LibDialog::None;
                }
            }
            Some(zip) => {
                d.zip = Some(zip.clone());
                d.info = None;
                d.preview = ImportPreview::None;
                d.phase = ImportPhase::Inspecting;
                self.lib_ui.tasks.inspect(token, zip);
            }
        }
    }

    fn on_inspected(
        &mut self,
        token: u64,
        result: Result<crate::library_bundle::BundleInfo, crate::library_bundle::BundleError>,
    ) {
        let LibDialog::Import(d) = &mut self.lib_ui.dialog else {
            return;
        };
        if d.token != token || !matches!(d.phase, ImportPhase::Inspecting) {
            return;
        }
        match result {
            Ok(info) => {
                if let Some(zip) = &d.zip {
                    d.target = default_import_dir(
                        &crate::paths::library_dir().join("imported"),
                        &info.name,
                        zip,
                    )
                    .to_string_lossy()
                    .into_owned();
                }
                d.info = Some(info);
                d.phase = ImportPhase::Ready;
                request_import_preview(d, &self.library, &self.lib_ui.tasks);
            }
            Err(error) => {
                d.phase = ImportPhase::Failed {
                    safety: is_safety_error(&error),
                    message: describe_bundle_error(&error),
                };
            }
        }
    }

    fn on_export_done(
        &mut self,
        token: u64,
        result: Result<crate::library_bundle::ExportReport, crate::library_bundle::BundleError>,
    ) {
        let LibDialog::Export(d) = &mut self.lib_ui.dialog else {
            return;
        };
        if d.token != token {
            return;
        }
        let dest = match &d.phase {
            ExportPhase::Running { dest, .. } => dest.clone(),
            _ => return,
        };
        d.phase = match result {
            Ok(report) => ExportPhase::Done { dest, report },
            Err(crate::library_bundle::BundleError::Cancelled) => ExportPhase::Idle,
            Err(error) => ExportPhase::Failed {
                safety: is_safety_error(&error),
                message: describe_bundle_error(&error),
            },
        };
    }

    fn on_import_done(
        &mut self,
        token: u64,
        result: Result<
            (
                crate::library_bundle::ImportReport,
                Box<super::tasks::ImportedConfig>,
            ),
            crate::library_bundle::BundleError,
        >,
    ) {
        let LibDialog::Import(d) = &mut self.lib_ui.dialog else {
            return;
        };
        if d.token != token || !matches!(d.phase, ImportPhase::Running { .. }) {
            return;
        }
        match result {
            Ok((report, outcome)) => {
                d.phase = ImportPhase::Done(Box::new(report));
                // Merge by id into what the library is now, not what it was
                // when the import started: edits made meanwhile are kept.
                let merged =
                    merge_import(&outcome.base, &outcome.imported, &self.library.to_config());
                self.replace_library(&merged);
                self.lib_ui.notify("Bundle imported");
            }
            Err(crate::library_bundle::BundleError::Cancelled) => {
                d.phase = ImportPhase::Ready;
            }
            Err(error) => {
                d.phase = ImportPhase::Failed {
                    safety: is_safety_error(&error),
                    message: describe_bundle_error(&error),
                };
            }
        }
    }

    // --------------------------------------------------------- scanning -----

    fn poll_scan(&mut self, ctx: &egui::Context) {
        let LibDialog::AddFolder(d) = &mut self.lib_ui.dialog else {
            return;
        };
        let AddFolderPhase::Scanning { handle, stats, .. } = &mut d.phase else {
            return;
        };
        let mut finished: Option<ScanReport> = None;
        while let Ok(event) = handle.events().try_recv() {
            match event {
                ScanEvent::Progress(now) => *stats = now,
                ScanEvent::Done(report) => finished = Some(report),
            }
        }
        match finished {
            Some(report) => {
                let summary = summarize_scan(&report, &self.library);
                d.auto_groups = !summary.folders.is_empty();
                d.phase = AddFolderPhase::Ready { report, summary };
            }
            None => ctx.request_repaint_after(Duration::from_millis(80)),
        }
    }

    // ------------------------------------------------------- file check -----

    /// Start a background look at the files when one is due.
    fn schedule_file_check(&mut self) {
        let check = &mut self.lib_ui.check;
        if check.in_flight {
            return;
        }
        let due = check.queued || check.last.is_none_or(|at| at.elapsed() >= RECHECK_EVERY);
        if !due {
            return;
        }
        let paths: Vec<PathBuf> = self
            .library
            .sounds()
            .iter()
            .map(|s| s.path.clone())
            .collect();
        let reload = std::mem::take(&mut self.lib_ui.check.reload);
        self.lib_ui.check.queued = false;
        if self.lib_ui.tasks.check_files(paths, reload) {
            self.lib_ui.check.in_flight = true;
        } else {
            self.lib_ui.check.last = Some(Instant::now());
        }
    }

    /// Hand queued files to the engine loader a few at a time.
    fn feed_loads(&mut self, ctx: &egui::Context) {
        if self.lib_ui.load_queue.is_empty() && self.lib_ui.load_in_flight.is_empty() {
            return;
        }
        let loaded = self.loaded_paths();
        self.lib_ui
            .load_in_flight
            .retain(|path, since| !loaded.contains(path) && since.elapsed() < LOAD_TIMEOUT);
        while self.lib_ui.load_in_flight.len() < LOAD_WINDOW {
            let Some(path) = self.lib_ui.load_queue.pop_front() else {
                break;
            };
            if loaded.contains(&path) {
                continue;
            }
            self.lib_ui
                .load_in_flight
                .insert(path.clone(), Instant::now());
            self.request_load_sound(path);
        }
        if !self.lib_ui.load_queue.is_empty() || !self.lib_ui.load_in_flight.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    // ---------------------------------------------------------- preview -----

    /// Start or stop auditioning a sound on this machine only.
    ///
    /// A second click on the playing sound stops it; clicking another sound
    /// switches to it. The audio goes to a local-only track of the mixer and
    /// never reaches the players' stream.
    pub fn toggle_preview(&mut self, sound: SoundId, path: &Path) {
        let same = self.lib_ui.is_previewing(sound);
        self.stop_preview();
        if same {
            return;
        }
        if !self.local_output().state.is_running() {
            self.lib_ui.notify_error(format!(
                "Preview plays on this computer only, and its audio output is {}",
                self.local_output().state
            ));
            return;
        }
        let started = {
            let guard = self.audio_engine.lock().unwrap();
            match guard.as_ref() {
                Some(engine) => engine
                    .play_preview(sound, false)
                    .map(|handle| handle.track_id)
                    .map_err(|error| error.to_string()),
                None => Err("the audio engine is not running".to_string()),
            }
        };
        match started {
            Ok(track) => {
                self.lib_ui.preview = Some(PreviewTrack {
                    sound,
                    path: path.to_path_buf(),
                    track,
                });
            }
            Err(error) => self
                .lib_ui
                .notify_error(format!("Could not preview this sound: {error}")),
        }
    }

    fn stop_preview(&mut self) {
        let Some(preview) = self.lib_ui.preview.take() else {
            return;
        };
        if let Some(engine) = self.audio_engine.lock().unwrap().as_ref() {
            // A track that already ended is gone from the mixer; that is fine.
            let _ = engine.stop_track(preview.track);
        }
    }

    /// Notice a preview that ended by itself or whose sound was removed.
    fn poll_preview(&mut self, ctx: &egui::Context) {
        let Some(preview) = &self.lib_ui.preview else {
            return;
        };
        let (sound, track) = (preview.sound, preview.track);
        let still_loaded = self.sounds.lock().unwrap().contains_key(&sound);
        if !still_loaded {
            self.stop_preview();
            return;
        }
        let playing = self
            .audio_engine
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|engine| engine.is_track_playing(track).unwrap_or(false));
        if playing {
            ctx.request_repaint_after(Duration::from_millis(150));
        } else {
            self.lib_ui.preview = None;
        }
    }
}

/// Ask for a dry run of the import with the dialog's current options.
fn request_import_preview(d: &mut Import, library: &Library, tasks: &super::tasks::Tasks) {
    let Some(zip) = d.zip.clone() else {
        return;
    };
    if d.target.trim().is_empty() {
        d.preview = ImportPreview::None;
        return;
    }
    d.preview_seq += 1;
    d.preview = ImportPreview::Computing;
    d.preview_key = Some((d.policy, d.target.clone()));
    tasks.preview_import(
        d.token,
        d.preview_seq,
        library.clone(),
        zip,
        ImportOptions::new(PathBuf::from(d.target.trim()), d.policy),
    );
}

/// The preview line of the remap dialog for the typed values, recomputed only
/// when they change.
pub(crate) fn refresh_remap_preview(d: &mut Remap, library: &Library, missing: &HashSet<PathBuf>) {
    let stale = match &d.preview {
        Some((old, new, _)) => *old != d.old || *new != d.new,
        None => true,
    };
    if stale {
        let preview = preview_remap(library, missing, &d.old, &d.new, 3);
        d.preview = Some((d.old.clone(), d.new.clone(), preview));
    }
}

// ----------------------------------------------------------- import merge --

/// Merge the effect of a background import into the library as it is *now*.
///
/// The import worker runs on a copy of the library (`base`) and produces
/// `imported`; the operator may keep editing the live library (`live`) while it
/// runs. Replacing the live library with `imported` would silently undo those
/// edits, so the import is applied as a three-way merge by id, per kind of
/// entity (groups, sounds, playlists, scenes):
///
/// - an entity the import left alone (`imported == base`) keeps its live state,
///   including a deletion made in the meantime;
/// - an entity the import added is appended;
/// - an entity the import changed (a `Replace` conflict) takes the imported
///   version, unless the operator changed or deleted that same entity during the
///   import, in which case the live edit wins;
/// - everything else in `live` is kept as it is, in its order.
///
/// Limitation: ids are compared as written, so the merge relies on the id
/// strings the library itself writes (`Library::to_config`).
pub(super) fn merge_import(
    base: &LibraryConfig,
    imported: &LibraryConfig,
    live: &LibraryConfig,
) -> LibraryConfig {
    LibraryConfig {
        groups: merge_list(&base.groups, &imported.groups, &live.groups, |g| &g.id),
        sounds: merge_list(&base.sounds, &imported.sounds, &live.sounds, |s| &s.id),
        scenes: merge_list(&base.scenes, &imported.scenes, &live.scenes, |s| &s.id),
        playlists: merge_list(&base.playlists, &imported.playlists, &live.playlists, |p| {
            &p.id
        }),
    }
}

fn merge_list<T: Clone + PartialEq>(
    base: &[T],
    imported: &[T],
    live: &[T],
    id: impl Fn(&T) -> &str,
) -> Vec<T> {
    let mut out: Vec<T> = live.to_vec();
    for item in imported {
        let key = id(item);
        let before = base.iter().find(|b| id(b) == key);
        if before == Some(item) {
            continue; // untouched by the import
        }
        match out.iter().position(|o| id(o) == key) {
            Some(at) => {
                // Changed by the import; keep a live edit made meanwhile.
                if before == Some(&out[at]) {
                    out[at] = item.clone();
                }
            }
            None if before.is_none() => out.push(item.clone()),
            None => {} // deleted by the operator during the import
        }
    }
    out
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use crate::config::{GroupConfig, SoundConfigEntry};

    fn sound(id: &str, name: &str) -> SoundConfigEntry {
        SoundConfigEntry {
            id: id.into(),
            path: format!("/s/{id}.wav"),
            name: name.into(),
            volume: 1.0,
            looped: false,
            group: None,
            tags: Vec::new(),
            gain_db: 0.0,
            bus: Default::default(),
            oneshot_stinger: false,
            pan: 0.0,
        }
    }

    fn cfg(sounds: Vec<SoundConfigEntry>) -> LibraryConfig {
        LibraryConfig {
            sounds,
            ..Default::default()
        }
    }

    #[test]
    fn edits_made_during_the_import_survive() {
        let base = cfg(vec![sound("a", "A"), sound("b", "B")]);
        let mut imported = base.clone();
        imported.sounds.push(sound("c", "C"));
        // The operator renamed A, deleted B and added D while the import ran.
        let live = cfg(vec![sound("a", "A renamed"), sound("d", "D")]);
        let merged = merge_import(&base, &imported, &live);
        let names: Vec<&str> = merged.sounds.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["A renamed", "D", "C"]);
    }

    #[test]
    fn a_replaced_entity_takes_the_import_unless_edited_meanwhile() {
        let base = cfg(vec![sound("a", "A"), sound("b", "B")]);
        let imported = cfg(vec![
            sound("a", "A from bundle"),
            sound("b", "B from bundle"),
        ]);
        let live = cfg(vec![sound("a", "A"), sound("b", "B edited")]);
        let merged = merge_import(&base, &imported, &live);
        assert_eq!(merged.sounds[0].name, "A from bundle");
        assert_eq!(merged.sounds[1].name, "B edited");
    }

    #[test]
    fn an_idle_import_equals_the_imported_library() {
        let base = cfg(vec![sound("a", "A")]);
        let mut imported = base.clone();
        imported.sounds.push(sound("b", "B"));
        imported.groups.push(GroupConfig {
            id: "g".into(),
            name: "G".into(),
        });
        assert_eq!(merge_import(&base, &imported, &base), imported);
    }
}
