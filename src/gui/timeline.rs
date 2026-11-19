//! Timers in the console: the "Timers" dialog and the glue that runs fired
//! timers against the Director.
//!
//! The definitions are `[[timers]]` in `config.toml` ([`TimerConfig`]); the
//! countdowns live in a [`TimelineRunner`] thread (see `audio::timeline`) that
//! the dialog only talks to through non-blocking calls: controls go over a
//! channel, the display reads a snapshot copy, fired events are drained once
//! per frame by [`SoundSharedApp::run_timers`] and run on the GUI thread
//! (which owns the audio engine) through `Director::run_timer_action`.
//!
//! Everything that decides what is shown (countdown text, row tone, button
//! states, form validation) is a plain function over plain data so it can be
//! unit-tested without a window.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use egui::{Context, RichText, Ui, Vec2};
use uuid::Uuid;

use crate::audio::timeline::{
    TimelineRunner, TimelineSnapshot, TimelineStatus, TimerEvent, TimerPhase, TimerView, Wake,
};
use crate::config::{TimerAction, TimerConfig, TimerTrigger, MIN_TIMER_MS};
use crate::gui::app::{mirror_playing, sync_director, Dialog, SoundSharedApp};
use crate::gui::hotkeys_ui::Catalog;
use crate::gui::theme::{
    alpha, hairline, micro_label, micro_label_accent, panel_title, Metrics, Palette,
};
use crate::gui::widgets::{elide, glyph, glyph_font, ConsoleButton, IconButton};

/// Width of the Timers dialog.
const DIALOG_W: f32 = 560.0;

/// How many log lines the dialog keeps.
const LOG_LINES: usize = 40;

/// Longest timer name.
pub const MAX_NAME_CHARS: usize = 60;

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

/// `MM:SS`, or `H:MM:SS` from an hour up. Rounds up, so a countdown shows
/// `00:00` only when the timer is actually due.
pub fn format_countdown(ms: u64) -> String {
    let secs = ms.div_ceil(1000);
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Minutes and seconds for the interval fields (whole seconds, minutes may
/// exceed 59).
pub fn split_interval(ms: u64) -> (u32, u32) {
    let secs = ms / 1000;
    (
        u32::try_from(secs / 60).unwrap_or(u32::MAX),
        (secs % 60) as u32,
    )
}

/// The inverse of [`split_interval`].
pub fn join_interval(minutes: u32, seconds: u32) -> u64 {
    (u64::from(minutes) * 60 + u64::from(seconds)) * 1000
}

/// "After 10:00" / "Every 30:00".
pub fn trigger_label(trigger: &TimerTrigger) -> String {
    match trigger {
        TimerTrigger::AfterMs(ms) => format!("After {}", format_countdown(*ms)),
        TimerTrigger::EveryMs(ms) => format!("Every {}", format_countdown(*ms)),
    }
}

/// Which trigger the form builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// Once, after the interval.
    After,
    /// Repeatedly.
    Every,
}

impl TriggerKind {
    /// Both, in menu order.
    pub const ALL: [TriggerKind; 2] = [TriggerKind::After, TriggerKind::Every];

    /// Menu text.
    pub fn label(self) -> &'static str {
        match self {
            Self::After => "Once, after",
            Self::Every => "Repeat every",
        }
    }

    fn of(trigger: &TimerTrigger) -> Self {
        match trigger {
            TimerTrigger::AfterMs(_) => Self::After,
            TimerTrigger::EveryMs(_) => Self::Every,
        }
    }

    fn build(self, ms: u64) -> TimerTrigger {
        match self {
            Self::After => TimerTrigger::AfterMs(ms),
            Self::Every => TimerTrigger::EveryMs(ms),
        }
    }
}

/// The kind of a timer action, for the form's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    /// [`TimerAction::ActivateScene`].
    Scene,
    /// [`TimerAction::StopScene`].
    StopScene,
    /// [`TimerAction::PlayStinger`].
    Stinger,
    /// [`TimerAction::StopAll`].
    StopAll,
    /// [`TimerAction::StartPlaylist`].
    Playlist,
    /// [`TimerAction::StopPlaylist`].
    StopMusic,
    /// [`TimerAction::NextTrack`].
    NextTrack,
    /// [`TimerAction::PauseMusic`].
    PauseMusic,
    /// [`TimerAction::ResumeMusic`].
    ResumeMusic,
}

impl ActionKind {
    /// Every kind, in menu order.
    pub const ALL: [ActionKind; 9] = [
        Self::Scene,
        Self::Stinger,
        Self::StopAll,
        Self::StopScene,
        Self::Playlist,
        Self::StopMusic,
        Self::NextTrack,
        Self::PauseMusic,
        Self::ResumeMusic,
    ];

    /// Menu text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Scene => "Activate scene",
            Self::StopScene => "Stop scene",
            Self::Stinger => "Play stinger",
            Self::StopAll => "Stop all (fade out)",
            Self::Playlist => "Start playlist",
            Self::StopMusic => "Stop music",
            Self::NextTrack => "Next track",
            Self::PauseMusic => "Pause music",
            Self::ResumeMusic => "Resume music",
        }
    }

    /// Whether the action points at a library item.
    pub fn needs_target(self) -> bool {
        matches!(self, Self::Scene | Self::Stinger | Self::Playlist)
    }

    /// The kind of an existing action.
    pub fn of(action: &TimerAction) -> Self {
        match action {
            TimerAction::ActivateScene { .. } => Self::Scene,
            TimerAction::StopScene => Self::StopScene,
            TimerAction::PlayStinger { .. } => Self::Stinger,
            TimerAction::StopAll { .. } => Self::StopAll,
            TimerAction::StartPlaylist { .. } => Self::Playlist,
            TimerAction::StopPlaylist => Self::StopMusic,
            TimerAction::NextTrack => Self::NextTrack,
            TimerAction::PauseMusic => Self::PauseMusic,
            TimerAction::ResumeMusic => Self::ResumeMusic,
        }
    }

    /// Build the action; `None` when a required target is missing.
    pub fn build(self, target: Option<&str>, fade_ms: u32) -> Option<TimerAction> {
        let target = || target.map(str::trim).filter(|t| !t.is_empty());
        Some(match self {
            Self::Scene => TimerAction::ActivateScene {
                scene: target()?.to_string(),
            },
            Self::Stinger => TimerAction::PlayStinger {
                sound: target()?.to_string(),
            },
            Self::Playlist => TimerAction::StartPlaylist {
                playlist: target()?.to_string(),
            },
            Self::StopScene => TimerAction::StopScene,
            Self::StopAll => TimerAction::StopAll { fade_ms },
            Self::StopMusic => TimerAction::StopPlaylist,
            Self::NextTrack => TimerAction::NextTrack,
            Self::PauseMusic => TimerAction::PauseMusic,
            Self::ResumeMusic => TimerAction::ResumeMusic,
        })
    }
}

