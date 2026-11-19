//! The strip above the sound list (search, tag chips, library actions, the
//! "files not found" banner, status messages) and the painted pieces the rows
//! share: the headphones button, the not-found badge and the not-found row.

use egui::{
    pos2, vec2, Align, Color32, CursorIcon, Layout, Painter, Pos2, Rect, Response, Rounding, Sense,
    Shape, Stroke, Ui,
};

use crate::gui::app::SoundSharedApp;
use crate::gui::theme::{alpha, micro_label, Metrics, Palette};
use crate::gui::widgets::{elide, glyph, glyph_font, paint_icon, ConsoleButton, IconButton};

use super::logic::{
    elide_start, highlight_job, missing_count, missing_summary, tag_chips, MissingSound,
};

/// Most tag chips shown at once; the rest are counted.
const MAX_CHIPS: usize = 14;

/// Draws the toolbar. Takes the vertical space it needs.
pub fn library_toolbar(ui: &mut Ui, state: &mut SoundSharedApp) {
    search_row(ui, state);
    ui.add_space(Metrics::SM);
    filter_row(ui, state);
    banner_row(ui, state);
    toast_row(ui, state);
}

fn search_row(ui: &mut Ui, state: &mut SoundSharedApp) {
    const BTN_W: [f32; 3] = [112.0, 84.0, 84.0];
    let buttons_w: f32 = BTN_W.iter().sum::<f32>() + 3.0 * Metrics::SM;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Metrics::SM;
        let has_text = !state.lib_ui.filter.text.is_empty();
        let clear_w = if has_text { 26.0 } else { 0.0 };
        let field_w = (ui.available_width() - buttons_w - clear_w - Metrics::SM).max(120.0);
        let field = egui::TextEdit::singleline(&mut state.lib_ui.filter.text)
            .hint_text("Search name or tag…")
            .desired_width(field_w)
            .font(glyph_font(12.0))
            .margin(egui::Margin::symmetric(10.0, 7.0));
        ui.add(field);
        if has_text
            && IconButton::new(glyph::CROSS)
                .size(26.0)
                .font_size(10.0)
                .tooltip("Clear search")
                .id_salt("library_search_clear")
                .show(ui)
                .clicked()
        {
            state.lib_ui.filter.text.clear();
        }
        if ConsoleButton::ghost("Folder…")
            .glyph(glyph::FOLDER)
            .small()
            .width(BTN_W[0])
            .tooltip("Add every sound in a folder (and its sub-folders)")
            .id_salt("library_add_folder")
            .show(ui)
            .clicked()
        {
            state.open_add_folder();
        }
        if ConsoleButton::ghost("Export…")
            .small()
            .width(BTN_W[1])
            .tooltip("Save the library as a shareable .zip bundle")
            .id_salt("library_export")
            .show(ui)
            .clicked()
        {
            state.open_export();
        }
        if ConsoleButton::ghost("Import…")
            .small()
            .width(BTN_W[2])
            .tooltip("Merge a .zip bundle into the library")
            .id_salt("library_import")
            .show(ui)
            .clicked()
        {
            state.open_import();
        }
    });
}

/// Tag chips and the "showing N of M" line.
fn filter_row(ui: &mut Ui, state: &mut SoundSharedApp) {
    let query = state.current_query();
    let counts = state.library().tag_counts(&query);
    let selected = state.lib_ui.filter.tags.clone();
    let (chips, hidden) = tag_chips(&selected, &counts, MAX_CHIPS);
    let active = state.lib_ui.filter.is_active();
    if chips.is_empty() && !active {
        return;
    }

    let mut toggled: Option<String> = None;
    let mut clear = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for chip_data in &chips {
            let label = if chip_data.count > 0 {
                format!("{}  {}", chip_data.tag, chip_data.count)
            } else {
                chip_data.tag.clone()
            };
            if chip(ui, &label, chip_data.selected).clicked() {
                toggled = Some(chip_data.tag.clone());
            }
        }
        if hidden > 0 {
            ui.label(micro_label(format!("+{hidden} more tags")));
        }
        if active
            && ConsoleButton::ghost("Clear")
                .glyph(glyph::CROSS)
                .small()
                .width(80.0)
                .height(22.0)
                .id_salt("library_filter_clear")
                .show(ui)
                .clicked()
        {
            clear = true;
        }
    });
    if let Some(tag) = toggled {
        state.lib_ui.filter.toggle_tag(&tag);
    }
    if clear {
        state.lib_ui.filter.clear();
    }

    if active {
        let shown = state.library().count_matching(&query);
        ui.add_space(2.0);
        ui.label(micro_label(format!(
            "Showing {shown} of {} sounds",
            state.library().len()
        )));
    }
    ui.add_space(Metrics::SM);
}

