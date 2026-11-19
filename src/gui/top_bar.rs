//! Top command bar — brand lockup, master volume, live server status.
//!
//! 64px tall, `#0A0A0A`, hairline bottom border, 24px horizontal padding:
//! brand left · master volume centre-left · live signal right.

use crate::gui::app::{ServerStatus, SoundSharedApp};
use crate::gui::theme::{micro_label, wordmark, Metrics, Palette};
use crate::gui::widgets::{glyph, level_slider, signal_dot};
use egui::{Align, Layout, Rect, Rounding, Sense, Ui, Vec2};

/// Width of the master volume slider in the roomy bar.
const MASTER_SLIDER_W: f32 = 150.0;
/// Width of the master volume slider in the compact bar.
const MASTER_SLIDER_W_COMPACT: f32 = 110.0;
/// Below this window width the bar drops its optional text (the brand caption,
/// the server line) and tightens its gaps, so the live status never gets
/// squeezed out at the 960 px minimum window. The full bar needs about
/// 1150 px.
const COMPACT_BELOW: f32 = 1180.0;

/// How much room the command bar has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarDensity {
    Full,
    Compact,
}

impl BarDensity {
    /// Gap between the bar's groups on each side of a divider.
    pub fn group_gap(self) -> f32 {
        match self {
            Self::Full => Metrics::XL,
            Self::Compact => Metrics::MD,
        }
    }

    /// Width of the master volume slider.
    pub fn master_slider_w(self) -> f32 {
        match self {
            Self::Full => MASTER_SLIDER_W,
            Self::Compact => MASTER_SLIDER_W_COMPACT,
        }
    }
}

/// Density for a bar of `width` points.
pub fn bar_density(width: f32) -> BarDensity {
    if width < COMPACT_BELOW {
        BarDensity::Compact
    } else {
        BarDensity::Full
    }
}

/// Renders the top command bar.
pub fn top_bar(ui: &mut Ui, state: &mut SoundSharedApp) {
    let bar_rect = ui.max_rect();
    ui.painter()
        .rect_filled(bar_rect, Rounding::ZERO, Palette::APP);

    let density = bar_density(bar_rect.width());
    let gap = density.group_gap();

    // Reserve the full bar and lay the groups out on one row.
    ui.allocate_ui_with_layout(
        ui.available_size(),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(Metrics::PANEL_PAD);
            brand(ui, density);
            ui.add_space(gap);
            vertical_divider(ui);
            ui.add_space(gap);
            master_volume(ui, state, density.master_slider_w());
            ui.add_space(gap);
            vertical_divider(ui);
            ui.add_space(gap);
            quick_clear(ui, state);
            ui.add_space(gap);
            vertical_divider(ui);
            ui.add_space(gap);
            crate::gui::recorder_ui::top_bar_controls(ui, state);
            ui.add_space(Metrics::BLOCK_GAP);
            crate::gui::profiles_ui::profile_menu(ui, state);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(Metrics::PANEL_PAD);
                live_status(ui, state, density);
                ui.add_space(Metrics::XL);
                vertical_divider(ui);
                ui.add_space(Metrics::XL);
                crate::gui::onboarding::help_menu(ui, state);
            });
        },
    );

    // Bottom hairline.
    let line = Rect::from_min_size(
        egui::pos2(bar_rect.left(), bar_rect.bottom() - 1.0),
        Vec2::new(bar_rect.width(), 1.0),
    );
    ui.painter()
        .rect_filled(line, Rounding::ZERO, Palette::BORDER);
}

/// Brand lockup: rose signal mark + wordmark + `BROADCAST CONSOLE` subtitle.
fn brand(ui: &mut Ui, density: BarDensity) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, Rounding::same(2.0), Palette::ROSE_600);
        crate::gui::widgets::paint_icon(
            ui.painter(),
            glyph::WAVE,
            rect.center(),
            13.8,
            Palette::WHITE,
        );

        ui.add_space(Metrics::SM);

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = Metrics::LOCKUP_GAP;
            ui.label(wordmark("Bardwave"));
            if density == BarDensity::Full {
                ui.label(micro_label("Broadcast Console").size(9.0));
            }
        });
    });
}

