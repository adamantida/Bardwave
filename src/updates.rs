//! Update check: "is there a newer Bardwave release?" -- notification only.
//!
//! Privacy first: the check is **off by default** (`gui.check_updates =
//! false`), so a stock install never talks to the network on its own. The
//! operator either clicks "Check for updates" in the console, or turns on the
//! daily check, which then runs once at start-up when more than a day has
//! passed since the last one (`gui.last_update_check`). The request is a plain
//! `GET` of the public GitHub "latest release" document; the only thing it
//! carries is a `User-Agent` of `Bardwave/<version>`. Nothing is downloaded or
//! installed: the console only shows a link to the release page, which the
//! browser opens on click.
//!
//! Layout of this module:
//!
//! * pure logic ([`Version`], [`parse_version`], [`parse_release`],
//!   [`evaluate`], [`auto_check_due`]), unit-tested without any network;
//! * the blocking [`fetch_latest`] (bounded time, bounded size), tested against
//!   a throw-away local listener;
//! * [`UpdateChecker`], which runs one fetch on its own thread and hands the
//!   result back over a channel, so the render thread never waits;
//! * the small egui pieces ([`frame`], [`settings_section`],
//!   [`status_indicator`]).

use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, TryRecvError};
use serde::Deserialize;

use crate::gui::app::SoundSharedApp;

/// GitHub API document of the newest non-draft, non-prerelease release.
pub const RELEASES_API_URL: &str =
    "https://api.github.com/repos/adamantida/Bardwave/releases/latest";

/// Release pages live under this prefix; the tag is appended.
pub const RELEASE_PAGE_PREFIX: &str = "https://github.com/adamantida/Bardwave/releases/tag/";

/// Whole-request budget (connect + TLS + response).
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(8);

/// A genuine "latest release" document is a few KiB; anything bigger than this
/// is refused instead of being read into memory.
pub const MAX_RESPONSE_BYTES: u64 = 256 * 1024;

/// The automatic check runs at most once per this many seconds.
pub const AUTO_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Longest `tag_name` taken seriously.
const MAX_TAG_LEN: usize = 64;

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// A `major.minor.patch` release version (ordered numerically, so
/// `1.5.0 < 1.10.0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major number.
    pub major: u64,
    /// Minor number.
    pub minor: u64,
    /// Patch number.
    pub patch: u64,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Strip surrounding whitespace and one leading `v`/`V`.
fn strip_v(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix(['v', 'V']).unwrap_or(text)
}

/// Whether `text` carries a semver pre-release part (`1.6.0-beta.1`).
pub fn is_prerelease(text: &str) -> bool {
    let text = strip_v(text);
    let core = text.split('+').next().unwrap_or(text);
    core.contains('-')
}

/// Parse `1.5.0` or `v1.5.0` (build metadata after `+` is ignored).
///
/// Returns `None` for anything that is not exactly three plain numbers --
/// including pre-releases such as `1.6.0-rc.1`, which are never offered.
pub fn parse_version(text: &str) -> Option<Version> {
    let text = strip_v(text);
    let core = text.split('+').next()?;
    if core.contains('-') {
        return None;
    }
    let mut parts = core.split('.');
    let major = parse_number(parts.next()?)?;
    let minor = parse_number(parts.next()?)?;
    let patch = parse_number(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
    })
}

/// Digits only, no sign, no empty part, no absurd length.
fn parse_number(part: &str) -> Option<u64> {
    if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

// ---------------------------------------------------------------------------
// Response handling
// ---------------------------------------------------------------------------

/// A newer release the operator can go and read about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// The release version.
    pub version: Version,
    /// Release page, built from the validated version (never taken from the
    /// response, so a hostile answer cannot choose what the click opens).
    pub url: String,
}

impl ReleaseInfo {
    fn new(version: Version) -> Self {
        Self {
            url: format!("{RELEASE_PAGE_PREFIX}v{version}"),
            version,
        }
    }

    /// `v1.6.0`, the text shown in the console.
    pub fn label(&self) -> String {
        format!("v{}", self.version)
    }
}

/// What a finished check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The running version is the newest stable one (or newer).
    UpToDate,
    /// A newer stable release exists.
    Available(ReleaseInfo),
}

/// Why a check failed. The text is what the console shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// No connection, DNS failure, TLS failure or timeout.
    Network,
    /// The server answered with an error status (rate limit, outage, ...).
    Status(u16),
    /// The answer was larger than [`MAX_RESPONSE_BYTES`].
    TooLarge,
    /// The answer was not a release document this code understands.
    Malformed,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network => write!(f, "Network error: could not reach the update server"),
            Self::Status(code) => write!(f, "Update server answered HTTP {code}"),
            Self::TooLarge => write!(f, "Update server sent an oversized reply"),
            Self::Malformed => write!(f, "Update server sent an unreadable reply"),
        }
    }
}

impl std::error::Error for UpdateError {}

