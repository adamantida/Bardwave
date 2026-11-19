//! `02 / NETWORK` — the broadcast server column.
//!
//! Fixed 340px column, mirroring the mockup:
//!
//! ```text
//! 02 / NETWORK
//! NETWORK SERVER
//! ● LIVE
//! BROADCASTING TO 3 LISTENERS
//! ─────────────────────────────
//! LISTEN URL   [ http://192.168.1.245:8080  ⧉ ]
//!              LOCAL http://127.0.0.1:8080
//!              INTERNET http://203.0.113.7:8080
//! LISTENERS    03  ▮▮▮▯▯▯▯▯   PEAK 3
//! ─────────────────────────────
//! [ ■ STOP SERVER ] [ ⧉ COPY LINK ]
//! ─────────────────────────────
//! CONFIGURATION  PORT [8080]   BIND [0.0.0.0]
//! WS / 48 kHz / STEREO
//! ```
//!
//! The two CONFIGURATION wells are real text inputs: a valid edit updates the
//! config, `config.toml` and the server thread without restarting the app.

use crate::gui::app::{ServerStatus as GuiServerStatus, SoundSharedApp};
use crate::gui::theme::{
    console_panel, hairline, micro_label, micro_label_accent, panel_title, telemetry, Metrics,
    Palette,
};
use crate::gui::widgets::{glyph, occupancy_cells, signal_dot, ConsoleButton};
use egui::{Align, Layout, RichText, Sense, Ui, Vec2};
use qrcode::{Color as QrColor, EcLevel, QrCode};

