//! Modal dialogs for the console: create / rename / delete a group, and move a
//! sound between groups.
//!
//! Every modal is drawn with the standard console frame so it reads as part of
//! the same instrument: hairline border, `#111111` surface, mono micro-labels
//! and square buttons. `Esc` and the scrim click both dismiss without applying.

use crate::gui::app::{Dialog, SoundSharedApp};
use crate::gui::groups::{GroupFilter, GroupId};
use crate::gui::theme::{alpha, hairline, micro_label, panel_title, Metrics, Palette};
use crate::gui::widgets::{glyph, ConsoleButton};
use egui::{Align, Context, Layout, RichText, Ui, Vec2};

/// Fixed width of every modal.
const MODAL_W: f32 = 340.0;

/// Paints whichever modal is open. Safe to call every frame.
pub fn show_dialogs(ctx: &Context, state: &mut SoundSharedApp) {
    let dialog = state.dialog().clone();
    if matches!(dialog, Dialog::None) {
        return;
    }

    paint_scrim(ctx);

    // A click on the scrim or `Esc` dismisses the modal — except while a
    // YouTube download is running: that modal closes when the worker reports
    // back, so the download cannot be forgotten mid-flight.
    // While the hotkey editor waits for a key, Esc cancels the capture instead.
    let escaped = ctx.input(|i| i.key_pressed(egui::Key::Escape))
        && !state.youtube_busy()
        && !state.hotkeys_capturing();
    if escaped {
        state.close_dialog();
        return;
    }

    match dialog {
        Dialog::None => {}

        Dialog::Hotkeys => crate::gui::hotkeys_ui::show_hotkeys_dialog(ctx, state),

        Dialog::Timers => crate::gui::timeline::show_timers_dialog(ctx, state),

        Dialog::NewGroup { name, error } => {
            let mut field = name;
            let mut submit = false;
            let mut cancel = false;

            modal(ctx, |ui| {
                header(ui, "New Group", "Categories keep a board readable");
                ui.add_space(Metrics::LG);

                field_label(ui, "Group name");
                let resp = name_field(ui, &mut field, "e.g. Combat");
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(Metrics::SM);
                match &error {
                    Some(message) => error_line(ui, message),
                    None => hint_line(ui, "Up to 32 characters"),
                }

                footer_rule(ui);

                dialog_buttons(ui, |ui| {
                    if ConsoleButton::primary("Create")
                        .glyph(glyph::CHECK)
                        .width(150.0)
                        .height(Metrics::BTN_H)
                        .id_salt("dlg_new_ok")
                        .show(ui)
                        .clicked()
                    {
                        submit = true;
                    }
                    if ConsoleButton::ghost("Cancel")
                        .glyph(glyph::CROSS)
                        .width(120.0)
                        .height(Metrics::BTN_H)
                        .id_salt("dlg_new_cancel")
                        .show(ui)
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });

            if let Dialog::NewGroup { name, .. } = state.dialog_mut() {
                *name = field;
            }
            if cancel {
                state.close_dialog();
            } else if submit {
                state.submit_new_group();
            }
        }

        Dialog::RenameGroup { name, error, .. } => {
            let mut field = name;
            let mut submit = false;
            let mut cancel = false;

            modal(ctx, |ui| {
                header(ui, "Rename Group", "Sounds stay where they are");
                ui.add_space(Metrics::LG);

                field_label(ui, "Group name");
                let resp = name_field(ui, &mut field, "Group name");
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(Metrics::SM);
                match &error {
                    Some(message) => error_line(ui, message),
                    None => hint_line(ui, "Up to 32 characters"),
                }

                footer_rule(ui);

                dialog_buttons(ui, |ui| {
                    if ConsoleButton::primary("Save")
                        .glyph(glyph::CHECK)
                        .width(150.0)
                        .height(Metrics::BTN_H)
                        .id_salt("dlg_rename_ok")
                        .show(ui)
                        .clicked()
                    {
                        submit = true;
                    }
                    if ConsoleButton::ghost("Cancel")
                        .glyph(glyph::CROSS)
                        .width(120.0)
                        .height(Metrics::BTN_H)
                        .id_salt("dlg_rename_cancel")
                        .show(ui)
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });

            if let Dialog::RenameGroup { name, .. } = state.dialog_mut() {
                *name = field;
            }
            if cancel {
                state.close_dialog();
            } else if submit {
                state.submit_rename_group();
            }
        }

        Dialog::DeleteGroup { id } => {
            let group_name = state.group_name(id).unwrap_or_else(|| "this group".into());
            let count = state.count_for(GroupFilter::Group(id));
            let mut confirm = false;
            let mut cancel = false;

            modal(ctx, |ui| {
                header(ui, "Delete Group", "This cannot be undone");
                ui.add_space(Metrics::LG);

                ui.add(
                    egui::Label::new(
                        RichText::new(delete_group_message(&group_name, count))
                            .font(crate::gui::widgets::glyph_font(11.0))
                            .color(Palette::ZINC_300)
                            .line_height(Some(BODY_LINE_H)),
                    )
                    .wrap(),
                );

                footer_rule(ui);

                dialog_buttons(ui, |ui| {
                    if ConsoleButton::danger("Delete")
                        .glyph(glyph::TRASH)
                        .width(150.0)
                        .height(Metrics::BTN_H)
                        .id_salt("dlg_delete_ok")
                        .show(ui)
                        .clicked()
                    {
                        confirm = true;
                    }
                    if ConsoleButton::ghost("Keep")
                        .glyph(glyph::CHECK)
                        .width(120.0)
                        .height(Metrics::BTN_H)
                        .id_salt("dlg_delete_cancel")
                        .show(ui)
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });

            if cancel {
                state.close_dialog();
            } else if confirm {
                state.confirm_delete_group();
            }
        }

        Dialog::AssignSound { sound_id, name } => {
            let groups: Vec<(GroupId, String)> = state
                .library()
                .groups()
                .iter()
                .map(|group| (group.id, group.name.clone()))
                .collect();
            // Look the sound up among all loaded sounds, not the visible rows:
            // filtering the board to another group used to make this dialog
            // highlight "Ungrouped" for a sound that is in a group.
            let current = state
                .sounds()
                .iter()
                .find(|entry| entry.id == sound_id)
                .map(|entry| entry.group)
                .unwrap_or(None);

            let mut chosen: Option<Option<GroupId>> = None;
            let mut cancel = false;

            modal(ctx, |ui| {
                header(ui, "Move Sound", &crate::gui::widgets::elide(&name, 26));
                ui.add_space(Metrics::LG);

                field_label(ui, "Assign to");

                egui::ScrollArea::vertical()
                    .id_salt("assign_scroll")
                    .max_height(220.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = Metrics::XS;
                        if assign_row(ui, "Ungrouped", current.is_none()) {
                            chosen = Some(None);
                        }
                        for (id, group_name) in &groups {
                            if assign_row(ui, group_name, current == Some(*id)) {
                                chosen = Some(Some(*id));
                            }
                        }
                        if groups.is_empty() {
                            hint_line(ui, "No groups yet");
                        }
                    });

                footer_rule(ui);

                dialog_buttons(ui, |ui| {
                    if ConsoleButton::ghost("Cancel")
                        .glyph(glyph::CROSS)
                        .width(150.0)
                        .height(Metrics::BTN_H)
                        .id_salt("dlg_assign_cancel")
                        .show(ui)
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });

            if cancel {
                state.close_dialog();
            } else if let Some(group) = chosen {
                state.assign_sound_group(group);
            }
        }

        Dialog::SaveMixAsScene { .. } | Dialog::EditScene(_) | Dialog::DeleteScene { .. } => {
            crate::gui::scenes::show_scene_dialog(ctx, state, dialog);
        }

        Dialog::AddYouTube { url, error } => {
            let mut field = url;
            let mut submit = false;
            let mut cancel = false;
            let busy = state.youtube_busy();
            let status = state.youtube_status();

            modal(ctx, |ui| {
                header(
                    ui,
                    "Add From YouTube",
                    "The audio is downloaded once, then played like any local sound",
                );
                ui.add_space(Metrics::LG);

                field_label(ui, "Video or music link");
                let resp = name_field(ui, &mut field, "https://youtube.com/watch?v=…");
                if !busy && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(Metrics::SM);
                if let Some(message) = &error {
                    error_line(ui, message);
                } else if busy {
                    hint_line(ui, &status);
                } else {
                    hint_line(
                        ui,
                        "First use downloads yt-dlp + ffmpeg once (~230 MB), then it just works",
                    );
                }

                footer_rule(ui);

                dialog_buttons(ui, |ui| {
                    if busy {
                        if ConsoleButton::ghost("Close")
                            .font_size(11.0)
                            .width(150.0)
                            .height(Metrics::BTN_H)
                            .id_salt("dlg_yt_cancel")
                            .show(ui)
                            .clicked()
                        {
                            // The download keeps running in the background and
                            // lands on the board (or in the log) when it ends.
                            cancel = true;
                        }
                    } else if ConsoleButton::primary("Add")
                        .glyph(glyph::PLUS)
                        .width(150.0)
                        .height(Metrics::BTN_H)
                        .id_salt("dlg_yt_ok")
                        .show(ui)
                        .clicked()
                    {
                        submit = true;
                    }
                    if !busy
                        && ConsoleButton::ghost("Cancel")
                            .glyph(glyph::CROSS)
                            .width(120.0)
                            .height(Metrics::BTN_H)
                            .id_salt("dlg_yt_cancel2")
                            .show(ui)
                            .clicked()
                    {
                        cancel = true;
                    }
                });
            });

            if let Dialog::AddYouTube { url, .. } = state.dialog_mut() {
                *url = field;
            }
            if cancel {
                state.close_dialog();
            } else if submit {
                state.submit_add_youtube();
            }
        }

        Dialog::Music => crate::gui::music::show_music_dialog(ctx, state),

        Dialog::EditEmitters(scene) => crate::gui::music::show_emitters_dialog(ctx, state, scene),
    }
}

/// One selectable row inside the "move sound" list.
fn assign_row(ui: &mut Ui, label: &str, selected: bool) -> bool {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 32.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hovered = resp.hovered();
    let painter = ui.painter();

    if selected {
        painter.rect_filled(
            rect,
            egui::Rounding::same(Metrics::R_SM),
            alpha(Palette::ROSE_600, 40),
        );
        painter.rect_stroke(
            rect,
            egui::Rounding::same(Metrics::R_SM),
            egui::Stroke::new(1.0_f32, Palette::ROSE_500),
        );
    } else if hovered {
        painter.rect_filled(
            rect,
            egui::Rounding::same(Metrics::R_SM),
            Palette::ROW_HOVER,
        );
    }

    let color = if selected {
        Palette::WHITE
    } else {
        Palette::ZINC_300
    };
    let galley = painter.layout_no_wrap(
        crate::gui::widgets::elide(label, 28),
        crate::gui::widgets::glyph_font(12.0),
        color,
    );
    painter.galley(
        egui::pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );

    if selected {
        crate::gui::widgets::paint_icon(
            painter,
            glyph::CHECK,
            egui::pos2(rect.right() - 14.0, rect.center().y),
            15.0,
            Palette::ROSE_400,
        );
    }

    resp.clicked()
}

/// Standard modal chrome: centred, fixed width, console panel frame.
///
/// A window gets egui's implicit 8px between widgets on top of any explicit
/// gap. Zeroing the vertical spacing makes every gap in these dialogs one the
/// code asks for.
fn modal(ctx: &Context, add: impl FnOnce(&mut Ui)) {
    modal_sized(ctx, "soundshared_modal", MODAL_W, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui);
    });
}

/// [`modal`] with its own window id and width (the scene editor is wider).
pub(crate) fn modal_sized(ctx: &Context, id: &'static str, width: f32, add: impl FnOnce(&mut Ui)) {
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
            add(ui);
        });
}

/// Dim everything behind the modal so the dialog reads as modal.
fn paint_scrim(ctx: &Context) {
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("modal_scrim"),
    ));
    painter.rect_filled(screen, egui::Rounding::ZERO, alpha(Palette::BLACK, 170));
}

/// `TITLE` + a one-line explanation.
///
/// The gaps are explicit and independent of the caller's `item_spacing`, so
/// the header looks the same in every modal.
pub(crate) fn header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.label(crate::gui::theme::micro_label_accent("Confirm"));
        ui.add_space(Metrics::XS);
        ui.label(panel_title(title));
        ui.add_space(Metrics::XS);
        hint_line(ui, subtitle);
    });
}

