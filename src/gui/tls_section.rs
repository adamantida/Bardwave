//! HTTPS switch and certificate fingerprint for the NETWORK column.
//!
//! A small block under the CONFIGURATION wells: the `server.tls` checkbox, and
//! with it on, the SHA-256 fingerprint of the certificate (so the game master
//! can compare it with the one a phone shows next to the self-signed warning)
//! and a button that makes a new self-signed certificate. All file work runs
//! off the frame; the section only reads a cached copy of it.

use std::time::{Duration, Instant};

use egui::{RichText, Sense, Ui};

use crate::gui::app::SoundSharedApp;
use crate::gui::theme::{micro_label, Metrics, Palette};
use crate::gui::widgets::{glyph_font, ConsoleButton};
use crate::network::tls::{self, CertSource, SanSet, TlsInfo, TlsSettings};

/// How often the stored certificate is looked at again.
const REFRESH: Duration = Duration::from_secs(2);

/// Cached look at the certificate files (kept in egui's temp memory).
#[derive(Clone)]
struct Cached {
    at: Instant,
    info: Option<TlsInfo>,
}

/// Draw the HTTPS block. `editable` is false while the server runs: the
/// switch is applied on the next Start, so it is read-only in the meantime.
pub fn tls_section(ui: &mut Ui, state: &mut SoundSharedApp, editable: bool) {
    let settings = TlsSettings::from_config(&state.app_state.config.lock().unwrap().server);

    let mut on = settings.enabled;
    let toggled = ui
        .add_enabled_ui(editable, |ui| {
            ui.checkbox(
                &mut on,
                RichText::new("HTTPS (ENCRYPTED LINK)")
                    .font(glyph_font(Metrics::MICRO))
                    .color(Palette::ZINC_300)
                    .extra_letter_spacing(0.8),
            )
            .on_hover_text(
                "Serve https:// and wss:// on the same port so the game-master key is not sent \
                 in clear text over the internet. Applies on the next Start. Use your own \
                 certificate with tls_cert / tls_key in config.toml, or a self-signed one is made.",
            )
            .changed()
        })
        .inner;
    if toggled {
        state.app_state.config.lock().unwrap().server.tls = on;
        state.library_dirty = true;
    }
    if !settings.enabled {
        return;
    }

    let id = egui::Id::new("tls_section_cache");
    let cached: Option<Cached> = ui.ctx().memory(|m| m.data.get_temp(id));
    let info = tls::active_info().or_else(|| match cached {
        Some(c) if c.at.elapsed() < REFRESH => c.info,
        _ => {
            let info = tls::stored_info(&settings.cert_dir());
            ui.ctx().memory_mut(|m| {
                m.data.insert_temp(
                    id,
                    Cached {
                        at: Instant::now(),
                        info: info.clone(),
                    },
                )
            });
            info
        }
    });

    ui.add_space(Metrics::SM);
    match info {
        Some(info) => {
            let kind = match info.source {
                CertSource::Custom => "Your certificate",
                CertSource::SelfSigned => "Self-signed certificate",
            };
            ui.label(micro_label(format!("{kind} / SHA-256")));
            ui.add_space(Metrics::XS);
            let fingerprint = ui.add(
                egui::Label::new(
                    RichText::new(&info.fingerprint_sha256)
                        .font(glyph_font(Metrics::MICRO_TINY))
                        .color(Palette::ZINC_300)
                        .line_height(Some(Metrics::MICRO_TINY * 1.6)),
                )
                .wrap()
                .sense(Sense::click()),
            );
            if fingerprint
                .on_hover_text("Click to copy. Compare it with the fingerprint your browser shows.")
                .clicked()
            {
                ui.output_mut(|o| o.copied_text = info.fingerprint_sha256.clone());
                state.set_copied_feedback(true);
            }
        }
        None => {
            let note = if settings.uses_custom_certificate() {
                "Fingerprint appears when the server starts"
            } else {
                "A certificate is made on the next Start"
            };
            ui.label(micro_label(note));
        }
    }

    if !(editable && !settings.uses_custom_certificate()) {
        return;
    }
    ui.add_space(Metrics::SM);
    if ConsoleButton::ghost("New certificate")
        .small()
        .tooltip("Replace the self-signed certificate (phones will ask to trust it again)")
        .id_salt("tls_new_cert")
        .show(ui)
        .clicked()
    {
        let dir = settings.cert_dir();
        std::thread::spawn(move || {
            if let Err(e) = tls::regenerate_self_signed(&dir, &SanSet::detect()) {
                tracing::warn!("Could not make a new certificate: {e}");
            }
        });
        // Look at the files again right away, once the worker is done.
        ui.ctx().memory_mut(|m| m.data.remove::<Cached>(id));
        ui.ctx().request_repaint_after(Duration::from_millis(400));
    }
}
