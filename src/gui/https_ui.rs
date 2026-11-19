//! "SECURE CONNECTION (HTTPS)": the self-contained HTTPS section of the network
//! column.
//!
//! ```text
//! SECURE CONNECTION (HTTPS)
//! [ Off ][ On ]
//! Encrypts links over the internet. Not needed on a home network.      (Off)
//!
//! [ Self-signed (automatic) ]                                          (On)
//! [ My certificate          ]  -> certificate / key paths + Browse...
//! SHA-256 fingerprint, [ Copy ], valid until, issued for
//! [ Regenerate certificate ]   (self-signed only, asks first)
//! Applies on next Start. Player and GM links (and QR) will switch to https://
//! Restart the server to apply
//! ```
//!
//! The settings live in `server.tls`, `server.tls_cert` and `server.tls_key`
//! and are read by the server when it starts, so every change here applies on
//! the next Start. Everything that touches the disk (looking at the stored
//! certificate, checking the operator's own files, making a new certificate,
//! the file chooser) runs on a short-lived worker thread and comes back through
//! a channel: the render thread never waits for a file.
//!
//! The decisions (what is shown, which text, which file is at fault) are pure
//! functions at the top of this file, covered by unit tests and by
//! `tests/https_setting.rs`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, TryRecvError};
use egui::Ui;

use crate::gui::app::SoundSharedApp;
use crate::gui::settings_extras::kit;
use crate::gui::theme::{micro_label, Metrics, Palette};
use crate::gui::widgets::{glyph, glyph_font, BtnKind, ConsoleButton};
use crate::network::tls::{self, CertSource, SanSet, TlsError, TlsInfo, TlsSettings};

/// Heading of the section.
pub const TITLE: &str = "Secure connection (HTTPS)";
/// The one line shown while HTTPS is off.
pub const OFF_NOTE: &str = "Encrypts links over the internet. Not needed on a home network.";
/// Shown whenever the setting is on and not yet in use.
pub const APPLY_WARNING: &str =
    "Applies on next Start. Player and GM links (and QR) will switch to https://";
/// Shown while the running server has to be restarted for a change.
pub const RESTART_HINT: &str = "Restart the server to apply";

/// How often the stored certificate and the operator's files are read again
/// while the section is open (a Start can make the certificate, a renewal tool
/// can replace the files).
const REFRESH: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

/// Where the certificate comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertMode {
    /// Bardwave makes and keeps one (`tls_cert` / `tls_key` unset).
    SelfSigned,
    /// The operator's own PEM files.
    Custom,
}

impl CertMode {
    /// Button text.
    pub fn label(self) -> &'static str {
        match self {
            CertMode::SelfSigned => "Self-signed (automatic)",
            CertMode::Custom => "My certificate",
        }
    }
}

/// The mode a stored configuration is in: any path set means "My certificate".
pub fn mode_of(settings: &TlsSettings) -> CertMode {
    if settings.uses_custom_certificate() {
        CertMode::Custom
    } else {
        CertMode::SelfSigned
    }
}

/// Which of the two path fields hold something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathsState {
    /// Both empty.
    Neither,
    /// Only the certificate.
    OnlyCert,
    /// Only the key.
    OnlyKey,
    /// Both.
    Both,
}

/// Classify the two text fields (blank counts as empty).
pub fn paths_state(cert: &str, key: &str) -> PathsState {
    match (!cert.trim().is_empty(), !key.trim().is_empty()) {
        (false, false) => PathsState::Neither,
        (true, false) => PathsState::OnlyCert,
        (false, true) => PathsState::OnlyKey,
        (true, true) => PathsState::Both,
    }
}

/// A text field as the config stores it: trimmed, `None` when empty.
pub fn config_value(input: &str) -> Option<String> {
    let trimmed = input.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// One of the two path fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// `tls_cert`
    Cert,
    /// `tls_key`
    Key,
}

/// A message for one path field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    /// The field it belongs under.
    pub field: Field,
    /// Plain text for the operator.
    pub text: String,
}

/// What is wrong with a half-filled pair, `None` when there is nothing to say.
/// With only one of the two files the server refuses to start with HTTPS (it
/// does not fall back silently), so this says so before the Start.
pub fn missing_path_error(state: PathsState) -> Option<FieldError> {
    match state {
        PathsState::OnlyCert => Some(FieldError {
            field: Field::Key,
            text: "Choose the key file too. With only one of the two files the server will not \
                   start with HTTPS."
                .into(),
        }),
        PathsState::OnlyKey => Some(FieldError {
            field: Field::Cert,
            text: "Choose the certificate file too. With only one of the two files the server \
                   will not start with HTTPS."
                .into(),
        }),
        PathsState::Neither | PathsState::Both => None,
    }
}

/// Explain a certificate error in plain words and say which field it is about.
pub fn describe_error(error: &TlsError) -> FieldError {
    match error {
        TlsError::CertRead { reason, .. } => FieldError {
            field: Field::Cert,
            text: format!("Cannot read this file as a certificate: {reason}"),
        },
        TlsError::NoCertificate(_) => FieldError {
            field: Field::Cert,
            text: "No certificate found in this file. Expected a PEM file with a BEGIN \
                   CERTIFICATE line."
                .into(),
        },
        TlsError::KeyRead { reason, .. } => FieldError {
            field: Field::Key,
            text: format!("Cannot read this file as a private key: {reason}"),
        },
        TlsError::KeyMismatch(_) => FieldError {
            field: Field::Key,
            text: "This key does not belong to the certificate (or it is a type of key that \
                   cannot be used)."
                .into(),
        },
        TlsError::HalfConfigured => FieldError {
            field: Field::Key,
            text: "Both files are needed.".into(),
        },
        other => FieldError {
            field: Field::Cert,
            text: other.to_string(),
        },
    }
}

