//! Pure presentation logic of the library UI.
//!
//! Everything here is free of egui widgets and of I/O (the one exception,
//! [`highlight_job`], only builds an egui text description). The panels and
//! dialogs call these functions and draw the result, so the rules can be tested
//! without a window.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};

use crate::gui::app::SoundEntry;
use crate::gui::groups::{
    normalize_tag, GroupFilter, GroupId, Library, MAX_GROUP_NAME_LEN, MAX_TAG_LEN,
};
use crate::library_bundle::{BundleError, ConflictPolicy, EntityStats, ImportReport};
use crate::library_query::{highlight_ranges, LibraryQuery};
use crate::library_scan::{ScanReport, ScanStats};

// ------------------------------------------------------------------ filter --

/// What the operator typed into the search box and which tag chips are lit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewFilter {
    /// Free text from the search box.
    pub text: String,
    /// Tags that are switched on (a sound must carry all of them).
    pub tags: Vec<String>,
}

impl ViewFilter {
    /// The library query for this filter inside `group`.
    pub fn query(&self, group: GroupFilter) -> LibraryQuery {
        LibraryQuery {
            group,
            tags_all: self.tags.clone(),
            text: self.text.clone(),
        }
    }

    /// True when the text or at least one tag narrows the list.
    pub fn is_active(&self) -> bool {
        !self.text.trim().is_empty() || !self.tags.is_empty()
    }

    /// Light a tag chip, or switch it off when it is already lit. The tag is
    /// compared after normalization; an unusable tag changes nothing.
    pub fn toggle_tag(&mut self, tag: &str) {
        let Some(tag) = normalize_tag(tag) else {
            return;
        };
        if let Some(at) = self.tags.iter().position(|t| *t == tag) {
            self.tags.remove(at);
        } else {
            self.tags.push(tag);
        }
    }

    /// Reset the search box and the chips.
    pub fn clear(&mut self) {
        self.text.clear();
        self.tags.clear();
    }

    /// Drop lit tags that no sound carries any more (after a tag was removed
    /// or the library was replaced), so the list cannot get stuck on a filter
    /// with no chip to switch it off.
    pub fn retain_known(&mut self, known: &[(String, usize)]) {
        self.tags.retain(|t| known.iter().any(|(k, _)| k == t));
    }
}

// -------------------------------------------------------------------- rows --

/// A library sound whose file is not on disk and which is not loaded in the
/// engine, so it has no [`SoundEntry`]; it still deserves a row.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingSound {
    /// Registered path.
    pub path: PathBuf,
    /// Display name (the file name when the library name is empty).
    pub name: String,
    /// Group membership.
    pub group: Option<GroupId>,
}

/// One row of the sound list.
#[derive(Debug, Clone)]
pub enum RowItem {
    /// A sound loaded in the engine (it may still be flagged missing).
    Loaded(SoundEntry),
    /// A registered sound that could not be loaded because its file is gone.
    Missing(MissingSound),
}

impl RowItem {
    /// Path of the sound.
    pub fn path(&self) -> &Path {
        match self {
            RowItem::Loaded(entry) => &entry.path,
            RowItem::Missing(sound) => &sound.path,
        }
    }
}

/// Rows for the list: the loaded sounds that pass `query` in load order, then
/// the registered sounds whose file is missing and that are not loaded, in
/// library order.
pub fn build_rows(
    loaded: Vec<SoundEntry>,
    library: &Library,
    missing: &HashSet<PathBuf>,
    query: &LibraryQuery,
) -> Vec<RowItem> {
    let loaded_paths: HashSet<PathBuf> = loaded.iter().map(|e| e.path.clone()).collect();
    let mut rows: Vec<RowItem> = loaded
        .into_iter()
        .filter(|entry| library.matches_query(&entry.path, query))
        .map(RowItem::Loaded)
        .collect();
    if missing.is_empty() {
        return rows;
    }
    for sound in library.sounds() {
        if missing.contains(&sound.path)
            && !loaded_paths.contains(&sound.path)
            && query.matches(sound)
        {
            rows.push(RowItem::Missing(MissingSound {
                path: sound.path.clone(),
                name: display_name(&sound.name, &sound.path),
                group: sound.group,
            }));
        }
    }
    rows
}

/// The library name, or the file name when the name is blank.
pub fn display_name(name: &str, path: &Path) -> String {
    if name.trim().is_empty() {
        path.file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        name.to_string()
    }
}

/// Shorten a path from the left (`…/Tavern/crowd.ogg`): the end of a path is
/// the part that tells files apart.
pub fn elide_start(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let tail: String = text.chars().skip(count - keep).collect();
    format!("\u{2026}{tail}")
}

/// How many registered sounds are flagged missing right now.
pub fn missing_count(library: &Library, missing: &HashSet<PathBuf>) -> usize {
    if missing.is_empty() {
        return 0;
    }
    library
        .sounds()
        .iter()
        .filter(|s| missing.contains(&s.path))
        .count()
}

/// `1 file not found` / `3 files not found`.
pub fn missing_summary(count: usize) -> String {
    match count {
        1 => "1 file not found".to_string(),
        n => format!("{n} files not found"),
    }
}

// -------------------------------------------------------------------- tags --

/// One tag chip of the filter bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagChip {
    /// The tag (normalized).
    pub tag: String,
    /// Sounds that carry it among the current results.
    pub count: usize,
    /// Is the chip lit?
    pub selected: bool,
}

