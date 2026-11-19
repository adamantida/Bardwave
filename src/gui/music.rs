//! Music and emitters in the console: the transport bar, the playlist manager
//! and the "Emitters & music" dialog of a scene.
//!
//! ```text
//!   draw_*(ui, &MusicEnv, &mut MusicUi) -> Vec<MusicAction>      (egui, pure view)
//!                                  |
//!   SoundSharedApp::apply_music_action  -> Library edits + Director commands
//! ```
//!
//! * Everything that decides *what to show* (clock text, which button the
//!   play key is, where a dragged track lands, what the countdown says) is a
//!   plain function with a unit test.
//! * The drawing functions only read a [`MusicEnv`] snapshot and a
//!   [`MusicUi`] (selection, text fields, drags) and answer with typed
//!   [`MusicAction`]s. They never touch the engine, so a frame cannot block on
//!   audio, and they can run in a headless egui context in the tests.
//! * [`MusicUi::observe`] copies the Director's music state and emitter
//!   snapshot once per frame (both are cheap clones, no engine call).
//! * Library edits go through the [`Library`] API (`add_playlist`,
//!   `set_emitters`, ...) and are followed by `sync_director`, so a playlist
//!   that plays picks the edit up at the next tick.
//!
//! # Where the scene UI plugs in
//!
//! [`SoundSharedApp::open_edit_emitters`] opens `Dialog::EditEmitters(scene)`
//! (emitters, plus the scene's music playlist); the playlist manager lists
//! every scene with a button for it as well.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{
    pos2, vec2, Align, Color32, Context, Layout, Rect, Response, RichText, Rounding, Sense, Stroke,
    Ui, UiBuilder, Vec2,
};

use crate::audio::{Director, EmitterSnapshot, PlaylistCommand, PlaylistState, PlaylistStatus};
use crate::config::RepeatMode;
use crate::gui::app::{mirror_playing, sync_director, Dialog, SoundSharedApp};
use crate::gui::groups::{Emitter, Library, LibrarySound, Playlist, PlaylistId, SceneId, SoundId};
use crate::gui::settings_extras::kit::{self as panel_kit, modal_window};
use crate::gui::theme::{
    alpha, hairline, micro_label, micro_label_accent, mono_font, panel_title, row_subline,
    telemetry, ui_font, Metrics, Palette,
};
use crate::gui::widgets::{elide, glyph, level_slider, paint_icon, BtnKind, ConsoleButton, Glyph};

/// Height of the transport bar above the status strip.
pub const MUSIC_BAR_H: f32 = 56.0;
/// Longest crossfade the manager offers.
pub const MAX_CROSSFADE_MS: u32 = 30_000;
/// Shortest interval of an emitter (the scheduler floors lower ones anyway).
pub const MIN_EMITTER_INTERVAL_MS: u32 = 100;
/// Longest interval the dialog offers (one hour).
pub const MAX_EMITTER_INTERVAL_MS: u32 = 3_600_000;
/// Most simultaneous instances the dialog offers.
pub const MAX_EMITTER_OVERLAP: u32 = 16;
/// Seeking never lands closer than this to the end (it would end the track).
pub const SEEK_END_GUARD_MS: u64 = 500;

const NOW_PLAYING_W: f32 = 232.0;
const MANAGER_W: f32 = 780.0;
const EMITTERS_W: f32 = 600.0;

// =====================================================================
// Pure view logic
// =====================================================================

/// `m:ss`, or `h:mm:ss` from an hour up.
pub fn format_clock(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Position in the track as `0.0..=1.0` (0 when the length is unknown).
pub fn progress_fraction(position_ms: u64, duration_ms: Option<u64>) -> f32 {
    match duration_ms {
        Some(d) if d > 0 => (position_ms as f32 / d as f32).clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// Horizontal pointer position to `0.0..=1.0` over a bar.
pub fn fraction_from_x(x: f32, left: f32, width: f32) -> f32 {
    if width <= 0.0 || !x.is_finite() {
        return 0.0;
    }
    ((x - left) / width).clamp(0.0, 1.0)
}

/// Where a click or release at `fraction` of the bar seeks to, in
/// milliseconds. `None` while the length is unknown (nothing to seek in).
pub fn seek_target_ms(fraction: f32, duration_ms: Option<u64>) -> Option<u64> {
    let duration = duration_ms.filter(|d| *d > 0)?;
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let target = (duration as f64 * f64::from(fraction)).round() as u64;
    Some(target.min(duration.saturating_sub(SEEK_END_GUARD_MS)))
}

/// What the play key of the transport does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayAction {
    /// Music plays: pause it.
    Pause,
    /// Music is paused: continue.
    Resume,
    /// Nothing plays: start this playlist (library id).
    Start(String),
    /// Nothing plays and there is no playlist to start: open the manager.
    OpenManager,
}

/// What the transport bar offers for a given music state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportView {
    pub play: PlayAction,
    /// Previous / next / stop make sense (a playlist plays or is paused).
    pub active: bool,
    /// The track has a known length, so the bar can seek.
    pub can_seek: bool,
}

/// Is a playlist playing or paused?
pub fn is_active(state: &PlaylistState) -> bool {
    matches!(
        state.status,
        PlaylistStatus::Playing | PlaylistStatus::Paused
    )
}

/// The transport for `state`. A stopped transport restarts the playlist the
/// operator selected in the manager, else the one that played last, else the
/// first playlist of the library.
pub fn transport_view(
    state: &PlaylistState,
    selected: Option<&str>,
    first: Option<&str>,
) -> TransportView {
    let active = is_active(state);
    let play = match state.status {
        PlaylistStatus::Playing => PlayAction::Pause,
        PlaylistStatus::Paused => PlayAction::Resume,
        PlaylistStatus::Stopped | PlaylistStatus::Finished => selected
            .or(state.playlist.as_deref())
            .or(first)
            .map(|id| PlayAction::Start(id.to_string()))
            .unwrap_or(PlayAction::OpenManager),
    };
    TransportView {
        play,
        active,
        can_seek: active && state.duration_ms.is_some_and(|d| d > 0),
    }
}

/// The play button of one playlist in the manager.
pub fn playlist_play_action(state: &PlaylistState, playlist: &str) -> PlayAction {
    if state.playlist.as_deref() == Some(playlist) {
        match state.status {
            PlaylistStatus::Playing => return PlayAction::Pause,
            PlaylistStatus::Paused => return PlayAction::Resume,
            _ => {}
        }
    }
    PlayAction::Start(playlist.to_string())
}

/// Does `state` describe the playlist with this library id?
pub fn is_loaded(state: &PlaylistState, playlist: &str) -> bool {
    state.playlist.as_deref() == Some(playlist)
}

/// "Artist - Title" of the playing track; the sound's own name (looked up by
/// library id with `name_of`) when the file has no tags. `None` without a
/// current track.
pub fn track_display(
    state: &PlaylistState,
    name_of: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if !is_active(state) && state.track_index.is_none() {
        return None;
    }
    let id = state.track.as_deref()?;
    let fallback = name_of(id).unwrap_or_else(|| "Unknown track".to_string());
    Some(state.label(&fallback))
}

/// "Track 3 / 12" (1-based) while a track is current.
pub fn track_position_label(state: &PlaylistState) -> Option<String> {
    let index = state.track_index?;
    Some(format!(
        "Track {} / {}",
        index + 1,
        state.track_count.max(index + 1)
    ))
}

/// One word for the status.
pub fn status_label(status: PlaylistStatus) -> &'static str {
    match status {
        PlaylistStatus::Stopped => "Stopped",
        PlaylistStatus::Playing => "Playing",
        PlaylistStatus::Paused => "Paused",
        PlaylistStatus::Finished => "Finished",
    }
}

/// The next end-of-list mode when the repeat button is pressed.
pub fn next_repeat(mode: RepeatMode) -> RepeatMode {
    match mode {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    }
}

/// Label of a repeat mode.
pub fn repeat_label(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "Off",
        RepeatMode::All => "All",
        RepeatMode::One => "One",
    }
}

/// "Off" (gapless) or "2.5 s".
pub fn crossfade_label(ms: u32) -> String {
    if ms == 0 {
        "Off".to_string()
    } else {
        format!("{:.1} s", ms as f32 / 1000.0)
    }
}

/// Seconds from a drag value to a crossfade in ms, within `0..=MAX_CROSSFADE_MS`.
pub fn crossfade_ms_from_secs(secs: f32) -> u32 {
    if !secs.is_finite() || secs <= 0.0 {
        return 0;
    }
    ((secs * 1000.0).round() as u32).min(MAX_CROSSFADE_MS)
}

/// Name for a sound in lists: its name, else the file stem.
pub fn sound_label(sound: &LibrarySound) -> String {
    if !sound.name.trim().is_empty() {
        return sound.name.clone();
    }
    sound
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unnamed sound")
        .to_string()
}

/// Name of a library sound by id, `"Missing sound"` when it was removed.
pub fn sound_name(library: &Library, id: SoundId) -> String {
    library
        .sound_by_id(id)
        .map(sound_label)
        .unwrap_or_else(|| "Missing sound".to_string())
}