/// Renders the network server panel at the given size.
pub fn server_panel(ui: &mut Ui, state: &mut SoundSharedApp, size: Vec2) {
    console_panel(ui, size, |ui| {
        let server = state.get_server_state();

        // ---- Header ----
        ui.label(micro_label_accent("02 / Network"));
        ui.add_space(2.0);
        ui.label(panel_title("Network Server"));
        ui.add_space(Metrics::LG);

        // ---- Big status ----
        let (status_color, headline, subline, pulse) = match &server.status {
            GuiServerStatus::Running => (
                Palette::ROSE_500,
                "Live".to_string(),
                format!(
                    "Broadcasting to {} listener{}",
                    server.listener_count,
                    if server.listener_count == 1 { "" } else { "s" }
                ),
                true,
            ),
            GuiServerStatus::Starting => (
                Palette::AMBER_400,
                "Starting".to_string(),
                "Binding the listen socket".to_string(),
                true,
            ),
            GuiServerStatus::Error(message) => (
                Palette::ROSE_400,
                "Error".to_string(),
                crate::gui::widgets::elide(message, 34),
                false,
            ),
            GuiServerStatus::Stopped => (
                Palette::ZINC_500,
                "Off Air".to_string(),
                "Server stopped".to_string(),
                false,
            ),
        };

        ui.horizontal(|ui| {
            signal_dot(ui, status_color, 8.0, pulse && !state.reduced_motion());
            ui.add_space(Metrics::SM);
            ui.label(
                RichText::new(headline.to_uppercase())
                    .font(crate::gui::widgets::glyph_font(24.0))
                    .color(status_color)
                    .strong(),
            );
        });
        ui.add_space(Metrics::XS);
        ui.label(micro_label(subline));

        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::LG);

        // ---- Listen URL ----
        ui.label(micro_label("Listen URL"));
        ui.add_space(Metrics::SM);

        let url = server.bind_url.clone().unwrap_or_else(|| "—".to_string());
        let running = matches!(server.status, GuiServerStatus::Running);
        // What players actually need: the address plus the access key.
        let share = if running {
            state.share_url(&url)
        } else {
            url.clone()
        };

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let well_w = (ui.available_width() - 34.0 - Metrics::XS).max(60.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(well_w, 40.0), Sense::hover());
            let painter = ui.painter();
            painter.rect(
                rect,
                egui::Rounding::same(Metrics::R_SM),
                Palette::WELL,
                egui::Stroke::new(1.0_f32, Palette::BORDER),
            );
            let color = if running {
                Palette::ROSE_500
            } else {
                Palette::ZINC_600
            };
            let galley = painter.layout_no_wrap(
                crate::gui::widgets::elide(&share, 30),
                crate::gui::widgets::glyph_font(12.0),
                color,
            );
            painter.galley(
                egui::pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
                galley,
                color,
            );

            ui.add_space(Metrics::XS);
            let copy = ConsoleButton::ghost("")
                .glyph(glyph::COPY)
                .width(30.0)
                .height(40.0)
                .enabled(running)
                .tooltip("Copy listen URL")
                .id_salt("copy_url_icon")
                .show(ui);
            if copy.clicked() && running {
                ui.output_mut(|o| o.copied_text = share.clone());
                state.set_copied_feedback(true);
            }
        });

        if state.copied_feedback() {
            ui.add_space(Metrics::XS);
            ui.label(
                RichText::new("COPIED TO CLIPBOARD")
                    .font(crate::gui::widgets::glyph_font(9.0))
                    .color(Palette::ROSE_400)
                    .extra_letter_spacing(1.0),
            );
        }

        // The URL above carries the LAN address, which is what the players
        // open. This machine always has loopback, and showing it means a wrong
        // or blocked address detection can never hide the way in.
        let advertised_is_loopback =
            url.starts_with("http://127.0.0.1") || url.starts_with("http://[::1]");
        if running && !advertised_is_loopback {
            if let Some(local) = server.local_url.as_deref() {
                ui.add_space(Metrics::XS);
                let label = ui.add(
                    egui::Label::new(micro_label(format!("Local {local}"))).sense(Sense::click()),
                );
                if label
                    .on_hover_text("Click to copy the link for this PC (with the access key)")
                    .clicked()
                {
                    ui.output_mut(|o| o.copied_text = state.share_url(local));
                    state.set_copied_feedback(true);
                }
            }
        }

        // Router port forwarding: players outside the LAN use this URL.
        if running {
            use crate::network::upnp::UpnpStatus;
            let line = match &server.upnp {
                UpnpStatus::Off => None,
                UpnpStatus::Searching => Some("Internet: asking the router…".to_string()),
                UpnpStatus::Mapped { url, .. } => Some(format!("Internet {url}")),
                UpnpStatus::DoubleNat { external } => {
                    Some(format!("Internet: blocked by a second NAT ({external})"))
                }
                UpnpStatus::Unavailable(_) => Some("Internet: no UPnP router".to_string()),
            };
            if let Some(line) = line {
                ui.add_space(Metrics::XS);
                let label = ui.add(egui::Label::new(micro_label(line)).sense(Sense::click()));
                match &server.upnp {
                    UpnpStatus::Mapped { url, .. } => {
                        if label
                            .on_hover_text(
                                "Click to copy the internet URL.\n\nOpening it from inside your own \
                                 network may fail on routers without NAT loopback - test it from \
                                 a phone on mobile data instead.",
                            )
                            .clicked()
                        {
                            ui.output_mut(|o| o.copied_text = state.share_url(url));
                            state.set_copied_feedback(true);
                        }
                    }
                    UpnpStatus::Unavailable(reason) => {
                        label.on_hover_text(reason.as_str());
                    }
                    UpnpStatus::DoubleNat { .. } => {
                        label.on_hover_text(
                            "The router's external address is private (CGNAT or a modem in \
                             front of it): forward the port on the upstream device or ask the \
                             provider for a public IP.",
                        );
                    }
                    _ => {}
                }
            }
        }

        // What keeps players out even though the server runs: a VPN taking
        // the default route, a firewall rule that could not be added.
        if running {
            for note in &server.notes {
                ui.add_space(Metrics::XS);
                ui.add(
                    egui::Label::new(
                        RichText::new(note.to_uppercase())
                            .font(crate::gui::widgets::glyph_font(Metrics::MICRO_TINY))
                            .color(Palette::AMBER_400)
                            .extra_letter_spacing(0.6),
                    )
                    .wrap(),
                );
            }
        }

        // ---- Access key ----
        // Players need it in the link; a new one revokes the old link and
        // disconnects everyone who used it (e.g. after it leaked).
        ui.add_space(Metrics::SM);
        ui.horizontal(|ui| {
            // The key itself stays lowercase (it is case-sensitive); only the
            // label follows the console's uppercase style.
            let (text, color) = match state.access_key() {
                Some(key) => (
                    format!("ACCESS KEY {}", crate::gui::widgets::elide(&key, 6)),
                    Palette::ZINC_500,
                ),
                None => (
                    "ACCESS KEY OFF - ANYONE CAN LISTEN".to_string(),
                    Palette::AMBER_400,
                ),
            };
            ui.label(
                RichText::new(text)
                    .font(crate::gui::widgets::glyph_font(Metrics::MICRO))
                    .color(color)
                    .extra_letter_spacing(0.8),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::gui::widgets::IconButton::new(glyph::REFRESH)
                    .size(20.0)
                    .font_size(10.0)
                    .tooltip(
                        "New access key: the current link stops working and everyone \
                         listening with it is disconnected. Share the new link afterwards.",
                    )
                    .id_salt("server_new_key")
                    .show(ui)
                    .clicked()
                {
                    state.regenerate_access_key();
                }
            });
        });

        // ---- Game-master panel link ----
        // Next to the players' link, but a different secret: it opens the
        // controls, so it is shown masked and never offered to players.
        let gm_base = if running {
            server.bind_url.clone()
        } else {
            None
        };
        gm_link_block(ui, state, gm_base.as_deref());

        ui.add_space(Metrics::LG);

        // ---- Listener occupancy ----
        ui.label(micro_label("Listeners"));
        ui.add_space(Metrics::SM);
        ui.horizontal(|ui| {
            ui.label(telemetry(
                format!("{:02}", server.listener_count),
                30.0,
                Palette::WHITE,
            ));
            ui.add_space(Metrics::MD);
            occupancy_cells(ui, server.listener_count.min(8), 8, 8.0, 6.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(micro_label(format!("Peak {}", server.peak_listeners)));
            });
        });

        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::LG);

        // ---- Controls ----
        // Both buttons live on one line, so each needs an explicit width: with no
        // width a `ConsoleButton` claims the whole row and pushes its neighbour
        // outside the panel.
        let copy_w = 124.0;
        let primary_w = (ui.available_width() - copy_w - Metrics::SM).max(96.0);
        ui.horizontal(|ui| {
            // Widths above already include the one explicit gap.
            ui.spacing_mut().item_spacing.x = 0.0;
            match &server.status {
                GuiServerStatus::Stopped | GuiServerStatus::Error(_) => {
                    let label = if matches!(server.status, GuiServerStatus::Error(_)) {
                        "Retry Server"
                    } else {
                        "Start Server"
                    };
                    if ConsoleButton::primary(label)
                        .glyph(glyph::BROADCAST)
                        .width(primary_w)
                        .height(40.0)
                        .id_salt("server_start")
                        .show(ui)
                        .clicked()
                    {
                        state.request_start_server();
                    }
                }
                GuiServerStatus::Starting => {
                    let _ = ConsoleButton::primary("Starting")
                        .glyph(glyph::REFRESH)
                        .width(primary_w)
                        .height(40.0)
                        .enabled(false)
                        .id_salt("server_starting")
                        .show(ui);
                }
                GuiServerStatus::Running => {
                    if ConsoleButton::danger("Stop Server")
                        .glyph(glyph::STOP)
                        .width(primary_w)
                        .height(40.0)
                        .id_salt("server_stop")
                        .show(ui)
                        .clicked()
                    {
                        state.request_stop_server();
                    }
                }
            }

            ui.add_space(Metrics::SM);
            if ConsoleButton::ghost("Copy Link")
                .glyph(glyph::COPY)
                .width(copy_w)
                .height(40.0)
                .enabled(running)
                .id_salt("server_copy")
                .show(ui)
                .clicked()
                && running
            {
                ui.output_mut(|o| o.copied_text = share.clone());
                state.set_copied_feedback(true);
            }
        });

        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::LG);

        // ---- Mix: bus faders, duck, loudness, hotkeys ----
        crate::gui::settings_extras::mix_section(ui, state);

        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::LG);

        // ---- Configuration ----
        ui.label(micro_label("Configuration"));
        ui.add_space(Metrics::SM);

        // The bind address and port can only be changed while the socket is
        // down: the running server captured its address at `Start`, and
        // `ServerCommand::SetBind` refuses edits in that state.
        let config_enabled = !matches!(
            server.status,
            GuiServerStatus::Running | GuiServerStatus::Starting
        );

        let mut port_edited = false;
        let mut bind_edited = false;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(micro_label("Port"));
                ui.add_space(2.0);
                let response = config_input(
                    ui,
                    state.port_input_mut(),
                    config_enabled,
                    PORT_WELL_W,
                    crate::gui::app::PORT_INPUT_MAX_CHARS,
                    "server_port_input",
                );
                port_edited = response.changed();
            });
            ui.add_space(Metrics::SM);
            ui.vertical(|ui| {
                ui.label(micro_label("Bind Address"));
                ui.add_space(2.0);
                let response = config_input(
                    ui,
                    state.bind_input_mut(),
                    config_enabled,
                    BIND_WELL_W,
                    crate::gui::app::BIND_INPUT_MAX_CHARS,
                    "server_bind_input",
                );
                bind_edited = response.changed();
            });
        });

        // Keystroke → validation → commit. Invalid text leaves the last valid
        // host/port in force and only raises the inline error below.
        if port_edited {
            state.on_port_edited();
        }
        if bind_edited {
            state.on_bind_edited();
        }

        if let Some(error) = state.config_error() {
            ui.add_space(Metrics::XS);
            ui.label(
                RichText::new(error.to_uppercase())
                    .font(crate::gui::widgets::glyph_font(9.0))
                    .color(Palette::ROSE_400)
                    .extra_letter_spacing(0.8),
            );
        }

        ui.add_space(Metrics::XS);
        ui.horizontal(|ui| {
            ui.label(micro_label(if config_enabled {
                "Editable while stopped"
            } else {
                "Stop the server to edit"
            }));
            if state.config_saved() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new("Saved")
                            .font(crate::gui::widgets::glyph_font(Metrics::MICRO_TINY))
                            .color(Palette::ROSE_400)
                            .extra_letter_spacing(1.0),
                    );
                });
            }
        });

        ui.add_space(Metrics::LG);
        hairline(ui);
        ui.add_space(Metrics::MD);
        ui.label(micro_label("WS / 48 kHz / Stereo"));
    });
}

