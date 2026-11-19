//! Scenes panel: the scene deck above the sound list, the scene editor, the
//! "save the current mix" flow, stinger pads and the shared transition control.
//!
//! ```text
//! SCENES  3 scenes · Active: Tavern               [SAVE MIX AS SCENE] [NEW] [HIDE]
//! [STOP SCENE] [STOP ALL]   TRANSITION [AUTO] ▬▬▬▬▬●▬▬▬  SCENE
//! ┌ ▌Tavern ────────┐ ┌ ▌Forest ────────┐ ┌ ▌Storm ─────────┐
//! │ ACTIVE · 62%    │ │ 4 SOUNDS · 2 s  │ │ 3 SOUNDS · 2 s  │
//! └▬▬▬▬▬▬▬──────────┘ └─────────────────┘ └─────────────────┘
//! STINGERS  [▶ Door slam] [▶ Thunder]
//! ```
//!
//! # Structure
//!
//! * The first half of the file is **pure presentation logic** (form state,
//!   validation, `Scene` <-> form conversion, card view models, transition
//!   progress). It draws nothing, never touches the audio thread and is covered
//!   by the unit tests at the bottom.
//! * The second half paints it with egui and routes every click into a typed
//!   [`AudioCommand`]. Replies are **never awaited**: each command carries a
//!   one-slot reply channel that [`SceneUi::poll`] drains once per frame, so the
//!   render thread never blocks (the audio commands are executed by the GUI
//!   thread itself, one frame later, so waiting here would deadlock anyway).

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use egui::{
    Align, Color32, Context, CursorIcon, Layout, Rect, Response, RichText, Rounding, Sense, Stroke,
    Ui, Vec2, WidgetInfo, WidgetType,
};

use crate::audio::SoundId as EngineSoundId;
use crate::config::{FadeCurve, SceneConfig, SceneStart};
use crate::gui::app::{Dialog, SoundEntry, SoundSharedApp};
use crate::gui::dialog::{header, modal_sized};
use crate::gui::groups::{
    Library, LibrarySound, Scene, SceneId, SceneItem, SoundId as LibSoundId, MAX_NAME_LEN,
};
use crate::gui::settings_extras::kit as settings_kit;
use crate::gui::settings_extras::kit::error_line;
use crate::gui::theme::{
    alpha, hairline, micro_label, micro_label_accent, micro_label_dim, ui_font, Metrics, Palette,
};
use crate::gui::types::AudioCommand;
use crate::gui::widgets::{
    elide, glyph, glyph_font, level_slider, paint_icon, BtnKind, ConsoleButton, Glyph, IconButton,
};

// ============================================================================
// Pure logic: constants
// ============================================================================

/// Longest fade the editor accepts (one minute).
pub const MAX_FADE_MS: u32 = 60_000;
/// Longest start delay the editor accepts (ten minutes).
pub const MAX_DELAY_MS: u32 = 600_000;
/// Right end of the shared transition slider.
pub const TRANSITION_MAX_MS: u32 = 10_000;
/// Granularity of the shared transition slider.
pub const TRANSITION_STEP_MS: u32 = 100;
/// Starting value of the shared transition slider.
pub const DEFAULT_TRANSITION_MS: u32 = 2_000;
/// "Stop all" fade when neither the slider nor an active scene says otherwise.
pub const DEFAULT_STOP_ALL_FADE_MS: u32 = 2_000;

/// How long a confirmation stays in the deck.
const NOTICE_INFO_HOLD: Duration = Duration::from_millis(3_500);
/// How long an error stays in the deck.
const NOTICE_ERROR_HOLD: Duration = Duration::from_millis(7_000);
/// Frame interval while a fade-in bar is animating (about 30 fps).
const ANIMATION_FRAME: Duration = Duration::from_millis(33);

/// Surface the scene cards are painted on, for contrast calculations.
const CARD_BG: [u8; 3] = [0x18, 0x18, 0x1B];
/// Accent of a scene that has no colour of its own (the console's rose).
const DEFAULT_ACCENT: [u8; 3] = [0xF4, 0x3F, 0x5E];

/// Named swatches offered by the editor.
pub const COLOR_PRESETS: [(&str, &str); 8] = [
    ("Crimson", "#a04a4a"),
    ("Amber", "#b7791f"),
    ("Forest", "#2f855a"),
    ("Teal", "#2c7a7b"),
    ("Azure", "#2b6cb0"),
    ("Violet", "#6b46c1"),
    ("Magenta", "#b83280"),
    ("Slate", "#718096"),
];

// ============================================================================
// Pure logic: numbers and colours
// ============================================================================

/// Parse a time typed by the operator into milliseconds.
///
/// Accepted: `2000`, `2000 ms`, `2.5 s`, `2.5s`. Everything else (empty,
/// negative, not a number, above `max`) is an error with a ready-to-show text.
pub fn parse_millis(text: &str, max: u32) -> Result<u32, String> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return Err("Enter a time, e.g. 2000 or 2.5 s".to_string());
    }
    let bad = || "Use milliseconds or seconds, e.g. 2000 or 2.5 s".to_string();
    let ms: f64 = if let Some(number) = text.strip_suffix("ms") {
        f64::from(number.trim().parse::<u32>().map_err(|_| bad())?)
    } else if let Some(number) = text.strip_suffix('s') {
        let seconds: f64 = number.trim().parse().map_err(|_| bad())?;
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(bad());
        }
        (seconds * 1000.0).round()
    } else {
        f64::from(text.parse::<u32>().map_err(|_| bad())?)
    };
    if ms > f64::from(max) {
        return Err(format!("At most {} s", max / 1000));
    }
    Ok(ms as u32)
}

/// Text-field form of a millisecond value.
pub fn format_millis(ms: u32) -> String {
    ms.to_string()
}

/// `2.0 s` — a time for labels.
pub fn format_seconds(ms: u32) -> String {
    format!("{:.1} s", f64::from(ms) / 1000.0)
}

/// `#rgb` / `#rrggbb` to RGB bytes.
pub fn parse_hex_rgb(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().strip_prefix('#')?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // Every byte is an ASCII hex digit, so byte slicing is character slicing.
    let nibble = |index: usize| u8::from_str_radix(&digits[index..=index], 16).ok();
    let byte = |index: usize| u8::from_str_radix(&digits[index..index + 2], 16).ok();
    match digits.len() {
        3 => Some([nibble(0)? * 17, nibble(1)? * 17, nibble(2)? * 17]),
        6 => Some([byte(0)?, byte(2)?, byte(4)?]),
        _ => None,
    }
}

/// Colour field to the stored value: empty is "no colour" (`None`), a missing
/// `#` is added, letters are lower-cased. Anything that is not a colour is an
/// error text.
pub fn normalize_color_input(text: &str) -> Result<Option<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let with_hash = if text.starts_with('#') {
        text.to_string()
    } else {
        format!("#{text}")
    };
    if parse_hex_rgb(&with_hash).is_none() {
        return Err("Use a hex colour such as #a04a4a".to_string());
    }
    Ok(Some(with_hash.to_lowercase()))
}

fn linear(channel: u8) -> f32 {
    let c = f32::from(channel) / 255.0;
    if c <= 0.039_28 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance.
pub fn relative_luminance(rgb: [u8; 3]) -> f32 {
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

/// WCAG contrast ratio between two colours, `1.0..=21.0`.
pub fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Lighten `rgb` until it reaches 3:1 against `background` (the WCAG minimum
/// for graphical objects), so a scene colour chosen dark never disappears into
/// the card.
pub fn visible_accent(rgb: [u8; 3], background: [u8; 3]) -> [u8; 3] {
    let mut color = rgb;
    for _ in 0..40 {
        if contrast_ratio(color, background) >= 3.0 {
            break;
        }
        for channel in &mut color {
            *channel = (f32::from(*channel) + (255.0 - f32::from(*channel)) * 0.1).round() as u8;
        }
    }
    color
}

/// The colour a scene is drawn with: its own (made visible) or the default.
pub fn scene_accent(color: Option<&str>) -> [u8; 3] {
    let base = color.and_then(parse_hex_rgb).unwrap_or(DEFAULT_ACCENT);
    visible_accent(base, CARD_BG)
}

fn to_color32(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

// ============================================================================
// Pure logic: names
// ============================================================================

/// Check a scene name the way the library will, with an error text for the
/// dialog. `except` is the scene being renamed (its own name is not a clash).
pub fn validate_scene_name(
    library: &Library,
    name: &str,
    except: Option<SceneId>,
) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Enter a scene name".to_string());
    }
    if trimmed.chars().count() > MAX_NAME_LEN {
        return Err(format!("Maximum {MAX_NAME_LEN} characters"));
    }
    let lowered = trimmed.to_lowercase();
    let taken = library
        .scenes()
        .iter()
        .any(|scene| Some(scene.id) != except && scene.name.to_lowercase() == lowered);
    if taken {
        return Err("A scene with this name already exists".to_string());
    }
    Ok(trimmed.to_string())
}

/// First free `Scene N` name.
pub fn suggest_scene_name(library: &Library) -> String {
    (library.scenes().len() + 1..)
        .map(|n| format!("Scene {n}"))
        .find(|candidate| validate_scene_name(library, candidate, None).is_ok())
        .unwrap_or_else(|| "Scene".to_string())
}

/// Name for a duplicate: `X copy`, then `X copy 2`, ... cut to fit the limit.
pub fn copy_name(library: &Library, original: &str) -> String {
    let mut n: u32 = 1;
    loop {
        let suffix = if n == 1 {
            " copy".to_string()
        } else {
            format!(" copy {n}")
        };
        let keep = MAX_NAME_LEN.saturating_sub(suffix.chars().count());
        let base: String = original.trim().chars().take(keep).collect();
        let candidate = format!("{}{}", base.trim_end(), suffix);
        if validate_scene_name(library, &candidate, None).is_ok() {
            return candidate;
        }
        n += 1;
    }
}

/// How a library sound is shown: its name, else its file name.
pub fn sound_label(sound: &LibrarySound) -> String {
    if !sound.name.trim().is_empty() {
        return sound.name.trim().to_string();
    }
    sound
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| sound.path.to_string_lossy().into_owned())
}

// ============================================================================
// Pure logic: the editor form
// ============================================================================

/// One sound row of the editor.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemForm {
    /// Library id of the sound.
    pub sound: LibSoundId,
    /// Display name (resolved when the form was built).
    pub name: String,
    /// Level inside the scene, `0.0..=1.0`.
    pub volume: f32,
    /// Repeat while the scene is active.
    pub looped: bool,
    /// `true` = started by hand (`SceneStart::Manual`), `false` = on entry.
    pub manual: bool,
    /// Start delay as typed (milliseconds or seconds, see [`parse_millis`]).
    pub delay: String,
    /// Pan inside the scene, `-1.0..=1.0`; `0.0` uses the sound's own pan.
    pub pan: f32,
}

impl ItemForm {
    /// A row for a library sound: its own level and repeat flag, starts on entry.
    pub fn for_sound(sound: &LibrarySound) -> Self {
        Self {
            sound: sound.id,
            name: sound_label(sound),
            volume: sound.volume.clamp(0.0, 1.0),
            looped: sound.looped,
            manual: false,
            delay: format_millis(0),
            pan: 0.0,
        }
    }

    /// The delay in ms. A manual item ignores its delay, so a bad value there
    /// is not an error (it is stored as 0).
    fn delay_ms(&self) -> Result<u32, String> {
        match parse_millis(&self.delay, MAX_DELAY_MS) {
            Ok(ms) => Ok(ms),
            Err(_) if self.manual => Ok(0),
            Err(message) => Err(message),
        }
    }
}

/// Per-field validation errors. All `None` / empty = the form can be saved.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FormErrors {
    pub name: Option<String>,
    pub color: Option<String>,
    pub fade_in: Option<String>,
    pub fade_out: Option<String>,
    /// One entry per item, aligned with [`SceneForm::items`].
    pub delays: Vec<Option<String>>,
    /// A sound of the scene is no longer in the library.
    pub sounds: Option<String>,
}

impl FormErrors {
    /// No field has an error.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.color.is_none()
            && self.fade_in.is_none()
            && self.fade_out.is_none()
            && self.sounds.is_none()
            && self.delays.iter().all(Option::is_none)
    }
}

/// The scene editor's working copy: text fields as typed, flags and levels.
///
/// Emitters and the music playlist are deliberately not part of it; applying
/// the form leaves them as they are.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneForm {
    /// The scene being edited, `None` for a new one.
    pub id: Option<SceneId>,
    pub name: String,
    /// Colour as typed (`#a04a4a`), empty for none.
    pub color: String,
    /// Fade-in as typed.
    pub fade_in: String,
    /// Fade-out as typed.
    pub fade_out: String,
    pub curve: FadeCurve,
    pub stop_others: bool,
    pub items: Vec<ItemForm>,
    /// Set by the first Save attempt: only then are errors shown (and kept
    /// up to date as the operator types).
    pub attempted: bool,
}

/// A form that passed validation, in the library's own types.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidScene {
    pub name: String,
    pub color: Option<String>,
    pub fade_in_ms: u32,
    pub fade_out_ms: u32,
    pub curve: FadeCurve,
    pub stop_others: bool,
    pub items: Vec<SceneItem>,
}

impl SceneForm {
    /// An empty form with the schema defaults and a free name.
    pub fn blank(library: &Library) -> Self {
        let defaults = SceneConfig::default();
        Self {
            id: None,
            name: suggest_scene_name(library),
            color: String::new(),
            fade_in: format_millis(defaults.fade_in_ms),
            fade_out: format_millis(defaults.fade_out_ms),
            curve: defaults.curve,
            stop_others: defaults.stop_others,
            items: Vec::new(),
            attempted: false,
        }
    }

    /// The form of an existing scene.
    pub fn from_scene(library: &Library, scene: &Scene) -> Self {
        let items = scene
            .items
            .iter()
            .map(|item| ItemForm {
                sound: item.sound,
                name: library
                    .sound_by_id(item.sound)
                    .map(sound_label)
                    .unwrap_or_else(|| "Missing sound".to_string()),
                volume: item.volume.clamp(0.0, 1.0),
                looped: item.looped,
                manual: item.start == SceneStart::Manual,
                delay: format_millis(item.delay_ms),
                pan: item.pan,
            })
            .collect();
        Self {
            id: Some(scene.id),
            name: scene.name.clone(),
            color: scene.color.clone().unwrap_or_default(),
            fade_in: format_millis(scene.fade_in_ms),
            fade_out: format_millis(scene.fade_out_ms),
            curve: scene.curve,
            stop_others: scene.stop_others,
            items,
            attempted: false,
        }
    }

    /// Library sounds that are not in the scene yet, as `(id, label)`.
    pub fn available_sounds(&self, library: &Library) -> Vec<(LibSoundId, String)> {
        library
            .sounds()
            .iter()
            .filter(|sound| !self.items.iter().any(|item| item.sound == sound.id))
            .map(|sound| (sound.id, sound_label(sound)))
            .collect()
    }

    /// Add a library sound as the last row. False when it is unknown or
    /// already in the form.
    pub fn add_sound(&mut self, library: &Library, sound: LibSoundId) -> bool {
        if self.items.iter().any(|item| item.sound == sound) {
            return false;
        }
        match library.sound_by_id(sound) {
            Some(sound) => {
                self.items.push(ItemForm::for_sound(sound));
                true
            }
            None => false,
        }
    }

    /// Remove a row. False when the index is out of range.
    pub fn remove_item(&mut self, index: usize) -> bool {
        if index < self.items.len() {
            self.items.remove(index);
            true
        } else {
            false
        }
    }

    /// Every field's current error.
    pub fn errors(&self, library: &Library) -> FormErrors {
        let missing = self
            .items
            .iter()
            .find(|item| library.sound_by_id(item.sound).is_none());
        FormErrors {
            name: validate_scene_name(library, &self.name, self.id).err(),
            color: normalize_color_input(&self.color).err(),
            fade_in: parse_millis(&self.fade_in, MAX_FADE_MS).err(),
            fade_out: parse_millis(&self.fade_out, MAX_FADE_MS).err(),
            delays: self
                .items
                .iter()
                .map(|item| item.delay_ms().err())
                .collect(),
            sounds: missing.map(|item| format!("“{}” is no longer in the library", item.name)),
        }
    }

