//! The modal dialogs of the library UI: add folder, replace path prefix, export
//! and import bundles, edit tags.
//!
//! A dialog only draws itself and records what it wants as [`Act`]s; the app
//! carries them out afterwards (`SoundSharedApp::run_acts`), so no dialog code
//! holds a borrow of the library while it is being changed.

use std::collections::HashSet;
use std::path::PathBuf;

use egui::{Color32, Context, RichText, Ui, Vec2};

use crate::gui::app::SoundSharedApp;
use crate::gui::groups::Library;
use crate::gui::theme::{
    alpha, hairline, micro_label, micro_label_accent, panel_title, Metrics, Palette,
};
use crate::gui::widgets::{elide, glyph, glyph_font, paint_icon, ConsoleButton};
use crate::library_bundle::{BundleProgress, ConflictPolicy};
use crate::library_query::LibraryQuery;

use super::logic::{
    complete_tag_input, elide_start, format_bytes, import_report_lines, policy_help, policy_label,
    scan_progress_line, skipped_lines, suggest_tags,
};
use super::ops::{refresh_remap_preview, Act};
use super::state::{
    AddFolder, AddFolderPhase, Export, ExportPhase, Import, ImportPhase, ImportPreview, LibDialog,
    Remap, TagEdit,
};

/// Modal width of the wide dialogs.
const WIDE: f32 = 500.0;
/// Modal width of the tag editor.
const NARROW: f32 = 420.0;

/// Draw the open dialog (if any) and carry out what it asked for. Safe to call
/// every frame.
pub fn show(ctx: &Context, state: &mut SoundSharedApp) {
    let mut dialog = std::mem::take(&mut state.lib_ui.dialog);
    if matches!(dialog, LibDialog::None) {
        return;
    }
    paint_scrim(ctx);
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));

    let mut acts: Vec<Act> = Vec::new();
    {
        let library = &state.library;
        let missing = &state.lib_ui.missing;
        match &mut dialog {
            LibDialog::None => {}
            LibDialog::AddFolder(d) => add_folder(ctx, d, &mut acts, escape),
            LibDialog::Remap(d) => remap(ctx, d, library, missing, &mut acts, escape),
            LibDialog::Export(d) => export(ctx, d, library, missing, &mut acts, escape),
            LibDialog::Import(d) => import(ctx, d, &mut acts, escape),
            LibDialog::Tags(d) => tags(ctx, d, library, &mut acts, escape),
        }
    }
    if state.run_acts(acts, &mut dialog) {
        state.lib_ui.dialog = dialog;
    }
}

// ---------------------------------------------------------------- chrome ---

/// Dim the console behind a dialog and swallow clicks on it, so the library
/// cannot be edited while a bundle is being imported.
fn paint_scrim(ctx: &Context) {
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("library_scrim"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, egui::Rounding::ZERO, alpha(Palette::BLACK, 170));
        });
}

fn modal(ctx: &Context, width: f32, add: impl FnOnce(&mut Ui)) {
    egui::Window::new("library_modal")
        .id(egui::Id::new("library_modal"))
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

fn header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(micro_label_accent("Library"));
    ui.add_space(2.0);
    ui.label(panel_title(title));
    ui.add_space(2.0);
    ui.label(micro_label(subtitle));
    ui.add_space(Metrics::MD);
}

fn body(ui: &mut Ui, text: impl Into<String>) {
    ui.label(
        RichText::new(text.into())
            .font(glyph_font(11.0))
            .color(Palette::ZINC_300),
    );
}

fn dim(ui: &mut Ui, text: impl Into<String>) {
    ui.label(
        RichText::new(text.into())
            .font(glyph_font(10.0))
            .color(Palette::ZINC_500),
    );
}

fn problem(ui: &mut Ui, text: impl Into<String>, safety: bool) {
    let color = if safety {
        Palette::AMBER_400
    } else {
        Palette::ROSE_400
    };
    egui::Frame::none()
        .fill(alpha(color, 14))
        .stroke(egui::Stroke::new(1.0_f32, alpha(color, 80)))
        .rounding(egui::Rounding::same(Metrics::R_SM))
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text.into())
                    .font(glyph_font(11.0))
                    .color(color),
            );
        });
}

fn label(ui: &mut Ui, text: &str) {
    ui.label(micro_label(text));
    ui.add_space(Metrics::XS);
}