/// Width of the `PORT` well, and of the `BIND ADDRESS` well next to it.
///
/// Together with the 8px gaps they fill the 292px panel body exactly the way the
/// read-only pair did.
const PORT_WELL_W: f32 = 80.0;
const BIND_WELL_W: f32 = 120.0;

/// Height of the two CONFIGURATION wells.
const CONFIG_WELL_H: f32 = 36.0;

/// Space the well leaves between its border and the text, per side.
const CONFIG_WELL_PAD_X: f32 = 12.0;

/// Editable CONFIGURATION field: the console's inset well around a real
/// `TextEdit`, so PORT and BIND ADDRESS can actually be typed into.
///
/// Painted with the same tokens as the read-only wells it replaces (`Palette::WELL`,
/// `Palette::BORDER`, `Metrics::R_SM`, 12px mono text). A disabled field is
/// greyed out and stops accepting clicks and keystrokes; the caller keeps
/// showing the hint line that explains why.
fn config_input(
    ui: &mut Ui,
    value: &mut String,
    enabled: bool,
    width: f32,
    max_chars: usize,
    id_salt: &str,
) -> egui::Response {
    let font = crate::gui::widgets::glyph_font(12.0);
    // The field is fixed-height, so the text is centred by padding it instead of
    // by an extra layout: a `TextEdit` sizes itself to its row height.
    let row_h = ui.fonts(|fonts| fonts.row_height(&font));
    let pad_y = ((CONFIG_WELL_H - row_h) / 2.0).max(0.0);
    let inner_w = (width - 2.0 * CONFIG_WELL_PAD_X).max(24.0);
    let text_color = if enabled {
        Palette::WHITE
    } else {
        Palette::ZINC_600
    };

    egui::Frame::none()
        .fill(Palette::WELL)
        .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER))
        .rounding(egui::Rounding::same(Metrics::R_SM))
        .inner_margin(egui::Margin::symmetric(CONFIG_WELL_PAD_X, 0.0))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(value)
                    .id_salt(id_salt)
                    .frame(false)
                    .interactive(enabled)
                    .font(font)
                    .text_color(text_color)
                    .desired_width(inner_w)
                    .char_limit(max_chars)
                    .margin(egui::Margin::symmetric(0.0, pad_y)),
            )
        })
        .inner
}