/// `base`, or `base 2`, `base 3`, ... for the first spelling `taken` allows.
pub fn unique_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|candidate| !taken(candidate))
        .expect("an unused name exists")
}

/// Case-insensitive "is this name in use" over playlists.
pub fn playlist_name_taken(library: &Library, name: &str) -> bool {
    let wanted = name.trim().to_lowercase();
    library
        .playlists()
        .iter()
        .any(|p| p.name.to_lowercase() == wanted)
}

/// Create an empty playlist called "New playlist" (numbered when taken).
pub fn create_playlist(library: &mut Library) -> Option<PlaylistId> {
    let name = unique_name("New playlist", |n| playlist_name_taken(library, n));
    library.add_playlist(&name)
}

/// The index the track ends up at when `from` is dropped on `slot` (a gap
/// between rows: `0` before the first row, `len` after the last).
pub fn move_target(len: usize, from: usize, slot: usize) -> usize {
    let slot = slot.min(len);
    let target = if slot > from { slot - 1 } else { slot };
    target.min(len.saturating_sub(1))
}

/// The gap a pointer at `y` is over, given the vertical centres of the rows:
/// the number of rows whose centre lies above the pointer.
pub fn drop_slot(centers: &[f32], y: f32) -> usize {
    centers.iter().filter(|c| **c < y).count()
}

/// Move a track of a playlist by dropping it on `slot`. False when nothing
/// changes (same place, unknown playlist, bad index).
pub fn move_track(library: &mut Library, playlist: PlaylistId, from: usize, slot: usize) -> bool {
    let Some(items) = library.playlist(playlist).map(|p| p.items.clone()) else {
        return false;
    };
    if from >= items.len() {
        return false;
    }
    let target = move_target(items.len(), from, slot);
    if target == from {
        return false;
    }
    let mut items = items;
    let moved = items.remove(from);
    items.insert(target, moved);
    library.set_playlist_items(playlist, items)
}

/// Sounds whose name or tags contain `query` (case-insensitive), at most
/// `limit`, in library order. An empty query lists everything.
pub fn filter_sounds<'a>(
    sounds: &'a [LibrarySound],
    query: &str,
    limit: usize,
) -> Vec<&'a LibrarySound> {
    let query = query.trim().to_lowercase();
    sounds
        .iter()
        .filter(|sound| {
            query.is_empty()
                || sound_label(sound).to_lowercase().contains(&query)
                || sound.tags.iter().any(|tag| tag.contains(&query))
        })
        .take(limit)
        .collect()
}

/// `"20 s"`, `"90 s"`, `"1.5 s"`, `"2 min 05 s"` (minutes from two minutes up).
pub fn format_seconds(ms: u64) -> String {
    if ms >= 120_000 {
        let secs = ms / 1000;
        return format!("{} min {:02} s", secs / 60, secs % 60);
    }
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{:.1} s", ms as f32 / 1000.0)
    }
}

/// Time to the next sound, rounded up to whole seconds.
pub fn format_countdown(ms: u64) -> String {
    if ms == 0 {
        return "now".to_string();
    }
    let secs = ms.div_ceil(1000);
    if secs >= 60 {
        format!("{} min {:02} s", secs / 60, secs % 60)
    } else {
        format!("{secs} s")
    }
}

/// The emitter's status line: "Next in 12 s", plus "2 playing" when its
/// sounds still sound. `None` snapshot = the scene is not running.
pub fn countdown_label(snapshot: Option<&EmitterSnapshot>) -> String {
    let Some(snapshot) = snapshot else {
        return "Scene not active".to_string();
    };
    if !snapshot.enabled {
        return "No playable sounds".to_string();
    }
    let mut text = match snapshot.next_in_ms {
        Some(ms) => format!("Next in {}", format_countdown(ms)),
        None => "Waiting".to_string(),
    };
    if snapshot.active > 0 {
        text.push_str(&format!(" · {} playing", snapshot.active));
    }
    text
}

/// "20 s - 60 s".
pub fn interval_label(min_ms: u32, max_ms: u32) -> String {
    format!(
        "{} - {}",
        format_seconds(u64::from(min_ms)),
        format_seconds(u64::from(max_ms))
    )
}

/// Which bound of an emitter interval was edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalEdit {
    Min(u32),
    Max(u32),
}

/// Apply an interval edit and keep `min <= max` within the legal range: a
/// minimum raised past the maximum drags the maximum along, a maximum lowered
/// below the minimum drags the minimum.
pub fn set_interval(min_ms: u32, max_ms: u32, edit: IntervalEdit) -> (u32, u32) {
    let clamp = |ms: u32| ms.clamp(MIN_EMITTER_INTERVAL_MS, MAX_EMITTER_INTERVAL_MS);
    match edit {
        IntervalEdit::Min(value) => {
            let min = clamp(value);
            (min, max_ms.max(min))
        }
        IntervalEdit::Max(value) => {
            let max = clamp(value);
            (min_ms.min(max), max)
        }
    }
}

/// Why an emitter cannot be saved, if it cannot.
pub fn emitter_issue(emitter: &Emitter) -> Option<&'static str> {
    if emitter.sounds.is_empty() {
        Some("add at least one sound")
    } else {
        None
    }
}

/// The scene's emitters and music layer being edited in the dialog. Nothing
/// reaches the library until [`EmitterDraft::commit`].
#[derive(Debug, Clone, PartialEq)]
pub struct EmitterDraft {
    pub scene: SceneId,
    pub emitters: Vec<Emitter>,
    pub playlist: Option<PlaylistId>,
}

impl EmitterDraft {
    /// A copy of what the scene has now. `None` for an unknown scene.
    pub fn from_library(library: &Library, scene: SceneId) -> Option<Self> {
        let current = library.scene(scene)?;
        Some(Self {
            scene,
            emitters: current.emitters.clone(),
            playlist: current.playlist,
        })
    }

    /// Differs from the library's version.
    pub fn is_dirty(&self, library: &Library) -> bool {
        match library.scene(self.scene) {
            Some(scene) => scene.emitters != self.emitters || scene.playlist != self.playlist,
            None => false,
        }
    }

    /// First problem that blocks saving ("Emitter 2: add at least one sound").
    pub fn issue(&self) -> Option<String> {
        self.emitters
            .iter()
            .enumerate()
            .find_map(|(index, emitter)| {
                emitter_issue(emitter).map(|why| format!("Emitter {}: {why}", index + 1))
            })
    }

    /// Write the draft into the library. False (library unchanged) when the
    /// draft has an issue or the scene / a sound / the playlist is gone.
    pub fn commit(&self, library: &mut Library) -> bool {
        if self.issue().is_some() {
            return false;
        }
        if let Some(playlist) = self.playlist {
            if library.playlist(playlist).is_none() {
                return false;
            }
        }
        if !library.set_emitters(self.scene, self.emitters.clone()) {
            return false;
        }
        library.set_scene_playlist(self.scene, self.playlist)
    }
}

/// How tall the dialogs' scroll lists may be on a window `screen_h` tall:
/// `(tracks / emitters list, sound picker)`.
pub fn list_heights(screen_h: f32) -> (f32, f32) {
    let free = (screen_h - 360.0).clamp(150.0, 330.0);
    (free * 0.62, free * 0.38)
}

// =====================================================================
// State
// =====================================================================

/// Which page of the manager is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MusicTab {
    #[default]
    Playlists,
    Scenes,
}

/// Everything the console asks of the app for music, typed.
#[derive(Debug, Clone, PartialEq)]
pub enum MusicAction {
    /// A command for the playlist player.
    Command(PlaylistCommand),
    /// Open the playlist manager.
    OpenManager,
    /// Close the open music dialog.
    Close,
    /// Open the emitters dialog of a scene.
    OpenEmitters(SceneId),
    /// Create a playlist and start naming it.
    CreatePlaylist,
    RenamePlaylist(PlaylistId, String),
    DeletePlaylist(PlaylistId),
    AddTrack(PlaylistId, SoundId),
    RemoveTrack(PlaylistId, usize),
    /// Drop track `from` on `slot` (a gap, see [`move_target`]).
    MoveTrack {
        playlist: PlaylistId,
        from: usize,
        slot: usize,
    },
    SetMode {
        playlist: PlaylistId,
        shuffle: bool,
        repeat: RepeatMode,
    },
    SetCrossfade(PlaylistId, u32),
    /// Write the emitters dialog's draft into the library.
    SaveEmitters,
}

/// View state of the music widgets (selection, text fields, drags) plus the
/// latest copy of the Director's music state.
#[derive(Debug, Default)]
pub struct MusicUi {
    live: PlaylistState,
    emitters: Vec<EmitterSnapshot>,
    active_scene: Option<String>,
    tab: MusicTab,
    selected: Option<PlaylistId>,
    rename: Option<(PlaylistId, String)>,
    confirm_delete: Option<PlaylistId>,
    filter: String,
    /// Fraction under the pointer while the seek bar is dragged.
    seek_drag: Option<f32>,
    /// Track row being dragged and the pointer's last vertical position.
    drag_track: Option<usize>,
    drag_y: f32,
    /// Last problem to show the operator (a command that failed, a name that
    /// was refused).
    message: Option<String>,
    draft: Option<EmitterDraft>,
}

