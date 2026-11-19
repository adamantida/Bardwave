//! First-run wizard: "Getting started".
//!
//! A small card in the middle of the window walks a new operator through the
//! three things the console is for:
//!
//! 1. add sounds (files, a folder, a YouTube link),
//! 2. build a mix and save it as a scene,
//! 3. start the server and give the players the link.
//!
//! The card never blocks the console: it is a plain overlay area, not a modal,
//! and every step is ticked off by the real state of the app (the library has
//! sounds, a scene exists, the server runs), not by clicking through.
//!
//! The rules (when the wizard opens by itself, which step is done) are pure
//! functions at the top of the file; the app glue and the drawing follow.
//!
//! Nothing here waits for anything: every button either opens a dialog the
//! console already has or sends one of the app's non-blocking commands.

use crate::config::Config;
use crate::gui::app::{Dialog, ServerState, ServerStatus, SoundSharedApp};
use crate::gui::groups::Library;
use crate::gui::library_ui::LibDialog;
use crate::gui::server_panel::{qr_display_size, qr_matrix, QrMatrix};
use crate::gui::theme::{
    hairline, micro_label, micro_label_accent, mono_font, panel_title, ui_font, Metrics, Palette,
};
use crate::gui::widgets::{glyph, glyph_font, paint_icon, ConsoleButton};
use egui::{Align, Align2, Layout, Rounding, Sense, Stroke, Ui, Vec2};

/// Width of the text column inside the card.
const CARD_W: f32 = 432.0;
/// Target side of the player QR code, in points.
const QR_TARGET: usize = 132;
/// Quiet zone around the QR code, in modules.
const QR_QUIET: usize = 2;

// ---------------------------------------------------------------- logic ----

/// One page of the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Step {
    #[default]
    Sounds,
    Scene,
    Share,
}

impl Step {
    /// The steps in the order they are shown.
    pub const ALL: [Step; 3] = [Step::Sounds, Step::Scene, Step::Share];

    /// Position in [`Step::ALL`], from zero.
    pub fn index(self) -> usize {
        match self {
            Step::Sounds => 0,
            Step::Scene => 1,
            Step::Share => 2,
        }
    }

    /// Heading of the page.
    pub fn title(self) -> &'static str {
        match self {
            Step::Sounds => "Add your sounds",
            Step::Scene => "Build a scene",
            Step::Share => "Share with players",
        }
    }

    /// Short name in the progress row.
    pub fn short(self) -> &'static str {
        match self {
            Step::Sounds => "Sounds",
            Step::Scene => "Scene",
            Step::Share => "Share",
        }
    }

    /// The following step, `None` on the last one.
    pub fn next(self) -> Option<Step> {
        Step::ALL.get(self.index() + 1).copied()
    }

    /// The previous step, `None` on the first one.
    pub fn prev(self) -> Option<Step> {
        self.index().checked_sub(1).map(|i| Step::ALL[i])
    }
}

/// What the app currently has, as far as the wizard cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub sounds: bool,
    pub scene: bool,
    pub server: bool,
}

impl Progress {
    /// Progress from the counts the app can read without waiting.
    pub fn new(sound_count: usize, scene_count: usize, server_running: bool) -> Self {
        Self {
            sounds: sound_count > 0,
            scene: scene_count > 0,
            server: server_running,
        }
    }

    /// Whether `step` is ticked.
    pub fn is_done(&self, step: Step) -> bool {
        match step {
            Step::Sounds => self.sounds,
            Step::Scene => self.scene,
            Step::Share => self.server,
        }
    }

    /// How many steps are ticked.
    pub fn completed(&self) -> usize {
        Step::ALL.iter().filter(|s| self.is_done(**s)).count()
    }

    /// The step to open on: the first one that is not done yet (the last one
    /// when everything is).
    pub fn first_open_step(&self) -> Step {
        Step::ALL
            .iter()
            .copied()
            .find(|s| !self.is_done(*s))
            .unwrap_or(Step::Share)
    }
}

