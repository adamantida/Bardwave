//! Hotkeys in the console: running the bindings and the "Hotkeys" dialog.
//!
//! The bindings themselves live in the [`Library`] (`[[hotkeys]]` in
//! `config.toml`, saved with it); parsing, matching and the system-wide
//! shortcut service are in [`crate::hotkeys`]. This module is the GUI side:
//!
//! * [`SoundSharedApp::poll_hotkeys`] runs once per frame, before any widget:
//!   window shortcuts ([`poll_local`]) and system-wide ones
//!   ([`HotkeyService::poll`]) become [`HotkeyEvent`]s that are run against the
//!   Director on the GUI thread (which owns the engine).
//! * [`show_hotkeys_dialog`] lists the bindings, captures a new key ("press a
//!   combination"), shows conflicts and problems, and switches global
//!   shortcuts on or off.
//!
//! Everything that can be decided without a window is a plain function with a
//! unit test: [`route`] (which part of the app runs an action),
//! [`row_statuses`] (conflicts / broken bindings), [`capture_from_events`]
//! (egui key events to a [`Binding`]), [`plan_add`], [`Catalog`].
//!
//! Ducking and the playlist actions do not go through
//! [`HotkeyDispatcher`]: they use the Director's own duck state, so a hotkey,
//! the DUCK button and the phone panel all agree on what is ducked.

use egui::{Context, Event, Modifiers, RichText, Ui};
use uuid::Uuid;

use crate::audio::director::{Director, DirectorError, Playback};
use crate::audio::{DuckTarget, PlaylistCommand};
use crate::config::{HotkeyAction, HotkeyConfig};
use crate::gui::app::{mirror_playing, sync_director, Dialog, SoundSharedApp};
use crate::gui::groups::Library;
use crate::gui::settings_extras::kit;
use crate::gui::theme::Metrics;
use crate::gui::theme::{alpha, hairline, micro_label, micro_label_accent, panel_title, Palette};
use crate::gui::types::AppState;
use crate::gui::widgets::{glyph, glyph_font, ConsoleButton, IconButton};
use crate::hotkeys::{
    find_conflicts_for, is_midi_key, poll_local, Binding, HotkeyDispatcher, HotkeyEvent, HotkeyMap,
    HotkeyService, Key, KeyPhase, Mods, NamedKey, NoExtras, Platform, ProblemKind, ServiceStatus,
};

/// Width of the dialog window.
const DIALOG_W: f32 = 560.0;

// ---------------------------------------------------------------------------
// Action kinds (the schema the dialog offers)
// ---------------------------------------------------------------------------

/// What a binding can do, without its target. One entry per
/// [`HotkeyAction`] variant, in the order the dialog lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    /// Play a sound (stingers fire once, others toggle).
    Sound,
    /// Activate a scene.
    Scene,
    /// Start a playlist.
    Playlist,
    /// Stop everything.
    StopAll,
    /// Stop the active scene.
    StopScene,
    /// Master volume up one step.
    MasterUp,
    /// Master volume down one step.
    MasterDown,
    /// Mute / unmute master.
    MasterMute,
    /// Duck the background, latched.
    DuckToggle,
    /// Duck the background while the key is held.
    DuckHold,
    /// Pause / resume the music.
    PauseMusic,
    /// Next playlist track.
    NextTrack,
    /// Previous playlist track.
    PrevTrack,
}

impl ActionKind {
    /// Every kind, in menu order.
    pub const ALL: [ActionKind; 13] = [
        Self::Sound,
        Self::Scene,
        Self::Playlist,
        Self::StopAll,
        Self::StopScene,
        Self::MasterUp,
        Self::MasterDown,
        Self::MasterMute,
        Self::DuckToggle,
        Self::DuckHold,
        Self::PauseMusic,
        Self::NextTrack,
        Self::PrevTrack,
    ];

    /// Menu text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sound => "Play sound / stinger",
            Self::Scene => "Activate scene",
            Self::Playlist => "Start playlist",
            Self::StopAll => "Stop all",
            Self::StopScene => "Stop scene",
            Self::MasterUp => "Master volume up",
            Self::MasterDown => "Master volume down",
            Self::MasterMute => "Master mute",
            Self::DuckToggle => "Duck background (toggle)",
            Self::DuckHold => "Duck background (hold)",
            Self::PauseMusic => "Pause / resume music",
            Self::NextTrack => "Next track",
            Self::PrevTrack => "Previous track",
        }
    }

    /// Whether the action points at a sound, scene or playlist.
    pub fn needs_target(self) -> bool {
        matches!(self, Self::Sound | Self::Scene | Self::Playlist)
    }

    /// The kind of an action.
    pub fn of(action: &HotkeyAction) -> Self {
        match action {
            HotkeyAction::Sound { .. } => Self::Sound,
            HotkeyAction::Scene { .. } => Self::Scene,
            HotkeyAction::Playlist { .. } => Self::Playlist,
            HotkeyAction::StopAll => Self::StopAll,
            HotkeyAction::StopScene => Self::StopScene,
            HotkeyAction::MasterUp => Self::MasterUp,
            HotkeyAction::MasterDown => Self::MasterDown,
            HotkeyAction::MasterMute => Self::MasterMute,
            HotkeyAction::DuckToggle => Self::DuckToggle,
            HotkeyAction::DuckHold => Self::DuckHold,
            HotkeyAction::PauseMusic => Self::PauseMusic,
            HotkeyAction::NextTrack => Self::NextTrack,
            HotkeyAction::PrevTrack => Self::PrevTrack,
        }
    }

    /// The action, or `None` when a target is needed and `target` is missing.
    pub fn build(self, target: Option<&str>) -> Option<HotkeyAction> {
        let target = || target.map(|t| t.to_string());
        Some(match self {
            Self::Sound => HotkeyAction::Sound { target: target()? },
            Self::Scene => HotkeyAction::Scene { target: target()? },
            Self::Playlist => HotkeyAction::Playlist { target: target()? },
            Self::StopAll => HotkeyAction::StopAll,
            Self::StopScene => HotkeyAction::StopScene,
            Self::MasterUp => HotkeyAction::MasterUp,
            Self::MasterDown => HotkeyAction::MasterDown,
            Self::MasterMute => HotkeyAction::MasterMute,
            Self::DuckToggle => HotkeyAction::DuckToggle,
            Self::DuckHold => HotkeyAction::DuckHold,
            Self::PauseMusic => HotkeyAction::PauseMusic,
            Self::NextTrack => HotkeyAction::NextTrack,
            Self::PrevTrack => HotkeyAction::PrevTrack,
        })
    }
}

// ---------------------------------------------------------------------------
// Names of the things a binding can point at
// ---------------------------------------------------------------------------

/// `(id, display name)` pairs of the library's sounds, scenes and playlists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// Sounds.
    pub sounds: Vec<(String, String)>,
    /// Scenes.
    pub scenes: Vec<(String, String)>,
    /// Playlists.
    pub playlists: Vec<(String, String)>,
}

/// Ids are UUID strings; compare them without caring about case or spaces.
fn same_id(a: &str, b: &str) -> bool {
    match (Uuid::parse_str(a.trim()), Uuid::parse_str(b.trim())) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.trim() == b.trim(),
    }
}

impl Catalog {
    /// Read the names from the library.
    pub fn from_library(library: &Library) -> Self {
        Self {
            sounds: library
                .sounds()
                .iter()
                .map(|sound| {
                    let name = if sound.name.trim().is_empty() {
                        sound
                            .path
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| sound.path.to_string_lossy().into_owned())
                    } else {
                        sound.name.clone()
                    };
                    (sound.id.to_string(), name)
                })
                .collect(),
            scenes: library
                .scenes()
                .iter()
                .map(|scene| (scene.id.to_string(), scene.name.clone()))
                .collect(),
            playlists: library
                .playlists()
                .iter()
                .map(|playlist| (playlist.id.to_string(), playlist.name.clone()))
                .collect(),
        }
    }

    /// The choices for a kind that needs a target.
    pub fn options(&self, kind: ActionKind) -> &[(String, String)] {
        match kind {
            ActionKind::Sound => &self.sounds,
            ActionKind::Scene => &self.scenes,
            ActionKind::Playlist => &self.playlists,
            _ => &[],
        }
    }

    /// Name of the target of `kind` with this id.
    pub fn name_of(&self, kind: ActionKind, id: &str) -> Option<&str> {
        self.options(kind)
            .iter()
            .find(|(candidate, _)| same_id(candidate, id))
            .map(|(_, name)| name.as_str())
    }

    /// Whether the action's target exists (always true without a target).
    pub fn has_target(&self, action: &HotkeyAction) -> bool {
        match action.target() {
            Some(id) => self.name_of(ActionKind::of(action), id).is_some(),
            None => true,
        }
    }
}