fn text_field(ui: &mut Ui, value: &mut String, hint: &str) -> egui::Response {
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .font(glyph_font(12.0))
            .margin(egui::Margin::symmetric(10.0, 8.0)),
    )
}

fn check(ui: &mut Ui, value: &mut bool, text: &str) -> egui::Response {
    ui.checkbox(
        value,
        RichText::new(text)
            .font(glyph_font(11.0))
            .color(Palette::ZINC_300),
    )
}

fn footer(ui: &mut Ui) {
    ui.add_space(Metrics::LG);
    hairline(ui);
    ui.add_space(Metrics::MD);
}

fn button(ui: &mut Ui, primary: bool, text: &str, salt: &str, width: f32, enabled: bool) -> bool {
    let b = if primary {
        ConsoleButton::primary(text)
    } else {
        ConsoleButton::ghost(text)
    };
    b.width(width)
        .height(Metrics::BTN_H)
        .enabled(enabled)
        .id_salt(salt)
        .show(ui)
        .clicked()
}

fn scroll_list(ui: &mut Ui, id: &str, max_height: f32, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(max_height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            add(ui);
        });
}

fn progress(ui: &mut Ui, progress: &Option<BundleProgress>) {
    match progress {
        Some(p) => {
            let fraction = if p.total == 0 {
                0.0
            } else {
                p.done as f32 / p.total as f32
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(ui.available_width())
                    .text(format!("{} / {} files", p.done, p.total)),
            );
            ui.add_space(Metrics::XS);
            dim(ui, elide_start(&p.current, 64));
        }
        None => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                body(ui, "Starting…");
            });
        }
    }
}

// ------------------------------------------------------------ add folder ---

fn add_folder(ctx: &Context, d: &mut AddFolder, acts: &mut Vec<Act>, escape: bool) {
    let AddFolder {
        auto_groups, phase, ..
    } = d;
    let mut close = escape;
    let mut apply = false;
    modal(ctx, WIDE, |ui| {
        header(
            ui,
            "Add Folder",
            "Every audio file below the folder is added; duplicates are skipped",
        );
        match phase {
            AddFolderPhase::Picking => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    body(ui, "Waiting for the folder chooser…");
                });
                footer(ui);
                close |= button(ui, false, "Cancel", "lib_af_cancel_pick", 120.0, true);
            }
            AddFolderPhase::Scanning { root, stats, .. } => {
                dim(ui, elide_start(&root.to_string_lossy(), 70));
                ui.add_space(Metrics::SM);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    body(ui, scan_progress_line(stats));
                });
                footer(ui);
                close |= button(ui, false, "Cancel", "lib_af_cancel_scan", 120.0, true);
            }
            AddFolderPhase::Ready { report, summary } => {
                dim(ui, elide_start(&report.root.to_string_lossy(), 70));
                ui.add_space(Metrics::SM);
                if summary.new_sounds == 0 {
                    body(ui, "No new sounds were found in this folder.");
                } else {
                    body(
                        ui,
                        format!(
                            "{} new sound{} found",
                            summary.new_sounds,
                            if summary.new_sounds == 1 { "" } else { "s" }
                        ),
                    );
                }
                if report.cancelled {
                    problem(
                        ui,
                        "The scan was stopped early; this is what it found.",
                        true,
                    );
                }
                let skipped = skipped_lines(summary);
                if !skipped.is_empty() {
                    ui.add_space(Metrics::XS);
                    for line in &skipped {
                        dim(ui, line.clone());
                    }
                }
                if !report.errors.is_empty() {
                    ui.add_space(Metrics::XS);
                    for issue in report.errors.iter().take(3) {
                        dim(
                            ui,
                            format!(
                                "{}: {}",
                                elide_start(&issue.path.to_string_lossy(), 40),
                                issue.message
                            ),
                        );
                    }
                }
                if !summary.folders.is_empty() {
                    ui.add_space(Metrics::MD);
                    label(ui, "Sub-folders");
                    scroll_list(ui, "lib_af_folders", 110.0, |ui| {
                        if summary.top_level > 0 {
                            dim(ui, format!("(this folder)  {}", summary.top_level));
                        }
                        for (name, count) in &summary.folders {
                            dim(ui, format!("{}  {count}", elide(name, 40)));
                        }
                    });
                    ui.add_space(Metrics::SM);
                    check(ui, auto_groups, "Create a group for each sub-folder");
                    if *auto_groups {
                        let mut line = String::new();
                        if !summary.new_groups.is_empty() {
                            let names: Vec<String> = summary
                                .new_groups
                                .iter()
                                .take(4)
                                .map(|n| elide(n, 18))
                                .collect();
                            line.push_str(&format!(
                                "{} new group{}: {}{}",
                                summary.new_groups.len(),
                                if summary.new_groups.len() == 1 {
                                    ""
                                } else {
                                    "s"
                                },
                                names.join(", "),
                                if summary.new_groups.len() > 4 {
                                    ", …"
                                } else {
                                    ""
                                }
                            ));
                        }
                        if summary.reused_groups > 0 {
                            if !line.is_empty() {
                                line.push_str("; ");
                            }
                            line.push_str(&format!(
                                "{} existing group{} reused",
                                summary.reused_groups,
                                if summary.reused_groups == 1 { "" } else { "s" }
                            ));
                        }
                        if !line.is_empty() {
                            dim(ui, line);
                        }
                    }
                }
                footer(ui);
                ui.horizontal(|ui| {
                    let label = match summary.new_sounds {
                        1 => "Add 1 Sound".to_string(),
                        n => format!("Add {n} Sounds"),
                    };
                    apply |= button(
                        ui,
                        true,
                        &label,
                        "lib_af_apply",
                        190.0,
                        summary.new_sounds > 0,
                    );
                    ui.add_space(Metrics::SM);
                    close |= button(ui, false, "Cancel", "lib_af_cancel", 120.0, true);
                });
            }
            AddFolderPhase::Failed(message) => {
                problem(ui, message.clone(), false);
                footer(ui);
                close |= button(ui, false, "Close", "lib_af_close", 120.0, true);
            }
        }
    });
    if apply {
        acts.push(Act::ApplyScan);
    } else if close {
        acts.push(Act::Close);
    }
}