/// Ids are UUID strings; compare them without caring about case or spaces.
fn same_id(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn name_in<'a>(list: &'a [(String, String)], id: &str) -> Option<&'a str> {
    list.iter()
        .find(|(candidate, _)| same_id(candidate, id))
        .map(|(_, name)| name.as_str())
}

/// One line describing an action, with the library names filled in
/// ("Activate scene Alarm"). A target that is gone reads `(missing)`.
pub fn action_summary(action: &TimerAction, catalog: &Catalog) -> String {
    let with_name = |label: &str, list: &[(String, String)], id: &str| match name_in(list, id) {
        Some(name) => format!("{label} {name}"),
        None => format!("{label} (missing)"),
    };
    match action {
        TimerAction::ActivateScene { scene } => with_name("Activate scene", &catalog.scenes, scene),
        TimerAction::PlayStinger { sound } => with_name("Play stinger", &catalog.sounds, sound),
        TimerAction::StartPlaylist { playlist } => {
            with_name("Start playlist", &catalog.playlists, playlist)
        }
        TimerAction::StopAll { fade_ms } if *fade_ms > 0 => {
            format!("Stop all, fade {:.1}s", f64::from(*fade_ms) / 1000.0)
        }
        other => ActionKind::of(other).label().to_string(),
    }
}

/// Why the target of an action cannot be found in the library.
pub fn target_problem(action: &TimerAction, catalog: &Catalog) -> Option<String> {
    let (what, list, id) = match action {
        TimerAction::ActivateScene { scene } => ("Scene", &catalog.scenes, scene),
        TimerAction::PlayStinger { sound } => ("Sound", &catalog.sounds, sound),
        TimerAction::StartPlaylist { playlist } => ("Playlist", &catalog.playlists, playlist),
        _ => return None,
    };
    name_in(list, id)
        .is_none()
        .then(|| format!("{what} no longer exists in the library"))
}

/// How a row is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Waiting for Start.
    Idle,
    /// Counting down.
    Armed,
    /// Counting down and fires first: highlighted.
    Next,
    /// A one-shot that has fired.
    Done,
    /// Switched off.
    Off,
    /// Cannot run.
    Problem,
}

/// What one row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowView {
    /// Timer name.
    pub name: String,
    /// "After 10:00 · Activate scene Alarm".
    pub detail: String,
    /// Big text on the right.
    pub countdown: String,
    /// Colour class.
    pub tone: Tone,
    /// Why the timer will not do what it says, if it will not.
    pub warning: Option<String>,
    /// Whether the checkbox is on.
    pub enabled: bool,
}

/// Build the row of one timer. `live` is the timer's entry in the snapshot
/// (absent only for the one frame between an edit and the next snapshot).
pub fn row_view(
    cfg: &TimerConfig,
    live: Option<&TimerView>,
    is_next: bool,
    catalog: &Catalog,
) -> RowView {
    let phase = live.map_or(TimerPhase::Idle, |view| view.phase);
    let remaining = live.and_then(|view| view.remaining_ms);
    let runs = live.map_or(0, |view| view.runs);

    let problem = cfg
        .problem()
        .map(|why| format!("Cannot run: {why}"))
        .or_else(|| target_problem(&cfg.action, catalog));
    let tone = match phase {
        _ if !cfg.enabled => Tone::Off,
        TimerPhase::Disabled => Tone::Off,
        TimerPhase::Invalid => Tone::Problem,
        TimerPhase::Done => Tone::Done,
        TimerPhase::Armed if is_next => Tone::Next,
        TimerPhase::Armed => Tone::Armed,
        TimerPhase::Idle if problem.is_some() => Tone::Problem,
        TimerPhase::Idle => Tone::Idle,
    };
    let countdown = match tone {
        Tone::Off => "OFF".to_string(),
        Tone::Done => "DONE".to_string(),
        Tone::Problem if cfg.problem().is_some() => "--:--".to_string(),
        Tone::Armed | Tone::Next => remaining.map_or("--:--".to_string(), format_countdown),
        Tone::Idle | Tone::Problem => "--:--".to_string(),
    };

    let mut detail = format!(
        "{} \u{b7} {}",
        trigger_label(&cfg.trigger),
        action_summary(&cfg.action, catalog)
    );
    if runs > 0 && cfg.trigger.repeats() {
        detail.push_str(&format!(" \u{b7} fired {runs}x"));
    }
    RowView {
        name: display_name(cfg, catalog),
        detail,
        countdown,
        tone,
        warning: problem,
        enabled: cfg.enabled,
    }
}

/// The name shown for a timer: its own, or one made from what it does.
pub fn display_name(cfg: &TimerConfig, catalog: &Catalog) -> String {
    if cfg.name.trim().is_empty() {
        auto_name(&cfg.trigger, &cfg.action, catalog)
    } else {
        cfg.name.trim().to_string()
    }
}

fn auto_name(trigger: &TimerTrigger, action: &TimerAction, catalog: &Catalog) -> String {
    format!(
        "{}: {}",
        trigger_label(trigger),
        action_summary(action, catalog)
    )
}

/// State of the three transport buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controls {
    /// "Start" or "Resume".
    pub start_label: &'static str,
    /// Start / Resume can be pressed.
    pub start_enabled: bool,
    /// Pause can be pressed.
    pub pause_enabled: bool,
    /// Reset can be pressed.
    pub reset_enabled: bool,
}

/// Which buttons work in a status.
pub fn controls(status: TimelineStatus) -> Controls {
    Controls {
        start_label: if status == TimelineStatus::Paused {
            "Resume"
        } else {
            "Start"
        },
        start_enabled: status != TimelineStatus::Running,
        pause_enabled: status == TimelineStatus::Running,
        reset_enabled: status != TimelineStatus::Stopped,
    }
}