/// The fields of the GitHub release document this module reads. Everything is
/// optional: a missing field is "not understood", never a panic.
#[derive(Debug, Deserialize)]
struct RawRelease {
    #[serde(default)]
    tag_name: Option<String>,
    #[serde(default)]
    prerelease: Option<bool>,
    #[serde(default)]
    draft: Option<bool>,
}

/// Parse a GitHub "latest release" body.
///
/// `Ok(None)` means "a release exists but is not one to offer" (draft or
/// pre-release). Garbage of any kind is `Err(Malformed)`.
pub fn parse_release(body: &[u8]) -> Result<Option<ReleaseInfo>, UpdateError> {
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(UpdateError::TooLarge);
    }
    let raw: RawRelease = serde_json::from_slice(body).map_err(|_| UpdateError::Malformed)?;
    let tag = raw.tag_name.ok_or(UpdateError::Malformed)?;
    if tag.len() > MAX_TAG_LEN {
        return Err(UpdateError::Malformed);
    }
    if raw.draft == Some(true) || raw.prerelease == Some(true) || is_prerelease(&tag) {
        return Ok(None);
    }
    let version = parse_version(&tag).ok_or(UpdateError::Malformed)?;
    Ok(Some(ReleaseInfo::new(version)))
}

/// Compare the running `current` version with a release `body`.
pub fn evaluate(current: &str, body: &[u8]) -> Result<CheckOutcome, UpdateError> {
    let Some(release) = parse_release(body)? else {
        return Ok(CheckOutcome::UpToDate);
    };
    // A running version that does not parse (a dev build) never nags.
    match parse_version(current) {
        Some(running) if release.version > running => Ok(CheckOutcome::Available(release)),
        _ => Ok(CheckOutcome::UpToDate),
    }
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/// The `User-Agent` of every request: the only identification that is sent.
pub fn user_agent(version: &str) -> String {
    format!("Bardwave/{version}")
}

/// Download the release document at `url` (blocking; call it off the UI
/// thread). Bounded in time by [`CHECK_TIMEOUT`] and in size by
/// [`MAX_RESPONSE_BYTES`]. The request carries `User-Agent` and `Accept` only.
pub fn fetch_latest(url: &str, current_version: &str) -> Result<Vec<u8>, UpdateError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(CHECK_TIMEOUT)
        .user_agent(&user_agent(current_version))
        .build();
    let response = match agent
        .get(url)
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) => return Err(UpdateError::Status(code)),
        Err(ureq::Error::Transport(_)) => return Err(UpdateError::Network),
    };

    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| UpdateError::Network)?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(UpdateError::TooLarge);
    }
    Ok(body)
}

/// Fetch `url` and compare with `current`: the whole check, blocking.
pub fn check_blocking(url: &str, current: &str) -> Result<CheckOutcome, UpdateError> {
    let body = fetch_latest(url, current)?;
    evaluate(current, &body)
}

// ---------------------------------------------------------------------------
// Scheduling
// ---------------------------------------------------------------------------

/// Unix time in seconds (0 if the clock is before 1970).
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether the start-up check should run: only when the operator enabled it
/// and a day has passed since `last_check` (0 = never). A clock that went
/// backwards counts as due, so the stored time heals itself.
pub fn auto_check_due(enabled: bool, last_check: u64, now: u64) -> bool {
    enabled && (last_check == 0 || now < last_check || now - last_check >= AUTO_CHECK_INTERVAL_SECS)
}

// ---------------------------------------------------------------------------
// Checker (thread + channel)
// ---------------------------------------------------------------------------

/// What the console currently knows about updates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateState {
    /// No check has run in this session.
    #[default]
    Idle,
    /// A check is in flight.
    Checking,
    /// The last check found nothing newer.
    UpToDate,
    /// A newer release exists.
    Available(ReleaseInfo),
    /// The last check failed (message for the operator).
    Failed(String),
}

type CheckResult = Result<CheckOutcome, UpdateError>;

/// Runs update checks on a worker thread and collects the answer.
#[derive(Debug, Default)]
pub struct UpdateChecker {
    state: UpdateState,
    rx: Option<Receiver<CheckResult>>,
    /// Whether the start-up check was already considered this session.
    auto_done: bool,
}

impl UpdateChecker {
    /// A checker in the idle state; touches nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current state.
    pub fn state(&self) -> &UpdateState {
        &self.state
    }

    /// Whether a check is in flight.
    pub fn is_checking(&self) -> bool {
        self.rx.is_some()
    }