// ---------------------------------------------------------------------------
// Game-master panel link
// ---------------------------------------------------------------------------

/// Quiet zone around a QR code, in modules (the standard asks for four).
const QR_QUIET_MODULES: usize = 4;

/// Side of the QR code on screen, in points, before rounding to whole points
/// per module.
const QR_TARGET_PX: usize = 176;

/// State of the game-master block: the QR toggle, the "replace key?" question
/// and the QR texture.
#[derive(Default)]
pub struct GmLinkUi {
    /// The QR code is hidden until asked for: the link is a secret and the
    /// screen may be in view of the players.
    qr_visible: bool,
    /// Waiting for the operator to confirm a new key.
    confirm_regen: bool,
    qr: Option<QrTexture>,
}

/// The QR code of one link, uploaded as a texture.
struct QrTexture {
    link: String,
    handle: egui::TextureHandle,
    /// Side on screen, in points.
    size: f32,
}

/// The link with the key cut short (`.../control?g=ab12...`), safe to show.
pub fn masked_gm_link(link: &str, token: &str) -> String {
    const SHOWN: usize = 4;
    if token.is_empty() {
        return link.to_string();
    }
    let hidden: String = token.chars().take(SHOWN).collect::<String>() + "…";
    link.replace(token, &hidden)
}

/// A QR code as a grid of modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrMatrix {
    /// Modules per side (without the quiet zone).
    pub width: usize,
    /// Row-major, `true` = dark.
    pub dark: Vec<bool>,
}