/// One line describing an action: `Scene: Tavern`, `Stop all`,
/// `Sound: (missing)` for a target that no longer exists.
pub fn action_label(action: &HotkeyAction, catalog: &Catalog) -> String {
    let kind = ActionKind::of(action);
    match action.target() {
        Some(id) => {
            let name = catalog.name_of(kind, id).unwrap_or("(missing)");
            let what = match kind {
                ActionKind::Sound => "Sound",
                ActionKind::Scene => "Scene",
                _ => "Playlist",
            };
            format!("{what}: {name}")
        }
        None => kind.label().to_string(),
    }
}

// ---------------------------------------------------------------------------
// Row status: conflicts and broken bindings
// ---------------------------------------------------------------------------

/// What is wrong (or not) with one `[[hotkeys]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowStatus {
    /// Fine.
    Ok,
    /// The key text is not a key combination.
    Invalid(String),
    /// A MIDI trigger: MIDI input is not connected to the app, so it never fires.
    MidiUnavailable,
    /// Another entry (0-based index) has the same key and comes first: this
    /// entry never fires.
    Shadowed(usize),
    /// This entry fires, but the listed later entries use the same key and
    /// never will.
    Shadows(Vec<usize>),
    /// The action points at a sound, scene or playlist that is gone.
    MissingTarget,
}

impl RowStatus {
    /// Whether the row needs the user's attention.
    pub fn is_problem(&self) -> bool {
        *self != Self::Ok
    }

    /// The text under the row, `None` for a fine one. Numbers are 1-based.
    pub fn message(&self) -> Option<String> {
        Some(match self {
            Self::Ok => return None,
            Self::Invalid(why) => why.clone(),
            Self::MidiUnavailable => {
                "MIDI binding: MIDI input is not connected in this build, it will not fire"
                    .to_string()
            }
            Self::Shadowed(by) => format!(
                "Conflict: the same key is bound in #{}, which wins - this one never fires",
                by + 1
            ),
            Self::Shadows(others) => {
                let list = others
                    .iter()
                    .map(|i| format!("#{}", i + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Conflict: the same key is also bound in {list}, which never fire")
            }
            Self::MissingTarget => {
                "The sound, scene or playlist it points at no longer exists".to_string()
            }
        })
    }
}

/// The status of every entry, in order. Uses the hotkey map (broken and
/// shadowed entries) and [`find_conflicts_for`] (who wins a shared key).
pub fn row_statuses(
    hotkeys: &[HotkeyConfig],
    platform: Platform,
    catalog: &Catalog,
) -> Vec<RowStatus> {
    let map = HotkeyMap::build_for(hotkeys, platform);
    let conflicts = find_conflicts_for(hotkeys, platform);
    (0..hotkeys.len())
        .map(|index| {
            if let Some(problem) = map.problems().iter().find(|p| p.index == index) {
                return match &problem.kind {
                    ProblemKind::Invalid(e) => RowStatus::Invalid(e.to_string()),
                    ProblemKind::MidiUnavailable => RowStatus::MidiUnavailable,
                    ProblemKind::Shadowed { by } => RowStatus::Shadowed(*by),
                };
            }
            let mut losers: Vec<usize> = conflicts
                .iter()
                .filter(|c| c.first == index)
                .map(|c| c.second)
                .collect();
            losers.sort_unstable();
            losers.dedup();
            if !losers.is_empty() {
                return RowStatus::Shadows(losers);
            }
            if !catalog.has_target(&hotkeys[index].action) {
                return RowStatus::MissingTarget;
            }
            RowStatus::Ok
        })
        .collect()
}

/// How a new binding is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddPlan {
    /// Add at the end.
    Append,
    /// The key is already bound at this index: replace that entry, so the
    /// dialog never makes a new conflict by itself.
    Replace(usize),
}

/// Decide between adding and replacing for a key.
pub fn plan_add(hotkeys: &[HotkeyConfig], binding: &Binding, platform: Platform) -> AddPlan {
    hotkeys
        .iter()
        .position(|h| {
            !is_midi_key(&h.key)
                && Binding::parse_for(&h.key, platform).ok().as_ref() == Some(binding)
        })
        .map_or(AddPlan::Append, AddPlan::Replace)
}

/// Parse the text of the "or type it" field.
pub fn parse_key_text(text: &str, platform: Platform) -> Result<Binding, String> {
    if is_midi_key(text) {
        return Err("MIDI bindings are written in config.toml, not here".to_string());
    }
    Binding::parse_for(text, platform).map_err(|e| e.to_string())
}

/// The key as shown to the user (`Ctrl+Option+1` on macOS); the raw text when
/// it does not parse.
pub fn key_label(key: &str, platform: Platform) -> String {
    Binding::parse_for(key, platform)
        .map(|b| b.label_for(platform))
        .unwrap_or_else(|_| key.trim().to_string())
}

/// What to tell the user about permissions for system-wide shortcuts.
pub fn global_hint(platform: Platform) -> &'static str {
    match platform {
        Platform::MacOs => {
            "Works when Bardwave is in the background. macOS may ask you to allow it under \
             System Settings > Privacy & Security (Accessibility / Input Monitoring); without \
             that, shortcuts only work while this window is focused. Keys without Ctrl, Option \
             or Cmd (F-keys excepted) are never global."
        }
        Platform::Other => {
            "Works when Bardwave is in the background. Only available on Windows and on Linux \
             with X11 (not Wayland). Keys without Ctrl, Alt or Win (F-keys excepted) are never \
             global."
        }
    }
}

// ---------------------------------------------------------------------------
// Capture: egui key events -> Binding
// ---------------------------------------------------------------------------

/// The key of a combination for an egui key, `None` for keys with no binding
/// name. The inverse of [`Key::to_egui`].
pub fn key_from_egui(key: egui::Key) -> Option<Key> {
    let candidates = ('A'..='Z')
        .map(Key::Letter)
        .chain((0..=9).map(Key::Digit))
        .chain((1..=24).map(Key::Function))
        .chain(NamedKey::ALL.into_iter().map(Key::Named));
    candidates.into_iter().find(|k| k.to_egui() == Some(key))
}

/// Modifiers of a captured combination. egui sees no Windows / Super key, so
/// `cmd` can only be set on macOS.
pub fn mods_from_egui(modifiers: &Modifiers, platform: Platform) -> Mods {
    Mods {
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        shift: modifiers.shift,
        cmd: platform == Platform::MacOs && modifiers.mac_cmd,
    }
}

/// What the user did while the editor waited for a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    /// A combination was pressed.
    Binding(Binding),
    /// Escape on its own: stop capturing.
    Cancel,
    /// A key that cannot be bound (the text says why); keep waiting.
    Unsupported(String),
}

/// The outcome of this frame's events, if a key went down.
pub fn capture_from_events(events: &[Event], platform: Platform) -> Option<Captured> {
    events.iter().find_map(|event| {
        let Event::Key {
            key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } = event
        else {
            return None;
        };
        let mods = mods_from_egui(modifiers, platform);
        if *key == egui::Key::Escape && !mods.any() {
            return Some(Captured::Cancel);
        }
        Some(match key_from_egui(*key) {
            Some(key) => Captured::Binding(Binding { mods, key }),
            None => Captured::Unsupported(format!("{} cannot be used as a hotkey", key.name())),
        })
    })
}

// ---------------------------------------------------------------------------
// Running an event
// ---------------------------------------------------------------------------

/// A duck request from a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuckOp {
    /// Latch on / off.
    Toggle,
    /// Key down of a hold binding.
    HoldStart,
    /// Key up of a hold binding.
    HoldEnd,
}

/// Which part of the app runs an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// The Director's duck state.
    Duck(DuckOp),
    /// The Director's music layer.
    Playlist(PlaylistCommand),
    /// [`HotkeyDispatcher`]: scenes, sounds, stop, master.
    Dispatch,
}

/// Where an event goes; `None` when it does nothing (the release of a key
/// whose action only acts on the press).
pub fn route(action: &HotkeyAction, phase: KeyPhase) -> Option<Route> {
    let pressed = phase == KeyPhase::Pressed;
    match action {
        HotkeyAction::DuckHold => Some(Route::Duck(if pressed {
            DuckOp::HoldStart
        } else {
            DuckOp::HoldEnd
        })),
        _ if !pressed => None,
        HotkeyAction::DuckToggle => Some(Route::Duck(DuckOp::Toggle)),
        HotkeyAction::Playlist { target } => {
            Some(Route::Playlist(PlaylistCommand::Start(target.clone())))
        }
        HotkeyAction::NextTrack => Some(Route::Playlist(PlaylistCommand::Next)),
        HotkeyAction::PrevTrack => Some(Route::Playlist(PlaylistCommand::Prev)),
        HotkeyAction::PauseMusic => Some(Route::Playlist(PlaylistCommand::TogglePause)),
        _ => Some(Route::Dispatch),
    }
}