    /// The form in the library's types, or the errors that block saving.
    pub fn validate(&self, library: &Library) -> Result<ValidScene, Box<FormErrors>> {
        let errors = self.errors(library);
        if !errors.is_empty() {
            return Err(Box::new(errors));
        }
        // `errors` is empty, so every parse below succeeds; the fallbacks only
        // keep this function total.
        let items = self
            .items
            .iter()
            .map(|item| SceneItem {
                sound: item.sound,
                volume: item.volume.clamp(0.0, 1.0),
                looped: item.looped,
                start: if item.manual {
                    SceneStart::Manual
                } else {
                    SceneStart::OnEnter
                },
                delay_ms: item.delay_ms().unwrap_or(0),
                pan: item.pan,
            })
            .collect();
        Ok(ValidScene {
            name: self.name.trim().to_string(),
            color: normalize_color_input(&self.color).unwrap_or(None),
            fade_in_ms: parse_millis(&self.fade_in, MAX_FADE_MS).unwrap_or(0),
            fade_out_ms: parse_millis(&self.fade_out, MAX_FADE_MS).unwrap_or(0),
            curve: self.curve,
            stop_others: self.stop_others,
            items,
        })
    }
}

/// Write a validated form into the library: creates the scene when `id` is
/// `None`, otherwise edits it. Emitters and the playlist are not touched.
/// Nothing is changed when an error is returned.
pub fn apply_scene(
    library: &mut Library,
    id: Option<SceneId>,
    scene: &ValidScene,
) -> Result<SceneId, String> {
    if let Some(unknown) = scene
        .items
        .iter()
        .find(|item| library.sound_by_id(item.sound).is_none())
    {
        return Err(format!("sound {} is not in the library", unknown.sound));
    }
    if let Some(id) = id {
        if library.scene(id).is_none() {
            return Err("the scene no longer exists".to_string());
        }
    }
    validate_scene_name(library, &scene.name, id)?;

    let id = match id {
        Some(id) => {
            if !library.rename_scene(id, &scene.name) {
                return Err("the scene name was rejected".to_string());
            }
            id
        }
        None => library
            .add_scene(&scene.name)
            .ok_or_else(|| "the scene name was rejected".to_string())?,
    };
    library.set_scene_color(id, scene.color.as_deref());
    library.set_scene_fades(id, scene.fade_in_ms, scene.fade_out_ms);
    library.set_scene_curve(id, scene.curve);
    library.set_scene_stop_others(id, scene.stop_others);

    let current: Vec<LibSoundId> = library
        .scene(id)
        .map(|s| s.items.iter().map(|item| item.sound).collect())
        .unwrap_or_default();
    for sound in current {
        if !scene.items.iter().any(|item| item.sound == sound) {
            library.remove_item(id, sound);
        }
    }
    for (position, item) in scene.items.iter().enumerate() {
        library.set_item(id, item.clone());
        library.reorder_item(id, item.sound, position);
    }
    Ok(id)
}

/// Copy a scene (items, emitters, playlist, fades, colour) under a fresh name,
/// right after the original. `None` when the scene is unknown.
pub fn duplicate_scene(library: &mut Library, id: SceneId) -> Option<SceneId> {
    let source = library.scene(id)?.clone();
    let position = library.scenes().iter().position(|scene| scene.id == id)?;
    let copy = library.add_scene(&copy_name(library, &source.name))?;
    library.set_scene_color(copy, source.color.as_deref());
    library.set_scene_fades(copy, source.fade_in_ms, source.fade_out_ms);
    library.set_scene_curve(copy, source.curve);
    library.set_scene_stop_others(copy, source.stop_others);
    for item in source.items {
        library.set_item(copy, item);
    }
    library.set_emitters(copy, source.emitters);
    library.set_scene_playlist(copy, source.playlist);
    library.reorder_scene(copy, position + 1);
    Some(copy)
}

/// What [`add_sound_to_scene`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddToScene {
    Added,
    AlreadyPresent,
    UnknownScene,
    UnknownSound,
}

/// Put a sound into a scene with its own level and repeat flag (the context
/// menu of a sound row). An existing item is left alone.
pub fn add_sound_to_scene(library: &mut Library, scene: SceneId, sound: LibSoundId) -> AddToScene {
    let Some(existing) = library.scene(scene) else {
        return AddToScene::UnknownScene;
    };
    if existing.items.iter().any(|item| item.sound == sound) {
        return AddToScene::AlreadyPresent;
    }
    let Some(entry) = library.sound_by_id(sound) else {
        return AddToScene::UnknownSound;
    };
    let item = SceneItem {
        volume: entry.volume.clamp(0.0, 1.0),
        looped: entry.looped,
        ..SceneItem::new(sound)
    };
    if library.set_item(scene, item) {
        AddToScene::Added
    } else {
        AddToScene::UnknownSound
    }
}

// ============================================================================
// Pure logic: view models
// ============================================================================

/// Everything a scene button shows.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneCard {
    pub id: SceneId,
    pub name: String,
    /// Colour made visible on the card, see [`visible_accent`].
    pub accent: [u8; 3],
    pub sounds: usize,
    pub fade_in_ms: u32,
    pub active: bool,
    /// What would silently go wrong when this scene is activated (missing
    /// sound files, an empty scene, ...), one short line each. Filled from
    /// the [`WarningCache`]; empty when all is well.
    pub warnings: Vec<String>,
}

/// One card per scene, in library order. `active` is the Director's active
/// scene id (a UUID string).
pub fn scene_cards(library: &Library, active: Option<&str>) -> Vec<SceneCard> {
    library
        .scenes()
        .iter()
        .map(|scene| SceneCard {
            id: scene.id,
            name: scene.name.clone(),
            accent: scene_accent(scene.color.as_deref()),
            sounds: scene.items.len(),
            fade_in_ms: scene.fade_in_ms,
            active: active == Some(scene.id.to_string().as_str()),
            warnings: Vec::new(),
        })
        .collect()
}

// ============================================================================
// Pure logic: scene warnings
// ============================================================================

/// How often the scene warnings are looked up again (it touches the disk).
pub const WARNING_REFRESH: Duration = Duration::from_secs(3);
/// Lines a warning tooltip lists before it says "and N more".
const WARNING_TOOLTIP_LINES: usize = 6;

/// `scene "Tavern" sound X is missing its file ...` -> `Sound X is missing its
/// file ...`: the card already names the scene.
pub fn short_warning(line: &str, scene_name: &str) -> String {
    let owner = format!("scene \u{201c}{scene_name}\u{201d} ");
    let rest = line.strip_prefix(&owner).unwrap_or(line);
    let mut chars = rest.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The tooltip of a card's "!" badge: one bullet per problem.
pub fn warning_tooltip(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines.iter().take(WARNING_TOOLTIP_LINES) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("\u{2022} ");
        out.push_str(line);
    }
    if lines.len() > WARNING_TOOLTIP_LINES {
        out.push_str(&format!(
            "\nand {} more",
            lines.len() - WARNING_TOOLTIP_LINES
        ));
    }
    out
}

/// Short warning lines of every scene that has any, keyed by scene id.
/// `exists` answers "is this file on disk"; it is the slow part, so callers
/// run this off the render thread (see [`WarningCache`]).
pub fn scene_warning_lines(
    library: &Library,
    exists: impl Fn(&Path) -> bool,
) -> HashMap<SceneId, Vec<String>> {
    let mut by_scene = HashMap::new();
    for scene in library.scenes() {
        let lines: Vec<String> = library
            .scene_warnings_with(scene.id, &exists)
            .iter()
            .map(|warning| short_warning(&library.describe_warning(warning), &scene.name))
            .collect();
        if !lines.is_empty() {
            by_scene.insert(scene.id, lines);
        }
    }
    by_scene
}

/// Scene warnings kept for drawing. The lookup stats every referenced file,
/// so it runs on a short-lived worker thread every [`WARNING_REFRESH`] (or
/// right after a library change) and the render thread only reads the result.
#[derive(Default)]
pub struct WarningCache {
    lines: HashMap<SceneId, Vec<String>>,
    refreshed: Option<Instant>,
    dirty: bool,
    job: Option<Receiver<HashMap<SceneId, Vec<String>>>>,
}

impl WarningCache {
    /// The library changed: look again at the next frame.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Warning lines of a scene (empty when it has none).
    pub fn lines(&self, scene: SceneId) -> &[String] {
        self.lines.get(&scene).map_or(&[], Vec::as_slice)
    }

    /// Whether a new lookup should start now.
    pub fn due(&self, now: Instant) -> bool {
        self.job.is_none()
            && (self.dirty
                || self
                    .refreshed
                    .is_none_or(|at| now.saturating_duration_since(at) >= WARNING_REFRESH))
    }

    /// Take a finished lookup, and start the next one when it is due. Never
    /// blocks. `repaint` is called from the worker once its result is ready.
    pub fn pump(
        &mut self,
        library: &Library,
        now: Instant,
        repaint: impl FnOnce() + Send + 'static,
    ) {
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(lines) => {
                    self.lines = lines;
                    self.job = None;
                    self.refreshed = Some(now);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.job = None;
                    self.refreshed = Some(now);
                }
            }
        }
        if !self.due(now) {
            return;
        }
        self.dirty = false;
        let library = library.clone();
        let (tx, rx) = bounded(1);
        let spawned = std::thread::Builder::new()
            .name("scene-warnings".into())
            .spawn(move || {
                let _ = tx.send(scene_warning_lines(&library, |path| path.exists()));
                repaint();
            });
        if spawned.is_ok() {
            self.job = Some(rx);
        } else {
            self.refreshed = Some(now);
        }
    }
}

/// Second line of a scene button. State is spelled out in words, never only
/// in colour. With reduced motion the percentage (which would change every
/// frame) is left out.
pub fn card_subline(card: &SceneCard, fade_in: Option<f32>, reduced_motion: bool) -> String {
    if card.active {
        return match fade_in {
            Some(_) if reduced_motion => "ACTIVE · FADING IN".to_string(),
            Some(progress) => format!("ACTIVE · FADING IN {:.0}%", progress * 100.0),
            None => "ACTIVE".to_string(),
        };
    }
    let count = match card.sounds {
        1 => "1 SOUND".to_string(),
        n => format!("{n} SOUNDS"),
    };
    format!(
        "{count} · FADE {}",
        format_seconds(card.fade_in_ms).to_uppercase()
    )
}

/// A stinger pad: a loaded sound flagged `oneshot_stinger`.
#[derive(Debug, Clone, PartialEq)]
pub struct StingerPad {
    /// Engine id, what [`AudioCommand::PlayStinger`] takes.
    pub id: EngineSoundId,
    pub name: String,
}

/// Stinger pads in board order.
pub fn stinger_pads(library: &Library, sounds: &[SoundEntry]) -> Vec<StingerPad> {
    sounds
        .iter()
        .filter(|entry| {
            library
                .sound(&entry.path)
                .is_some_and(|sound| sound.oneshot_stinger)
        })
        .map(|entry| StingerPad {
            id: entry.id,
            name: entry.name.clone(),
        })
        .collect()
}

/// How many cards of at least `min_width` fit a row of `width`, at least 1.
pub fn grid_columns(width: f32, min_width: f32, gap: f32) -> usize {
    if width <= min_width {
        return 1;
    }
    (((width + gap) / (min_width + gap)).floor() as usize).max(1)
}

/// Width of one card so that `columns` of them (plus gaps) fill `width`
/// without overflowing it.
pub fn card_width(width: f32, columns: usize, gap: f32) -> f32 {
    let columns = columns.max(1) as f32;
    ((width - gap * (columns - 1.0)) / columns).floor().max(1.0)
}

/// Slider position `0..=1` to a transition time, snapped to
/// [`TRANSITION_STEP_MS`].
pub fn slider_to_ms(position: f32) -> u32 {
    let ms = position.clamp(0.0, 1.0) * TRANSITION_MAX_MS as f32;
    ((ms / TRANSITION_STEP_MS as f32).round() as u32 * TRANSITION_STEP_MS).min(TRANSITION_MAX_MS)
}

/// Inverse of [`slider_to_ms`].
pub fn ms_to_slider(ms: u32) -> f32 {
    (ms.min(TRANSITION_MAX_MS) as f32 / TRANSITION_MAX_MS as f32).clamp(0.0, 1.0)
}

/// Progress `0..=1` of a fade that has run for `elapsed`; a zero-length fade
/// is complete at once.
pub fn transition_progress(elapsed: Duration, total_ms: u32) -> f32 {
    if total_ms == 0 {
        return 1.0;
    }
    (elapsed.as_secs_f32() / (total_ms as f32 / 1000.0)).clamp(0.0, 1.0)
}

/// Fade the scene will really use: the shared slider when it overrides.
pub fn effective_fade_ms(override_ms: Option<u32>, scene_ms: u32) -> u32 {
    override_ms.unwrap_or(scene_ms)
}

/// Fade of "Stop all": the slider, else the active scene's fade-out, else
/// [`DEFAULT_STOP_ALL_FADE_MS`].
pub fn stop_all_fade_ms(override_ms: Option<u32>, active: Option<&SceneConfig>) -> u32 {
    override_ms
        .or_else(|| active.map(|scene| scene.fade_out_ms))
        .unwrap_or(DEFAULT_STOP_ALL_FADE_MS)
}

/// Put the shared transition time into every scene's fades.
pub fn patch_scene_fades(scenes: &mut [SceneConfig], ms: u32) {
    for scene in scenes {
        scene.fade_in_ms = ms;
        scene.fade_out_ms = ms;
    }
}

/// Make the Director use the shared transition time (`Some`) for the command
/// that is about to run. The Director's scenes are re-synced from the library
/// before every scene command, so this never outlives it.
pub fn patch_director(director: &mut crate::audio::Director, override_ms: Option<u32>) {
    let Some(ms) = override_ms else { return };
    let mut scenes = director.scenes().to_vec();
    patch_scene_fades(&mut scenes, ms);
    director.set_scenes(scenes);
}

// ============================================================================
// Transient UI state
// ============================================================================

/// A line shown in the deck for a few seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

/// The reply channel of a command that is on its way to the audio engine.
enum Reply {
    Unit(Receiver<anyhow::Result<()>>),
    Scene(Receiver<anyhow::Result<SceneConfig>>),
}

impl Reply {
    /// `None` while the engine has not answered; `Some(Ok(detail))` on
    /// success (`detail` is the saved scene's name for a snapshot),
    /// `Some(Err(text))` on failure.
    fn poll(&self) -> Option<Result<Option<String>, String>> {
        let silent = || "the audio engine did not answer".to_string();
        match self {
            Self::Unit(rx) => match rx.try_recv() {
                Ok(Ok(())) => Some(Ok(None)),
                Ok(Err(e)) => Some(Err(format!("{e:#}"))),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(silent())),
            },
            Self::Scene(rx) => match rx.try_recv() {
                Ok(Ok(scene)) => Some(Ok(Some(scene.name))),
                Ok(Err(e)) => Some(Err(format!("{e:#}"))),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(silent())),
            },
        }
    }
}

struct Pending {
    /// What was asked, for the error line: `Activate “Tavern”`.
    what: String,
    reply: Reply,
}

/// A fade-in that is being shown.
#[derive(Debug, Clone, PartialEq)]
struct Transition {
    scene: String,
    started: Instant,
    total_ms: u32,
}

impl Transition {
    fn total(&self) -> Duration {
        Duration::from_millis(u64::from(self.total_ms))
    }
}

/// Everything the scene panel remembers between frames.
pub struct SceneUi {
    /// The deck (controls and grid) is expanded.
    pub deck_open: bool,
    /// `true`: scenes use their own fades; `false`: the shared slider wins.
    pub auto_fade: bool,
    /// The shared transition time, ms.
    pub transition_ms: u32,
    notice: Option<Notice>,
    pending: Vec<Pending>,
    transition: Option<Transition>,
    last_active: Option<String>,
    /// Problems of each scene, shown as a badge on its card.
    pub warnings: WarningCache,
}

