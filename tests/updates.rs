//! Update check: version comparison, parsing of GitHub answers (good and
//! garbage), the privacy default, and the fetch/thread path against a local
//! one-shot HTTP listener on an ephemeral port (never the real GitHub).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use soundshared::config::Config;
use soundshared::updates::{
    auto_check_due, check_blocking, evaluate, fetch_latest, is_prerelease, parse_release,
    parse_version, user_agent, CheckOutcome, UpdateChecker, UpdateError, UpdateState, Version,
    AUTO_CHECK_INTERVAL_SECS, MAX_RESPONSE_BYTES, RELEASES_API_URL,
};

fn v(major: u64, minor: u64, patch: u64) -> Version {
    Version {
        major,
        minor,
        patch,
    }
}

/// A trimmed but realistic `releases/latest` document.
const TYPICAL: &str = r#"{
  "url": "https://api.github.com/repos/adamantida/Bardwave/releases/1",
  "html_url": "https://github.com/adamantida/Bardwave/releases/tag/v1.6.0",
  "id": 1,
  "tag_name": "v1.6.0",
  "name": "Bardwave 1.6.0",
  "draft": false,
  "prerelease": false,
  "published_at": "2026-01-01T00:00:00Z",
  "assets": [{"name": "Bardwave.dmg", "size": 12345}],
  "body": "Notes \u0000 with odd bytes"
}"#;

// ---- versions -------------------------------------------------------------

#[test]
fn versions_compare_numerically() {
    assert!(parse_version("1.5.0").unwrap() < parse_version("1.10.0").unwrap());
    assert!(parse_version("1.9.9").unwrap() < parse_version("2.0.0").unwrap());
    assert!(parse_version("1.5.1").unwrap() > parse_version("1.5.0").unwrap());
    assert_eq!(parse_version("1.5.0"), parse_version("v1.5.0"));
    assert_eq!(parse_version("  V1.5.0 "), Some(v(1, 5, 0)));
    assert_eq!(parse_version("v1.5.0+build.7"), Some(v(1, 5, 0)));
    assert_eq!(v(1, 10, 0).to_string(), "1.10.0");
}

#[test]
fn prereleases_and_junk_do_not_parse() {
    assert!(is_prerelease("v1.6.0-beta.1"));
    assert!(is_prerelease("1.6.0-rc1"));
    assert!(!is_prerelease("v1.6.0"));
    assert!(!is_prerelease("v1.6.0+meta-data"));
    for bad in [
        "",
        "v",
        "1",
        "1.5",
        "1.5.0.1",
        "1.5.x",
        "-1.5.0",
        "+1.5.0",
        "1..0",
        "1.5.0-beta",
        "latest",
        "99999999999999999999.0.0",
        "1.5.0\n2.0.0",
    ] {
        assert_eq!(parse_version(bad), None, "{bad:?}");
    }
}

#[test]
fn running_version_parses() {
    // Cargo.toml's version must stay in the form this module understands.
    assert!(parse_version(soundshared::VERSION).is_some());
}

// ---- responses ------------------------------------------------------------

#[test]
fn typical_release_parses_and_links_to_the_release_page() {
    let release = parse_release(TYPICAL.as_bytes()).unwrap().unwrap();
    assert_eq!(release.version, v(1, 6, 0));
    assert_eq!(release.label(), "v1.6.0");
    assert_eq!(
        release.url,
        "https://github.com/adamantida/Bardwave/releases/tag/v1.6.0"
    );
}

#[test]
fn evaluate_reports_newer_equal_and_older() {
    let newer = evaluate("1.5.0", TYPICAL.as_bytes()).unwrap();
    assert!(matches!(newer, CheckOutcome::Available(r) if r.version == v(1, 6, 0)));
    assert_eq!(
        evaluate("1.6.0", TYPICAL.as_bytes()).unwrap(),
        CheckOutcome::UpToDate
    );
    assert_eq!(
        evaluate("1.10.0", TYPICAL.as_bytes()).unwrap(),
        CheckOutcome::UpToDate
    );
    // 1.5.0 < 1.10.0 end to end.
    let body = br#"{"tag_name":"v1.10.0"}"#;
    assert!(matches!(
        evaluate("1.5.0", body).unwrap(),
        CheckOutcome::Available(_)
    ));
    // A dev build with an unparsable version never nags.
    assert_eq!(
        evaluate("dev", TYPICAL.as_bytes()).unwrap(),
        CheckOutcome::UpToDate
    );
}