/// What to do with the wizard when the console starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Open the card.
    Show,
    /// Stay closed (already dismissed for good).
    Hidden,
    /// Stay closed and remember it: the operator already has a library, so the
    /// wizard has nothing to teach them.
    HiddenAndDone,
}

/// The start-up rule: the wizard opens only for a fresh install, i.e. while it
/// was not dismissed and the library has no sounds. A config that already
/// carries a library (any older version) is marked done at once.
pub fn launch_decision(onboarding_done: bool, library_empty: bool) -> Launch {
    if onboarding_done {
        Launch::Hidden
    } else if library_empty {
        Launch::Show
    } else {
        Launch::HiddenAndDone
    }
}

/// Apply [`launch_decision`] to the config and the wizard state. Returns true
/// when the config changed and has to be saved.
pub fn startup(config: &mut Config, library: &Library, ui: &mut OnboardingUi) -> bool {
    ui.started = true;
    match launch_decision(config.gui.onboarding_done, library.is_empty()) {
        Launch::Show => {
            ui.visible = true;
            ui.step = Step::Sounds;
            false
        }
        Launch::Hidden => false,
        Launch::HiddenAndDone => {
            config.gui.onboarding_done = true;
            true
        }
    }
}

/// A button press on the card.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    AddSound,
    AddFolder,
    AddYouTube,
    SaveMix,
    StartServer,
    CopyLink(String),
    ToggleQr,
    Back,
    Next,
    Skip,
    Finish,
}

/// What the wizard remembers between frames.
#[derive(Default)]
pub struct OnboardingUi {
    /// The start-up rule has run.
    started: bool,
    /// The card is on screen.
    pub visible: bool,
    /// The page shown.
    pub step: Step,
    /// "Don't show again" box.
    pub dont_show: bool,
    /// The player QR code is expanded.
    show_qr: bool,
    /// The QR code and the link it was built from.
    qr: Option<(String, QrMatrix)>,
}

impl OnboardingUi {
    /// Open the card (Help menu), on the first step that is not done yet.
    pub fn open(&mut self, progress: Progress) {
        self.visible = true;
        self.step = progress.first_open_step();
    }

    /// Hide the card. Returns `remember`, i.e. whether the dismissal has to be
    /// kept in the config.
    pub fn close(&mut self, remember: bool) -> bool {
        self.visible = false;
        remember
    }

    /// Move one page forward or back; stays put at the ends.
    pub fn go(&mut self, forward: bool) {
        let target = if forward {
            self.step.next()
        } else {
            self.step.prev()
        };
        if let Some(step) = target {
            self.step = step;
        }
    }
}

/// Text of the status line under the buttons of a step.
pub fn step_status(step: Step, sounds: usize, scenes: usize, status: &ServerStatus) -> String {
    match step {
        Step::Sounds => match sounds {
            0 => "No sounds yet".to_string(),
            1 => "1 sound in the library".to_string(),
            n => format!("{n} sounds in the library"),
        },
        Step::Scene => match scenes {
            0 => "No scenes yet".to_string(),
            1 => "1 scene saved".to_string(),
            n => format!("{n} scenes saved"),
        },
        Step::Share => match status {
            ServerStatus::Running => "Server is live".to_string(),
            ServerStatus::Starting => "Starting the server".to_string(),
            ServerStatus::Error(_) => "The server did not start".to_string(),
            ServerStatus::Stopped => "Server is stopped".to_string(),
        },
    }
}

// ------------------------------------------------------------- app glue ----

impl SoundSharedApp {
    /// Current wizard progress, from state that is read without blocking.
    fn onboarding_progress(&self) -> Progress {
        let running = self.get_server_state().status == ServerStatus::Running;
        Progress::new(self.library.len(), self.library.scenes().len(), running)
    }