/// Run a duck request on the Director's duck state, for the buses of
/// `[audio] duck_buses`.
pub fn run_duck_op<P: Playback + ?Sized>(
    director: &mut Director,
    pb: &P,
    op: DuckOp,
) -> Result<(), DirectorError> {
    let target = DuckTarget::Configured;
    match op {
        DuckOp::Toggle => director.duck_toggle(pb, target).map(|_| ()),
        DuckOp::HoldStart => director.duck_start(pb, target),
        DuckOp::HoldEnd => director.duck_end(pb, target),
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What the next captured key is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureTarget {
    /// The "new binding" row.
    Draft,
    /// The key of an existing entry (0-based index).
    Row(usize),
}

/// The "new binding" row of the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// What it does.
    pub kind: ActionKind,
    /// Library id of the target, when the kind needs one.
    pub target: Option<String>,
    /// The captured (or typed) key.
    pub key: Option<Binding>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            kind: ActionKind::Scene,
            target: None,
            key: None,
        }
    }
}

/// Everything the hotkeys need between frames.
pub struct HotkeyUi {
    map: HotkeyMap,
    /// The bindings `map` and `service` were built from. Deleting a sound,
    /// scene or playlist prunes the library's bindings behind the dialog's
    /// back; comparing against this list notices it.
    source: Vec<HotkeyConfig>,
    dispatcher: HotkeyDispatcher,
    service: Option<HotkeyService>,
    status: ServiceStatus,
    capture: Option<CaptureTarget>,
    draft: Draft,
    key_text: String,
    /// A message for the dialog: `(text, is_error)`.
    notice: Option<(String, bool)>,
}

impl HotkeyUi {
    /// Build the key map and, when `gui.global_hotkeys` is on, start the
    /// system-wide service. Call on the main thread (macOS requires it).
    pub fn start(ctx: &Context, app_state: &AppState, library: &Library) -> Self {
        let global = app_state.config.lock().unwrap().gui.global_hotkeys;
        let started = HotkeyService::start(global, library.hotkeys(), Some(wake_for(ctx)));
        if let ServiceStatus::NeedsPermission(text) | ServiceStatus::Unavailable(text) =
            &started.status
        {
            tracing::warn!("Global hotkeys: {text}");
        }
        Self {
            map: HotkeyMap::build(library.hotkeys()),
            source: library.hotkeys().to_vec(),
            dispatcher: HotkeyDispatcher::new(),
            service: started.service,
            status: started.status,
            capture: None,
            draft: Draft::default(),
            key_text: String::new(),
            notice: None,
        }
    }

    /// What the dialog says about the system-wide service.
    pub fn status(&self) -> &ServiceStatus {
        &self.status
    }

    /// Whether `list` differs from the bindings the map was built from.
    fn is_stale(&self, list: &[HotkeyConfig]) -> bool {
        self.source != list
    }

    /// Number of bindings that can fire (for the one-line summary).
    pub fn active_count(&self) -> usize {
        self.map.entries().len()
    }
}

/// Wake the UI when a system-wide shortcut fires while the window is idle.
fn wake_for(ctx: &Context) -> crate::hotkeys::service::Wake {
    let ctx = ctx.clone();
    std::sync::Arc::new(move || ctx.request_repaint())
}

// ---------------------------------------------------------------------------
// The app side: polling, running, editing
// ---------------------------------------------------------------------------

impl SoundSharedApp {
    /// Whether the hotkey editor is waiting for a key (the Escape key then
    /// belongs to it, and no binding may fire).
    pub fn hotkeys_capturing(&self) -> bool {
        matches!(self.dialog, Dialog::Hotkeys) && self.extras.hotkeys.capture.is_some()
    }

    /// Open the hotkey dialog.
    pub fn open_hotkeys(&mut self) {
        self.extras.hotkeys.capture = None;
        self.extras.hotkeys.notice = None;
        self.dialog = Dialog::Hotkeys;
    }

    /// Collect this frame's shortcuts (window and system-wide) and run them.
    /// Call at the start of `update`, before any widget is drawn.
    pub(crate) fn poll_hotkeys(&mut self, ctx: &Context) {
        let capturing = self.hotkeys_capturing();
        // Deleting a sound, scene or playlist drops its bindings from the
        // library only; bring the key map and OS shortcuts back in line.
        if self.extras.hotkeys.is_stale(self.library.hotkeys()) {
            self.hotkeys_changed();
        }
        let hk = &mut self.extras.hotkeys;
        let mut events: Vec<HotkeyEvent> = Vec::new();
        if let Some(service) = hk.service.as_ref() {
            // Drained even while capturing, so nothing fires later.
            let global = service.poll();
            if !capturing {
                events.extend(global);
            }
        }
        if !capturing {
            events.extend(poll_local(ctx, &hk.map, hk.service.as_ref()));
        }
        for event in events {
            self.run_hotkey_event(&event);
        }
    }

    /// Run one event against the Director on this (the engine's) thread.
    fn run_hotkey_event(&mut self, event: &HotkeyEvent) {
        let Some(route) = route(&event.action, event.phase) else {
            return;
        };
        let guard = self.audio_engine.lock().unwrap();
        let Some(engine) = guard.as_ref() else {
            tracing::warn!("Hotkey ignored: the audio engine is not running");
            return;
        };
        if !matches!(route, Route::Duck(_)) {
            sync_director(&mut self.director, &self.library, &self.sounds);
        }
        let result: Result<(), String> = match route {
            Route::Duck(op) => {
                run_duck_op(&mut self.director, engine, op).map_err(|e| e.to_string())
            }
            Route::Playlist(command) => self
                .director
                .playlist_command(engine, command)
                .map_err(|e| e.to_string()),
            Route::Dispatch => self
                .extras
                .hotkeys
                .dispatcher
                .handle(&mut self.director, engine, &mut NoExtras, event)
                .map(|_| ())
                .map_err(|e| e.to_string()),
        };
        mirror_playing(&self.sounds, engine);
        *self.master_volume.lock().unwrap() = Playback::master_volume(engine);
        drop(guard);

        if let Err(message) = result {
            let catalog = Catalog::from_library(&self.library);
            let what = action_label(&event.action, &catalog);
            tracing::warn!("Hotkey {what}: {message}");
            self.extras.hotkeys.notice = Some((format!("{what}: {message}"), true));
        }
    }

    /// The bindings changed: rebuild the key map, re-register the system-wide
    /// shortcuts and queue a save.
    fn hotkeys_changed(&mut self) {
        let list = self.library.hotkeys().to_vec();
        let hk = &mut self.extras.hotkeys;
        hk.map = HotkeyMap::build(&list);
        hk.source = list.clone();
        if let Some(service) = hk.service.as_mut() {
            let report = service.sync(&list);
            hk.status = ServiceStatus::Active {
                registered: report.registered,
                failed: report.failed,
            };
        }
        self.library_dirty = true;
    }

    /// Switch system-wide shortcuts on or off (saved in `[gui]`).
    fn set_global_hotkeys(&mut self, ctx: &Context, on: bool) {
        self.app_state.config.lock().unwrap().gui.global_hotkeys = on;
        self.library_dirty = true;
        let hk = &mut self.extras.hotkeys;
        // Only one service may exist at a time: drop the old one first.
        hk.service = None;
        let started = HotkeyService::start(on, self.library.hotkeys(), Some(wake_for(ctx)));
        hk.service = started.service;
        hk.status = started.status;
    }

    /// Remove binding `index`.
    fn remove_hotkey_at(&mut self, index: usize) {
        if self.library.remove_hotkey(index) {
            self.hotkeys_changed();
        }
    }

    /// Give binding `index` a new key.
    fn rekey_hotkey(&mut self, index: usize, binding: Binding) {
        let Some(action) = self.library.hotkeys().get(index).map(|h| h.action.clone()) else {
            return;
        };
        match self.library.set_hotkey(index, &binding.to_string(), action) {
            Ok(()) => {
                self.extras.hotkeys.notice = None;
                self.hotkeys_changed();
            }
            Err(e) => self.extras.hotkeys.notice = Some((e.to_string(), true)),
        }
    }

    /// Add (or replace by key) the binding of the draft row.
    fn add_draft_hotkey(&mut self) {
        let platform = Platform::current();
        let draft = self.extras.hotkeys.draft.clone();
        let (Some(binding), Some(action)) = (draft.key, draft.kind.build(draft.target.as_deref()))
        else {
            return;
        };
        let key = binding.to_string();
        let result = match plan_add(self.library.hotkeys(), &binding, platform) {
            AddPlan::Append => self.library.add_hotkey(&key, action).map(|_| ()),
            AddPlan::Replace(index) => self.library.set_hotkey(index, &key, action),
        };
        match result {
            Ok(()) => {
                let hk = &mut self.extras.hotkeys;
                hk.draft.key = None;
                hk.key_text.clear();
                hk.notice = None;
                self.hotkeys_changed();
            }
            Err(e) => self.extras.hotkeys.notice = Some((e.to_string(), true)),
        }
    }

