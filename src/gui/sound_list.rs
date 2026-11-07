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

use crate::audio::SoundId;
use crate::gui::app::{SoundEntry, SoundSharedApp};
use crate::gui::groups::GroupFilter;
use crate::gui::theme::{
    alpha, console_panel, hairline, micro_label, micro_label_accent, panel_title, telemetry,
    ui_font, Metrics, Palette,
};
use crate::gui::widgets::{
    elide, format_duration, format_sample_rate, glyph, level_slider, segmented_meter,
    ConsoleButton, IconButton,
};
use egui::{Align, CursorIcon, Layout, Rect, Rounding, Sense, Stroke, Ui, Vec2};

/// Renders the whole `01 / LIBRARY` panel at the given size.
pub fn sound_library_panel(ui: &mut Ui, state: &mut SoundSharedApp, size: Vec2) {
    console_panel(ui, size, |ui| {
        header(ui, state);
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
                // The rail is the primary navigation; it only collapses when the
                // sound rows themselves would become too narrow to read.
                let rail_fits = size.x >= Metrics::RAIL_W + Metrics::MD + Metrics::MIN_ROW_W;
                if rail_fits {
                    group_rail(ui, state, list_h);
                    ui.add_space(Metrics::MD);
                }
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), list_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        sound_rows(ui, state, list_h);
                    },
                );
            },
        );

        ui.add_space(Metrics::MD);
        hairline(ui);
        ui.add_space(Metrics::MD - 2.0);
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
                ui.add_space(Metrics::XS);
                let total = state.count_for(GroupFilter::All);
                ui.label(micro_label(match total {
                    1 => "1 Track".to_string(),
                    n => format!("{} Tracks", n),
                }));
            });
        });

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ConsoleButton::primary("Add Sound")
                .glyph(glyph::PLUS)
                .font_size(11.0)
                .width(150.0)
                .id_salt("library_add")
                .show(ui)
                .clicked()
            {
                state.open_file_dialog();
            }
            ui.add_space(Metrics::SM);
            if ConsoleButton::ghost("YouTube")
                .glyph(glyph::WAVE)
                .font_size(11.0)
                .width(110.0)
                .id_salt("library_add_youtube")
                .show(ui)
                .clicked()
            {
                state.open_add_youtube();
            }
        });
    });
}