    /// Start a check of `url` against `current` unless one is already
    /// running. `on_done` is called on the worker thread when the answer is
    /// ready (the console passes a repaint request). Returns whether a check
    /// was started.
    pub fn start_with(
        &mut self,
        url: &str,
        current: &str,
        on_done: impl FnOnce() + Send + 'static,
    ) -> bool {
        if self.rx.is_some() {
            return false;
        }
        let (tx, rx) = crossbeam_channel::bounded::<CheckResult>(1);
        let url = url.to_string();
        let current = current.to_string();
        let spawned = std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let result = check_blocking(&url, &current);
                let _ = tx.send(result);
                on_done();
            });
        match spawned {
            Ok(_) => {
                self.rx = Some(rx);
                self.state = UpdateState::Checking;
                true
            }
            Err(_) => {
                self.state = UpdateState::Failed("Could not start the update check".into());
                false
            }
        }
    }

    /// Start the real check (GitHub, this build's version).
    pub fn start(&mut self, ctx: &egui::Context) -> bool {
        let ctx = ctx.clone();
        self.start_with(RELEASES_API_URL, crate::VERSION, move || {
            ctx.request_repaint()
        })
    }

    /// Collect a finished answer, if any. Never blocks.
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            // The worker died without answering.
            Err(TryRecvError::Disconnected) => Err(UpdateError::Network),
        };
        self.rx = None;
        self.state = match result {
            Ok(CheckOutcome::UpToDate) => UpdateState::UpToDate,
            Ok(CheckOutcome::Available(release)) => UpdateState::Available(release),
            Err(error) => UpdateState::Failed(error.to_string()),
        };
    }
}

// ---------------------------------------------------------------------------
// egui
// ---------------------------------------------------------------------------

/// Per-frame hook: collect a finished check and, once per session, run the
/// daily automatic check when the operator enabled it. With the default
/// config (`check_updates = false`) this never touches the network.
pub fn frame(state: &mut SoundSharedApp, ctx: &egui::Context) {
    state.extras.updates.poll();
    if state.extras.updates.auto_done {
        return;
    }
    state.extras.updates.auto_done = true;

    let now = unix_now();
    let due = {
        let config = state.app_state.config.lock().unwrap();
        auto_check_due(config.gui.check_updates, config.gui.last_update_check, now)
    };
    if due && state.extras.updates.start(ctx) {
        state.app_state.config.lock().unwrap().gui.last_update_check = now;
        state.library_dirty = true;
    }
}

/// The "UPDATES" block of the console: the button, the automatic-check
/// switch and the result line.
pub fn settings_section(ui: &mut egui::Ui, state: &mut SoundSharedApp) {
    use crate::gui::theme::{micro_label, Metrics, Palette};
    use crate::gui::widgets::ConsoleButton;

    ui.label(micro_label("Updates"));
    ui.add_space(Metrics::SM);

    let checking = state.extras.updates.is_checking();
    let clicked = ConsoleButton::ghost("Check for updates")
        .width(ui.available_width())
        .height(Metrics::BTN_H)
        .enabled(!checking)
        .tooltip("Asks GitHub for the newest release. Nothing else is sent or downloaded.")
        .id_salt("updates_check")
        .show(ui)
        .clicked();
    if clicked && !checking {
        let ctx = ui.ctx().clone();
        if state.extras.updates.start(&ctx) {
            state.app_state.config.lock().unwrap().gui.last_update_check = unix_now();
            state.library_dirty = true;
        }
    }

    ui.add_space(Metrics::XS);
    let mut auto = state.app_state.config.lock().unwrap().gui.check_updates;
    if ui
        .checkbox(&mut auto, "Check once a day at start")
        .on_hover_text("Off by default: Bardwave makes no network requests on its own.")
        .changed()
    {
        state.app_state.config.lock().unwrap().gui.check_updates = auto;
        state.library_dirty = true;
    }

    ui.add_space(Metrics::XS);
    let (text, color) = match state.extras.updates.state() {
        UpdateState::Idle => (None, Palette::ZINC_500),
        UpdateState::Checking => (Some("Checking...".to_string()), Palette::ZINC_400),
        UpdateState::UpToDate => (
            Some(format!("Up to date (v{})", crate::VERSION)),
            Palette::ZINC_400,
        ),
        UpdateState::Available(release) => (
            Some(format!("Update available: {}", release.label())),
            Palette::ROSE_400,
        ),
        UpdateState::Failed(message) => (Some(message.clone()), Palette::AMBER_400),
    };
    if let Some(text) = text {
        ui.add(egui::Label::new(egui::RichText::new(text).size(11.0).color(color)).wrap());
    }
    if let UpdateState::Available(release) = state.extras.updates.state() {
        if ui.link("Open release page").clicked() {
            ui.ctx()
                .open_url(egui::OpenUrl::new_tab(release.url.clone()));
        }
    }
}

/// Small "Update available: vX.Y.Z" link for the status strip; draws nothing
/// unless a newer release was found. Clicking opens the release page in the
/// browser.
pub fn status_indicator(ui: &mut egui::Ui, state: &SoundSharedApp) {
    use crate::gui::theme::Palette;

    if let UpdateState::Available(release) = state.extras.updates.state() {
        let text = egui::RichText::new(format!("Update available: {}", release.label()))
            .font(egui::FontId::new(9.0, egui::FontFamily::Monospace))
            .color(Palette::ROSE_400)
            .extra_letter_spacing(1.0);
        let response = ui
            .add(egui::Link::new(text))
            .on_hover_text("Open the release page in your browser");
        if response.clicked() {
            ui.ctx()
                .open_url(egui::OpenUrl::new_tab(release.url.clone()));
        }
    }
}