/// Chips for the filter bar: the lit tags first (in the order they were lit),
/// then the other tags of `counts` (already sorted most used first) until
/// `limit` chips are shown. The second value is how many tags did not fit.
pub fn tag_chips(
    selected: &[String],
    counts: &[(String, usize)],
    limit: usize,
) -> (Vec<TagChip>, usize) {
    let mut chips: Vec<TagChip> = selected
        .iter()
        .map(|tag| TagChip {
            tag: tag.clone(),
            count: counts.iter().find(|(t, _)| t == tag).map_or(0, |(_, n)| *n),
            selected: true,
        })
        .collect();
    let mut hidden = 0;
    for (tag, count) in counts {
        if selected.contains(tag) {
            continue;
        }
        if chips.len() < limit.max(selected.len()) {
            chips.push(TagChip {
                tag: tag.clone(),
                count: *count,
                selected: false,
            });
        } else {
            hidden += 1;
        }
    }
    (chips, hidden)
}

/// Why a typed tag list was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagInputError {
    /// A tag is longer than [`MAX_TAG_LEN`] characters.
    TooLong(String),
}

impl std::fmt::Display for TagInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TagInputError::TooLong(tag) => write!(
                f,
                "\"{}\" is too long (at most {MAX_TAG_LEN} characters)",
                crate::gui::widgets::elide(tag, 20)
            ),
        }
    }
}

/// Split what the operator typed into tags (separated by commas), normalized
/// and de-duplicated; blanks are ignored. One over-long tag refuses the whole
/// input, so nothing is half-added.
pub fn parse_tag_input(input: &str) -> Result<Vec<String>, TagInputError> {
    let mut out: Vec<String> = Vec::new();
    for part in input.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        match normalize_tag(trimmed) {
            Some(tag) => {
                if !out.contains(&tag) {
                    out.push(tag);
                }
            }
            None => return Err(TagInputError::TooLong(trimmed.to_string())),
        }
    }
    Ok(out)
}

/// Autocomplete for the tag field: tags already used in the library (`known`,
/// with their use counts) that the sound does not carry yet and that match the
/// fragment being typed (the text after the last comma). Prefix matches come
/// before substring matches, then most used first, then alphabetical. An empty
/// fragment suggests the most used tags.
pub fn suggest_tags(
    input: &str,
    known: &[(String, usize)],
    have: &[String],
    limit: usize,
) -> Vec<String> {
    let fragment = input.rsplit(',').next().unwrap_or("").trim().to_lowercase();
    let typed_already: Vec<String> = input
        .split(',')
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    let mut scored: Vec<(u8, usize, &str)> = known
        .iter()
        .filter(|(tag, _)| !have.contains(tag) && !typed_already.iter().any(|t| t == tag))
        .filter_map(|(tag, count)| {
            if fragment.is_empty() || tag.starts_with(&fragment) {
                Some((0, *count, tag.as_str()))
            } else if tag.contains(&fragment) {
                Some((1, *count, tag.as_str()))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(b.2))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, tag)| tag.to_string())
        .collect()
}

/// Replace the fragment being typed (after the last comma) with `tag` and a
/// trailing separator, for a click on a suggestion.
pub fn complete_tag_input(input: &str, tag: &str) -> String {
    match input.rfind(',') {
        Some(at) => format!("{} {tag}", &input[..=at]),
        None => tag.to_string(),
    }
}

/// `#tavern #loud +2`: the tags of a row on one line.
pub fn format_tags(tags: &[String], shown: usize) -> String {
    let mut out: Vec<String> = tags.iter().take(shown).map(|t| format!("#{t}")).collect();
    if tags.len() > shown {
        out.push(format!("+{}", tags.len() - shown));
    }
    out.join(" ")
}

// --------------------------------------------------------------- highlight --

/// Text with the search matches highlighted, ready for `Painter::layout_job`.
pub fn highlight_job(
    text: &str,
    terms: &[String],
    font: FontId,
    color: Color32,
    match_color: Color32,
    match_background: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat {
        font_id: font.clone(),
        color,
        ..TextFormat::default()
    };
    let marked = TextFormat {
        font_id: font,
        color: match_color,
        background: match_background,
        ..TextFormat::default()
    };
    let mut at = 0;
    for range in highlight_ranges(text, terms) {
        if range.start > at {
            job.append(&text[at..range.start], 0.0, plain.clone());
        }
        job.append(&text[range.clone()], 0.0, marked.clone());
        at = range.end;
    }
    if at < text.len() || text.is_empty() {
        job.append(&text[at..], 0.0, plain);
    }
    job
}

// ------------------------------------------------------------ missing files --

/// The answer of a "which files are gone" check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MissingCheck {
    /// Paths with no file behind them.
    pub missing: HashSet<PathBuf>,
    /// Paths asked about in `reload` whose file exists (they should be loaded
    /// into the engine now).
    pub reload: Vec<PathBuf>,
}