    /// Remember in `config.toml` (with the next flush) that the wizard is done.
    fn mark_onboarding_done(&mut self) {
        let mut config = self.app_state.config.lock().unwrap();
        if !config.gui.onboarding_done {
            config.gui.onboarding_done = true;
            drop(config);
            self.library_dirty = true;
        }
    }

    /// Whether a modal is open: the card steps aside so it never covers one.
    fn onboarding_modal_open(&self) -> bool {
        !matches!(self.dialog, Dialog::None) || !matches!(self.lib_ui.dialog, LibDialog::None)
    }

    fn apply_onboarding_action(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::AddSound => self.open_add_files(),
            Action::AddFolder => self.open_add_folder(),
            Action::AddYouTube => self.open_add_youtube(),
            Action::SaveMix => self.open_save_mix(),
            Action::StartServer => self.request_start_server(),
            Action::CopyLink(link) => {
                ctx.output_mut(|o| o.copied_text = link);
                self.set_copied_feedback(true);
            }
            Action::ToggleQr => self.onboarding.show_qr = !self.onboarding.show_qr,
            Action::Back => self.onboarding.go(false),
            Action::Next => self.onboarding.go(true),
            Action::Skip => {
                let remember = self.onboarding.dont_show;
                if self.onboarding.close(remember) {
                    self.mark_onboarding_done();
                }
            }
            Action::Finish => {
                self.onboarding.close(true);
                self.mark_onboarding_done();
            }
        }
    }
}

/// Run the start-up rule once and draw the card while it is open.
pub fn show(ctx: &egui::Context, state: &mut SoundSharedApp) {
    if !state.onboarding.started {
        let changed = {
            let mut config = state.app_state.config.lock().unwrap();
            startup(&mut config, &state.library, &mut state.onboarding)
        };
        if changed {
            state.library_dirty = true;
        }
    }
    if !state.onboarding.visible || state.onboarding_modal_open() {
        return;
    }

    let progress = state.onboarding_progress();
    let server = state.get_server_state();
    let mut action: Option<Action> = None;

    egui::Area::new(egui::Id::new("onboarding_card"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(Palette::PANEL)
                .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG))
                .rounding(Rounding::same(Metrics::R_PANEL))
                .inner_margin(egui::Margin::same(Metrics::XL))
                .show(ui, |ui| {
                    ui.set_width(CARD_W);
                    // Gaps are placed explicitly, as in the console panels.
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    action = card(ui, state, progress, &server);
                });
        });

    if let Some(action) = action {
        state.apply_onboarding_action(action, ctx);
        ctx.request_repaint();
    }
}

/// Width of the Help button: its text and the console button padding.
pub fn help_button_width(ui: &Ui) -> f32 {
    crate::gui::widgets::bar_menu_width(ui, "Help")
}

/// Entry for the top bar: a Help menu with "Getting started".
pub fn help_menu(ui: &mut Ui, state: &mut SoundSharedApp) {
    let button = crate::gui::widgets::bar_menu_button(ui, "help", "Help", |ui| {
        if ui.button("Getting started").clicked() {
            let progress = state.onboarding_progress();
            state.onboarding.open(progress);
        }
    });
    button.on_hover_text("Open the first-run guide");
}

// -------------------------------------------------------------- drawing ----

fn card(
    ui: &mut Ui,
    state: &mut SoundSharedApp,
    progress: Progress,
    server: &ServerState,
) -> Option<Action> {
    let mut action = None;
    let step = state.onboarding.step;

    ui.horizontal(|ui| {
        ui.label(micro_label_accent("Getting started"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(micro_label(format!(
                "Step {} of {}",
                step.index() + 1,
                Step::ALL.len()
            )));
        });
    });
    ui.add_space(Metrics::XS);
    ui.label(panel_title(step.title()));
    ui.add_space(Metrics::LG);

    progress_row(ui, step, progress);
    ui.add_space(Metrics::LG);
    hairline(ui);
    ui.add_space(Metrics::LG);

    match step {
        Step::Sounds => step_sounds(ui, &mut action),
        Step::Scene => step_scene(ui, state, &mut action),
        Step::Share => step_share(ui, state, server, &mut action),
    }

    ui.add_space(Metrics::SM);
    ui.label(micro_label(step_status(
        step,
        state.library.len(),
        state.library.scenes().len(),
        &server.status,
    )));

    ui.add_space(Metrics::LG);
    hairline(ui);
    ui.add_space(Metrics::MD);
    footer(ui, state, step, &mut action);
    action
}

