//! Top command bar — brand lockup, master volume, live server status.
//!
//! 64px tall, `#0A0A0A`, hairline bottom border, 24px horizontal padding:
//! brand left · master volume centre-left · live signal right.

use crate::gui::app::{ServerStatus, SoundSharedApp};
use crate::gui::theme::{micro_label, wordmark, Metrics, Palette};
use crate::gui::widgets::{glyph, level_slider, signal_dot, IconButton};
use egui::{Align, Layout, Rect, Rounding, Sense, Ui, Vec2};

/// Height reserved for the master volume slider inside the bar.
const MASTER_SLIDER_W: f32 = 150.0;

/// Renders the top command bar.
pub fn top_bar(ui: &mut Ui, state: &mut SoundSharedApp) {
    let bar_rect = ui.max_rect();
    ui.painter()
        .rect_filled(bar_rect, Rounding::ZERO, Palette::APP);

    // Reserve the full bar and lay the three groups out on one row.
    ui.allocate_ui_with_layout(
        ui.available_size(),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(Metrics::PANEL_PAD);
            brand(ui);
            ui.add_space(Metrics::XL);
            vertical_divider(ui);
            ui.add_space(Metrics::XL);
            master_volume(ui, state);
            ui.add_space(Metrics::XL);
            vertical_divider(ui);
            ui.add_space(Metrics::XL);
            quick_clear(ui, state);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(Metrics::PANEL_PAD);
                live_status(ui, state);
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
fn brand(ui: &mut Ui) {
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
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(wordmark("Bardwave"));
            ui.label(micro_label("Broadcast Console").size(9.0));
        });
    });
}

/// Master volume: micro label, 150px slider, mono readout.
fn master_volume(ui: &mut Ui, state: &mut SoundSharedApp) {
    ui.horizontal(|ui| {
        ui.label(micro_label("Master"));
        ui.add_space(Metrics::SM);

        let mut value = state.get_master_volume();
        let resp = level_slider(ui, &mut value, MASTER_SLIDER_W, "master_top");
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

/// A small, quiet "stop everything" action in the command bar.
fn quick_clear(ui: &mut Ui, state: &mut SoundSharedApp) {
    let any_playing = state.get_sounds().iter().any(|s| s.playing);
    let resp = IconButton::new(glyph::STOP)
        .size(26.0)
        .font_size(10.0)
        .tooltip("Stop all sounds")
        .id_salt("top_stop_all")
        .show(ui);
    if resp.clicked() && any_playing {
        state.stop_all_sounds();
    }
    ui.add_space(Metrics::XS);
    ui.label(micro_label("Panic"));
}

/// Right-hand live signal: pulsing dot, `LIVE`/`OFF AIR`, secondary line.
fn live_status(ui: &mut Ui, state: &mut SoundSharedApp) {
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
            ui.spacing_mut().item_spacing.y = 0.0;
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
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(micro_label(subline).size(9.0).extra_letter_spacing(0.8));
            });
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
