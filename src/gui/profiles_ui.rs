//! Profile menu in the top bar and its dialogs (switch, new, manage, rename,
//! delete). The file-system work lives in [`crate::profiles`]; this module is
//! the small state machine and the painting around it.
//!
//! Nothing here blocks a frame: copying a library into a new profile runs on a
//! worker thread and is polled once per frame; every other operation touches a
//! handful of tiny directories.

use crate::gui::app::SoundSharedApp;
use crate::gui::dialog::{error_line, header, modal_sized};
use crate::gui::settings_extras::kit;
use crate::gui::theme::{alpha, hairline, micro_label, Metrics, Palette};
use crate::gui::widgets::{glyph, ConsoleButton};
use crate::profiles::{self, ProfileError, RecentProfiles};
use crossbeam_channel::{bounded, Receiver};
use egui::{Context, RichText, Ui};
use std::path::PathBuf;

/// Width of every profile modal.
const MODAL_W: f32 = 380.0;
/// Longest profile name shown on the top bar button.
const BUTTON_NAME_CHARS: usize = 18;

/// Where profiles live and which one is running.
#[derive(Clone, Debug)]
pub struct ProfileEnv {
    /// Root data directory (holds `profiles/` and `recent.toml`).
    pub root: PathBuf,
    /// Data directory of the running profile (source of a library copy).
    pub data_dir: PathBuf,
    /// The running profile; `None` is the default one.
    pub active: Option<String>,
}

impl ProfileEnv {
    /// The environment of this process.
    pub fn real() -> Self {
        Self {
            root: crate::paths::root_dir(),
            data_dir: crate::paths::data_dir(),
            active: crate::paths::active_profile(),
        }
    }
}

/// Which profile dialog is open.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum ProfileDialog {
    #[default]
    None,
    /// Confirm the restart into `target` (`None` = default profile).
    Switch {
        target: Option<String>,
        created: bool,
    },
    New {
        name: String,
        copy: bool,
        error: Option<String>,
        busy: bool,
    },
    Manage {
        message: Option<String>,
    },
    Rename {
        old: String,
        name: String,
        error: Option<String>,
    },
    Delete {
        name: String,
        typed: String,
        empty: bool,
        error: Option<String>,
    },
}

/// State of the profile menu and dialogs (one field of the app).
#[derive(Default)]
pub struct ProfilesUi {
    pub dialog: ProfileDialog,
    names: Vec<String>,
    recent: Vec<Option<String>>,
    loaded: bool,
    job: Option<Receiver<Result<String, ProfileError>>>,
}

impl ProfilesUi {
    /// Re-read the profile list and the recent list from disk.
    pub fn refresh(&mut self, env: &ProfileEnv) {
        self.names = profiles::list_profiles(&env.root);
        self.recent = RecentProfiles::load(&env.root).existing(&env.root);
        self.loaded = true;
    }