/// One tag chip.
fn chip(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let color = if selected {
        Palette::ROSE_400
    } else {
        Palette::ZINC_400
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), glyph_font(10.0), color);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 18.0, 22.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let hovered = resp.hovered();
    let (fill, border) = if selected {
        (alpha(Palette::ROSE_950, 190), alpha(Palette::ROSE_500, 160))
    } else if hovered {
        (Palette::ROW, Palette::BORDER_STRONG)
    } else {
        (Palette::TRANSPARENT, Palette::BORDER)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        Rounding::same(Metrics::R_SM),
        fill,
        Stroke::new(1.0_f32, border),
    );
    painter.galley(
        pos2(rect.left() + 9.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    resp
}

/// Amber banner: how many files are gone, and what to do about it.
fn banner_row(ui: &mut Ui, state: &mut SoundSharedApp) {
    let count = missing_count(state.library(), &state.lib_ui.missing);
    if count == 0 {
        return;
    }
    let mut remap = false;
    let mut recheck = false;
    egui::Frame::none()
        .fill(alpha(Palette::AMBER_400, 14))
        .stroke(Stroke::new(1.0_f32, alpha(Palette::AMBER_400, 70)))
        .rounding(Rounding::same(Metrics::R_SM))
        .inner_margin(egui::Margin::symmetric(10.0, 6.0))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                let (icon_rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                paint_warning(ui.painter(), icon_rect.center(), 15.0, Palette::AMBER_400);
                ui.label(
                    egui::RichText::new(missing_summary(count).to_uppercase())
                        .font(glyph_font(10.0))
                        .color(Palette::AMBER_400)
                        .extra_letter_spacing(0.8),
                );
                if ConsoleButton::ghost("Replace Path Prefix…")
                    .small()
                    .height(22.0)
                    .width(168.0)
                    .tooltip(
                        "The whole sound folder moved: rewrite the start of every path at once",
                    )
                    .id_salt("library_remap")
                    .show(ui)
                    .clicked()
                {
                    remap = true;
                }
                if ConsoleButton::ghost("Recheck")
                    .glyph(glyph::REFRESH)
                    .small()
                    .height(22.0)
                    .width(96.0)
                    .tooltip("Look for the files again")
                    .id_salt("library_recheck")
                    .show(ui)
                    .clicked()
                {
                    recheck = true;
                }
            });
        });
    ui.add_space(Metrics::SM);
    if remap {
        state.open_remap();
    }
    if recheck {
        state.recheck_files();
    }
}

fn toast_row(ui: &mut Ui, state: &mut SoundSharedApp) {
    let Some(toast) = state.lib_ui.toast() else {
        return;
    };
    let color = if toast.error {
        Palette::ROSE_400
    } else {
        Palette::ZINC_300
    };
    ui.label(
        egui::RichText::new(&toast.text)
            .font(glyph_font(11.0))
            .color(color),
    );
    ui.add_space(Metrics::SM);
}

// ------------------------------------------------------------- painting ----

/// Headphones: a headband arc and two ear cups.
pub fn paint_headphones(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let r = 0.36 * s;
    let stroke = Stroke::new((s * 0.11).max(1.3), color);
    let steps = 18;
    let band: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / steps as f32);
            pos2(center.x + r * a.cos(), center.y + 0.06 * s + r * a.sin())
        })
        .collect();
    painter.add(Shape::line(band, stroke));
    for side in [-1.0_f32, 1.0] {
        let cup = Rect::from_center_size(
            pos2(center.x + side * r, center.y + 0.22 * s),
            vec2(0.26 * s, 0.42 * s),
        );
        painter.rect_filled(cup, Rounding::same(s * 0.07), color);
    }
}

/// A small warning triangle with an exclamation mark.
pub fn paint_warning(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let top = pos2(center.x, center.y - 0.42 * s);
    let left = pos2(center.x - 0.48 * s, center.y + 0.38 * s);
    let right = pos2(center.x + 0.48 * s, center.y + 0.38 * s);
    let stroke = Stroke::new((s * 0.1).max(1.2), color);
    painter.add(Shape::closed_line(vec![top, left, right], stroke));
    painter.line_segment(
        [
            pos2(center.x, center.y - 0.12 * s),
            pos2(center.x, center.y + 0.14 * s),
        ],
        stroke,
    );
    painter.circle_filled(
        pos2(center.x, center.y + 0.27 * s),
        (s * 0.05).max(0.9),
        color,
    );
}

/// The headphones button of a row: returns the response of its rect.
pub fn preview_button(ui: &Ui, rect: Rect, id: egui::Id, active: bool, enabled: bool) -> Response {
    let resp = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let resp = if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    };
    let hovered = enabled && resp.hovered();
    let (fill, border, fg) = if active {
        (
            alpha(Palette::ROSE_950, 190),
            alpha(Palette::ROSE_500, 160),
            Palette::ROSE_400,
        )
    } else if !enabled {
        (
            Palette::TRANSPARENT,
            Palette::BORDER,
            alpha(Palette::ZINC_600, 160),
        )
    } else if hovered {
        (
            Palette::TRANSPARENT,
            Palette::BORDER_STRONG,
            Palette::ZINC_300,
        )
    } else {
        (Palette::TRANSPARENT, Palette::BORDER, Palette::ZINC_500)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        Rounding::same(Metrics::R_SM),
        fill,
        Stroke::new(1.0_f32, border),
    );
    paint_headphones(painter, rect.center(), 14.0, fg);
    if resp.hovered() {
        let text = if !enabled {
            "Preview is not available for this sound"
        } else if active {
            "Stop preview"
        } else {
            "Preview in your headphones (players do not hear it)"
        };
        resp.clone().on_hover_text(text);
    }
    resp
}