/// A problem with the file itself (checked before it is parsed): `None` when it
/// looks like a readable file. Touches the disk, so it belongs on a worker
/// thread.
pub fn file_problem(path: &Path) -> Option<&'static str> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some("File not found."),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Some("Bardwave is not allowed to read this file.")
        }
        Err(_) => Some("Cannot open this file."),
        Ok(meta) if meta.is_dir() => Some("This is a folder, not a file."),
        Ok(_) => None,
    }
}

/// Check the operator's two files: they exist, are readable, hold a certificate
/// and a key that belong together. Touches the disk, so it belongs on a worker
/// thread. On success it gives the certificate's facts (fingerprint).
pub fn check_files(cert: &Path, key: &Path) -> Result<TlsInfo, FieldError> {
    if let Some(text) = file_problem(cert) {
        return Err(FieldError {
            field: Field::Cert,
            text: text.into(),
        });
    }
    if let Some(text) = file_problem(key) {
        return Err(FieldError {
            field: Field::Key,
            text: text.into(),
        });
    }
    tls::check_custom(cert, key).map_err(|e| describe_error(&e))
}

/// What the section shows for a given setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Anything below the On/Off switch.
    pub expanded: bool,
    /// The "Self-signed / My certificate" choice.
    pub show_mode: bool,
    /// The two path fields.
    pub show_files: bool,
    /// Fingerprint, expiry and addresses.
    pub show_certificate: bool,
    /// The "Regenerate certificate" button (only for a certificate Bardwave
    /// made itself).
    pub show_regenerate: bool,
}

/// The state of the section: switched off it is a single line.
pub fn layout(enabled: bool, mode: CertMode) -> Layout {
    Layout {
        expanded: enabled,
        show_mode: enabled,
        show_files: enabled && mode == CertMode::Custom,
        show_certificate: enabled,
        show_regenerate: enabled && mode == CertMode::SelfSigned,
    }
}

/// The certificate the server uses now, or would use on the next Start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertFacts<'a> {
    /// SHA-256 fingerprint.
    pub fingerprint: &'a str,
    /// The operator's own certificate (not a generated one).
    pub custom: bool,
}

/// What the operator has to be told about applying the setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyNotice {
    /// Nothing pending.
    None,
    /// The server is stopped: the setting applies at the next Start.
    NextStart,
    /// The server runs over plain HTTP: it has to be restarted for HTTPS.
    RestartToEnable,
    /// The server runs over HTTPS with another certificate than the one now
    /// configured.
    RestartToChange,
    /// The server runs over HTTPS and HTTPS has been switched off.
    RestartToDisable,
    /// The running server already uses what is configured.
    InUse,
}

/// Pick the notice. `active` is the certificate the running HTTPS server uses
/// (`None` while it serves plain HTTP or is stopped); `desired` is the one the
/// next Start would use, when it is known.
pub fn apply_notice(
    enabled: bool,
    running: bool,
    active: Option<CertFacts<'_>>,
    desired: Option<CertFacts<'_>>,
) -> ApplyNotice {
    if !running {
        return if enabled {
            ApplyNotice::NextStart
        } else {
            ApplyNotice::None
        };
    }
    match (enabled, active) {
        (true, None) => ApplyNotice::RestartToEnable,
        (false, Some(_)) => ApplyNotice::RestartToDisable,
        (true, Some(active)) => match desired {
            Some(desired) if desired != active => ApplyNotice::RestartToChange,
            _ => ApplyNotice::InUse,
        },
        (false, None) => ApplyNotice::None,
    }
}

impl ApplyNotice {
    /// The warning about the links, shown while HTTPS is on and not in use yet.
    pub fn warning(self) -> Option<&'static str> {
        match self {
            ApplyNotice::NextStart | ApplyNotice::RestartToEnable => Some(APPLY_WARNING),
            ApplyNotice::RestartToChange => {
                Some("The certificate changes on the next Start. Players will be asked to trust it again.")
            }
            ApplyNotice::RestartToDisable => {
                Some("Links will switch back to http:// on the next Start.")
            }
            ApplyNotice::InUse => Some("HTTPS is active. Links start with https://"),
            ApplyNotice::None => None,
        }
    }

    /// The "restart" hint, shown while the running server is out of date.
    pub fn restart_hint(self) -> Option<&'static str> {
        match self {
            ApplyNotice::RestartToEnable
            | ApplyNotice::RestartToChange
            | ApplyNotice::RestartToDisable => Some(RESTART_HINT),
            _ => None,
        }
    }
}