/// Three markers with their names: ticked, current, or still ahead.
fn progress_row(ui: &mut Ui, current: Step, progress: Progress) {
    ui.horizontal(|ui| {
        for step in Step::ALL {
            let done = progress.is_done(step);
            let is_current = step == current;
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
            let painter = ui.painter();
            if done {
                painter.circle_filled(rect.center(), 9.0, Palette::ROSE_600);
                paint_icon(painter, glyph::CHECK, rect.center(), 11.0, Palette::WHITE);
            } else {
                let edge = if is_current {
                    Palette::ROSE_500
                } else {
                    Palette::BORDER_STRONG
                };
                painter.circle_stroke(rect.center(), 8.5, Stroke::new(1.0_f32, edge));
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    (step.index() + 1).to_string(),
                    mono_font(9.0),
                    if is_current {
                        Palette::WHITE
                    } else {
                        Palette::ZINC_500
                    },
                );
            }
            ui.add_space(Metrics::SM);
            let color = if is_current || done {
                Palette::WHITE
            } else {
                Palette::ZINC_500
            };
            ui.label(
                egui::RichText::new(step.short().to_uppercase())
                    .font(mono_font(Metrics::MICRO))
                    .color(color)
                    .extra_letter_spacing(1.0),
            );
            ui.add_space(Metrics::XL);
        }
    });
}

fn body(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(ui_font(13.0))
                .color(Palette::ZINC_400),
        )
        .wrap(),
    );
}

fn step_sounds(ui: &mut Ui, action: &mut Option<Action>) {
    body(
        ui,
        "Add what you want to play: single files, a whole folder, or audio from a YouTube link.",
    );
    ui.add_space(Metrics::MD);
    let width = (ui.available_width() - 2.0 * Metrics::SM) / 3.0;
    ui.horizontal(|ui| {
        if ConsoleButton::primary("Add sound")
            .glyph(glyph::PLUS)
            .small()
            .width(width)
            .id_salt("onboarding_add_sound")
            .show(ui)
            .clicked()
        {
            *action = Some(Action::AddSound);
        }
        ui.add_space(Metrics::SM);
        if ConsoleButton::ghost("Add folder")
            .glyph(glyph::FOLDER)
            .small()
            .width(width)
            .id_salt("onboarding_add_folder")
            .show(ui)
            .clicked()
        {
            *action = Some(Action::AddFolder);
        }
        ui.add_space(Metrics::SM);
        if ConsoleButton::ghost("YouTube")
            .glyph(glyph::WAVE)
            .small()
            .width(width)
            .id_salt("onboarding_add_youtube")
            .show(ui)
            .clicked()
        {
            *action = Some(Action::AddYouTube);
        }
    });
}

fn step_scene(ui: &mut Ui, state: &SoundSharedApp, action: &mut Option<Action>) {
    body(
        ui,
        "Play a few sounds and set their levels, then save the mix as a scene to bring it back with one click.",
    );
    ui.add_space(Metrics::MD);
    let has_sounds = !state.library.is_empty();
    if ConsoleButton::primary("Save current mix as scene")
        .glyph(glyph::PLUS)
        .small()
        .width(ui.available_width())
        .enabled(has_sounds)
        .tooltip("Add some sounds first")
        .id_salt("onboarding_save_mix")
        .show(ui)
        .clicked()
        && has_sounds
    {
        *action = Some(Action::SaveMix);
    }
}