impl MusicUi {
    /// Copy the Director's music and emitter state. Cheap (two small clones);
    /// called once per frame after the Director ticked.
    pub fn observe(&mut self, director: &Director, now: Instant) {
        self.live = director.playlist_state();
        self.emitters = director.emitter_snapshot(now);
        if self.active_scene.as_deref() != director.active_scene() {
            self.active_scene = director.active_scene().map(str::to_string);
        }
    }

    /// The music state as last observed.
    pub fn live(&self) -> &PlaylistState {
        &self.live
    }

    /// How soon the UI must repaint for what it shows: the progress of a
    /// playing track, the emitter countdowns of an open dialog.
    pub fn repaint_hint(&self, dialog: &Dialog) -> Duration {
        if matches!(dialog, Dialog::EditEmitters(_)) && !self.emitters.is_empty() {
            Duration::from_millis(200)
        } else if self.live.status == PlaylistStatus::Playing {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(3600)
        }
    }

    /// The problem to show, if any.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    fn selected_str(&self) -> Option<String> {
        self.selected.map(|id| id.to_string())
    }
}

/// What the drawing functions may look at.
pub struct MusicEnv<'a> {
    pub library: &'a Library,
    pub live: PlaylistState,
    pub emitters: Vec<EmitterSnapshot>,
    pub active_scene: Option<String>,
    /// Length in seconds of loaded sounds, by library id.
    pub durations: HashMap<SoundId, f32>,
}

impl<'a> MusicEnv<'a> {
    /// An environment from the library and the observed state.
    pub fn new(library: &'a Library, ui: &MusicUi, durations: HashMap<SoundId, f32>) -> Self {
        Self {
            library,
            live: ui.live.clone(),
            emitters: ui.emitters.clone(),
            active_scene: ui.active_scene.clone(),
            durations,
        }
    }

    fn first_playlist(&self) -> Option<String> {
        self.library.playlists().first().map(|p| p.id.to_string())
    }

    fn track_name(&self, id: &str) -> Option<String> {
        let id = uuid::Uuid::parse_str(id).ok()?;
        self.library.sound_by_id(id).map(sound_label)
    }

    fn total_seconds(&self, playlist: &Playlist) -> (f32, usize) {
        let mut total = 0.0;
        let mut unknown = 0;
        for item in &playlist.items {
            match self.durations.get(item) {
                Some(secs) => total += secs,
                None => unknown += 1,
            }
        }
        (total, unknown)
    }
}

// =====================================================================
// Small painted widgets
// =====================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ico {
    Glyph(Glyph),
    Pause,
    Next,
    Prev,
    Up,
    Down,
    Grip,
}

fn paint_ico(painter: &egui::Painter, ico: Ico, center: egui::Pos2, size: f32, color: Color32) {
    use egui::Shape;
    let p = |x: f32, y: f32| pos2(center.x + x * size, center.y + y * size);
    let stroke = Stroke::new((size * 0.11).max(1.2), color);
    let tri = |a, b, c| Shape::convex_polygon(vec![a, b, c], color, Stroke::NONE);
    match ico {
        Ico::Glyph(glyph) => paint_icon(painter, glyph, center, size, color),
        Ico::Pause => {
            for x in [-0.22, 0.22] {
                painter.rect_filled(
                    Rect::from_center_size(p(x, 0.0), vec2(size * 0.2, size * 0.64)),
                    Rounding::same(size * 0.03),
                    color,
                );
            }
        }
        Ico::Next => {
            painter.add(tri(p(-0.36, -0.36), p(0.2, 0.0), p(-0.36, 0.36)));
            painter.line_segment([p(0.36, -0.36), p(0.36, 0.36)], stroke);
        }
        Ico::Prev => {
            painter.add(tri(p(0.36, -0.36), p(-0.2, 0.0), p(0.36, 0.36)));
            painter.line_segment([p(-0.36, -0.36), p(-0.36, 0.36)], stroke);
        }
        Ico::Up => {
            painter.add(Shape::line(
                vec![p(-0.3, 0.14), p(0.0, -0.16), p(0.3, 0.14)],
                stroke,
            ));
        }
        Ico::Down => {
            painter.add(Shape::line(
                vec![p(-0.3, -0.14), p(0.0, 0.16), p(0.3, -0.14)],
                stroke,
            ));
        }
        Ico::Grip => {
            for x in [-0.14, 0.14] {
                for y in [-0.26, 0.0, 0.26] {
                    painter.circle_filled(p(x, y), size * 0.06, color);
                }
            }
        }
    }
}

/// Square icon button. A disabled button is dimmed and does not react.
fn tool_button(
    ui: &mut Ui,
    ico: Ico,
    size: f32,
    kind: BtnKind,
    enabled: bool,
    tooltip: &str,
    salt: &str,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), sense);
    let hovered = enabled && resp.hovered();
    let (fill, border, fg) = if !enabled {
        (
            Palette::TRANSPARENT,
            alpha(Palette::BORDER, 120),
            Palette::ZINC_600,
        )
    } else {
        match kind {
            BtnKind::Primary => {
                let fill = if hovered {
                    Palette::ROSE_700
                } else {
                    Palette::ROSE_600
                };
                (fill, fill, Palette::WHITE)
            }
            BtnKind::Armed => (
                Palette::ROSE_950,
                alpha(Palette::ROSE_500, if hovered { 255 } else { 150 }),
                Palette::ROSE_400,
            ),
            BtnKind::Danger => (
                if hovered {
                    alpha(Palette::ROSE_950, 180)
                } else {
                    Palette::TRANSPARENT
                },
                Palette::ROSE_700,
                Palette::ROSE_400,
            ),
            BtnKind::Ghost => (
                if hovered {
                    alpha(Palette::BORDER, 160)
                } else {
                    Palette::TRANSPARENT
                },
                if hovered {
                    Palette::BORDER_STRONG
                } else {
                    Palette::BORDER
                },
                if hovered {
                    Palette::WHITE
                } else {
                    Palette::ZINC_300
                },
            ),
        }
    };
    let painter = ui.painter();
    if fill != Palette::TRANSPARENT {
        painter.rect_filled(rect, Rounding::same(Metrics::R_SM), fill);
    }
    painter.rect_stroke(
        rect,
        Rounding::same(Metrics::R_SM),
        Stroke::new(1.0_f32, border),
    );
    paint_ico(painter, ico, rect.center(), (size * 0.5).round(), fg);
    let _ = salt;
    let resp = if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    };
    resp.on_hover_text(tooltip)
}

/// Thin seek bar. Returns the fraction to seek to on release / click; while
/// the pointer is down `drag` holds the fraction under it.
fn seek_bar(
    ui: &mut Ui,
    width: f32,
    fraction: f32,
    enabled: bool,
    drag: &mut Option<f32>,
) -> Option<f32> {
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 20.0), sense);
    let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - 1.0), vec2(width, 2.0));

    let mut commit = None;
    if enabled {
        if resp.is_pointer_button_down_on() {
            if let Some(pos) = resp.interact_pointer_pos() {
                *drag = Some(fraction_from_x(pos.x, track.left(), track.width()));
            }
        }
        if resp.clicked() || resp.drag_stopped() {
            if let Some(pos) = resp.interact_pointer_pos() {
                *drag = Some(fraction_from_x(pos.x, track.left(), track.width()));
            }
            commit = drag.take();
        }
    } else {
        *drag = None;
    }

    let shown = drag.unwrap_or(fraction).clamp(0.0, 1.0);
    let painter = ui.painter();
    painter.rect_filled(track, Rounding::ZERO, Palette::BORDER_STRONG);
    if enabled && shown > 0.0 {
        painter.rect_filled(
            Rect::from_min_size(track.min, vec2(track.width() * shown, track.height())),
            Rounding::ZERO,
            Palette::ROSE_500,
        );
    }
    if enabled {
        let size = if resp.hovered() || resp.dragged() {
            7.0
        } else {
            5.0
        };
        painter.rect_filled(
            Rect::from_center_size(
                pos2(track.left() + track.width() * shown, track.center().y),
                Vec2::splat(size),
            ),
            Rounding::ZERO,
            Palette::ROSE_500,
        );
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    commit
}

/// A selectable list row: name over a mono subline.
fn list_row(ui: &mut Ui, title: &str, subline: &str, selected: bool, live: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    let rounding = Rounding::same(Metrics::R_SM);
    if selected {
        painter.rect_filled(rect, rounding, alpha(Palette::ROSE_600, 40));
        painter.rect_stroke(rect, rounding, Stroke::new(1.0_f32, Palette::ROSE_500));
    } else if resp.hovered() {
        painter.rect_filled(rect, rounding, Palette::ROW_HOVER);
    }
    let color = if selected {
        Palette::WHITE
    } else {
        Palette::ZINC_300
    };
    let name = painter.layout_no_wrap(elide(title, 24), ui_font(13.0), color);
    painter.galley(pos2(rect.left() + 12.0, rect.top() + 7.0), name, color);
    let sub = painter.layout_no_wrap(subline.to_uppercase(), mono_font(9.0), Palette::ZINC_500);
    painter.galley(
        pos2(rect.left() + 12.0, rect.top() + 25.0),
        sub,
        Palette::ZINC_500,
    );
    if live {
        painter.circle_filled(
            pos2(rect.right() - 12.0, rect.center().y),
            3.5,
            Palette::ROSE_500,
        );
    }
    resp
}

