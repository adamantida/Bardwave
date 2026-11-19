//! System-wide shortcuts (the console window is not focused), through the
//! `global-hotkey` crate. Enabled by `gui.global_hotkeys`.
//!
//! # Using it from an eframe app
//!
//! ```ignore
//! // In the app's creation closure (`eframe::run_native(.., |cc| ..)`), which
//! // eframe runs on the main thread:
//! let ctx = cc.egui_ctx.clone();
//! let started = HotkeyService::start(
//!     config.gui.global_hotkeys,
//!     &config.hotkeys,
//!     Some(Arc::new(move || ctx.request_repaint())),
//! );
//! app.hotkey_status = started.status;      // show it in the settings
//! app.hotkey_service = started.service;    // Option<HotkeyService>
//!
//! // Every frame, on the thread that owns the engine and the Director:
//! if let Some(service) = &app.hotkey_service {
//!     for event in service.poll() {
//!         dispatcher.handle(&mut director, &engine, &mut extras, &event)?;
//!     }
//! }
//! // After the hotkeys changed in the settings:
//! service.sync(&config.hotkeys);
//! ```
//!
//! # Threads
//!
//! * **macOS: [`HotkeyService::new`] must run on the main thread** (the
//!   Carbon hotkey registration is tied to the main run loop). Called from any
//!   other thread it returns [`ServiceError::NotMainThread`] instead of
//!   crashing. eframe creates the app on the main thread, and the manager
//!   lives as long as the service; keep the service on that thread (it is not
//!   `Send`). Windows and Linux (X11) run their own event thread, no
//!   restriction.
//! * Events arrive through a process-wide channel owned by `global-hotkey`;
//!   a small forwarder thread translates them into [`HotkeyEvent`]s, queues
//!   them for [`poll`](HotkeyService::poll) and calls the `wake` callback
//!   (use it to `request_repaint`, otherwise a window that is not focused may
//!   not run `update` for a long time). Because the channel is process-wide,
//!   **only one service may exist at a time.**
//!
//! # Permissions and failures
//!
//! No call here panics. On macOS the Carbon hotkeys used for ordinary keys
//! need no permission; failures are reported as [`ServiceError`] /
//! [`ServiceStatus`] with text for the user. A single shortcut that the OS
//! refuses (already taken by another application, for example) is listed in
//! [`SyncReport::failed`] and the rest still work. On Linux only X11 is
//! supported; under Wayland `new` fails with [`ServiceError::Unsupported`].
//!
//! Only shortcuts with Ctrl / Alt / Cmd (or F-keys) are registered globally
//! ([`Binding::is_global_safe`]): a bare `Space` or `A` would be stolen from
//! every other application. Keys on the keypad are fine when combined with a
//! modifier.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use parking_lot::RwLock;
use thiserror::Error;

use crate::config::{HotkeyAction, HotkeyConfig};

use super::map::{HotkeyEvent, HotkeyMap, KeyPhase, Source};
use super::{Binding, Key, NamedKey, NumpadKey, Platform};

/// Callback run (from the forwarder thread) when an event is queued.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Why the service could not start.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ServiceError {
    /// On macOS the service must be created on the main thread.
    #[error("global hotkeys must be set up on the main thread (macOS requirement)")]
    NotMainThread,
    /// The operating system asks the user to allow the app first.
    #[error("{0}")]
    NeedsPermission(String),
    /// Not available on this system (Wayland, no display, unsupported OS).
    #[error("global hotkeys are not supported here: {0}")]
    Unsupported(String),
    /// Any other operating system error.
    #[error("global hotkeys could not start: {0}")]
    Os(String),
}

/// A shortcut the system refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterFailure {
    /// Index of the `[[hotkeys]]` entry.
    pub index: usize,
    /// Its key, canonical.
    pub key: String,
    /// What to tell the user.
    pub reason: String,
}

/// Result of [`HotkeyService::sync`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// How many shortcuts are registered with the system now.
    pub registered: usize,
    /// Refused by the system or by the "needs a modifier" rule.
    pub failed: Vec<RegisterFailure>,
}