/// Line height of wrapped 10px notes: roughly 1.7x, so a second line does not
/// sit on the first.
const NOTE_LINE_H: f32 = Metrics::MICRO * 1.7;

/// Line height of the 11px body text of a dialog.
const BODY_LINE_H: f32 = 11.0 * 1.6;

/// Muted note that wraps inside the dialog, with room between its lines.
pub(crate) fn hint_line(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(micro_label(text).line_height(Some(NOTE_LINE_H))).wrap());
}

/// Caption above a field or a list; the field starts a full `SM` below it.
pub(crate) fn field_label(ui: &mut Ui, text: &str) {
    ui.label(micro_label(text));
    ui.add_space(Metrics::SM);
}

/// The rule above the button row of a dialog: `LG` of air above it, `MD`
/// below, so the buttons touch neither the content nor the rule.
pub(crate) fn footer_rule(ui: &mut Ui) {
    ui.add_space(Metrics::LG);
    hairline(ui);
    ui.add_space(Metrics::MD);
}

/// A row of dialog buttons with an `SM` gap between them.
pub(crate) fn dialog_buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Metrics::SM;
        add(ui);
    });
}

/// The question the delete-group dialog asks.
pub(crate) fn delete_group_message(group_name: &str, count: usize) -> String {
    format!(
        "Delete “{}”? Its {} sound{} stay in the library and become ungrouped.",
        group_name,
        count,
        if count == 1 { "" } else { "s" }
    )
}