/// "Stopped", "Running 12:34", "Paused 12:34".
pub fn status_line(snapshot: &TimelineSnapshot) -> String {
    match snapshot.status {
        TimelineStatus::Stopped => "Stopped".to_string(),
        TimelineStatus::Running => format!("Running {}", clock_text(snapshot.clock_ms)),
        TimelineStatus::Paused => format!("Paused {}", clock_text(snapshot.clock_ms)),
    }
}

/// Elapsed time as `MM:SS` (rounded down: it counts up).
fn clock_text(ms: u64) -> String {
    format_countdown(ms - ms % 1000)
}

/// Text of the button that opens the dialog: the nearest countdown while the
/// timers run, so it can be read without opening anything.
pub fn button_label(snapshot: &TimelineSnapshot) -> String {
    let next = snapshot
        .next
        .and_then(|index| snapshot.timers.get(index))
        .and_then(|view| view.remaining_ms);
    match (snapshot.status, next) {
        (TimelineStatus::Running, Some(ms)) => format!("Timers {}", format_countdown(ms)),
        (TimelineStatus::Paused, Some(ms)) => format!("Timers {} II", format_countdown(ms)),
        _ => "Timers".to_string(),
    }
}

/// Draft of the create / edit form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerForm {
    /// Id of the timer being edited; `None` for a new one.
    pub editing: Option<String>,
    /// Name field (may be empty: a name is made up).
    pub name: String,
    /// Once or repeating.
    pub trigger: TriggerKind,
    /// Interval, minutes part.
    pub minutes: u32,
    /// Interval, seconds part.
    pub seconds: u32,
    /// What it does.
    pub kind: ActionKind,
    /// Chosen library item, for the kinds that need one.
    pub target: Option<String>,
    /// Fade length for "stop all", in seconds.
    pub fade_secs: u32,
    /// Whether the edited timer is enabled (kept when saving).
    pub enabled: bool,
    /// Validation message under the form.
    pub error: Option<String>,
}

impl Default for TimerForm {
    fn default() -> Self {
        Self {
            editing: None,
            name: String::new(),
            trigger: TriggerKind::After,
            minutes: 10,
            seconds: 0,
            kind: ActionKind::Scene,
            target: None,
            fade_secs: 2,
            enabled: true,
            error: None,
        }
    }
}

impl TimerForm {
    /// A form that edits `cfg`.
    pub fn from_config(cfg: &TimerConfig) -> Self {
        let (minutes, seconds) = split_interval(cfg.trigger.ms());
        let fade_ms = match cfg.action {
            TimerAction::StopAll { fade_ms } => fade_ms,
            _ => 2000,
        };
        Self {
            editing: Some(cfg.id.clone()),
            name: cfg.name.clone(),
            trigger: TriggerKind::of(&cfg.trigger),
            minutes,
            seconds,
            kind: ActionKind::of(&cfg.action),
            target: cfg.action.target().map(str::to_string),
            fade_secs: fade_ms.div_ceil(1000),
            enabled: cfg.enabled,
            error: None,
        }
    }

    /// Validate and build the timer.
    pub fn to_config(&self, catalog: &Catalog) -> Result<TimerConfig, String> {
        let ms = join_interval(self.minutes, self.seconds);
        if ms < 1000.max(MIN_TIMER_MS) {
            return Err("Interval must be at least 1 second".to_string());
        }
        if self.name.chars().count() > MAX_NAME_CHARS {
            return Err(format!("Name is limited to {MAX_NAME_CHARS} characters"));
        }
        let action = self
            .kind
            .build(self.target.as_deref(), self.fade_secs.saturating_mul(1000))
            .ok_or_else(|| "Choose what the timer should start".to_string())?;
        let trigger = self.trigger.build(ms);
        let name = if self.name.trim().is_empty() {
            auto_name(&trigger, &action, catalog)
        } else {
            self.name.trim().to_string()
        };
        Ok(TimerConfig {
            id: self
                .editing
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            name,
            trigger,
            action,
            enabled: self.enabled,
        })
    }
}

/// Replace the timer with `timer.id`, or append it.
pub fn upsert(timers: &mut Vec<TimerConfig>, timer: TimerConfig) {
    match timers.iter_mut().find(|existing| existing.id == timer.id) {
        Some(existing) => *existing = timer,
        None => timers.push(timer),
    }
}

/// One line of the dialog's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Text.
    pub text: String,
    /// Whether it is a warning.
    pub warning: bool,
}

