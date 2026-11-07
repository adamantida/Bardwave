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
    let escaped = ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !state.youtube_busy();
    if escaped {
        state.close_dialog();
        return;
    }

    match dialog {
        Dialog::None => {}

        Dialog::NewGroup { name, error } => {
            let mut field = name;
            let mut submit = false;
            let mut cancel = false;

            modal(ctx, |ui| {
                header(ui, "New Group", "Categories keep a board readable");
                ui.add_space(Metrics::MD);

                ui.label(micro_label("Group name"));
                ui.add_space(Metrics::XS);
                let resp = name_field(ui, &mut field, "e.g. Combat");
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(Metrics::XS);
                match &error {
                    Some(message) => error_line(ui, message),
                    None => {
                        ui.label(micro_label("Up to 32 characters"));
                    }
                }

                ui.add_space(Metrics::LG);
                hairline(ui);
                ui.add_space(Metrics::MD);

                ui.horizontal(|ui| {
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
                    ui.add_space(Metrics::SM);
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
                ui.add_space(Metrics::MD);

                ui.label(micro_label("Group name"));
                ui.add_space(Metrics::XS);
                let resp = name_field(ui, &mut field, "Group name");
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(Metrics::XS);
                match &error {
                    Some(message) => error_line(ui, message),
                    None => {
                        ui.label(micro_label("Up to 32 characters"));
                    }
                }

                ui.add_space(Metrics::LG);
                hairline(ui);
                ui.add_space(Metrics::MD);

                ui.horizontal(|ui| {
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
                    ui.add_space(Metrics::SM);
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
                ui.add_space(Metrics::MD);

                ui.label(
                    RichText::new(format!(
                        "Delete “{}”? Its {} sound{} stay in the library and become ungrouped.",
                        group_name,
                        count,
                        if count == 1 { "" } else { "s" }
                    ))
                    .font(crate::gui::widgets::glyph_font(11.0))
                    .color(Palette::ZINC_300),
                );

                ui.add_space(Metrics::LG);
                hairline(ui);
                ui.add_space(Metrics::MD);

                ui.horizontal(|ui| {
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
                    ui.add_space(Metrics::SM);
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
                ui.add_space(Metrics::MD);

                ui.label(micro_label("Assign to"));
                ui.add_space(Metrics::SM);

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
                            ui.label(micro_label("No groups yet"));
                        }
                    });

                ui.add_space(Metrics::MD);
                hairline(ui);
                ui.add_space(Metrics::MD);

                ui.horizontal(|ui| {
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
                ui.add_space(Metrics::MD);

                ui.label(micro_label("Video or music link"));
                ui.add_space(Metrics::XS);
                let resp = name_field(ui, &mut field, "https://youtube.com/watch?v=…");
                if !busy && resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(Metrics::XS);
                if let Some(message) = &error {
                    error_line(ui, message);
                } else if busy {
                    ui.label(micro_label(&status));
                } else {
                    ui.label(micro_label(
                        "First use downloads yt-dlp + ffmpeg once (~230 MB), then it just works",
                    ));
                }

                ui.add_space(Metrics::LG);
                hairline(ui);
                ui.add_space(Metrics::MD);

                ui.horizontal(|ui| {
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
                    if !busy {
                        ui.add_space(Metrics::SM);
                        if ConsoleButton::ghost("Cancel")
                            .glyph(glyph::CROSS)
                            .width(120.0)
                            .height(Metrics::BTN_H)
                            .id_salt("dlg_yt_cancel2")
                            .show(ui)
                            .clicked()
                        {
                            cancel = true;
                        }
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
fn modal(ctx: &Context, add: impl FnOnce(&mut Ui)) {
    egui::Window::new("soundshared_modal")
        .id(egui::Id::new("soundshared_modal"))
        .order(egui::Order::Foreground)
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .frame(crate::gui::theme::panel_frame())
        .show(ctx, |ui| {
            ui.set_width(MODAL_W);
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
fn header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(crate::gui::theme::micro_label_accent("Confirm"));
    ui.add_space(2.0);
    ui.label(panel_title(title));
    ui.add_space(2.0);
    ui.label(micro_label(subtitle));
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
fn error_line(ui: &mut Ui, message: &str) {
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.label(
            RichText::new(message.to_uppercase())
                .font(crate::gui::widgets::glyph_font(9.0))
                .color(Palette::ROSE_400)
                .extra_letter_spacing(0.8),
        );
    });
}