/// Check `paths` with `exists` (a closure so tests need no files). `reload`
/// lists the paths whose presence should also be reported back.
pub fn check_missing(
    paths: &[PathBuf],
    reload: &HashSet<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> MissingCheck {
    let mut result = MissingCheck::default();
    for path in paths {
        if exists(path) {
            if reload.contains(path) {
                result.reload.push(path.clone());
            }
        } else {
            result.missing.insert(path.clone());
        }
    }
    result
}

/// The closest folder above `path` that exists, to start a file dialog there.
pub fn nearest_existing_ancestor(path: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut current = path.parent();
    while let Some(dir) = current {
        if dir.as_os_str().is_empty() {
            return None;
        }
        if exists(dir) {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// The deepest folder that contains every path (compared component by
/// component), or `None` when they only share a root or a drive. Used to
/// pre-fill "Replace path prefix" with the folder that went missing.
pub fn common_parent(paths: &[&Path]) -> Option<PathBuf> {
    let mut iter = paths.iter();
    let first = iter.next()?.parent()?;
    let mut common: Vec<Component> = first.components().collect();
    for path in iter {
        let parent = path.parent()?;
        let mut keep = 0;
        for (a, b) in common.iter().zip(parent.components()) {
            if *a != b {
                break;
            }
            keep += 1;
        }
        common.truncate(keep);
        if common.is_empty() {
            return None;
        }
    }
    if !common.iter().any(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(common.iter().collect())
}

// ------------------------------------------------------------------- scan --

/// What a finished folder scan found, in numbers the dialog shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanSummary {
    /// New sounds that would be added.
    pub new_sounds: usize,
    /// First-level sub-folders with how many new sounds each holds, in the
    /// order they were first met.
    pub folders: Vec<(String, usize)>,
    /// New sounds directly in the scanned folder.
    pub top_level: usize,
    /// Group names that "create groups by sub-folder" would create.
    pub new_groups: Vec<String>,
    /// Sub-folders whose group already exists.
    pub reused_groups: usize,
    /// Already in the library (same path).
    pub skipped_existing: usize,
    /// Same content as another file.
    pub skipped_duplicates: usize,
    /// Empty files.
    pub skipped_empty: usize,
    /// Links that lead outside the folder.
    pub skipped_links: usize,
    /// Entries that could not be read.
    pub errors: usize,
}

/// The group name a sub-folder gets (mirrors what applying a scan does: trimmed
/// and cut to [`MAX_GROUP_NAME_LEN`] characters).
pub fn group_name_for_folder(folder: &str) -> Option<String> {
    let name: String = folder.trim().chars().take(MAX_GROUP_NAME_LEN).collect();
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Summarize `report` against `library` (for the preview and the checkbox).
pub fn summarize_scan(report: &ScanReport, library: &Library) -> ScanSummary {
    let mut summary = ScanSummary {
        new_sounds: report.files.len(),
        skipped_existing: report.stats.skipped_existing,
        skipped_duplicates: report.stats.skipped_duplicates,
        skipped_empty: report.stats.skipped_empty,
        skipped_links: report.stats.skipped_links,
        errors: report.stats.errors,
        ..ScanSummary::default()
    };
    let mut seen_groups: HashSet<String> = HashSet::new();
    for file in &report.files {
        match &file.folder {
            None => summary.top_level += 1,
            Some(folder) => {
                match summary.folders.iter_mut().find(|(name, _)| name == folder) {
                    Some((_, count)) => *count += 1,
                    None => summary.folders.push((folder.clone(), 1)),
                }
                if let Some(name) = group_name_for_folder(folder) {
                    let lowered = name.to_lowercase();
                    if seen_groups.insert(lowered.clone()) {
                        let exists = library
                            .groups()
                            .iter()
                            .any(|g| g.name.to_lowercase() == lowered);
                        if exists {
                            summary.reused_groups += 1;
                        } else {
                            summary.new_groups.push(name);
                        }
                    }
                }
            }
        }
    }
    summary
}

/// Live line while a scan runs.
pub fn scan_progress_line(stats: &ScanStats) -> String {
    format!(
        "{} folders - {} audio files seen - {} new",
        stats.dirs_visited, stats.audio_seen, stats.found
    )
}

/// The lines of "skipped" statistics worth showing (zero counts are left out).
pub fn skipped_lines(summary: &ScanSummary) -> Vec<String> {
    let mut lines = Vec::new();
    if summary.skipped_existing > 0 {
        lines.push(format!(
            "{} already in the library (same path)",
            summary.skipped_existing
        ));
    }
    if summary.skipped_duplicates > 0 {
        lines.push(format!(
            "{} duplicates skipped (same content as another file)",
            summary.skipped_duplicates
        ));
    }
    if summary.skipped_empty > 0 {
        lines.push(format!("{} empty files skipped", summary.skipped_empty));
    }
    if summary.skipped_links > 0 {
        lines.push(format!(
            "{} links skipped (they lead outside the folder)",
            summary.skipped_links
        ));
    }
    if summary.errors > 0 {
        lines.push(format!("{} entries could not be read", summary.errors));
    }
    lines
}

// ------------------------------------------------------------ path prefix --

/// What the "Replace path prefix" dialog shows for the typed prefixes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemapPreview {
    /// Paths that would change.
    pub count: usize,
    /// How many of them are flagged missing now.
    pub missing_hits: usize,
    /// The first few changes as `(old, new)`.
    pub samples: Vec<(String, String)>,
}

/// Preview `Library::remap_prefix` without changing anything.
pub fn preview_remap(
    library: &Library,
    missing: &HashSet<PathBuf>,
    old_prefix: &str,
    new_prefix: &str,
    max_samples: usize,
) -> RemapPreview {
    let plan = library.remap_prefix_plan(old_prefix, new_prefix);
    let mut preview = RemapPreview {
        count: plan.len(),
        ..RemapPreview::default()
    };
    for (id, new_path) in &plan {
        let old = library.path_of(*id);
        if old.is_some_and(|p| missing.contains(p)) {
            preview.missing_hits += 1;
        }
        if preview.samples.len() < max_samples {
            if let Some(old) = old {
                preview.samples.push((
                    old.to_string_lossy().into_owned(),
                    new_path.to_string_lossy().into_owned(),
                ));
            }
        }
    }
    preview
}

/// `(old path, new path)` for every sound `remap_prefix(old, new)` would move.
pub fn remap_moves(
    library: &Library,
    old_prefix: &str,
    new_prefix: &str,
) -> Vec<(PathBuf, PathBuf)> {
    library
        .remap_prefix_plan(old_prefix, new_prefix)
        .into_iter()
        .filter_map(|(id, new)| library.path_of(id).map(|old| (old.to_path_buf(), new)))
        .collect()
}

// ----------------------------------------------------------------- bundle --

/// Replace characters that are illegal in file names and trim the result.
pub fn sanitize_file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "bundle".to_string()
    } else {
        trimmed.chars().take(80).collect()
    }
}

/// `name` as a `.zip` file name (`My set` becomes `My set.zip`).
pub fn bundle_file_name(name: &str) -> String {
    format!("{}.zip", sanitize_file_stem(name))
}

/// Make sure a chosen export path ends with `.zip`.
pub fn with_zip_extension(path: &Path) -> PathBuf {
    let is_zip = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    if is_zip {
        path.to_path_buf()
    } else {
        let mut text = path.as_os_str().to_os_string();
        text.push(".zip");
        PathBuf::from(text)
    }
}

/// Where an imported bundle's sounds go by default: a folder named after the
/// bundle (or the zip) below `base`.
pub fn default_import_dir(base: &Path, bundle_name: &str, zip: &Path) -> PathBuf {
    let stem = if bundle_name.trim().is_empty() {
        zip.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        bundle_name.to_string()
    };
    base.join(sanitize_file_stem(&stem))
}

/// `1.5 MB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Short name of a conflict policy for the radio buttons.
pub fn policy_label(policy: ConflictPolicy) -> &'static str {
    match policy {
        ConflictPolicy::Skip => "Keep mine",
        ConflictPolicy::Rename => "Keep both",
        ConflictPolicy::Replace => "Use the bundle's",
    }
}

/// One sentence on what a conflict policy does.
pub fn policy_help(policy: ConflictPolicy) -> &'static str {
    match policy {
        ConflictPolicy::Skip => {
            "Sounds, groups, scenes and playlists you already have are left alone; \
             the bundle's copies are not imported."
        }
        ConflictPolicy::Rename => {
            "Both are kept: the bundle's copy gets a free name, such as \"Tavern (2)\"."
        }
        ConflictPolicy::Replace => {
            "The bundle's content replaces yours (ids are kept) and existing audio files \
             in the target folder are overwritten."
        }
    }
}