impl Default for SceneUi {
    fn default() -> Self {
        Self {
            deck_open: true,
            auto_fade: true,
            transition_ms: DEFAULT_TRANSITION_MS,
            notice: None,
            pending: Vec::new(),
            transition: None,
            last_active: None,
            warnings: WarningCache::default(),
        }
    }
}

impl SceneUi {
    /// The shared transition time when it overrides the scenes' fades.
    pub fn override_ms(&self) -> Option<u32> {
        if self.auto_fade {
            None
        } else {
            Some(self.transition_ms)
        }
    }

    /// The line currently shown, if it has not expired.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Show a line in the deck.
    pub fn notify(&mut self, text: impl Into<String>, error: bool, now: Instant) {
        let hold = if error {
            NOTICE_ERROR_HOLD
        } else {
            NOTICE_INFO_HOLD
        };
        self.notice = Some(Notice {
            text: text.into(),
            error,
            until: now + hold,
        });
    }

    fn expect_reply(&mut self, what: String, reply: Reply) {
        self.pending.push(Pending { what, reply });
    }

    /// Drain the replies that have arrived and drop expired state. Never
    /// blocks.
    pub fn poll(&mut self, now: Instant) {
        let mut finished = Vec::new();
        self.pending.retain(|pending| match pending.reply.poll() {
            None => true,
            Some(outcome) => {
                finished.push((pending.what.clone(), outcome));
                false
            }
        });
        for (what, outcome) in finished {
            match outcome {
                Err(e) => self.notify(format!("{what}: {e}"), true, now),
                Ok(Some(name)) => self.notify(format!("Saved scene “{name}”"), false, now),
                Ok(None) => {}
            }
        }
        if self.notice.as_ref().is_some_and(|n| now >= n.until) {
            self.notice = None;
        }
        if self
            .transition
            .as_ref()
            .is_some_and(|t| now.saturating_duration_since(t.started) >= t.total())
        {
            self.transition = None;
        }
    }

    /// Note the Director's active scene. A change starts the fade-in display
    /// (`fade_in_ms` is asked for only then) or clears it.
    pub fn observe_active(
        &mut self,
        active: Option<&str>,
        now: Instant,
        fade_in_ms: impl FnOnce(&str) -> u32,
    ) {
        if self.last_active.as_deref() == active {
            return;
        }
        self.last_active = active.map(str::to_string);
        self.transition = active.and_then(|id| {
            let total_ms = fade_in_ms(id);
            (total_ms > 0).then(|| Transition {
                scene: id.to_string(),
                started: now,
                total_ms,
            })
        });
    }

    /// The scene that is fading in and how far along it is.
    pub fn progress(&self, now: Instant) -> Option<(&str, f32)> {
        let t = self.transition.as_ref()?;
        let elapsed = now.saturating_duration_since(t.started);
        if elapsed >= t.total() {
            return None;
        }
        Some((t.scene.as_str(), transition_progress(elapsed, t.total_ms)))
    }

    /// When the next frame is needed for the deck's own sake.
    pub fn wakeup(&self, now: Instant, reduced_motion: bool) -> Option<Duration> {
        let mut wait: Option<Duration> = None;
        let mut want = |d: Duration| wait = Some(wait.map_or(d, |w| w.min(d)));
        if let Some(t) = &self.transition {
            let remaining = (t.started + t.total()).saturating_duration_since(now);
            want(if reduced_motion {
                remaining
            } else {
                remaining.min(ANIMATION_FRAME)
            });
        }
        if let Some(n) = &self.notice {
            want(n.until.saturating_duration_since(now));
        }
        wait
    }
}

// ============================================================================
// SoundSharedApp: commands and library edits
// ============================================================================

impl SoundSharedApp {
    /// Once per frame, after the audio commands ran: collect replies, follow
    /// the active scene and ask for the repaints the deck itself needs.
    pub fn scene_frame(&mut self, ctx: &Context) {
        let now = Instant::now();
        self.scenes.poll(now);
        let active = self.director.active_scene().map(str::to_string);
        let director = &self.director;
        self.scenes.observe_active(active.as_deref(), now, |id| {
            director
                .scene(id)
                .map(|scene| scene.fade_in_ms)
                .unwrap_or(0)
        });
        if let Some(wait) = self.scenes.wakeup(now, self.reduced_motion()) {
            ctx.request_repaint_after(wait);
        }
        let repaint_ctx = ctx.clone();
        self.scenes
            .warnings
            .pump(&self.library, now, move || repaint_ctx.request_repaint());
        // The interface scale lives here because this runs once per frame,
        // right after the settings were last edited.
        let scale = self
            .app_state
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .gui
            .scale();
        crate::gui::settings_extras::apply_ui_scale(ctx, scale);
    }

    /// Queue a command whose reply is only an acknowledgement.
    fn send_unit(
        &mut self,
        ctx: &Context,
        what: String,
        make: impl FnOnce(Sender<anyhow::Result<()>>) -> AudioCommand,
    ) {
        let (tx, rx) = bounded(1);
        match self.app_state.audio_cmd_tx.try_send(make(tx)) {
            Ok(()) => {
                self.scenes.expect_reply(what, Reply::Unit(rx));
                // The command runs at the start of the next frame; without this
                // an idle console would wait for its slow wake-up timer.
                ctx.request_repaint();
            }
            Err(e) => {
                tracing::error!("scene command not queued ({what}): {e}");
                self.scenes.notify(
                    format!("{what}: the audio command queue is full, try again"),
                    true,
                    Instant::now(),
                );
            }
        }
    }

    /// Switch to a scene.
    pub fn request_activate_scene(&mut self, ctx: &Context, id: SceneId) {
        let name = self
            .library
            .scene(id)
            .map(|scene| scene.name.clone())
            .unwrap_or_default();
        self.send_unit(ctx, format!("Activate “{name}”"), |response| {
            AudioCommand::ActivateScene {
                id: id.to_string(),
                response,
            }
        });
    }

    /// Fade out the active scene.
    pub fn request_stop_scene(&mut self, ctx: &Context) {
        self.send_unit(ctx, "Stop scene".to_string(), |response| {
            AudioCommand::StopScene { response }
        });
    }

    /// Fade everything out over the shared transition time (or the active
    /// scene's fade-out).
    pub fn request_stop_all_fade(&mut self, ctx: &Context) {
        let active = self
            .director
            .active_scene()
            .and_then(|id| self.director.scene(id));
        let fade_ms = stop_all_fade_ms(self.scenes.override_ms(), active);
        self.send_unit(ctx, "Stop all".to_string(), |response| {
            AudioCommand::StopAllFade { fade_ms, response }
        });
    }

    /// Fire a stinger pad.
    pub fn request_stinger(&mut self, ctx: &Context, pad: &StingerPad) {
        let id = pad.id;
        self.send_unit(ctx, format!("Stinger “{}”", pad.name), |response| {
            AudioCommand::PlayStinger { id, response }
        });
    }

    /// Save the mix that plays now as a scene named `name`.
    fn request_snapshot(&mut self, ctx: &Context, name: String) {
        let (tx, rx) = bounded(1);
        let command = AudioCommand::SnapshotScene {
            name: name.clone(),
            response: tx,
        };
        match self.app_state.audio_cmd_tx.try_send(command) {
            Ok(()) => {
                self.scenes
                    .expect_reply(format!("Save mix as “{name}”"), Reply::Scene(rx));
                ctx.request_repaint();
            }
            Err(e) => {
                tracing::error!("snapshot command not queued: {e}");
                self.scenes.notify(
                    "Save mix: the audio command queue is full, try again",
                    true,
                    Instant::now(),
                );
            }
        }
    }

    /// The library's scenes or sounds changed: persist them and hand the
    /// Director the new set (so the active highlight and the next activation
    /// see it).
    fn library_changed(&mut self) {
        self.library_dirty = true;
        self.scenes.warnings.invalidate();
        crate::gui::app::sync_director(&mut self.director, &self.library, &self.sounds);
    }

    // ------------------------------------------------------------ dialogs ----

    /// Open the editor on a blank scene.
    pub fn open_new_scene(&mut self) {
        self.dialog = Dialog::EditScene(Box::new(SceneForm::blank(&self.library)));
    }

    /// Open the editor on an existing scene.
    pub fn open_edit_scene(&mut self, id: SceneId) {
        if let Some(scene) = self.library.scene(id) {
            self.dialog = Dialog::EditScene(Box::new(SceneForm::from_scene(&self.library, scene)));
        }
    }

    /// Open the "save the current mix" dialog with a free name.
    pub fn open_save_mix(&mut self) {
        self.dialog = Dialog::SaveMixAsScene {
            name: suggest_scene_name(&self.library),
            error: None,
        };
    }

    /// Open the delete confirmation.
    pub fn open_delete_scene(&mut self, id: SceneId) {
        self.dialog = Dialog::DeleteScene { id };
    }

    /// Apply the open editor. Errors stay in the form.
    pub fn submit_scene_form(&mut self) {
        let Dialog::EditScene(form) = &mut self.dialog else {
            return;
        };
        form.attempted = true;
        let Ok(valid) = form.validate(&self.library) else {
            return;
        };
        match apply_scene(&mut self.library, form.id, &valid) {
            Ok(_) => {
                self.dialog = Dialog::None;
                self.library_changed();
                // A pan edited on an item of the active scene is heard at once.
                self.request_sync_pans();
            }
            Err(reason) => self.scenes.notify(
                format!("Could not save the scene: {reason}"),
                true,
                Instant::now(),
            ),
        }
    }

    /// Start saving the mix named in the open dialog.
    pub fn submit_save_mix(&mut self, ctx: &Context) {
        let Dialog::SaveMixAsScene { name, error } = &mut self.dialog else {
            return;
        };
        match validate_scene_name(&self.library, name, None) {
            Ok(valid) => {
                self.dialog = Dialog::None;
                self.request_snapshot(ctx, valid);
            }
            Err(message) => *error = Some(message),
        }
    }

    /// Delete the scene named in the open confirmation.
    pub fn confirm_delete_scene(&mut self) {
        let Dialog::DeleteScene { id } = &self.dialog else {
            return;
        };
        let id = *id;
        if self.library.remove_scene(id) {
            self.library_changed();
        }
        self.dialog = Dialog::None;
    }

    /// Duplicate a scene from its menu.
    pub fn duplicate_scene_ui(&mut self, id: SceneId) {
        let name = self.library.scene(id).map(|scene| scene.name.clone());
        match duplicate_scene(&mut self.library, id) {
            Some(copy) => {
                let copy_name = self
                    .library
                    .scene(copy)
                    .map(|scene| scene.name.clone())
                    .unwrap_or_default();
                self.library_changed();
                self.scenes
                    .notify(format!("Created “{copy_name}”"), false, Instant::now());
            }
            None => self.scenes.notify(
                format!("Could not duplicate “{}”", name.unwrap_or_default()),
                true,
                Instant::now(),
            ),
        }
    }

    /// Context menu of a sound: add it to a scene.
    fn add_sound_to_scene_ui(&mut self, scene: SceneId, sound: LibSoundId) {
        let scene_name = self
            .library
            .scene(scene)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let sound_name = self
            .library
            .sound_by_id(sound)
            .map(sound_label)
            .unwrap_or_default();
        let outcome = add_sound_to_scene(&mut self.library, scene, sound);
        let now = Instant::now();
        match outcome {
            AddToScene::Added => {
                self.library_changed();
                self.scenes.notify(
                    format!("Added “{sound_name}” to “{scene_name}”"),
                    false,
                    now,
                );
            }
            AddToScene::AlreadyPresent => self.scenes.notify(
                format!("“{sound_name}” is already in “{scene_name}”"),
                false,
                now,
            ),
            AddToScene::UnknownScene | AddToScene::UnknownSound => {
                self.scenes
                    .notify("Could not add the sound to the scene", true, now)
            }
        }
    }

    /// Context menu of a sound: toggle its stinger flag.
    fn toggle_stinger_ui(&mut self, sound: LibSoundId) {
        let Some(entry) = self.library.sound_by_id(sound) else {
            return;
        };
        let (path, now_stinger) = (entry.path.clone(), !entry.oneshot_stinger);
        if self.library.set_oneshot_stinger(&path, now_stinger) {
            self.library_changed();
        }
    }
}

// ============================================================================
// Painting: the deck
// ============================================================================

/// Smallest scene button.
const CARD_MIN_W: f32 = 150.0;
/// Scene button height.
const CARD_H: f32 = 54.0;
/// Gap between scene buttons and stinger pads.
const GRID_GAP: f32 = 8.0;
/// Stinger pad width.
const PAD_W: f32 = 132.0;

// ---- Deck layout (pure, tested over the whole range of panel widths) ----

/// `SCENES` caption plus the `N scenes` count (10px mono, tracked), with room
/// for a three-digit count.
const HEAD_LEFT_W: f32 = 136.0;
/// `· ACTIVE: <name>` with the name cut at 24 characters.
const HEAD_ACTIVE_W: f32 = 250.0;
/// `Save Mix As Scene` / `Save Mix` buttons.
const SAVE_FULL_W: f32 = 170.0;
const SAVE_SHORT_W: f32 = 108.0;
/// A glyph-only button.
const ICON_BTN_W: f32 = 30.0;
/// `New` button with its text.
const NEW_W: f32 = 70.0;
/// `Show` / `Hide` toggle.
const TOGGLE_W: f32 = 64.0;

/// How the save-mix button is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStyle {
    /// `Save Mix As Scene`.
    Full,
    /// `Save Mix`.
    Short,
    /// The wave glyph alone (`New` turns into its glyph too).
    Icon,
}

/// How the deck header spends its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeckHeaderLayout {
    pub save: SaveStyle,
    /// `· Active: <scene>` on the header line (else on a line of its own).
    pub active_inline: bool,
    /// The buttons sit on a line under the caption: not even the icon form
    /// fits next to it.
    pub stacked: bool,
}

impl DeckHeaderLayout {
    /// Width of the three buttons with `Metrics::SM` between them.
    pub fn buttons_w(&self) -> f32 {
        let (save, new) = match self.save {
            SaveStyle::Full => (SAVE_FULL_W, NEW_W),
            SaveStyle::Short => (SAVE_SHORT_W, NEW_W),
            SaveStyle::Icon => (ICON_BTN_W, ICON_BTN_W),
        };
        save + new + TOGGLE_W + 2.0 * Metrics::SM
    }

    /// Width of the header line's items.
    pub fn width(&self) -> f32 {
        if self.stacked {
            return HEAD_LEFT_W.max(self.buttons_w());
        }
        let active = if self.active_inline {
            Metrics::SM + HEAD_ACTIVE_W
        } else {
            0.0
        };
        HEAD_LEFT_W + active + Metrics::SM + self.buttons_w()
    }
}

/// Header layout for a deck `avail` points wide: the roomiest button set that
/// fits next to the caption; the active scene's name moves to its own line
/// when it does not fit on the header line.
pub fn deck_header_layout(avail: f32) -> DeckHeaderLayout {
    for save in [SaveStyle::Full, SaveStyle::Short, SaveStyle::Icon] {
        let below = DeckHeaderLayout {
            save,
            active_inline: false,
            stacked: false,
        };
        if below.width() <= avail {
            let inline = DeckHeaderLayout {
                active_inline: true,
                ..below
            };
            return if inline.width() <= avail {
                inline
            } else {
                below
            };
        }
    }
    DeckHeaderLayout {
        save: SaveStyle::Icon,
        active_inline: false,
        stacked: true,
    }
}

const STOP_SCENE_W: f32 = 118.0;
const FADE_OUT_W: f32 = 104.0;
/// `TRANSITION` micro label.
const TRANSITION_LABEL_W: f32 = 76.0;
const AUTO_W: f32 = 56.0;
/// Transition slider range.
const TRANSITION_SLIDER_MAX_W: f32 = 120.0;
const TRANSITION_SLIDER_MIN_W: f32 = 48.0;
/// Value text after the slider: `2.5 s` or `Per scene`.
const TRANSITION_VALUE_W: f32 = 66.0;