fn step_share(
    ui: &mut Ui,
    state: &mut SoundSharedApp,
    server: &ServerState,
    action: &mut Option<Action>,
) {
    body(
        ui,
        "Start the server, then send the player link to everyone at the table. They open it in a browser.",
    );
    ui.add_space(Metrics::MD);

    let link = match (&server.status, server.bind_url.as_deref()) {
        (ServerStatus::Running, Some(url)) => Some(state.share_url(url)),
        _ => None,
    };

    let Some(link) = link else {
        let starting = server.status == ServerStatus::Starting;
        let label = match &server.status {
            ServerStatus::Error(_) => "Retry server",
            ServerStatus::Starting => "Starting…",
            _ => "Start server",
        };
        if ConsoleButton::primary(label)
            .glyph(glyph::BROADCAST)
            .small()
            .width(ui.available_width())
            .enabled(!starting)
            .id_salt("onboarding_start_server")
            .show(ui)
            .clicked()
            && !starting
        {
            *action = Some(Action::StartServer);
        }
        return;
    };

    // Link well + copy.
    ui.horizontal(|ui| {
        let copy_w = 96.0;
        let well_w = ui.available_width() - copy_w - Metrics::SM;
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(well_w, Metrics::BTN_H_SM), Sense::hover());
        let painter = ui.painter();
        painter.rect(
            rect,
            Rounding::same(Metrics::R_SM),
            Palette::WELL,
            Stroke::new(1.0_f32, Palette::BORDER),
        );
        let text = crate::gui::widgets::elide(&link, 40);
        let galley = painter.layout_no_wrap(text, glyph_font(11.0), Palette::ROSE_500);
        painter.galley(
            egui::pos2(
                rect.left() + Metrics::MD,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            Palette::ROSE_500,
        );
        ui.add_space(Metrics::SM);
        if ConsoleButton::ghost("Copy")
            .glyph(glyph::COPY)
            .small()
            .width(copy_w)
            .id_salt("onboarding_copy")
            .show(ui)
            .clicked()
        {
            *action = Some(Action::CopyLink(link.clone()));
        }
    });
    if state.copied_feedback() {
        ui.add_space(Metrics::XS);
        ui.label(
            egui::RichText::new("COPIED TO CLIPBOARD")
                .font(glyph_font(9.0))
                .color(Palette::ROSE_400)
                .extra_letter_spacing(1.0),
        );
    }

    ui.add_space(Metrics::SM);
    let qr_label = if state.onboarding.show_qr {
        "Hide QR code"
    } else {
        "Show QR code"
    };
    if ConsoleButton::ghost(qr_label)
        .small()
        .width(ui.available_width())
        .id_salt("onboarding_qr")
        .show(ui)
        .clicked()
    {
        *action = Some(Action::ToggleQr);
    }
    if state.onboarding.show_qr {
        ui.add_space(Metrics::SM);
        paint_qr(ui, &mut state.onboarding, &link);
    }
}

/// The player link as a QR code on a white tile (centred).
fn paint_qr(ui: &mut Ui, onboarding: &mut OnboardingUi, link: &str) {
    if onboarding.qr.as_ref().map(|(l, _)| l.as_str()) != Some(link) {
        onboarding.qr = qr_matrix(link).ok().map(|m| (link.to_string(), m));
    }
    let Some((_, matrix)) = onboarding.qr.as_ref() else {
        ui.label(micro_label("QR code unavailable for this link"));
        return;
    };
    let modules = matrix.width + 2 * QR_QUIET;
    let side = qr_display_size(modules, QR_TARGET);
    let scale = side / modules as f32;
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, Rounding::ZERO, egui::Color32::WHITE);
        for y in 0..matrix.width {
            for x in 0..matrix.width {
                if matrix.is_dark(x, y) {
                    let min = rect.min
                        + Vec2::new((x + QR_QUIET) as f32 * scale, (y + QR_QUIET) as f32 * scale);
                    painter.rect_filled(
                        egui::Rect::from_min_size(min, Vec2::splat(scale)),
                        Rounding::ZERO,
                        egui::Color32::BLACK,
                    );
                }
            }
        }
    });
}