fn error_text(ui: &mut Ui, message: &str) {
    ui.label(
        RichText::new(message.to_uppercase())
            .font(mono_font(9.0))
            .color(Palette::ROSE_400)
            .extra_letter_spacing(0.8),
    );
}

/// Centred modal window of a given width (the console frame).
fn window(ctx: &Context, id: &'static str, width: f32, add: impl FnOnce(&mut Ui)) {
    modal_window(ctx, id, width, add);
}

fn dialog_header(ui: &mut Ui, code: &str, title: &str, subtitle: &str) {
    ui.label(micro_label_accent(code));
    ui.add_space(2.0);
    ui.label(panel_title(title));
    ui.add_space(2.0);
    ui.label(micro_label(subtitle));
}

// =====================================================================
// Transport bar
// =====================================================================

/// Draw the transport bar into `ui` (the full-width bottom panel).
pub fn draw_bar(ui: &mut Ui, env: &MusicEnv, st: &mut MusicUi) -> Vec<MusicAction> {
    let mut actions = Vec::new();
    let full = ui.max_rect();
    let rect = Rect::from_min_max(
        pos2(full.left() + Metrics::APP_PAD, full.top()),
        pos2(full.right() - Metrics::APP_PAD, full.bottom() - 8.0),
    );
    ui.painter().rect(
        rect,
        Rounding::same(Metrics::R_PANEL),
        Palette::PANEL,
        Stroke::new(1.0_f32, Palette::BORDER),
    );
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(Metrics::LG, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    ui.spacing_mut().item_spacing.x = Metrics::SM;

    let live = &env.live;
    let selected = st.selected_str();
    let first = env.first_playlist();
    let view = transport_view(live, selected.as_deref(), first.as_deref());

    if tool_button(
        &mut ui,
        Ico::Prev,
        32.0,
        BtnKind::Ghost,
        view.active,
        "Previous track (restarts the track after 3 s)",
        "mb_prev",
    )
    .clicked()
    {
        actions.push(MusicAction::Command(PlaylistCommand::Prev));
    }
    let (play_ico, play_tip) = match &view.play {
        PlayAction::Pause => (Ico::Pause, "Pause"),
        PlayAction::Resume => (Ico::Glyph(glyph::PLAY), "Resume"),
        PlayAction::Start(_) => (Ico::Glyph(glyph::PLAY), "Play the playlist"),
        PlayAction::OpenManager => (Ico::Glyph(glyph::PLAY), "Create a playlist"),
    };
    if tool_button(
        &mut ui,
        play_ico,
        32.0,
        BtnKind::Primary,
        true,
        play_tip,
        "mb_play",
    )
    .clicked()
    {
        actions.push(play_action(&view.play));
    }
    if tool_button(
        &mut ui,
        Ico::Glyph(glyph::STOP),
        32.0,
        BtnKind::Ghost,
        view.active,
        "Stop the music",
        "mb_stop",
    )
    .clicked()
    {
        actions.push(MusicAction::Command(PlaylistCommand::Stop));
    }
    if tool_button(
        &mut ui,
        Ico::Next,
        32.0,
        BtnKind::Ghost,
        view.active,
        "Next track",
        "mb_next",
    )
    .clicked()
    {
        actions.push(MusicAction::Command(PlaylistCommand::Next));
    }
    ui.add_space(Metrics::SM);

    // Now playing.
    ui.allocate_ui_with_layout(
        vec2(NOW_PLAYING_W, 36.0),
        Layout::top_down(Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let title = track_display(live, |id| env.track_name(id));
            match &title {
                Some(title) => ui.label(
                    RichText::new(elide(title, 30))
                        .font(ui_font(13.0))
                        .color(Palette::WHITE),
                ),
                None => ui.label(
                    RichText::new("No music playing")
                        .font(ui_font(13.0))
                        .color(Palette::ZINC_500),
                ),
            };
            if let Some(message) = st.message().or(live.error.as_deref()) {
                ui.label(
                    RichText::new(elide(&message.to_uppercase(), 36))
                        .font(mono_font(9.0))
                        .color(Palette::ROSE_400),
                )
                .on_hover_text(message);
            } else {
                let sub = match (&live.name, track_position_label(live)) {
                    (Some(name), Some(pos)) => format!("{} · {}", elide(name, 18), pos),
                    (Some(name), None) => {
                        format!("{} · {}", elide(name, 18), status_label(live.status))
                    }
                    (None, _) => "Open Music to build a playlist".to_string(),
                };
                ui.label(row_subline(sub));
            }
        },
    );

    // Seek: clock, bar, clock. The right-hand group has a fixed width, the
    // bar takes what is left.
    const CLOCK_W: f32 = 44.0;
    const RIGHT_W: f32 = 86.0 + 104.0 + 88.0 + 3.0 * Metrics::SM;
    let seek_w = (ui.available_width() - RIGHT_W - 2.0 * CLOCK_W - 3.0 * Metrics::SM).max(60.0);
    let duration = live.duration_ms.filter(|_| view.active);
    let shown_pos = match (st.seek_drag, duration) {
        (Some(fraction), Some(_)) => seek_target_ms(fraction, duration).unwrap_or(0),
        _ => live.position_ms,
    };
    ui.allocate_ui_with_layout(
        vec2(CLOCK_W, 16.0),
        Layout::right_to_left(Align::Center),
        |ui| {
            let text = if view.active {
                format_clock(shown_pos)
            } else {
                "-:--".to_string()
            };
            ui.label(telemetry(text, 11.0, Palette::ZINC_400));
        },
    );
    if let Some(fraction) = seek_bar(
        &mut ui,
        seek_w,
        progress_fraction(live.position_ms, duration),
        view.can_seek,
        &mut st.seek_drag,
    ) {
        if let Some(ms) = seek_target_ms(fraction, duration) {
            actions.push(MusicAction::Command(PlaylistCommand::Seek(ms)));
        }
    }
    ui.allocate_ui_with_layout(
        vec2(CLOCK_W, 16.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            let text = match duration {
                Some(d) => format_clock(d),
                None => "-:--".to_string(),
            };
            ui.label(telemetry(text, 11.0, Palette::ZINC_400));
        },
    );

    // Shuffle / repeat of the loaded playlist.
    let loaded: Option<PlaylistId> = live
        .playlist
        .as_deref()
        .and_then(|id| uuid::Uuid::parse_str(id).ok());
    let shuffle = ConsoleButton::new("Shuffle")
        .small()
        .width(86.0)
        .kind(if live.shuffle {
            BtnKind::Armed
        } else {
            BtnKind::Ghost
        })
        .enabled(loaded.is_some())
        .tooltip("Play the tracks in random order")
        .id_salt("mb_shuffle")
        .show(&mut ui);
    if let (true, Some(playlist)) = (shuffle.clicked(), loaded) {
        actions.push(MusicAction::SetMode {
            playlist,
            shuffle: !live.shuffle,
            repeat: live.repeat,
        });
    }
    let repeat = ConsoleButton::new(format!("Repeat {}", repeat_label(live.repeat)))
        .small()
        .width(104.0)
        .kind(if live.repeat == RepeatMode::Off {
            BtnKind::Ghost
        } else {
            BtnKind::Armed
        })
        .enabled(loaded.is_some())
        .tooltip("What happens at the end: off, all tracks again, or the same track")
        .id_salt("mb_repeat")
        .show(&mut ui);
    if let (true, Some(playlist)) = (repeat.clicked(), loaded) {
        actions.push(MusicAction::SetMode {
            playlist,
            shuffle: live.shuffle,
            repeat: next_repeat(live.repeat),
        });
    }
    if ConsoleButton::ghost("Music…")
        .small()
        .width(88.0)
        .tooltip("Playlists, scene music and emitters")
        .id_salt("mb_music")
        .show(&mut ui)
        .clicked()
    {
        actions.push(MusicAction::OpenManager);
    }
    actions
}

fn play_action(action: &PlayAction) -> MusicAction {
    match action {
        PlayAction::Pause => MusicAction::Command(PlaylistCommand::Pause),
        PlayAction::Resume => MusicAction::Command(PlaylistCommand::Resume),
        PlayAction::Start(id) => MusicAction::Command(PlaylistCommand::Start(id.clone())),
        PlayAction::OpenManager => MusicAction::OpenManager,
    }
}

// =====================================================================
// Playlist manager
// =====================================================================

/// Draw the manager's contents (inside the modal window).
pub fn draw_manager(
    ui: &mut Ui,
    env: &MusicEnv,
    st: &mut MusicUi,
    screen_h: f32,
) -> Vec<MusicAction> {
    let mut actions = Vec::new();
    let library = env.library;

    // Keep the selection valid.
    if st.selected.is_none_or(|id| library.playlist(id).is_none()) {
        st.selected = library.playlists().first().map(|p| p.id);
    }

    dialog_header(
        ui,
        "Music",
        "Music & scenes",
        "Playlists play as background music; scenes can start one with them",
    );
    ui.add_space(Metrics::MD);
    ui.horizontal(|ui| {
        for (tab, label) in [
            (MusicTab::Playlists, "Playlists"),
            (MusicTab::Scenes, "Scenes"),
        ] {
            if ConsoleButton::new(label)
                .small()
                .width(110.0)
                .kind(if st.tab == tab {
                    BtnKind::Armed
                } else {
                    BtnKind::Ghost
                })
                .id_salt(label)
                .show(ui)
                .clicked()
            {
                st.tab = tab;
            }
        }
    });
    ui.add_space(Metrics::MD);

    match st.tab {
        MusicTab::Playlists => draw_playlists_tab(ui, env, st, screen_h, &mut actions),
        MusicTab::Scenes => draw_scenes_tab(ui, env, st, screen_h, &mut actions),
    }

    ui.add_space(Metrics::MD);
    hairline(ui);
    ui.add_space(Metrics::MD);
    ui.horizontal(|ui| {
        if ConsoleButton::primary("Done")
            .glyph(glyph::CHECK)
            .width(140.0)
            .height(Metrics::BTN_H)
            .id_salt("music_done")
            .show(ui)
            .clicked()
        {
            actions.push(MusicAction::Close);
        }
        if let Some(message) = st.message() {
            ui.add_space(Metrics::MD);
            error_text(ui, message);
        }
    });
    actions
}

fn draw_playlists_tab(
    ui: &mut Ui,
    env: &MusicEnv,
    st: &mut MusicUi,
    screen_h: f32,
    actions: &mut Vec<MusicAction>,
) {
    let library = env.library;
    let (tracks_h, picker_h) = list_heights(screen_h);
    let body_h = tracks_h + picker_h + 134.0;
    ui.horizontal_top(|ui| {
        // Left: the playlists.
        ui.allocate_ui_with_layout(vec2(210.0, body_h), Layout::top_down(Align::Min), |ui| {
            ui.label(micro_label("Playlists"));
            ui.add_space(Metrics::SM);
            egui::ScrollArea::vertical()
                .id_salt("music_playlists")
                .max_height(body_h - 80.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = Metrics::XS;
                    for playlist in library.playlists() {
                        let id = playlist.id.to_string();
                        let loaded = is_loaded(&env.live, &id) && is_active(&env.live);
                        let sub = format!(
                            "{} track{}{}",
                            playlist.items.len(),
                            if playlist.items.len() == 1 { "" } else { "s" },
                            if loaded { " · on air" } else { "" }
                        );
                        if list_row(
                            ui,
                            &playlist.name,
                            &sub,
                            st.selected == Some(playlist.id),
                            loaded,
                        )
                        .clicked()
                        {
                            st.selected = Some(playlist.id);
                            st.rename = None;
                            st.confirm_delete = None;
                        }
                    }
                    if library.playlists().is_empty() {
                        panel_kit::empty_state(
                            ui,
                            "No playlists yet",
                            "A playlist is background music: an ordered list of your sounds.",
                        );
                    }
                });
            ui.add_space(Metrics::SM);
            if ConsoleButton::primary("New playlist")
                .glyph(glyph::PLUS)
                .small()
                .width(210.0)
                .id_salt("music_new")
                .show(ui)
                .clicked()
            {
                actions.push(MusicAction::CreatePlaylist);
            }
        });
        ui.add_space(Metrics::LG);

        // Right: the selected playlist.
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), body_h),
            Layout::top_down(Align::Min),
            |ui| match st.selected.and_then(|id| library.playlist(id)) {
                Some(playlist) => {
                    draw_playlist_editor(ui, env, st, playlist, tracks_h, picker_h, actions)
                }
                None => {
                    if panel_kit::empty_state_action(
                        ui,
                        "Nothing selected",
                        "Create a playlist, then add sounds from your library. A scene can start \
                         it automatically.",
                        "Create a playlist",
                        "music_empty_create",
                    ) {
                        actions.push(MusicAction::CreatePlaylist);
                    }
                }
            },
        );
    });
}

