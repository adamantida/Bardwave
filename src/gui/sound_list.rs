//! `01 / SOUND LIBRARY` — the operator's sound board.
//!
//! Layout mirrors the Red Noir console:
//!
//! ```text
//! ┌ 01 / LIBRARY ──────────────── 12 TRACKS ──────── + ADD SOUND ┐
//! │ ┌ groups ──┐ ┌ row: ▶  Intro Theme   48k · 00:42 · WAV  ▬▬ │
//! │ │ All   12 │ │ row: ■  Rain         44k · 03:10 · WAV  ▬▬ │
//! │ │ Combat  4 │ │ …                                          │
//! │ │ Ambience3 │ └────────────────────────────────────────────┘
//! │ └──────────┘ [MASTER OUTPUT ▮▮▮▮▮▮▮▮▮▮▮▮▮▮▯▯▯▯▯▯  80% MUTE]
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! The group rail is the RPG-Sounds workflow: pick a category, add sounds to it.

mod layout;

use crate::audio::SoundId;
use crate::gui::app::{SoundEntry, SoundSharedApp};
use crate::gui::groups::GroupFilter;
use crate::gui::library_ui::logic::{format_tags, highlight_job, RowItem};
use crate::gui::library_ui::{
    library_toolbar, missing_row, paint_badge, preview_button, MissingAction,
};
use crate::gui::theme::{
    alpha, console_panel, hairline, micro_label, micro_label_accent, panel_title, telemetry,
    ui_font, Metrics, Palette,
};
use crate::gui::widgets::{
    elide, format_duration, format_sample_rate, glyph, level_slider, segmented_meter,
    ConsoleButton, IconButton,
};
use egui::{Align, CursorIcon, Layout, Rect, Rounding, Sense, Stroke, Ui, Vec2};
use layout::{
    header_layout, master_layout, meter_cells, rail_label_chars, rail_width, row_layout_for,
};

/// Renders the whole `01 / LIBRARY` panel at the given size.
pub fn sound_library_panel(ui: &mut Ui, state: &mut SoundSharedApp, size: Vec2) {
    console_panel(ui, size, |ui| {
        header(ui, state);
        ui.add_space(Metrics::MD);
        hairline(ui);
        ui.add_space(Metrics::MD);

        // Scene deck: scene buttons, stingers, transition time.
        crate::gui::scenes::scenes_deck(ui, state);
        ui.add_space(Metrics::MD);
        hairline(ui);
        ui.add_space(Metrics::MD);

        // Reserve the master strip at the bottom; the list takes the rest.
        let master_h = Metrics::ROW_H - 8.0;
        let list_h = (ui.available_height() - master_h - Metrics::MD).max(80.0);

        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), list_h),
            Layout::left_to_right(Align::Min),
            |ui| {
                // Gaps are placed by hand, so the widths budgeted by
                // `rail_width` are exact.
                ui.spacing_mut().item_spacing.x = 0.0;
                // The rail is the primary navigation: it narrows before the
                // rows do and is dropped only when the rows would become too
                // narrow to read. The decision uses the body width, not the
                // outer panel size (which still contains the padding).
                if let Some(rail_w) = rail_width(ui.available_width()) {
                    group_rail(ui, state, list_h, rail_w);
                    ui.add_space(Metrics::MD);
                }
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), list_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        // Search, tag chips and library actions, then the rows
                        // in whatever height is left.
                        library_toolbar(ui, state);
                        let rows_h = ui.available_height().max(80.0);
                        sound_rows(ui, state, rows_h);
                    },
                );
            },
        );

        ui.add_space(Metrics::MD);
        hairline(ui);
        ui.add_space(Metrics::BLOCK_GAP);
        master_output(ui, state);
    });
}

/// `01 / LIBRARY` + title + track count + `+ ADD SOUND`.
fn header(ui: &mut Ui, state: &mut SoundSharedApp) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(micro_label_accent("01 / Library"));
            ui.horizontal(|ui| {
                ui.label(panel_title("Sound Library"));
                ui.add_space(Metrics::LABEL_GAP);
                let total = state.count_for(GroupFilter::All);
                ui.label(micro_label(match total {
                    1 => "1 Track".to_string(),
                    n => format!("{} Tracks", n),
                }));
            });
        });

        // The buttons get what the title leaves; they narrow, and the YouTube
        // one drops out, before anything can leave the panel.
        let room = (ui.available_width() - 1.0).max(0.0);
        let lay = header_layout(room);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Gaps are placed by hand so the widths budgeted above are exact.
            ui.spacing_mut().item_spacing.x = 0.0;
            if ConsoleButton::primary("Add Sound")
                .glyph(glyph::PLUS)
                .font_size(11.0)
                .width(lay.add_w)
                .id_salt("library_add")
                .show(ui)
                .clicked()
            {
                state.open_file_dialog();
            }
            if let Some(youtube_w) = lay.youtube_w {
                ui.add_space(Metrics::SM);
                if ConsoleButton::ghost("YouTube")
                    .glyph(glyph::WAVE)
                    .font_size(11.0)
                    .width(youtube_w)
                    .id_salt("library_add_youtube")
                    .show(ui)
                    .clicked()
                {
                    state.open_add_youtube();
                }
            }
        });
    });
}