    /// While the editor waits for a key: swallow this frame's keyboard events
    /// (nothing else may react to them) and apply the combination.
    fn hotkeys_capture_step(&mut self, ctx: &Context, platform: Platform) {
        let Some(target) = self.extras.hotkeys.capture else {
            return;
        };
        let outcome = ctx.input_mut(|input| {
            let outcome = capture_from_events(&input.events, platform);
            input
                .events
                .retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_)));
            outcome
        });
        match outcome {
            None => {}
            Some(Captured::Cancel) => self.extras.hotkeys.capture = None,
            Some(Captured::Unsupported(message)) => {
                self.extras.hotkeys.notice = Some((message, true));
            }
            Some(Captured::Binding(binding)) => {
                self.extras.hotkeys.capture = None;
                match target {
                    CaptureTarget::Draft => {
                        let hk = &mut self.extras.hotkeys;
                        hk.draft.key = Some(binding);
                        hk.key_text.clear();
                        hk.notice = None;
                    }
                    CaptureTarget::Row(index) => self.rekey_hotkey(index, binding),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The dialog
// ---------------------------------------------------------------------------

/// Things the dialog asks for; applied after the frame is drawn.
enum Intent {
    Close,
    SetGlobal(bool),
    Remove(usize),
    Capture(CaptureTarget),
    CancelCapture,
    AddDraft,
}

/// The "Hotkeys" modal. Safe to call every frame while `Dialog::Hotkeys` is
/// open.
pub fn show_hotkeys_dialog(ctx: &Context, state: &mut SoundSharedApp) {
    let platform = Platform::current();
    state.hotkeys_capture_step(ctx, platform);

    let catalog = Catalog::from_library(&state.library);
    let hotkeys: Vec<HotkeyConfig> = state.library.hotkeys().to_vec();
    let statuses = row_statuses(&hotkeys, platform, &catalog);
    let global_on = state.app_state.config.lock().unwrap().gui.global_hotkeys;
    let service_line = state.extras.hotkeys.status.message();
    let service_problem = matches!(
        state.extras.hotkeys.status,
        ServiceStatus::NeedsPermission(_) | ServiceStatus::Unavailable(_)
    );
    let capture = state.extras.hotkeys.capture;
    let notice = state.extras.hotkeys.notice.clone();
    let mut draft = std::mem::take(&mut state.extras.hotkeys.draft);
    let mut key_text = std::mem::take(&mut state.extras.hotkeys.key_text);
    let mut text_error: Option<String> = None;
    let mut intents: Vec<Intent> = Vec::new();

    kit::modal_window(ctx, "hotkeys_modal", DIALOG_W, |ui| {
        ui.label(micro_label_accent("Settings"));
        ui.add_space(2.0);
        ui.label(panel_title("Hotkeys"));
        ui.add_space(2.0);
        ui.label(micro_label(
            "Click a key to change it. Keys fire while this window is focused.",
        ));
        ui.add_space(Metrics::LG);

        // ---- Global shortcuts ----
        let mut global = global_on;
        if ui
            .checkbox(
                &mut global,
                RichText::new("GLOBAL HOTKEYS (WHEN THE WINDOW IS NOT FOCUSED)")
                    .font(glyph_font(Metrics::MICRO))
                    .color(Palette::ZINC_300)
                    .extra_letter_spacing(0.8),
            )
            .changed()
        {
            intents.push(Intent::SetGlobal(global));
        }
        kit::note_under(ui, global_hint(platform), Palette::ZINC_500);
        kit::note_under(
            ui,
            service_line,
            if service_problem {
                Palette::AMBER_400
            } else if global_on {
                Palette::ROSE_400
            } else {
                Palette::ZINC_500
            },
        );

        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::LG);

        // ---- Bindings ----
        ui.label(micro_label(format!("Bindings ({})", hotkeys.len())));
        ui.add_space(Metrics::SM);
        egui::ScrollArea::vertical()
            .id_salt("hotkeys_scroll")
            .max_height(330.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = Metrics::SM;
                if hotkeys.is_empty() {
                    kit::empty_state(
                        ui,
                        "No hotkeys yet",
                        "Pick an action below, press Capture key and press the combination, \
                             then Add hotkey. Try Ctrl+1 for a scene or F5 for Stop All.",
                    );
                }
                for (index, hotkey) in hotkeys.iter().enumerate() {
                    binding_row(
                        ui,
                        index,
                        hotkey,
                        &statuses[index],
                        &catalog,
                        platform,
                        capture == Some(CaptureTarget::Row(index)),
                        &mut intents,
                    );
                }
            });

        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::LG);

        // ---- New binding ----
        ui.label(micro_label("New hotkey"));
        ui.add_space(Metrics::SM);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("hk_new_kind")
                .width(200.0)
                .selected_text(draft.kind.label())
                .show_ui(ui, |ui| {
                    for kind in ActionKind::ALL {
                        if ui
                            .selectable_value(&mut draft.kind, kind, kind.label())
                            .changed()
                        {
                            draft.target = None;
                        }
                    }
                });
            if draft.kind.needs_target() {
                let options = catalog.options(draft.kind);
                let current = draft
                    .target
                    .as_deref()
                    .and_then(|id| catalog.name_of(draft.kind, id))
                    .unwrap_or(if options.is_empty() {
                        "(none in the library)"
                    } else {
                        "Choose..."
                    })
                    .to_string();
                egui::ComboBox::from_id_salt("hk_new_target")
                    .width(ui.available_width().max(120.0))
                    .selected_text(crate::gui::widgets::elide(&current, 28))
                    .show_ui(ui, |ui| {
                        for (id, name) in options {
                            let selected = draft.target.as_deref().is_some_and(|t| same_id(t, id));
                            if ui.selectable_label(selected, name).clicked() {
                                draft.target = Some(id.clone());
                            }
                        }
                    });
            }
        });
        ui.add_space(Metrics::SM);
        ui.horizontal(|ui| {
            let capturing_draft = capture == Some(CaptureTarget::Draft);
            let (label, kind_hint) = if capturing_draft {
                ("Press a key...  (Esc cancels)".to_string(), true)
            } else {
                match &draft.key {
                    Some(b) => (b.label_for(platform), false),
                    None => ("Capture key".to_string(), false),
                }
            };
            let button = if kind_hint {
                ConsoleButton::primary(label)
            } else {
                ConsoleButton::ghost(label)
            };
            if button
                .width(210.0)
                .height(Metrics::BTN_H)
                .tooltip(
                    "Click, then press the combination. The numeric keypad and the Win/Super key \
                         cannot be captured: type them instead.",
                )
                .id_salt("hk_capture_draft")
                .show(ui)
                .clicked()
            {
                intents.push(if capturing_draft {
                    Intent::CancelCapture
                } else {
                    Intent::Capture(CaptureTarget::Draft)
                });
            }

            let response = ui.add(
                egui::TextEdit::singleline(&mut key_text)
                    .hint_text("or type: Ctrl+Shift+1")
                    .desired_width(ui.available_width().max(100.0))
                    .font(glyph_font(12.0))
                    .margin(egui::Margin::symmetric(10.0, 8.0)),
            );
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                match parse_key_text(&key_text, platform) {
                    Ok(binding) => draft.key = Some(binding),
                    Err(e) => text_error = Some(e),
                }
            }
        });

        // What the draft would do to existing bindings.
        let draft_action = draft.kind.build(draft.target.as_deref());
        let plan = draft.key.as_ref().map(|b| plan_add(&hotkeys, b, platform));
        ui.add_space(Metrics::XS);
        if let Some(error) = text_error.clone().or_else(|| {
            notice
                .as_ref()
                .filter(|(_, is_error)| *is_error)
                .map(|(text, _)| text.clone())
        }) {
            kit::note_colored(ui, error, Palette::ROSE_400);
        } else if let (Some(AddPlan::Replace(index)), Some(b)) = (plan, &draft.key) {
            kit::note_colored(
                ui,
                format!(
                    "{} is already bound in #{} ({}): adding replaces that binding.",
                    b.label_for(platform),
                    index + 1,
                    action_label(&hotkeys[index].action, &catalog)
                ),
                Palette::AMBER_400,
            );
        } else if let Some(b) = &draft.key {
            if global_on && !b.is_global_safe() {
                kit::note_colored(
                    ui,
                    "Without Ctrl / Alt / Cmd this key only works while the window is focused.",
                    Palette::ZINC_500,
                );
            } else if !b.is_locally_matchable(platform) {
                kit::note_colored(
                    ui,
                    "This key cannot be seen by the window: it only works as a global hotkey.",
                    Palette::AMBER_400,
                );
            }
        }

        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::MD);

        ui.horizontal(|ui| {
            let can_add = draft.key.is_some() && draft_action.is_some();
            let add_label = match plan {
                Some(AddPlan::Replace(_)) => "Replace",
                _ => "Add hotkey",
            };
            if ConsoleButton::primary(add_label)
                .glyph(glyph::PLUS)
                .width(170.0)
                .height(Metrics::BTN_H)
                .enabled(can_add)
                .tooltip(if can_add {
                    "Save this binding"
                } else {
                    "Choose an action (and its target) and a key first"
                })
                .id_salt("hk_add")
                .show(ui)
                .clicked()
                && can_add
            {
                intents.push(Intent::AddDraft);
            }
            ui.add_space(Metrics::SM);
            if ConsoleButton::ghost("Done")
                .glyph(glyph::CHECK)
                .width(120.0)
                .height(Metrics::BTN_H)
                .id_salt("hk_close")
                .show(ui)
                .clicked()
            {
                intents.push(Intent::Close);
            }
        });
    });

    state.extras.hotkeys.draft = draft;
    state.extras.hotkeys.key_text = key_text;
    for intent in intents {
        match intent {
            Intent::Close => {
                state.extras.hotkeys.capture = None;
                state.close_dialog();
            }
            Intent::SetGlobal(on) => state.set_global_hotkeys(ctx, on),
            Intent::Remove(index) => state.remove_hotkey_at(index),
            Intent::Capture(target) => {
                state.extras.hotkeys.capture = Some(target);
                state.extras.hotkeys.notice = None;
            }
            Intent::CancelCapture => state.extras.hotkeys.capture = None,
            Intent::AddDraft => state.add_draft_hotkey(),
        }
    }
}