// ---------------------------------------------------------------- remap ----

fn remap(
    ctx: &Context,
    d: &mut Remap,
    library: &Library,
    missing: &HashSet<PathBuf>,
    acts: &mut Vec<Act>,
    escape: bool,
) {
    refresh_remap_preview(d, library, missing);
    let mut close = escape;
    let mut apply = false;
    let mut browse = false;
    let count = d.preview.as_ref().map_or(0, |(_, _, p)| p.count);
    modal(ctx, WIDE, |ui| {
        header(
            ui,
            "Replace Path Prefix",
            "Use it when the whole sound folder moved to another place",
        );
        label(ui, "Old start of the path");
        text_field(ui, &mut d.old, "/Volumes/Old/Sounds");
        ui.add_space(Metrics::SM);
        label(ui, "New start of the path");
        ui.horizontal(|ui| {
            let field_w = ui.available_width() - 110.0 - Metrics::SM;
            ui.add(
                egui::TextEdit::singleline(&mut d.new)
                    .hint_text("/Users/me/Sounds")
                    .desired_width(field_w)
                    .font(glyph_font(12.0))
                    .margin(egui::Margin::symmetric(10.0, 8.0)),
            );
            browse |= button(ui, false, "Browse…", "lib_rm_browse", 110.0, true);
        });
        ui.add_space(Metrics::MD);
        match &d.preview {
            _ if d.old.trim().is_empty() => dim(ui, "Type the part of the path that changed."),
            Some((_, _, p)) if p.count == 0 => {
                problem(ui, "No sound path starts with that prefix.", true);
            }
            Some((_, _, p)) => {
                body(
                    ui,
                    format!(
                        "{} path{} will change",
                        p.count,
                        if p.count == 1 { "" } else { "s" }
                    ),
                );
                if p.missing_hits > 0 {
                    dim(
                        ui,
                        format!("{} of them are not found right now", p.missing_hits),
                    );
                }
                ui.add_space(Metrics::XS);
                for (old, new) in &p.samples {
                    dim(
                        ui,
                        format!("{}  ->  {}", elide_start(old, 36), elide_start(new, 36)),
                    );
                }
            }
            None => {}
        }
        footer(ui);
        ui.horizontal(|ui| {
            apply |= button(ui, true, "Replace", "lib_rm_apply", 160.0, count > 0);
            ui.add_space(Metrics::SM);
            close |= button(ui, false, "Cancel", "lib_rm_cancel", 120.0, true);
        });
    });
    if browse {
        acts.push(Act::RemapBrowse);
    }
    if apply {
        acts.push(Act::RemapApply);
    } else if close {
        acts.push(Act::Close);
    }
}