/// Vertical list of category chips: `ALL`, `UNGROUPED`, then each group.
///
/// Selecting a chip filters the rows. Each chip carries its sound count and, on
/// hover, reveals rename/delete actions for real groups.
fn group_rail(ui: &mut Ui, state: &mut SoundSharedApp, height: f32, width: f32) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        Layout::top_down(Align::Min),
        |ui| {
            // Exactly `width`: the rail never grows with its content.
            ui.set_width(width);
            ui.set_max_width(width);

            ui.horizontal(|ui| {
                ui.label(micro_label("Groups"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if IconButton::new(glyph::PLUS)
                        .size(18.0)
                        .font_size(11.0)
                        .tooltip("New group")
                        .id_salt("rail_new_group")
                        .show(ui)
                        .clicked()
                    {
                        state.open_new_group();
                    }
                });
            });
            ui.add_space(Metrics::LABEL_GAP);

            egui::ScrollArea::vertical()
                .id_salt("group_rail_scroll")
                .auto_shrink([false, false])
                .max_height((height - 28.0).max(40.0))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;

                    let active = state.group_filter();
                    // Counts follow the search text and the lit tags, so the
                    // rail shows what each category would list right now.
                    let counts = state.library().group_counts(&state.current_query());
                    let ungrouped = counts.for_filter(GroupFilter::Ungrouped);
                    let all = counts.for_filter(GroupFilter::All);

                    if rail_chip(
                        ui,
                        width,
                        "All Sounds",
                        all,
                        active == GroupFilter::All,
                        None,
                    ) == RailAction::Select
                    {
                        state.set_group_filter(GroupFilter::All);
                    }
                    if rail_chip(
                        ui,
                        width,
                        "Ungrouped",
                        ungrouped,
                        active == GroupFilter::Ungrouped,
                        None,
                    ) == RailAction::Select
                    {
                        state.set_group_filter(GroupFilter::Ungrouped);
                    }

                    if !state.library().groups().is_empty() {
                        ui.add_space(Metrics::BLOCK_GAP);
                        ui.label(micro_label("Categories"));
                        ui.add_space(Metrics::LABEL_GAP);
                    }

                    // Collect first so the rail can mutate the library on click
                    // without borrowing it while iterating.
                    let groups: Vec<(crate::gui::groups::GroupId, String, usize)> = state
                        .library()
                        .groups()
                        .iter()
                        .map(|group| {
                            (
                                group.id,
                                group.name.clone(),
                                counts.for_filter(GroupFilter::Group(group.id)),
                            )
                        })
                        .collect();

                    for (id, name, count) in groups {
                        let selected = active == GroupFilter::Group(id);
                        match rail_chip(ui, width, &name, count, selected, Some(id)) {
                            RailAction::Select => state.set_group_filter(GroupFilter::Group(id)),
                            RailAction::Rename => state.open_rename_group(id),
                            RailAction::Delete => state.open_delete_group(id),
                            RailAction::None => {}
                        }
                    }
                });
        },
    );
}

/// What a rail chip wants the panel to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RailAction {
    None,
    Select,
    Rename,
    Delete,
}

/// One rail chip.
///
/// Group chips reveal inline rename (`✎`) and delete (`✕`) actions on hover,
/// replacing their count badge; non-group chips always show the count.
fn rail_chip(
    ui: &mut Ui,
    width: f32,
    label: &str,
    count: usize,
    selected: bool,
    group: Option<crate::gui::groups::GroupId>,
) -> RailAction {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(width, Metrics::RAIL_ITEM_H), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let hovered = resp.hovered();
    let painter = ui.painter();

    if selected {
        painter.rect_filled(rect, Rounding::same(Metrics::R_RAIL), Palette::ROW);
        painter.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(2.0, rect.height())),
            Rounding::ZERO,
            Palette::ROSE_500,
        );
    } else if hovered {
        painter.rect_filled(
            rect,
            Rounding::same(Metrics::R_RAIL),
            alpha(Palette::ROW, 140),
        );
    }

    let text_color = if selected {
        Palette::WHITE
    } else if hovered {
        Palette::ZINC_300
    } else {
        Palette::ZINC_400
    };

    let text_x = rect.left() + 10.0;
    let galley = painter.layout_no_wrap(
        elide(label, rail_label_chars(width)),
        ui_font(12.0),
        text_color,
    );
    painter.galley(
        egui::pos2(text_x, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );

    let mut action = RailAction::None;
    let badge_color = if selected {
        Palette::ROSE_400
    } else {
        Palette::ZINC_600
    };

    match group {
        Some(_) if hovered => {
            // Delete sits right-most, rename to its left.
            let del_rect = Rect::from_center_size(
                egui::pos2(rect.right() - 14.0, rect.center().y),
                Vec2::splat(18.0),
            );
            let del = ui
                .interact(del_rect, resp.id.with("del"), Sense::click())
                .on_hover_text("Delete group");
            crate::gui::widgets::paint_icon(
                painter,
                glyph::CROSS,
                del_rect.center(),
                12.5,
                if del.hovered() {
                    Palette::ROSE_400
                } else {
                    Palette::ZINC_500
                },
            );

            let ren_rect = Rect::from_center_size(
                egui::pos2(rect.right() - 32.0, rect.center().y),
                Vec2::splat(18.0),
            );
            let ren = ui
                .interact(ren_rect, resp.id.with("ren"), Sense::click())
                .on_hover_text("Rename group");
            crate::gui::widgets::paint_icon(
                painter,
                glyph::EDIT,
                ren_rect.center(),
                12.5,
                if ren.hovered() {
                    Palette::WHITE
                } else {
                    Palette::ZINC_500
                },
            );

            if del.clicked() {
                action = RailAction::Delete;
            } else if ren.clicked() {
                action = RailAction::Rename;
            } else {
                count_badge(painter, rect, count, badge_color);
            }
        }
        _ => count_badge(painter, rect, count, badge_color),
    }

    if action == RailAction::None && resp.clicked() {
        action = RailAction::Select;
    }
    action
}