/// One line of the bindings list (and its problem text underneath).
#[allow(clippy::too_many_arguments)]
fn binding_row(
    ui: &mut Ui,
    index: usize,
    hotkey: &HotkeyConfig,
    status: &RowStatus,
    catalog: &Catalog,
    platform: Platform,
    capturing: bool,
    intents: &mut Vec<Intent>,
) {
    let problem = status.is_problem();
    let frame_color = if problem {
        alpha(Palette::AMBER_400, 18)
    } else {
        Palette::ROW
    };
    egui::Frame::none()
        .fill(frame_color)
        .rounding(egui::Rounding::same(Metrics::R_SM))
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(micro_label(format!("{:02}", index + 1)));
                let key_text = if capturing {
                    "Press a key...".to_string()
                } else if hotkey.key.trim().is_empty() {
                    "(no key)".to_string()
                } else {
                    key_label(&hotkey.key, platform)
                };
                let salt = format!("hk_key_{index}");
                let button = if capturing {
                    ConsoleButton::primary(key_text)
                } else {
                    ConsoleButton::ghost(key_text)
                };
                if button
                    .width(150.0)
                    .height(Metrics::BTN_H_SM)
                    .tooltip("Click, then press the new combination (Esc cancels)")
                    .id_salt(&salt)
                    .show(ui)
                    .clicked()
                {
                    intents.push(if capturing {
                        Intent::CancelCapture
                    } else {
                        Intent::Capture(CaptureTarget::Row(index))
                    });
                }
                ui.label(
                    RichText::new(crate::gui::widgets::elide(
                        &action_label(&hotkey.action, catalog),
                        30,
                    ))
                    .font(glyph_font(11.0))
                    .color(Palette::ZINC_300),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let salt = format!("hk_del_{index}");
                    if IconButton::new(glyph::TRASH)
                        .size(24.0)
                        .font_size(11.0)
                        .tooltip("Delete this hotkey")
                        .id_salt(&salt)
                        .show(ui)
                        .clicked()
                    {
                        intents.push(Intent::Remove(index));
                    }
                });
            });
            if let Some(message) = status.message() {
                kit::note_under(ui, message, Palette::AMBER_400);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::testing::SpyPlayback;

    const MAC: Platform = Platform::MacOs;
    const OTHER: Platform = Platform::Other;

    fn hk(key: &str, action: HotkeyAction) -> HotkeyConfig {
        HotkeyConfig {
            key: key.to_string(),
            action,
        }
    }

    fn key_event(key: egui::Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    const ID_A: &str = "11111111-1111-4111-8111-111111111111";
    const ID_B: &str = "22222222-2222-4222-8222-222222222222";

    fn catalog() -> Catalog {
        Catalog {
            sounds: vec![(ID_A.to_string(), "Thunder".to_string())],
            scenes: vec![(ID_B.to_string(), "Tavern".to_string())],
            playlists: vec![],
        }
    }

    fn idle_ui(library: &Library) -> HotkeyUi {
        HotkeyUi {
            map: HotkeyMap::build(library.hotkeys()),
            source: library.hotkeys().to_vec(),
            dispatcher: HotkeyDispatcher::new(),
            service: None,
            status: ServiceStatus::Disabled,
            capture: None,
            draft: Draft::default(),
            key_text: String::new(),
            notice: None,
        }
    }

    #[test]
    fn deleting_a_scene_makes_the_key_map_stale() {
        let mut library = Library::new();
        let tavern = library.add_scene("Tavern").expect("scene");
        let camp = library.add_scene("Camp").expect("scene");
        library
            .add_hotkey(
                "Ctrl+1",
                HotkeyAction::Scene {
                    target: tavern.to_string(),
                },
            )
            .unwrap();
        library
            .add_hotkey(
                "Ctrl+2",
                HotkeyAction::Scene {
                    target: camp.to_string(),
                },
            )
            .unwrap();
        let mut ui = idle_ui(&library);
        assert_eq!(ui.active_count(), 2);
        assert!(!ui.is_stale(library.hotkeys()));

        assert!(library.remove_scene(tavern));
        assert_eq!(library.hotkeys().len(), 1);
        // The map still holds the dead Ctrl+1 until it is rebuilt.
        assert_eq!(ui.active_count(), 2);
        assert!(ui.is_stale(library.hotkeys()));

        ui.map = HotkeyMap::build(library.hotkeys());
        ui.source = library.hotkeys().to_vec();
        assert_eq!(ui.active_count(), 1);
        assert!(!ui.is_stale(library.hotkeys()));
    }

    #[test]
    fn action_kinds_round_trip_through_build() {
        for kind in ActionKind::ALL {
            let action = kind.build(Some(ID_A)).expect("target given");
            assert_eq!(ActionKind::of(&action), kind, "{kind:?}");
            assert_eq!(action.target().is_some(), kind.needs_target(), "{kind:?}");
        }
        assert_eq!(ActionKind::Scene.build(None), None);
        assert_eq!(ActionKind::StopAll.build(None), Some(HotkeyAction::StopAll));
    }

    #[test]
    fn every_action_has_a_menu_entry() {
        // The dialog must offer everything the config schema knows.
        let all = [
            HotkeyAction::Sound { target: "x".into() },
            HotkeyAction::Scene { target: "x".into() },
            HotkeyAction::Playlist { target: "x".into() },
            HotkeyAction::StopAll,
            HotkeyAction::StopScene,
            HotkeyAction::MasterUp,
            HotkeyAction::MasterDown,
            HotkeyAction::MasterMute,
            HotkeyAction::DuckToggle,
            HotkeyAction::DuckHold,
            HotkeyAction::NextTrack,
            HotkeyAction::PrevTrack,
            HotkeyAction::PauseMusic,
        ];
        assert_eq!(all.len(), ActionKind::ALL.len());
        for action in all {
            assert!(ActionKind::ALL.contains(&ActionKind::of(&action)));
        }
    }

    #[test]
    fn labels_name_targets_and_flag_missing_ones() {
        let cat = catalog();
        assert_eq!(
            action_label(
                &HotkeyAction::Scene {
                    target: ID_B.into()
                },
                &cat
            ),
            "Scene: Tavern"
        );
        // Upper-case spelling of the same id still resolves.
        assert_eq!(
            action_label(
                &HotkeyAction::Sound {
                    target: ID_A.to_uppercase()
                },
                &cat
            ),
            "Sound: Thunder"
        );
        assert_eq!(
            action_label(
                &HotkeyAction::Playlist {
                    target: ID_A.into()
                },
                &cat
            ),
            "Playlist: (missing)"
        );
        assert_eq!(action_label(&HotkeyAction::StopAll, &cat), "Stop all");
        assert_eq!(
            action_label(&HotkeyAction::DuckHold, &cat),
            "Duck background (hold)"
        );
    }

    #[test]
    fn statuses_report_conflicts_losers_and_broken_entries() {
        let list = vec![
            hk("Ctrl+1", HotkeyAction::StopAll),        // 0: wins the key
            hk("F5", HotkeyAction::StopScene),          // 1: fine
            hk("control+1", HotkeyAction::MasterUp),    // 2: loses to 0
            hk("Ctrl+Shift", HotkeyAction::MasterDown), // 3: no key
            hk(
                "Ctrl+9",
                HotkeyAction::Scene {
                    target: ID_A.into(),
                },
            ), // 4: missing scene
            hk(
                "Ctrl+2",
                HotkeyAction::Scene {
                    target: ID_B.into(),
                },
            ), // 5: fine
        ];
        let status = row_statuses(&list, OTHER, &catalog());
        assert_eq!(status[0], RowStatus::Shadows(vec![2]));
        assert_eq!(status[1], RowStatus::Ok);
        assert_eq!(status[2], RowStatus::Shadowed(0));
        assert!(matches!(status[3], RowStatus::Invalid(_)));
        assert_eq!(status[4], RowStatus::MissingTarget);
        assert_eq!(status[5], RowStatus::Ok);

        assert!(status[0].message().unwrap().contains("#3"));
        assert!(status[2].message().unwrap().contains("#1"));
        assert_eq!(status[1].message(), None);
    }

    #[test]
    fn three_entries_on_one_key_list_both_losers() {
        let list = vec![
            hk("Ctrl+1", HotkeyAction::StopAll),
            hk("Ctrl+1", HotkeyAction::StopScene),
            hk("Ctrl+1", HotkeyAction::MasterMute),
        ];
        let status = row_statuses(&list, OTHER, &catalog());
        assert_eq!(status[0], RowStatus::Shadows(vec![1, 2]));
        assert_eq!(status[1], RowStatus::Shadowed(0));
        assert_eq!(status[2], RowStatus::Shadowed(0));
    }

    #[test]
    fn cmd_or_ctrl_conflicts_follow_the_platform() {
        let list = vec![
            hk("CmdOrCtrl+K", HotkeyAction::StopAll),
            hk("Ctrl+K", HotkeyAction::StopScene),
        ];
        // Off macOS both are Ctrl+K; on macOS they are different keys.
        assert!(row_statuses(&list, OTHER, &catalog())[0].is_problem());
        assert!(!row_statuses(&list, MAC, &catalog())[0].is_problem());
    }

    #[test]
    fn plan_add_replaces_an_existing_key_in_any_spelling() {
        let list = vec![
            hk("F5", HotkeyAction::StopAll),
            hk("Shift+Ctrl+1", HotkeyAction::StopScene),
            hk("midi:ch1:note60", HotkeyAction::MasterMute),
        ];
        let b = |t: &str| Binding::parse_for(t, OTHER).unwrap();
        assert_eq!(
            plan_add(&list, &b("ctrl+shift+1"), OTHER),
            AddPlan::Replace(1)
        );
        assert_eq!(plan_add(&list, &b("f5"), OTHER), AddPlan::Replace(0));
        assert_eq!(plan_add(&list, &b("F6"), OTHER), AddPlan::Append);
        assert_eq!(plan_add(&[], &b("F6"), OTHER), AddPlan::Append);
    }

    #[test]
    fn key_text_is_parsed_with_readable_errors() {
        assert_eq!(
            parse_key_text("ctrl + shift + 1", OTHER)
                .unwrap()
                .to_string(),
            "Ctrl+Shift+1"
        );
        assert!(parse_key_text("", OTHER).is_err());
        assert!(parse_key_text("Ctrl+A+B", OTHER)
            .unwrap_err()
            .contains("two keys"));
        assert!(parse_key_text("midi:ch1:note60", OTHER)
            .unwrap_err()
            .contains("MIDI"));
        // Numpad keys cannot be captured but can be typed.
        assert!(parse_key_text("Ctrl+Numpad1", OTHER).is_ok());
    }

    #[test]
    fn key_labels_use_platform_names() {
        assert_eq!(key_label("alt+f5", MAC), "Option+F5");
        assert_eq!(key_label("alt+f5", OTHER), "Alt+F5");
        assert_eq!(key_label("nonsense+", OTHER), "nonsense+");
    }

    #[test]
    fn global_hint_mentions_the_macos_permission() {
        assert!(global_hint(MAC).contains("Privacy & Security"));
        assert!(!global_hint(OTHER).contains("Privacy & Security"));
    }

    #[test]
    fn every_bindable_key_survives_the_egui_round_trip() {
        let mut keys: Vec<Key> = ('A'..='Z').map(Key::Letter).collect();
        keys.extend((0..=9).map(Key::Digit));
        keys.extend((1..=24).map(Key::Function));
        keys.extend(NamedKey::ALL.into_iter().map(Key::Named));
        for key in keys {
            let egui_key = key
                .to_egui()
                .unwrap_or_else(|| panic!("{key} has no egui key"));
            assert_eq!(key_from_egui(egui_key), Some(key), "{key}");
        }
        // Keys with no binding name.
        assert_eq!(key_from_egui(egui::Key::F30), None);
        assert_eq!(key_from_egui(egui::Key::Copy), None);
    }

    #[test]
    fn capture_turns_events_into_bindings() {
        let ctrl = Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let events = vec![key_event(egui::Key::Num1, true, ctrl)];
        assert_eq!(
            capture_from_events(&events, OTHER),
            Some(Captured::Binding(
                Binding::parse_for("Ctrl+1", OTHER).unwrap()
            ))
        );

        // macOS Command is `mac_cmd`; on other systems it is never reported.
        let cmd = Modifiers {
            mac_cmd: true,
            command: true,
            ..Default::default()
        };
        let events = vec![key_event(egui::Key::K, true, cmd)];
        assert_eq!(
            capture_from_events(&events, MAC),
            Some(Captured::Binding(Binding::parse_for("Cmd+K", MAC).unwrap()))
        );
        assert_eq!(
            capture_from_events(&events, OTHER),
            Some(Captured::Binding(Binding::parse_for("K", OTHER).unwrap()))
        );
    }

    #[test]
    fn capture_ignores_releases_and_repeats_and_cancels_on_escape() {
        let none = Modifiers::default();
        assert_eq!(
            capture_from_events(&[key_event(egui::Key::A, false, none)], OTHER),
            None
        );
        let repeat = Event::Key {
            key: egui::Key::A,
            physical_key: Some(egui::Key::A),
            pressed: true,
            repeat: true,
            modifiers: none,
        };
        assert_eq!(capture_from_events(&[repeat], OTHER), None);
        assert_eq!(
            capture_from_events(&[key_event(egui::Key::Escape, true, none)], OTHER),
            Some(Captured::Cancel)
        );
        // Shift+Escape can be bound.
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            capture_from_events(&[key_event(egui::Key::Escape, true, shift)], OTHER),
            Some(Captured::Binding(
                Binding::parse_for("Shift+Escape", OTHER).unwrap()
            ))
        );
        assert!(matches!(
            capture_from_events(&[key_event(egui::Key::F30, true, none)], OTHER),
            Some(Captured::Unsupported(_))
        ));
        assert_eq!(capture_from_events(&[], OTHER), None);
    }

    #[test]
    fn routes_split_ducking_and_music_from_the_dispatcher() {
        use KeyPhase::{Pressed, Released};
        assert_eq!(
            route(&HotkeyAction::DuckToggle, Pressed),
            Some(Route::Duck(DuckOp::Toggle))
        );
        assert_eq!(route(&HotkeyAction::DuckToggle, Released), None);
        assert_eq!(
            route(&HotkeyAction::DuckHold, Pressed),
            Some(Route::Duck(DuckOp::HoldStart))
        );
        assert_eq!(
            route(&HotkeyAction::DuckHold, Released),
            Some(Route::Duck(DuckOp::HoldEnd))
        );
        assert_eq!(
            route(&HotkeyAction::Playlist { target: "p".into() }, Pressed),
            Some(Route::Playlist(PlaylistCommand::Start("p".into())))
        );
        assert_eq!(
            route(&HotkeyAction::PauseMusic, Pressed),
            Some(Route::Playlist(PlaylistCommand::TogglePause))
        );
        assert_eq!(
            route(&HotkeyAction::NextTrack, Pressed),
            Some(Route::Playlist(PlaylistCommand::Next))
        );
        assert_eq!(route(&HotkeyAction::NextTrack, Released), None);
        assert_eq!(
            route(&HotkeyAction::StopAll, Pressed),
            Some(Route::Dispatch)
        );
        assert_eq!(
            route(&HotkeyAction::Scene { target: "s".into() }, Pressed),
            Some(Route::Dispatch)
        );
        // The dispatcher itself ignores releases of these.
        assert_eq!(route(&HotkeyAction::StopAll, Released), None);
    }

    #[test]
    fn dispatched_actions_reach_the_director() {
        use crate::audio::director::mock::MockCall;
        let pb = SpyPlayback::new();
        let mut director = Director::new();
        let mut dispatcher = HotkeyDispatcher::new();
        let event = HotkeyEvent {
            index: 0,
            action: HotkeyAction::MasterDown,
            phase: KeyPhase::Pressed,
            source: crate::hotkeys::Source::Local,
        };
        assert_eq!(route(&event.action, event.phase), Some(Route::Dispatch));
        dispatcher
            .handle(&mut director, &pb, &mut NoExtras, &event)
            .unwrap();
        assert!(pb
            .inner
            .calls()
            .iter()
            .any(|c| matches!(c, MockCall::SetMaster(v) if (*v - 0.95).abs() < 1e-4)));
    }

    #[test]
    fn duck_toggle_latches_and_releases_the_configured_buses() {
        let pb = SpyPlayback::new();
        let mut director = Director::new();
        run_duck_op(&mut director, &pb, DuckOp::Toggle).unwrap();
        let log = pb.duck_log();
        assert!(log.iter().any(|l| l.starts_with("duck Music")), "{log:?}");
        assert!(
            log.iter().any(|l| l.starts_with("duck Ambience")),
            "{log:?}"
        );
        assert!(!log.iter().any(|l| l.contains("Sfx")), "{log:?}");
        assert!(director.is_ducked(crate::config::Bus::Music));

        pb.clear_duck_log();
        run_duck_op(&mut director, &pb, DuckOp::Toggle).unwrap();
        assert_eq!(pb.duck_log(), vec!["release Music", "release Ambience"]);
        assert!(!director.is_ducked(crate::config::Bus::Music));
    }

    #[test]
    fn duck_hold_lasts_exactly_as_long_as_the_key() {
        let pb = SpyPlayback::new();
        let mut director = Director::new();
        run_duck_op(&mut director, &pb, DuckOp::HoldStart).unwrap();
        assert!(director.is_ducked(crate::config::Bus::Ambience));
        pb.clear_duck_log();
        run_duck_op(&mut director, &pb, DuckOp::HoldEnd).unwrap();
        assert_eq!(pb.duck_log(), vec!["release Music", "release Ambience"]);
        assert!(!director.is_ducked(crate::config::Bus::Ambience));

        // A latched toggle survives the end of a hold.
        run_duck_op(&mut director, &pb, DuckOp::Toggle).unwrap();
        run_duck_op(&mut director, &pb, DuckOp::HoldStart).unwrap();
        pb.clear_duck_log();
        run_duck_op(&mut director, &pb, DuckOp::HoldEnd).unwrap();
        assert!(pb.duck_log().is_empty());
        assert!(director.is_ducked(crate::config::Bus::Music));
    }
}