#[test]
fn prereleases_and_drafts_are_ignored() {
    for body in [
        r#"{"tag_name":"v9.0.0-beta.1"}"#,
        r#"{"tag_name":"v9.0.0","prerelease":true}"#,
        r#"{"tag_name":"v9.0.0","draft":true}"#,
    ] {
        assert_eq!(parse_release(body.as_bytes()), Ok(None), "{body}");
        assert_eq!(
            evaluate("1.5.0", body.as_bytes()),
            Ok(CheckOutcome::UpToDate),
            "{body}"
        );
    }
}

#[test]
fn hostile_url_in_response_is_not_used() {
    let body = br#"{"tag_name":"v2.0.0","html_url":"https://evil.example/x"}"#;
    let release = parse_release(body).unwrap().unwrap();
    assert!(release
        .url
        .starts_with("https://github.com/adamantida/Bardwave/"));
}

#[test]
fn corrupt_responses_are_errors_not_panics() {
    let long_tag = format!(r#"{{"tag_name":"{}"}}"#, "1".repeat(500));
    let cases: Vec<&[u8]> = vec![
        b"",
        b"null",
        b"[]",
        b"42",
        b"{",
        b"{\"tag_name\":",
        b"{\"tag_name\":null}",
        b"{\"tag_name\":42}",
        b"{\"tag_name\":\"latest\"}",
        b"{\"tag_name\":\"1.5\"}",
        b"{\"message\":\"Not Found\"}",
        b"<html>rate limited</html>",
        b"\xff\xfe\x00\x01",
        long_tag.as_bytes(),
    ];
    for body in cases {
        assert_eq!(
            parse_release(body),
            Err(UpdateError::Malformed),
            "{:?}",
            String::from_utf8_lossy(body)
        );
        assert!(evaluate("1.5.0", body).is_err());
    }
    // Deeply nested garbage hits serde_json's recursion limit instead of the stack.
    let deep = "[".repeat(10_000);
    assert!(parse_release(deep.as_bytes()).is_err());
    // Wrong field types for the optional flags are rejected, not guessed.
    assert!(parse_release(br#"{"tag_name":"v2.0.0","prerelease":"yes"}"#).is_err());
}

#[test]
fn oversized_body_is_refused() {
    let huge = vec![b' '; MAX_RESPONSE_BYTES as usize + 1];
    assert_eq!(parse_release(&huge), Err(UpdateError::TooLarge));
}

// ---- privacy defaults and scheduling --------------------------------------

#[test]
fn network_is_off_by_default() {
    let config = Config::default();
    assert!(!config.gui.check_updates);
    assert_eq!(config.gui.last_update_check, 0);

    // A config file without the keys (v1.4.0 and current 1.5 formats) keeps it off.
    for text in [
        "",
        "[gui]\nreduce_motion = true\n",
        "[gui]\nglobal_hotkeys = true\n",
    ] {
        let parsed: Config = toml::from_str(text).unwrap();
        assert!(!parsed.gui.check_updates, "{text:?}");
        assert_eq!(parsed.gui.last_update_check, 0);
    }

    // With the switch off, a startup check is never due, whatever the clock says.
    assert!(!auto_check_due(false, 0, 1_000_000));
    assert!(!auto_check_due(false, 5, 5 + 10 * AUTO_CHECK_INTERVAL_SECS));
}

#[test]
fn update_settings_round_trip_in_config() {
    let mut config = Config::default();
    config.gui.check_updates = true;
    config.gui.last_update_check = 1_700_000_000;
    let text = toml::to_string_pretty(&config).unwrap();
    let back: Config = toml::from_str(&text).unwrap();
    assert!(back.gui.check_updates);
    assert_eq!(back.gui.last_update_check, 1_700_000_000);

    // "never checked" is not written at all.
    let text = toml::to_string_pretty(&Config::default()).unwrap();
    assert!(!text.contains("last_update_check"));
}

#[test]
fn startup_check_runs_at_most_daily() {
    let now = 2_000_000_000;
    assert!(auto_check_due(true, 0, now));
    assert!(!auto_check_due(true, now - 60, now));
    assert!(!auto_check_due(
        true,
        now - AUTO_CHECK_INTERVAL_SECS + 1,
        now
    ));
    assert!(auto_check_due(true, now - AUTO_CHECK_INTERVAL_SECS, now));
    // Clock moved backwards: check again and rewrite the stored time.
    assert!(auto_check_due(true, now + 5_000, now));
}

#[test]
fn request_identifies_only_as_bardwave() {
    assert_eq!(user_agent("1.5.0"), "Bardwave/1.5.0");
    assert!(RELEASES_API_URL.starts_with("https://api.github.com/repos/"));
}

// ---- fetch / thread against a local listener ------------------------------

/// Serve exactly one canned HTTP response; returns the URL and a receiver for
/// the raw request the client sent.
fn serve_once(status_line: &str, body: Vec<u8>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/latest", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let status = status_line.to_string();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => request.extend_from_slice(&buf[..n]),
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&request).into_owned());
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&body);
    });
    (url, rx)
}