fn draw_playlist_editor(
    ui: &mut Ui,
    env: &MusicEnv,
    st: &mut MusicUi,
    playlist: &Playlist,
    tracks_h: f32,
    picker_h: f32,
    actions: &mut Vec<MusicAction>,
) {
    let id = playlist.id;
    let id_str = id.to_string();
    let loaded = is_loaded(&env.live, &id_str);

    // Title row: name or the rename field; rename / delete.
    ui.horizontal(|ui| {
        if let Some((rename_id, text)) = st.rename.as_mut().filter(|(r, _)| *r == id) {
            let _ = rename_id;
            let resp = ui.add(
                egui::TextEdit::singleline(text)
                    .desired_width(260.0)
                    .font(ui_font(13.0))
                    .margin(egui::Margin::symmetric(8.0, 6.0)),
            );
            if !resp.has_focus() && !resp.lost_focus() {
                resp.request_focus();
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let accept = tool_button(
                ui,
                Ico::Glyph(glyph::CHECK),
                28.0,
                BtnKind::Primary,
                true,
                "Save the name",
                "pl_rename_ok",
            )
            .clicked();
            let cancel = tool_button(
                ui,
                Ico::Glyph(glyph::CROSS),
                28.0,
                BtnKind::Ghost,
                true,
                "Keep the old name",
                "pl_rename_no",
            )
            .clicked();
            if enter || accept {
                actions.push(MusicAction::RenamePlaylist(id, text.clone()));
                st.rename = None;
            } else if cancel {
                st.rename = None;
            }
        } else {
            ui.label(
                RichText::new(elide(&playlist.name, 30))
                    .font(ui_font(16.0))
                    .strong()
                    .color(Palette::WHITE),
            );
            if tool_button(
                ui,
                Ico::Glyph(glyph::EDIT),
                26.0,
                BtnKind::Ghost,
                true,
                "Rename the playlist",
                "pl_rename",
            )
            .clicked()
            {
                st.rename = Some((id, playlist.name.clone()));
            }
            if st.confirm_delete == Some(id) {
                if ConsoleButton::danger("Delete")
                    .glyph(glyph::TRASH)
                    .small()
                    .width(92.0)
                    .id_salt("pl_delete_yes")
                    .show(ui)
                    .clicked()
                {
                    actions.push(MusicAction::DeletePlaylist(id));
                    st.confirm_delete = None;
                }
                if ConsoleButton::ghost("Keep")
                    .small()
                    .width(64.0)
                    .id_salt("pl_delete_no")
                    .show(ui)
                    .clicked()
                {
                    st.confirm_delete = None;
                }
            } else if tool_button(
                ui,
                Ico::Glyph(glyph::TRASH),
                26.0,
                BtnKind::Danger,
                true,
                "Delete the playlist (sounds stay in the library)",
                "pl_delete",
            )
            .clicked()
            {
                st.confirm_delete = Some(id);
            }
        }
    });

    let (total, unknown) = env.total_seconds(playlist);
    let mut sub = format!(
        "{} track{}",
        playlist.items.len(),
        if playlist.items.len() == 1 { "" } else { "s" }
    );
    if total > 0.0 {
        sub.push_str(&format!(
            " · {}{}",
            format_clock((total * 1000.0) as u64),
            if unknown > 0 { "+" } else { "" }
        ));
    }
    ui.add_space(Metrics::XS);
    ui.label(micro_label(sub));
    ui.add_space(Metrics::MD);

    // Playback + mode.
    ui.horizontal(|ui| {
        let play = playlist_play_action(&env.live, &id_str);
        let (label, glyph_) = match play {
            PlayAction::Pause => ("Pause", None),
            PlayAction::Resume => ("Resume", Some(glyph::PLAY)),
            _ => ("Play", Some(glyph::PLAY)),
        };
        let mut button = ConsoleButton::primary(label)
            .small()
            .width(92.0)
            .enabled(!playlist.items.is_empty())
            .id_salt("pl_play");
        if let Some(g) = glyph_ {
            button = button.glyph(g);
        }
        if button.show(ui).clicked() {
            actions.push(play_action(&play));
        }
        if ConsoleButton::ghost("Stop")
            .glyph(glyph::STOP)
            .small()
            .width(78.0)
            .enabled(loaded && is_active(&env.live))
            .id_salt("pl_stop")
            .show(ui)
            .clicked()
        {
            actions.push(MusicAction::Command(PlaylistCommand::Stop));
        }
        if ConsoleButton::new("Shuffle")
            .small()
            .width(84.0)
            .kind(if playlist.shuffle {
                BtnKind::Armed
            } else {
                BtnKind::Ghost
            })
            .id_salt("pl_shuffle")
            .show(ui)
            .clicked()
        {
            actions.push(MusicAction::SetMode {
                playlist: id,
                shuffle: !playlist.shuffle,
                repeat: playlist.repeat,
            });
        }
        if ConsoleButton::new(format!("Repeat {}", repeat_label(playlist.repeat)))
            .small()
            .width(104.0)
            .kind(if playlist.repeat == RepeatMode::Off {
                BtnKind::Ghost
            } else {
                BtnKind::Armed
            })
            .id_salt("pl_repeat")
            .show(ui)
            .clicked()
        {
            actions.push(MusicAction::SetMode {
                playlist: id,
                shuffle: playlist.shuffle,
                repeat: next_repeat(playlist.repeat),
            });
        }
    });
    ui.add_space(Metrics::SM);
    ui.horizontal(|ui| {
        ui.label(micro_label("Crossfade"));
        let mut secs = playlist.crossfade_ms as f32 / 1000.0;
        let resp = ui.add(
            egui::DragValue::new(&mut secs)
                .range(0.0..=MAX_CROSSFADE_MS as f64 / 1000.0)
                .speed(0.1)
                .fixed_decimals(1)
                .suffix(" s"),
        );
        if resp.changed() {
            actions.push(MusicAction::SetCrossfade(id, crossfade_ms_from_secs(secs)));
        }
        ui.label(micro_label(format!(
            "{} between tracks",
            crossfade_label(playlist.crossfade_ms)
        )));
    });
    ui.add_space(Metrics::MD);
    hairline(ui);
    ui.add_space(Metrics::MD);

    // The tracks.
    ui.label(micro_label("Tracks · drag the handle or use the arrows"));
    ui.add_space(Metrics::SM);
    draw_tracks(ui, env, st, playlist, tracks_h, actions);
    ui.add_space(Metrics::LG);

    // The picker.
    ui.horizontal(|ui| {
        ui.label(micro_label("Add sounds"));
        ui.add(
            egui::TextEdit::singleline(&mut st.filter)
                .hint_text("Filter by name or tag")
                .desired_width(ui.available_width())
                .font(ui_font(12.0))
                .margin(egui::Margin::symmetric(8.0, 4.0)),
        );
    });
    ui.add_space(Metrics::SM);
    egui::ScrollArea::vertical()
        .id_salt("music_picker")
        .max_height(picker_h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let matches = filter_sounds(env.library.sounds(), &st.filter, 300);
            for sound in &matches {
                ui.horizontal(|ui| {
                    if tool_button(
                        ui,
                        Ico::Glyph(glyph::PLUS),
                        22.0,
                        BtnKind::Ghost,
                        true,
                        "Add to the end of the playlist",
                        "pl_add",
                    )
                    .clicked()
                    {
                        actions.push(MusicAction::AddTrack(id, sound.id));
                    }
                    ui.label(
                        RichText::new(elide(&sound_label(sound), 48))
                            .font(ui_font(12.0))
                            .color(Palette::ZINC_300),
                    );
                    if let Some(secs) = env.durations.get(&sound.id) {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(telemetry(
                                format_clock((secs * 1000.0) as u64),
                                10.0,
                                Palette::ZINC_500,
                            ));
                        });
                    }
                });
            }
            if matches.is_empty() {
                if st.filter.trim().is_empty() {
                    panel_kit::empty_state(
                        ui,
                        "No sounds in the library",
                        "Add sound files to the library first, then pick them for the playlist here.",
                    );
                } else {
                    panel_kit::empty_state(
                        ui,
                        "No sound matches",
                        "Try a shorter name, or clear the filter to see every sound.",
                    );
                }
            }
        });
}