/// What the settings dialog shows about global hotkeys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    /// `gui.global_hotkeys` is off.
    Disabled,
    /// Running.
    Active {
        /// Registered shortcuts.
        registered: usize,
        /// Shortcuts that could not be registered.
        failed: Vec<RegisterFailure>,
    },
    /// The system wants the user's permission first; the text says where.
    NeedsPermission(String),
    /// Could not start; the text says why.
    Unavailable(String),
}

impl ServiceStatus {
    /// Map a start error to a status.
    pub fn from_error(error: &ServiceError) -> Self {
        match error {
            ServiceError::NeedsPermission(text) => Self::NeedsPermission(text.clone()),
            other => Self::Unavailable(other.to_string()),
        }
    }

    /// One line for the UI.
    pub fn message(&self) -> String {
        match self {
            Self::Disabled => "Global hotkeys are off.".to_string(),
            Self::Active { registered, failed } if failed.is_empty() => {
                format!("Global hotkeys active ({registered}).")
            }
            Self::Active { registered, failed } => format!(
                "Global hotkeys active ({registered}); {} could not be registered.",
                failed.len()
            ),
            Self::NeedsPermission(text) | Self::Unavailable(text) => text.clone(),
        }
    }
}

/// What [`HotkeyService::start`] returns: the service (when it runs) and the
/// status to display.
pub struct Started {
    /// The running service, `None` when disabled or failed.
    pub service: Option<HotkeyService>,
    /// What to show.
    pub status: ServiceStatus,
}

/// Table from system hotkey ids to entries, shared with the forwarder thread.
type Routes = Arc<RwLock<HashMap<u32, (usize, HotkeyAction)>>>;

/// Turn a raw system event into a [`HotkeyEvent`] (the forwarder's logic,
/// separate so it can be tested without the operating system).
fn route(
    routes: &HashMap<u32, (usize, HotkeyAction)>,
    id: u32,
    state: HotKeyState,
) -> Option<HotkeyEvent> {
    let (index, action) = routes.get(&id)?;
    Some(HotkeyEvent {
        index: *index,
        action: action.clone(),
        phase: match state {
            HotKeyState::Pressed => KeyPhase::Pressed,
            HotKeyState::Released => KeyPhase::Released,
        },
        source: Source::Global,
    })
}

/// The system-wide shortcut service. See the module docs, in particular the
/// main-thread rule on macOS.
pub struct HotkeyService {
    manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
    covered: HashSet<Binding>,
    routes: Routes,
    events: Receiver<HotkeyEvent>,
    stop: Arc<AtomicBool>,
    forwarder: Option<JoinHandle<()>>,
}