/// "Don't show again" on the left, Skip / Back / Next on the right.
fn footer(ui: &mut Ui, state: &mut SoundSharedApp, step: Step, action: &mut Option<Action>) {
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut state.onboarding.dont_show,
            egui::RichText::new("Don't show again")
                .font(ui_font(12.0))
                .color(Palette::ZINC_400),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let last = step.next().is_none();
            let primary = if last {
                ConsoleButton::primary("Done")
            } else {
                ConsoleButton::primary("Next")
            };
            if primary
                .small()
                .width(76.0)
                .id_salt("onboarding_next")
                .show(ui)
                .clicked()
            {
                *action = Some(if last { Action::Finish } else { Action::Next });
            }
            ui.add_space(Metrics::SM);
            if ConsoleButton::ghost("Back")
                .small()
                .width(64.0)
                .enabled(step.prev().is_some())
                .id_salt("onboarding_back")
                .show(ui)
                .clicked()
                && step.prev().is_some()
            {
                *action = Some(Action::Back);
            }
            ui.add_space(Metrics::SM);
            if ConsoleButton::ghost("Skip")
                .small()
                .width(64.0)
                .tooltip("Close the guide. Reopen it from Help.")
                .id_salt("onboarding_skip")
                .show(ui)
                .clicked()
            {
                *action = Some(Action::Skip);
            }
        });
    });
}

// ---------------------------------------------------------------- tests ----

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_CONFIG_WITH_LIBRARY: &str = r#"
[server]
port = 8080
host = "0.0.0.0"

[audio]
[logging]