fn draw_tracks(
    ui: &mut Ui,
    env: &MusicEnv,
    st: &mut MusicUi,
    playlist: &Playlist,
    height: f32,
    actions: &mut Vec<MusicAction>,
) {
    let id = playlist.id;
    let playing_index = if is_loaded(&env.live, &id.to_string()) && is_active(&env.live) {
        env.live.track_index
    } else {
        None
    };
    let mut centers: Vec<f32> = Vec::with_capacity(playlist.items.len());
    let mut bounds: (f32, f32) = (f32::MAX, f32::MIN);
    let mut stopped_drag = false;

    egui::ScrollArea::vertical()
        .id_salt("music_tracks")
        .max_height(height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            if playlist.items.is_empty() {
                panel_kit::empty_state(
                    ui,
                    "This playlist is empty",
                    "Add sounds from the list below with the + buttons.",
                );
            }
            for (index, item) in playlist.items.iter().enumerate() {
                let current = playing_index == Some(index);
                let frame = egui::Frame::none()
                    .fill(if current {
                        alpha(Palette::ROSE_600, 36)
                    } else {
                        Palette::ROW
                    })
                    .rounding(Rounding::same(Metrics::R_SM))
                    .inner_margin(egui::Margin::symmetric(6.0, 3.0));
                let shown = frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (hrect, handle) =
                            ui.allocate_exact_size(vec2(18.0, 24.0), Sense::drag());
                        let handle = handle.on_hover_cursor(egui::CursorIcon::Grab);
                        paint_ico(
                            ui.painter(),
                            Ico::Grip,
                            hrect.center(),
                            16.0,
                            if handle.hovered() || handle.dragged() {
                                Palette::WHITE
                            } else {
                                Palette::ZINC_500
                            },
                        );
                        if handle.drag_started() {
                            st.drag_track = Some(index);
                        }
                        if handle.dragged() {
                            if let Some(pos) = handle.interact_pointer_pos() {
                                st.drag_y = pos.y;
                            }
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        }
                        if handle.drag_stopped() {
                            if let Some(pos) = handle.interact_pointer_pos() {
                                st.drag_y = pos.y;
                            }
                            stopped_drag = true;
                        }
                        ui.label(telemetry(
                            format!("{:02}", index + 1),
                            10.0,
                            if current {
                                Palette::ROSE_400
                            } else {
                                Palette::ZINC_500
                            },
                        ));
                        let missing = env.library.sound_by_id(*item).is_none();
                        ui.label(
                            RichText::new(elide(&sound_name(env.library, *item), 34))
                                .font(ui_font(12.0))
                                .color(if missing {
                                    Palette::ROSE_400
                                } else if current {
                                    Palette::WHITE
                                } else {
                                    Palette::ZINC_300
                                }),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if tool_button(
                                ui,
                                Ico::Glyph(glyph::CROSS),
                                22.0,
                                BtnKind::Ghost,
                                true,
                                "Remove from the playlist",
                                "tr_remove",
                            )
                            .clicked()
                            {
                                actions.push(MusicAction::RemoveTrack(id, index));
                            }
                            if tool_button(
                                ui,
                                Ico::Down,
                                22.0,
                                BtnKind::Ghost,
                                index + 1 < playlist.items.len(),
                                "Move down",
                                "tr_down",
                            )
                            .clicked()
                            {
                                actions.push(MusicAction::MoveTrack {
                                    playlist: id,
                                    from: index,
                                    slot: index + 2,
                                });
                            }
                            if tool_button(
                                ui,
                                Ico::Up,
                                22.0,
                                BtnKind::Ghost,
                                index > 0,
                                "Move up",
                                "tr_up",
                            )
                            .clicked()
                            {
                                actions.push(MusicAction::MoveTrack {
                                    playlist: id,
                                    from: index,
                                    slot: index - 1,
                                });
                            }
                            if let Some(secs) = env.durations.get(item) {
                                ui.label(telemetry(
                                    format_clock((secs * 1000.0) as u64),
                                    10.0,
                                    Palette::ZINC_500,
                                ));
                            }
                        });
                    });
                });
                let rect = shown.response.rect;
                centers.push(rect.center().y);
                bounds = (bounds.0.min(rect.top()), bounds.1.max(rect.bottom()));
            }

            // The drop target is known once every row was laid out.
            if let Some(from) = st.drag_track {
                let slot = drop_slot(&centers, st.drag_y);
                let rows = centers.len();
                if rows > 0 && !stopped_drag {
                    // Line between the rows the track would land on.
                    let y = match slot {
                        0 => bounds.0 - 1.0,
                        s if s >= rows => bounds.1 + 1.0,
                        s => (centers[s - 1] + centers[s]) / 2.0,
                    };
                    let x = ui.max_rect().x_range();
                    ui.painter()
                        .hline(x, y, Stroke::new(2.0_f32, Palette::ROSE_500));
                }
                if stopped_drag {
                    actions.push(MusicAction::MoveTrack {
                        playlist: id,
                        from,
                        slot,
                    });
                    st.drag_track = None;
                }
            }
        });
    if st.drag_track.is_some() && !ui.input(|i| i.pointer.any_down()) {
        // Released outside any handle response (e.g. the row vanished).
        st.drag_track = None;
    }
}