/// A headless console for tests: the real app state on a bare egui context,
/// with a real (device-less) audio engine. Nothing here may call `update` after
/// marking the library dirty (the debounced save would write the user's
/// `config.toml`).
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use crossbeam_channel::{bounded, Receiver};

    use crate::audio::AudioEngine;
    use crate::config::Config;
    use crate::gui::app::SoundSharedApp;
    use crate::gui::types::{AppState, AudioCommand, ServerCommand};
    use crate::network::GmGuard;

    pub struct Harness {
        pub app: SoundSharedApp,
        pub ctx: egui::Context,
        /// Commands the app queued for the engine thread.
        pub audio_rx: Receiver<AudioCommand>,
        pub gm: GmGuard,
        pub config: Arc<Mutex<Config>>,
        _server_rx: Receiver<ServerCommand>,
    }

    /// An app over `config`; `engine` decides whether an audio engine exists.
    pub fn harness(config: Config, engine: bool) -> Harness {
        let ctx = egui::Context::default();
        let (audio_tx, audio_rx) = bounded(64);
        let (server_tx, server_rx) = bounded(64);
        let gm = GmGuard::new(config.server.gm_token.clone());
        let config = Arc::new(Mutex::new(config));
        let state = AppState::new(audio_tx, server_tx, Arc::new(Mutex::new(0)), config.clone())
            .with_gm(gm.clone(), true);
        #[allow(clippy::arc_with_non_send_sync)]
        let engine =
            Arc::new(Mutex::new(engine.then(|| {
                AudioEngine::new().expect("audio engine without a device")
            })));
        let app = SoundSharedApp::with_context(&ctx, state, engine, audio_rx.clone());
        Harness {
            app,
            ctx,
            audio_rx,
            gm,
            config,
            _server_rx: server_rx,
        }
    }

    /// Run one frame with `events` on a tall 1280x2400 screen (nothing scrolls).
    pub fn frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        mut run: impl FnMut(&egui::Context),
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 2400.0),
            )),
            events,
            ..Default::default()
        };
        ctx.run(input, &mut run)
    }

    /// A key event without modifiers.
    pub fn key(key: egui::Key, pressed: bool) -> egui::Event {
        key_with(key, pressed, egui::Modifiers::default())
    }

    /// A key event with modifiers.
    pub fn key_with(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    /// Every piece of text painted in the frame, upper-cased, one per line.
    pub fn painted_text(output: &egui::FullOutput) -> String {
        fn walk(shape: &egui::Shape, out: &mut String) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push_str(&text.galley.text().to_uppercase());
                    out.push('\n');
                }
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = String::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    /// Run `n` frames of `run` (windows need a sizing pass) and return the
    /// painted text of the last one.
    pub fn settle(ctx: &egui::Context, n: usize, mut run: impl FnMut(&egui::Context)) -> String {
        let mut text = String::new();
        for _ in 0..n {
            text = painted_text(&frame(ctx, vec![], &mut run));
        }
        text
    }
}