/// Master volume: micro label, 150px slider, mono readout.
fn master_volume(ui: &mut Ui, state: &mut SoundSharedApp, slider_w: f32) {
    ui.horizontal(|ui| {
        ui.label(micro_label("Master"));
        ui.add_space(Metrics::LABEL_GAP);

        let mut value = state.get_master_volume();
        let resp = level_slider(ui, &mut value, slider_w, "master_top");
        if resp.changed() {
            state.set_master_volume(value);
        }

        ui.add_space(Metrics::SM);
        ui.label(
            egui::RichText::new(format!("{:.0}%", value * 100.0))
                .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                .color(Palette::WHITE),
        );
    });
}

/// What is running right now, as far as the "Stop all" button cares.
///
/// Sounds are not the only thing a panic has to silence: the active scene (its
/// delayed starts and emitters), the music layer and the local preview all keep
/// making noise or scheduling it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoardActivity {
    /// At least one sound row is playing.
    pub sounds: bool,
    /// A scene is active (its emitters and delayed starts are live).
    pub scene: bool,
    /// A playlist plays or is paused.
    pub music: bool,
    /// The headphone preview is running.
    pub preview: bool,
}

impl BoardActivity {
    /// Anything at all that a stop-all would change.
    pub fn any(self) -> bool {
        self.sounds || self.scene || self.music || self.preview
    }
}

/// Visual state of the "Stop all" button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanicState {
    /// Nothing plays: muted, but still readable and still clickable.
    Idle,
    /// Something plays: the button is lit.
    Live,
}

/// Pick the button state from what is running.
pub fn panic_state(activity: BoardActivity) -> PanicState {
    if activity.any() {
        PanicState::Live
    } else {
        PanicState::Idle
    }
}

/// Tooltip of the button for a state.
pub fn panic_tooltip(state: PanicState) -> &'static str {
    match state {
        PanicState::Live => "Stop all sounds immediately (panic)",
        PanicState::Idle => "Stop all sounds immediately (panic) - nothing is playing",
    }
}

/// Colours of the "Stop all" button for a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanicColors {
    pub fill: egui::Color32,
    pub border: egui::Color32,
    pub text: egui::Color32,
    pub glyph: egui::Color32,
}

/// Colours of the button.
///
/// Live: rose-on-dark like a playing row, white on rose while hovered. Idle:
/// transparent with a hairline border; the text stays zinc-400 so it reads at
/// 4.5:1 on the bar, and the missing fill and rose make it look inactive.
pub fn panic_colors(state: PanicState, hovered: bool) -> PanicColors {
    let (fill, border, text, glyph) = match (state, hovered) {
        (PanicState::Live, false) => (
            Palette::ROSE_950,
            Palette::ROSE_500,
            Palette::ROSE_400,
            Palette::ROSE_400,
        ),
        (PanicState::Live, true) => (
            Palette::ROSE_600,
            Palette::ROSE_500,
            Palette::WHITE,
            Palette::WHITE,
        ),
        (PanicState::Idle, false) => (
            Palette::TRANSPARENT,
            Palette::BORDER,
            Palette::ZINC_400,
            Palette::ZINC_500,
        ),
        (PanicState::Idle, true) => (
            Palette::ROW,
            Palette::BORDER_STRONG,
            Palette::ZINC_300,
            Palette::ZINC_400,
        ),
    };
    PanicColors {
        fill,
        border,
        text,
        glyph,
    }
}

/// Collect what is running from the app (a few flags; the music state is the
/// copy the frame already observed, so nothing is cloned or blocked on).
fn board_activity(state: &SoundSharedApp) -> BoardActivity {
    BoardActivity {
        sounds: state.sounds.lock().unwrap().values().any(|s| s.playing),
        scene: state.director.active_scene().is_some(),
        music: crate::gui::music::is_active(state.music.live()),
        preview: state.lib_ui.preview.is_some(),
    }
}