fn count_badge(painter: &egui::Painter, rect: Rect, count: usize, color: egui::Color32) {
    let galley = painter.layout_no_wrap(
        format!("{:02}", count),
        crate::gui::widgets::glyph_font(10.0),
        color,
    );
    let pos = egui::pos2(
        rect.right() - 10.0 - galley.size().x,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(pos, galley, color);
}

/// Scrollable list of sound rows for the active filter.
fn sound_rows(ui: &mut Ui, state: &mut SoundSharedApp, height: f32) {
    let rows = state.list_rows();
    let filter = state.group_filter();

    if rows.is_empty() {
        empty_state(ui, state, filter, height);
        return;
    }

    // What the search box is looking for, to highlight it in the names.
    let terms = state.current_query().text_terms();

    egui::ScrollArea::vertical()
        .id_salt("sound_rows_scroll")
        .auto_shrink([false, false])
        .max_height(height)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = Metrics::ROW_GAP;

            // Actions are collected during the pass and applied afterwards, so a
            // row can never be mutated while it is being drawn.
            let mut toggle: Option<SoundId> = None;
            let mut remove: Option<SoundId> = None;
            let mut assign: Option<(SoundId, String)> = None;
            let mut volume: Option<(SoundId, std::path::PathBuf, f32)> = None;
            let mut set_loop: Option<(SoundId, std::path::PathBuf, bool)> = None;
            let mut preview: Option<(SoundId, std::path::PathBuf)> = None;
            let mut edit_tags: Option<std::path::PathBuf> = None;
            let mut locate: Option<std::path::PathBuf> = None;
            let mut remove_missing: Option<std::path::PathBuf> = None;

            for row in &rows {
                let sound = match row {
                    RowItem::Loaded(sound) => sound,
                    RowItem::Missing(missing) => {
                        match missing_row(ui, missing, &terms) {
                            MissingAction::Locate => locate = Some(missing.path.clone()),
                            MissingAction::Remove => remove_missing = Some(missing.path.clone()),
                            MissingAction::None => {}
                        }
                        continue;
                    }
                };
                let view = RowView {
                    missing: state.lib_ui.missing.contains(&sound.path),
                    previewing: state.lib_ui.is_previewing(sound.id),
                    tags: state
                        .library()
                        .sound(&sound.path)
                        .map(|s| s.tags.clone())
                        .unwrap_or_default(),
                    terms: &terms,
                };
                let action = sound_row(ui, state, sound, &view);
                match action {
                    RowAction::Toggle => toggle = Some(sound.id),
                    RowAction::Remove => remove = Some(sound.id),
                    RowAction::Assign => assign = Some((sound.id, sound.name.clone())),
                    RowAction::Volume(value) => {
                        volume = Some((sound.id, sound.path.clone(), value))
                    }
                    RowAction::SetLoop(looped) => {
                        set_loop = Some((sound.id, sound.path.clone(), looped))
                    }
                    RowAction::Preview => preview = Some((sound.id, sound.path.clone())),
                    RowAction::EditTags => edit_tags = Some(sound.path.clone()),
                    RowAction::Locate => locate = Some(sound.path.clone()),
                    RowAction::None => {}
                }
            }

            if let Some((id, path, looped)) = set_loop {
                state.request_set_sound_loop(id, &path, looped);
            }
            if let Some((id, path, value)) = volume {
                state.request_set_sound_volume(id, &path, value);
            }
            if let Some(id) = toggle {
                let playing = rows
                    .iter()
                    .any(|row| matches!(row, RowItem::Loaded(s) if s.id == id && s.playing));
                if playing {
                    state.request_stop_sound(id);
                } else {
                    state.request_play_sound(id);
                }
            }
            if let Some((id, name)) = assign {
                state.open_assign_sound(id, name);
            }
            if let Some(id) = remove {
                state.request_remove_sound(id);
            }
            if let Some((id, path)) = preview {
                state.toggle_preview(id, &path);
            }
            if let Some(path) = edit_tags {
                state.open_tag_editor(path);
            }
            if let Some(path) = locate {
                state.locate_sound(path);
            }
            if let Some(path) = remove_missing {
                state.remove_missing_sound(&path);
            }
        });
}

/// What a row wants the panel to do once the frame's borrows are released.
enum RowAction {
    None,
    Toggle,
    Remove,
    Assign,
    Volume(f32),
    SetLoop(bool),
    /// Start or stop the local preview.
    Preview,
    /// Open the tag editor.
    EditTags,
    /// Pick the new location of a missing file.
    Locate,
}

/// Library facts about a row that the engine-side entry does not carry.
struct RowView<'a> {
    /// The file is not on disk.
    missing: bool,
    /// This sound is being auditioned on this machine.
    previewing: bool,
    /// Tags of the sound.
    tags: Vec<String>,
    /// Lower-cased search terms to highlight.
    terms: &'a [String],
}

/// Width reserved for the `FILE NOT FOUND` badge next to a name.
const MISSING_BADGE_W: f32 = 96.0;