impl HotkeyService {
    /// Create the service with no shortcuts registered yet (call
    /// [`sync`](Self::sync)). `wake` runs whenever an event is queued.
    pub fn new(wake: Option<Wake>) -> Result<Self, ServiceError> {
        if !is_main_thread() {
            return Err(ServiceError::NotMainThread);
        }
        let manager = std::panic::catch_unwind(GlobalHotKeyManager::new)
            .map_err(|_| ServiceError::Os("the hotkey backend panicked".to_string()))?
            .map_err(|e| classify_start_error(&e.to_string()))?;

        let routes: Routes = Arc::new(RwLock::new(HashMap::new()));
        let (tx, events) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let forwarder = {
            let routes = routes.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("hotkey-forwarder".to_string())
                .spawn(move || {
                    let source = GlobalHotKeyEvent::receiver();
                    while !stop.load(Ordering::Relaxed) {
                        match source.recv_timeout(Duration::from_millis(100)) {
                            Ok(raw) => {
                                let event = route(&routes.read(), raw.id, raw.state);
                                if let Some(event) = event {
                                    if tx.send(event).is_err() {
                                        break;
                                    }
                                    if let Some(wake) = &wake {
                                        wake();
                                    }
                                }
                            }
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .map_err(|e| ServiceError::Os(format!("cannot start the hotkey thread: {e}")))?
        };

        Ok(Self {
            manager,
            registered: Vec::new(),
            covered: HashSet::new(),
            routes,
            events,
            stop,
            forwarder: Some(forwarder),
        })
    }

    /// The whole start-up in one call: nothing when `enabled` is false,
    /// otherwise create the service and register `hotkeys`. Never panics; the
    /// returned status says what happened.
    pub fn start(enabled: bool, hotkeys: &[HotkeyConfig], wake: Option<Wake>) -> Started {
        if !enabled {
            return Started {
                service: None,
                status: ServiceStatus::Disabled,
            };
        }
        match Self::new(wake) {
            Ok(mut service) => {
                let report = service.sync(hotkeys);
                Started {
                    status: ServiceStatus::Active {
                        registered: report.registered,
                        failed: report.failed,
                    },
                    service: Some(service),
                }
            }
            Err(e) => Started {
                status: ServiceStatus::from_error(&e),
                service: None,
            },
        }
    }

    /// Replace the registered shortcuts with those of `hotkeys` (call after
    /// the user edits them). Entries that cannot be global are reported, not
    /// fatal.
    pub fn sync(&mut self, hotkeys: &[HotkeyConfig]) -> SyncReport {
        let _ = self.manager.unregister_all(&self.registered);
        self.registered.clear();
        self.covered.clear();
        let mut routes = HashMap::new();
        let mut failed = Vec::new();

        let map = HotkeyMap::build_for(hotkeys, Platform::current());
        for entry in map.entries() {
            let key = entry.binding.to_string();
            if !entry.binding.is_global_safe() {
                failed.push(RegisterFailure {
                    index: entry.index,
                    key,
                    reason: "a global shortcut needs Ctrl, Alt or Cmd (or an F-key); a plain key would be taken from every other app".to_string(),
                });
                continue;
            }
            let hotkey = entry.binding.to_global();
            match self.manager.register(hotkey) {
                Ok(()) => {
                    routes.insert(hotkey.id(), (entry.index, entry.action.clone()));
                    self.registered.push(hotkey);
                    self.covered.insert(entry.binding);
                }
                Err(e) => failed.push(RegisterFailure {
                    index: entry.index,
                    key,
                    reason: format!("the system refused it (another application may use it): {e}"),
                }),
            }
        }
        *self.routes.write() = routes;
        SyncReport {
            registered: self.registered.len(),
            failed,
        }
    }

    /// Events since the last call, oldest first. Never blocks.
    pub fn poll(&self) -> Vec<HotkeyEvent> {
        self.events.try_iter().collect()
    }

    /// Whether the combination is registered with the system (then
    /// [`poll_local`](super::poll_local) must skip it, see its docs).
    pub fn covers(&self, binding: &Binding) -> bool {
        self.covered.contains(binding)
    }

    /// How many shortcuts are registered.
    pub fn registered_count(&self) -> usize {
        self.registered.len()
    }
}

impl Drop for HotkeyService {
    fn drop(&mut self) {
        let _ = self.manager.unregister_all(&self.registered);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.forwarder.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(target_os = "macos")]
fn is_main_thread() -> bool {
    extern "C" {
        fn pthread_main_np() -> std::os::raw::c_int;
    }
    // SAFETY: pthread_main_np takes no arguments and only reads thread state.
    unsafe { pthread_main_np() == 1 }
}

#[cfg(not(target_os = "macos"))]
fn is_main_thread() -> bool {
    true
}

/// Turn the backend's error text into a user-facing error.
fn classify_start_error(text: &str) -> ServiceError {
    if cfg!(target_os = "macos") {
        ServiceError::NeedsPermission(format!(
            "macOS did not let Bardwave listen for global hotkeys ({text}). Open System Settings > Privacy & Security, allow Bardwave under Accessibility and Input Monitoring, then restart the app."
        ))
    } else if cfg!(target_os = "linux") {
        ServiceError::Unsupported(format!(
            "{text} (only X11 sessions are supported; Wayland does not allow global hotkeys)"
        ))
    } else {
        ServiceError::Os(text.to_string())
    }
}

impl Binding {
    /// The `global-hotkey` form of the combination.
    pub fn to_global(&self) -> HotKey {
        let mut mods = Modifiers::empty();
        if self.mods.ctrl {
            mods |= Modifiers::CONTROL;
        }
        if self.mods.alt {
            mods |= Modifiers::ALT;
        }
        if self.mods.shift {
            mods |= Modifiers::SHIFT;
        }
        if self.mods.cmd {
            mods |= Modifiers::SUPER;
        }
        HotKey::new(Some(mods), self.key.to_code())
    }
}

impl Key {
    /// The physical key code used by the system hotkey layer.
    pub fn to_code(self) -> Code {
        const LETTERS: [Code; 26] = [
            Code::KeyA,
            Code::KeyB,
            Code::KeyC,
            Code::KeyD,
            Code::KeyE,
            Code::KeyF,
            Code::KeyG,
            Code::KeyH,
            Code::KeyI,
            Code::KeyJ,
            Code::KeyK,
            Code::KeyL,
            Code::KeyM,
            Code::KeyN,
            Code::KeyO,
            Code::KeyP,
            Code::KeyQ,
            Code::KeyR,
            Code::KeyS,
            Code::KeyT,
            Code::KeyU,
            Code::KeyV,
            Code::KeyW,
            Code::KeyX,
            Code::KeyY,
            Code::KeyZ,
        ];
        const DIGITS: [Code; 10] = [
            Code::Digit0,
            Code::Digit1,
            Code::Digit2,
            Code::Digit3,
            Code::Digit4,
            Code::Digit5,
            Code::Digit6,
            Code::Digit7,
            Code::Digit8,
            Code::Digit9,
        ];
        const NUMPAD: [Code; 10] = [
            Code::Numpad0,
            Code::Numpad1,
            Code::Numpad2,
            Code::Numpad3,
            Code::Numpad4,
            Code::Numpad5,
            Code::Numpad6,
            Code::Numpad7,
            Code::Numpad8,
            Code::Numpad9,
        ];
        const FUNCTION: [Code; 24] = [
            Code::F1,
            Code::F2,
            Code::F3,
            Code::F4,
            Code::F5,
            Code::F6,
            Code::F7,
            Code::F8,
            Code::F9,
            Code::F10,
            Code::F11,
            Code::F12,
            Code::F13,
            Code::F14,
            Code::F15,
            Code::F16,
            Code::F17,
            Code::F18,
            Code::F19,
            Code::F20,
            Code::F21,
            Code::F22,
            Code::F23,
            Code::F24,
        ];
        match self {
            // The parser only builds in-range values; the fallbacks are
            // unreachable guards for a hand-built `Key`.
            Key::Letter(c) => LETTERS[((c as u8).saturating_sub(b'A') as usize).min(25)],
            Key::Digit(d) => DIGITS[(d as usize).min(9)],
            Key::Function(n) => FUNCTION[(n as usize).clamp(1, 24) - 1],
            Key::Numpad(NumpadKey::Digit(d)) => NUMPAD[(d as usize).min(9)],
            Key::Numpad(NumpadKey::Add) => Code::NumpadAdd,
            Key::Numpad(NumpadKey::Subtract) => Code::NumpadSubtract,
            Key::Numpad(NumpadKey::Multiply) => Code::NumpadMultiply,
            Key::Numpad(NumpadKey::Divide) => Code::NumpadDivide,
            Key::Numpad(NumpadKey::Decimal) => Code::NumpadDecimal,
            Key::Numpad(NumpadKey::Enter) => Code::NumpadEnter,
            Key::Named(named) => match named {
                NamedKey::Space => Code::Space,
                NamedKey::Enter => Code::Enter,
                NamedKey::Escape => Code::Escape,
                NamedKey::Tab => Code::Tab,
                NamedKey::Backspace => Code::Backspace,
                NamedKey::Delete => Code::Delete,
                NamedKey::Insert => Code::Insert,
                NamedKey::Home => Code::Home,
                NamedKey::End => Code::End,
                NamedKey::PageUp => Code::PageUp,
                NamedKey::PageDown => Code::PageDown,
                NamedKey::Up => Code::ArrowUp,
                NamedKey::Down => Code::ArrowDown,
                NamedKey::Left => Code::ArrowLeft,
                NamedKey::Right => Code::ArrowRight,
                NamedKey::Minus => Code::Minus,
                NamedKey::Equals => Code::Equal,
                NamedKey::Comma => Code::Comma,
                NamedKey::Period => Code::Period,
                NamedKey::Slash => Code::Slash,
                NamedKey::Backslash => Code::Backslash,
                NamedKey::Semicolon => Code::Semicolon,
                NamedKey::Quote => Code::Quote,
                NamedKey::Backtick => Code::Backquote,
                NamedKey::BracketLeft => Code::BracketLeft,
                NamedKey::BracketRight => Code::BracketRight,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(text: &str) -> Binding {
        Binding::parse_for(text, Platform::Other).unwrap()
    }

    #[test]
    fn conversion_to_system_hotkey() {
        let hk = b("Ctrl+Shift+1").to_global();
        assert_eq!(hk.key, Code::Digit1);
        assert!(hk.mods.contains(Modifiers::CONTROL | Modifiers::SHIFT));
        assert!(!hk.mods.contains(Modifiers::ALT));
        let hk = b("Alt+F5").to_global();
        assert_eq!(hk.key, Code::F5);
        assert_eq!(hk.mods, Modifiers::ALT);
        assert_eq!(b("Cmd+K").to_global().mods, Modifiers::SUPER);
        assert_eq!(b("Ctrl+Numpad1").to_global().key, Code::Numpad1);
        assert_eq!(b("Ctrl+Backtick").to_global().key, Code::Backquote);
        assert_eq!(b("Ctrl+Up").to_global().key, Code::ArrowUp);
    }

    #[test]
    fn different_bindings_get_different_system_ids() {
        let mut ids = HashSet::new();
        for text in ["Ctrl+1", "Ctrl+2", "Alt+1", "Ctrl+Shift+1", "F5", "Ctrl+A"] {
            assert!(ids.insert(b(text).to_global().id()), "{text}");
        }
    }

    #[test]
    fn every_key_has_a_code() {
        for c in 'A'..='Z' {
            let _ = Key::Letter(c).to_code();
        }
        for n in 1..=24 {
            let _ = Key::Function(n).to_code();
        }
        for named in NamedKey::ALL {
            let _ = Key::Named(named).to_code();
        }
        assert_eq!(Key::Letter('A').to_code(), Code::KeyA);
        assert_eq!(Key::Letter('Z').to_code(), Code::KeyZ);
        assert_eq!(Key::Function(24).to_code(), Code::F24);
    }

    #[test]
    fn routing_translates_ids_and_states() {
        let mut routes = HashMap::new();
        let hk = b("Ctrl+D").to_global();
        routes.insert(hk.id(), (3, HotkeyAction::DuckHold));
        let down = route(&routes, hk.id(), HotKeyState::Pressed).unwrap();
        assert_eq!(down.index, 3);
        assert_eq!(down.action, HotkeyAction::DuckHold);
        assert_eq!(down.phase, KeyPhase::Pressed);
        assert_eq!(down.source, Source::Global);
        let up = route(&routes, hk.id(), HotKeyState::Released).unwrap();
        assert_eq!(up.phase, KeyPhase::Released);
        assert!(route(&routes, hk.id() + 1, HotKeyState::Pressed).is_none());
    }

    #[test]
    fn disabled_start_does_nothing() {
        let started = HotkeyService::start(false, &[], None);
        assert!(started.service.is_none());
        assert_eq!(started.status, ServiceStatus::Disabled);
        assert!(started.status.message().contains("off"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn off_the_main_thread_it_is_an_error_not_a_crash() {
        // Test threads are never the main thread.
        let started = HotkeyService::start(true, &[], None);
        assert!(started.service.is_none());
        assert!(matches!(started.status, ServiceStatus::Unavailable(_)));
        assert!(started.status.message().contains("main thread"));
        assert_eq!(
            HotkeyService::new(None).err(),
            Some(ServiceError::NotMainThread)
        );
    }

    #[test]
    fn status_mapping() {
        let perm = ServiceError::NeedsPermission("allow it".into());
        assert_eq!(
            ServiceStatus::from_error(&perm),
            ServiceStatus::NeedsPermission("allow it".into())
        );
        let other = ServiceError::Os("boom".into());
        assert!(matches!(
            ServiceStatus::from_error(&other),
            ServiceStatus::Unavailable(_)
        ));
        let active = ServiceStatus::Active {
            registered: 2,
            failed: vec![RegisterFailure {
                index: 0,
                key: "F1".into(),
                reason: "x".into(),
            }],
        };
        assert!(active.message().contains("1 could not"));
    }

    #[test]
    fn start_errors_have_guidance() {
        let e = classify_start_error("oops");
        if cfg!(target_os = "macos") {
            assert!(matches!(e, ServiceError::NeedsPermission(_)));
            assert!(e.to_string().contains("System Settings"));
        } else if cfg!(target_os = "linux") {
            assert!(e.to_string().contains("X11"));
        }
    }
}