// --------------------------------------------------------------- export ----

fn export(
    ctx: &Context,
    d: &mut Export,
    library: &Library,
    missing: &HashSet<PathBuf>,
    acts: &mut Vec<Act>,
    escape: bool,
) {
    let Export {
        name,
        include_audio,
        phase,
        ..
    } = d;
    // A running export can only be stopped with its own button.
    let mut close = escape && !matches!(phase, ExportPhase::Running { .. });
    let mut choose = false;
    let mut cancel_run = false;
    let mut back = false;
    modal(ctx, WIDE, |ui| {
        header(
            ui,
            "Export Bundle",
            "One .zip with groups, tags, volumes, scenes and playlists",
        );
        match phase {
            ExportPhase::Idle => {
                label(ui, "Bundle name");
                text_field(ui, name, "Bardwave set");
                ui.add_space(Metrics::SM);
                check(ui, include_audio, "Include the audio files");
                ui.add_space(Metrics::XS);
                dim(
                    ui,
                    if *include_audio {
                        "The bundle is self-contained but can be large."
                    } else {
                        "Small bundle; whoever imports it must locate the sound files."
                    },
                );
                ui.add_space(Metrics::MD);
                dim(
                    ui,
                    format!(
                        "{} sounds, {} groups, {} scenes, {} playlists",
                        library.sounds().len(),
                        library.groups().len(),
                        library.scenes().len(),
                        library.playlists().len()
                    ),
                );
                if *include_audio {
                    let gone = library
                        .sounds()
                        .iter()
                        .filter(|s| missing.contains(&s.path))
                        .count();
                    if gone > 0 {
                        ui.add_space(Metrics::XS);
                        problem(
                            ui,
                            format!(
                                "{gone} sound file{} not found; {} exported without audio.",
                                if gone == 1 { " is" } else { "s are" },
                                if gone == 1 { "it is" } else { "they are" }
                            ),
                            true,
                        );
                    }
                }
                footer(ui);
                ui.horizontal(|ui| {
                    choose |= button(
                        ui,
                        true,
                        "Choose File & Export…",
                        "lib_ex_choose",
                        240.0,
                        !library.sounds().is_empty(),
                    );
                    ui.add_space(Metrics::SM);
                    close |= button(ui, false, "Cancel", "lib_ex_cancel", 120.0, true);
                });
            }
            ExportPhase::Picking => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    body(ui, "Waiting for the save dialog…");
                });
                footer(ui);
                close |= button(ui, false, "Cancel", "lib_ex_cancel_pick", 120.0, true);
            }
            ExportPhase::Running {
                dest, progress: p, ..
            } => {
                dim(ui, elide_start(&dest.to_string_lossy(), 70));
                ui.add_space(Metrics::SM);
                progress(ui, p);
                footer(ui);
                cancel_run |= button(ui, false, "Cancel Export", "lib_ex_cancel_run", 160.0, true);
            }
            ExportPhase::Done { dest, report } => {
                body(
                    ui,
                    format!(
                        "Exported {} sound{} with {} audio file{} ({}).",
                        report.sounds,
                        if report.sounds == 1 { "" } else { "s" },
                        report.audio_files,
                        if report.audio_files == 1 { "" } else { "s" },
                        format_bytes(report.audio_bytes)
                    ),
                );
                dim(ui, elide_start(&dest.to_string_lossy(), 70));
                if !report.missing_audio.is_empty() {
                    ui.add_space(Metrics::SM);
                    problem(
                        ui,
                        format!(
                            "{} sound file{} could not be found and went in without audio: {}",
                            report.missing_audio.len(),
                            if report.missing_audio.len() == 1 {
                                ""
                            } else {
                                "s"
                            },
                            report
                                .missing_audio
                                .iter()
                                .take(3)
                                .map(|p| {
                                    p.file_name()
                                        .map(|f| f.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        true,
                    );
                }
                footer(ui);
                close |= button(ui, true, "Done", "lib_ex_done", 120.0, true);
            }
            ExportPhase::Failed { message, safety } => {
                problem(ui, message.clone(), *safety);
                footer(ui);
                ui.horizontal(|ui| {
                    back |= button(ui, true, "Back", "lib_ex_back", 120.0, true);
                    ui.add_space(Metrics::SM);
                    close |= button(ui, false, "Close", "lib_ex_close", 120.0, true);
                });
            }
        }
    });
    if back {
        *phase = ExportPhase::Idle;
    }
    if choose {
        acts.push(Act::ExportChoose);
    }
    if cancel_run {
        acts.push(Act::ExportCancel);
    }
    if close {
        acts.push(Act::Close);
    }
}

// --------------------------------------------------------------- import ----

fn import(ctx: &Context, d: &mut Import, acts: &mut Vec<Act>, escape: bool) {
    let mut close = escape && !matches!(d.phase, ImportPhase::Running { .. });
    let mut start = false;
    let mut cancel_run = false;
    let mut another = false;
    let mut browse = false;
    let mut refresh = false;
    modal(ctx, WIDE, |ui| {
        header(
            ui,
            "Import Bundle",
            "Merge a shared sound set into your library",
        );
        match &d.phase {
            ImportPhase::Picking => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    body(ui, "Waiting for the file chooser…");
                });
                footer(ui);
                close |= button(ui, false, "Cancel", "lib_im_cancel_pick", 120.0, true);
            }
            ImportPhase::Inspecting => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    body(ui, "Checking the bundle…");
                });
                footer(ui);
                close |= button(ui, false, "Cancel", "lib_im_cancel_inspect", 120.0, true);
            }
            ImportPhase::Failed { message, safety } => {
                problem(ui, message.clone(), *safety);
                footer(ui);
                ui.horizontal(|ui| {
                    another |= button(ui, true, "Choose Another…", "lib_im_another", 190.0, true);
                    ui.add_space(Metrics::SM);
                    close |= button(ui, false, "Close", "lib_im_close", 120.0, true);
                });
            }
            ImportPhase::Running { progress: p, .. } => {
                progress(ui, p);
                footer(ui);
                cancel_run |= button(ui, false, "Cancel Import", "lib_im_cancel_run", 160.0, true);
            }
            ImportPhase::Done(report) => {
                body(ui, "Bundle imported.");
                ui.add_space(Metrics::XS);
                for line in import_report_lines(report) {
                    dim(ui, line);
                }
                for warning in report.warnings.iter().take(4) {
                    problem(ui, warning.clone(), true);
                }
                footer(ui);
                close |= button(ui, true, "Done", "lib_im_done", 120.0, true);
            }
            ImportPhase::Ready => {
                ready_section(ui, d, &mut browse, &mut refresh);
                let can_import = !d.target.trim().is_empty()
                    && matches!(
                        d.preview,
                        ImportPreview::Ready(_) | ImportPreview::Computing
                    );
                footer(ui);
                ui.horizontal(|ui| {
                    start |= button(ui, true, "Import", "lib_im_start", 160.0, can_import);
                    ui.add_space(Metrics::SM);
                    another |= button(ui, false, "Choose Another…", "lib_im_another2", 190.0, true);
                    ui.add_space(Metrics::SM);
                    close |= button(ui, false, "Cancel", "lib_im_cancel", 110.0, true);
                });
            }
        }
    });
    if another {
        acts.push(Act::ImportPickAnother);
    }
    if browse {
        acts.push(Act::ImportBrowse);
    }
    if refresh {
        acts.push(Act::ImportRefresh);
    }
    if start {
        acts.push(Act::ImportStart);
    }
    if cancel_run {
        acts.push(Act::ImportCancel);
    }
    if close {
        acts.push(Act::Close);
    }
}