#[test]
fn fetch_sends_only_user_agent_and_accept() {
    let (url, requests) = serve_once("200 OK", TYPICAL.as_bytes().to_vec());
    let outcome = check_blocking(&url, "1.5.0").unwrap();
    assert!(matches!(outcome, CheckOutcome::Available(r) if r.version == v(1, 6, 0)));

    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    let lower = request.to_ascii_lowercase();
    assert!(lower.contains("user-agent: bardwave/1.5.0"), "{request}");
    for forbidden in ["cookie", "authorization", "referer", "x-"] {
        assert!(!lower.contains(forbidden), "{forbidden} in {request}");
    }
}

#[test]
fn fetch_maps_http_errors_and_garbage() {
    let (url, _rx) = serve_once("403 Forbidden", b"{\"message\":\"rate limit\"}".to_vec());
    assert_eq!(fetch_latest(&url, "1.5.0"), Err(UpdateError::Status(403)));

    let (url, _rx) = serve_once("200 OK", b"not json at all".to_vec());
    assert_eq!(check_blocking(&url, "1.5.0"), Err(UpdateError::Malformed));
}

#[test]
fn fetch_caps_the_response_size() {
    let big = vec![b'a'; MAX_RESPONSE_BYTES as usize * 2];
    let (url, _rx) = serve_once("200 OK", big);
    assert_eq!(fetch_latest(&url, "1.5.0"), Err(UpdateError::TooLarge));
}

#[test]
fn unreachable_server_is_a_network_error() {
    // Bind then drop: the port is closed when the client connects.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let url = format!("http://127.0.0.1:{port}/latest");
    assert_eq!(fetch_latest(&url, "1.5.0"), Err(UpdateError::Network));
}

fn wait_done(checker: &mut UpdateChecker) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while checker.is_checking() {
        checker.poll();
        assert!(Instant::now() < deadline, "check did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn checker_runs_off_thread_and_reports_states() {
    let mut checker = UpdateChecker::new();
    assert_eq!(*checker.state(), UpdateState::Idle);
    assert!(!checker.is_checking());

    let (url, _rx) = serve_once("200 OK", TYPICAL.as_bytes().to_vec());
    assert!(checker.start_with(&url, "1.5.0", || {}));
    assert_eq!(*checker.state(), UpdateState::Checking);
    // A second click while checking does nothing.
    assert!(!checker.start_with(&url, "1.5.0", || {}));
    wait_done(&mut checker);
    assert!(matches!(
        checker.state(),
        UpdateState::Available(r) if r.version == v(1, 6, 0)
    ));

    let (url, _rx) = serve_once("200 OK", TYPICAL.as_bytes().to_vec());
    assert!(checker.start_with(&url, "1.6.0", || {}));
    wait_done(&mut checker);
    assert_eq!(*checker.state(), UpdateState::UpToDate);

    let (url, _rx) = serve_once("500 Internal Server Error", Vec::new());
    assert!(checker.start_with(&url, "1.5.0", || {}));
    wait_done(&mut checker);
    assert!(matches!(checker.state(), UpdateState::Failed(m) if m.contains("500")));
}