impl QrMatrix {
    /// The module at column `x`, row `y` (light outside the grid).
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.width && self.dark[y * self.width + x]
    }
}

/// Encode `text`. Medium error correction: a phone camera reading a screen
/// does not need more, and a lower version keeps the modules big.
pub fn qr_matrix(text: &str) -> Result<QrMatrix, String> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)
        .map_err(|e| e.to_string())?;
    Ok(QrMatrix {
        width: code.width(),
        dark: code
            .to_colors()
            .into_iter()
            .map(|c| c == QrColor::Dark)
            .collect(),
    })
}

/// One pixel per module plus the quiet zone, black on white. Drawn scaled with
/// nearest-neighbour sampling.
pub fn qr_image(matrix: &QrMatrix, quiet: usize) -> egui::ColorImage {
    let side = matrix.width + 2 * quiet;
    let mut pixels = vec![egui::Color32::WHITE; side * side];
    for y in 0..matrix.width {
        for x in 0..matrix.width {
            if matrix.is_dark(x, y) {
                pixels[(y + quiet) * side + x + quiet] = egui::Color32::BLACK;
            }
        }
    }
    egui::ColorImage {
        size: [side, side],
        pixels,
    }
}

/// On-screen side for an image of `modules` pixels: a whole number of points
/// per module (so modules stay crisp), as close to `target` as it gets.
pub fn qr_display_size(modules: usize, target: usize) -> f32 {
    let scale = (target / modules.max(1)).max(1);
    (modules * scale) as f32
}

impl SoundSharedApp {
    /// The game-master link for the server at `base_url`, `None` while there
    /// is no token.
    pub fn gm_link(&self, base_url: &str) -> Option<String> {
        self.app_state.gm.share_url(base_url)
    }

    /// Issue a new game-master key. The old link stops working at once and
    /// every phone connected with it is disconnected; the players' link is
    /// not touched. Saved with the next config flush.
    pub fn regenerate_gm_token(&mut self) {
        let key = crate::network::generate_key();
        self.app_state.gm.set(Some(key.clone()));
        self.app_state.config.lock().unwrap().server.gm_token = Some(key);
        self.library_dirty = true;
        self.extras.gm.qr = None;
        self.extras.gm.confirm_regen = false;
        tracing::info!("Issued a new game-master key");
    }
}

/// The inset well the links are shown in; returns whether it was clicked.
fn link_well(ui: &mut Ui, text: &str, active: bool, hover: &str) -> bool {
    let well_w = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(well_w, 34.0), Sense::click());
    let painter = ui.painter();
    painter.rect(
        rect,
        egui::Rounding::same(Metrics::R_SM),
        Palette::WELL,
        egui::Stroke::new(1.0_f32, Palette::BORDER),
    );
    let color = if active {
        Palette::ROSE_500
    } else {
        Palette::ZINC_600
    };
    let galley = painter.layout_no_wrap(
        crate::gui::widgets::elide(text, 38),
        crate::gui::widgets::glyph_font(11.0),
        color,
    );
    painter.galley(
        egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    crate::gui::widgets::paint_icon(
        painter,
        glyph::COPY,
        egui::pos2(rect.right() - 16.0, rect.center().y),
        14.0,
        if active {
            Palette::ZINC_400
        } else {
            Palette::ZINC_600
        },
    );
    let response = if active {
        response
            .on_hover_text(hover)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    };
    active && response.clicked()
}

/// One small caution line.
fn caution(ui: &mut Ui, text: &str, color: egui::Color32) {
    ui.add(
        egui::Label::new(
            RichText::new(text.to_uppercase())
                .font(crate::gui::widgets::glyph_font(Metrics::MICRO_TINY))
                .color(color)
                .extra_letter_spacing(0.6),
        )
        .wrap(),
    );
}