/// The single-line name field used by the create/rename modals.
fn name_field(ui: &mut Ui, value: &mut String, hint: &str) -> egui::Response {
    let resp = ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .font(crate::gui::widgets::glyph_font(12.0))
            .margin(egui::Margin::symmetric(10.0, 8.0)),
    );
    resp
}

/// Red validation line under a field.
pub(crate) fn error_line(ui: &mut Ui, message: &str) {
    ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
        ui.add(
            egui::Label::new(
                RichText::new(message.to_uppercase())
                    .font(crate::gui::widgets::glyph_font(9.0))
                    .color(Palette::ROSE_400)
                    .extra_letter_spacing(0.8)
                    .line_height(Some(NOTE_LINE_H)),
            )
            .wrap(),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::gui::hotkeys_ui::test_support::*;

    /// Run `add` in a bare panel (egui's default spacing, no console theme).
    fn in_panel(width: f32, mut add: impl FnMut(&mut Ui)) {
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width + 40.0, 800.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_width(width);
                    add(ui);
                });
            });
        }
    }

    #[test]
    fn the_delete_message_counts_sounds() {
        assert_eq!(
            delete_group_message("Combat", 1),
            "Delete “Combat”? Its 1 sound stay in the library and become ungrouped."
        );
        assert!(delete_group_message("Combat", 0).contains("0 sounds"));
        assert!(delete_group_message("Combat", 7).contains("7 sounds"));
    }

    #[test]
    fn a_field_starts_a_full_sm_below_its_label() {
        in_panel(300.0, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let caption = ui.label(micro_label("Assign to"));
            let plain_gap = ui.cursor().top() - caption.rect.bottom();
            assert_eq!(plain_gap, 0.0);

            let top = ui.cursor().top();
            field_label(ui, "Group name");
            let used = ui.cursor().top() - top;
            let label_h = ui.fonts(|f| f.row_height(&crate::gui::theme::mono_font(Metrics::MICRO)));
            assert!(
                used >= label_h + Metrics::SM,
                "used {used}, label {label_h}"
            );
        });
    }

    #[test]
    fn the_footer_rule_separates_content_rule_and_buttons() {
        in_panel(300.0, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let before = ui.cursor().top();
            footer_rule(ui);
            let after = ui.cursor().top();
            assert!(
                after - before >= Metrics::LG + Metrics::MD + 1.0,
                "{before} -> {after}"
            );
        });
    }

    #[test]
    fn dialog_buttons_keep_a_gap_between_them() {
        in_panel(300.0, |ui| {
            let mut rects = Vec::new();
            dialog_buttons(ui, |ui| {
                rects.push(ui.add_sized([100.0, 30.0], egui::Button::new("A")).rect);
                rects.push(ui.add_sized([100.0, 30.0], egui::Button::new("B")).rect);
            });
            let gap = rects[1].left() - rects[0].right();
            assert!(gap >= Metrics::SM, "gap {gap}");
        });
    }

    #[test]
    fn long_hints_wrap_with_air_between_their_lines() {
        in_panel(120.0, |ui| {
            let before = ui.cursor().top();
            hint_line(
                ui,
                "First use downloads yt-dlp + ffmpeg once, then it just works",
            );
            let height = ui.cursor().top() - before;
            assert!(height >= 2.0 * NOTE_LINE_H, "height {height}");
        });
    }

    #[test]
    fn the_header_does_not_depend_on_the_callers_spacing() {
        let mut heights = Vec::new();
        for spacing in [0.0_f32, 8.0] {
            in_panel(300.0, |ui| {
                ui.spacing_mut().item_spacing.y = spacing;
                let before = ui.cursor().top();
                header(ui, "New Group", "Categories keep a board readable");
                heights.push(ui.cursor().top() - before);
            });
        }
        // The last frame of each run is the settled one; spacing after the
        // final label is the caller's business, so allow exactly that much.
        let last = heights.len() - 1;
        assert!((heights[last] - heights[last - 1]).abs() <= 8.0 + f32::EPSILON);
    }

    #[test]
    fn every_dialog_of_this_file_renders() {
        let mut config = Config::default();
        config.library.groups.push(crate::config::GroupConfig {
            id: "6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22".to_string(),
            name: "Nature".to_string(),
        });
        let mut h = harness(config, false);
        let group = h
            .app
            .library()
            .groups()
            .first()
            .map(|g| g.id)
            .expect("the configured group is loaded");

        type Open = Box<dyn Fn(&mut SoundSharedApp)>;
        let cases: Vec<(&str, Open)> = vec![
            ("NEW GROUP", Box::new(|a| a.open_new_group())),
            (
                "RENAME GROUP",
                Box::new(move |a| a.open_rename_group(group)),
            ),
            (
                "DELETE GROUP",
                Box::new(move |a| a.open_delete_group(group)),
            ),
            ("ADD FROM YOUTUBE", Box::new(|a| a.open_add_youtube())),
            (
                "MOVE SOUND",
                Box::new(|a| {
                    a.open_assign_sound(crate::audio::engine::SoundId(uuid::Uuid::new_v4()), "Rain")
                }),
            ),
        ];
        for (title, open) in cases {
            open(&mut h.app);
            let text = settle(&h.ctx, 2, |ctx| show_dialogs(ctx, &mut h.app));
            assert!(text.contains(title), "{title}: {text}");
            h.app.close_dialog();
        }
    }
}