/// `2027-12-21`.
pub fn format_date(unix_secs: i64) -> String {
    let (y, m, d) = tls::date_utc(unix_secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The expiry line of a certificate. `auto_renew` is true for a certificate
/// Bardwave made itself: it replaces it at the next Start when it has less
/// than [`tls::RENEW_BEFORE_DAYS`] days left.
pub fn expiry_text(not_after: i64, now: i64, auto_renew: bool) -> String {
    let days_left = (not_after - now).div_euclid(86_400);
    let date = format_date(not_after);
    if not_after <= now {
        return if auto_renew {
            format!("Expired on {date}. A new one is made on the next Start.")
        } else {
            format!("Expired on {date}.")
        };
    }
    let left = if days_left == 1 {
        "1 day left".to_string()
    } else {
        format!("{days_left} days left")
    };
    if auto_renew && days_left < tls::RENEW_BEFORE_DAYS {
        format!("Valid until {date} ({left}). A new one is made on the next Start.")
    } else {
        format!("Valid until {date} ({left})")
    }
}

/// The names and addresses a generated certificate covers, as one line.
pub fn issued_for_text(names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| names.join(", "))
}

/// What to say about the certificate, by where it comes from.
pub fn source_label(source: CertSource) -> &'static str {
    match source {
        CertSource::Custom => "Your certificate",
        CertSource::SelfSigned => "Self-signed certificate",
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The answer of a file check, with the text it was made for.
struct CheckAnswer {
    cert: String,
    key: String,
    result: Result<TlsInfo, FieldError>,
}

/// Per-window state of the section.
#[derive(Default)]
pub struct HttpsUi {
    /// Read from the config on the first frame.
    inited: bool,
    /// The "My certificate" choice (kept apart from the config because it is
    /// legal to pick it before any path is typed).
    custom: bool,
    /// Text of the two path fields.
    cert_input: String,
    key_input: String,
    /// The latest file check, and the one on its way.
    check: Option<CheckAnswer>,
    check_at: Option<Instant>,
    check_rx: Option<Receiver<CheckAnswer>>,
    /// The generated certificate as stored in the data directory.
    stored: Option<TlsInfo>,
    stored_loaded: bool,
    stored_at: Option<Instant>,
    stored_rx: Option<Receiver<Option<TlsInfo>>>,
    /// "Regenerate certificate": the question, the worker and its error.
    confirm_regen: bool,
    regen_rx: Option<Receiver<Result<TlsInfo, String>>>,
    regen_error: Option<String>,
    /// The file chooser that is open.
    picking: Option<(Field, Receiver<Option<PathBuf>>)>,
    /// Where the generated certificate lives, instead of `<data dir>/tls`
    /// (tests only: they must not touch the real data directory).
    #[cfg(test)]
    dir_override: Option<PathBuf>,
}

impl HttpsUi {
    fn spawn_check(&mut self, ctx: &egui::Context) {
        let (cert, key) = (self.cert_input.clone(), self.key_input.clone());
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("https-check".into())
            .spawn(move || {
                let result = check_files(Path::new(cert.trim()), Path::new(key.trim()));
                let _ = tx.send(CheckAnswer { cert, key, result });
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.check_rx = Some(rx);
        }
        self.check_at = Some(Instant::now());
    }

    fn spawn_stored(&mut self, ctx: &egui::Context, dir: PathBuf) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("https-stored".into())
            .spawn(move || {
                let _ = tx.send(tls::stored_info(&dir));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.stored_rx = Some(rx);
        }
        self.stored_at = Some(Instant::now());
    }

    fn spawn_regenerate(&mut self, ctx: &egui::Context, dir: PathBuf) {
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("https-regenerate".into())
            .spawn(move || {
                let result =
                    tls::regenerate_self_signed(&dir, &SanSet::detect()).map_err(|e| e.to_string());
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.regen_rx = Some(rx),
            Err(e) => self.regen_error = Some(format!("Cannot start the worker: {e}")),
        }
    }

    fn spawn_picker(&mut self, ctx: &egui::Context, field: Field) {
        if self.picking.is_some() {
            return;
        }
        let current = match field {
            Field::Cert => self.cert_input.clone(),
            Field::Key => self.key_input.clone(),
        };
        let (tx, rx) = bounded(1);
        let ctx = ctx.clone();
        let title = match field {
            Field::Cert => "Choose the certificate file (PEM)",
            Field::Key => "Choose the private key file (PEM)",
        };
        let spawned = std::thread::Builder::new()
            .name("https-file-dialog".into())
            .spawn(move || {
                let mut dialog = rfd::FileDialog::new().set_title(title);
                if let Some(parent) = Path::new(current.trim()).parent().filter(|p| p.is_dir()) {
                    dialog = dialog.set_directory(parent);
                }
                let _ = tx.send(dialog.pick_file());
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.picking = Some((field, rx));
        }
    }

    /// Collect what the workers have finished. Returns the field a chosen file
    /// went into, so the caller can save the config.
    fn poll(&mut self) -> bool {
        let mut picked = false;
        if let Some(rx) = &self.check_rx {
            match rx.try_recv() {
                Ok(answer) => {
                    self.check = Some(answer);
                    self.check_rx = None;
                }
                Err(TryRecvError::Disconnected) => self.check_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.stored_rx {
            match rx.try_recv() {
                Ok(info) => {
                    self.stored = info;
                    self.stored_loaded = true;
                    self.stored_rx = None;
                }
                Err(TryRecvError::Disconnected) => self.stored_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.regen_rx {
            match rx.try_recv() {
                Ok(Ok(info)) => {
                    self.stored = Some(info);
                    self.stored_loaded = true;
                    self.regen_error = None;
                    self.regen_rx = None;
                }
                Ok(Err(message)) => {
                    self.regen_error = Some(message);
                    self.regen_rx = None;
                }
                Err(TryRecvError::Disconnected) => self.regen_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some((field, rx)) = &self.picking {
            let field = *field;
            match rx.try_recv() {
                Ok(path) => {
                    self.picking = None;
                    if let Some(path) = path {
                        let text = path.to_string_lossy().into_owned();
                        match field {
                            Field::Cert => self.cert_input = text,
                            Field::Key => self.key_input = text,
                        }
                        picked = true;
                    }
                }
                Err(TryRecvError::Disconnected) => self.picking = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        picked
    }

    /// The check that belongs to the text now in the fields.
    fn current_check(&self) -> Option<&CheckAnswer> {
        self.check
            .as_ref()
            .filter(|c| c.cert == self.cert_input && c.key == self.key_input)
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Write the path fields to `server.tls_cert` / `server.tls_key`.
fn store_paths(state: &mut SoundSharedApp, cert: Option<String>, key: Option<String>) {
    let mut config = state.app_state.config.lock().unwrap();
    config.server.tls_cert = cert;
    config.server.tls_key = key;
    drop(config);
    state.library_dirty = true;
}

/// Two or more buttons side by side; the chosen one is solid. Returns the
/// index that was clicked.
fn segmented(ui: &mut Ui, labels: &[&str], selected: usize, salt: &str) -> Option<usize> {
    let gap = Metrics::SM;
    let width = (ui.available_width() - gap * (labels.len() as f32 - 1.0)) / labels.len() as f32;
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, label) in labels.iter().enumerate() {
            let kind = if i == selected {
                BtnKind::Primary
            } else {
                BtnKind::Ghost
            };
            let id = format!("{salt}_{i}");
            if ConsoleButton::new(*label)
                .kind(kind)
                .small()
                .width(width.floor())
                .id_salt(&id)
                .show(ui)
                .clicked()
                && i != selected
            {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Height of a path well.
const PATH_WELL_H: f32 = Metrics::BTN_H_SM + 2.0 * Metrics::XS;

/// A path well: the console's inset field around a text edit. The well is
/// allocated at exactly `width` and the text edit is clipped to it: a long path
/// must scroll inside the field, not push the panel wider.
fn path_field(ui: &mut Ui, value: &mut String, width: f32, salt: &str) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width.max(48.0), PATH_WELL_H),
        egui::Sense::hover(),
    );
    ui.painter().rect(
        rect,
        egui::Rounding::same(Metrics::R_SM),
        Palette::WELL,
        egui::Stroke::new(1.0_f32, Palette::BORDER),
    );
    let inner = rect.shrink2(egui::vec2(Metrics::SM, 0.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.set_clip_rect(inner.intersect(ui.clip_rect()));
    child.add(
        egui::TextEdit::singleline(value)
            .id_salt(salt)
            .frame(false)
            .font(glyph_font(11.0))
            .text_color(Palette::WHITE)
            .desired_width(inner.width())
            .margin(egui::Margin::ZERO),
    )
}

/// A caption, the path well with its Browse button, and the message for it.
/// Returns whether the text changed, and whether Browse was pressed.
fn path_row(
    ui: &mut Ui,
    caption: &str,
    value: &mut String,
    salt: &str,
    busy: bool,
    message: Option<&str>,
) -> (bool, bool) {
    ui.label(micro_label(caption));
    kit::caption_gap(ui);
    let browse_w = 84.0;
    let mut changed = false;
    let mut browse = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Metrics::SM;
        let field_w = ui.available_width() - browse_w - Metrics::SM;
        changed = path_field(ui, value, field_w, salt).changed();
        browse = ConsoleButton::ghost("Browse…")
            .glyph(glyph::FOLDER)
            .small()
            .width(browse_w)
            .height(PATH_WELL_H)
            .enabled(!busy)
            .id_salt(&format!("{salt}_browse"))
            .show(ui)
            .clicked();
    });
    if let Some(message) = message {
        kit::note_under(ui, message, Palette::ROSE_400);
    }
    (changed, browse)
}

/// The "SECURE CONNECTION (HTTPS)" section. `running` is whether the server is
/// started or starting.
pub fn https_section(ui: &mut Ui, state: &mut SoundSharedApp, running: bool) {
    // The section's state is taken out of the app for the frame, so the
    // widgets can call app methods without a double borrow.
    let mut s = std::mem::take(&mut state.extras.https);
    draw(ui, state, &mut s, running);
    state.extras.https = s;
}

fn draw(ui: &mut Ui, state: &mut SoundSharedApp, s: &mut HttpsUi, running: bool) {
    let (enabled, settings, raw_cert, raw_key) = {
        let config = state.app_state.config.lock().unwrap();
        (
            config.server.tls,
            TlsSettings::from_config(&config.server),
            config.server.tls_cert.clone().unwrap_or_default(),
            config.server.tls_key.clone().unwrap_or_default(),
        )
    };
    #[cfg(test)]
    let settings = TlsSettings {
        dir: s.dir_override.clone(),
        ..settings
    };
    if !s.inited {
        s.inited = true;
        s.custom = mode_of(&settings) == CertMode::Custom;
        s.cert_input = raw_cert;
        s.key_input = raw_key;
    }
    if s.poll() {
        // A file was chosen.
        store_paths(
            state,
            config_value(&s.cert_input),
            config_value(&s.key_input),
        );
    }

    ui.label(micro_label(TITLE));
    ui.add_space(Metrics::SM);

    if let Some(index) = segmented(ui, &["Off", "On"], usize::from(enabled), "https_switch") {
        state.app_state.config.lock().unwrap().server.tls = index == 1;
        state.library_dirty = true;
    }
    let enabled = state.app_state.config.lock().unwrap().server.tls;

    let mode = if s.custom {
        CertMode::Custom
    } else {
        CertMode::SelfSigned
    };
    let shape = layout(enabled, mode);
    if !shape.expanded {
        kit::note_under(ui, OFF_NOTE, Palette::ZINC_500);
        // A switched-off section still has to say that a running HTTPS server
        // keeps its links until it is restarted.
        let active = tls::active_info();
        let notice = apply_notice(
            false,
            running,
            active.as_ref().map(|i| CertFacts {
                fingerprint: &i.fingerprint_sha256,
                custom: i.source == CertSource::Custom,
            }),
            None,
        );
        notices(ui, notice);
        return;
    }

    ui.add_space(Metrics::MD);
    if shape.show_mode {
        let selected = usize::from(mode == CertMode::Custom);
        let labels = [CertMode::SelfSigned.label(), CertMode::Custom.label()];
        if let Some(index) = segmented_column(ui, &labels, selected, "https_mode") {
            s.custom = index == 1;
            if s.custom {
                store_paths(
                    state,
                    config_value(&s.cert_input),
                    config_value(&s.key_input),
                );
            } else {
                store_paths(state, None, None);
            }
        }
    }
    let mode = if s.custom {
        CertMode::Custom
    } else {
        CertMode::SelfSigned
    };
    let shape = layout(enabled, mode);

    let ctx = ui.ctx().clone();
    let mut desired: Option<TlsInfo> = None;

    if shape.show_files {
        ui.add_space(Metrics::MD);
        let state_of_paths = paths_state(&s.cert_input, &s.key_input);
        if state_of_paths == PathsState::Both {
            let stale = s.current_check().is_none();
            let due = s.check_at.is_none_or(|at| at.elapsed() >= REFRESH);
            if (stale || due) && s.check_rx.is_none() {
                s.spawn_check(&ctx);
            }
        }
        let missing = missing_path_error(state_of_paths);
        let (check_error, check_ok) = match s.current_check() {
            Some(CheckAnswer { result: Err(e), .. }) if state_of_paths == PathsState::Both => {
                (Some(e.clone()), None)
            }
            Some(CheckAnswer { result: Ok(i), .. }) if state_of_paths == PathsState::Both => {
                (None, Some(i.clone()))
            }
            _ => (None, None),
        };
        let message_for = |field: Field| -> Option<String> {
            missing
                .iter()
                .chain(check_error.iter())
                .find(|e| e.field == field)
                .map(|e| e.text.clone())
        };

        let busy = s.picking.is_some();
        let (cert_changed, cert_browse) = path_row(
            ui,
            "Certificate file (PEM)",
            &mut s.cert_input,
            "https_cert_path",
            busy,
            message_for(Field::Cert).as_deref(),
        );
        ui.add_space(Metrics::SM);
        let (key_changed, key_browse) = path_row(
            ui,
            "Private key file (PEM)",
            &mut s.key_input,
            "https_key_path",
            busy,
            message_for(Field::Key).as_deref(),
        );
        if cert_browse {
            s.spawn_picker(&ctx, Field::Cert);
        }
        if key_browse {
            s.spawn_picker(&ctx, Field::Key);
        }
        if cert_changed || key_changed {
            store_paths(
                state,
                config_value(&s.cert_input),
                config_value(&s.key_input),
            );
        }
        match state_of_paths {
            PathsState::Neither => {
                kit::note_under(
                    ui,
                    "No files chosen: a self-signed certificate is used until you choose both.",
                    Palette::AMBER_400,
                );
            }
            PathsState::Both if check_ok.is_some() => {
                kit::note_under(
                    ui,
                    "The key belongs to the certificate. Bardwave reads the files at Start, \
                     so restart the server after you renew them.",
                    Palette::ZINC_500,
                );
            }
            PathsState::Both if check_error.is_none() => {
                kit::note_under(ui, "Checking the files…", Palette::ZINC_500);
            }
            _ => {}
        }
        desired = check_ok;
    }
    // The generated certificate is what is used in self-signed mode, and in
    // "My certificate" mode while no file is chosen.
    let uses_generated =
        !shape.show_files || paths_state(&s.cert_input, &s.key_input) == PathsState::Neither;
    if uses_generated {
        // Look at the stored certificate now and then (a Start makes it).
        let due = s.stored_at.is_none_or(|at| at.elapsed() >= REFRESH);
        if (!s.stored_loaded || due) && s.stored_rx.is_none() {
            s.spawn_stored(&ctx, settings.cert_dir());
        }
        desired = s.stored.clone();
    }
    ctx.request_repaint_after(REFRESH);

    // ---- Certificate facts ----
    let active = tls::active_info();
    let shown = desired.clone().or_else(|| active.clone());
    if shape.show_certificate {
        ui.add_space(Metrics::MD);
        match &shown {
            Some(info) => certificate_block(ui, state, info),
            None => {
                let note = match mode {
                    CertMode::SelfSigned => "A certificate is made on the next Start.",
                    CertMode::Custom => "The fingerprint appears once both files are valid.",
                };
                kit::note(ui, note);
            }
        }
    }

    // ---- Regenerate ----
    if shape.show_regenerate {
        ui.add_space(Metrics::MD);
        regenerate_block(ui, s, &settings);
    }

    // ---- What happens when ----
    let facts = |i: &TlsInfo| -> (String, bool) {
        (i.fingerprint_sha256.clone(), i.source == CertSource::Custom)
    };
    let active_facts = active.as_ref().map(facts);
    let desired_facts = desired.as_ref().map(facts);
    let notice = apply_notice(
        enabled,
        running,
        active_facts.as_ref().map(|(f, c)| CertFacts {
            fingerprint: f,
            custom: *c,
        }),
        desired_facts.as_ref().map(|(f, c)| CertFacts {
            fingerprint: f,
            custom: *c,
        }),
    );
    ui.add_space(Metrics::MD);
    notices(ui, notice);
}

/// The mode choice: one full-width button per option, stacked.
fn segmented_column(ui: &mut Ui, labels: &[&str], selected: usize, salt: &str) -> Option<usize> {
    let mut clicked = None;
    for (i, label) in labels.iter().enumerate() {
        if i > 0 {
            ui.add_space(Metrics::XS);
        }
        let kind = if i == selected {
            BtnKind::Primary
        } else {
            BtnKind::Ghost
        };
        let id = format!("{salt}_{i}");
        if ConsoleButton::new(*label)
            .kind(kind)
            .small()
            .id_salt(&id)
            .show(ui)
            .clicked()
            && i != selected
        {
            clicked = Some(i);
        }
    }
    clicked
}

/// The amber warning and the restart hint.
fn notices(ui: &mut Ui, notice: ApplyNotice) {
    if let Some(text) = notice.warning() {
        let color = if notice == ApplyNotice::InUse {
            Palette::ZINC_500
        } else {
            Palette::AMBER_400
        };
        kit::note_colored(ui, text, color);
    }
    if let Some(text) = notice.restart_hint() {
        kit::note_under(ui, text, Palette::ROSE_400);
    }
}

/// Fingerprint with a Copy button, expiry and the addresses.
fn certificate_block(ui: &mut Ui, state: &SoundSharedApp, info: &TlsInfo) {
    ui.label(micro_label(format!(
        "{} / SHA-256",
        source_label(info.source)
    )));
    kit::caption_gap(ui);
    ui.add(
        egui::Label::new(kit::note_text(
            &info.fingerprint_sha256,
            Metrics::MICRO_TINY,
            Palette::ZINC_300,
        ))
        .wrap(),
    );
    ui.add_space(Metrics::SM);
    if ConsoleButton::ghost("Copy")
        .glyph(glyph::COPY)
        .small()
        .width(110.0)
        .tooltip("Copy the fingerprint. Compare it with the one the browser shows in its warning.")
        .id_salt("https_copy_fp")
        .show(ui)
        .clicked()
    {
        ui.output_mut(|o| o.copied_text = info.fingerprint_sha256.clone());
        state.set_copied_feedback(true);
    }

    match info.source {
        CertSource::SelfSigned => {
            if let Some(not_after) = info.not_after {
                ui.add_space(Metrics::SM);
                kit::note_colored(
                    ui,
                    expiry_text(not_after, now_unix(), true),
                    Palette::ZINC_400,
                );
            }
            if let Some(names) = issued_for_text(&info.names) {
                kit::note_under(ui, format!("Issued for: {names}"), Palette::ZINC_500);
            }
        }
        CertSource::Custom => {
            kit::note_under(
                ui,
                "Expiry is not read from your own certificate: your certificate authority or \
                 tool manages it.",
                Palette::ZINC_500,
            );
        }
    }
}

/// "Regenerate certificate" with its question.
fn regenerate_block(ui: &mut Ui, s: &mut HttpsUi, settings: &TlsSettings) {
    let busy = s.regen_rx.is_some();
    if !s.confirm_regen {
        if ConsoleButton::ghost(if busy {
            "Regenerating…"
        } else {
            "Regenerate certificate"
        })
        .glyph(glyph::REFRESH)
        .small()
        .enabled(!busy)
        .tooltip("Make a new self-signed certificate (asks first)")
        .id_salt("https_regen")
        .show(ui)
        .clicked()
        {
            s.confirm_regen = true;
        }
    } else {
        kit::note_colored(
            ui,
            "A new certificate has a new fingerprint. Players' browsers will warn again and \
             must trust it again.",
            Palette::AMBER_400,
        );
        ui.add_space(Metrics::SM);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Metrics::SM;
            if ConsoleButton::danger("Regenerate")
                .small()
                .width(110.0)
                .id_salt("https_regen_yes")
                .show(ui)
                .clicked()
            {
                s.confirm_regen = false;
                s.regen_error = None;
                let ctx = ui.ctx().clone();
                s.spawn_regenerate(&ctx, settings.cert_dir());
            }
            if ConsoleButton::ghost("Cancel")
                .small()
                .width(110.0)
                .id_salt("https_regen_no")
                .show(ui)
                .clicked()
            {
                s.confirm_regen = false;
            }
        });
    }
    if let Some(error) = &s.regen_error {
        kit::note_under(
            ui,
            format!("Could not make a new certificate: {error}"),
            Palette::ROSE_400,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::gui::hotkeys_ui::test_support::{harness, settle};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "bardwave-https-ui-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn facts(fp: &str, custom: bool) -> CertFacts<'_> {
        CertFacts {
            fingerprint: fp,
            custom,
        }
    }

    #[test]
    fn the_mode_follows_the_paths() {
        assert_eq!(mode_of(&TlsSettings::default()), CertMode::SelfSigned);
        let with_cert = TlsSettings {
            cert: Some("/a.pem".into()),
            ..Default::default()
        };
        assert_eq!(mode_of(&with_cert), CertMode::Custom);
    }

    #[test]
    fn switched_off_is_one_collapsed_line() {
        let off = layout(false, CertMode::SelfSigned);
        assert!(!off.expanded && !off.show_mode && !off.show_files);
        assert!(!off.show_certificate && !off.show_regenerate);
        assert!(!layout(false, CertMode::Custom).show_files);
    }

    #[test]
    fn switched_on_shows_what_the_mode_needs() {
        let own = layout(true, CertMode::SelfSigned);
        assert!(own.expanded && own.show_mode && own.show_certificate);
        assert!(own.show_regenerate && !own.show_files);
        let custom = layout(true, CertMode::Custom);
        assert!(custom.show_files && !custom.show_regenerate);
    }

    #[test]
    fn paths_are_classified_and_trimmed() {
        assert_eq!(paths_state("", "  "), PathsState::Neither);
        assert_eq!(paths_state("/a", ""), PathsState::OnlyCert);
        assert_eq!(paths_state(" ", "/k"), PathsState::OnlyKey);
        assert_eq!(paths_state("/a", "/k"), PathsState::Both);
        assert_eq!(config_value("  /a.pem "), Some("/a.pem".into()));
        assert_eq!(config_value("   "), None);
    }

    #[test]
    fn half_a_pair_names_the_missing_field() {
        let e = missing_path_error(PathsState::OnlyCert).unwrap();
        assert_eq!(e.field, Field::Key);
        assert!(e.text.contains("will not start"), "{}", e.text);
        let e = missing_path_error(PathsState::OnlyKey).unwrap();
        assert_eq!(e.field, Field::Cert);
        assert!(missing_path_error(PathsState::Both).is_none());
        assert!(missing_path_error(PathsState::Neither).is_none());
    }

    #[test]
    fn certificate_errors_land_under_the_right_field() {
        let cert = describe_error(&TlsError::NoCertificate("/x".into()));
        assert_eq!(cert.field, Field::Cert);
        assert!(cert.text.contains("BEGIN CERTIFICATE"));
        let key = describe_error(&TlsError::KeyMismatch("boom".into()));
        assert_eq!(key.field, Field::Key);
        assert!(key.text.contains("does not belong"));
        let unreadable = describe_error(&TlsError::KeyRead {
            path: "/k".into(),
            reason: "denied".into(),
        });
        assert_eq!(unreadable.field, Field::Key);
        assert!(unreadable.text.contains("denied"));
    }

    #[test]
    fn the_notice_follows_the_server_and_the_setting() {
        // Stopped.
        assert_eq!(apply_notice(false, false, None, None), ApplyNotice::None);
        assert_eq!(
            apply_notice(true, false, None, None),
            ApplyNotice::NextStart
        );
        // Running over plain HTTP.
        assert_eq!(
            apply_notice(true, true, None, None),
            ApplyNotice::RestartToEnable
        );
        assert_eq!(apply_notice(false, true, None, None), ApplyNotice::None);
        // Running over HTTPS.
        let a = facts("AA", false);
        assert_eq!(
            apply_notice(false, true, Some(a), None),
            ApplyNotice::RestartToDisable
        );
        assert_eq!(
            apply_notice(true, true, Some(a), Some(a)),
            ApplyNotice::InUse
        );
        assert_eq!(
            apply_notice(true, true, Some(a), Some(facts("BB", false))),
            ApplyNotice::RestartToChange
        );
        assert_eq!(
            apply_notice(true, true, Some(a), Some(facts("AA", true))),
            ApplyNotice::RestartToChange
        );
        // An unknown desired certificate is not a change.
        assert_eq!(apply_notice(true, true, Some(a), None), ApplyNotice::InUse);
    }

    #[test]
    fn the_warning_and_the_restart_hint_use_the_required_words() {
        for notice in [ApplyNotice::NextStart, ApplyNotice::RestartToEnable] {
            assert_eq!(
                notice.warning(),
                Some("Applies on next Start. Player and GM links (and QR) will switch to https://")
            );
        }
        assert_eq!(
            ApplyNotice::RestartToEnable.restart_hint(),
            Some("Restart the server to apply")
        );
        assert_eq!(ApplyNotice::NextStart.restart_hint(), None);
        assert_eq!(ApplyNotice::InUse.restart_hint(), None);
        assert!(ApplyNotice::None.warning().is_none());
        assert!(ApplyNotice::RestartToDisable
            .warning()
            .unwrap()
            .contains("http://"));
    }

    #[test]
    fn dates_and_expiry_read_clearly() {
        // 2024-03-01 00:00:00 UTC
        let day = 19_783 * 86_400;
        assert_eq!(format_date(day), "2024-03-01");
        assert_eq!(
            expiry_text(day + 100 * 86_400, day, true),
            "Valid until 2024-06-09 (100 days left)"
        );
        assert!(expiry_text(day + 10 * 86_400, day, true).contains("made on the next Start"));
        assert!(!expiry_text(day + 10 * 86_400, day, false).contains("made on"));
        assert!(expiry_text(day + 86_400 + 5, day, false).contains("1 day left"));
        assert!(expiry_text(day - 5, day, true).starts_with("Expired on"));
        assert_eq!(issued_for_text(&[]), None);
        assert_eq!(
            issued_for_text(&["localhost".into(), "10.0.0.2".into()]),
            Some("localhost, 10.0.0.2".into())
        );
    }

    #[test]
    fn check_files_reports_each_kind_of_problem() {
        let dir = TempDir::new("check");
        let tls_dir = dir.0.join("gen-a");
        let a = tls::regenerate_self_signed(&tls_dir, &SanSet::detect()).expect("cert a");
        let cert = tls_dir.join(tls::CERT_FILE);
        let key = tls_dir.join(tls::KEY_FILE);

        let ok = check_files(&cert, &key).expect("a matching pair is valid");
        assert_eq!(ok.fingerprint_sha256, a.fingerprint_sha256);

        let missing = check_files(&dir.0.join("nope.pem"), &key).unwrap_err();
        assert_eq!(missing.field, Field::Cert);
        assert_eq!(missing.text, "File not found.");

        let folder = check_files(&cert, &dir.0).unwrap_err();
        assert_eq!(folder.field, Field::Key);
        assert!(folder.text.contains("folder"));

        // A key of another certificate.
        let other_dir = dir.0.join("gen-b");
        tls::regenerate_self_signed(&other_dir, &SanSet::detect()).expect("cert b");
        let mismatch = check_files(&cert, &other_dir.join(tls::KEY_FILE)).unwrap_err();
        assert_eq!(mismatch.field, Field::Key);
        assert!(
            mismatch.text.contains("does not belong"),
            "{}",
            mismatch.text
        );

        // A text file is no certificate.
        let junk = dir.0.join("junk.pem");
        std::fs::write(&junk, "hello").unwrap();
        let none = check_files(&junk, &key).unwrap_err();
        assert_eq!(none.field, Field::Cert);
        assert!(none.text.contains("No certificate"), "{}", none.text);
    }

    fn draw_section(
        h: &mut crate::gui::hotkeys_ui::test_support::Harness,
        running: bool,
    ) -> String {
        settle(&h.ctx, 2, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| https_section(ui, &mut h.app, running));
        })
    }

    #[test]
    fn off_by_default_shows_one_explanatory_line() {
        let mut h = harness(Config::default(), false);
        let text = draw_section(&mut h, false);
        assert!(text.contains("SECURE CONNECTION (HTTPS)"), "{text}");
        assert!(text.contains("ENCRYPTS LINKS OVER THE INTERNET"), "{text}");
        assert!(!text.contains("SELF-SIGNED"), "{text}");
        assert!(!text.contains("APPLIES ON NEXT START"), "{text}");
    }

    #[test]
    fn switched_on_it_shows_the_mode_and_the_warning() {
        let mut config = Config::default();
        config.server.tls = true;
        let mut h = harness(config, false);
        let text = draw_section(&mut h, false);
        assert!(text.contains("SELF-SIGNED (AUTOMATIC)"), "{text}");
        assert!(text.contains("MY CERTIFICATE"), "{text}");
        assert!(text.contains("REGENERATE CERTIFICATE"), "{text}");
        assert!(
            text.contains(
                "APPLIES ON NEXT START. PLAYER AND GM LINKS (AND QR) WILL SWITCH TO HTTPS://"
            ),
            "{text}"
        );
        assert!(!text.contains("RESTART THE SERVER TO APPLY"), "{text}");
        // A running plain-HTTP server has to be restarted.
        let running = draw_section(&mut h, true);
        assert!(running.contains("RESTART THE SERVER TO APPLY"), "{running}");
    }

    #[test]
    fn my_certificate_shows_the_two_path_fields() {
        let mut config = Config::default();
        config.server.tls = true;
        config.server.tls_cert = Some("/etc/ssl/none-cert.pem".into());
        let mut h = harness(config, false);
        let text = draw_section(&mut h, false);
        assert!(text.contains("CERTIFICATE FILE (PEM)"), "{text}");
        assert!(text.contains("PRIVATE KEY FILE (PEM)"), "{text}");
        assert!(text.contains("BROWSE"), "{text}");
        assert!(text.contains("CHOOSE THE KEY FILE TOO"), "{text}");
        assert!(!text.contains("REGENERATE CERTIFICATE"), "{text}");
    }

    #[test]
    fn the_section_fits_the_panel_width_in_every_state() {
        const PANEL_BODY_W: f32 = 292.0;
        for (tls, cert) in [
            (false, None),
            (true, None),
            (true, Some("/a/very/long/path/to/a/certificate/file.pem")),
        ] {
            let mut config = Config::default();
            config.server.tls = tls;
            config.server.tls_cert = cert.map(str::to_string);
            config.server.tls_key =
                cert.map(|_| "/a/very/long/path/to/a/private/key.pem".to_string());
            let mut h = harness(config, false);
            h.app.extras.https.dir_override = Some(std::env::temp_dir().join(format!(
                "bardwave-https-ui-width-{}-nothing-here",
                std::process::id()
            )));
            let mut width = 0.0;
            let mut app_ref = &mut h.app;
            for _ in 0..2 {
                crate::gui::hotkeys_ui::test_support::frame(&h.ctx, vec![], |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.scope(|ui| {
                            ui.set_max_width(PANEL_BODY_W);
                            https_section(ui, app_ref, false);
                            width = ui.min_rect().width();
                        });
                    });
                });
            }
            let _ = &mut app_ref;
            assert!(
                width <= PANEL_BODY_W + 0.5,
                "tls={tls} cert={cert:?}: {width}"
            );
        }
    }
}