/// "GM PANEL LINK": the link to the remote control panel (with its own key), a
/// QR code for the phone and the control to replace the key. `base_url` is the
/// address of the running server, `None` while it is stopped.
fn gm_link_block(ui: &mut Ui, state: &mut SoundSharedApp, base_url: Option<&str>) {
    ui.add_space(Metrics::MD);
    ui.label(micro_label("GM panel link"));
    ui.add_space(Metrics::SM);

    let token = state.app_state.gm.current();
    let Some(token) = token else {
        // `gm_token = ""`: only this computer may open the panel.
        caution(
            ui,
            "Remote panel is off: it opens on this computer only",
            Palette::ZINC_500,
        );
        ui.add_space(Metrics::SM);
        if ConsoleButton::ghost("Create GM key")
            .glyph(glyph::PLUS)
            .height(34.0)
            .tooltip("Make a key so the panel can be opened from a phone.")
            .id_salt("gm_create_key")
            .show(ui)
            .clicked()
        {
            state.regenerate_gm_token();
        }
        return;
    };

    let link = base_url.and_then(|base| state.gm_link(base));
    let shown = match &link {
        // Without the scheme the whole masked key fits the well.
        Some(link) => masked_gm_link(link, &token)
            .trim_start_matches("http://")
            .to_string(),
        None => "Start the server to get the link".to_string(),
    };
    if link_well(
        ui,
        &shown,
        link.is_some(),
        "Click to copy the game-master link (with its key)",
    ) {
        if let Some(link) = &link {
            ui.output_mut(|o| o.copied_text = link.clone());
            state.set_copied_feedback(true);
        }
    }

    ui.add_space(Metrics::XS);
    caution(
        ui,
        "Do not share with players: this link controls the console",
        Palette::AMBER_400,
    );
    if !state.app_state.gm_panel_served {
        ui.add_space(2.0);
        caution(
            ui,
            "This window does not serve the phone panel yet (it runs with --server-only)",
            Palette::ZINC_500,
        );
    }

    ui.add_space(Metrics::SM);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Metrics::SM;
        let qr_label = if state.extras.gm.qr_visible {
            "Hide QR"
        } else {
            "Show QR"
        };
        if ConsoleButton::ghost(qr_label)
            .small()
            .width(110.0)
            .enabled(link.is_some())
            .tooltip(
                "A QR code of the link for a phone camera. Anyone who sees it can control \
                 the console.",
            )
            .id_salt("gm_qr_toggle")
            .show(ui)
            .clicked()
        {
            let gm = &mut state.extras.gm;
            gm.qr_visible = !gm.qr_visible;
            if !gm.qr_visible {
                gm.qr = None;
            }
        }
        if ConsoleButton::ghost("New key")
            .small()
            .glyph(glyph::REFRESH)
            .width(110.0)
            .tooltip("Replace the game-master key (asks first)")
            .id_salt("gm_new_key")
            .show(ui)
            .clicked()
        {
            state.extras.gm.confirm_regen = true;
        }
    });

    if state.extras.gm.confirm_regen {
        ui.add_space(Metrics::SM);
        caution(
            ui,
            "The current link stops working at once and every phone using it is \
             disconnected. Players' links are not affected.",
            Palette::AMBER_400,
        );
        ui.add_space(Metrics::SM);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Metrics::SM;
            if ConsoleButton::danger("Replace key")
                .small()
                .width(110.0)
                .id_salt("gm_new_key_yes")
                .show(ui)
                .clicked()
            {
                state.regenerate_gm_token();
            }
            if ConsoleButton::ghost("Cancel")
                .small()
                .width(110.0)
                .id_salt("gm_new_key_no")
                .show(ui)
                .clicked()
            {
                state.extras.gm.confirm_regen = false;
            }
        });
    }

    // The QR code, only while asked for and only for the current link.
    match (&link, state.extras.gm.qr_visible) {
        (Some(link), true) => {
            let stale = state
                .extras
                .gm
                .qr
                .as_ref()
                .is_none_or(|qr| qr.link != *link);
            if stale {
                state.extras.gm.qr = qr_matrix(link).ok().map(|matrix| {
                    let image = qr_image(&matrix, QR_QUIET_MODULES);
                    let modules = image.size[0];
                    QrTexture {
                        link: link.clone(),
                        handle: ui.ctx().load_texture(
                            "gm_link_qr",
                            image,
                            egui::TextureOptions::NEAREST,
                        ),
                        size: qr_display_size(modules, QR_TARGET_PX),
                    }
                });
            }
            ui.add_space(Metrics::SM);
            match &state.extras.gm.qr {
                Some(qr) => {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Image::new((qr.handle.id(), Vec2::splat(qr.size))));
                    });
                }
                None => caution(ui, "The link is too long for a QR code", Palette::ROSE_400),
            }
        }
        _ => state.extras.gm.qr = None,
    }
}

#[cfg(test)]
mod gm_tests {
    use super::*;

    #[test]
    fn the_link_is_masked_but_keeps_its_shape() {
        let link = "http://192.168.1.5:8080/control?g=abcd1234efgh5678";
        assert_eq!(
            masked_gm_link(link, "abcd1234efgh5678"),
            "http://192.168.1.5:8080/control?g=abcd…"
        );
        // No token: nothing to hide, nothing changes.
        assert_eq!(masked_gm_link("http://x/control", ""), "http://x/control");
        assert!(!masked_gm_link(link, "abcd1234efgh5678").contains("efgh"));
    }

