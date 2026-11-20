//! Spacing and text helpers shared by the console's panels and dialogs.
//!
//! The console panels zero `item_spacing.y` (see `theme::console_panel`), so
//! everything that is stacked without an explicit gap sticks together. These
//! helpers put the gaps on the [`Metrics`] scale (XS 4 / SM 8 / MD 12 / LG 16 /
//! XL 24) and give wrapped notes a readable line height.
//!
//! | gap | use |
//! |-----|-----|
//! | XS  | caption above its field, a note under its field |
//! | SM  | row to row inside one block, buttons in a row |
//! | MD  | block to block |
//! | LG  | above a section heading |

use egui::{Color32, Context, Margin, Response, RichText, Rounding, Stroke, Ui, Vec2};

use crate::gui::theme::{micro_label, ui_font, Metrics, Palette};
use crate::gui::widgets::{glyph_font, ConsoleButton};

/// Line height of a wrapped note, as a multiple of its font size.
pub const NOTE_LINE_FACTOR: f32 = 1.45;

/// Line height, in points, for text of `size`.
pub fn note_line_height(size: f32) -> f32 {
    (size * NOTE_LINE_FACTOR).round()
}

/// A note's text: mono, with the spaced-out line height.
pub fn note_text(text: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(text.into())
        .font(glyph_font(size))
        .color(color)
        .line_height(Some(note_line_height(size)))
}

/// A wrapped note in the console's small type, `ZINC_500` unless coloured.
pub fn note(ui: &mut Ui, text: impl Into<String>) -> Response {
    note_colored(ui, text, Palette::ZINC_500)
}

/// [`note`] with a colour (warnings amber, errors rose).
pub fn note_colored(ui: &mut Ui, text: impl Into<String>, color: Color32) -> Response {
    ui.add(egui::Label::new(note_text(text, Metrics::MICRO, color)).wrap())
}

/// A note that sits `Metrics::XS` below the control above it.
pub fn note_under(ui: &mut Ui, text: impl Into<String>, color: Color32) -> Response {
    ui.add_space(Metrics::XS);
    note_colored(ui, text, color)
}

/// A section heading: `Metrics::LG` of air above, `Metrics::SM` below.
/// Pass `first = true` for the heading that opens a panel (no air above).
pub fn section_title(ui: &mut Ui, text: &str, first: bool) {
    if !first {
        ui.add_space(Metrics::LG);
    }
    ui.label(micro_label(text));
    ui.add_space(Metrics::SM);
}

/// Space between blocks.
pub fn block_gap(ui: &mut Ui) {
    ui.add_space(Metrics::MD);
}

/// Space between rows of one block.
pub fn row_gap(ui: &mut Ui) {
    ui.add_space(Metrics::SM);
}

/// Space between a caption and its field.
pub fn caption_gap(ui: &mut Ui) {
    ui.add_space(Metrics::XS);
}

/// A validation message in rose, same look as `dialog::error_line`. That one
/// centres itself in the remaining height, which inside a scroll area is
/// infinite: the text lands out of sight. This one lays out from the top.
pub fn error_line(ui: &mut Ui, message: &str) {
    ui.label(
        RichText::new(message.to_uppercase())
            .font(glyph_font(9.0))
            .color(Palette::ROSE_400)
            .extra_letter_spacing(0.8),
    );
}

/// Room kept free around a modal so it never touches the window edge.
pub const MODAL_MARGIN: f32 = Metrics::LG;

/// Tallest a modal may be on a window `screen_h` points tall.
pub fn modal_max_height(screen_h: f32) -> f32 {
    (screen_h - 2.0 * MODAL_MARGIN).max(200.0)
}

/// Widest content a modal may have on a window `screen_w` points wide (the
/// frame's padding and the margin come on top).
pub fn modal_max_width(screen_w: f32) -> f32 {
    (screen_w - 2.0 * MODAL_MARGIN - 2.0 * Metrics::PANEL_PAD).max(280.0)
}