/// A single 64px sound row.
///
/// Playing rows get the rose tint, a flush left signal bar and a pulsing dot —
/// exactly like the mockup's `Intro Theme` row.
fn sound_row(
    ui: &mut Ui,
    state: &mut SoundSharedApp,
    sound: &SoundEntry,
    view: &RowView,
) -> RowAction {
    let mut action = RowAction::None;
    let height = Metrics::ROW_H;
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let hovered = resp.hovered();
    let painter = ui.painter();

    let playing = sound.playing;

    // Surface
    let fill = if playing {
        let base = alpha(Palette::ROSE_950, 68);
        // The mockup fades the tint out to the right.
        painter.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(rect.width() * 0.55, rect.height())),
            Rounding::same(Metrics::R_SM),
            base,
        );
        Palette::ROW
    } else if hovered {
        Palette::ROW_HOVER
    } else {
        Palette::ROW
    };
    painter.rect(
        rect,
        Rounding::same(Metrics::R_SM),
        fill,
        Stroke::new(
            1.0_f32,
            if playing {
                alpha(Palette::ROSE_600, 110)
            } else if view.missing {
                alpha(Palette::AMBER_400, 90)
            } else if hovered {
                Palette::BORDER_STRONG
            } else {
                Palette::BORDER
            },
        ),
    );

    if playing {
        painter.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(2.0, rect.height())),
            Rounding::ZERO,
            Palette::ROSE_500,
        );
    }

    // ---- Transport button (left) ----
    let btn_rect = Rect::from_center_size(
        egui::pos2(
            rect.left() + 16.0 + Metrics::TRANSPORT / 2.0,
            rect.center().y,
        ),
        Vec2::splat(Metrics::TRANSPORT),
    );
    let btn_resp = ui.interact(
        btn_rect,
        egui::Id::new(("row_transport", sound.id.0)),
        Sense::click(),
    );
    let (btn_fill, btn_border, btn_fg) = if playing {
        (
            Palette::ROSE_950,
            alpha(Palette::ROSE_500, 150),
            Palette::ROSE_400,
        )
    } else {
        (Palette::ROSE_600, Palette::ROSE_600, Palette::WHITE)
    };
    let btn_fill = if btn_resp.hovered() && !playing {
        Palette::ROSE_700
    } else {
        btn_fill
    };
    painter.rect(
        btn_rect,
        Rounding::same(Metrics::R_BTN),
        btn_fill,
        Stroke::new(1.0_f32, btn_border),
    );
    crate::gui::widgets::paint_icon(
        painter,
        if playing { glyph::STOP } else { glyph::PLAY },
        btn_rect.center(),
        13.8,
        btn_fg,
    );
    if btn_resp.clicked() {
        action = RowAction::Toggle;
    }

    // ---- Repeat toggle (next to the transport) ----
    // A checkbox in the console's language: an outlined square that fills with
    // rose while the sound is set to restart itself.
    let loop_rect = Rect::from_center_size(
        egui::pos2(btn_rect.right() + 8.0 + 9.0, rect.center().y),
        Vec2::splat(18.0),
    );
    let loop_resp = ui
        .interact(loop_rect, resp.id.with("loop"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    let (loop_fill, loop_border, loop_fg) = if sound.looped {
        (
            alpha(Palette::ROSE_950, 190),
            alpha(Palette::ROSE_500, 160),
            Palette::ROSE_400,
        )
    } else if loop_resp.hovered() {
        (
            Palette::TRANSPARENT,
            Palette::BORDER_STRONG,
            Palette::ZINC_300,
        )
    } else {
        (Palette::TRANSPARENT, Palette::BORDER, Palette::ZINC_500)
    };
    painter.rect(
        loop_rect,
        Rounding::same(Metrics::R_SM),
        loop_fill,
        Stroke::new(1.0_f32, loop_border),
    );
    crate::gui::widgets::paint_icon(painter, glyph::LOOP, loop_rect.center(), 13.8, loop_fg);
    if loop_resp.clicked() {
        action = RowAction::SetLoop(!sound.looped);
    }
    if loop_resp.hovered() {
        loop_resp.on_hover_text(if sound.looped {
            "Repeat: on"
        } else {
            "Repeat: off"
        });
    }

    // ---- Preview in the headphones (never sent to the players) ----
    let preview_rect = Rect::from_center_size(
        egui::pos2(loop_rect.right() + 6.0 + 9.0, rect.center().y),
        Vec2::splat(18.0),
    );
    let preview_resp = preview_button(
        ui,
        preview_rect,
        resp.id.with("preview"),
        view.previewing,
        !view.missing,
    );
    if preview_resp.clicked() {
        action = RowAction::Preview;
    }

    // The group chip is measured first: its width goes into the layout, which
    // decides what the row can show at this width (see `layout::row_layout_for`).
    let group_name = sound
        .group
        .and_then(|id| state.group_name(id))
        .unwrap_or_else(|| "No group".to_string());
    let tag_text = elide(&group_name, 14).to_uppercase();
    let tag_galley = painter.layout_no_wrap(
        tag_text,
        crate::gui::widgets::glyph_font(9.0),
        Palette::ZINC_500,
    );
    let tag_w = tag_galley.size().x + 14.0;
    let lay = row_layout_for(width, tag_w);
    let x_at = |x: f32| rect.left() + x;
    let span_rect = |span: layout::Span, h: f32| {
        Rect::from_min_max(
            egui::pos2(x_at(span.left), rect.center().y - h / 2.0),
            egui::pos2(x_at(span.right), rect.center().y + h / 2.0),
        )
    };

    // ---- Name + metadata ----
    let text_x = x_at(layout::TEXT_X);
    let name_right = x_at(lay.name.right);
    let name_clip = Rect::from_min_max(rect.min, egui::pos2(name_right.max(text_x), rect.max.y));
    let text_painter = painter.with_clip_rect(name_clip);
    let mut name_x = text_x;

    if playing {
        // Pulsing dot in front of the name (the mockup's playing indicator).
        // Reduced motion keeps the dot, drops the breath.
        let a = if state.reduced_motion() {
            200
        } else {
            let t = ui.input(|i| i.time) as f32;
            let phase = ((t * std::f32::consts::PI).sin() + 1.0) * 0.5;
            (120.0 + 135.0 * phase) as u8
        };
        painter.circle_filled(
            egui::pos2(text_x + 3.0, rect.center().y - 9.0),
            3.0,
            alpha(Palette::ROSE_500, a),
        );
        name_x += 12.0;
    }

    let name_color = if playing {
        Palette::ROSE_400
    } else {
        Palette::WHITE
    };
    // Search matches are lit inside the name. A name that does not fit is cut
    // with an ellipsis (the full name is in the tooltip).
    let full_name = elide(&sound.name, 200);
    let mut name_job = highlight_job(
        &full_name,
        view.terms,
        ui_font(14.0),
        name_color,
        Palette::WHITE,
        alpha(Palette::ROSE_600, 90),
    );
    let name_room = (name_right - name_x).max(0.0);
    let name_cut = painter.layout_job(name_job.clone()).size().x > name_room + 0.5;
    if name_cut {
        name_job.wrap = egui::text::TextWrapping {
            max_width: name_room,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('\u{2026}'),
        };
    }
    let name_galley = painter.layout_job(name_job);
    // Name sits right above the centre line, the meta line right below it;
    // the old fixed offset let the two overlap by a few pixels.
    let name_y = rect.center().y - name_galley.size().y;
    let name_size = name_galley.size();
    text_painter.galley(egui::pos2(name_x, name_y), name_galley, name_color);
    if view.missing && name_x + name_size.x + 10.0 + MISSING_BADGE_W <= name_right {
        paint_badge(
            &text_painter,
            name_x + name_size.x + 10.0,
            name_y + name_size.y / 2.0,
            "File not found",
            Palette::AMBER_400,
        );
    }

    // `48 kHz / 00:42 / WAV` — mono telemetry under the name; the first thing
    // to go when the row narrows.
    let mut extra_x = text_x;
    if lay.show_meta {
        let rate = if sound.sample_rate > 0 {
            format_sample_rate(sound.sample_rate)
        } else {
            format_sample_rate(crate::audio::TARGET_SAMPLE_RATE)
        };
        let meta = format!(
            "{} / {} / {}",
            rate,
            format_duration(sound.duration),
            crate::gui::widgets::format_extension(&sound.path)
        );
        let meta_rect = text_painter.text(
            egui::pos2(text_x, rect.center().y + 3.0),
            egui::Align2::LEFT_TOP,
            meta.to_uppercase(),
            crate::gui::widgets::glyph_font(10.0),
            Palette::ZINC_500,
        );
        // After the telemetry: how to fix a missing file, or the tags.
        extra_x = meta_rect.right() + 14.0;
    }

    // A click on the tags edits them; an untagged row offers "+ TAG" while
    // hovered. A missing file offers "LOCATE…" instead. The hit area is cut to
    // the name area so it can never cover the controls on the right.
    let meta_top = rect.center().y + 3.0;
    if view.missing {
        let galley = painter.layout_no_wrap(
            "LOCATE…".to_string(),
            crate::gui::widgets::glyph_font(10.0),
            Palette::AMBER_400,
        );
        let locate_rect = Rect::from_min_size(
            egui::pos2(extra_x - 4.0, meta_top - 2.0),
            galley.size() + Vec2::new(8.0, 4.0),
        )
        .intersect(name_clip);
        if locate_rect.is_positive() {
            let locate_resp = ui
                .interact(locate_rect, resp.id.with("locate"), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text("Pick the file's new location");
            let color = if locate_resp.hovered() {
                Palette::WHITE
            } else {
                Palette::AMBER_400
            };
            text_painter.galley(egui::pos2(extra_x, meta_top), galley, color);
            if locate_resp.clicked() {
                action = RowAction::Locate;
            }
        }
    } else if !view.tags.is_empty() || hovered {
        let (text, color) = if view.tags.is_empty() {
            ("+ TAG".to_string(), Palette::ZINC_600)
        } else {
            (format_tags(&view.tags, 4), Palette::ZINC_400)
        };
        let galley = painter.layout_no_wrap(text, crate::gui::widgets::glyph_font(10.0), color);
        let tags_rect = Rect::from_min_size(
            egui::pos2(extra_x - 4.0, meta_top - 2.0),
            galley.size() + Vec2::new(8.0, 4.0),
        )
        .intersect(name_clip);
        if tags_rect.is_positive() {
            let tags_resp = ui
                .interact(tags_rect, resp.id.with("tags"), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text("Edit tags");
            let color = if tags_resp.hovered() {
                Palette::WHITE
            } else {
                color
            };
            text_painter.galley(egui::pos2(extra_x, meta_top), galley, color);
            if tags_resp.clicked() {
                action = RowAction::EditTags;
            }
        }
    }

    // ---- Right side: group tag, volume, remove ----

    // Remove (✕)
    let remove_rect = Rect::from_center_size(
        span_rect(lay.remove, layout::REMOVE_W).center(),
        Vec2::splat(layout::SMALL_BTN),
    );
    let remove_resp = ui.interact(remove_rect, resp.id.with("remove"), Sense::click());
    crate::gui::widgets::paint_icon(
        painter,
        glyph::CROSS,
        remove_rect.center(),
        13.8,
        if remove_resp.hovered() {
            Palette::ROSE_400
        } else {
            Palette::ZINC_500
        },
    );
    if remove_resp.clicked() {
        action = RowAction::Remove;
    }

    // Volume readout, right-aligned in its fixed box.
    let pct_galley = painter.layout_no_wrap(
        format!("{:.0}%", sound.volume * 100.0),
        crate::gui::widgets::glyph_font(10.0),
        Palette::ZINC_400,
    );
    painter.galley(
        egui::pos2(
            x_at(lay.percent.right) - pct_galley.size().x,
            rect.center().y - pct_galley.size().y / 2.0,
        ),
        pct_galley,
        Palette::ZINC_400,
    );

    // Volume slider (it narrows, and goes only on a very narrow row).
    if let Some(slider_span) = lay.slider {
        let slider_rect = span_rect(slider_span, 18.0);
        let slider_resp = ui.interact(slider_rect, resp.id.with("volume"), Sense::click_and_drag());
        let slider_resp = slider_resp.on_hover_cursor(CursorIcon::ResizeHorizontal);

        let mut value = sound.volume;
        let mut changed = false;
        if let Some(pointer) = slider_resp.interact_pointer_pos() {
            if slider_resp.is_pointer_button_down_on() || slider_resp.dragged() {
                let track_left = slider_rect.left();
                let t = ((pointer.x - track_left) / slider_rect.width().max(1.0)).clamp(0.0, 1.0);
                if (t - value).abs() > f32::EPSILON {
                    value = t;
                    changed = true;
                }
            }
        }

        // 2px track with a square thumb, matching the mockup.
        let track = Rect::from_min_size(
            egui::pos2(slider_rect.left(), slider_rect.center().y - 1.0),
            Vec2::new(slider_rect.width(), 2.0),
        );
        painter.rect_filled(track, Rounding::ZERO, Palette::BORDER_STRONG);
        let shown = value.clamp(0.0, 1.0);
        if shown > 0.0 {
            painter.rect_filled(
                Rect::from_min_size(track.min, Vec2::new(track.width() * shown, 2.0)),
                Rounding::ZERO,
                if playing {
                    Palette::ROSE_500
                } else {
                    Palette::ZINC_500
                },
            );
        }
        let thumb_size = if slider_resp.hovered() { 6.0 } else { 4.5 };
        painter.rect_filled(
            Rect::from_center_size(
                egui::pos2(track.left() + track.width() * shown, track.center().y),
                Vec2::splat(thumb_size),
            ),
            Rounding::ZERO,
            if playing {
                Palette::ROSE_500
            } else {
                Palette::ZINC_400
            },
        );

        if changed {
            action = RowAction::Volume(value);
        }
    }

    // Group tag (click to reassign); only when the row is wide enough.
    if let Some(tag_span) = lay.tag {
        let tag_rect = span_rect(tag_span, 18.0);
        let tag_resp = ui.interact(tag_rect, resp.id.with("tag"), Sense::click());
        let tag_resp = tag_resp.on_hover_cursor(CursorIcon::PointingHand);
        painter.rect(
            tag_rect,
            Rounding::same(Metrics::R_SM),
            if tag_resp.hovered() {
                Palette::BORDER
            } else {
                Palette::TRANSPARENT
            },
            Stroke::new(1.0_f32, Palette::BORDER),
        );
        let text_w = tag_galley.size().x.min(tag_rect.width() - 4.0);
        painter.with_clip_rect(tag_rect).galley(
            egui::pos2(
                tag_rect.center().x - text_w / 2.0,
                tag_rect.center().y - tag_galley.size().y / 2.0,
            ),
            tag_galley,
            Palette::ZINC_500,
        );
        if tag_resp.clicked() {
            action = RowAction::Assign;
        } else if tag_resp.hovered() {
            tag_resp.on_hover_text("Change group");
        }
    }

    // Right-click: add to a scene, flag as a stinger.
    resp.context_menu(|ui| {
        crate::gui::scenes::sound_context_menu(ui, state, sound);
        crate::gui::pan_ui::sound_pan_section(ui, state, sound);
    });

    if action_is_none(&action) && resp.hovered() {
        // A cut name, a hidden group and a missing slider are all in here.
        resp.on_hover_text(format!(
            "{}\n{}\nGroup: {}",
            sound.name,
            sound.path.to_string_lossy(),
            group_name
        ));
    }

    action
}

fn action_is_none(action: &RowAction) -> bool {
    matches!(action, RowAction::None)
}

/// Height of the empty state's core: title, filter line, hint and the primary
/// button with their gaps.
const EMPTY_CORE_H: f32 = 136.0;
/// Height of the "...or drop in a YouTube link" block (gap, caption, gap,
/// button).
const EMPTY_YOUTUBE_H: f32 = 84.0;
/// Height of the "Show All" block (gap, button).
const EMPTY_SHOW_ALL_H: f32 = 48.0;

/// What the empty state shows for the height it is given.
#[derive(Debug, Clone, Copy, PartialEq)]
struct EmptyLayout {
    /// Space above the title.
    top_pad: f32,
    /// Show the YouTube alternative.
    youtube: bool,
    /// Show the "Show All" button.
    show_all: bool,
}

/// Decide how much of the empty state fits in `height`.
///
/// The optional blocks are dropped rather than letting the column spill over
/// the master strip below it ("Show All" is kept before the YouTube hint when
/// there is a group filter to leave); the header offers the same actions.
fn empty_layout(height: f32, has_group_filter: bool) -> EmptyLayout {
    let show_all = has_group_filter && height >= EMPTY_CORE_H + EMPTY_SHOW_ALL_H + Metrics::SM;
    let base = EMPTY_CORE_H + if show_all { EMPTY_SHOW_ALL_H } else { 0.0 };
    let youtube = height >= base + EMPTY_YOUTUBE_H + Metrics::SM;
    let content = base + if youtube { EMPTY_YOUTUBE_H } else { 0.0 };
    let top_pad = ((height - content) * 0.4).clamp(Metrics::SM, 120.0);
    EmptyLayout {
        top_pad,
        youtube,
        show_all,
    }
}

/// Centred empty state with a contextual call to action.
fn empty_state(ui: &mut Ui, state: &mut SoundSharedApp, filter: GroupFilter, height: f32) {
    // A search or tag filter that matches nothing is not an empty library.
    if state.lib_ui.filter.is_active() {
        ui.allocate_ui(Vec2::new(ui.available_width(), height), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((height * 0.2).clamp(12.0, 90.0));
                ui.label(telemetry("NO MATCHES", 22.0, Palette::BORDER_STRONG));
                ui.add_space(Metrics::SM);
                ui.label(micro_label("Nothing fits the search and tags above"));
                ui.add_space(Metrics::LG);
                if ConsoleButton::ghost("Clear Filters")
                    .glyph(glyph::CROSS)
                    .width(150.0)
                    .id_salt("empty_clear_filters")
                    .show(ui)
                    .clicked()
                {
                    state.lib_ui.filter.clear();
                }
            });
        });
        return;
    }
    let filter_desc = match filter {
        GroupFilter::All => "No sounds yet".to_string(),
        GroupFilter::Ungrouped => "Nothing ungrouped".to_string(),
        GroupFilter::Group(id) => match state.group_name(id) {
            Some(name) => format!("{} is empty", name),
            None => "Group is empty".to_string(),
        },
    };

    let layout = empty_layout(
        height,
        matches!(filter, GroupFilter::Group(_) | GroupFilter::Ungrouped),
    );
    ui.allocate_ui(Vec2::new(ui.available_width(), height), |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(layout.top_pad);
            ui.label(telemetry("NO SIGNAL", 22.0, Palette::BORDER_STRONG));
            ui.add_space(Metrics::SM);
            ui.label(telemetry(
                filter_desc.to_uppercase(),
                11.0,
                Palette::ZINC_400,
            ));
            ui.add_space(Metrics::SM);
            ui.label(micro_label(match filter {
                GroupFilter::Group(_) => "Add a sound to attach it to this group",
                _ => "Add a WAV / OGG / MP3 to start a board",
            }));
            ui.add_space(Metrics::LG);
            if ConsoleButton::primary("Add Sound")
                .glyph(glyph::PLUS)
                .width(150.0)
                .id_salt("empty_add")
                .show(ui)
                .clicked()
            {
                state.open_file_dialog();
            }
            // The panel zeroes egui's implicit vertical spacing, so every gap
            // between the stacked call-to-action pieces is placed here.
            if layout.youtube {
                ui.add_space(Metrics::XL);
                ui.label(micro_label("…or drop in a YouTube link"));
                ui.add_space(Metrics::MD);
                if ConsoleButton::ghost("YouTube")
                    .glyph(glyph::WAVE)
                    .width(150.0)
                    .id_salt("empty_add_youtube")
                    .show(ui)
                    .clicked()
                {
                    state.open_add_youtube();
                }
            }
            if layout.show_all {
                ui.add_space(Metrics::MD);
                if ConsoleButton::ghost("Show All")
                    .glyph(glyph::FOLDER)
                    .width(150.0)
                    .id_salt("empty_show_all")
                    .show(ui)
                    .clicked()
                {
                    state.set_group_filter(GroupFilter::All);
                }
            }
        });
    });
}

