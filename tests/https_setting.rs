//! The HTTPS setting as the operator meets it: the pure view logic of the
//! section, links and QR with `https://`, the config keys and the two guides.
//!
//! Nothing here touches the real data directory or a fixed port: certificates
//! are made in temporary directories and no server is started.

use std::path::{Path, PathBuf};

use soundshared::config::Config;
use soundshared::gui::https_ui::{
    apply_notice, check_files, config_value, expiry_text, layout, missing_path_error, mode_of,
    paths_state, ApplyNotice, CertFacts, CertMode, Field, PathsState, APPLY_WARNING, OFF_NOTE,
    RESTART_HINT,
};
use soundshared::gui::server_panel::{qr_image, qr_matrix};
use soundshared::network::server::{listen_url_with_scheme, loopback_url};
use soundshared::network::tls::{self, SanSet, TlsSettings};
use soundshared::network::{AccessGuard, GmGuard};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bardwave-https-setting-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ------------------------------------------------------------- view logic --

#[test]
fn https_is_off_by_default_and_the_section_is_collapsed() {
    let config = Config::default();
    assert!(!config.server.tls);
    assert_eq!(config.server.tls_cert, None);
    assert_eq!(config.server.tls_key, None);

    let settings = TlsSettings::from_config(&config.server);
    assert!(!settings.enabled);
    let shape = layout(settings.enabled, mode_of(&settings));
    assert!(!shape.expanded);
    assert!(!shape.show_mode && !shape.show_files && !shape.show_certificate);
    assert_eq!(
        OFF_NOTE,
        "Encrypts links over the internet. Not needed on a home network."
    );
}

#[test]
fn the_sections_follow_the_switch_and_the_mode() {
    let on_self = layout(true, CertMode::SelfSigned);
    assert!(on_self.expanded && on_self.show_regenerate && on_self.show_certificate);
    assert!(!on_self.show_files);
    let on_custom = layout(true, CertMode::Custom);
    assert!(on_custom.show_files && !on_custom.show_regenerate);
    // Nothing is shown for a stored custom path while HTTPS is off.
    assert!(!layout(false, CertMode::Custom).show_files);
}

#[test]
fn path_inputs_are_classified_and_a_half_pair_is_flagged() {
    assert_eq!(paths_state(" ", ""), PathsState::Neither);
    assert_eq!(paths_state("/c.pem", ""), PathsState::OnlyCert);
    assert_eq!(paths_state("", "/k.pem"), PathsState::OnlyKey);
    assert_eq!(paths_state("/c.pem", "/k.pem"), PathsState::Both);
    assert_eq!(config_value(" /c.pem\n"), Some("/c.pem".to_string()));
    assert_eq!(config_value(""), None);

    assert_eq!(
        missing_path_error(PathsState::OnlyCert).map(|e| e.field),
        Some(Field::Key)
    );
    assert_eq!(
        missing_path_error(PathsState::OnlyKey).map(|e| e.field),
        Some(Field::Cert)
    );
    assert!(missing_path_error(PathsState::Both).is_none());
}

#[test]
fn the_applying_text_depends_on_the_server_and_the_setting() {
    assert_eq!(
        APPLY_WARNING,
        "Applies on next Start. Player and GM links (and QR) will switch to https://"
    );
    assert_eq!(RESTART_HINT, "Restart the server to apply");

    let a = CertFacts {
        fingerprint: "AA:BB",
        custom: false,
    };
    // Stopped: the warning, no restart hint.
    let stopped = apply_notice(true, false, None, None);
    assert_eq!(stopped, ApplyNotice::NextStart);
    assert_eq!(stopped.warning(), Some(APPLY_WARNING));
    assert_eq!(stopped.restart_hint(), None);
    // Running over HTTP with the setting on: both lines.
    let running = apply_notice(true, true, None, None);
    assert_eq!(running.warning(), Some(APPLY_WARNING));
    assert_eq!(running.restart_hint(), Some(RESTART_HINT));
    // Running over HTTPS and nothing changed: no restart hint.
    let same = apply_notice(true, true, Some(a), Some(a));
    assert_eq!(same.restart_hint(), None);
    // Another certificate is configured now.
    let changed = apply_notice(
        true,
        true,
        Some(a),
        Some(CertFacts {
            fingerprint: "CC:DD",
            custom: true,
        }),
    );
    assert_eq!(changed.restart_hint(), Some(RESTART_HINT));
    // Switched off while HTTPS runs.
    assert_eq!(
        apply_notice(false, true, Some(a), None).restart_hint(),
        Some(RESTART_HINT)
    );
    assert_eq!(apply_notice(false, false, None, None).warning(), None);
}

#[test]
fn expiry_is_written_as_a_date_and_days() {
    let day = 19_783 * 86_400; // 2024-03-01
    assert_eq!(
        expiry_text(day + 100 * 86_400, day, true),
        "Valid until 2024-06-09 (100 days left)"
    );
    assert!(expiry_text(day - 1, day, true).starts_with("Expired on"));
}

// ---------------------------------------------------- certificate checking --