/// How the transport row of the deck is arranged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportLayout {
    /// Stop buttons and the transition group share one line.
    pub one_row: bool,
    /// The transition group is cut in two lines: `TRANSITION` + `Auto`, then
    /// the slider and its value.
    pub split_group: bool,
    /// The two stop buttons are one above the other (they do not fit side by
    /// side). egui cannot wrap these painted buttons by itself.
    pub buttons_stacked: bool,
    pub slider_w: f32,
}

impl TransportLayout {
    fn group_w(slider_w: f32) -> f32 {
        TRANSITION_LABEL_W
            + Metrics::SM
            + AUTO_W
            + Metrics::SM
            + slider_w
            + Metrics::SM
            + TRANSITION_VALUE_W
    }

    /// Width of the widest line of the arrangement.
    pub fn widest_line(&self) -> f32 {
        let buttons = if self.buttons_stacked {
            STOP_SCENE_W.max(FADE_OUT_W)
        } else {
            STOP_SCENE_W + Metrics::SM + FADE_OUT_W
        };
        if self.split_group {
            let top = TRANSITION_LABEL_W + Metrics::SM + AUTO_W;
            let bottom = self.slider_w + Metrics::SM + TRANSITION_VALUE_W;
            return buttons.max(top).max(bottom);
        }
        let group = Self::group_w(self.slider_w);
        if self.one_row {
            buttons + Metrics::MD + group
        } else {
            buttons.max(group)
        }
    }
}

/// Arrangement of the transport row for `avail` points. The transition group is
/// never torn apart mid-way, so its value text cannot end up alone on a line;
/// the stop buttons stack when they do not fit side by side.
pub fn transport_layout(avail: f32) -> TransportLayout {
    let buttons = STOP_SCENE_W + Metrics::SM + FADE_OUT_W;
    let buttons_stacked = avail < buttons;
    let roomy = buttons + Metrics::MD + TransportLayout::group_w(TRANSITION_SLIDER_MAX_W);
    if avail >= roomy {
        return TransportLayout {
            one_row: true,
            split_group: false,
            buttons_stacked: false,
            slider_w: TRANSITION_SLIDER_MAX_W,
        };
    }
    let least = TransportLayout::group_w(TRANSITION_SLIDER_MIN_W);
    if avail >= least {
        let fixed = TransportLayout::group_w(0.0);
        return TransportLayout {
            one_row: false,
            split_group: false,
            buttons_stacked,
            slider_w: (avail - fixed).clamp(TRANSITION_SLIDER_MIN_W, TRANSITION_SLIDER_MAX_W),
        };
    }
    TransportLayout {
        one_row: false,
        split_group: true,
        buttons_stacked,
        slider_w: (avail - Metrics::SM - TRANSITION_VALUE_W).clamp(24.0, TRANSITION_SLIDER_MAX_W),
    }
}

/// What a click in the deck asks for; applied after the frame's borrows end.
#[derive(Debug, Clone, PartialEq)]
enum DeckAction {
    Activate(SceneId),
    Edit(SceneId),
    Duplicate(SceneId),
    Delete(SceneId),
    SaveMix,
    New,
    StopScene,
    StopAll,
    Stinger(usize),
    ToggleDeck,
    SetAuto(bool),
    SetTransition(u32),
}

/// Report a custom-painted button to the accessibility tree.
fn labelled(resp: Response, label: &str) -> Response {
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    resp
}

/// A toggle that says what it does in words (`Loop`, `Auto`, ...) and reports
/// its state to the accessibility tree.
fn toggle_button(ui: &mut Ui, label: &str, on: bool, width: f32, salt: &str) -> bool {
    let resp = ConsoleButton::new(label)
        .kind(if on { BtnKind::Armed } else { BtnKind::Ghost })
        .small()
        .width(width)
        .id_salt(salt)
        .show(ui);
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, on, label));
    resp.clicked()
}

/// Outline the field red while it holds an error.
fn mark_invalid(ui: &mut Ui) {
    let visuals = ui.visuals_mut();
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
    ] {
        widget.bg_stroke = Stroke::new(1.0_f32, Palette::ROSE_500);
    }
}

/// The scene deck. Called by the library panel between its header and the
/// sound rows.
pub fn scenes_deck(ui: &mut Ui, state: &mut SoundSharedApp) {
    let ctx = ui.ctx().clone();
    let now = Instant::now();
    let reduced = state.reduced_motion();
    let sounds = state.sounds();
    let mut cards = scene_cards(state.library(), state.director.active_scene());
    for card in &mut cards {
        card.warnings = state.scenes.warnings.lines(card.id).to_vec();
    }
    let pads = stinger_pads(state.library(), &sounds);
    let progress = state
        .scenes
        .progress(now)
        .map(|(scene, value)| (scene.to_string(), value));
    let open = state.scenes.deck_open;
    let playing_any = sounds.iter().any(|sound| sound.playing);
    let any_sounds = !sounds.is_empty();
    let auto = state.scenes.auto_fade;
    let transition_ms = state.scenes.transition_ms;
    let notice = state.scenes.notice().cloned();
    let active_name = cards
        .iter()
        .find(|card| card.active)
        .map(|card| card.name.clone());

    let mut actions: Vec<DeckAction> = Vec::new();

    // ---- Header ----
    //
    // The buttons are placed first with their exact width and the caption gets
    // what is left, clipped to it: the two can never overlap.
    let avail = ui.available_width();
    let head = deck_header_layout(avail);
    let active_label = active_name
        .as_ref()
        .map(|name| format!("· Active: {}", elide(name, 24)));
    let caption = |ui: &mut Ui| {
        ui.label(micro_label_accent("Scenes"));
        ui.label(micro_label(match cards.len() {
            1 => "1 scene".to_string(),
            n => format!("{n} scenes"),
        }));
        if let (true, Some(text)) = (head.active_inline, &active_label) {
            ui.add(egui::Label::new(micro_label(text.as_str())).truncate());
        }
    };
    if head.stacked {
        ui.horizontal(|ui| caption(ui));
        ui.add_space(Metrics::XS);
    }
    ui.horizontal(|ui| {
        if !head.stacked {
            let left_w = (avail - head.buttons_w() - Metrics::SM).max(0.0);
            ui.allocate_ui_with_layout(
                Vec2::new(left_w, Metrics::BTN_H_SM),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                    caption(ui);
                },
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let toggle = ConsoleButton::ghost(if open { "Hide" } else { "Show" })
                .small()
                .width(TOGGLE_W)
                .id_salt("scenes_toggle")
                .tooltip(if open {
                    "Collapse the scene deck"
                } else {
                    "Expand the scene deck"
                })
                .show(ui);
            if labelled(toggle, if open { "Hide scenes" } else { "Show scenes" }).clicked() {
                actions.push(DeckAction::ToggleDeck);
            }
            let icon_only = head.save == SaveStyle::Icon;
            let new = ConsoleButton::ghost(if icon_only { "" } else { "New" })
                .glyph(glyph::PLUS)
                .small()
                .width(if icon_only { ICON_BTN_W } else { NEW_W })
                .id_salt("scenes_new")
                .tooltip("Create an empty scene and choose its sounds")
                .show(ui);
            if labelled(new, "New scene").clicked() {
                actions.push(DeckAction::New);
            }
            let save = ConsoleButton::primary(match head.save {
                SaveStyle::Full => "Save Mix As Scene",
                SaveStyle::Short => "Save Mix",
                SaveStyle::Icon => "",
            })
            .glyph(glyph::WAVE)
            .small()
            .width(match head.save {
                SaveStyle::Full => SAVE_FULL_W,
                SaveStyle::Short => SAVE_SHORT_W,
                SaveStyle::Icon => ICON_BTN_W,
            })
            .enabled(playing_any)
            .id_salt("scenes_save_mix")
            .tooltip(if playing_any {
                "Save the sounds that play now, with their levels, as a scene"
            } else {
                "Start some sounds first: the scene is built from what is playing"
            })
            .show(ui);
            if labelled(save, "Save current mix as a scene").clicked() {
                actions.push(DeckAction::SaveMix);
            }
        });
    });
    if let (false, Some(text)) = (head.active_inline, &active_label) {
        ui.add_space(Metrics::XS);
        ui.add(egui::Label::new(micro_label(text.as_str())).truncate());
    }

    if let Some(notice) = &notice {
        ui.add_space(Metrics::SM);
        let color = if notice.error {
            Palette::ROSE_400
        } else {
            Palette::ZINC_300
        };
        settings_kit::note_colored(ui, notice.text.as_str(), color);
    }

    if open {
        ui.add_space(Metrics::MD);

        // ---- Transport + shared transition time ----
        //
        // Explicit rows from `transport_layout`: egui cannot wrap these painted
        // buttons, so a row that does not fit is built by hand. The transition
        // group goes under the buttons as a unit instead of breaking apart, so
        // no piece of it ends up alone.
        let transport = transport_layout(ui.available_width());
        ui.horizontal(|ui| {
            let stop_scene = ConsoleButton::ghost("Stop Scene")
                .glyph(glyph::STOP)
                .small()
                .width(STOP_SCENE_W)
                .enabled(active_name.is_some())
                .id_salt("scenes_stop_scene")
                .tooltip("Fade out the active scene's sounds")
                .show(ui);
            if labelled(stop_scene, "Stop the active scene").clicked() {
                actions.push(DeckAction::StopScene);
            }
            if !transport.buttons_stacked {
                fade_out_button(ui, &mut actions);
            }
            if transport.one_row {
                ui.add_space(Metrics::XS);
                transition_group(ui, &mut actions, &transport, auto, transition_ms);
            }
        });
        if transport.buttons_stacked {
            ui.add_space(Metrics::XS);
            ui.horizontal(|ui| fade_out_button(ui, &mut actions));
        }
        if !transport.one_row {
            ui.add_space(Metrics::SM);
            transition_group(ui, &mut actions, &transport, auto, transition_ms);
        }

        ui.add_space(Metrics::MD);

        // ---- Scene grid ----
        if cards.is_empty() {
            if settings_kit::empty_state_action(
                ui,
                "No scenes yet",
                "Create your first scene \u{2014} set up a mix, then press Save Mix As Scene. \
                 A scene remembers which sounds play and how loud.",
                "New scene",
                "scenes_empty_new",
            ) {
                actions.push(DeckAction::New);
            }
        } else {
            let grid_h =
                (ui.available_height() * 0.34).clamp(CARD_H + 6.0, CARD_H * 2.0 + GRID_GAP + 6.0);
            let width = ui.available_width();
            let columns = grid_columns(width, CARD_MIN_W, GRID_GAP);
            let each = card_width(width, columns, GRID_GAP);
            egui::ScrollArea::vertical()
                .id_salt("scene_grid_scroll")
                .auto_shrink([false, true])
                .max_height(grid_h)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(GRID_GAP);
                    ui.horizontal_wrapped(|ui| {
                        for card in &cards {
                            let fading = progress
                                .as_ref()
                                .filter(|(scene, _)| *scene == card.id.to_string())
                                .map(|(_, value)| *value);
                            scene_button(ui, card, each, fading, reduced, &mut actions);
                        }
                    });
                });
        }

        // ---- Stinger pads ----
        ui.add_space(Metrics::LG);
        if pads.is_empty() {
            if any_sounds {
                ui.label(micro_label("Stingers"));
                ui.add_space(Metrics::XS);
                settings_kit::note_under(
                    ui,
                    "One-shot sounds that play over the scene. Right-click a sound and choose \
                     Use As Stinger.",
                    Palette::ZINC_600,
                );
            }
        } else {
            ui.horizontal(|ui| {
                ui.label(micro_label("Stingers"));
                ui.label(micro_label_dim("one-shots over the scene"));
            });
            ui.add_space(Metrics::SM);
            egui::ScrollArea::vertical()
                .id_salt("stinger_scroll")
                .auto_shrink([false, true])
                .max_height(CARD_H)
                .show(ui, |ui| {
                    // Rows built by hand: egui cannot wrap painted buttons.
                    let per_row = grid_columns(ui.available_width(), PAD_W, Metrics::SM);
                    for (row, chunk) in pads.chunks(per_row).enumerate() {
                        if row > 0 {
                            ui.add_space(Metrics::SM);
                        }
                        ui.horizontal(|ui| {
                            for (offset, pad) in chunk.iter().enumerate() {
                                let index = row * per_row + offset;
                                let salt = format!("stinger_{}", pad.id.0);
                                let resp = ConsoleButton::ghost(elide(&pad.name, 13))
                                    .glyph(glyph::PLAY)
                                    .small()
                                    .width(PAD_W)
                                    .id_salt(&salt)
                                    .tooltip(format!("Play “{}” once, over everything", pad.name))
                                    .show(ui);
                                if labelled(resp, &format!("Stinger {}", pad.name)).clicked() {
                                    actions.push(DeckAction::Stinger(index));
                                }
                            }
                        });
                    }
                });
        }
    }

    apply_deck_actions(state, &ctx, actions, &pads);
}

/// `Fade Out`: fade everything that plays over the transition time.
fn fade_out_button(ui: &mut Ui, actions: &mut Vec<DeckAction>) {
    let stop_all = ConsoleButton::danger("Fade Out")
        .glyph(glyph::STOP)
        .small()
        .width(FADE_OUT_W)
        .id_salt("scenes_stop_all")
        .tooltip("Fade out everything that plays, over the transition time")
        .show(ui);
    if labelled(stop_all, "Stop all sounds with a fade-out").clicked() {
        actions.push(DeckAction::StopAll);
    }
}

/// `TRANSITION`, the `Auto` toggle, the slider and its value, as one unit.
/// With `layout.split_group` it is drawn as two lines, still as a unit.
fn transition_group(
    ui: &mut Ui,
    actions: &mut Vec<DeckAction>,
    layout: &TransportLayout,
    auto: bool,
    transition_ms: u32,
) {
    let head = |ui: &mut Ui, actions: &mut Vec<DeckAction>| {
        ui.add_sized(
            [TRANSITION_LABEL_W, Metrics::BTN_H_SM],
            egui::Label::new(micro_label("Transition")),
        );
        if toggle_button(ui, "Auto", auto, AUTO_W, "scenes_auto_fade") {
            actions.push(DeckAction::SetAuto(!auto));
        }
    };
    let tail = |ui: &mut Ui, actions: &mut Vec<DeckAction>| {
        let mut position = ms_to_slider(transition_ms);
        let slider = level_slider(ui, &mut position, layout.slider_w, "scenes_transition");
        slider.widget_info(|| {
            WidgetInfo::slider(
                true,
                f64::from(transition_ms) / 1000.0,
                if auto {
                    "Transition time: each scene's own fade".to_string()
                } else {
                    format!("Transition time: {}", format_seconds(transition_ms))
                },
            )
        });
        if slider.changed() {
            actions.push(DeckAction::SetTransition(slider_to_ms(position)));
        }
        // Fixed width: the value changing between `Per scene` and `2.5 s` never
        // moves anything else.
        let (text, color) = if auto {
            ("Per scene".to_string(), Palette::ZINC_400)
        } else {
            (format_seconds(transition_ms), Palette::WHITE)
        };
        let value = ui.add_sized(
            [TRANSITION_VALUE_W, Metrics::BTN_H_SM],
            egui::Label::new(RichText::new(text).font(glyph_font(11.0)).color(color)),
        );
        if auto {
            value.on_hover_text("Auto: every scene fades with its own fade-in time");
        }
    };
    if layout.split_group {
        ui.horizontal(|ui| head(ui, actions));
        ui.add_space(Metrics::XS);
        ui.horizontal(|ui| tail(ui, actions));
    } else {
        ui.horizontal(|ui| {
            head(ui, actions);
            tail(ui, actions);
        });
    }
}