#[cfg(test)]
mod ui_tests {
    use super::test_support::*;
    use super::*;
    use crate::config::Config;

    const SCENE_ID: &str = "22222222-2222-4222-8222-222222222222";

    fn config_with_hotkeys(hotkeys: Vec<HotkeyConfig>) -> Config {
        let mut config = Config::default();
        config.library.scenes.push(crate::config::SceneConfig {
            id: SCENE_ID.to_string(),
            name: "Tavern".to_string(),
            ..Default::default()
        });
        config.hotkeys = hotkeys;
        config
    }

    fn hk(key: &str, action: HotkeyAction) -> HotkeyConfig {
        HotkeyConfig {
            key: key.to_string(),
            action,
        }
    }

    #[test]
    fn a_window_hotkey_toggles_the_duck_through_the_frame_loop() {
        let mut h = harness(
            config_with_hotkeys(vec![hk("F9", HotkeyAction::DuckToggle)]),
            true,
        );
        assert!(!h.app.mix_view().ducked);
        frame(&h.ctx, vec![key(egui::Key::F9, true)], |ctx| {
            h.app.update(ctx)
        });
        assert!(h.app.mix_view().ducked);
        // The release does nothing for a toggle.
        frame(&h.ctx, vec![key(egui::Key::F9, false)], |ctx| {
            h.app.update(ctx)
        });
        assert!(h.app.mix_view().ducked);
        frame(&h.ctx, vec![key(egui::Key::F9, true)], |ctx| {
            h.app.update(ctx)
        });
        assert!(!h.app.mix_view().ducked);
    }

    #[test]
    fn a_held_hotkey_ducks_only_while_it_is_down() {
        let mut h = harness(
            config_with_hotkeys(vec![hk("F10", HotkeyAction::DuckHold)]),
            true,
        );
        frame(&h.ctx, vec![key(egui::Key::F10, true)], |ctx| {
            h.app.update(ctx)
        });
        assert!(h.app.mix_view().ducked);
        frame(&h.ctx, vec![key(egui::Key::F10, false)], |ctx| {
            h.app.update(ctx)
        });
        assert!(!h.app.mix_view().ducked);
    }

    #[test]
    fn master_hotkeys_move_the_master_volume() {
        let mut h = harness(
            config_with_hotkeys(vec![
                hk("F6", HotkeyAction::MasterDown),
                hk("F7", HotkeyAction::MasterMute),
            ]),
            true,
        );
        // egui turns a key-down of a key that is already down into a repeat,
        // so every press is released again first.
        let tap = |h: &mut Harness, k: egui::Key| {
            frame(&h.ctx, vec![key(k, true)], |ctx| h.app.update(ctx));
            frame(&h.ctx, vec![key(k, false)], |ctx| h.app.update(ctx));
        };
        // Mute, then unmute: back to the level it had.
        tap(&mut h, egui::Key::F7);
        assert_eq!(h.app.get_master_volume(), 0.0);
        tap(&mut h, egui::Key::F7);
        let level = h.app.get_master_volume();
        assert!(level > 0.0);
        // One step down.
        tap(&mut h, egui::Key::F6);
        assert!((h.app.get_master_volume() - (level - 0.05)).abs() < 0.011);
    }