    #[test]
    fn qr_codes_have_the_standard_geometry() {
        let m = qr_matrix("http://192.168.1.5:8080/control?g=abcd1234efgh5678").unwrap();
        // Version v has 4v + 17 modules per side.
        assert!(m.width >= 21 && (m.width - 17) & 3 == 0, "{}", m.width);
        assert_eq!(m.dark.len(), m.width * m.width);
        // Finder pattern: a 7x7 dark ring with a 3x3 dark centre and a light
        // ring between them, in three corners.
        let finder = |ox: usize, oy: usize| {
            (0..7).all(|i| {
                m.is_dark(ox + i, oy)
                    && m.is_dark(ox + i, oy + 6)
                    && m.is_dark(ox, oy + i)
                    && m.is_dark(ox + 6, oy + i)
            }) && (2..5).all(|y| (2..5).all(|x| m.is_dark(ox + x, oy + y)))
                && !m.is_dark(ox + 1, oy + 1)
        };
        assert!(finder(0, 0));
        assert!(finder(m.width - 7, 0));
        assert!(finder(0, m.width - 7));
        // No finder pattern in the fourth corner.
        assert!(!finder(m.width - 7, m.width - 7));
    }

    #[test]
    fn qr_is_deterministic_and_depends_on_the_text() {
        let a = qr_matrix("http://10.0.0.2:8080/control?g=aaaaaaaaaaaaaaaa").unwrap();
        let b = qr_matrix("http://10.0.0.2:8080/control?g=aaaaaaaaaaaaaaaa").unwrap();
        let c = qr_matrix("http://10.0.0.2:8080/control?g=bbbbbbbbbbbbbbbb").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn oversized_text_is_an_error_not_a_panic() {
        assert!(qr_matrix(&"x".repeat(10_000)).is_err());
    }

    #[test]
    fn the_image_adds_a_white_quiet_zone() {
        let m = qr_matrix("http://10.0.0.2:8080/control?g=aaaaaaaaaaaaaaaa").unwrap();
        let image = qr_image(&m, QR_QUIET_MODULES);
        let side = m.width + 2 * QR_QUIET_MODULES;
        assert_eq!(image.size, [side, side]);
        assert_eq!(image.pixels.len(), side * side);
        // Border is white, the top-left finder corner is black.
        assert!(image.pixels[..side]
            .iter()
            .all(|p| *p == egui::Color32::WHITE));
        let q = QR_QUIET_MODULES;
        assert_eq!(image.pixels[q * side + q], egui::Color32::BLACK);
        // Dark module count is preserved.
        let dark = image
            .pixels
            .iter()
            .filter(|p| **p == egui::Color32::BLACK)
            .count();
        assert_eq!(dark, m.dark.iter().filter(|d| **d).count());
    }

    #[test]
    fn display_size_is_a_whole_number_of_points_per_module() {
        // 33 modules + 8 quiet = 41; 176 / 41 = 4 points per module.
        assert_eq!(qr_display_size(41, 176), 164.0);
        assert_eq!(qr_display_size(29, 176), 174.0);
        // Never below one point per module.
        assert_eq!(qr_display_size(500, 176), 500.0);
    }
}

#[cfg(test)]
mod gm_ui_tests {
    use super::*;
    use crate::config::Config;
    use crate::gui::hotkeys_ui::test_support::*;

    const TOKEN: &str = "abcd1234efgh5678";
    const BASE: &str = "http://192.168.1.5:8080";

    fn harness_with_token(token: Option<&str>) -> Harness {
        let mut config = Config::default();
        config.server.gm_token = token.map(str::to_string);
        harness(config, false)
    }