/// "Stop all": the panic control of the command bar.
///
/// The click always goes out, also when the button looks idle: a panic control
/// that ignores a press because the display thinks nothing plays can never be
/// trusted. The command goes through the Director (see
/// `SoundSharedApp::stop_all_sounds`), so the active scene, emitters, delayed
/// starts, the music layer and the preview stop with the sounds.
fn quick_clear(ui: &mut Ui, state: &mut SoundSharedApp) {
    let panic = panic_state(board_activity(state));
    let font = crate::gui::theme::mono_font(Metrics::MICRO);
    let label = "STOP ALL";

    let measure = ui
        .painter()
        .layout_no_wrap(label.to_string(), font.clone(), Palette::ZINC_400);
    let icon = 12.0;
    let pad = Metrics::MD;
    let width = pad + icon + Metrics::SM + measure.size().x + pad;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, Metrics::BTN_H_SM), Sense::click());
    let mut resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        resp.request_focus();
    }
    if resp.fake_primary_click {
        resp.clicked = true;
    }

    let colors = panic_colors(panic, resp.hovered());
    let painter = ui.painter();
    painter.rect(
        rect,
        Rounding::same(Metrics::R_BTN),
        colors.fill,
        egui::Stroke::new(1.0_f32, colors.border),
    );
    let galley = painter.layout_no_wrap(label.to_string(), font, colors.text);
    crate::gui::widgets::paint_icon(
        painter,
        glyph::STOP,
        egui::pos2(rect.left() + pad + icon / 2.0, rect.center().y),
        icon,
        colors.glyph,
    );
    painter.galley(
        egui::pos2(
            rect.left() + pad + icon + Metrics::SM,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        colors.text,
    );

    let resp = resp.on_hover_text(panic_tooltip(panic));
    if resp.clicked() {
        state.stop_all_sounds();
    }
}

/// Right-hand live signal: pulsing dot, `LIVE`/`OFF AIR`, secondary line.
fn live_status(ui: &mut Ui, state: &mut SoundSharedApp, density: BarDensity) {
    let server = state.get_server_state();
    let (color, headline, subline, pulse) = match &server.status {
        ServerStatus::Running => (
            Palette::ROSE_500,
            "Live".to_string(),
            format!("Server running · :{}", state.get_server_port()),
            true,
        ),
        ServerStatus::Starting => (
            Palette::AMBER_400,
            "Starting".to_string(),
            "Binding listener".to_string(),
            true,
        ),
        ServerStatus::Error(_) => (
            Palette::ROSE_400,
            "Error".to_string(),
            "Server failed — see panel".to_string(),
            false,
        ),
        ServerStatus::Stopped => (
            Palette::ZINC_500,
            "Off Air".to_string(),
            "Server stopped".to_string(),
            false,
        ),
    };

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = Metrics::LOCKUP_GAP;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(headline.to_uppercase())
                        .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                        .color(color)
                        .extra_letter_spacing(1.0)
                        .strong(),
                );
                signal_dot(ui, color, 7.0, pulse && !state.reduced_motion());
            });
            if density == BarDensity::Full {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(micro_label(subline).size(9.0).extra_letter_spacing(0.8));
                });
            }
        });
    });
}

/// 1px vertical hairline between bar groups.
fn vertical_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 32.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::ZERO, Palette::BORDER);
}

/// Bottom status strip (28px): version left, local output centre, format right.
pub fn status_strip(ui: &mut Ui, state: &SoundSharedApp) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, Rounding::ZERO, Palette::APP);
    let line = Rect::from_min_size(
        egui::pos2(rect.left(), rect.top()),
        Vec2::new(rect.width(), 1.0),
    );
    ui.painter()
        .rect_filled(line, Rounding::ZERO, Palette::BORDER);

    ui.allocate_ui_with_layout(
        ui.available_size(),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(Metrics::PANEL_PAD);
            ui.label(
                egui::RichText::new(format!("Bardwave v{}", crate::VERSION))
                    .font(egui::FontId::new(9.0, egui::FontFamily::Monospace))
                    .color(Palette::ZINC_600)
                    .extra_letter_spacing(1.0),
            );

            ui.add_space(Metrics::LG);
            local_audio_readout(ui, state);
            ui.add_space(Metrics::LG);
            crate::updates::status_indicator(ui, state);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(Metrics::PANEL_PAD);
                let tracks = state.get_sounds().len();
                // The wire format is 32-bit float (see the browser client and
                // the broadcaster header), so the strip must not claim 16-bit.
                let label = format!(
                    "{} kHz / f32 / stereo  ·  {} tracks  ·  {} groups",
                    crate::audio::decoder::TARGET_SAMPLE_RATE / 1000,
                    tracks,
                    state.group_count()
                );
                ui.label(
                    egui::RichText::new(label)
                        .font(egui::FontId::new(9.0, egui::FontFamily::Monospace))
                        .color(Palette::ZINC_600)
                        .extra_letter_spacing(1.0),
                );
            });
        },
    );
}