    #[test]
    fn keys_are_not_fired_while_the_editor_captures() {
        let mut h = harness(
            config_with_hotkeys(vec![hk("F9", HotkeyAction::DuckToggle)]),
            true,
        );
        h.app.open_hotkeys();
        h.app.extras.hotkeys.capture = Some(CaptureTarget::Draft);
        assert!(h.app.hotkeys_capturing());
        frame(&h.ctx, vec![key(egui::Key::F9, true)], |ctx| {
            h.app.update(ctx)
        });
        assert!(!h.app.mix_view().ducked);
    }

    #[test]
    fn hotkeys_without_an_engine_do_nothing_and_do_not_panic() {
        let mut h = harness(
            config_with_hotkeys(vec![hk("F9", HotkeyAction::DuckToggle)]),
            false,
        );
        frame(&h.ctx, vec![key(egui::Key::F9, true)], |ctx| {
            h.app.update(ctx)
        });
        assert!(!h.app.mix_view().ducked);
    }

    #[test]
    fn a_failing_action_leaves_a_message_for_the_dialog() {
        // The scene does not exist in the library.
        let mut h = harness(
            config_with_hotkeys(vec![hk(
                "F8",
                HotkeyAction::Scene {
                    target: "33333333-3333-4333-8333-333333333333".into(),
                },
            )]),
            true,
        );
        frame(&h.ctx, vec![key(egui::Key::F8, true)], |ctx| {
            h.app.update(ctx)
        });
        let (message, is_error) = h.app.extras.hotkeys.notice.clone().expect("a notice");
        assert!(is_error);
        assert!(message.contains("(missing)"), "{message}");
    }

    #[test]
    fn captured_key_then_add_stores_a_canonical_binding() {
        let mut h = harness(config_with_hotkeys(vec![]), false);
        h.app.open_hotkeys();
        h.app.extras.hotkeys.draft.kind = ActionKind::StopAll;
        h.app.extras.hotkeys.capture = Some(CaptureTarget::Draft);

        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Default::default()
        };
        frame(
            &h.ctx,
            vec![key_with(egui::Key::Num7, true, ctrl_shift)],
            |ctx| show_hotkeys_dialog(ctx, &mut h.app),
        );
        assert_eq!(h.app.extras.hotkeys.capture, None);
        let captured = h.app.extras.hotkeys.draft.key.expect("a captured key");
        assert_eq!(captured.to_string(), "Ctrl+Shift+7");

        h.app.add_draft_hotkey();
        let list = h.app.library.hotkeys().to_vec();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].action, HotkeyAction::StopAll);
        assert_eq!(list[0].key, "Ctrl+Shift+7");
        // The live map follows, the draft is reset and a save is queued.
        assert_eq!(h.app.extras.hotkeys.active_count(), 1);
        assert_eq!(h.app.extras.hotkeys.draft.key, None);
        assert!(h.app.library_dirty);
    }

    #[test]
    fn adding_a_taken_key_replaces_instead_of_conflicting() {
        let mut h = harness(
            config_with_hotkeys(vec![hk("Ctrl+1", HotkeyAction::StopAll)]),
            false,
        );
        let hk_ui = &mut h.app.extras.hotkeys;
        hk_ui.draft.kind = ActionKind::MasterMute;
        hk_ui.draft.key = Binding::parse("control+1").ok();
        h.app.add_draft_hotkey();
        let list = h.app.library.hotkeys().to_vec();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].action, HotkeyAction::MasterMute);
    }

    #[test]
    fn capturing_a_row_changes_its_key_and_keeps_its_action() {
        let mut h = harness(
            config_with_hotkeys(vec![hk("F5", HotkeyAction::StopScene)]),
            false,
        );
        h.app.open_hotkeys();
        h.app.extras.hotkeys.capture = Some(CaptureTarget::Row(0));
        frame(&h.ctx, vec![key(egui::Key::F6, true)], |ctx| {
            show_hotkeys_dialog(ctx, &mut h.app)
        });
        let list = h.app.library.hotkeys().to_vec();
        assert_eq!(list[0].key, "F6");
        assert_eq!(list[0].action, HotkeyAction::StopScene);
    }

    #[test]
    fn escape_cancels_a_capture_without_changing_anything() {
        let mut h = harness(
            config_with_hotkeys(vec![hk("F5", HotkeyAction::StopScene)]),
            false,
        );
        h.app.open_hotkeys();
        h.app.extras.hotkeys.capture = Some(CaptureTarget::Row(0));
        frame(&h.ctx, vec![key(egui::Key::Escape, true)], |ctx| {
            show_hotkeys_dialog(ctx, &mut h.app)
        });
        assert_eq!(h.app.extras.hotkeys.capture, None);
        assert_eq!(h.app.library.hotkeys()[0].key, "F5");
        assert!(!h.app.library_dirty);
    }

    #[test]
    fn removing_a_row_updates_the_map() {
        let mut h = harness(
            config_with_hotkeys(vec![
                hk("F5", HotkeyAction::StopScene),
                hk("F6", HotkeyAction::StopAll),
            ]),
            false,
        );
        assert_eq!(h.app.extras.hotkeys.active_count(), 2);
        h.app.remove_hotkey_at(0);
        assert_eq!(h.app.extras.hotkeys.active_count(), 1);
        assert_eq!(h.app.library.hotkeys()[0].key, "F6");
    }

    #[test]
    fn the_dialog_lists_bindings_conflicts_and_the_global_switch() {
        let mut h = harness(
            config_with_hotkeys(vec![
                hk(
                    "Ctrl+1",
                    HotkeyAction::Scene {
                        target: SCENE_ID.into(),
                    },
                ),
                hk("ctrl+1", HotkeyAction::StopAll),
                hk(
                    "Ctrl+9",
                    HotkeyAction::Sound {
                        target: SCENE_ID.into(),
                    },
                ),
            ]),
            false,
        );
        h.app.open_hotkeys();
        let text = settle(&h.ctx, 3, |ctx| show_hotkeys_dialog(ctx, &mut h.app));
        assert!(text.contains("HOTKEYS"), "{text}");
        assert!(text.contains("GLOBAL HOTKEYS"), "{text}");
        assert!(text.contains("GLOBAL HOTKEYS ARE OFF"), "{text}");
        assert!(text.contains("SCENE: TAVERN"), "{text}");
        assert!(text.contains("STOP ALL"), "{text}");
        assert!(text.contains("CONFLICT"), "{text}");
        assert!(text.contains("SOUND: (MISSING)"), "{text}");
        assert!(text.contains("NO LONGER EXISTS"), "{text}");
        assert!(text.contains("DONE"), "{text}");
    }

    #[test]
    fn the_global_switch_off_keeps_the_service_stopped_and_saves_the_flag() {
        let mut h = harness(config_with_hotkeys(vec![]), false);
        h.app.set_global_hotkeys(&h.ctx.clone(), false);
        assert_eq!(*h.app.extras.hotkeys.status(), ServiceStatus::Disabled);
        assert!(!h.config.lock().unwrap().gui.global_hotkeys);
    }

    #[test]
    fn hotkeys_load_from_config_and_come_back_unchanged() {
        let list = vec![
            hk(
                "Ctrl+1",
                HotkeyAction::Scene {
                    target: SCENE_ID.into(),
                },
            ),
            hk("F5", HotkeyAction::StopAll),
        ];
        let h = harness(config_with_hotkeys(list.clone()), false);
        assert_eq!(h.app.library.hotkeys(), &list[..]);
        let mut written = Config::default();
        h.app.library.write_config(&mut written);
        assert_eq!(written.hotkeys, list);
    }

    #[test]
    fn escape_closes_the_dialog_unless_a_key_is_being_captured() {
        let mut h = harness(config_with_hotkeys(vec![]), false);
        h.app.open_hotkeys();
        // The shared modal handling draws the dialog.
        settle(&h.ctx, 2, |ctx| {
            crate::gui::dialog::show_dialogs(ctx, &mut h.app)
        });
        assert!(matches!(h.app.dialog, Dialog::Hotkeys));

        // While capturing, Escape belongs to the capture.
        h.app.extras.hotkeys.capture = Some(CaptureTarget::Draft);
        frame(&h.ctx, vec![key(egui::Key::Escape, true)], |ctx| {
            crate::gui::dialog::show_dialogs(ctx, &mut h.app)
        });
        assert!(matches!(h.app.dialog, Dialog::Hotkeys));
        assert_eq!(h.app.extras.hotkeys.capture, None);

        // Otherwise it closes the dialog.
        frame(&h.ctx, vec![key(egui::Key::Escape, false)], |ctx| {
            crate::gui::dialog::show_dialogs(ctx, &mut h.app)
        });
        frame(&h.ctx, vec![key(egui::Key::Escape, true)], |ctx| {
            crate::gui::dialog::show_dialogs(ctx, &mut h.app)
        });
        assert!(matches!(h.app.dialog, Dialog::None));
    }
}