#[test]
fn own_files_are_checked_with_a_message_for_the_right_field() {
    let dir = TempDir::new("files");
    let a_dir = dir.path().join("a");
    let b_dir = dir.path().join("b");
    let a = tls::regenerate_self_signed(&a_dir, &SanSet::detect()).expect("a");
    tls::regenerate_self_signed(&b_dir, &SanSet::detect()).expect("b");
    let (cert, key) = (a_dir.join(tls::CERT_FILE), a_dir.join(tls::KEY_FILE));

    let ok = check_files(&cert, &key).expect("a pair that belongs together");
    assert_eq!(ok.fingerprint_sha256, a.fingerprint_sha256);

    let missing = check_files(&dir.path().join("none.pem"), &key).unwrap_err();
    assert_eq!(missing.field, Field::Cert);
    let wrong_key = check_files(&cert, &b_dir.join(tls::KEY_FILE)).unwrap_err();
    assert_eq!(wrong_key.field, Field::Key);
    assert!(wrong_key.text.contains("does not belong"));
    let not_a_cert = check_files(&key, &key).unwrap_err();
    assert_eq!(not_a_cert.field, Field::Cert);
}

// --------------------------------------------------------- links and QR ----

#[test]
fn player_and_gm_links_and_the_qr_follow_https() {
    let bind = "0.0.0.0".parse().unwrap();
    let actual = "0.0.0.0:21000".parse().unwrap();
    let detected = Some("192.168.1.20".parse().unwrap());
    let base = listen_url_with_scheme(bind, actual, detected, true);
    assert_eq!(base, "https://192.168.1.20:21000");
    assert_eq!(
        loopback_url(&base).as_deref(),
        Some("https://127.0.0.1:21000")
    );

    let players = AccessGuard::new(Some("player-key".into())).share_url(&base);
    assert_eq!(players, "https://192.168.1.20:21000/?k=player-key");
    let gm = GmGuard::new(Some("gm-token".into()))
        .share_url(&base)
        .expect("gm link");
    assert_eq!(gm, "https://192.168.1.20:21000/control?g=gm-token");

    // The QR code encodes the https link.
    let matrix = qr_matrix(&gm).expect("qr");
    assert!(qr_image(&matrix, 4).size[0] > 0);

    let plain = listen_url_with_scheme(bind, actual, detected, false);
    assert!(plain.starts_with("http://"), "{plain}");
}

// ---------------------------------------------------------------- config ---

#[test]
fn the_example_config_documents_the_three_keys_and_loads() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/config.toml"),
    )
    .expect("packaging/config.toml");
    for key in ["tls = false", "tls_cert", "tls_key"] {
        assert!(text.contains(key), "packaging/config.toml lacks {key}");
    }
    let config: Config = toml::from_str(&text).expect("the example config parses");
    assert!(!config.server.tls);
    assert_eq!(config.server.tls_cert, None);
}

// ------------------------------------------------------------------ docs ---

fn guide(lang: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join(lang)
            .join("https.md"),
    )
    .unwrap_or_else(|e| panic!("docs/{lang}/https.md: {e}"))
}

/// The anchors and heading levels, in order (`<a id="x"></a>` then `## ...`).
fn outline(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut anchor: Option<String> = None;
    let mut in_code = false;
    for line in text.lines() {
        if line.starts_with("```") {
            in_code = !in_code;
        }
        if in_code {
            continue;
        }
        if let Some(rest) = line.strip_prefix("<a id=\"") {
            anchor = rest.split('"').next().map(str::to_string);
        } else if line.starts_with('#') {
            let level = line.chars().take_while(|c| *c == '#').count();
            out.push((anchor.take().unwrap_or_default(), level));
        }
    }
    out
}

#[test]
fn the_guides_have_the_same_structure_and_the_required_sections() {
    let en = guide("en");
    let ru = guide("ru");
    assert_eq!(
        outline(&en),
        outline(&ru),
        "headings differ between languages"
    );

    let anchors: Vec<String> = outline(&en).into_iter().map(|(a, _)| a).collect();
    for wanted in [
        "why-https",
        "when-you-do-not-need-it",
        "quick-start",
        "self-signed-certificate",
        "using-your-own-certificate",
        "what-changes",
        "turning-it-off",
        "troubleshooting",
        "limitations-and-security-notes",
    ] {
        assert!(anchors.iter().any(|a| a == wanted), "no section {wanted}");
    }
    // The troubleshooting table exists in both and has the same row count.
    let rows = |t: &str| t.lines().filter(|l| l.starts_with("| ")).count();
    assert_eq!(rows(&en), rows(&ru));
}

#[test]
fn the_guides_match_the_code() {
    for text in [guide("en"), guide("ru")] {
        // Names that exist in the interface and the config.
        for word in [
            "SECURE CONNECTION (HTTPS)",
            "Self-signed (automatic)",
            "My certificate",
            "Regenerate certificate",
            "tls_cert",
            "tls_key",
            "Restart the server to apply",
            "wss://",
            "397",
            tls::CERT_FILE,
            tls::KEY_FILE,
            tls::META_FILE,
        ] {
            assert!(text.contains(word), "a guide lacks {word:?}");
        }
        assert!(text.contains(&format!("{}/", tls::TLS_DIR)));
        // Nothing that the code does not have.
        for invented in ["tls_port", "tls_names", "redirect_http", "auto_https"] {
            assert!(!text.contains(invented), "a guide mentions {invented}");
        }
    }
}