/// The log lines for a fired timer and its warnings (empty warnings = one
/// plain line).
pub fn log_lines(event: &TimerEvent, summary: &str, warnings: &[String]) -> Vec<LogLine> {
    let name = if event.name.trim().is_empty() {
        "Timer"
    } else {
        event.name.trim()
    };
    let mut lines = Vec::new();
    if warnings.is_empty() {
        lines.push(LogLine {
            text: format!("{name}: {summary}"),
            warning: false,
        });
    } else {
        for warning in warnings {
            lines.push(LogLine {
                text: format!("{name}: {warning}"),
                warning: true,
            });
        }
    }
    if event.missed > 0 {
        lines.push(LogLine {
            text: format!("{name}: skipped {} late repeat(s)", event.missed),
            warning: true,
        });
    }
    lines
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Everything the timers add to the window.
pub struct TimersUi {
    runner: TimelineRunner,
    /// Copy of the runner's state, refreshed once per frame.
    pub snapshot: TimelineSnapshot,
    /// Open create / edit form.
    pub form: Option<TimerForm>,
    /// What fired and what went wrong, newest first.
    pub log: VecDeque<LogLine>,
}

impl TimersUi {
    /// Start the timeline thread with the saved definitions. Fired timers wake
    /// the window through `ctx`.
    pub fn start(ctx: &Context, timers: Vec<TimerConfig>) -> Self {
        let ctx = ctx.clone();
        let wake: Wake = Arc::new(move || ctx.request_repaint());
        let runner = TimelineRunner::start(timers, Some(wake));
        let snapshot = runner.snapshot();
        Self {
            runner,
            snapshot,
            form: None,
            log: VecDeque::new(),
        }
    }

    fn push_log(&mut self, line: LogLine) {
        self.log.push_front(line);
        self.log.truncate(LOG_LINES);
    }
}

// ---------------------------------------------------------------------------
// App glue
// ---------------------------------------------------------------------------

impl SoundSharedApp {
    /// Open the Timers dialog.
    pub fn open_timers(&mut self) {
        self.extras.timers.form = None;
        self.dialog = Dialog::Timers;
    }

    /// The saved timer definitions.
    pub fn timer_configs(&self) -> Vec<TimerConfig> {
        self.app_state.config.lock().unwrap().timers.clone()
    }

    /// Save new definitions: config (flushed with the next library save) and
    /// the running timeline.
    pub(crate) fn commit_timers(&mut self, timers: Vec<TimerConfig>) {
        self.app_state.config.lock().unwrap().timers = timers.clone();
        self.library_dirty = true;
        self.extras.timers.runner.set_timers(timers);
    }

    /// Once per frame: refresh the countdown copy and run the timers that
    /// fired. Never blocks; a missing engine or a vanished scene becomes a
    /// warning line instead of an error.
    pub(crate) fn run_timers(&mut self) {
        self.extras.timers.snapshot = self.extras.timers.runner.snapshot();
        let mut events = Vec::new();
        while let Some(event) = self.extras.timers.runner.try_event() {
            events.push(event);
        }
        if events.is_empty() {
            return;
        }
        let catalog = Catalog::from_library(&self.library);
        let mut lines: Vec<LogLine> = Vec::new();
        {
            let guard = self.audio_engine.lock().unwrap();
            match guard.as_ref() {
                None => {
                    for event in &events {
                        let warnings = ["the audio engine is not running".to_string()];
                        lines.extend(log_lines(event, "", &warnings));
                    }
                }
                Some(engine) => {
                    sync_director(&mut self.director, &self.library, &self.sounds);
                    for event in &events {
                        let run = self.director.run_timer_action(engine, &event.action);
                        let summary = action_summary(&event.action, &catalog);
                        lines.extend(log_lines(event, &summary, &run.warnings));
                    }
                    mirror_playing(&self.sounds, engine);
                }
            }
        }
        for line in lines {
            if line.warning {
                tracing::warn!("Timer {}", line.text);
            } else {
                tracing::info!("Timer {}", line.text);
            }
            self.extras.timers.push_log(line);
        }
    }

    /// How soon the window should repaint for the countdowns.
    pub(crate) fn timers_repaint_hint(&self) -> Duration {
        if self.extras.timers.snapshot.status != TimelineStatus::Running {
            return Duration::from_secs(3600);
        }
        if matches!(self.dialog, Dialog::Timers) {
            Duration::from_millis(200)
        } else {
            Duration::from_millis(500)
        }
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// What the dialog asks for this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Intent {
    Start,
    Pause,
    Reset,
    New,
    Edit(String),
    Delete(String),
    Toggle(String, bool),
    Save,
    CancelForm,
    Close,
}

/// The "Timers" button of the network column.
pub fn timers_button(ui: &mut Ui, state: &mut SoundSharedApp) {
    let snapshot = &state.extras.timers.snapshot;
    let label = button_label(snapshot);
    let count = state.app_state.config.lock().unwrap().timers.len();
    let tooltip = format!(
        "Session timers ({count}): \"in 10 minutes start the Alarm scene\", \
         \"every 30 minutes ring the bell\".\nThey run only while the app is open."
    );
    ui.add_space(Metrics::SM);
    if ConsoleButton::ghost(label)
        .width(ui.available_width())
        .height(Metrics::BTN_H)
        .tooltip(tooltip)
        .id_salt("mix_timers")
        .show(ui)
        .clicked()
    {
        state.open_timers();
    }
}

fn tone_colors(tone: Tone) -> (egui::Color32, egui::Color32, egui::Color32) {
    // (row fill, countdown colour, border)
    match tone {
        Tone::Next => (
            alpha(Palette::ROSE_600, 28),
            Palette::ROSE_400,
            Palette::ROSE_600,
        ),
        Tone::Armed => (Palette::ROW, Palette::WHITE, Palette::ROW),
        Tone::Idle => (Palette::ROW, Palette::ZINC_400, Palette::ROW),
        Tone::Done => (Palette::ROW, Palette::ZINC_600, Palette::ROW),
        Tone::Off => (Palette::ROW, Palette::ZINC_600, Palette::ROW),
        Tone::Problem => (
            alpha(Palette::AMBER_400, 18),
            Palette::AMBER_400,
            Palette::ROW,
        ),
    }
}

fn timer_row(
    ui: &mut Ui,
    index: usize,
    cfg: &TimerConfig,
    row: &RowView,
    intents: &mut Vec<Intent>,
) {
    let (fill, count_color, border) = tone_colors(row.tone);
    egui::Frame::none()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, border))
        .rounding(egui::Rounding::same(Metrics::R_SM))
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let mut enabled = row.enabled;
                if ui
                    .checkbox(&mut enabled, "")
                    .on_hover_text("Enabled timers are armed by Start")
                    .changed()
                {
                    intents.push(Intent::Toggle(cfg.id.clone(), enabled));
                }
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(elide(&row.name, 38))
                            .font(glyph_font(12.0))
                            .color(Palette::WHITE),
                    );
                    ui.label(
                        RichText::new(elide(&row.detail, 52))
                            .font(glyph_font(Metrics::MICRO))
                            .color(Palette::ZINC_400),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let salt = format!("timer_del_{index}");
                    if IconButton::new(glyph::TRASH)
                        .size(24.0)
                        .font_size(11.0)
                        .tooltip("Delete this timer")
                        .id_salt(&salt)
                        .show(ui)
                        .clicked()
                    {
                        intents.push(Intent::Delete(cfg.id.clone()));
                    }
                    let salt = format!("timer_edit_{index}");
                    if IconButton::new(glyph::EDIT)
                        .size(24.0)
                        .font_size(11.0)
                        .tooltip("Edit this timer")
                        .id_salt(&salt)
                        .show(ui)
                        .clicked()
                    {
                        intents.push(Intent::Edit(cfg.id.clone()));
                    }
                    ui.add_space(Metrics::SM);
                    ui.label(
                        RichText::new(&row.countdown)
                            .font(glyph_font(20.0))
                            .color(count_color),
                    );
                });
            });
            if let Some(message) = &row.warning {
                ui.add(
                    egui::Label::new(
                        RichText::new(message)
                            .font(glyph_font(Metrics::MICRO))
                            .color(Palette::AMBER_400),
                    )
                    .wrap(),
                );
            }
        });
}