/// Compact "where the sound is going" readout for the status strip.
///
/// The web listeners are fed from the mixer in every state, so a silent desktop
/// is otherwise indistinguishable from a working one: this strip is what says
/// `LOCAL OFF` (headless), `LOCAL FAILED` (with the device error) or the device
/// the mix is being played on.
fn local_audio_readout(ui: &mut Ui, state: &SoundSharedApp) {
    use crate::audio::LocalOutputState as State;

    let status = state.local_output();
    let (dot, color, text) = match &status.state {
        State::Running => {
            let text = match status.device.as_deref() {
                Some(device) => format!("Local · {device}"),
                None => "Local".to_string(),
            };
            (Palette::ROSE_500, Palette::ZINC_400, text)
        }
        State::Headless => (
            Palette::AMBER_400,
            Palette::AMBER_400,
            "Local off — headless (network only)".to_string(),
        ),
        State::Failed(message) => (
            Palette::ROSE_700,
            Palette::ROSE_400,
            format!("Local failed — {message}"),
        ),
        State::Stopped => (
            Palette::ZINC_500,
            Palette::ZINC_600,
            "Local stopped".to_string(),
        ),
    };

    // The readout competes with the format text on the right, so it is clipped
    // rather than allowed to push the strip wider than the window.
    ui.allocate_ui_with_layout(
        Vec2::new(320.0, ui.available_height()),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_max_width(320.0);
            crate::gui::widgets::signal_dot(
                ui,
                dot,
                6.0,
                matches!(status.state, State::Running) && !state.reduced_motion(),
            );
            ui.add_space(Metrics::SM);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text.to_uppercase())
                        .font(egui::FontId::new(9.0, egui::FontFamily::Monospace))
                        .color(color)
                        .extra_letter_spacing(1.0),
                )
                .truncate(),
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::scenes::contrast_ratio;

    fn rgb(c: egui::Color32) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    #[test]
    fn nothing_running_is_idle() {
        assert_eq!(panic_state(BoardActivity::default()), PanicState::Idle);
    }

    #[test]
    fn anything_running_lights_the_button() {
        let cases = [
            BoardActivity {
                sounds: true,
                ..Default::default()
            },
            BoardActivity {
                scene: true,
                ..Default::default()
            },
            BoardActivity {
                music: true,
                ..Default::default()
            },
            BoardActivity {
                preview: true,
                ..Default::default()
            },
        ];
        for activity in cases {
            assert_eq!(panic_state(activity), PanicState::Live, "{activity:?}");
        }
    }

    #[test]
    fn live_and_idle_look_different() {
        for hovered in [false, true] {
            assert_ne!(
                panic_colors(PanicState::Live, hovered),
                panic_colors(PanicState::Idle, hovered)
            );
        }
        assert_ne!(
            panic_tooltip(PanicState::Live),
            panic_tooltip(PanicState::Idle)
        );
    }

    #[test]
    fn label_text_reads_in_every_state() {
        // Idle sits on the bar, the lit button on its own fill.
        for state in [PanicState::Idle, PanicState::Live] {
            for hovered in [false, true] {
                let colors = panic_colors(state, hovered);
                let background = if colors.fill == Palette::TRANSPARENT {
                    Palette::APP
                } else {
                    colors.fill
                };
                let ratio = contrast_ratio(rgb(colors.text), rgb(background));
                assert!(
                    ratio >= 4.5,
                    "{state:?} hovered={hovered}: contrast {ratio:.2}"
                );
            }
        }
    }

    #[test]
    fn bar_goes_compact_below_the_threshold() {
        assert_eq!(bar_density(960.0), BarDensity::Compact);
        assert_eq!(bar_density(1179.0), BarDensity::Compact);
        assert_eq!(bar_density(1180.0), BarDensity::Full);
        assert_eq!(bar_density(1600.0), BarDensity::Full);
    }

    #[test]
    fn compact_bar_keeps_block_gaps_and_saves_room() {
        let (full, compact) = (BarDensity::Full, BarDensity::Compact);
        assert!(compact.group_gap() >= Metrics::MD);
        assert!(compact.group_gap() < full.group_gap());
        assert!(compact.master_slider_w() < full.master_slider_w());
    }

    #[test]
    fn spacing_tokens_keep_the_scale() {
        const { assert!(Metrics::LABEL_GAP >= Metrics::SM) };
        const { assert!(Metrics::BLOCK_GAP >= Metrics::MD) };
    }
}