    /// Profiles in menu order: recently used first, then the rest. The default
    /// profile is `None`.
    pub fn menu_entries(&self) -> Vec<Option<String>> {
        let mut out: Vec<Option<String>> = Vec::new();
        let known = |p: &Option<String>, list: &[Option<String>]| {
            list.iter().any(|q| match (p, q) {
                (None, None) => true,
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                _ => false,
            })
        };
        for r in &self.recent {
            if !known(r, &out) {
                out.push(r.clone());
            }
        }
        if !known(&None, &out) {
            out.push(None);
        }
        for n in &self.names {
            let entry = Some(n.clone());
            if !known(&entry, &out) {
                out.push(entry);
            }
        }
        out
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn open_new(&mut self) {
        self.dialog = ProfileDialog::New {
            name: String::new(),
            copy: false,
            error: None,
            busy: false,
        };
    }

    pub fn open_manage(&mut self, message: Option<String>) {
        self.dialog = ProfileDialog::Manage { message };
    }

    pub fn open_switch(&mut self, target: Option<String>) {
        self.dialog = ProfileDialog::Switch {
            target,
            created: false,
        };
    }

    pub fn open_rename(&mut self, old: &str) {
        self.dialog = ProfileDialog::Rename {
            old: old.to_string(),
            name: old.to_string(),
            error: None,
        };
    }

    pub fn open_delete(&mut self, env: &ProfileEnv, name: &str) {
        self.dialog = ProfileDialog::Delete {
            name: name.to_string(),
            typed: String::new(),
            empty: profiles::profile_is_empty(&env.root, name),
            error: None,
        };
    }

    pub fn close(&mut self) {
        // A running copy cannot be cancelled; keep the dialog until it ends.
        if !self.is_busy() {
            self.dialog = ProfileDialog::None;
        }
    }

    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// Create the profile typed into the New dialog. Without the copy option it
    /// finishes at once; with it, a worker thread copies the library and
    /// [`ProfilesUi::poll_job`] picks the result up.
    pub fn submit_new(&mut self, env: &ProfileEnv) {
        let ProfileDialog::New {
            name, copy, error, ..
        } = &mut self.dialog
        else {
            return;
        };
        let (name, copy) = (name.clone(), *copy);
        if copy {
            let (tx, rx) = bounded(1);
            let (root, data_dir) = (env.root.clone(), env.data_dir.clone());
            std::thread::spawn(move || {
                let result = profiles::create_profile(&root, &name, Some(&data_dir))
                    .and_then(|_| profiles::validate_profile_name(&name));
                let _ = tx.send(result);
            });
            self.job = Some(rx);
            if let ProfileDialog::New { busy, error, .. } = &mut self.dialog {
                *busy = true;
                *error = None;
            }
            return;
        }
        match profiles::create_profile(&env.root, &name, None) {
            Ok(_) => {
                let created = profiles::validate_profile_name(&name).unwrap_or(name);
                self.refresh(env);
                self.dialog = ProfileDialog::Switch {
                    target: Some(created),
                    created: true,
                };
            }
            Err(e) => *error = Some(e.to_string()),
        }
    }

    /// Check the worker started by [`ProfilesUi::submit_new`].
    pub fn poll_job(&mut self, env: &ProfileEnv) {
        let Some(rx) = &self.job else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(crossbeam_channel::TryRecvError::Empty) => return,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Err(ProfileError::Io("the copy was interrupted".into()))
            }
        };
        self.job = None;
        match result {
            Ok(created) => {
                self.refresh(env);
                self.dialog = ProfileDialog::Switch {
                    target: Some(created),
                    created: true,
                };
            }
            Err(e) => {
                self.refresh(env);
                if let ProfileDialog::New { busy, error, .. } = &mut self.dialog {
                    *busy = false;
                    *error = Some(e.to_string());
                }
            }
        }
    }

    pub fn submit_rename(&mut self, env: &ProfileEnv) {
        let ProfileDialog::Rename { old, name, error } = &mut self.dialog else {
            return;
        };
        match profiles::rename_profile(&env.root, old, name, env.active.as_deref()) {
            Ok(()) => {
                let (old, new) = (old.clone(), name.trim().to_string());
                let mut recent = RecentProfiles::load(&env.root);
                recent.forget(&old);
                let _ = recent.save(&env.root);
                self.refresh(env);
                self.open_manage(Some(format!("Renamed {old} to {new}")));
            }
            Err(e) => *error = Some(e.to_string()),
        }
    }

    pub fn submit_delete(&mut self, env: &ProfileEnv) {
        let ProfileDialog::Delete {
            name, typed, error, ..
        } = &mut self.dialog
        else {
            return;
        };
        match profiles::delete_profile(&env.root, name, typed, env.active.as_deref()) {
            Ok(()) => {
                let name = name.clone();
                let mut recent = RecentProfiles::load(&env.root);
                recent.forget(&name);
                let _ = recent.save(&env.root);
                self.refresh(env);
                self.open_manage(Some(format!("Deleted {name}")));
            }
            Err(e) => *error = Some(e.to_string()),
        }
    }

    /// The target of a confirmed switch, if the open dialog is one and the
    /// profile still exists. The caller saves, requests the switch and closes.
    pub fn confirmed_switch(&mut self, env: &ProfileEnv) -> Option<Option<String>> {
        let ProfileDialog::Switch { target, .. } = &self.dialog else {
            return None;
        };
        let target = target.clone();
        if !profiles::profile_exists(&env.root, target.as_deref()) {
            self.dialog = ProfileDialog::None;
            return None;
        }
        self.dialog = ProfileDialog::None;
        Some(target)
    }
}

fn display_name(profile: &Option<String>) -> &str {
    profile.as_deref().unwrap_or("Default")
}

fn same_profile(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => false,
    }
}