fn draw_form(ui: &mut Ui, form: &mut TimerForm, catalog: &Catalog, intents: &mut Vec<Intent>) {
    ui.label(micro_label(if form.editing.is_some() {
        "Edit timer"
    } else {
        "New timer"
    }));
    ui.add_space(Metrics::SM);

    ui.horizontal(|ui| {
        ui.add_sized([68.0, 18.0], egui::Label::new(micro_label("Name")));
        ui.add(
            egui::TextEdit::singleline(&mut form.name)
                .hint_text("optional")
                .desired_width(f32::INFINITY)
                .font(glyph_font(12.0))
                .margin(egui::Margin::symmetric(8.0, 5.0)),
        );
    });
    ui.add_space(Metrics::SM);

    ui.horizontal(|ui| {
        ui.add_sized([68.0, 18.0], egui::Label::new(micro_label("When")));
        egui::ComboBox::from_id_salt("timer_form_trigger")
            .width(120.0)
            .selected_text(form.trigger.label())
            .show_ui(ui, |ui| {
                for kind in TriggerKind::ALL {
                    ui.selectable_value(&mut form.trigger, kind, kind.label());
                }
            });
        ui.add(
            egui::DragValue::new(&mut form.minutes)
                .range(0..=999)
                .speed(0.2)
                .suffix(" min"),
        );
        ui.add(
            egui::DragValue::new(&mut form.seconds)
                .range(0..=59)
                .speed(0.5)
                .suffix(" s"),
        );
    });
    ui.add_space(Metrics::SM);

    ui.horizontal(|ui| {
        ui.add_sized([68.0, 18.0], egui::Label::new(micro_label("Action")));
        egui::ComboBox::from_id_salt("timer_form_action")
            .width(190.0)
            .selected_text(form.kind.label())
            .show_ui(ui, |ui| {
                for kind in ActionKind::ALL {
                    if ui
                        .selectable_value(&mut form.kind, kind, kind.label())
                        .changed()
                    {
                        form.target = None;
                    }
                }
            });
        if form.kind.needs_target() {
            let options: &[(String, String)] = match form.kind {
                ActionKind::Scene => &catalog.scenes,
                ActionKind::Stinger => &catalog.sounds,
                _ => &catalog.playlists,
            };
            let current = form
                .target
                .as_deref()
                .and_then(|id| name_in(options, id))
                .unwrap_or(if options.is_empty() {
                    "(none in the library)"
                } else {
                    "Choose..."
                })
                .to_string();
            egui::ComboBox::from_id_salt("timer_form_target")
                .width(ui.available_width().max(120.0))
                .selected_text(elide(&current, 26))
                .show_ui(ui, |ui| {
                    for (id, name) in options {
                        let selected = form.target.as_deref().is_some_and(|t| same_id(t, id));
                        if ui.selectable_label(selected, name).clicked() {
                            form.target = Some(id.clone());
                        }
                    }
                });
        } else if form.kind == ActionKind::StopAll {
            ui.add(
                egui::DragValue::new(&mut form.fade_secs)
                    .range(0..=120)
                    .suffix(" s fade"),
            );
        }
    });

    if let Some(message) = &form.error {
        ui.add_space(Metrics::XS);
        crate::gui::dialog::error_line(ui, message);
    }
    ui.add_space(Metrics::SM);
    ui.horizontal(|ui| {
        if ConsoleButton::primary("Save timer")
            .height(Metrics::BTN_H_SM)
            .id_salt("timer_form_save")
            .show(ui)
            .clicked()
        {
            intents.push(Intent::Save);
        }
        if ConsoleButton::ghost("Cancel")
            .height(Metrics::BTN_H_SM)
            .id_salt("timer_form_cancel")
            .show(ui)
            .clicked()
        {
            intents.push(Intent::CancelForm);
        }
    });
}