    fn draw(h: &mut Harness, base: Option<&str>) -> String {
        settle(&h.ctx, 2, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| gm_link_block(ui, &mut h.app, base));
        })
    }

    #[test]
    fn the_link_is_built_from_the_server_address_and_the_gm_token() {
        let h = harness_with_token(Some(TOKEN));
        assert_eq!(
            h.app.gm_link(BASE).as_deref(),
            Some("http://192.168.1.5:8080/control?g=abcd1234efgh5678")
        );
        let none = harness_with_token(None);
        assert_eq!(none.app.gm_link(BASE), None);
    }

    #[test]
    fn the_block_shows_a_masked_link_and_warns_against_sharing_it() {
        let mut h = harness_with_token(Some(TOKEN));
        let text = draw(&mut h, Some(BASE));
        assert!(text.contains("GM PANEL LINK"), "{text}");
        assert!(text.contains("192.168.1.5:8080/CONTROL?G=ABCD"), "{text}");
        assert!(
            !text.contains("EFGH5678"),
            "the key must not be shown: {text}"
        );
        assert!(text.contains("DO NOT SHARE WITH PLAYERS"), "{text}");
        assert!(text.contains("SHOW QR"), "{text}");
        assert!(text.contains("NEW KEY"), "{text}");
        // The QR code is not drawn until asked for.
        assert!(h.app.extras.gm.qr.is_none());
    }

    #[test]
    fn a_stopped_server_has_no_link() {
        let mut h = harness_with_token(Some(TOKEN));
        let text = draw(&mut h, None);
        assert!(text.contains("START THE SERVER TO GET THE LINK"), "{text}");
        h.app.extras.gm.qr_visible = true;
        draw(&mut h, None);
        assert!(h.app.extras.gm.qr.is_none());
    }

    #[test]
    fn the_qr_code_follows_the_toggle_and_the_current_link() {
        let mut h = harness_with_token(Some(TOKEN));
        h.app.extras.gm.qr_visible = true;
        draw(&mut h, Some(BASE));
        let first = h.app.extras.gm.qr.as_ref().expect("a QR texture");
        assert_eq!(
            first.link,
            "http://192.168.1.5:8080/control?g=abcd1234efgh5678"
        );
        assert!(first.size >= 100.0);

        // A new key: the old image is gone and the next frame draws the new one.
        h.app.regenerate_gm_token();
        assert!(h.app.extras.gm.qr.is_none());
        draw(&mut h, Some(BASE));
        let second = h.app.extras.gm.qr.as_ref().expect("a QR texture");
        assert_ne!(second.link, first_link_for_compare());
        assert!(second.link.contains(&h.gm.current().unwrap()));

        // Hiding drops the texture.
        h.app.extras.gm.qr_visible = false;
        draw(&mut h, Some(BASE));
        assert!(h.app.extras.gm.qr.is_none());
    }

    fn first_link_for_compare() -> String {
        format!("{BASE}/control?g={TOKEN}")
    }

    #[test]
    fn a_new_key_revokes_the_old_one_at_once_and_is_saved() {
        let mut h = harness_with_token(Some(TOKEN));
        let listener_key = h.app.access_key();
        assert!(h.gm.allows(Some(TOKEN)));
        h.app.regenerate_gm_token();
        let new = h.gm.current().expect("a token");
        assert_ne!(new, TOKEN);
        assert!(!h.gm.allows(Some(TOKEN)), "the old key must stop working");
        assert!(h.gm.allows(Some(&new)));
        assert_eq!(
            h.config.lock().unwrap().server.gm_token.as_deref(),
            Some(new.as_str())
        );
        assert!(h.app.library_dirty, "the new key must reach config.toml");
        // The players' key is a different secret and is not touched.
        assert_eq!(h.app.access_key(), listener_key);
    }

    #[test]
    fn replacing_the_key_asks_first_and_says_what_happens() {
        let mut h = harness_with_token(Some(TOKEN));
        h.app.extras.gm.confirm_regen = true;
        let text = draw(&mut h, Some(BASE));
        assert!(text.contains("STOPS WORKING AT ONCE"), "{text}");
        assert!(text.contains("REPLACE KEY"), "{text}");
        assert!(text.contains("CANCEL"), "{text}");
        // Nothing changed yet.
        assert_eq!(h.gm.current().as_deref(), Some(TOKEN));
    }

    #[test]
    fn without_a_token_the_panel_is_local_only_and_a_key_can_be_created() {
        let mut h = harness_with_token(None);
        let text = draw(&mut h, Some(BASE));
        assert!(text.contains("REMOTE PANEL IS OFF"), "{text}");
        assert!(text.contains("CREATE GM KEY"), "{text}");
        assert!(!text.contains("SHOW QR"), "{text}");
        h.app.regenerate_gm_token();
        assert!(h.gm.remote_enabled());
    }

    #[test]
    fn the_block_says_when_this_window_does_not_serve_the_panel() {
        let mut h = harness_with_token(Some(TOKEN));
        assert!(!draw(&mut h, Some(BASE)).contains("SERVER-ONLY"));
        h.app.app_state.gm_panel_served = false;
        let text = draw(&mut h, Some(BASE));
        assert!(text.contains("--SERVER-ONLY"), "{text}");
    }

    #[test]
    fn the_whole_column_renders_with_the_new_sections() {
        let mut h = harness_with_token(Some(TOKEN));
        let text = settle(&h.ctx, 2, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                server_panel(ui, &mut h.app, Vec2::new(340.0, 2000.0));
            });
        });
        for expected in [
            "NETWORK SERVER",
            "ACCESS KEY",
            "GM PANEL LINK",
            "LISTENERS",
            "MIX",
            "NORMALIZE LOUDNESS",
            "CONFIGURATION",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
    }
}