/// `MASTER OUTPUT ▮▮▮▮…  80%  [MUTE]` strip at the bottom of the panel.
///
/// The slider and the level meter narrow with the panel (the meter goes first
/// when there is no room for it), so Mute never leaves the panel.
fn master_output(ui: &mut Ui, state: &mut SoundSharedApp) {
    ui.horizontal(|ui| {
        // Gaps are placed by hand so the widths budgeted by `master_layout`
        // are exact.
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.label(micro_label("Master Output"));
        ui.add_space(Metrics::LABEL_GAP);
        // What is left of the strip after the caption (1px of slack keeps a
        // rounding error from wrapping the last control).
        let room = (ui.available_width() - 1.0).max(0.0);
        let lay = master_layout(room);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let muted = state.is_muted();
            let mute_label = if muted { "Unmute" } else { "Mute" };
            let resp = ConsoleButton::new(mute_label)
                .glyph(if muted { glyph::MUTED } else { glyph::VOLUME })
                .kind(if muted {
                    crate::gui::widgets::BtnKind::Armed
                } else {
                    crate::gui::widgets::BtnKind::Ghost
                })
                .small()
                .width(lay.mute_w)
                .id_salt("master_mute")
                .show(ui);
            if resp.clicked() {
                state.toggle_mute();
            }

            ui.add_space(Metrics::SM);

            let value = state.get_master_volume();
            ui.label(
                egui::RichText::new(format!("{:.0}%", value * 100.0))
                    .font(crate::gui::widgets::glyph_font(11.0))
                    .color(Palette::WHITE),
            );

            if let Some(meter_w) = lay.meter_w {
                ui.add_space(Metrics::SM);
                // Live output level (peak, dB scale), not the fader position.
                segmented_meter(ui, state.output_meter(), meter_cells(meter_w), meter_w, 8.0);
                ui.add_space(Metrics::MD);
            } else {
                ui.add_space(Metrics::SM);
            }
            let mut slider_value = value;
            let slider = level_slider(ui, &mut slider_value, lay.slider_w, "master_panel");
            if slider.changed() {
                state.set_master_volume(slider_value);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roomy_empty_state_shows_everything() {
        let layout = empty_layout(400.0, true);
        assert!(layout.youtube && layout.show_all);
        assert!(layout.top_pad >= Metrics::SM && layout.top_pad <= 120.0);
    }

    #[test]
    fn short_empty_state_drops_optional_blocks() {
        let layout = empty_layout(100.0, true);
        assert!(!layout.youtube && !layout.show_all);
        assert_eq!(layout.top_pad, Metrics::SM);
    }

    #[test]
    fn show_all_is_kept_before_the_youtube_hint() {
        let height = EMPTY_CORE_H + EMPTY_SHOW_ALL_H + Metrics::SM + 1.0;
        let layout = empty_layout(height, true);
        assert!(layout.show_all && !layout.youtube);
    }

    #[test]
    fn show_all_needs_a_group_filter() {
        assert!(!empty_layout(500.0, false).show_all);
    }

    #[test]
    fn shown_content_always_fits() {
        let mut height = 60.0;
        while height < 600.0 {
            for filter in [false, true] {
                let layout = empty_layout(height, filter);
                let content = EMPTY_CORE_H
                    + if layout.show_all {
                        EMPTY_SHOW_ALL_H
                    } else {
                        0.0
                    }
                    + if layout.youtube { EMPTY_YOUTUBE_H } else { 0.0 };
                if height >= EMPTY_CORE_H + 2.0 * Metrics::SM {
                    assert!(
                        layout.top_pad + content <= height + f32::EPSILON,
                        "height {height}: {layout:?}"
                    );
                }
            }
            height += 7.0;
        }
    }

    // ---- headless frames of the whole panel ----

    /// The panel's border box for a window `width` wide, as `gui::mod` sizes it.
    fn left_panel_rect(width: f32, height: f32) -> Rect {
        let content = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(width, height))
            .shrink(Metrics::APP_PAD);
        let inner = content.shrink(1.0);
        let right_left = inner.right() - (Metrics::RIGHT_COL_W - 2.0);
        Rect::from_min_max(
            inner.min,
            egui::pos2(right_left - Metrics::COL_GAP - 2.0, inner.bottom()),
        )
    }

    /// Right edge of every painted box or line (text is clipped by its own
    /// painter, so only filled / stroked shapes are measured).
    fn box_rights(shape: &egui::Shape, out: &mut Vec<f32>) {
        match shape {
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| box_rights(s, out)),
            egui::Shape::Rect(_) | egui::Shape::LineSegment { .. } => {
                out.push(shape.visual_bounding_rect().right());
            }
            _ => {}
        }
    }

    /// Run the panel for a few frames in a window `width` wide; returns the
    /// right edges of everything it painted and the panel's right border.
    fn run_panel(width: f32, search: &str, muted: bool) -> (Vec<f32>, f32) {
        use crate::gui::hotkeys_ui::test_support::harness;

        let mut h = harness(crate::config::Config::default(), false);
        let rain =
            std::path::PathBuf::from("/nonexistent/rain/Rain On A Very Long Tavern Roof.ogg");
        let gone = std::path::PathBuf::from("/nonexistent/gone/Missing Thunder.ogg");
        let group = h.app.library.add_group("A rather long group name");
        for (n, name) in [
            "Rain",
            "A sound with a really quite long name indeed",
            "Wolf",
        ]
        .iter()
        .enumerate()
        {
            let path = if n == 0 {
                rain.clone()
            } else {
                std::path::PathBuf::from(format!("/nonexistent/{n}.ogg"))
            };
            let group = if n == 1 { group } else { None };
            h.app.library.insert_sound(&path, name, 1.0, group);
            let id = SoundId(uuid::Uuid::from_u128(n as u128 + 1));
            h.app.sounds.lock().unwrap().insert(
                id,
                SoundEntry {
                    id,
                    name: name.to_string(),
                    path,
                    playing: n == 1,
                    volume: 1.0,
                    looped: false,
                    group,
                    duration: 190.0,
                    sample_rate: 44_100,
                },
            );
            h.app.sound_order.lock().unwrap().push(id);
        }
        h.app
            .library
            .insert_sound(&gone, "Missing Thunder", 0.7, None);
        h.app.lib_ui.missing.insert(gone);
        h.app.lib_ui.filter.text = search.to_string();
        if muted {
            h.app.toggle_mute();
        }

        let height = 700.0;
        let panel = left_panel_rect(width, height);
        let mut last = None;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width, height),
                )),
                ..Default::default()
            };
            last = Some(h.ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(panel)
                                .layout(Layout::top_down(Align::Min)),
                            |ui| sound_library_panel(ui, &mut h.app, panel.size()),
                        );
                    });
            }));
        }
        let mut rights = Vec::new();
        for clipped in &last.unwrap().shapes {
            box_rights(&clipped.shape, &mut rights);
        }
        // The panel is stroked one pixel outside its fill rect.
        (rights, panel.right() + 1.0)
    }

    #[test]
    fn panel_draws_at_900_and_960_without_leaving_its_border() {
        for width in [900.0_f32, 960.0] {
            for (search, muted) in [("", false), ("ra", true)] {
                let (rights, limit) = run_panel(width, search, muted);
                assert!(!rights.is_empty());
                for right in rights {
                    assert!(
                        right <= limit + 1.0,
                        "width {width} search {search:?}: a box reaches {right}, limit {limit}"
                    );
                }
            }
        }
    }
}