fn entity_line(label: &str, stats: &EntityStats) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if stats.added > 0 {
        parts.push(format!("{} new", stats.added));
    }
    if stats.renamed > 0 {
        parts.push(format!("{} renamed", stats.renamed));
    }
    if stats.replaced > 0 {
        parts.push(format!("{} replaced", stats.replaced));
    }
    if stats.skipped > 0 {
        parts.push(format!("{} skipped", stats.skipped));
    }
    (!parts.is_empty()).then(|| format!("{label}: {}", parts.join(", ")))
}

/// The lines of an import report (or dry run), zero rows left out.
pub fn import_report_lines(report: &ImportReport) -> Vec<String> {
    let mut lines: Vec<String> = [
        entity_line("Sounds", &report.sounds),
        entity_line("Groups", &report.groups),
        entity_line("Scenes", &report.scenes),
        entity_line("Playlists", &report.playlists),
    ]
    .into_iter()
    .flatten()
    .collect();
    if report.files_written > 0 {
        let verb = if report.dry_run {
            "Audio files to write"
        } else {
            "Audio files written"
        };
        lines.push(format!(
            "{verb}: {} ({})",
            report.files_written,
            format_bytes(report.bytes_written)
        ));
    }
    if lines.is_empty() {
        lines.push("Nothing to import".to_string());
    }
    lines
}

/// A bundle error as the operator should read it. Safety rejections say what
/// was refused and that nothing was changed.
pub fn describe_bundle_error(error: &BundleError) -> String {
    match error {
        BundleError::UnsafePath(path) => format!(
            "Import blocked for safety: the bundle contains a path that could write \
             outside the chosen folder (\"{path}\"). Nothing was imported. \
             Only open bundles from people you trust."
        ),
        BundleError::LimitExceeded(what) => format!(
            "Import blocked for safety: the bundle is larger than the allowed limits \
             ({what}). This protects the computer from files that unpack into far more \
             data than they claim. Nothing was imported."
        ),
        BundleError::MissingManifest => {
            "This zip is not a Bardwave bundle: it has no bundle.toml.".to_string()
        }
        BundleError::Manifest(detail) => {
            format!("The bundle's bundle.toml is damaged or inconsistent ({detail}). Nothing was imported.")
        }
        BundleError::UnsupportedVersion(version) => format!(
            "This bundle was made by a newer Bardwave (bundle format {version}). \
             Update Bardwave and try again."
        ),
        BundleError::Zip(detail) => {
            format!("This file is not a readable zip archive ({detail}).")
        }
        BundleError::Io(detail) => format!("A file could not be read or written: {detail}."),
        BundleError::Cancelled => "Cancelled. Nothing was changed.".to_string(),
    }
}

