//! The game-master link in the console, through the public API only: the
//! token guard the console shows is the one the server checks, the link shown
//! on screen hides the key, and the QR code encodes the whole link.

use std::sync::{Arc, Mutex};

use soundshared::config::Config;
use soundshared::gui::server_panel::{masked_gm_link, qr_matrix};
use soundshared::gui::types::AppState;
use soundshared::network::GmGuard;

fn app_state(guard: &GmGuard) -> AppState {
    let (audio_tx, _audio_rx) = crossbeam_channel::bounded(4);
    let (server_tx, _server_rx) = crossbeam_channel::bounded(4);
    AppState::new(
        audio_tx,
        server_tx,
        Arc::new(Mutex::new(0)),
        Arc::new(Mutex::new(Config::default())),
    )
    .with_gm(guard.clone(), true)
}

#[test]
fn the_console_and_the_server_share_one_token_guard() {
    let server_side = GmGuard::new(Some("oldtoken00000000".to_string()));
    let console = app_state(&server_side);
    assert!(console.gm_panel_served);

    // The console issues a new key: the server's guard changes at once.
    let watcher = server_side.subscribe();
    console.gm.set(Some("newtoken11111111".to_string()));
    assert!(watcher.has_changed().unwrap(), "sessions must be told");
    assert!(!server_side.allows(Some("oldtoken00000000")));
    assert!(server_side.allows(Some("newtoken11111111")));
}

#[test]
fn a_state_without_a_guard_offers_no_remote_access() {
    let (audio_tx, _a) = crossbeam_channel::bounded(1);
    let (server_tx, _s) = crossbeam_channel::bounded(1);
    let state = AppState::new(
        audio_tx,
        server_tx,
        Arc::new(Mutex::new(0)),
        Arc::new(Mutex::new(Config::default())),
    );
    assert!(!state.gm.remote_enabled());
    assert!(!state.gm_panel_served);
    assert_eq!(state.gm.share_url("http://10.0.0.2:8080"), None);
}

#[test]
fn the_shown_link_hides_the_key_and_the_qr_carries_all_of_it() {
    let guard = GmGuard::new(Some("abcd1234efgh5678".to_string()));
    let link = guard.share_url("http://192.168.1.5:8080").unwrap();
    assert_eq!(link, "http://192.168.1.5:8080/control?g=abcd1234efgh5678");

    let shown = masked_gm_link(&link, "abcd1234efgh5678");
    assert!(shown.starts_with("http://192.168.1.5:8080/control?g=abcd"));
    assert!(!shown.contains("1234efgh5678"));

    // Different keys give different codes, so the code really depends on it.
    let a = qr_matrix(&link).unwrap();
    let other = guard
        .share_url("http://192.168.1.5:8080")
        .unwrap()
        .replace('a', "b");
    assert_ne!(a, qr_matrix(&other).unwrap());
}