/// Bundle summary, conflict policy, target folder and the dry-run preview.
fn ready_section(ui: &mut Ui, d: &mut Import, browse: &mut bool, refresh: &mut bool) {
    if let Some(info) = &d.info {
        let title = if info.name.trim().is_empty() {
            d.zip
                .as_ref()
                .and_then(|z| z.file_name())
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            info.name.clone()
        };
        body(ui, elide(&title, 56));
        let mut counts = format!(
            "{} sounds, {} groups, {} scenes, {} playlists",
            info.sounds, info.groups, info.scenes, info.playlists
        );
        if !info.app_version.is_empty() {
            counts.push_str(&format!(" - made with Bardwave {}", info.app_version));
        }
        dim(ui, counts);
        if info.audio_files > 0 {
            dim(
                ui,
                format!(
                    "{} audio file{} inside ({})",
                    info.audio_files,
                    if info.audio_files == 1 { "" } else { "s" },
                    format_bytes(info.audio_bytes)
                ),
            );
        } else {
            dim(
                ui,
                "No audio inside: the sounds show as \"file not found\" until you locate them.",
            );
        }
    }
    ui.add_space(Metrics::MD);

    label(ui, "If something already exists");
    let before = d.policy;
    ui.horizontal_wrapped(|ui| {
        for policy in [
            ConflictPolicy::Skip,
            ConflictPolicy::Rename,
            ConflictPolicy::Replace,
        ] {
            ui.radio_value(
                &mut d.policy,
                policy,
                RichText::new(policy_label(policy))
                    .font(glyph_font(11.0))
                    .color(Palette::ZINC_300),
            );
        }
    });
    dim(ui, policy_help(d.policy));
    if d.policy != before {
        *refresh = true;
    }
    ui.add_space(Metrics::MD);

    label(ui, "Sound files go to");
    ui.horizontal(|ui| {
        let field_w = ui.available_width() - 110.0 - Metrics::SM;
        let resp = ui.add(
            egui::TextEdit::singleline(&mut d.target)
                .hint_text("Folder for the imported sounds")
                .desired_width(field_w)
                .font(glyph_font(12.0))
                .margin(egui::Margin::symmetric(10.0, 8.0)),
        );
        if resp.changed() {
            d.preview = ImportPreview::None;
        }
        if resp.lost_focus() {
            *refresh = true;
        }
        *browse |= button(ui, false, "Browse…", "lib_im_browse", 110.0, true);
    });
    ui.add_space(Metrics::MD);

    match &d.preview {
        ImportPreview::None => {
            if !d.target.trim().is_empty() {
                dim(ui, "The preview updates when you finish typing.");
            }
        }
        ImportPreview::Computing => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                dim(ui, "Working out what would change…");
            });
        }
        ImportPreview::Ready(report) => {
            label(ui, "This import would");
            for line in import_report_lines(report) {
                body(ui, line);
            }
            for warning in report.warnings.iter().take(3) {
                dim(ui, warning.clone());
            }
        }
        ImportPreview::Failed { message, safety } => problem(ui, message.clone(), *safety),
    }
}