fn draw_scenes_tab(
    ui: &mut Ui,
    env: &MusicEnv,
    _st: &mut MusicUi,
    screen_h: f32,
    actions: &mut Vec<MusicAction>,
) {
    let (tracks_h, picker_h) = list_heights(screen_h);
    panel_kit::note(
        ui,
        "Each scene can play one playlist and run random emitters.",
    );
    ui.add_space(Metrics::MD);
    egui::ScrollArea::vertical()
        .id_salt("music_scenes")
        .max_height(tracks_h + picker_h + 80.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = Metrics::XS;
            if env.library.scenes().is_empty() {
                panel_kit::empty_state(
                    ui,
                    "No scenes yet",
                    "Create a scene in the Scenes deck first (set up a mix, then Save Mix As \
                     Scene). Then attach music and emitters to it here.",
                );
            }
            for scene in env.library.scenes() {
                egui::Frame::none()
                    .fill(Palette::ROW)
                    .rounding(Rounding::same(Metrics::R_SM))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(elide(&scene.name, 40))
                                        .font(ui_font(13.0))
                                        .color(Palette::WHITE),
                                );
                                let music = scene
                                    .playlist
                                    .and_then(|id| env.library.playlist(id))
                                    .map(|p| elide(&p.name, 24))
                                    .unwrap_or_else(|| "no music".to_string());
                                ui.label(row_subline(format!(
                                    "{music} · {} emitter{}",
                                    scene.emitters.len(),
                                    if scene.emitters.len() == 1 { "" } else { "s" }
                                )));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ConsoleButton::ghost("Emitters & music…")
                                    .small()
                                    .width(150.0)
                                    .id_salt(&format!("scene_em_{}", scene.id))
                                    .show(ui)
                                    .clicked()
                                {
                                    actions.push(MusicAction::OpenEmitters(scene.id));
                                }
                            });
                        });
                    });
            }
        });
}

// =====================================================================
// Emitters & music dialog
// =====================================================================

/// Draw the emitters dialog's contents for `scene`.
pub fn draw_emitters(
    ui: &mut Ui,
    env: &MusicEnv,
    st: &mut MusicUi,
    scene: SceneId,
    screen_h: f32,
) -> Vec<MusicAction> {
    let mut actions = Vec::new();
    let library = env.library;
    let Some(scene_cfg) = library.scene(scene) else {
        return actions;
    };
    let scene_running = env.active_scene.as_deref() == Some(scene.to_string().as_str());
    let (list_h, _) = list_heights(screen_h);

    dialog_header(ui, "Scene", "Emitters & music", &elide(&scene_cfg.name, 48));
    ui.add_space(Metrics::MD);

    let Some(draft) = st.draft.as_mut() else {
        return actions;
    };

    // Music layer.
    ui.horizontal(|ui| {
        ui.label(micro_label("Music playlist"));
        let selected = match draft.playlist.and_then(|id| library.playlist(id)) {
            Some(p) => elide(&p.name, 30),
            None => "None".to_string(),
        };
        egui::ComboBox::from_id_salt("scene_playlist")
            .selected_text(selected)
            .width(240.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut draft.playlist, None, "None");
                for playlist in library.playlists() {
                    ui.selectable_value(
                        &mut draft.playlist,
                        Some(playlist.id),
                        elide(&playlist.name, 40),
                    );
                }
            });
        ui.label(micro_label("Starts and stops with the scene"));
    });
    ui.add_space(Metrics::LG);
    hairline(ui);
    ui.add_space(Metrics::LG);

    // Emitters.
    ui.horizontal(|ui| {
        ui.label(micro_label(
            "Emitters · random sounds while the scene plays",
        ));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ConsoleButton::ghost("Add emitter")
                .glyph(glyph::PLUS)
                .small()
                .width(130.0)
                .id_salt("em_add")
                .show(ui)
                .clicked()
            {
                draft.emitters.push(Emitter::new(Vec::new()));
            }
        });
    });
    ui.add_space(Metrics::SM);

    let mut remove: Option<usize> = None;
    egui::ScrollArea::vertical()
        .id_salt("emitters_list")
        .max_height(list_h + 120.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = Metrics::SM;
            if draft.emitters.is_empty()
                && panel_kit::empty_state_action(
                    ui,
                    "No emitters yet",
                    "An emitter plays a random sound from its list every so often while the \
                     scene runs: crows, drips, distant thunder.",
                    "Add emitter",
                    "em_empty_add",
                )
            {
                draft.emitters.push(Emitter::new(Vec::new()));
            }
            for (index, emitter) in draft.emitters.iter_mut().enumerate() {
                let snapshot = if scene_running {
                    env.emitters.iter().find(|s| s.index == index)
                } else {
                    None
                };
                if draw_emitter_card(ui, library, index, emitter, snapshot) {
                    remove = Some(index);
                }
            }
        });
    if let Some(index) = remove {
        draft.emitters.remove(index);
    }

    ui.add_space(Metrics::MD);
    hairline(ui);
    ui.add_space(Metrics::MD);

    let issue = draft.issue();
    let dirty = draft.is_dirty(library);
    ui.horizontal(|ui| {
        if ConsoleButton::primary("Save")
            .glyph(glyph::CHECK)
            .width(150.0)
            .height(Metrics::BTN_H)
            .enabled(issue.is_none() && dirty)
            .id_salt("em_save")
            .show(ui)
            .clicked()
        {
            actions.push(MusicAction::SaveEmitters);
        }
        ui.add_space(Metrics::SM);
        if ConsoleButton::ghost("Close")
            .glyph(glyph::CROSS)
            .width(120.0)
            .height(Metrics::BTN_H)
            .id_salt("em_close")
            .show(ui)
            .clicked()
        {
            actions.push(MusicAction::Close);
        }
        ui.add_space(Metrics::MD);
        if let Some(issue) = &issue {
            error_text(ui, issue);
        } else if let Some(message) = st.message.as_deref() {
            error_text(ui, message);
        } else if dirty {
            ui.label(micro_label("Unsaved changes"));
        }
    });
    actions
}

/// One emitter's card. Returns true when its remove button was pressed.
fn draw_emitter_card(
    ui: &mut Ui,
    library: &Library,
    index: usize,
    emitter: &mut Emitter,
    snapshot: Option<&EmitterSnapshot>,
) -> bool {
    let mut remove = false;
    egui::Frame::none()
        .fill(Palette::ROW)
        .rounding(Rounding::same(Metrics::R_SM))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.push_id(("emitter", index), |ui| {
                // Header with the live countdown.
                ui.horizontal(|ui| {
                    ui.label(micro_label_accent(format!("Emitter {}", index + 1)));
                    ui.label(telemetry(
                        countdown_label(snapshot).to_uppercase(),
                        10.0,
                        if snapshot.is_some_and(|s| s.enabled) {
                            Palette::ROSE_400
                        } else {
                            Palette::ZINC_500
                        },
                    ));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if tool_button(
                            ui,
                            Ico::Glyph(glyph::TRASH),
                            24.0,
                            BtnKind::Danger,
                            true,
                            "Remove the emitter",
                            "em_remove",
                        )
                        .clicked()
                        {
                            remove = true;
                        }
                    });
                });
                ui.add_space(Metrics::SM);

                // Sounds: removable chips and an "add" menu.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(Metrics::SM, Metrics::SM);
                    let mut drop_sound: Option<usize> = None;
                    for (i, sound) in emitter.sounds.iter().enumerate() {
                        let missing = library.sound_by_id(*sound).is_none();
                        let text = elide(&sound_name(library, *sound), 24);
                        egui::Frame::none()
                            .fill(Palette::WELL)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER))
                            .rounding(Rounding::same(Metrics::R_SM))
                            .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(text).font(ui_font(11.0)).color(
                                        if missing {
                                            Palette::ROSE_400
                                        } else {
                                            Palette::ZINC_300
                                        },
                                    ));
                                    if tool_button(
                                        ui,
                                        Ico::Glyph(glyph::CROSS),
                                        16.0,
                                        BtnKind::Ghost,
                                        true,
                                        "Remove the sound from this emitter",
                                        "em_chip_x",
                                    )
                                    .clicked()
                                    {
                                        drop_sound = Some(i);
                                    }
                                });
                            });
                    }
                    if let Some(i) = drop_sound {
                        emitter.sounds.remove(i);
                    }
                    egui::ComboBox::from_id_salt("em_add_sound")
                        .selected_text("Add sound…")
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for sound in library.sounds() {
                                if emitter.sounds.contains(&sound.id) {
                                    continue;
                                }
                                if ui
                                    .selectable_label(false, elide(&sound_label(sound), 44))
                                    .clicked()
                                {
                                    emitter.sounds.push(sound.id);
                                    ui.close_menu();
                                }
                            }
                        });
                });
                ui.add_space(Metrics::MD);

                // Numbers.
                egui::Grid::new("em_grid")
                    .num_columns(3)
                    .spacing([Metrics::MD, Metrics::SM])
                    .show(ui, |ui| {
                        ui.label(micro_label("Every"));
                        ui.horizontal(|ui| {
                            let mut min = emitter.min_interval_ms as f32 / 1000.0;
                            if ui.add(interval_drag(&mut min)).changed() {
                                let (lo, hi) = set_interval(
                                    emitter.min_interval_ms,
                                    emitter.max_interval_ms,
                                    IntervalEdit::Min((min * 1000.0).round() as u32),
                                );
                                emitter.min_interval_ms = lo;
                                emitter.max_interval_ms = hi;
                            }
                            ui.label(micro_label("to"));
                            let mut max = emitter.max_interval_ms as f32 / 1000.0;
                            if ui.add(interval_drag(&mut max)).changed() {
                                let (lo, hi) = set_interval(
                                    emitter.min_interval_ms,
                                    emitter.max_interval_ms,
                                    IntervalEdit::Max((max * 1000.0).round() as u32),
                                );
                                emitter.min_interval_ms = lo;
                                emitter.max_interval_ms = hi;
                            }
                        });
                        ui.label(telemetry(
                            interval_label(emitter.min_interval_ms, emitter.max_interval_ms),
                            10.0,
                            Palette::ZINC_500,
                        ));
                        ui.end_row();

                        slider_row(ui, "Volume", &mut emitter.volume, "em_vol");
                        slider_row(ui, "Volume jitter", &mut emitter.volume_jitter, "em_vj");
                        slider_row(ui, "Pan jitter", &mut emitter.pan_jitter, "em_pj");

                        ui.label(micro_label("Max overlap"));
                        let mut overlap = emitter.max_overlap;
                        if ui
                            .add(
                                egui::DragValue::new(&mut overlap)
                                    .range(1..=MAX_EMITTER_OVERLAP)
                                    .speed(0.1),
                            )
                            .changed()
                        {
                            emitter.max_overlap = overlap.clamp(1, MAX_EMITTER_OVERLAP);
                        }
                        ui.label(micro_label("at once"));
                        ui.end_row();
                    });
            });
        });
    remove
}