/// The Timers modal. Safe to call every frame while `Dialog::Timers` is open.
pub fn show_timers_dialog(ctx: &Context, state: &mut SoundSharedApp) {
    let catalog = Catalog::from_library(&state.library);
    let timers = state.timer_configs();
    let snapshot = state.extras.timers.snapshot.clone();
    let mut form = state.extras.timers.form.take();
    let mut intents: Vec<Intent> = Vec::new();
    let buttons = controls(snapshot.status);

    egui::Window::new("timers_modal")
        .id(egui::Id::new("timers_modal"))
        .order(egui::Order::Foreground)
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .frame(crate::gui::theme::panel_frame())
        .show(ctx, |ui| {
            ui.set_width(DIALOG_W);
            ui.label(micro_label_accent("Session"));
            ui.add_space(2.0);
            ui.label(panel_title("Timers"));
            ui.add_space(2.0);
            ui.label(micro_label(
                "Timers run only while the app is open. Press Start when the game begins.",
            ));
            ui.add_space(Metrics::MD);

            // ---- Transport ----
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Metrics::SM;
                if ConsoleButton::primary(buttons.start_label)
                    .glyph(glyph::PLAY)
                    .height(Metrics::BTN_H_SM)
                    .enabled(buttons.start_enabled)
                    .id_salt("timers_start")
                    .show(ui)
                    .clicked()
                {
                    intents.push(Intent::Start);
                }
                if ConsoleButton::ghost("Pause timers")
                    .height(Metrics::BTN_H_SM)
                    .enabled(buttons.pause_enabled)
                    .tooltip("Freeze every countdown; Resume continues where they stopped")
                    .id_salt("timers_pause")
                    .show(ui)
                    .clicked()
                {
                    intents.push(Intent::Pause);
                }
                if ConsoleButton::ghost("Reset")
                    .height(Metrics::BTN_H_SM)
                    .enabled(buttons.reset_enabled)
                    .tooltip("Stop and disarm every timer; Start counts from zero again")
                    .id_salt("timers_reset")
                    .show(ui)
                    .clicked()
                {
                    intents.push(Intent::Reset);
                }
                ui.add_space(Metrics::SM);
                ui.label(
                    RichText::new(status_line(&snapshot).to_uppercase())
                        .font(glyph_font(Metrics::MICRO))
                        .color(match snapshot.status {
                            TimelineStatus::Running => Palette::ROSE_400,
                            TimelineStatus::Paused => Palette::AMBER_400,
                            TimelineStatus::Stopped => Palette::ZINC_500,
                        }),
                );
            });

            ui.add_space(Metrics::MD);
            hairline(ui);
            ui.add_space(Metrics::MD);

            // ---- List ----
            ui.label(micro_label(format!("Timers ({})", timers.len())));
            ui.add_space(Metrics::SM);
            egui::ScrollArea::vertical()
                .id_salt("timers_scroll")
                .max_height(240.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = Metrics::SM;
                    if timers.is_empty() {
                        ui.label(micro_label(
                            "No timers yet. Add one below, for example: after 10:00 activate a scene.",
                        ));
                    }
                    for (index, cfg) in timers.iter().enumerate() {
                        let live = snapshot.timers.iter().find(|view| view.id == cfg.id);
                        let is_next = snapshot
                            .next
                            .and_then(|i| snapshot.timers.get(i))
                            .is_some_and(|view| view.id == cfg.id);
                        let row = row_view(cfg, live, is_next, &catalog);
                        timer_row(ui, index, cfg, &row, &mut intents);
                    }
                });

            ui.add_space(Metrics::MD);
            hairline(ui);
            ui.add_space(Metrics::MD);

            // ---- Form or "new" button ----
            match form.as_mut() {
                Some(form) => draw_form(ui, form, &catalog, &mut intents),
                None => {
                    if ConsoleButton::ghost("New timer")
                        .glyph(glyph::PLUS)
                        .height(Metrics::BTN_H_SM)
                        .id_salt("timers_new")
                        .show(ui)
                        .clicked()
                    {
                        intents.push(Intent::New);
                    }
                }
            }

            // ---- Log ----
            if !state.extras.timers.log.is_empty() {
                ui.add_space(Metrics::MD);
                hairline(ui);
                ui.add_space(Metrics::SM);
                ui.label(micro_label("Fired"));
                ui.add_space(Metrics::XS);
                egui::ScrollArea::vertical()
                    .id_salt("timers_log")
                    .max_height(84.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for line in &state.extras.timers.log {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&line.text)
                                        .font(glyph_font(Metrics::MICRO))
                                        .color(if line.warning {
                                            Palette::AMBER_400
                                        } else {
                                            Palette::ZINC_400
                                        }),
                                )
                                .wrap(),
                            );
                        }
                    });
            }

            ui.add_space(Metrics::MD);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ConsoleButton::ghost("Close")
                    .height(Metrics::BTN_H_SM)
                    .id_salt("timers_close")
                    .show(ui)
                    .clicked()
                {
                    intents.push(Intent::Close);
                }
            });
        });

    apply_intents(state, &catalog, timers, &mut form, intents);
    // The form survives repaints; a closed dialog drops it.
    if matches!(state.dialog, Dialog::Timers) {
        state.extras.timers.form = form;
    }
}