/// Paint a small outlined badge starting at `left` (vertically centred on
/// `center_y`) and return its width.
pub fn paint_badge(painter: &Painter, left: f32, center_y: f32, text: &str, color: Color32) -> f32 {
    let galley = painter.layout_no_wrap(text.to_uppercase(), glyph_font(9.0), color);
    let width = galley.size().x + 12.0;
    let rect = Rect::from_min_size(pos2(left, center_y - 8.0), vec2(width, 16.0));
    painter.rect(
        rect,
        Rounding::same(Metrics::R_SM),
        alpha(color, 22),
        Stroke::new(1.0_f32, alpha(color, 110)),
    );
    painter.galley(
        pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    width
}

/// What a not-found row wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingAction {
    None,
    Locate,
    Remove,
}

/// A row for a registered sound whose file is gone and that is not loaded.
pub fn missing_row(ui: &mut Ui, sound: &MissingSound, terms: &[String]) -> MissingAction {
    let mut action = MissingAction::None;
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, Metrics::ROW_H), Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        Rounding::same(Metrics::R_SM),
        Palette::ROW,
        Stroke::new(1.0_f32, alpha(Palette::AMBER_400, 90)),
    );
    painter.rect_filled(
        Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
        Rounding::ZERO,
        Palette::AMBER_400,
    );
    paint_warning(
        painter,
        pos2(rect.left() + 28.0, rect.center().y),
        20.0,
        Palette::AMBER_400,
    );

    // Right side: remove, locate.
    let remove_rect =
        Rect::from_center_size(pos2(rect.right() - 19.0, rect.center().y), vec2(18.0, 18.0));
    let remove = ui
        .interact(remove_rect, resp.id.with("remove"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Remove from the library");
    paint_icon(
        ui.painter(),
        glyph::CROSS,
        remove_rect.center(),
        13.8,
        if remove.hovered() {
            Palette::ROSE_400
        } else {
            Palette::ZINC_500
        },
    );
    if remove.clicked() {
        action = MissingAction::Remove;
    }

    let locate_rect = Rect::from_center_size(
        pos2(remove_rect.left() - 12.0 - 52.0, rect.center().y),
        vec2(104.0, Metrics::BTN_H_SM),
    );
    let mut locate_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(locate_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let locate = locate_ui
        .push_id(&sound.path, |ui| {
            ConsoleButton::ghost("Locate…")
                .glyph(glyph::FOLDER)
                .small()
                .width(104.0)
                .tooltip("Pick the file's new location")
                .id_salt("locate")
                .show(ui)
        })
        .inner;
    if locate.clicked() {
        action = MissingAction::Locate;
    }

    // Text between the icon and the buttons.
    let text_left = rect.left() + 52.0;
    let text_right = locate_rect.left() - 12.0;
    let clip = Rect::from_min_max(
        pos2(text_left, rect.top()),
        pos2(text_right.max(text_left), rect.bottom()),
    );
    let text_painter = ui.painter().with_clip_rect(clip);
    let name = elide(&sound.name, 40);
    let job = highlight_job(
        &name,
        terms,
        crate::gui::theme::ui_font(14.0),
        Palette::ZINC_300,
        Palette::WHITE,
        alpha(Palette::ROSE_600, 90),
    );
    let name_galley = text_painter.layout_job(job);
    let name_w = name_galley.size().x;
    text_painter.galley(
        pos2(text_left, rect.center().y - name_galley.size().y - 1.0),
        name_galley,
        Palette::ZINC_300,
    );
    paint_badge(
        &text_painter,
        text_left + name_w + 10.0,
        rect.center().y - 10.0,
        "File not found",
        Palette::AMBER_400,
    );
    text_painter.text(
        pos2(text_left, rect.center().y + 3.0),
        egui::Align2::LEFT_TOP,
        elide_start(&sound.path.to_string_lossy(), 70),
        glyph_font(10.0),
        Palette::ZINC_500,
    );
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_row_and_preview_button_draw() {
        let ctx = egui::Context::default();
        let sound = MissingSound {
            path: std::path::PathBuf::from("/very/long/path/to/some/folder/tavern/crowd.ogg"),
            name: "Crowd".into(),
            group: None,
        };
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let action = missing_row(ui, &sound, &["cro".to_string()]);
                    assert_eq!(action, MissingAction::None);
                    let rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(18.0, 18.0));
                    for (active, enabled) in [(false, true), (true, true), (false, false)] {
                        let _ = preview_button(ui, rect, egui::Id::new("p"), active, enabled);
                    }
                    let _ = chip(ui, "tavern  4", true);
                    let _ = chip(ui, "loud", false);
                });
            });
        }
    }
}