fn interval_drag(secs: &mut f32) -> egui::DragValue<'_> {
    egui::DragValue::new(secs)
        .range(MIN_EMITTER_INTERVAL_MS as f64 / 1000.0..=MAX_EMITTER_INTERVAL_MS as f64 / 1000.0)
        .speed(0.5)
        .max_decimals(1)
        .suffix(" s")
}

fn slider_row(ui: &mut Ui, label: &str, value: &mut f32, salt: &str) {
    ui.label(micro_label(label));
    level_slider(ui, value, 220.0, salt);
    ui.label(telemetry(
        format!("{:.0}%", *value * 100.0),
        10.0,
        Palette::ZINC_400,
    ));
    ui.end_row();
}

// =====================================================================
// App glue
// =====================================================================

impl SoundSharedApp {
    /// Open the playlist manager.
    pub fn open_music(&mut self) {
        self.music.message = None;
        *self.dialog_mut() = Dialog::Music;
    }

    /// Open "Emitters & music" for a scene. This is the hook for the scene
    /// card / context menu: `state.open_edit_emitters(scene.id)`.
    pub fn open_edit_emitters(&mut self, scene: SceneId) {
        self.music.draft = None;
        self.music.message = None;
        *self.dialog_mut() = Dialog::EditEmitters(scene);
    }

    /// Everything the manager and the emitters dialog read.
    fn music_env<'a>(&'a self, st: &MusicUi) -> MusicEnv<'a> {
        let by_path: HashMap<std::path::PathBuf, f32> = self
            .sounds()
            .into_iter()
            .filter(|entry| entry.duration > 0.0)
            .map(|entry| (entry.path, entry.duration))
            .collect();
        let durations = self
            .library
            .sounds()
            .iter()
            .filter_map(|sound| by_path.get(&sound.path).map(|secs| (sound.id, *secs)))
            .collect();
        MusicEnv::new(&self.library, st, durations)
    }

    /// Library changed: persist it and let the Director see the edit.
    fn music_library_changed(&mut self) {
        self.library_dirty = true;
        sync_director(&mut self.director, &self.library, &self.sounds);
    }

    /// Run one command on the music layer. A failure becomes the message
    /// the bar and the manager show.
    fn run_playlist_command(&mut self, command: PlaylistCommand) {
        sync_director(&mut self.director, &self.library, &self.sounds);
        let guard = self.audio_engine.lock().unwrap();
        let result = match guard.as_ref() {
            Some(engine) => {
                let result = self.director.playlist_command(engine, command);
                mirror_playing(&self.sounds, engine);
                result.map_err(|e| e.to_string())
            }
            None => Err("Audio engine is not running".to_string()),
        };
        drop(guard);
        self.music.live = self.director.playlist_state();
        self.music.message = result.err();
    }

    /// Apply one typed music action.
    pub(crate) fn apply_music_action(&mut self, action: MusicAction) {
        match action {
            MusicAction::Command(command) => self.run_playlist_command(command),
            MusicAction::OpenManager => self.open_music(),
            MusicAction::Close => self.close_dialog(),
            MusicAction::OpenEmitters(scene) => self.open_edit_emitters(scene),
            MusicAction::CreatePlaylist => match create_playlist(&mut self.library) {
                Some(id) => {
                    let name = self
                        .library
                        .playlist(id)
                        .map(|p| p.name.clone())
                        .unwrap_or_default();
                    self.music.selected = Some(id);
                    self.music.rename = Some((id, name));
                    self.music.confirm_delete = None;
                    self.music.message = None;
                    self.music_library_changed();
                }
                None => self.music.message = Some("Could not create a playlist".to_string()),
            },
            MusicAction::RenamePlaylist(id, name) => {
                if self.library.rename_playlist(id, &name) {
                    self.music.message = None;
                    self.music_library_changed();
                } else {
                    self.music.message =
                        Some("Name is empty or another playlist already has it".to_string());
                }
            }
            MusicAction::DeletePlaylist(id) => {
                if is_loaded(&self.music.live, &id.to_string()) && is_active(&self.music.live) {
                    self.run_playlist_command(PlaylistCommand::Stop);
                }
                if self.library.remove_playlist(id) {
                    if self.music.selected == Some(id) {
                        self.music.selected = None;
                    }
                    self.music_library_changed();
                }
            }
            MusicAction::AddTrack(id, sound) => {
                if self.library.add_playlist_item(id, sound, None) {
                    self.music_library_changed();
                }
            }
            MusicAction::RemoveTrack(id, position) => {
                if self.library.remove_playlist_item(id, position) {
                    self.music_library_changed();
                }
            }
            MusicAction::MoveTrack {
                playlist,
                from,
                slot,
            } => {
                if move_track(&mut self.library, playlist, from, slot) {
                    self.music_library_changed();
                }
            }
            MusicAction::SetMode {
                playlist,
                shuffle,
                repeat,
            } => {
                let loaded = is_loaded(&self.music.live, &playlist.to_string());
                if self.library.set_playlist_mode(playlist, shuffle, repeat) {
                    self.music_library_changed();
                }
                if loaded && is_active(&self.music.live) {
                    self.run_playlist_command(PlaylistCommand::SetShuffle(shuffle));
                    if self.music.message.is_none() {
                        self.run_playlist_command(PlaylistCommand::SetRepeat(repeat));
                    }
                }
            }
            MusicAction::SetCrossfade(id, ms) => {
                if self.library.set_playlist_crossfade(id, ms) {
                    self.music_library_changed();
                }
            }
            MusicAction::SaveEmitters => {
                let Some(draft) = self.music.draft.clone() else {
                    return;
                };
                if draft.commit(&mut self.library) {
                    self.music.message = None;
                    self.music_library_changed();
                    self.close_dialog();
                } else {
                    self.music.message = Some(
                        draft
                            .issue()
                            .unwrap_or_else(|| "A sound or playlist no longer exists".into()),
                    );
                }
            }
        }
    }

    fn run_music_actions(&mut self, actions: Vec<MusicAction>) {
        for action in actions {
            self.apply_music_action(action);
        }
    }
}

/// The transport bar. Called every frame from the bottom panel.
pub fn music_bar(ui: &mut Ui, state: &mut SoundSharedApp) {
    let mut st = std::mem::take(&mut state.music);
    let env = MusicEnv::new(state.library(), &st, HashMap::new());
    let actions = draw_bar(ui, &env, &mut st);
    state.music = st;
    state.run_music_actions(actions);
}

/// The playlist manager modal.
pub fn show_music_dialog(ctx: &Context, state: &mut SoundSharedApp) {
    let mut st = std::mem::take(&mut state.music);
    let env = state.music_env(&st);
    let screen_h = ctx.screen_rect().height();
    let mut actions = Vec::new();
    window(ctx, "music_manager", MANAGER_W, |ui| {
        actions = draw_manager(ui, &env, &mut st, screen_h);
    });
    drop(env);
    state.music = st;
    state.run_music_actions(actions);
}

/// The "Emitters & music" modal of a scene.
pub fn show_emitters_dialog(ctx: &Context, state: &mut SoundSharedApp, scene: SceneId) {
    let mut st = std::mem::take(&mut state.music);
    if state.library().scene(scene).is_none() {
        state.music = st;
        state.close_dialog();
        return;
    }
    if st.draft.as_ref().is_none_or(|d| d.scene != scene) {
        st.draft = EmitterDraft::from_library(state.library(), scene);
    }
    let env = state.music_env(&st);
    let screen_h = ctx.screen_rect().height();
    let mut actions = Vec::new();
    window(ctx, "emitters_dialog", EMITTERS_W, |ui| {
        actions = draw_emitters(ui, &env, &mut st, scene, screen_h);
    });
    drop(env);
    state.music = st;
    state.run_music_actions(actions);
}

#[cfg(test)]
#[path = "music_tests.rs"]
mod tests;