fn apply_intents(
    state: &mut SoundSharedApp,
    catalog: &Catalog,
    mut timers: Vec<TimerConfig>,
    form: &mut Option<TimerForm>,
    intents: Vec<Intent>,
) {
    for intent in intents {
        match intent {
            Intent::Start => state.extras.timers.runner.start_timers(),
            Intent::Pause => state.extras.timers.runner.pause(),
            Intent::Reset => state.extras.timers.runner.reset(),
            Intent::New => *form = Some(TimerForm::default()),
            Intent::Edit(id) => {
                if let Some(cfg) = timers.iter().find(|t| t.id == id) {
                    *form = Some(TimerForm::from_config(cfg));
                }
            }
            Intent::Delete(id) => {
                timers.retain(|t| t.id != id);
                if form.as_ref().and_then(|f| f.editing.as_deref()) == Some(id.as_str()) {
                    *form = None;
                }
                state.commit_timers(timers.clone());
            }
            Intent::Toggle(id, enabled) => {
                if let Some(cfg) = timers.iter_mut().find(|t| t.id == id) {
                    cfg.enabled = enabled;
                    state.commit_timers(timers.clone());
                }
            }
            Intent::Save => {
                if let Some(draft) = form.as_mut() {
                    match draft.to_config(catalog) {
                        Ok(cfg) => {
                            upsert(&mut timers, cfg);
                            state.commit_timers(timers.clone());
                            *form = None;
                        }
                        Err(message) => draft.error = Some(message),
                    }
                }
            }
            Intent::CancelForm => *form = None,
            Intent::Close => {
                *form = None;
                state.close_dialog();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::timeline::Timeline;

    const SCENE: &str = "11111111-1111-4111-8111-111111111111";
    const SOUND: &str = "22222222-2222-4222-8222-222222222222";

    fn catalog() -> Catalog {
        Catalog {
            sounds: vec![(SOUND.into(), "Bell".into())],
            scenes: vec![(SCENE.into(), "Alarm".into())],
            playlists: Vec::new(),
        }
    }

    fn scene_timer(trigger: TimerTrigger) -> TimerConfig {
        TimerConfig {
            id: "t1".into(),
            name: "Alarm".into(),
            trigger,
            action: TimerAction::ActivateScene {
                scene: SCENE.into(),
            },
            enabled: true,
        }
    }

    #[test]
    fn countdown_formats() {
        assert_eq!(format_countdown(0), "00:00");
        assert_eq!(format_countdown(1), "00:01");
        assert_eq!(format_countdown(59_001), "01:00");
        assert_eq!(format_countdown(600_000), "10:00");
        assert_eq!(format_countdown(3_599_000), "59:59");
        assert_eq!(format_countdown(3_600_000), "1:00:00");
        assert_eq!(format_countdown(3_725_000), "1:02:05");
    }

    #[test]
    fn interval_split_and_join_roundtrip() {
        assert_eq!(split_interval(90_000), (1, 30));
        assert_eq!(split_interval(3_600_000), (60, 0));
        assert_eq!(join_interval(1, 30), 90_000);
        assert_eq!(join_interval(0, 0), 0);
        for ms in [1000, 61_000, 1_800_000, 5_999_000] {
            let (m, s) = split_interval(ms);
            assert_eq!(join_interval(m, s), ms);
        }
    }

    #[test]
    fn action_summary_names_targets_and_flags_missing_ones() {
        let c = catalog();
        let scene = TimerAction::ActivateScene {
            scene: SCENE.to_uppercase(),
        };
        assert_eq!(action_summary(&scene, &c), "Activate scene Alarm");
        let gone = TimerAction::ActivateScene {
            scene: "nope".into(),
        };
        assert_eq!(action_summary(&gone, &c), "Activate scene (missing)");
        assert!(target_problem(&gone, &c).is_some());
        assert!(target_problem(&scene, &c).is_none());
        assert!(target_problem(&TimerAction::StopScene, &c).is_none());
        assert_eq!(
            action_summary(&TimerAction::StopAll { fade_ms: 2500 }, &c),
            "Stop all, fade 2.5s"
        );
        assert_eq!(
            action_summary(&TimerAction::StopAll { fade_ms: 0 }, &c),
            "Stop all (fade out)"
        );
    }

    #[test]
    fn controls_follow_the_status() {
        let stopped = controls(TimelineStatus::Stopped);
        assert!(stopped.start_enabled && !stopped.pause_enabled && !stopped.reset_enabled);
        assert_eq!(stopped.start_label, "Start");
        let running = controls(TimelineStatus::Running);
        assert!(!running.start_enabled && running.pause_enabled && running.reset_enabled);
        let paused = controls(TimelineStatus::Paused);
        assert!(paused.start_enabled && !paused.pause_enabled && paused.reset_enabled);
        assert_eq!(paused.start_label, "Resume");
    }

    #[test]
    fn rows_show_countdown_and_highlight_the_nearest() {
        let c = catalog();
        let mut cfg = scene_timer(TimerTrigger::AfterMs(600_000));
        let mut timeline = Timeline::new(vec![cfg.clone()]);

        let idle = row_view(&cfg, timeline.snapshot().timers.first(), false, &c);
        assert_eq!(idle.tone, Tone::Idle);
        assert_eq!(idle.countdown, "--:--");
        assert_eq!(idle.detail, "After 10:00 \u{b7} Activate scene Alarm");

        timeline.start(0);
        timeline.tick(90_000);
        let snap = timeline.snapshot();
        let near = row_view(&cfg, snap.timers.first(), snap.next == Some(0), &c);
        assert_eq!(near.tone, Tone::Next);
        assert_eq!(near.countdown, "08:30");
        let other = row_view(&cfg, snap.timers.first(), false, &c);
        assert_eq!(other.tone, Tone::Armed);

        cfg.enabled = false;
        let off = row_view(&cfg, snap.timers.first(), false, &c);
        assert_eq!((off.tone, off.countdown.as_str()), (Tone::Off, "OFF"));
    }

    #[test]
    fn rows_flag_done_and_repeat_counts() {
        let c = catalog();
        let cfg = scene_timer(TimerTrigger::AfterMs(1000));
        let mut timeline = Timeline::new(vec![cfg.clone()]);
        timeline.start(0);
        timeline.tick(1000);
        let done = row_view(&cfg, timeline.snapshot().timers.first(), false, &c);
        assert_eq!((done.tone, done.countdown.as_str()), (Tone::Done, "DONE"));

        let every = scene_timer(TimerTrigger::EveryMs(1000));
        let mut timeline = Timeline::new(vec![every.clone()]);
        timeline.start(0);
        timeline.tick(2000);
        let row = row_view(&every, timeline.snapshot().timers.first(), true, &c);
        assert!(row.detail.ends_with("fired 1x"), "{}", row.detail);
    }

    #[test]
    fn a_deleted_scene_is_a_warning_on_the_row() {
        let mut cfg = scene_timer(TimerTrigger::AfterMs(1000));
        cfg.action = TimerAction::ActivateScene {
            scene: "deleted".into(),
        };
        let row = row_view(&cfg, None, false, &catalog());
        assert_eq!(row.tone, Tone::Problem);
        assert!(row.warning.unwrap().contains("Scene"));
    }

    #[test]
    fn button_label_shows_the_nearest_countdown_only_while_live() {
        let cfg = scene_timer(TimerTrigger::AfterMs(600_000));
        let mut timeline = Timeline::new(vec![cfg]);
        assert_eq!(button_label(&timeline.snapshot()), "Timers");
        timeline.start(0);
        timeline.tick(60_000);
        assert_eq!(button_label(&timeline.snapshot()), "Timers 09:00");
        timeline.pause(60_000);
        assert_eq!(button_label(&timeline.snapshot()), "Timers 09:00 II");
        timeline.reset();
        assert_eq!(button_label(&timeline.snapshot()), "Timers");
    }

    #[test]
    fn status_line_counts_up() {
        let mut timeline = Timeline::new(vec![scene_timer(TimerTrigger::AfterMs(600_000))]);
        assert_eq!(status_line(&timeline.snapshot()), "Stopped");
        timeline.start(0);
        timeline.tick(61_500);
        assert_eq!(status_line(&timeline.snapshot()), "Running 01:01");
        timeline.pause(61_500);
        assert_eq!(status_line(&timeline.snapshot()), "Paused 01:01");
    }

    #[test]
    fn form_builds_a_timer_and_validates() {
        let c = catalog();
        let mut form = TimerForm::default();
        assert_eq!(
            form.to_config(&c).unwrap_err(),
            "Choose what the timer should start"
        );
        form.target = Some(SCENE.into());
        let cfg = form.to_config(&c).unwrap();
        assert_eq!(cfg.trigger, TimerTrigger::AfterMs(600_000));
        assert_eq!(cfg.name, "After 10:00: Activate scene Alarm");
        assert!(Uuid::parse_str(&cfg.id).is_ok());
        assert!(cfg.enabled);

        form.minutes = 0;
        form.seconds = 0;
        assert!(form.to_config(&c).unwrap_err().contains("at least"));

        form.minutes = 30;
        form.trigger = TriggerKind::Every;
        form.kind = ActionKind::Stinger;
        form.target = Some(SOUND.into());
        form.name = "  Bell  ".into();
        let cfg = form.to_config(&c).unwrap();
        assert_eq!(cfg.name, "Bell");
        assert_eq!(cfg.trigger, TimerTrigger::EveryMs(1_800_000));
        assert_eq!(
            cfg.action,
            TimerAction::PlayStinger {
                sound: SOUND.into()
            }
        );

        form.kind = ActionKind::StopAll;
        form.fade_secs = 5;
        assert_eq!(
            form.to_config(&c).unwrap().action,
            TimerAction::StopAll { fade_ms: 5000 }
        );
    }

    #[test]
    fn form_roundtrips_an_existing_timer_and_keeps_its_id() {
        let cfg = TimerConfig {
            id: "keep-me".into(),
            name: "Bell".into(),
            trigger: TimerTrigger::EveryMs(90_000),
            action: TimerAction::PlayStinger {
                sound: SOUND.into(),
            },
            enabled: false,
        };
        let form = TimerForm::from_config(&cfg);
        assert_eq!((form.minutes, form.seconds), (1, 30));
        assert_eq!(form.to_config(&catalog()).unwrap(), cfg);
    }

    #[test]
    fn every_action_kind_roundtrips() {
        for kind in ActionKind::ALL {
            let action = kind.build(Some("id"), 3000).unwrap();
            assert_eq!(ActionKind::of(&action), kind);
            assert_eq!(kind.needs_target(), action.target().is_some());
            assert!(kind.build(None, 0).is_some() != kind.needs_target());
        }
    }

    #[test]
    fn upsert_replaces_by_id() {
        let mut list = vec![scene_timer(TimerTrigger::AfterMs(1000))];
        let mut changed = list[0].clone();
        changed.name = "New".into();
        upsert(&mut list, changed);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "New");
        let mut other = list[0].clone();
        other.id = "t2".into();
        upsert(&mut list, other);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn log_lines_carry_warnings_and_missed_repeats() {
        let event = TimerEvent {
            id: "t".into(),
            name: "Bell".into(),
            action: TimerAction::StopScene,
            run: 2,
            missed: 3,
            at_ms: 0,
        };
        let ok = log_lines(&event, "Stop scene", &[]);
        assert_eq!(ok[0].text, "Bell: Stop scene");
        assert!(!ok[0].warning);
        assert!(ok[1].text.contains("skipped 3"));
        let bad = log_lines(&event, "x", &["scene gone".to_string()]);
        assert!(bad[0].warning && bad[0].text == "Bell: scene gone");
    }

    // ---- through the window ----

    use crate::config::Config;
    use crate::gui::hotkeys_ui::test_support::{frame, harness, painted_text};

    #[test]
    fn dialog_lists_timers_and_edits_persist_in_the_config() {
        let config = Config {
            timers: vec![scene_timer(TimerTrigger::AfterMs(600_000))],
            ..Config::default()
        };
        let mut h = harness(config, false);
        h.app.open_timers();
        // A new window sizes itself on its first frame and paints on the second.
        frame(&h.ctx, Vec::new(), |ctx| {
            show_timers_dialog(ctx, &mut h.app);
        });
        let out = frame(&h.ctx, Vec::new(), |ctx| {
            show_timers_dialog(ctx, &mut h.app);
        });
        let text = painted_text(&out);
        assert!(text.contains("TIMERS"), "{text}");
        assert!(text.contains("PAUSE TIMERS"), "{text}");
        assert!(text.contains("RESET"), "{text}");
        assert!(text.contains("ALARM"), "{text}");

        // Deleting through the same path the trash button uses.
        let mut form = None;
        let timers = h.app.timer_configs();
        apply_intents(
            &mut h.app,
            &Catalog::default(),
            timers,
            &mut form,
            vec![Intent::Delete("t1".into())],
        );
        assert!(h.config.lock().unwrap().timers.is_empty());
        assert!(h.app.library_dirty);
    }

    #[test]
    fn new_timer_form_opens_and_reports_validation_errors() {
        let mut h = harness(Config::default(), false);
        h.app.open_timers();
        let mut form = None;
        let timers = h.app.timer_configs();
        apply_intents(
            &mut h.app,
            &Catalog::default(),
            timers.clone(),
            &mut form,
            vec![Intent::New],
        );
        assert!(form.is_some());
        // No target chosen: saving is refused and the form stays open.
        apply_intents(
            &mut h.app,
            &Catalog::default(),
            timers,
            &mut form,
            vec![Intent::Save],
        );
        let form = form.expect("form stays open");
        assert!(form.error.is_some());
        assert!(h.config.lock().unwrap().timers.is_empty());
        h.app.extras.timers.form = Some(form);
        let ctx = h.ctx.clone();
        frame(&ctx, Vec::new(), |ctx| show_timers_dialog(ctx, &mut h.app));
        let out = frame(&ctx, Vec::new(), |ctx| show_timers_dialog(ctx, &mut h.app));
        let text = painted_text(&out);
        assert!(text.contains("SAVE TIMER"), "{text}");
        assert!(text.contains("CHOOSE WHAT THE TIMER"), "{text}");
    }

    #[test]
    fn the_network_column_has_a_timers_button() {
        let mut h = harness(Config::default(), false);
        let out = frame(&h.ctx, Vec::new(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                crate::gui::server_panel::server_panel(ui, &mut h.app, egui::vec2(340.0, 2000.0));
            });
        });
        assert!(painted_text(&out).contains("TIMERS"));
    }

    #[test]
    fn fired_timer_with_a_vanished_scene_warns_instead_of_failing() {
        let config = Config {
            timers: vec![TimerConfig {
                action: TimerAction::ActivateScene {
                    scene: "deleted".into(),
                },
                ..scene_timer(TimerTrigger::AfterMs(100))
            }],
            ..Config::default()
        };
        // No engine in this harness: the event must still be consumed and
        // reported, not dropped or panicked on.
        let mut h = harness(config, false);
        h.app.extras.timers.runner.start_timers();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while h.app.extras.timers.log.is_empty() && std::time::Instant::now() < deadline {
            h.app.run_timers();
            std::thread::sleep(Duration::from_millis(30));
        }
        let line = h.app.extras.timers.log.front().expect("a log line");
        assert!(line.warning, "{line:?}");
        assert!(line.text.contains("engine"), "{line:?}");
    }
}