/// Run what the frame collected.
fn apply_deck_actions(
    state: &mut SoundSharedApp,
    ctx: &Context,
    actions: Vec<DeckAction>,
    pads: &[StingerPad],
) {
    for action in actions {
        match action {
            DeckAction::Activate(id) => state.request_activate_scene(ctx, id),
            DeckAction::Edit(id) => state.open_edit_scene(id),
            DeckAction::Duplicate(id) => state.duplicate_scene_ui(id),
            DeckAction::Delete(id) => state.open_delete_scene(id),
            DeckAction::SaveMix => state.open_save_mix(),
            DeckAction::New => state.open_new_scene(),
            DeckAction::StopScene => state.request_stop_scene(ctx),
            DeckAction::StopAll => state.request_stop_all_fade(ctx),
            DeckAction::Stinger(index) => {
                if let Some(pad) = pads.get(index) {
                    state.request_stinger(ctx, pad);
                }
            }
            DeckAction::ToggleDeck => state.scenes.deck_open = !state.scenes.deck_open,
            DeckAction::SetAuto(auto) => state.scenes.auto_fade = auto,
            DeckAction::SetTransition(ms) => {
                // Touching the slider takes over from the scenes' own fades.
                state.scenes.transition_ms = ms;
                state.scenes.auto_fade = false;
            }
        }
    }
}

/// One scene button: colour stripe, name, state line, fade-in bar, a pencil
/// for the editor and a right-click menu.
fn scene_button(
    ui: &mut Ui,
    card: &SceneCard,
    width: f32,
    fade_in: Option<f32>,
    reduced_motion: bool,
    actions: &mut Vec<DeckAction>,
) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, CARD_H), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let edit_rect = Rect::from_min_size(
        egui::pos2(rect.right() - 28.0, rect.top() + 6.0),
        Vec2::splat(22.0),
    );
    let edit = ui
        .interact(edit_rect, resp.id.with("edit"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Edit scene");

    let has_warnings = !card.warnings.is_empty();
    let badge_rect = Rect::from_center_size(
        egui::pos2(edit_rect.left() - 12.0, edit_rect.center().y),
        Vec2::splat(18.0),
    );
    let badge = has_warnings.then(|| {
        ui.interact(badge_rect, resp.id.with("warn"), Sense::hover())
            .on_hover_cursor(CursorIcon::Help)
            .on_hover_text(warning_tooltip(&card.warnings))
    });

    let accent = to_color32(card.accent);
    let hovered = resp.hovered() || edit.hovered();
    let focused = resp.has_focus();
    let painter = ui.painter();
    let round = Rounding::same(Metrics::R_SM);

    painter.rect_filled(
        rect,
        round,
        if hovered && !card.active {
            Palette::ROW_HOVER
        } else {
            Palette::ROW
        },
    );
    if card.active {
        painter.rect_filled(rect, round, alpha(accent, 46));
    }
    painter.rect_stroke(
        rect,
        round,
        Stroke::new(
            if card.active { 1.5_f32 } else { 1.0_f32 },
            if card.active {
                accent
            } else if hovered {
                Palette::BORDER_STRONG
            } else {
                Palette::BORDER
            },
        ),
    );
    if focused {
        painter.rect_stroke(
            rect.expand(1.0),
            Rounding::same(Metrics::R_SM + 1.0),
            Stroke::new(1.0_f32, Palette::ROSE_400),
        );
    }
    painter.rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height())),
        Rounding::ZERO,
        accent,
    );

    let text_x = rect.left() + 14.0;
    let reserved = if has_warnings {
        14.0 + 56.0
    } else {
        14.0 + 34.0
    };
    let max_chars = (((width - reserved) / 7.2) as usize).max(6);
    let name = painter.layout_no_wrap(elide(&card.name, max_chars), ui_font(13.0), Palette::WHITE);
    painter.galley(egui::pos2(text_x, rect.top() + 9.0), name, Palette::WHITE);

    let mut sub_x = text_x;
    if card.active {
        painter.circle_filled(egui::pos2(text_x + 3.0, rect.top() + 35.0), 3.0, accent);
        sub_x += 11.0;
    }
    painter.text(
        egui::pos2(sub_x, rect.top() + 30.0),
        egui::Align2::LEFT_TOP,
        card_subline(card, fade_in, reduced_motion),
        glyph_font(9.0),
        Palette::ZINC_400,
    );

    // Fade-in bar along the bottom edge; with reduced motion the state line
    // above carries the information instead.
    if let (Some(value), false) = (fade_in, reduced_motion) {
        let track = Rect::from_min_max(
            egui::pos2(rect.left() + 4.0, rect.bottom() - 4.0),
            egui::pos2(rect.right() - 1.0, rect.bottom() - 1.0),
        );
        painter.rect_filled(track, Rounding::ZERO, Palette::BORDER);
        painter.rect_filled(
            Rect::from_min_size(track.min, Vec2::new(track.width() * value, track.height())),
            Rounding::ZERO,
            accent,
        );
    }

    if has_warnings {
        painter.circle_filled(badge_rect.center(), 8.0, Palette::AMBER_400);
        painter.text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            "!",
            glyph_font(12.0),
            Palette::BLACK,
        );
    }

    paint_icon(
        painter,
        glyph::EDIT,
        edit_rect.center(),
        13.0,
        if edit.hovered() || edit.has_focus() {
            Palette::WHITE
        } else {
            Palette::ZINC_400
        },
    );
    if edit.has_focus() {
        painter.rect_stroke(
            edit_rect.expand(1.0),
            Rounding::same(Metrics::R_SM),
            Stroke::new(1.0_f32, Palette::ROSE_400),
        );
    }

    let label = format!("Scene {}", card.name);
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, card.active, &label));
    edit.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            true,
            format!("Edit scene {}", card.name),
        )
    });
    if let Some(badge) = &badge {
        badge.widget_info(|| {
            WidgetInfo::labeled(
                WidgetType::Label,
                true,
                format!(
                    "Scene {} has {} warning{}",
                    card.name,
                    card.warnings.len(),
                    if card.warnings.len() == 1 { "" } else { "s" }
                ),
            )
        });
    }

    if edit.clicked() || edit.fake_primary_click {
        actions.push(DeckAction::Edit(card.id));
    } else if resp.clicked() || resp.fake_primary_click {
        actions.push(DeckAction::Activate(card.id));
    }

    // Two tooltips would overlap: the badge's own wins while it is hovered.
    let resp = if badge.as_ref().is_some_and(Response::hovered) {
        resp
    } else {
        resp.on_hover_text(format!(
            "Activate “{}”. Right-click for more, or use the pencil to edit",
            card.name
        ))
    };
    resp.context_menu(|ui| {
        ui.set_min_width(180.0);
        if menu_item(ui, "Activate", glyph::PLAY, BtnKind::Ghost) {
            actions.push(DeckAction::Activate(card.id));
        }
        if menu_item(ui, "Edit…", glyph::EDIT, BtnKind::Ghost) {
            actions.push(DeckAction::Edit(card.id));
        }
        if menu_item(ui, "Duplicate", glyph::COPY, BtnKind::Ghost) {
            actions.push(DeckAction::Duplicate(card.id));
        }
        if menu_item(ui, "Delete…", glyph::TRASH, BtnKind::Danger) {
            actions.push(DeckAction::Delete(card.id));
        }
    });
}

/// A row in a context menu; closes the menu when used.
fn menu_item(ui: &mut Ui, label: &str, icon: Glyph, kind: BtnKind) -> bool {
    let resp = ConsoleButton::new(label)
        .glyph(icon)
        .kind(kind)
        .small()
        .width(180.0)
        .id_salt(label)
        .show(ui);
    let hit = labelled(resp, label).clicked();
    if hit {
        ui.close_menu();
    }
    hit
}

/// The right-click menu of a sound row: add it to a scene, flag it as a
/// stinger. Called from `sound_list`.
pub fn sound_context_menu(ui: &mut Ui, state: &mut SoundSharedApp, sound: &SoundEntry) {
    ui.set_min_width(200.0);
    let Some(library_id) = state.library().id_of(&sound.path) else {
        ui.label(micro_label("Not in the library"));
        return;
    };
    let is_stinger = state
        .library()
        .sound_by_id(library_id)
        .is_some_and(|s| s.oneshot_stinger);
    let scenes: Vec<(SceneId, String, bool)> = state
        .library()
        .scenes()
        .iter()
        .map(|scene| {
            (
                scene.id,
                scene.name.clone(),
                scene.items.iter().any(|item| item.sound == library_id),
            )
        })
        .collect();

    ui.label(micro_label("Add to scene"));
    ui.add_space(Metrics::XS);
    let mut add_to: Option<SceneId> = None;
    if scenes.is_empty() {
        ui.label(micro_label_dim("No scenes yet"));
    } else {
        egui::ScrollArea::vertical()
            .id_salt("sound_menu_scenes")
            .max_height(180.0)
            .auto_shrink([true, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = Metrics::XS;
                for (id, name, contains) in &scenes {
                    let salt = format!("add_to_{id}");
                    let resp = ConsoleButton::ghost(elide(name, 20))
                        .glyph(if *contains { glyph::CHECK } else { glyph::PLUS })
                        .small()
                        .width(200.0)
                        .enabled(!contains)
                        .id_salt(&salt)
                        .tooltip(if *contains {
                            format!("Already in “{name}”")
                        } else {
                            format!("Add to “{name}” at its saved level")
                        })
                        .show(ui);
                    if !contains && labelled(resp, &format!("Add to scene {name}")).clicked() {
                        add_to = Some(*id);
                    }
                }
            });
    }
    ui.add_space(Metrics::SM);
    hairline(ui);
    ui.add_space(Metrics::SM);
    let stinger_label = if is_stinger {
        "Remove Stinger Flag"
    } else {
        "Use As Stinger"
    };
    let toggle_stinger = ConsoleButton::ghost(stinger_label)
        .glyph(glyph::PLAY)
        .small()
        .width(200.0)
        .id_salt("toggle_stinger")
        .tooltip("A stinger is a one-shot that plays over the scene and gets a pad in the deck")
        .show(ui);
    let toggle_stinger = labelled(toggle_stinger, stinger_label).clicked();

    if let Some(scene) = add_to {
        state.add_sound_to_scene_ui(scene, library_id);
        ui.close_menu();
    }
    if toggle_stinger {
        state.toggle_stinger_ui(library_id);
        ui.close_menu();
    }
}

// ============================================================================
// Painting: dialogs
// ============================================================================

/// Width of the scene editor.
const EDITOR_W: f32 = 690.0;
/// Width of the small scene dialogs.
const SMALL_W: f32 = 340.0;

/// A labelled single-line field; `invalid` outlines it red.
fn field(
    ui: &mut Ui,
    label: &str,
    value: &mut String,
    hint: &str,
    width: f32,
    invalid: bool,
) -> Response {
    let caption = ui.label(micro_label(label));
    ui.add_space(Metrics::XS);
    let resp = ui
        .scope(|ui| {
            if invalid {
                mark_invalid(ui);
            }
            ui.add(
                egui::TextEdit::singleline(value)
                    .hint_text(hint)
                    .desired_width(width)
                    .font(glyph_font(12.0))
                    .margin(egui::Margin::symmetric(10.0, 8.0)),
            )
        })
        .inner;
    resp.labelled_by(caption.id)
}

/// Move focus into the first field of a dialog the moment it opens.
fn focus_when_opened(ui: &Ui, resp: &Response) {
    if ui.memory(|memory| memory.focused()).is_none() {
        resp.request_focus();
    }
}

fn enter_pressed(ui: &Ui, resp: &Response) -> bool {
    resp.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
}

/// Paints the scene dialogs; called by `dialog::show_dialogs`.
pub fn show_scene_dialog(ctx: &Context, state: &mut SoundSharedApp, dialog: Dialog) {
    match dialog {
        Dialog::SaveMixAsScene { name, error } => save_mix_dialog(ctx, state, name, error),
        Dialog::EditScene(form) => edit_scene_dialog(ctx, state, *form),
        Dialog::DeleteScene { id } => delete_scene_dialog(ctx, state, id),
        _ => {}
    }
}

fn save_mix_dialog(ctx: &Context, state: &mut SoundSharedApp, name: String, error: Option<String>) {
    let mut name_text = name;
    let playing = state.sounds().iter().filter(|s| s.playing).count();
    let mut submit = false;
    let mut cancel = false;

    modal_sized(ctx, "scene_save_mix_modal", SMALL_W, |ui| {
        header(
            ui,
            "Save Mix As Scene",
            "Keeps the sounds that play now, at their current levels",
        );
        ui.add_space(Metrics::MD);
        let resp = field(
            ui,
            "Scene name",
            &mut name_text,
            "e.g. Tavern",
            f32::INFINITY,
            error.is_some(),
        );
        focus_when_opened(ui, &resp);
        if enter_pressed(ui, &resp) {
            submit = true;
        }
        ui.add_space(Metrics::XS);
        match &error {
            Some(message) => error_line(ui, message),
            None if playing == 0 => error_line(ui, "Nothing is playing: start some sounds first"),
            None => {
                ui.label(micro_label(match playing {
                    1 => "1 sound will be saved".to_string(),
                    n => format!("{n} sounds will be saved"),
                }));
            }
        }
        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::MD);
        ui.horizontal(|ui| {
            let save = ConsoleButton::primary("Save Scene")
                .glyph(glyph::CHECK)
                .width(150.0)
                .enabled(playing > 0)
                .id_salt("dlg_savemix_ok")
                .show(ui);
            if labelled(save, "Save scene").clicked() {
                submit = true;
            }
            ui.add_space(Metrics::SM);
            let close = ConsoleButton::ghost("Cancel")
                .glyph(glyph::CROSS)
                .width(120.0)
                .id_salt("dlg_savemix_cancel")
                .show(ui);
            if labelled(close, "Cancel").clicked() {
                cancel = true;
            }
        });
    });

    if let Dialog::SaveMixAsScene { name, .. } = state.dialog_mut() {
        *name = name_text;
    }
    if cancel {
        state.close_dialog();
    } else if submit && playing > 0 {
        state.submit_save_mix(ctx);
    }
}

fn delete_scene_dialog(ctx: &Context, state: &mut SoundSharedApp, id: SceneId) {
    let (name, count) = state
        .library()
        .scene(id)
        .map(|scene| (scene.name.clone(), scene.items.len()))
        .unwrap_or_else(|| ("this scene".to_string(), 0));
    let mut confirm = false;
    let mut cancel = false;

    modal_sized(ctx, "scene_delete_modal", SMALL_W, |ui| {
        header(ui, "Delete Scene", "This cannot be undone");
        ui.add_space(Metrics::MD);
        ui.add(
            egui::Label::new(settings_kit::note_text(
                format!(
                    "Delete “{name}”? Its {count} sound{} stay in the library; hotkeys that start this scene are removed.",
                    if count == 1 { "" } else { "s" }
                ),
                11.0,
                Palette::ZINC_300,
            ))
            .wrap(),
        );
        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::MD);
        ui.horizontal(|ui| {
            let delete = ConsoleButton::danger("Delete")
                .glyph(glyph::TRASH)
                .width(150.0)
                .id_salt("dlg_scene_delete_ok")
                .show(ui);
            if labelled(delete, "Delete scene").clicked() {
                confirm = true;
            }
            ui.add_space(Metrics::SM);
            let keep = ConsoleButton::ghost("Keep")
                .glyph(glyph::CHECK)
                .width(120.0)
                .id_salt("dlg_scene_delete_keep")
                .show(ui);
            if labelled(keep, "Keep scene").clicked() {
                cancel = true;
            }
        });
    });

    if cancel {
        state.close_dialog();
    } else if confirm {
        state.confirm_delete_scene();
    }
}