fn short(name: &str) -> String {
    if name.chars().count() <= BUTTON_NAME_CHARS {
        name.to_string()
    } else {
        let cut: String = name.chars().take(BUTTON_NAME_CHARS - 1).collect();
        format!("{cut}…")
    }
}

/// Window title suffix for a non-default profile (`" · Camp"`), else empty.
pub fn title_suffix() -> String {
    match crate::paths::active_profile() {
        Some(name) => format!(" — {name}"),
        None => String::new(),
    }
}

/// How much room the profile control gets in the top bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileStyle {
    /// Draw the `PROFILE` caption before the button.
    pub label: bool,
    /// Widest the name text may be (the rest is cut with an ellipsis; the full
    /// name is in the tooltip).
    pub name_w: f32,
}

/// Longest prefix of `text` (plus an ellipsis when cut) that fits `max_w`.
pub fn fit_text(ui: &Ui, text: &str, font: egui::FontId, max_w: f32) -> String {
    let width = |t: &str| {
        ui.fonts(|f| {
            f.layout_no_wrap(t.to_string(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
    };
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| {
        let mut out: String = chars[..keep].iter().collect();
        out.push('\u{2026}');
        out
    };
    // Binary search for the most characters that still fit with the ellipsis.
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&cut(mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// The `PROFILE <name>` control of the top bar: the caption (when `style` has
/// room for it) and a menu button that switches, creates and manages profiles.
/// The caller places it; no spacing is added before it.
pub fn profile_menu(ui: &mut Ui, state: &mut SoundSharedApp, style: ProfileStyle) {
    let env = ProfileEnv::real();
    if !state.profiles_ui.loaded {
        state.profiles_ui.refresh(&env);
    }
    let active = env.active.clone();
    let full_name = display_name(&active).to_string();
    let font = crate::gui::widgets::glyph_font(11.0);
    let shown = fit_text(ui, &short(&full_name), font.clone(), style.name_w);
    if style.label {
        ui.label(micro_label("Profile"));
        ui.add_space(Metrics::SM);
    }
    let menu = ui.menu_button(
        RichText::new(shown).font(font).color(Palette::WHITE),
        |ui| {
            ui.set_min_width(220.0);
            ui.label(micro_label("Switch to (restarts Bardwave)"));
            ui.add_space(Metrics::SM);
            for entry in state.profiles_ui.menu_entries() {
                let current = same_profile(&entry, &active);
                let text = if current {
                    format!("{}  (running)", display_name(&entry))
                } else {
                    display_name(&entry).to_string()
                };
                if ui.selectable_label(current, text).clicked() && !current {
                    state.profiles_ui.open_switch(entry);
                    ui.close_menu();
                }
            }
            ui.add_space(Metrics::SM);
            hairline(ui);
            ui.add_space(Metrics::SM);
            if ui.button("New profile…").clicked() {
                state.profiles_ui.open_new();
                ui.close_menu();
            }
            if ui.button("Manage profiles…").clicked() {
                state.profiles_ui.refresh(&env);
                state.profiles_ui.open_manage(None);
                ui.close_menu();
            }
            if ui.button("Open data folder").clicked() {
                if let Err(e) = profiles::open_in_file_manager(&env.data_dir) {
                    tracing::warn!("Could not open {}: {}", env.data_dir.display(), e);
                }
                ui.close_menu();
            }
        },
    );
    menu.response.clone().on_hover_text(format!(
        "Profile: {full_name}. Switch, create or manage profiles"
    ));
    if menu.response.clicked() {
        // Opening the menu: pick up profiles created or removed elsewhere.
        state.profiles_ui.refresh(&env);
    }
}

fn paint_scrim(ctx: &Context) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("profiles_scrim"),
    ));
    painter.rect_filled(
        ctx.screen_rect(),
        egui::Rounding::ZERO,
        alpha(Palette::BLACK, 170),
    );
}

fn text_field(ui: &mut Ui, value: &mut String, hint: &str) -> egui::Response {
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .font(crate::gui::widgets::glyph_font(12.0))
            .margin(egui::Margin::symmetric(10.0, 8.0)),
    )
}

/// Which button of a dialog was pressed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Act {
    Cancel,
    Primary,
    Rename(String),
    Delete(String),
    Back,
    NewProfile,
}

fn buttons(ui: &mut Ui, primary: &str, id: &str, enabled: bool) -> Option<Act> {
    let mut act = None;
    ui.horizontal(|ui| {
        if ConsoleButton::primary(primary)
            .glyph(glyph::CHECK)
            .width(150.0)
            .height(Metrics::BTN_H)
            .enabled(enabled)
            .id_salt(id)
            .show(ui)
            .clicked()
        {
            act = Some(Act::Primary);
        }
        ui.add_space(Metrics::SM);
        if ConsoleButton::ghost("Cancel")
            .glyph(glyph::CROSS)
            .width(120.0)
            .height(Metrics::BTN_H)
            .id_salt(&format!("{id}_cancel"))
            .show(ui)
            .clicked()
        {
            act = Some(Act::Cancel);
        }
    });
    act
}

/// Paint the open profile dialog (if any) and apply what was pressed.
pub fn show_profile_dialogs(ctx: &Context, state: &mut SoundSharedApp) {
    let env = ProfileEnv::real();
    state.profiles_ui.poll_job(&env);
    if state.profiles_ui.is_busy() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    if matches!(state.profiles_ui.dialog, ProfileDialog::None) {
        return;
    }
    paint_scrim(ctx);
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.profiles_ui.close();
        if matches!(state.profiles_ui.dialog, ProfileDialog::None) {
            return;
        }
    }

    let active = env.active.clone();
    let names: Vec<String> = state.profiles_ui.names().to_vec();
    let mut act: Option<Act> = None;
    let mut dialog = state.profiles_ui.dialog.clone();

    modal_sized(ctx, "profiles_modal", MODAL_W, |ui| match &mut dialog {
        ProfileDialog::None => {}
        ProfileDialog::Switch { target, created } => {
            header(
                ui,
                "Switch Profile",
                if *created {
                    "Profile created"
                } else {
                    "Bardwave restarts with the other profile"
                },
            );
            ui.add_space(Metrics::MD);
            ui.label(
                micro_label(format!(
                    "Switch to \"{}\"? The server and all sounds stop, the current profile is saved.",
                    display_name(target)
                ))
                .line_height(Some(16.0)),
            );
            ui.add_space(Metrics::LG);
            hairline(ui);
            ui.add_space(Metrics::MD);
            act = buttons(ui, "Switch", "prof_switch", true);
        }
        ProfileDialog::New {
            name,
            copy,
            error,
            busy,
        } => {
            header(ui, "New Profile", "A separate config and library");
            ui.add_space(Metrics::MD);
            ui.label(micro_label("Profile name"));
            ui.add_space(Metrics::XS);
            ui.add_enabled_ui(!*busy, |ui| {
                let resp = text_field(ui, name, "Campaign name");
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    act = Some(Act::Primary);
                }
                ui.add_space(Metrics::SM);
                ui.checkbox(copy, "Copy the current library");
            });
            ui.add_space(Metrics::SM);
            match (&*error, *busy) {
                (_, true) => {
                    ui.label(micro_label("Copying the library…"));
                }
                (Some(message), _) => error_line(ui, message),
                _ => {
                    ui.label(micro_label("Letters, digits, space, - and _"));
                }
            }
            ui.add_space(Metrics::LG);
            hairline(ui);
            ui.add_space(Metrics::MD);
            let a = buttons(ui, "Create", "prof_new", !*busy);
            if a.is_some() {
                act = a;
            }
        }
        ProfileDialog::Manage { message } => {
            header(ui, "Profiles", "Rename or delete the other campaigns");
            ui.add_space(Metrics::MD);
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = Metrics::SM;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Default").color(Palette::WHITE));
                        if active.is_none() {
                            ui.label(micro_label("running"));
                        }
                    });
                    for name in &names {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(name).color(Palette::WHITE));
                            let running = active
                                .as_deref()
                                .is_some_and(|a| a.eq_ignore_ascii_case(name));
                            if running {
                                ui.label(micro_label("running"));
                            } else {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ConsoleButton::danger("Delete")
                                            .width(90.0)
                                            .small()
                                            .id_salt(&format!("prof_del_{name}"))
                                            .show(ui)
                                            .clicked()
                                        {
                                            act = Some(Act::Delete(name.clone()));
                                        }
                                        if ConsoleButton::ghost("Rename")
                                            .width(90.0)
                                            .small()
                                            .id_salt(&format!("prof_ren_{name}"))
                                            .show(ui)
                                            .clicked()
                                        {
                                            act = Some(Act::Rename(name.clone()));
                                        }
                                    },
                                );
                            }
                        });
                    }
                    if names.is_empty() {
                        ui.add_space(Metrics::XS);
                        if kit::empty_state_action(
                            ui,
                            "No other profiles yet",
                            "A profile is a separate config and library: keep one per campaign \
                             and switch between them without mixing sounds or scenes.",
                            "New profile…",
                            "prof_empty_new",
                        ) {
                            act = Some(Act::NewProfile);
                        }
                    }
                });
            if let Some(message) = message {
                ui.add_space(Metrics::MD);
                ui.label(micro_label(message.as_str()).line_height(Some(16.0)));
            }
            ui.add_space(Metrics::LG);
            hairline(ui);
            ui.add_space(Metrics::MD);
            ui.horizontal(|ui| {
                if ConsoleButton::primary("New profile…")
                    .glyph(glyph::PLUS)
                    .width(150.0)
                    .height(Metrics::BTN_H)
                    .id_salt("prof_manage_new")
                    .show(ui)
                    .clicked()
                {
                    act = Some(Act::NewProfile);
                }
                ui.add_space(Metrics::SM);
                if ConsoleButton::ghost("Close")
                    .width(120.0)
                    .height(Metrics::BTN_H)
                    .id_salt("prof_manage_close")
                    .show(ui)
                    .clicked()
                {
                    act = Some(Act::Cancel);
                }
            });
        }
        ProfileDialog::Rename { old, name, error } => {
            header(ui, "Rename Profile", old.as_str());
            ui.add_space(Metrics::MD);
            ui.label(micro_label("New name"));
            ui.add_space(Metrics::XS);
            let resp = text_field(ui, name, "Campaign name");
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                act = Some(Act::Primary);
            }
            ui.add_space(Metrics::XS);
            match &*error {
                Some(message) => error_line(ui, message),
                None => {
                    ui.label(micro_label("Letters, digits, space, - and _"));
                }
            }
            ui.add_space(Metrics::LG);
            hairline(ui);
            ui.add_space(Metrics::MD);
            let a = buttons(ui, "Rename", "prof_rename", true);
            if a == Some(Act::Cancel) {
                act = Some(Act::Back);
            } else if a.is_some() {
                act = a;
            }
        }
        ProfileDialog::Delete {
            name,
            typed,
            empty,
            error,
        } => {
            header(ui, "Delete Profile", name.as_str());
            ui.add_space(Metrics::MD);
            if *empty {
                ui.label(micro_label("This profile is empty."));
            } else {
                ui.label(micro_label(
                    "Its config and library are deleted for good. There is no trash.",
                ));
                ui.add_space(Metrics::SM);
                ui.label(micro_label(format!("Type \"{name}\" to confirm")));
                ui.add_space(Metrics::XS);
                text_field(ui, typed, name.as_str());
            }
            if let Some(message) = &*error {
                ui.add_space(Metrics::XS);
                error_line(ui, message);
            }
            ui.add_space(Metrics::LG);
            hairline(ui);
            ui.add_space(Metrics::MD);
            let ready = *empty || typed.trim() == name.as_str();
            let a = buttons(ui, "Delete", "prof_delete", ready);
            if a == Some(Act::Cancel) {
                act = Some(Act::Back);
            } else if a.is_some() {
                act = a;
            }
        }
    });

    // Typed text must survive to the next frame whatever was pressed.
    state.profiles_ui.dialog = dialog;

    let ui_state = &mut state.profiles_ui;
    match (act, ui_state.dialog.clone()) {
        (None, _) => {}
        (Some(Act::Cancel), _) => ui_state.close(),
        (Some(Act::Back), _) => ui_state.open_manage(None),
        (Some(Act::NewProfile), _) => ui_state.open_new(),
        (Some(Act::Rename(name)), _) => ui_state.open_rename(&name),
        (Some(Act::Delete(name)), _) => ui_state.open_delete(&env, &name),
        (Some(Act::Primary), ProfileDialog::New { .. }) => ui_state.submit_new(&env),
        (Some(Act::Primary), ProfileDialog::Rename { .. }) => ui_state.submit_rename(&env),
        (Some(Act::Primary), ProfileDialog::Delete { .. }) => ui_state.submit_delete(&env),
        (Some(Act::Primary), ProfileDialog::Switch { .. }) => {
            if let Some(target) = ui_state.confirmed_switch(&env) {
                // Save first: the new process starts from the files on disk.
                state.persist_library();
                profiles::request_switch(target);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        (Some(Act::Primary), _) => {}
    }
}