/// Vertical list of category chips: `ALL`, `UNGROUPED`, then each group.
///
/// Selecting a chip filters the rows. Each chip carries its sound count and, on
/// hover, reveals rename/delete actions for real groups.
fn group_rail(ui: &mut Ui, state: &mut SoundSharedApp, height: f32) {
    ui.allocate_ui_with_layout(
        Vec2::new(Metrics::RAIL_W, height),
        Layout::top_down(Align::Min),
        |ui| {
            ui.set_width(Metrics::RAIL_W);

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
            ui.add_space(Metrics::XS);

            egui::ScrollArea::vertical()
                .id_salt("group_rail_scroll")
                .auto_shrink([false, false])
                .max_height((height - 28.0).max(40.0))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;

                    let active = state.group_filter();
                    let ungrouped = state.count_for(GroupFilter::Ungrouped);
                    let all = state.count_for(GroupFilter::All);

                    if rail_chip(ui, "All Sounds", all, active == GroupFilter::All, None)
                        == RailAction::Select
                    {
                        state.set_group_filter(GroupFilter::All);
                    }
                    if rail_chip(
                        ui,
                        "Ungrouped",
                        ungrouped,
                        active == GroupFilter::Ungrouped,
                        None,
                    ) == RailAction::Select
                    {
                        state.set_group_filter(GroupFilter::Ungrouped);
                    }

                    if !state.library().groups().is_empty() {
                        ui.add_space(Metrics::SM);
                        ui.label(micro_label("Categories"));
                        ui.add_space(2.0);
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
                                state.count_for(GroupFilter::Group(group.id)),
                            )
                        })
                        .collect();

                    for (id, name, count) in groups {
                        let selected = active == GroupFilter::Group(id);
                        match rail_chip(ui, &name, count, selected, Some(id)) {
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
    label: &str,
    count: usize,
    selected: bool,
    group: Option<crate::gui::groups::GroupId>,
) -> RailAction {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(Metrics::RAIL_W, Metrics::RAIL_ITEM_H),
        Sense::click(),
    );
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
    let galley = painter.layout_no_wrap(elide(label, 17), ui_font(12.0), text_color);
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
    let sounds = state.visible_sounds();
    let filter = state.group_filter();

    if sounds.is_empty() {
        empty_state(ui, state, filter, height);
        return;
    }

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

            for sound in &sounds {
                let action = sound_row(ui, state, sound);
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
                let playing = sounds.iter().any(|s| s.id == id && s.playing);
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
}

/// A single 64px sound row.
///
/// Playing rows get the rose tint, a flush left signal bar and a pulsing dot —
/// exactly like the mockup's `Intro Theme` row.
fn sound_row(ui: &mut Ui, state: &mut SoundSharedApp, sound: &SoundEntry) -> RowAction {
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

    // ---- Name + metadata ----
    let text_x = loop_rect.right() + 12.0;
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
    let name = elide(&sound.name, 38);
    let name_galley = painter.layout_no_wrap(name, ui_font(14.0), name_color);
    // Name sits right above the centre line, the meta line right below it;
    // the old fixed offset let the two overlap by a few pixels.
    let name_y = rect.center().y - name_galley.size().y;
    painter.galley(egui::pos2(name_x, name_y), name_galley, name_color);

    // `48 kHz / 00:42 / WAV` — mono telemetry under the name.
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
    painter.text(
        egui::pos2(text_x, rect.center().y + 3.0),
        egui::Align2::LEFT_TOP,
        meta.to_uppercase(),
        crate::gui::widgets::glyph_font(10.0),
        Palette::ZINC_500,
    );

    // ---- Right side: group tag, volume, assign, remove ----
    let mut cursor = rect.right() - 10.0;

    // Remove (✕)
    let remove_rect =
        Rect::from_center_size(egui::pos2(cursor - 9.0, rect.center().y), Vec2::splat(18.0));
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
    cursor -= 24.0;

    // Volume readout + slider
    let pct_galley = painter.layout_no_wrap(
        format!("{:.0}%", sound.volume * 100.0),
        crate::gui::widgets::glyph_font(10.0),
        Palette::ZINC_400,
    );
    // Fixed-width, right-aligned readout: "100%" is wider than "70%", and a
    // width that follows the text shifted the slider from row to row.
    const PCT_W: f32 = 34.0;
    cursor -= PCT_W;
    painter.galley(
        egui::pos2(
            cursor + PCT_W - pct_galley.size().x,
            rect.center().y - pct_galley.size().y / 2.0,
        ),
        pct_galley,
        Palette::ZINC_400,
    );

    cursor -= Metrics::SM;
    let slider_rect = Rect::from_center_size(
        egui::pos2(cursor - Metrics::ROW_SLIDER_W / 2.0, rect.center().y),
        Vec2::new(Metrics::ROW_SLIDER_W, 18.0),
    );
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
    cursor -= Metrics::ROW_SLIDER_W;

    // Group tag (click to reassign)
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
    cursor -= Metrics::MD;
    let tag_rect = Rect::from_center_size(
        egui::pos2(cursor - tag_w / 2.0, rect.center().y),
        Vec2::new(tag_w, 18.0),
    );
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
    painter.galley(
        egui::pos2(
            tag_rect.center().x - tag_galley.size().x / 2.0,
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

    if action_is_none(&action) && resp.hovered() {
        resp.on_hover_text(sound.path.to_string_lossy());
    }

    action
}

fn action_is_none(action: &RowAction) -> bool {
    matches!(action, RowAction::None)
}

/// Centred empty state with a contextual call to action.
fn empty_state(ui: &mut Ui, state: &mut SoundSharedApp, filter: GroupFilter, height: f32) {
    let filter_desc = match filter {
        GroupFilter::All => "No sounds yet".to_string(),
        GroupFilter::Ungrouped => "Nothing ungrouped".to_string(),
        GroupFilter::Group(id) => match state.group_name(id) {
            Some(name) => format!("{} is empty", name),
            None => "Group is empty".to_string(),
        },
    };

    ui.allocate_ui(Vec2::new(ui.available_width(), height), |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((height * 0.28).clamp(12.0, 120.0));
            ui.label(telemetry("NO SIGNAL", 22.0, Palette::BORDER_STRONG));
            ui.add_space(Metrics::SM);
            ui.label(telemetry(
                filter_desc.to_uppercase(),
                11.0,
                Palette::ZINC_400,
            ));
            ui.add_space(Metrics::XS);
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
            ui.add_space(Metrics::SM);
            ui.label(micro_label("…or drop in a YouTube link"));
            if ConsoleButton::ghost("YouTube")
                .glyph(glyph::WAVE)
                .width(150.0)
                .id_salt("empty_add_youtube")
                .show(ui)
                .clicked()
            {
                state.open_add_youtube();
            }
            if matches!(filter, GroupFilter::Group(_) | GroupFilter::Ungrouped) {
                ui.add_space(Metrics::SM);
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
fn master_output(ui: &mut Ui, state: &mut SoundSharedApp) {
    ui.horizontal(|ui| {
        ui.label(micro_label("Master Output"));
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
                .width(92.0)
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

            ui.add_space(Metrics::SM);
            // Live output level (peak, dB scale), not the fader position.
            segmented_meter(ui, state.output_meter(), 20, 160.0, 8.0);

            ui.add_space(Metrics::MD);
            let mut slider_value = value;
            let slider = level_slider(ui, &mut slider_value, 120.0, "master_panel");
            if slider.changed() {
                state.set_master_volume(slider_value);
            }
        });
    });
}