// ----------------------------------------------------------------- tags ----

fn tags(ctx: &Context, d: &mut TagEdit, library: &Library, acts: &mut Vec<Act>, escape: bool) {
    let Some(sound) = library.sound(&d.path) else {
        acts.push(Act::Close);
        return;
    };
    let name = super::logic::display_name(&sound.name, &sound.path);
    let have = sound.tags.clone();
    let known = library.tag_counts(&LibraryQuery::new());
    let suggestions = suggest_tags(&d.input, &known, &have, 8);
    let mut close = escape;
    let mut add: Option<String> = None;
    let mut remove: Option<String> = None;
    modal(ctx, NARROW, |ui| {
        header(ui, "Edit Tags", &elide(&name, 40));
        label(ui, "Tags on this sound");
        if have.is_empty() {
            dim(ui, "No tags yet.");
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                for tag in &have {
                    if removable_chip(ui, tag) {
                        remove = Some(tag.clone());
                    }
                }
            });
        }
        ui.add_space(Metrics::MD);

        label(ui, "Add tags (separate with commas)");
        let id = egui::Id::new("library_tag_input");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut d.input)
                .id(id)
                .hint_text("tavern, loud")
                .desired_width(f32::INFINITY)
                .font(glyph_font(12.0))
                .margin(egui::Margin::symmetric(10.0, 8.0)),
        );
        if d.focus {
            resp.request_focus();
            d.focus = false;
        }
        if resp.changed() {
            d.error = None;
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            add = Some(d.input.clone());
        }
        if let Some(error) = &d.error {
            ui.add_space(Metrics::XS);
            ui.label(
                RichText::new(error.to_uppercase())
                    .font(glyph_font(9.0))
                    .color(Palette::ROSE_400)
                    .extra_letter_spacing(0.8),
            );
        }
        if !suggestions.is_empty() {
            ui.add_space(Metrics::SM);
            label(ui, "Already used");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                for tag in &suggestions {
                    if suggestion_chip(ui, tag) {
                        add = Some(complete_tag_input(&d.input, tag));
                    }
                }
            });
        }
        footer(ui);
        ui.horizontal(|ui| {
            if button(
                ui,
                true,
                "Add",
                "lib_tag_add",
                120.0,
                !d.input.trim().is_empty(),
            ) {
                add = Some(d.input.clone());
            }
            ui.add_space(Metrics::SM);
            close |= button(ui, false, "Done", "lib_tag_done", 120.0, true);
        });
    });
    if let Some(raw) = add {
        acts.push(Act::TagAdd(raw));
    }
    if let Some(tag) = remove {
        acts.push(Act::TagRemove(tag));
    }
    if close {
        acts.push(Act::Close);
    }
}