/// A centred modal in the console frame. Unlike a bare `egui::Window` it never
/// grows past the window: taller content scrolls inside it, which matters at
/// the 960x620 minimum size and with a large interface scale. Returns the
/// painted rect.
pub fn modal_window(
    ctx: &Context,
    id: &'static str,
    width: f32,
    add: impl FnOnce(&mut Ui),
) -> Option<egui::Rect> {
    // A window narrower than the dialog (a huge scale on a small screen) would
    // otherwise push the dialog's edges out of view.
    let width = width.min(modal_max_width(ctx.screen_rect().width()));
    let body_max = modal_max_height(ctx.screen_rect().height()) - 2.0 * Metrics::PANEL_PAD;
    egui::Window::new(id)
        .id(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .frame(crate::gui::theme::panel_frame())
        .show(ctx, |ui| {
            ui.set_width(width);
            fit_scroll(ui, egui::Id::new((id, "body")), body_max, add);
        })
        .map(|shown| shown.response.rect)
}

/// Lay `add` out plainly while it fits in `max_h` points of height, and inside
/// a vertical scroll area once it does not. The choice follows the height
/// measured on the previous frame, so a window that has room behaves exactly
/// like a bare one (it grows and shrinks with its content in the same frame),
/// and one that does not stays inside its limit instead of running off screen.
pub fn fit_scroll(ui: &mut Ui, id: egui::Id, max_h: f32, add: impl FnOnce(&mut Ui)) {
    let key = id.with("fit_scroll_h");
    let last: Option<f32> = ui.ctx().data(|d| d.get_temp(key));
    let scrolled = last.is_some_and(|h| h > max_h + 0.5);
    let height = if scrolled {
        egui::ScrollArea::vertical()
            .id_salt(id)
            .max_height(max_h)
            .auto_shrink([false, true])
            .show(ui, add)
            .content_size
            .y
    } else {
        let top = ui.cursor().top();
        add(ui);
        ui.min_rect().bottom() - top
    };
    let now_scrolled = height > max_h + 0.5;
    if last.is_none_or(|h| (h - height).abs() > 0.5) {
        ui.ctx().data_mut(|d| d.insert_temp(key, height));
    }
    // The mode, or the content, changed: lay out once more so the window shows
    // the new state without waiting for the next input event.
    if now_scrolled != scrolled || last.is_some_and(|h| scrolled && (h - height).abs() > 0.5) {
        ui.ctx().request_repaint();
    }
}

/// An empty list or panel: a framed well with a title and a hint that says
/// what to do next. No action button; see [`empty_state_action`].
pub fn empty_state(ui: &mut Ui, title: &str, hint: &str) {
    empty_frame(ui, title, hint, |_| ());
}

/// [`empty_state`] with a call-to-action button under the hint. True when the
/// button was clicked.
pub fn empty_state_action(ui: &mut Ui, title: &str, hint: &str, action: &str, salt: &str) -> bool {
    let mut clicked = false;
    empty_frame(ui, title, hint, |ui| {
        ui.add_space(Metrics::SM);
        // About 8 points per character plus the padding.
        let width = (action.chars().count() as f32 * 8.0 + 40.0).max(110.0);
        clicked = ConsoleButton::primary(action)
            .width(width)
            .small()
            .id_salt(salt)
            .show(ui)
            .clicked();
    });
    clicked
}

fn empty_frame(ui: &mut Ui, title: &str, hint: &str, extra: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    egui::Frame::none()
        .fill(Palette::WELL)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
        .rounding(Rounding::same(Metrics::R_SM))
        .inner_margin(Margin::same(Metrics::MD))
        .show(ui, |ui| {
            ui.set_width((width - 2.0 * Metrics::MD - 2.0).max(40.0));
            ui.label(
                RichText::new(title)
                    .font(ui_font(13.0))
                    .color(Palette::ZINC_300),
            );
            ui.add_space(Metrics::XS);
            note(ui, hint);
            extra(ui);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_lines_are_airier_than_the_font() {
        assert!(note_line_height(10.0) >= 14.0);
        assert!(note_line_height(11.0) > 11.0 * 1.3);
        // Whole points keep the text crisp.
        assert_eq!(note_line_height(10.0).fract(), 0.0);
    }

    #[test]
    fn modals_never_outgrow_the_window() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(738.0, 477.0));
        let mut rect = None;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                rect = modal_window(ctx, "kit_tall_modal", 420.0, |ui| {
                    for n in 0..80 {
                        ui.label(format!("row {n}"));
                    }
                });
            });
        }
        let rect = rect.expect("the modal is shown");
        assert!(
            rect.height() <= modal_max_height(screen.height()) + 1.0,
            "modal is {} tall on a {} window",
            rect.height(),
            screen.height()
        );
        assert!(screen.contains_rect(rect.shrink(1.0)), "fits: {rect:?}");
    }

    #[test]
    fn empty_states_render_title_hint_and_report_the_action() {
        let ctx = egui::Context::default();
        let mut clicked = true;
        let mut run = |ctx: &egui::Context| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    empty_state(ui, "Nothing here", "Do a thing first");
                    clicked = empty_state_action(ui, "Empty", "Hint", "Go", "kit_test");
                });
            });
        };
        run(&ctx);
        run(&ctx);
        assert!(!clicked, "no click happened");
    }
}