fn edit_scene_dialog(ctx: &Context, state: &mut SoundSharedApp, mut form: SceneForm) {
    let library_sounds = form.available_sounds(state.library());
    let errors = if form.attempted {
        form.errors(state.library())
    } else {
        FormErrors::default()
    };
    let list_h = (ctx.screen_rect().height() - 430.0).clamp(90.0, 260.0);
    let is_new = form.id.is_none();

    let mut submit = false;
    let mut cancel = false;
    let mut delete = false;
    let mut add: Option<LibSoundId> = None;
    let mut remove: Option<usize> = None;

    // The form scrolls inside the modal (the footer stays put), so the editor
    // fits the 960x620 minimum window even at a large interface scale.
    let body_max = (settings_kit::modal_max_height(ctx.screen_rect().height())
        - 2.0 * Metrics::PANEL_PAD
        - 90.0)
        .max(160.0);

    modal_sized(ctx, "scene_editor_modal", EDITOR_W, |ui| {
        settings_kit::fit_scroll(ui, egui::Id::new("scene_editor_body"), body_max, |ui| {
            header(
                ui,
                if is_new { "New Scene" } else { "Edit Scene" },
                "Which sounds start, how loud, and how the scene fades",
            );
            ui.add_space(Metrics::MD);

            // ---- Name ----
            let name = field(
                ui,
                "Scene name",
                &mut form.name,
                "e.g. Tavern",
                f32::INFINITY,
                errors.name.is_some(),
            );
            focus_when_opened(ui, &name);
            if enter_pressed(ui, &name) {
                submit = true;
            }
            match &errors.name {
                Some(message) => error_line(ui, message),
                None => {
                    ui.label(micro_label(format!("Up to {MAX_NAME_LEN} characters")));
                }
            }
            ui.add_space(Metrics::MD);

            // ---- Colour ----
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(micro_label("Button colour"));
                    ui.add_space(Metrics::XS);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = Metrics::XS + 2.0;
                        let current = normalize_color_input(&form.color).ok().flatten();
                        for (name, hex) in COLOR_PRESETS {
                            if swatch(ui, name, hex, current.as_deref() == Some(hex)) {
                                form.color = hex.to_string();
                            }
                        }
                        let none = ConsoleButton::ghost("None")
                            .small()
                            .width(56.0)
                            .id_salt("scene_color_none")
                            .show(ui);
                        if labelled(none, "No colour").clicked() {
                            form.color.clear();
                        }
                    });
                });
                ui.add_space(Metrics::LG);
                ui.vertical(|ui| {
                    field(
                        ui,
                        "Hex",
                        &mut form.color,
                        "#a04a4a",
                        96.0,
                        errors.color.is_some(),
                    );
                });
            });
            if let Some(message) = &errors.color {
                error_line(ui, message);
            }
            ui.add_space(Metrics::MD);

            // ---- Fades, curve, stop others ----
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    field(
                        ui,
                        "Fade in (ms)",
                        &mut form.fade_in,
                        "2000",
                        96.0,
                        errors.fade_in.is_some(),
                    );
                });
                ui.vertical(|ui| {
                    field(
                        ui,
                        "Fade out (ms)",
                        &mut form.fade_out,
                        "2000",
                        96.0,
                        errors.fade_out.is_some(),
                    );
                });
                ui.vertical(|ui| {
                    ui.label(micro_label("Curve"));
                    ui.add_space(Metrics::XS);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = Metrics::XS;
                        let equal = form.curve == FadeCurve::EqualPower;
                        if toggle_button(ui, "Equal power", equal, 108.0, "scene_curve_eq") {
                            form.curve = FadeCurve::EqualPower;
                        }
                        let linear = form.curve == FadeCurve::Linear;
                        if toggle_button(ui, "Linear", linear, 70.0, "scene_curve_lin") {
                            form.curve = FadeCurve::Linear;
                        }
                    });
                });
            });
            for message in [&errors.fade_in, &errors.fade_out].into_iter().flatten() {
                error_line(ui, message);
            }
            ui.add_space(Metrics::SM);
            let stop_label = if form.stop_others {
                "Stop other sounds: on"
            } else {
                "Stop other sounds: off"
            };
            if toggle_button(ui, stop_label, form.stop_others, 190.0, "scene_stop_others") {
                form.stop_others = !form.stop_others;
            }
            ui.add_space(Metrics::MD);
            hairline(ui);
            ui.add_space(Metrics::MD);

            // ---- Sounds ----
            ui.horizontal(|ui| {
                ui.label(micro_label(match form.items.len() {
                    1 => "1 sound".to_string(),
                    n => format!("{n} sounds"),
                }));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("scene_add_sound")
                        .selected_text("Add sound…")
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            if library_sounds.is_empty() {
                                ui.label(micro_label("Every library sound is here"));
                            }
                            for (id, label) in &library_sounds {
                                if ui.selectable_label(false, elide(label, 34)).clicked() {
                                    add = Some(*id);
                                }
                            }
                        });
                });
            });
            ui.add_space(Metrics::SM);

            if form.items.is_empty() {
                settings_kit::empty_state(
                    ui,
                    "No sounds in this scene yet",
                    "Use Add sound above to choose what starts when the scene is activated.",
                );
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("scene_items_scroll")
                    .max_height(list_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(Metrics::SM, Metrics::XS + 2.0);
                        for (index, item) in form.items.iter_mut().enumerate() {
                            let delay_bad = errors.delays.get(index).is_some_and(Option::is_some);
                            if item_row(ui, index, item, delay_bad) {
                                remove = Some(index);
                            }
                        }
                    });
            }
            for (index, message) in errors.delays.iter().enumerate() {
                if let (Some(message), Some(item)) = (message, form.items.get(index)) {
                    error_line(
                        ui,
                        &format!("Delay of {}: {message}", elide(&item.name, 24)),
                    );
                }
            }
            if let Some(message) = &errors.sounds {
                error_line(ui, message);
            }

            // EXTENSION: the emitter and playlist editors of a scene plug in here
            // (a separate dialog opened from a button in this spot). Emitters and
            // the playlist are not part of `SceneForm`; `apply_scene` leaves them
            // untouched, so nothing above has to change to add them.
        });

        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::MD);
        ui.horizontal(|ui| {
            let save = ConsoleButton::primary("Save Scene")
                .glyph(glyph::CHECK)
                .width(150.0)
                .id_salt("dlg_scene_save")
                .show(ui);
            if labelled(save, "Save scene").clicked() {
                submit = true;
            }
            ui.add_space(Metrics::SM);
            let close = ConsoleButton::ghost("Cancel")
                .glyph(glyph::CROSS)
                .width(120.0)
                .id_salt("dlg_scene_cancel")
                .show(ui);
            if labelled(close, "Cancel").clicked() {
                cancel = true;
            }
            if !is_new {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let del = ConsoleButton::danger("Delete")
                        .glyph(glyph::TRASH)
                        .width(110.0)
                        .id_salt("dlg_scene_delete")
                        .show(ui);
                    if labelled(del, "Delete scene").clicked() {
                        delete = true;
                    }
                });
            }
        });
    });

    if let Some(index) = remove {
        form.remove_item(index);
    }
    if let Some(sound) = add {
        form.add_sound(state.library(), sound);
    }
    // Keep what was typed across the frame.
    let id = form.id;
    if let Dialog::EditScene(slot) = state.dialog_mut() {
        **slot = form;
    }
    if cancel {
        state.close_dialog();
    } else if delete {
        if let Some(id) = id {
            state.open_delete_scene(id);
        }
    } else if submit {
        state.submit_scene_form();
    }
}

/// A colour swatch; true when used.
fn swatch(ui: &mut Ui, name: &str, hex: &str, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    let resp = resp
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(name);
    if resp.clicked() {
        resp.request_focus();
    }
    let rgb = parse_hex_rgb(hex).unwrap_or([0x71, 0x71, 0x7A]);
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(Metrics::R_SM), to_color32(rgb));
    if selected {
        painter.rect_stroke(
            rect.expand(2.0),
            Rounding::same(Metrics::R_SM + 1.0),
            Stroke::new(2.0_f32, Palette::WHITE),
        );
        let mark = if relative_luminance(rgb) > 0.4 {
            Palette::BLACK
        } else {
            Palette::WHITE
        };
        paint_icon(painter, glyph::CHECK, rect.center(), 13.0, mark);
    } else if resp.has_focus() || resp.hovered() {
        painter.rect_stroke(
            rect.expand(1.0),
            Rounding::same(Metrics::R_SM),
            Stroke::new(1.0_f32, Palette::ROSE_400),
        );
    }
    resp.widget_info(|| {
        WidgetInfo::selected(WidgetType::Button, true, selected, format!("Colour {name}"))
    });
    resp.clicked() || resp.fake_primary_click
}