/// A tag chip with an `x`; true when clicked (remove).
fn removable_chip(ui: &mut Ui, tag: &str) -> bool {
    let galley = ui
        .painter()
        .layout_no_wrap(tag.to_string(), glyph_font(11.0), Palette::ROSE_400);
    let size = Vec2::new(galley.size().x + 36.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Remove this tag");
    let painter = ui.painter();
    painter.rect(
        rect,
        egui::Rounding::same(Metrics::R_SM),
        alpha(Palette::ROSE_950, 190),
        egui::Stroke::new(1.0_f32, alpha(Palette::ROSE_500, 160)),
    );
    painter.galley(
        egui::pos2(rect.left() + 9.0, rect.center().y - galley.size().y / 2.0),
        galley,
        Palette::ROSE_400,
    );
    paint_icon(
        painter,
        glyph::CROSS,
        egui::pos2(rect.right() - 12.0, rect.center().y),
        11.0,
        if resp.hovered() {
            Palette::WHITE
        } else {
            Palette::ROSE_400
        },
    );
    resp.clicked()
}

/// An existing tag offered for a click; true when clicked (add).
fn suggestion_chip(ui: &mut Ui, tag: &str) -> bool {
    let color: Color32 = Palette::ZINC_300;
    let galley = ui
        .painter()
        .layout_no_wrap(format!("+ {tag}"), glyph_font(11.0), color);
    let size = Vec2::new(galley.size().x + 18.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    painter.rect(
        rect,
        egui::Rounding::same(Metrics::R_SM),
        if resp.hovered() {
            Palette::ROW
        } else {
            Palette::TRANSPARENT
        },
        egui::Stroke::new(
            1.0_f32,
            if resp.hovered() {
                Palette::BORDER_STRONG
            } else {
                Palette::BORDER
            },
        ),
    );
    painter.galley(
        egui::pos2(rect.left() + 9.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library_bundle::{BundleInfo, ExportReport};
    use crate::library_scan::{ScanReport, ScannedFile};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Run two frames of `add` in a headless egui context.
    fn frames(mut add: impl FnMut(&Context)) {
        let ctx = Context::default();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| add(ctx));
        }
    }

    fn library() -> Library {
        let mut lib = Library::new();
        lib.insert_sound(std::path::Path::new("/s/a.wav"), "a", 1.0, None);
        lib.add_tag(std::path::Path::new("/s/a.wav"), "storm");
        lib
    }

    #[test]
    fn add_folder_phases_draw_and_escape_closes() {
        let lib = library();
        let report = ScanReport {
            root: PathBuf::from("/s"),
            files: vec![ScannedFile {
                path: PathBuf::from("/s/Tavern/x.wav"),
                size: 5,
                folder: Some("Tavern".into()),
            }],
            ..ScanReport::default()
        };
        let summary = super::super::logic::summarize_scan(&report, &lib);
        let mut d = AddFolder {
            token: 1,
            auto_groups: true,
            phase: AddFolderPhase::Ready { report, summary },
        };
        let mut acts = Vec::new();
        frames(|ctx| add_folder(ctx, &mut d, &mut acts, false));
        assert!(acts.is_empty());
        frames(|ctx| add_folder(ctx, &mut d, &mut acts, true));
        assert!(acts.contains(&Act::Close));

        for phase in [
            AddFolderPhase::Picking,
            AddFolderPhase::Failed("boom".into()),
        ] {
            d.phase = phase;
            frames(|ctx| add_folder(ctx, &mut d, &mut Vec::new(), false));
        }
    }

    #[test]
    fn remap_dialog_previews_and_blocks_empty_apply() {
        let lib = library();
        let missing: HashSet<PathBuf> = [PathBuf::from("/s/a.wav")].into_iter().collect();
        let mut d = Remap {
            token: 1,
            old: "/s".into(),
            new: "/t".into(),
            preview: None,
        };
        frames(|ctx| remap(ctx, &mut d, &lib, &missing, &mut Vec::new(), false));
        let (_, _, preview) = d.preview.clone().unwrap();
        assert_eq!(preview.count, 1);
        assert_eq!(preview.missing_hits, 1);
        d.old = "/nope".into();
        frames(|ctx| remap(ctx, &mut d, &lib, &missing, &mut Vec::new(), false));
        assert_eq!(d.preview.as_ref().unwrap().2.count, 0);
    }

    #[test]
    fn export_dialog_draws_every_phase() {
        let lib = library();
        let missing: HashSet<PathBuf> = [PathBuf::from("/s/a.wav")].into_iter().collect();
        let mut d = Export {
            token: 1,
            name: "Set".into(),
            include_audio: true,
            phase: ExportPhase::Idle,
        };
        let phases = vec![
            ExportPhase::Idle,
            ExportPhase::Picking,
            ExportPhase::Running {
                dest: PathBuf::from("/x.zip"),
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Some(BundleProgress {
                    done: 1,
                    total: 4,
                    current: "Tavern/a.ogg".into(),
                }),
            },
            ExportPhase::Done {
                dest: PathBuf::from("/x.zip"),
                report: ExportReport {
                    missing_audio: vec![PathBuf::from("/s/a.wav")],
                    ..ExportReport::default()
                },
            },
            ExportPhase::Failed {
                message: "no".into(),
                safety: false,
            },
        ];
        for phase in phases {
            d.phase = phase;
            let mut acts = Vec::new();
            frames(|ctx| export(ctx, &mut d, &lib, &missing, &mut acts, false));
        }
        // Escape does not abort a running export.
        d.phase = ExportPhase::Running {
            dest: PathBuf::from("/x.zip"),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
        };
        let mut acts = Vec::new();
        frames(|ctx| export(ctx, &mut d, &lib, &missing, &mut acts, true));
        assert!(!acts.contains(&Act::Close));
    }

    #[test]
    fn import_dialog_draws_every_phase() {
        let mut d = Import {
            token: 1,
            zip: Some(PathBuf::from("/x/pack.zip")),
            info: Some(BundleInfo {
                name: "Pack".into(),
                sounds: 2,
                audio_files: 2,
                audio_bytes: 4096,
                ..BundleInfo::default()
            }),
            policy: ConflictPolicy::Rename,
            target: "/lib/imported/Pack".into(),
            preview_seq: 1,
            preview_key: None,
            preview: ImportPreview::Ready(Box::default()),
            phase: ImportPhase::Ready,
        };
        let phases = vec![
            ImportPhase::Picking,
            ImportPhase::Inspecting,
            ImportPhase::Ready,
            ImportPhase::Running {
                cancel: Arc::new(AtomicBool::new(false)),
                progress: None,
            },
            ImportPhase::Done(Box::default()),
            ImportPhase::Failed {
                message: "Import blocked for safety".into(),
                safety: true,
            },
        ];
        for phase in phases {
            d.phase = phase;
            frames(|ctx| import(ctx, &mut d, &mut Vec::new(), false));
        }
        d.phase = ImportPhase::Ready;
        for preview in [
            ImportPreview::None,
            ImportPreview::Computing,
            ImportPreview::Failed {
                message: "bad".into(),
                safety: true,
            },
        ] {
            d.preview = preview;
            frames(|ctx| import(ctx, &mut d, &mut Vec::new(), false));
        }
    }

    #[test]
    fn tag_dialog_closes_when_the_sound_is_gone() {
        let lib = library();
        let mut d = TagEdit {
            path: PathBuf::from("/s/a.wav"),
            input: "st".into(),
            error: Some("x".into()),
            focus: true,
        };
        let mut acts = Vec::new();
        frames(|ctx| tags(ctx, &mut d, &lib, &mut acts, false));
        assert!(acts.is_empty());
        d.path = PathBuf::from("/s/zzz.wav");
        frames(|ctx| tags(ctx, &mut d, &lib, &mut acts, false));
        assert!(acts.contains(&Act::Close));
    }
}
