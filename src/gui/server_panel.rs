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