/// One sound row of the editor; true when its remove button was used.
fn item_row(ui: &mut Ui, index: usize, item: &mut ItemForm, delay_invalid: bool) -> bool {
    let mut remove = false;
    ui.push_id(("scene_item", index), |ui| {
        ui.horizontal(|ui| {
            ui.add_sized(
                [150.0, 24.0],
                egui::Label::new(
                    RichText::new(item.name.as_str())
                        .font(ui_font(13.0))
                        .color(Palette::WHITE),
                )
                .truncate(),
            )
            .on_hover_text(item.name.as_str());

            let slider = level_slider(ui, &mut item.volume, 90.0, "scene_item_volume");
            slider.widget_info(|| {
                WidgetInfo::slider(
                    true,
                    f64::from(item.volume),
                    format!("Level of {}, {:.0}%", item.name, item.volume * 100.0),
                )
            });
            ui.add_sized(
                [36.0, 24.0],
                egui::Label::new(
                    RichText::new(format!("{:.0}%", item.volume * 100.0))
                        .font(glyph_font(10.0))
                        .color(Palette::ZINC_400),
                ),
            );

            if toggle_button(ui, "Loop", item.looped, 60.0, "item_loop") {
                item.looped = !item.looped;
            }
            let start_label = if item.manual { "Manual" } else { "Auto" };
            let start = ConsoleButton::new(start_label)
                .kind(if item.manual {
                    BtnKind::Armed
                } else {
                    BtnKind::Ghost
                })
                .small()
                .width(72.0)
                .id_salt("item_start")
                .tooltip("Auto starts with the scene; Manual only when you start it by hand")
                .show(ui);
            start.widget_info(|| {
                WidgetInfo::labeled(
                    WidgetType::Button,
                    true,
                    format!("Start mode of {}: {start_label}", item.name),
                )
            });
            if start.clicked() {
                item.manual = !item.manual;
            }

            crate::gui::pan_ui::pan_slider(
                ui,
                &mut item.pan,
                64.0,
                "Pan in this scene; centre uses the sound's own pan",
            );

            ui.add_enabled_ui(!item.manual, |ui| {
                ui.scope(|ui| {
                    if delay_invalid {
                        mark_invalid(ui);
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut item.delay)
                            .desired_width(54.0)
                            .hint_text("0")
                            .font(glyph_font(11.0))
                            .margin(egui::Margin::symmetric(6.0, 5.0)),
                    )
                    .on_hover_text("Start delay in ms (or seconds, e.g. 1.5 s)");
                });
            });

            let del = IconButton::new(glyph::CROSS)
                .size(22.0)
                .font_size(12.0)
                .tooltip("Remove from scene")
                .id_salt("item_remove")
                .show(ui);
            if labelled(del, &format!("Remove {} from scene", item.name)).clicked() {
                remove = true;
            }
        });
    });
    remove
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn library() -> Library {
        let mut lib = Library::new();
        for (path, name, volume) in [
            ("/fake/rain.ogg", "Rain", 0.6),
            ("/fake/fire.ogg", "Fire", 0.8),
            ("/fake/door.wav", "", 1.0),
        ] {
            assert!(lib.insert_sound(Path::new(path), name, volume, None));
        }
        lib
    }

    fn sound(lib: &Library, path: &str) -> LibSoundId {
        lib.id_of(Path::new(path)).unwrap()
    }

    fn named_form(lib: &Library, name: &str) -> SceneForm {
        let mut form = SceneForm::blank(lib);
        form.name = name.to_string();
        form
    }

    // ---- parsing ----

    #[test]
    fn millis_accept_plain_ms_suffixes_and_seconds() {
        assert_eq!(parse_millis("2000", MAX_FADE_MS), Ok(2000));
        assert_eq!(parse_millis(" 2000 ms ", MAX_FADE_MS), Ok(2000));
        assert_eq!(parse_millis("2.5 s", MAX_FADE_MS), Ok(2500));
        assert_eq!(parse_millis("2.5S", MAX_FADE_MS), Ok(2500));
        assert_eq!(parse_millis("0", MAX_FADE_MS), Ok(0));
        assert_eq!(parse_millis("60 s", MAX_FADE_MS), Ok(60_000));
    }

    #[test]
    fn millis_reject_garbage_negatives_and_overflow() {
        for bad in [
            "", "  ", "abc", "-5", "-1 s", "1.5", "1,5 s", "nan s", "inf s", "s", "ms",
        ] {
            assert!(parse_millis(bad, MAX_FADE_MS).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse_millis("60001", MAX_FADE_MS),
            Err("At most 60 s".into())
        );
        assert!(parse_millis("61 s", MAX_FADE_MS).is_err());
        assert!(parse_millis("99999999999", MAX_FADE_MS).is_err());
    }

    #[test]
    fn seconds_are_shown_with_one_decimal() {
        assert_eq!(format_seconds(2000), "2.0 s");
        assert_eq!(format_seconds(300), "0.3 s");
        assert_eq!(format_millis(1500), "1500");
    }

    #[test]
    fn hex_colours_parse_short_and_long() {
        assert_eq!(parse_hex_rgb("#a04a4a"), Some([0xa0, 0x4a, 0x4a]));
        assert_eq!(parse_hex_rgb(" #FFF "), Some([255, 255, 255]));
        assert_eq!(parse_hex_rgb("#f80"), Some([0xff, 0x88, 0x00]));
        for bad in ["a04a4a", "#12", "#1234", "#ggg", "", "#"] {
            assert_eq!(parse_hex_rgb(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn colour_field_normalizes_or_explains() {
        assert_eq!(normalize_color_input(""), Ok(None));
        assert_eq!(normalize_color_input("   "), Ok(None));
        assert_eq!(normalize_color_input("A04A4A"), Ok(Some("#a04a4a".into())));
        assert_eq!(normalize_color_input("#ABC"), Ok(Some("#abc".into())));
        assert!(normalize_color_input("red").is_err());
        assert!(normalize_color_input("#12345").is_err());
    }

    #[test]
    fn dark_scene_colours_are_lightened_to_stay_visible() {
        let black = scene_accent(Some("#000000"));
        assert!(contrast_ratio(black, CARD_BG) >= 3.0);
        let navy = scene_accent(Some("#101040"));
        assert!(contrast_ratio(navy, CARD_BG) >= 3.0);
        // A colour that is already visible is not touched.
        assert_eq!(scene_accent(Some("#ffffff")), [255, 255, 255]);
        // No colour (or a broken one) falls back to the console rose.
        assert_eq!(scene_accent(None), DEFAULT_ACCENT);
        assert_eq!(scene_accent(Some("nonsense")), DEFAULT_ACCENT);
    }

    #[test]
    fn contrast_ratio_matches_the_wcag_extremes() {
        assert!((contrast_ratio([0, 0, 0], [255, 255, 255]) - 21.0).abs() < 0.01);
        assert!((contrast_ratio([9, 9, 9], [9, 9, 9]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn every_preset_swatch_is_a_valid_visible_colour() {
        for (name, hex) in COLOR_PRESETS {
            let rgb = parse_hex_rgb(hex).unwrap_or_else(|| panic!("{name} {hex}"));
            assert!(contrast_ratio(visible_accent(rgb, CARD_BG), CARD_BG) >= 3.0);
            assert_eq!(normalize_color_input(hex), Ok(Some(hex.to_string())));
        }
    }

    // ---- names ----

    #[test]
    fn scene_names_are_trimmed_unique_and_bounded() {
        let mut lib = library();
        let tavern = lib.add_scene("Tavern").unwrap();
        assert_eq!(
            validate_scene_name(&lib, "  Forest ", None),
            Ok("Forest".into())
        );
        assert!(validate_scene_name(&lib, "   ", None).is_err());
        assert_eq!(
            validate_scene_name(&lib, "TAVERN", None),
            Err("A scene with this name already exists".into())
        );
        // Renaming a scene to its own name is fine.
        assert_eq!(
            validate_scene_name(&lib, "tavern", Some(tavern)),
            Ok("tavern".into())
        );
        let long = "x".repeat(MAX_NAME_LEN + 1);
        assert!(validate_scene_name(&lib, &long, None).is_err());
        assert!(validate_scene_name(&lib, &"x".repeat(MAX_NAME_LEN), None).is_ok());
    }

    #[test]
    fn suggested_and_copy_names_are_free() {
        let mut lib = library();
        assert_eq!(suggest_scene_name(&lib), "Scene 1");
        lib.add_scene("Scene 1").unwrap();
        assert_eq!(suggest_scene_name(&lib), "Scene 2");
        lib.add_scene("Scene 2").unwrap();
        lib.add_scene("Scene 3").unwrap();
        assert_eq!(suggest_scene_name(&lib), "Scene 4");

        assert_eq!(copy_name(&lib, "Scene 1"), "Scene 1 copy");
        lib.add_scene("Scene 1 copy").unwrap();
        assert_eq!(copy_name(&lib, "Scene 1"), "Scene 1 copy 2");
        let long = "y".repeat(MAX_NAME_LEN);
        assert!(copy_name(&lib, &long).chars().count() <= MAX_NAME_LEN);
    }

    #[test]
    fn a_sound_without_a_name_is_labelled_by_its_file() {
        let lib = library();
        let door = lib.sound(Path::new("/fake/door.wav")).unwrap();
        assert_eq!(sound_label(door), "door.wav");
        let rain = lib.sound(Path::new("/fake/rain.ogg")).unwrap();
        assert_eq!(sound_label(rain), "Rain");
    }

    // ---- form <-> scene ----

    #[test]
    fn blank_form_carries_the_schema_defaults() {
        let lib = library();
        let form = SceneForm::blank(&lib);
        assert_eq!(form.id, None);
        assert_eq!(form.name, "Scene 1");
        assert_eq!(
            (form.fade_in.as_str(), form.fade_out.as_str()),
            ("2000", "2000")
        );
        assert_eq!(form.curve, FadeCurve::EqualPower);
        assert!(form.stop_others);
        assert!(!form.attempted);
        assert!(form.validate(&lib).is_ok());
    }

    #[test]
    fn form_round_trips_a_scene() {
        let mut lib = library();
        let rain = sound(&lib, "/fake/rain.ogg");
        let fire = sound(&lib, "/fake/fire.ogg");
        let id = lib.add_scene("Tavern").unwrap();
        lib.set_scene_color(id, Some("#a04a4a"));
        lib.set_scene_fades(id, 1500, 800);
        lib.set_scene_curve(id, FadeCurve::Linear);
        lib.set_scene_stop_others(id, false);
        lib.set_item(
            id,
            SceneItem {
                sound: rain,
                volume: 0.4,
                looped: true,
                start: SceneStart::OnEnter,
                delay_ms: 0,
                pan: 0.0,
            },
        );
        lib.set_item(
            id,
            SceneItem {
                sound: fire,
                volume: 0.9,
                looped: false,
                start: SceneStart::Manual,
                delay_ms: 2500,
                pan: -0.5,
            },
        );
        let scene = lib.scene(id).unwrap().clone();

        let form = SceneForm::from_scene(&lib, &scene);
        assert_eq!(form.id, Some(id));
        assert_eq!(form.color, "#a04a4a");
        assert_eq!(form.fade_in, "1500");
        assert_eq!(form.items[0].name, "Rain");
        assert!(form.items[1].manual);
        assert_eq!(form.items[1].delay, "2500");
        assert_eq!((form.items[0].pan, form.items[1].pan), (0.0, -0.5));

        let valid = form.validate(&lib).expect("a scene's own form is valid");
        assert_eq!(valid.name, "Tavern");
        assert_eq!(valid.color.as_deref(), Some("#a04a4a"));
        assert_eq!((valid.fade_in_ms, valid.fade_out_ms), (1500, 800));
        assert_eq!(valid.curve, FadeCurve::Linear);
        assert!(!valid.stop_others);
        assert_eq!(valid.items, scene.items);

        // Applying it back changes nothing.
        let mut copy = lib.clone();
        assert_eq!(apply_scene(&mut copy, Some(id), &valid), Ok(id));
        assert_eq!(copy.scene(id), lib.scene(id));
    }

    #[test]
    fn validation_reports_every_bad_field_at_once() {
        let mut lib = library();
        lib.add_scene("Taken").unwrap();
        let mut form = named_form(&lib, "taken");
        form.color = "blue".into();
        form.fade_in = "soon".into();
        form.fade_out = "-1".into();
        assert!(form.add_sound(&lib, sound(&lib, "/fake/rain.ogg")));
        form.items[0].delay = "later".into();

        let errors = form.validate(&lib).unwrap_err();
        assert!(errors.name.is_some());
        assert!(errors.color.is_some());
        assert!(errors.fade_in.is_some());
        assert!(errors.fade_out.is_some());
        assert_eq!(errors.delays.len(), 1);
        assert!(errors.delays[0].is_some());
        assert!(!errors.is_empty());

        form.items[0].manual = true;
        assert!(
            form.errors(&lib).delays[0].is_none(),
            "a manual item ignores its delay"
        );
    }

    #[test]
    fn editing_a_scene_may_keep_its_own_name() {
        let mut lib = library();
        let id = lib.add_scene("Tavern").unwrap();
        let scene = lib.scene(id).unwrap().clone();
        let form = SceneForm::from_scene(&lib, &scene);
        assert!(form.errors(&lib).name.is_none());
    }

    #[test]
    fn a_sound_removed_from_the_library_blocks_saving_with_a_reason() {
        let lib = library();
        let mut form = SceneForm::blank(&lib);
        form.add_sound(&lib, sound(&lib, "/fake/rain.ogg"));
        let mut smaller = lib.clone();
        smaller.remove_sound(Path::new("/fake/rain.ogg"));
        let errors = form.validate(&smaller).unwrap_err();
        assert!(errors.sounds.unwrap().contains("Rain"));
    }

    #[test]
    fn add_and_remove_rows() {
        let lib = library();
        let rain = sound(&lib, "/fake/rain.ogg");
        let mut form = SceneForm::blank(&lib);
        assert_eq!(form.available_sounds(&lib).len(), 3);
        assert!(form.add_sound(&lib, rain));
        assert!(!form.add_sound(&lib, rain), "a sound appears once");
        assert!(!form.add_sound(&lib, uuid::Uuid::new_v4()), "unknown sound");
        assert_eq!(form.available_sounds(&lib).len(), 2);
        // A new row uses the sound's own level and repeat flag.
        assert!((form.items[0].volume - 0.6).abs() < 1e-6);
        assert!(!form.items[0].manual);
        assert!(form.remove_item(0));
        assert!(!form.remove_item(0));
        assert!(form.items.is_empty());
    }

    // ---- applying ----

    #[test]
    fn applying_a_new_form_creates_the_scene_in_form_order() {
        let mut lib = library();
        let rain = sound(&lib, "/fake/rain.ogg");
        let fire = sound(&lib, "/fake/fire.ogg");
        let mut form = named_form(&lib, " Camp ");
        form.color = "A04A4A".into();
        form.fade_in = "1.5 s".into();
        form.fade_out = "500".into();
        form.curve = FadeCurve::Linear;
        form.stop_others = false;
        form.add_sound(&lib, fire);
        form.add_sound(&lib, rain);
        form.items[1].volume = 0.25;
        form.items[1].looped = true;
        form.items[0].delay = "1 s".into();

        let valid = form.validate(&lib).unwrap();
        let id = apply_scene(&mut lib, None, &valid).unwrap();
        let scene = lib.scene(id).unwrap();
        assert_eq!(scene.name, "Camp");
        assert_eq!(scene.color.as_deref(), Some("#a04a4a"));
        assert_eq!((scene.fade_in_ms, scene.fade_out_ms), (1500, 500));
        assert_eq!(scene.curve, FadeCurve::Linear);
        assert!(!scene.stop_others);
        assert_eq!(scene.items.len(), 2);
        assert_eq!(scene.items[0].sound, fire);
        assert_eq!(scene.items[0].delay_ms, 1000);
        assert_eq!(scene.items[1].sound, rain);
        assert!(scene.items[1].looped);
        assert!((scene.items[1].volume - 0.25).abs() < 1e-6);
    }

    #[test]
    fn editing_removes_adds_and_reorders_items_but_keeps_emitters_and_playlist() {
        let mut lib = library();
        let rain = sound(&lib, "/fake/rain.ogg");
        let fire = sound(&lib, "/fake/fire.ogg");
        let door = sound(&lib, "/fake/door.wav");
        let id = lib.add_scene("Tavern").unwrap();
        lib.set_item(id, SceneItem::new(rain));
        lib.set_item(id, SceneItem::new(fire));
        assert!(lib.set_emitters(id, vec![crate::gui::groups::Emitter::new(vec![door])]));
        let playlist = lib.add_playlist("Music").unwrap();
        assert!(lib.set_scene_playlist(id, Some(playlist)));

        let scene = lib.scene(id).unwrap().clone();
        let mut form = SceneForm::from_scene(&lib, &scene);
        form.remove_item(0); // rain out
        form.add_sound(&lib, door); // door in, after fire
        form.items.swap(0, 1); // door first
        let valid = form.validate(&lib).unwrap();
        apply_scene(&mut lib, Some(id), &valid).unwrap();

        let after = lib.scene(id).unwrap();
        let order: Vec<_> = after.items.iter().map(|i| i.sound).collect();
        assert_eq!(order, vec![door, fire]);
        assert_eq!(after.emitters.len(), 1, "emitters are untouched");
        assert_eq!(after.playlist, Some(playlist), "the playlist is untouched");
    }

    #[test]
    fn a_refused_apply_changes_nothing() {
        let mut lib = library();
        let id = lib.add_scene("Tavern").unwrap();
        lib.add_scene("Forest").unwrap();
        let before = lib.clone();

        let clash = ValidScene {
            name: "Forest".into(),
            color: None,
            fade_in_ms: 1,
            fade_out_ms: 1,
            curve: FadeCurve::Linear,
            stop_others: false,
            items: vec![],
        };
        assert!(apply_scene(&mut lib, Some(id), &clash).is_err());
        assert_eq!(lib.scenes(), before.scenes());

        let ghost = ValidScene {
            name: "Ghost".into(),
            items: vec![SceneItem::new(uuid::Uuid::new_v4())],
            ..clash.clone()
        };
        assert!(apply_scene(&mut lib, None, &ghost).is_err());
        assert_eq!(lib.scenes(), before.scenes(), "no half-made scene is left");
        assert!(apply_scene(&mut lib, Some(uuid::Uuid::new_v4()), &clash).is_err());
    }

    #[test]
    fn duplicate_copies_everything_next_to_the_original() {
        let mut lib = library();
        let rain = sound(&lib, "/fake/rain.ogg");
        let door = sound(&lib, "/fake/door.wav");
        let first = lib.add_scene("Tavern").unwrap();
        let last = lib.add_scene("Storm").unwrap();
        lib.set_item(first, SceneItem::new(rain));
        lib.set_scene_color(first, Some("#2b6cb0"));
        lib.set_scene_fades(first, 100, 200);
        lib.set_scene_curve(first, FadeCurve::Linear);
        lib.set_emitters(first, vec![crate::gui::groups::Emitter::new(vec![door])]);

        let copy = duplicate_scene(&mut lib, first).unwrap();
        let names: Vec<_> = lib.scenes().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Tavern", "Tavern copy", "Storm"]);
        assert_ne!(copy, first);
        assert_eq!(lib.scenes()[2].id, last);
        let (a, b) = (lib.scene(first).unwrap(), lib.scene(copy).unwrap());
        assert_eq!(b.color, a.color);
        assert_eq!((b.fade_in_ms, b.fade_out_ms), (100, 200));
        assert_eq!(b.curve, a.curve);
        assert_eq!(b.items, a.items);
        assert_eq!(b.emitters, a.emitters);
        assert_eq!(duplicate_scene(&mut lib, uuid::Uuid::new_v4()), None);
    }

    #[test]
    fn add_to_scene_uses_the_sounds_own_level_and_is_idempotent() {
        let mut lib = library();
        let rain = sound(&lib, "/fake/rain.ogg");
        lib.set_looped(Path::new("/fake/rain.ogg"), true);
        let id = lib.add_scene("Tavern").unwrap();
        assert_eq!(add_sound_to_scene(&mut lib, id, rain), AddToScene::Added);
        let item = &lib.scene(id).unwrap().items[0];
        assert!((item.volume - 0.6).abs() < 1e-6);
        assert!(item.looped);
        assert_eq!(
            add_sound_to_scene(&mut lib, id, rain),
            AddToScene::AlreadyPresent
        );
        assert_eq!(lib.scene(id).unwrap().items.len(), 1);
        assert_eq!(
            add_sound_to_scene(&mut lib, uuid::Uuid::new_v4(), rain),
            AddToScene::UnknownScene
        );
        assert_eq!(
            add_sound_to_scene(&mut lib, id, uuid::Uuid::new_v4()),
            AddToScene::UnknownSound
        );
    }

    // ---- view models ----

    #[test]
    fn cards_mark_exactly_the_active_scene() {
        let mut lib = library();
        let a = lib.add_scene("A").unwrap();
        let b = lib.add_scene("B").unwrap();
        lib.set_item(b, SceneItem::new(sound(&lib, "/fake/rain.ogg")));
        let cards = scene_cards(&lib, Some(&b.to_string()));
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].id, a);
        assert!(!cards[0].active);
        assert!(cards[1].active);
        assert_eq!(cards[1].sounds, 1);
        assert!(scene_cards(&lib, None).iter().all(|c| !c.active));
        assert!(scene_cards(&lib, Some("not-an-id"))
            .iter()
            .all(|c| !c.active));
    }

    #[test]
    fn sublines_state_the_state_in_words() {
        let mut card = SceneCard {
            id: uuid::Uuid::nil(),
            name: "A".into(),
            accent: DEFAULT_ACCENT,
            sounds: 1,
            fade_in_ms: 2000,
            active: false,
            warnings: Vec::new(),
        };
        assert_eq!(card_subline(&card, None, false), "1 SOUND · FADE 2.0 S");
        card.sounds = 4;
        assert_eq!(
            card_subline(&card, Some(0.5), false),
            "4 SOUNDS · FADE 2.0 S"
        );
        card.active = true;
        assert_eq!(card_subline(&card, None, false), "ACTIVE");
        assert_eq!(
            card_subline(&card, Some(0.62), false),
            "ACTIVE · FADING IN 62%"
        );
        assert_eq!(
            card_subline(&card, Some(0.62), true),
            "ACTIVE · FADING IN",
            "reduced motion drops the moving number"
        );
    }

    #[test]
    fn stinger_pads_are_the_loaded_flagged_sounds_in_board_order() {
        let mut lib = library();
        lib.set_oneshot_stinger(Path::new("/fake/door.wav"), true);
        lib.set_oneshot_stinger(Path::new("/fake/rain.ogg"), true);
        let entry = |path: &str, name: &str| SoundEntry {
            id: EngineSoundId::new(),
            name: name.into(),
            path: path.into(),
            playing: false,
            volume: 1.0,
            looped: false,
            group: None,
            duration: 1.0,
            sample_rate: 48_000,
        };
        let sounds = vec![
            entry("/fake/rain.ogg", "Rain"),
            entry("/fake/fire.ogg", "Fire"),
            entry("/fake/door.wav", "Door"),
            entry("/not/in/library.wav", "Stranger"),
        ];
        let pads = stinger_pads(&lib, &sounds);
        let names: Vec<_> = pads.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Rain", "Door"]);
        assert_eq!(pads[0].id, sounds[0].id);
    }

    #[test]
    fn grid_fills_the_row_without_overflowing() {
        assert_eq!(grid_columns(100.0, 150.0, 8.0), 1);
        assert_eq!(grid_columns(150.0, 150.0, 8.0), 1);
        assert_eq!(grid_columns(316.0, 150.0, 8.0), 2);
        assert_eq!(grid_columns(307.0, 150.0, 8.0), 1);
        assert_eq!(grid_columns(800.0, 150.0, 8.0), 5);
        for width in [150.0_f32, 333.0, 500.0, 817.0, 1024.0] {
            let cols = grid_columns(width, 150.0, 8.0);
            let each = card_width(width, cols, 8.0);
            assert!(each >= 149.0 || cols == 1);
            assert!(each * cols as f32 + 8.0 * (cols as f32 - 1.0) <= width);
        }
        assert_eq!(card_width(0.0, 0, 8.0), 1.0);
    }

    // ---- transitions ----

    #[test]
    fn slider_snaps_to_steps_and_round_trips() {
        assert_eq!(slider_to_ms(0.0), 0);
        assert_eq!(slider_to_ms(1.0), TRANSITION_MAX_MS);
        assert_eq!(slider_to_ms(0.2), 2000);
        assert_eq!(slider_to_ms(0.2049), 2000);
        assert_eq!(slider_to_ms(0.2051), 2100);
        assert_eq!(slider_to_ms(-3.0), 0);
        assert_eq!(slider_to_ms(9.0), TRANSITION_MAX_MS);
        for ms in (0..=TRANSITION_MAX_MS).step_by(TRANSITION_STEP_MS as usize) {
            assert_eq!(slider_to_ms(ms_to_slider(ms)), ms);
        }
        assert_eq!(ms_to_slider(99_999), 1.0);
    }

    #[test]
    fn progress_is_clamped_and_instant_for_zero_fades() {
        assert_eq!(transition_progress(Duration::from_secs(5), 0), 1.0);
        assert_eq!(transition_progress(Duration::ZERO, 2000), 0.0);
        assert!((transition_progress(Duration::from_millis(500), 2000) - 0.25).abs() < 1e-6);
        assert_eq!(transition_progress(Duration::from_secs(9), 2000), 1.0);
    }

    #[test]
    fn the_shared_slider_overrides_scene_fades_only_when_set() {
        assert_eq!(effective_fade_ms(None, 3000), 3000);
        assert_eq!(effective_fade_ms(Some(500), 3000), 500);

        let mut scenes = vec![
            SceneConfig {
                fade_in_ms: 100,
                fade_out_ms: 200,
                ..Default::default()
            },
            SceneConfig::default(),
        ];
        patch_scene_fades(&mut scenes, 750);
        assert!(scenes
            .iter()
            .all(|s| s.fade_in_ms == 750 && s.fade_out_ms == 750));

        let active = SceneConfig {
            fade_out_ms: 4000,
            ..Default::default()
        };
        assert_eq!(stop_all_fade_ms(Some(900), Some(&active)), 900);
        assert_eq!(stop_all_fade_ms(None, Some(&active)), 4000);
        assert_eq!(stop_all_fade_ms(None, None), DEFAULT_STOP_ALL_FADE_MS);
    }

    #[test]
    fn patching_the_director_changes_the_fades_it_will_use() {
        let mut director = crate::audio::Director::new();
        director.set_scenes(vec![SceneConfig {
            id: "11111111-1111-1111-1111-111111111111".into(),
            name: "A".into(),
            ..Default::default()
        }]);
        patch_director(&mut director, None);
        assert_eq!(director.scenes()[0].fade_in_ms, 2000);
        patch_director(&mut director, Some(300));
        assert_eq!(director.scenes()[0].fade_in_ms, 300);
        assert_eq!(director.scenes()[0].fade_out_ms, 300);
    }

    #[test]
    fn override_follows_the_auto_switch() {
        let mut ui = SceneUi::default();
        assert_eq!(
            ui.override_ms(),
            None,
            "scenes use their own fades by default"
        );
        ui.auto_fade = false;
        ui.transition_ms = 1200;
        assert_eq!(ui.override_ms(), Some(1200));
    }

    #[test]
    fn a_new_active_scene_starts_a_progress_that_ends() {
        let mut ui = SceneUi::default();
        let t0 = Instant::now();
        assert!(ui.progress(t0).is_none());

        ui.observe_active(Some("scene-a"), t0, |_| 2000);
        let (scene, p) = ui.progress(t0).unwrap();
        assert_eq!(scene, "scene-a");
        assert_eq!(p, 0.0);
        let (_, p) = ui.progress(t0 + Duration::from_millis(1000)).unwrap();
        assert!((p - 0.5).abs() < 1e-3);
        assert!(ui.progress(t0 + Duration::from_millis(2000)).is_none());

        // The same scene again does not restart the animation, and the closure
        // is not even asked.
        ui.observe_active(Some("scene-a"), t0 + Duration::from_millis(500), |_| {
            panic!("unchanged scene")
        });
        assert!(ui.progress(t0 + Duration::from_millis(500)).is_some());

        // Another scene replaces it; no scene clears it; a zero fade shows none.
        ui.observe_active(Some("scene-b"), t0, |_| 1000);
        assert_eq!(ui.progress(t0).unwrap().0, "scene-b");
        ui.observe_active(None, t0, |_| 1000);
        assert!(ui.progress(t0).is_none());
        ui.observe_active(Some("scene-c"), t0, |_| 0);
        assert!(ui.progress(t0).is_none());
    }

    #[test]
    fn wakeups_are_frequent_only_while_animating_and_not_with_reduced_motion() {
        let mut ui = SceneUi::default();
        let t0 = Instant::now();
        assert_eq!(ui.wakeup(t0, false), None, "an idle deck needs no frames");

        ui.observe_active(Some("a"), t0, |_| 5000);
        assert_eq!(ui.wakeup(t0, false), Some(ANIMATION_FRAME));
        assert_eq!(
            ui.wakeup(t0, true),
            Some(Duration::from_millis(5000)),
            "reduced motion wakes once, when the fade ends"
        );

        ui.notify("hello", false, t0);
        assert_eq!(ui.wakeup(t0, true), Some(NOTICE_INFO_HOLD));
    }

    // ---- painting smoke test ----

    /// The painted widgets run for a few headless frames without panicking and
    /// report clicks as the actions they stand for.
    #[test]
    fn painted_widgets_run_headless_and_report_actions() {
        let ctx = egui::Context::default();
        let cards: Vec<SceneCard> = (0..4)
            .map(|n| SceneCard {
                id: uuid::Uuid::from_u128(n + 1),
                name: format!("Scene number {n} with a rather long name"),
                accent: scene_accent(Some("#101040")),
                sounds: n as usize,
                fade_in_ms: 2000,
                active: n == 1,
                warnings: if n == 2 {
                    vec!["Sound X is missing its file".into()]
                } else {
                    Vec::new()
                },
            })
            .collect();
        let mut item = ItemForm {
            sound: uuid::Uuid::nil(),
            name: "A long sound name that has to be truncated nicely".into(),
            volume: 0.5,
            looped: false,
            manual: false,
            delay: "oops".into(),
            pan: 0.0,
        };
        let mut text = String::from("#a04a4a");
        let mut removed = false;
        let mut actions = Vec::new();
        for frame in 0..3 {
            let input = egui::RawInput {
                time: Some(f64::from(frame) / 60.0),
                screen_rect: Some(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    Vec2::new(900.0, 700.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for card in &cards {
                            let fade = card.active.then_some(0.4);
                            scene_button(ui, card, 160.0, fade, frame == 1, &mut actions);
                        }
                    });
                    removed |= item_row(ui, 0, &mut item, true);
                    field(ui, "Hex", &mut text, "#a04a4a", 96.0, true);
                    swatch(ui, "Crimson", "#a04a4a", true);
                    toggle_button(ui, "Loop", true, 60.0, "t");
                });
            });
        }
        assert!(actions.is_empty(), "nothing was clicked");
        assert!(!removed);
    }

    // ---- replies ----

    #[test]
    fn replies_are_collected_without_blocking_and_errors_become_notices() {
        let mut ui = SceneUi::default();
        let now = Instant::now();

        let (tx, rx) = bounded::<anyhow::Result<()>>(1);
        ui.expect_reply("Activate “Tavern”".into(), Reply::Unit(rx));
        ui.poll(now);
        assert_eq!(
            ui.pending.len(),
            1,
            "no reply yet: keep waiting, do not block"
        );
        assert!(ui.notice().is_none());

        tx.send(Err(anyhow::anyhow!("Engine not running"))).unwrap();
        ui.poll(now);
        assert!(ui.pending.is_empty());
        let notice = ui.notice().unwrap();
        assert!(notice.error);
        assert_eq!(notice.text, "Activate “Tavern”: Engine not running");

        // A successful acknowledgement is silent.
        let mut quiet = SceneUi::default();
        let (tx, rx) = bounded::<anyhow::Result<()>>(1);
        quiet.expect_reply("Stop scene".into(), Reply::Unit(rx));
        tx.send(Ok(())).unwrap();
        quiet.poll(now);
        assert!(quiet.pending.is_empty() && quiet.notice().is_none());
    }

    #[test]
    fn a_saved_snapshot_is_confirmed_by_name_and_a_failed_one_explained() {
        let now = Instant::now();
        let mut ui = SceneUi::default();
        let (tx, rx) = bounded::<anyhow::Result<SceneConfig>>(1);
        ui.expect_reply("Save mix as “Camp”".into(), Reply::Scene(rx));
        tx.send(Ok(SceneConfig {
            name: "Camp".into(),
            ..Default::default()
        }))
        .unwrap();
        ui.poll(now);
        let notice = ui.notice().unwrap();
        assert!(!notice.error);
        assert_eq!(notice.text, "Saved scene “Camp”");

        let (tx, rx) = bounded::<anyhow::Result<SceneConfig>>(1);
        ui.expect_reply("Save mix as “Camp 2”".into(), Reply::Scene(rx));
        tx.send(Err(anyhow::anyhow!("no sound from the library is playing")))
            .unwrap();
        ui.poll(now);
        assert!(ui.notice().unwrap().error);
        assert!(ui
            .notice()
            .unwrap()
            .text
            .contains("no sound from the library"));
    }

    #[test]
    fn a_dropped_reply_channel_is_reported_not_waited_on_forever() {
        let mut ui = SceneUi::default();
        let (tx, rx) = bounded::<anyhow::Result<()>>(1);
        ui.expect_reply("Stop all".into(), Reply::Unit(rx));
        drop(tx);
        ui.poll(Instant::now());
        assert!(ui.pending.is_empty());
        assert!(ui.notice().unwrap().text.contains("did not answer"));
    }

    #[test]
    fn notices_expire() {
        let mut ui = SceneUi::default();
        let t0 = Instant::now();
        ui.notify("saved", false, t0);
        ui.poll(t0 + Duration::from_secs(1));
        assert!(ui.notice().is_some());
        ui.poll(t0 + NOTICE_INFO_HOLD);
        assert!(ui.notice().is_none());

        ui.notify("boom", true, t0);
        ui.poll(t0 + NOTICE_INFO_HOLD);
        assert!(ui.notice().is_some(), "errors stay longer");
        ui.poll(t0 + NOTICE_ERROR_HOLD);
        assert!(ui.notice().is_none());
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use crate::config::Config;
    use crate::gui::hotkeys_ui::test_support::harness;
    use crate::gui::layout_probe::{assert_clean, run_frames, text_boxes};

    /// Panel inner widths the deck sees between a 640 and a 1600 window: the
    /// library column is `window - 48 - 340 - 24` wide minus 48 of padding.
    fn deck_widths() -> impl Iterator<Item = f32> {
        (640..=1600).map(|w| w as f32 - 48.0 - 340.0 - 24.0 - 48.0)
    }

    #[test]
    fn header_fits_next_to_its_caption_at_any_width() {
        for width in deck_widths() {
            let head = deck_header_layout(width);
            assert!(
                head.width() <= width + 0.001,
                "{width}: {head:?} needs {}",
                head.width()
            );
        }
    }

    #[test]
    fn header_gives_up_text_before_it_gives_up_the_buttons() {
        let roomy = deck_header_layout(1000.0);
        assert_eq!(roomy.save, SaveStyle::Full);
        assert!(roomy.active_inline);
        let mut previous = roomy;
        for width in (130..=1000).rev() {
            let head = deck_header_layout(width as f32);
            // Never goes back to a richer form while narrowing.
            assert!(head.save as u8 >= previous.save as u8, "{width}");
            if head.save == previous.save {
                assert!(head.active_inline <= previous.active_inline, "{width}");
            }
            previous = head;
        }
    }

    #[test]
    fn transport_lines_fit_at_any_width() {
        for width in deck_widths() {
            let layout = transport_layout(width);
            assert!(
                layout.widest_line() <= width + 0.001,
                "{width}: {layout:?} needs {}",
                layout.widest_line()
            );
            assert!(layout.slider_w >= 24.0 && layout.slider_w <= TRANSITION_SLIDER_MAX_W);
        }
        // The two stop buttons wrap by themselves, so each must fit alone.
        assert!(deck_widths().all(|w| w >= STOP_SCENE_W.max(FADE_OUT_W)));
    }

    #[test]
    fn transition_group_is_never_torn_apart_before_it_has_to() {
        let roomy = transport_layout(900.0);
        assert!(roomy.one_row && !roomy.split_group);
        // The group stays whole in every width where it fits at all.
        let least = TransportLayout::group_w(TRANSITION_SLIDER_MIN_W);
        for width in deck_widths().filter(|w| *w >= least) {
            assert!(!transport_layout(width).split_group, "{width}");
        }
    }

    fn deck_boxes(width: f32, auto: bool, open: bool) -> Vec<crate::gui::layout_probe::TextBox> {
        let mut config = Config::default();
        for name in ["Tavern", "A scene with a rather long name", "Road"] {
            config.library.scenes.push(crate::config::SceneConfig {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.to_string(),
                ..Default::default()
            });
        }
        let mut h = harness(config, false);
        h.app.scenes.auto_fade = auto;
        h.app.scenes.deck_open = open;
        let ctx = h.ctx.clone();
        let out = run_frames(&ctx, egui::vec2(width + 200.0, 700.0), 2, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.allocate_ui_with_layout(
                    Vec2::new(width, 600.0),
                    Layout::top_down(Align::Min),
                    |ui| scenes_deck(ui, &mut h.app),
                );
            });
        });
        text_boxes(&out)
    }

    #[test]
    fn painted_deck_stays_inside_its_panel_at_any_width() {
        for width in deck_widths().step_by(7) {
            for auto in [true, false] {
                let boxes = deck_boxes(width, auto, true);
                assert!(boxes.iter().any(|b| b.text == "TRANSITION"), "{width}");
                // The deck's left edge is its leftmost text; it is `width` wide.
                let left = boxes.iter().map(|b| b.rect.left()).fold(f32::MAX, f32::min);
                let bounds = Rect::from_min_max(
                    egui::pos2(left - 0.5, 0.0),
                    egui::pos2(left + width + 0.5, 700.0),
                );
                // Scene cards paint their own clipped labels; only the header
                // and the transport above them are held to the panel edge.
                let cards_top = boxes
                    .iter()
                    .filter(|b| b.text.contains("TAVERN"))
                    .map(|b| b.rect.top())
                    .fold(f32::MAX, f32::min);
                let head: Vec<_> = boxes
                    .iter()
                    .filter(|b| b.rect.bottom() < cards_top)
                    .cloned()
                    .collect();
                assert!(head.len() >= 4, "{width}: {boxes:?}");
                assert_clean(&head, bounds, &format!("deck at {width} (auto {auto})"));
            }
        }
    }

    #[test]
    fn no_lone_scene_label_hangs_under_the_buttons() {
        for width in [228.0 - 48.0, 440.0, 548.0 - 48.0, 820.0] {
            for auto in [true, false] {
                let boxes = deck_boxes(width, auto, true);
                assert!(
                    !boxes.iter().any(|b| b.text == "SCENE"),
                    "{width}: a bare Scene label is back: {boxes:?}"
                );
                if auto {
                    let value = boxes
                        .iter()
                        .find(|b| b.text == "PER SCENE")
                        .unwrap_or_else(|| panic!("{width}: no value text in {boxes:?}"));
                    // It sits on the slider's line, not on its own.
                    let transition = boxes.iter().find(|b| b.text == "TRANSITION").unwrap();
                    assert!(
                        (value.rect.center().y - transition.rect.center().y).abs() < 40.0,
                        "{width}"
                    );
                }
            }
        }
    }

    #[test]
    fn collapsed_deck_draws_only_its_header() {
        let boxes = deck_boxes(440.0, true, false);
        assert!(boxes.iter().any(|b| b.text == "SCENES"));
        assert!(!boxes.iter().any(|b| b.text == "TRANSITION"));
    }
}