/// Is this error one of the safety checks (to colour the message as a warning)?
pub fn is_safety_error(error: &BundleError) -> bool {
    matches!(
        error,
        BundleError::UnsafePath(_) | BundleError::LimitExceeded(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::SoundId;
    use crate::library_scan::ScannedFile;

    fn entry(id: u32, name: &str, path: &str) -> SoundEntry {
        SoundEntry {
            id: SoundId(uuid::Uuid::from_u128(u128::from(id))),
            name: name.to_string(),
            path: PathBuf::from(path),
            playing: false,
            volume: 1.0,
            looped: false,
            group: None,
            duration: 1.0,
            sample_rate: 48_000,
        }
    }

    fn library_with(paths: &[&str]) -> Library {
        let mut lib = Library::new();
        for path in paths {
            let name = Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            assert!(lib.insert_sound(Path::new(path), &name, 1.0, None));
        }
        lib
    }

    #[test]
    fn filter_builds_query_and_toggles_tags() {
        let mut filter = ViewFilter::default();
        assert!(!filter.is_active());
        filter.text = "  ".into();
        assert!(!filter.is_active());
        filter.toggle_tag(" Tavern ");
        filter.toggle_tag("loud");
        assert_eq!(filter.tags, ["tavern", "loud"]);
        let query = filter.query(GroupFilter::Ungrouped);
        assert_eq!(query.group, GroupFilter::Ungrouped);
        assert_eq!(query.tags_all, ["tavern", "loud"]);
        filter.toggle_tag("TAVERN");
        assert_eq!(filter.tags, ["loud"]);
        filter.toggle_tag("   ");
        assert_eq!(filter.tags, ["loud"]);
        filter.text = "rain".into();
        assert!(filter.is_active());
        filter.clear();
        assert!(!filter.is_active());
    }

    #[test]
    fn retain_known_drops_dead_tags() {
        let mut filter = ViewFilter {
            text: String::new(),
            tags: vec!["a".into(), "b".into()],
        };
        filter.retain_known(&[("b".into(), 2)]);
        assert_eq!(filter.tags, ["b"]);
    }

    #[test]
    fn rows_apply_query_and_append_missing() {
        let mut lib = library_with(&["/s/rain.ogg", "/s/fire.ogg", "/s/gone.ogg", "/s/wind.ogg"]);
        assert!(lib.add_tag(Path::new("/s/gone.ogg"), "storm"));
        assert!(lib.add_tag(Path::new("/s/rain.ogg"), "storm"));
        let loaded = vec![
            entry(1, "rain", "/s/rain.ogg"),
            entry(2, "fire", "/s/fire.ogg"),
        ];
        let missing: HashSet<PathBuf> =
            [PathBuf::from("/s/gone.ogg"), PathBuf::from("/s/rain.ogg")]
                .into_iter()
                .collect();

        // No filter: loaded rows, then the missing one that is not loaded.
        let rows = build_rows(loaded.clone(), &lib, &missing, &LibraryQuery::new());
        let paths: Vec<&Path> = rows.iter().map(RowItem::path).collect();
        assert_eq!(
            paths,
            [
                Path::new("/s/rain.ogg"),
                Path::new("/s/fire.ogg"),
                Path::new("/s/gone.ogg")
            ]
        );
        assert!(matches!(rows[0], RowItem::Loaded(_)));
        assert!(matches!(rows[2], RowItem::Missing(_)));

        // Tag filter applies to both kinds.
        let rows = build_rows(
            loaded.clone(),
            &lib,
            &missing,
            &LibraryQuery::new().with_tag("storm"),
        );
        let paths: Vec<&Path> = rows.iter().map(RowItem::path).collect();
        assert_eq!(paths, [Path::new("/s/rain.ogg"), Path::new("/s/gone.ogg")]);

        // Text filter.
        let rows = build_rows(
            loaded,
            &lib,
            &missing,
            &LibraryQuery::new().with_text("GON"),
        );
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], RowItem::Missing(m) if m.name == "gone"));
    }

    #[test]
    fn rows_without_missing_set_equal_loaded_filter() {
        let lib = library_with(&["/s/a.wav"]);
        let rows = build_rows(
            vec![
                entry(1, "a", "/s/a.wav"),
                entry(2, "stray", "/s/not-in-library.wav"),
            ],
            &lib,
            &HashSet::new(),
            &LibraryQuery::new(),
        );
        // An unregistered loaded sound is not shown (same as the group filter).
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn display_name_falls_back_to_file_name() {
        assert_eq!(display_name("Rain", Path::new("/a/b.ogg")), "Rain");
        assert_eq!(display_name("  ", Path::new("/a/b.ogg")), "b.ogg");
    }

    #[test]
    fn paths_are_cut_from_the_left() {
        assert_eq!(elide_start("/a/b.wav", 20), "/a/b.wav");
        assert_eq!(
            elide_start("/music/tavern/crowd.ogg", 12),
            "\u{2026}n/crowd.ogg"
        );
        assert_eq!(elide_start("abc", 0), "\u{2026}");
        assert_eq!(elide_start("Привет мир", 5), "\u{2026} мир");
    }

    #[test]
    fn missing_counts_and_text() {
        let lib = library_with(&["/s/a.wav", "/s/b.wav"]);
        let missing: HashSet<PathBuf> = [PathBuf::from("/s/a.wav"), PathBuf::from("/s/zzz.wav")]
            .into_iter()
            .collect();
        assert_eq!(missing_count(&lib, &missing), 1);
        assert_eq!(missing_count(&lib, &HashSet::new()), 0);
        assert_eq!(missing_summary(1), "1 file not found");
        assert_eq!(missing_summary(0), "0 files not found");
        assert_eq!(missing_summary(7), "7 files not found");
    }

    #[test]
    fn chips_put_selected_first_and_count_hidden() {
        let counts: Vec<(String, usize)> = vec![
            ("loud".into(), 5),
            ("tavern".into(), 4),
            ("rain".into(), 3),
            ("night".into(), 1),
        ];
        let (chips, hidden) = tag_chips(&["rain".to_string()], &counts, 3);
        let tags: Vec<&str> = chips.iter().map(|c| c.tag.as_str()).collect();
        assert_eq!(tags, ["rain", "loud", "tavern"]);
        assert!(chips[0].selected && chips[0].count == 3);
        assert!(!chips[1].selected);
        assert_eq!(hidden, 1);

        // A lit tag that no result carries still gets a chip (count 0).
        let (chips, _) = tag_chips(&["ghost".to_string()], &counts, 2);
        assert_eq!(chips[0].tag, "ghost");
        assert_eq!(chips[0].count, 0);

        // More lit tags than the limit: all lit chips stay, nothing else.
        let sel: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let (chips, hidden) = tag_chips(&sel, &counts, 2);
        assert_eq!(chips.len(), 3);
        assert_eq!(hidden, 4);
    }

    #[test]
    fn tag_input_is_split_normalized_and_deduplicated() {
        assert_eq!(
            parse_tag_input(" Tavern, LOUD ,, tavern ,").unwrap(),
            ["tavern", "loud"]
        );
        assert!(parse_tag_input("   ").unwrap().is_empty());
        let long = "x".repeat(MAX_TAG_LEN + 1);
        let err = parse_tag_input(&format!("ok, {long}")).unwrap_err();
        assert_eq!(err, TagInputError::TooLong(long));
        assert!(err.to_string().contains("too long"));
        // Exactly MAX_TAG_LEN is fine.
        assert!(parse_tag_input(&"y".repeat(MAX_TAG_LEN)).is_ok());
    }

    #[test]
    fn suggestions_rank_prefix_before_substring() {
        let known: Vec<(String, usize)> = vec![
            ("night".into(), 9),
            ("tavern".into(), 4),
            ("nightmare".into(), 2),
            ("alight".into(), 7),
            ("loud".into(), 1),
        ];
        let have = vec!["loud".to_string()];
        // prefix "nig": night (9), nightmare (2); substring: alight? no ("alight" contains "ight" not "nig")
        assert_eq!(
            suggest_tags("nig", &known, &have, 8),
            ["night", "nightmare"]
        );
        // substring match comes after prefix matches.
        assert_eq!(
            suggest_tags("ight", &known, &have, 8),
            ["night", "alight", "nightmare"]
        );
        // empty fragment: most used first, tags the sound has are hidden.
        assert_eq!(
            suggest_tags("", &known, &have, 3),
            ["night", "alight", "tavern"]
        );
        // only the fragment after the last comma matters; typed tags are not offered again.
        assert_eq!(suggest_tags("night, tav", &known, &have, 8), ["tavern"]);
        assert!(suggest_tags("night, nig", &known, &have, 8).contains(&"nightmare".to_string()));
        assert!(!suggest_tags("night, nig", &known, &have, 8).contains(&"night".to_string()));
    }

    #[test]
    fn completion_replaces_only_the_last_fragment() {
        assert_eq!(complete_tag_input("tav", "tavern"), "tavern");
        assert_eq!(complete_tag_input("a, tav", "tavern"), "a, tavern");
        assert_eq!(complete_tag_input("a,", "tavern"), "a, tavern");
    }

    #[test]
    fn tag_line_is_short() {
        let tags: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(format_tags(&tags, 5), "#a #b #c");
        assert_eq!(format_tags(&tags, 2), "#a #b +1");
        assert_eq!(format_tags(&[], 2), "");
    }

    #[test]
    fn highlight_job_marks_matches() {
        let job = highlight_job(
            "Tavern crowd",
            &["crow".to_string()],
            FontId::default(),
            Color32::GRAY,
            Color32::WHITE,
            Color32::RED,
        );
        assert_eq!(job.text, "Tavern crowd");
        assert_eq!(job.sections.len(), 3);
        assert_eq!(job.sections[0].byte_range, 0..7);
        assert_eq!(job.sections[1].byte_range, 7..11);
        assert_eq!(job.sections[1].format.background, Color32::RED);
        assert_eq!(job.sections[2].format.background, Color32::TRANSPARENT);

        let plain = highlight_job(
            "abc",
            &[],
            FontId::default(),
            Color32::GRAY,
            Color32::WHITE,
            Color32::RED,
        );
        assert_eq!(plain.sections.len(), 1);
        let empty = highlight_job(
            "",
            &[],
            FontId::default(),
            Color32::GRAY,
            Color32::WHITE,
            Color32::RED,
        );
        assert!(empty.text.is_empty());
    }

    #[test]
    fn missing_check_reports_reloads() {
        let paths: Vec<PathBuf> = ["/a/1.wav", "/a/2.wav", "/a/3.wav"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let reload: HashSet<PathBuf> = [PathBuf::from("/a/2.wav"), PathBuf::from("/a/3.wav")]
            .into_iter()
            .collect();
        let result = check_missing(&paths, &reload, |p| p != Path::new("/a/3.wav"));
        assert_eq!(result.missing.len(), 1);
        assert!(result.missing.contains(Path::new("/a/3.wav")));
        // 2 exists and was asked about; 1 exists but was not asked; 3 is missing.
        assert_eq!(result.reload, [PathBuf::from("/a/2.wav")]);
    }

    #[test]
    fn nearest_ancestor_skips_missing_folders() {
        let exists = |p: &Path| p == Path::new("/music");
        assert_eq!(
            nearest_existing_ancestor(Path::new("/music/old/deep/a.wav"), exists),
            Some(PathBuf::from("/music"))
        );
        assert_eq!(
            nearest_existing_ancestor(Path::new("/other/a.wav"), exists),
            None
        );
        assert_eq!(
            nearest_existing_ancestor(Path::new("a.wav"), |_| true),
            None
        );
    }

    #[test]
    fn common_parent_is_a_real_folder() {
        let a = Path::new("/music/rpg/tavern/a.wav");
        let b = Path::new("/music/rpg/combat/b.wav");
        let c = Path::new("/music/rpg/c.wav");
        assert_eq!(common_parent(&[a, b]), Some(PathBuf::from("/music/rpg")));
        assert_eq!(common_parent(&[a, b, c]), Some(PathBuf::from("/music/rpg")));
        assert_eq!(
            common_parent(&[a]),
            Some(PathBuf::from("/music/rpg/tavern"))
        );
        // Only the root in common: nothing useful.
        assert_eq!(common_parent(&[a, Path::new("/other/x.wav")]), None);
        assert_eq!(common_parent(&[]), None);
        // The sibling folder "rpg2" does not share "rpg".
        assert_eq!(
            common_parent(&[Path::new("/m/rpg/a.wav"), Path::new("/m/rpg2/b.wav")]),
            Some(PathBuf::from("/m"))
        );
    }

    fn scanned(path: &str, folder: Option<&str>) -> ScannedFile {
        ScannedFile {
            path: PathBuf::from(path),
            size: 10,
            folder: folder.map(str::to_string),
        }
    }

    #[test]
    fn scan_summary_counts_groups() {
        let mut lib = Library::new();
        lib.add_group("Tavern").unwrap();
        let report = ScanReport {
            root: PathBuf::from("/s"),
            files: vec![
                scanned("/s/a.wav", None),
                scanned("/s/tavern/b.wav", Some("tavern")),
                scanned("/s/tavern/x/c.wav", Some("tavern")),
                scanned("/s/Combat/d.wav", Some("Combat")),
                scanned("/s/combat2/e.wav", Some("COMBAT")),
                scanned("/s/blank/f.wav", Some("   ")),
            ],
            stats: ScanStats {
                skipped_existing: 2,
                skipped_duplicates: 3,
                skipped_empty: 0,
                skipped_links: 1,
                errors: 4,
                ..ScanStats::default()
            },
            ..ScanReport::default()
        };
        let summary = summarize_scan(&report, &lib);
        assert_eq!(summary.new_sounds, 6);
        assert_eq!(summary.top_level, 1);
        assert_eq!(summary.reused_groups, 1, "Tavern exists (case-insensitive)");
        assert_eq!(summary.new_groups, ["Combat"], "COMBAT folds into Combat");
        assert_eq!(summary.folders[0], ("tavern".to_string(), 2));
        let lines = skipped_lines(&summary);
        assert_eq!(lines.len(), 4);
        assert!(lines[1].starts_with("3 duplicates"));
        assert!(skipped_lines(&ScanSummary::default()).is_empty());
    }

    #[test]
    fn scan_summary_matches_what_applying_does() {
        let mut lib = Library::new();
        lib.add_group("Tavern").unwrap();
        let long = "L".repeat(MAX_GROUP_NAME_LEN + 10);
        let report = ScanReport {
            root: PathBuf::from("/s"),
            files: vec![
                scanned("/s/tavern/a.wav", Some("tavern")),
                scanned("/s/new/b.wav", Some("new")),
                scanned("/s/long/c.wav", Some(&long)),
            ],
            ..ScanReport::default()
        };
        let summary = summarize_scan(&report, &lib);
        let before = lib.groups().len();
        let stats = report.apply_to_library(&mut lib, true);
        assert_eq!(stats.groups_created, summary.new_groups.len());
        assert_eq!(lib.groups().len(), before + summary.new_groups.len());
        for name in &summary.new_groups {
            assert!(lib.groups().iter().any(|g| &g.name == name), "{name}");
        }
    }

    #[test]
    fn progress_line_reads_well() {
        let stats = ScanStats {
            dirs_visited: 3,
            audio_seen: 40,
            found: 12,
            ..ScanStats::default()
        };
        assert_eq!(
            scan_progress_line(&stats),
            "3 folders - 40 audio files seen - 12 new"
        );
    }

    #[test]
    fn remap_preview_counts_missing_hits() {
        let lib = library_with(&["/old/a.wav", "/old/sub/b.wav", "/keep/c.wav"]);
        let missing: HashSet<PathBuf> = [PathBuf::from("/old/a.wav")].into_iter().collect();
        let preview = preview_remap(&lib, &missing, "/old", "/new", 1);
        assert_eq!(preview.count, 2);
        assert_eq!(preview.missing_hits, 1);
        assert_eq!(
            preview.samples,
            [("/old/a.wav".to_string(), "/new/a.wav".to_string())]
        );
        let none = preview_remap(&lib, &missing, "/nothing", "/new", 3);
        assert_eq!(none, RemapPreview::default());
        let empty = preview_remap(&lib, &missing, "", "/new", 3);
        assert_eq!(empty.count, 0);
    }

    #[test]
    fn remap_moves_agree_with_remap_prefix() {
        let mut lib = library_with(&["/old/a.wav", "/old/b.wav", "/keep/c.wav"]);
        let moves = remap_moves(&lib, "/old", "/new");
        assert_eq!(moves.len(), 2);
        let applied = lib.remap_prefix("/old", "/new");
        assert_eq!(applied, moves.len());
        for (old, new) in &moves {
            assert!(lib.sound(old).is_none());
            assert!(lib.sound(new).is_some());
        }
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(sanitize_file_stem("My: set/1?"), "My_ set_1_");
        assert_eq!(sanitize_file_stem("   "), "bundle");
        assert_eq!(sanitize_file_stem("..."), "bundle");
        assert_eq!(bundle_file_name("Tavern pack"), "Tavern pack.zip");
        assert_eq!(
            with_zip_extension(Path::new("/x/set")),
            PathBuf::from("/x/set.zip")
        );
        assert_eq!(
            with_zip_extension(Path::new("/x/set.ZIP")),
            PathBuf::from("/x/set.ZIP")
        );
        assert_eq!(
            with_zip_extension(Path::new("/x/set.tar")),
            PathBuf::from("/x/set.tar.zip")
        );
    }

    #[test]
    fn import_dir_is_named_after_the_bundle() {
        let base = Path::new("/lib/imported");
        assert_eq!(
            default_import_dir(base, "Tavern: night", Path::new("/dl/x.zip")),
            PathBuf::from("/lib/imported/Tavern_ night")
        );
        assert_eq!(
            default_import_dir(base, "", Path::new("/dl/pack one.zip")),
            PathBuf::from("/lib/imported/pack one")
        );
        // A hostile name cannot climb out of the base folder.
        let dir = default_import_dir(base, "../../etc", Path::new("/dl/x.zip"));
        assert!(dir.starts_with(base));
        assert!(!dir.components().any(|c| matches!(c, Component::ParentDir)));
    }

    #[test]
    fn bytes_are_readable() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn import_lines_skip_zero_rows() {
        let report = ImportReport {
            sounds: EntityStats {
                added: 3,
                replaced: 1,
                renamed: 0,
                skipped: 2,
            },
            files_written: 3,
            bytes_written: 2048,
            dry_run: true,
            ..ImportReport::default()
        };
        assert_eq!(
            import_report_lines(&report),
            [
                "Sounds: 3 new, 1 replaced, 2 skipped".to_string(),
                "Audio files to write: 3 (2.0 KB)".to_string()
            ]
        );
        assert_eq!(
            import_report_lines(&ImportReport::default()),
            ["Nothing to import"]
        );
    }

    #[test]
    fn policies_have_text() {
        for policy in [
            ConflictPolicy::Skip,
            ConflictPolicy::Rename,
            ConflictPolicy::Replace,
        ] {
            assert!(!policy_label(policy).is_empty());
            assert!(!policy_help(policy).is_empty());
        }
        assert!(policy_help(ConflictPolicy::Replace).contains("overwritten"));
    }

    #[test]
    fn bundle_errors_are_explained() {
        let unsafe_path = describe_bundle_error(&BundleError::UnsafePath("../evil".into()));
        assert!(unsafe_path.contains("../evil"));
        assert!(unsafe_path.contains("Nothing was imported"));
        assert!(is_safety_error(&BundleError::UnsafePath("x".into())));

        let limit = describe_bundle_error(&BundleError::LimitExceeded("too many entries".into()));
        assert!(limit.contains("too many entries"));
        assert!(is_safety_error(&BundleError::LimitExceeded("x".into())));

        assert!(describe_bundle_error(&BundleError::MissingManifest).contains("bundle.toml"));
        assert!(describe_bundle_error(&BundleError::Manifest("bad id".into())).contains("bad id"));
        assert!(describe_bundle_error(&BundleError::UnsupportedVersion(9)).contains('9'));
        assert!(describe_bundle_error(&BundleError::Cancelled).contains("Cancelled"));
        let io = describe_bundle_error(&BundleError::Io(std::io::Error::other("disk full")));
        assert!(io.contains("disk full"));
        assert!(!is_safety_error(&BundleError::MissingManifest));
        assert!(!is_safety_error(&BundleError::Cancelled));
    }
}