[[library.sounds]]
path = '/music/hit.wav'
"#;

    fn config_from(text: &str) -> Config {
        toml::from_str(text).expect("parse config")
    }

    #[test]
    fn steps_walk_in_order_and_stop_at_the_ends() {
        assert_eq!(Step::Sounds.prev(), None);
        assert_eq!(Step::Sounds.next(), Some(Step::Scene));
        assert_eq!(Step::Scene.next(), Some(Step::Share));
        assert_eq!(Step::Share.next(), None);
        assert_eq!(Step::Share.prev(), Some(Step::Scene));
        for (i, step) in Step::ALL.iter().enumerate() {
            assert_eq!(step.index(), i);
        }
    }

    #[test]
    fn progress_follows_the_app_state() {
        let none = Progress::new(0, 0, false);
        assert_eq!(none.completed(), 0);
        assert_eq!(none.first_open_step(), Step::Sounds);

        let sounds = Progress::new(3, 0, false);
        assert!(sounds.is_done(Step::Sounds));
        assert!(!sounds.is_done(Step::Scene));
        assert_eq!(sounds.first_open_step(), Step::Scene);

        let live = Progress::new(3, 1, true);
        assert_eq!(live.completed(), 3);
        assert_eq!(live.first_open_step(), Step::Share);

        // The server can run before anything else is done.
        let only_server = Progress::new(0, 0, true);
        assert!(only_server.is_done(Step::Share));
        assert_eq!(only_server.first_open_step(), Step::Sounds);
    }

    #[test]
    fn decision_table() {
        assert_eq!(launch_decision(false, true), Launch::Show);
        assert_eq!(launch_decision(false, false), Launch::HiddenAndDone);
        assert_eq!(launch_decision(true, true), Launch::Hidden);
        assert_eq!(launch_decision(true, false), Launch::Hidden);
    }

    #[test]
    fn fresh_install_shows_the_wizard() {
        let mut config = Config::default();
        assert!(!config.gui.onboarding_done);
        let library = Library::from_full_config(&config);
        let mut ui = OnboardingUi::default();

        let changed = startup(&mut config, &library, &mut ui);

        assert!(!changed, "nothing to save yet");
        assert!(ui.visible);
        assert_eq!(ui.step, Step::Sounds);
        assert!(!config.gui.onboarding_done);
    }

    #[test]
    fn old_config_with_a_library_skips_the_wizard_and_is_marked_done() {
        // A v1.4.0-style config: no onboarding flag, a saved library.
        let mut config = config_from(OLD_CONFIG_WITH_LIBRARY);
        assert!(!config.gui.onboarding_done);
        let library = Library::from_full_config(&config);
        assert!(!library.is_empty());
        let mut ui = OnboardingUi::default();

        let changed = startup(&mut config, &library, &mut ui);

        assert!(changed, "the flag has to be saved");
        assert!(!ui.visible);
        assert!(config.gui.onboarding_done);
    }

    #[test]
    fn dismissed_wizard_stays_closed_even_with_an_empty_library() {
        let mut config = config_from(
            r#"
[server]
port = 8080
host = "0.0.0.0"

[audio]
[logging]

[gui]
onboarding_done = true
"#,
        );
        let library = Library::from_full_config(&config);
        assert!(library.is_empty());
        let mut ui = OnboardingUi::default();

        assert!(!startup(&mut config, &library, &mut ui));
        assert!(!ui.visible);
    }

    #[test]
    fn reopening_lands_on_the_first_open_step() {
        let mut ui = OnboardingUi::default();
        ui.open(Progress::new(2, 0, false));
        assert!(ui.visible);
        assert_eq!(ui.step, Step::Scene);

        ui.open(Progress::new(2, 1, true));
        assert_eq!(ui.step, Step::Share);
    }

    #[test]
    fn navigation_clamps_and_close_reports_whether_to_remember() {
        let mut ui = OnboardingUi::default();
        ui.go(false);
        assert_eq!(ui.step, Step::Sounds);
        ui.go(true);
        ui.go(true);
        ui.go(true);
        assert_eq!(ui.step, Step::Share);
        ui.visible = true;
        assert!(!ui.close(false));
        assert!(!ui.visible);
        assert!(ui.close(true));
    }

    #[test]
    fn status_lines() {
        assert_eq!(
            step_status(Step::Sounds, 0, 0, &ServerStatus::Stopped),
            "No sounds yet"
        );
        assert_eq!(
            step_status(Step::Sounds, 1, 0, &ServerStatus::Stopped),
            "1 sound in the library"
        );
        assert_eq!(
            step_status(Step::Scene, 1, 4, &ServerStatus::Stopped),
            "4 scenes saved"
        );
        assert_eq!(
            step_status(Step::Share, 1, 1, &ServerStatus::Running),
            "Server is live"
        );
    }

    #[test]
    fn onboarding_flag_round_trips_and_defaults_to_false() {
        let mut config = Config::default();
        assert!(!config.gui.onboarding_done);
        let text = toml::to_string_pretty(&config).expect("serialize");
        assert!(
            !text.contains("onboarding_done"),
            "default stays out of the file"
        );

        config.gui.onboarding_done = true;
        let text = toml::to_string_pretty(&config).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert!(back.gui.onboarding_done);
    }

    #[test]
    fn card_draws_on_a_fresh_console_without_touching_the_config() {
        use crate::gui::hotkeys_ui::test_support::{frame, harness};

        let mut h = harness(Config::default(), false);
        let ctx = h.ctx.clone();
        let out = frame(&ctx, vec![], |ctx| {
            show(ctx, &mut h.app);
        });
        assert!(h.app.onboarding.visible);
        assert!(!out.shapes.is_empty(), "the card painted something");
        assert!(!h.app.library_dirty, "no config write for a fresh install");
    }

    #[test]
    fn existing_library_never_opens_the_card() {
        use crate::gui::hotkeys_ui::test_support::harness;

        // Only the pure rule runs here: the app-level path flags the config
        // dirty and the debounced save would write a real config.toml.
        let h = harness(config_from(OLD_CONFIG_WITH_LIBRARY), false);
        assert_eq!(
            launch_decision(false, h.app.library.is_empty()),
            Launch::HiddenAndDone
        );
        assert!(!h.app.onboarding.visible);
    }
}
