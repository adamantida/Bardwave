//! Hotkeys: the keyboard (and MIDI) front of the [`Director`](crate::audio::director::Director).
//!
//! The `[[hotkeys]]` table of `config.toml` ([`HotkeyConfig`](crate::config::HotkeyConfig))
//! stores a key string and a [`HotkeyAction`](crate::config::HotkeyAction). This
//! module turns that data into behaviour, in layers that can each be used (and
//! tested) alone:
//!
//! ```text
//!  "Ctrl+Shift+1"  --parse-->  Binding  --HotkeyMap-->  entries (first wins)
//!                                  |                          |
//!             local window  <------+------>  global (OS)      +--> MIDI (feature "midi")
//!             poll_local()                    HotkeyService
//!                      \                       /
//!                       v                     v
//!                     HotkeyEvent { action, phase }
//!                                  |
//!                       HotkeyDispatcher::handle  -->  Director + Playback
//! ```
//!
//! * [`Binding`] parses and prints key combinations (this file).
//! * [`HotkeyMap`] / [`find_conflicts`] ([`map`]): the configured bindings,
//!   duplicates and invalid entries.
//! * [`HotkeyDispatcher`] ([`dispatch`]): runs a `HotkeyAction` against the
//!   Director. Actions the Director does not own yet (playlists) go through the
//!   small [`HotkeyExtras`] trait.
//! * [`poll_local`] ([`local`]): shortcuts while the console window has focus,
//!   with consuming of the egui events so keys do not leak into text fields.
//! * [`HotkeyService`] ([`service`]): system-wide shortcuts through the
//!   `global-hotkey` crate when `gui.global_hotkeys` is on.
//! * `midi` (cargo feature `midi`, off by default): note / CC triggers.
//!
//! # Key syntax
//!
//! Modifiers and exactly one key, joined by `+`, case-insensitive, any order:
//! `Ctrl+Shift+1`, `alt+f5`, `Space`, `NumPad1`, `Cmd+K`. Modifiers: `Ctrl`
//! (`Control`), `Alt` (`Option`, `Opt`), `Shift`, `Cmd` (`Command`, `Super`,
//! `Meta`, `Win`). `CmdOrCtrl` (also `CommandOrControl`, `CtrlOrCmd`) is an
//! alias that means `Cmd` on macOS and `Ctrl` elsewhere, so one string can be
//! the "standard shortcut" key on every OS. On macOS `Cmd` and `Ctrl` are
//! different keys; on Windows/Linux `Cmd` is the Windows / Super key.
//! Keys: `A`-`Z`, `0`-`9`, `F1`-`F24`, `Numpad0`-`Numpad9` (+ `NumpadAdd`,
//! `NumpadSubtract`, `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`,
//! `NumpadEnter`), `Space`, `Enter`, `Escape`, `Tab`, `Backspace`, `Delete`,
//! `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `Up`/`Down`/`Left`/`Right`,
//! `Minus`, `Equals`, `Comma`, `Period`, `Slash`, `Backslash`, `Semicolon`,
//! `Quote`, `Backtick`, `BracketLeft`, `BracketRight` (a single punctuation
//! character such as `-` or `/` is accepted too).
//!
//! The printed form ([`Binding`]'s `Display`) is canonical: modifiers in the
//! order `Ctrl+Alt+Shift+Cmd` (the same order as
//! [`canonical_hotkey`](crate::config::canonical_hotkey)), the key capitalized
//! as listed above, `CmdOrCtrl` already resolved for the platform it was parsed
//! for. Note that `canonical_hotkey` capitalizes only the first letter of a
//! name (`PageUp` -> `Pageup`); the two normal forms are only compared within
//! their own module, never against each other.
//!
//! # Threads
//!
//! Everything here is plain data except [`HotkeyService`], which must be
//! created on the main thread on macOS (see its docs). Dispatching touches
//! the Director, so it runs on the thread that owns the audio engine (the GUI
//! thread), once per frame, after collecting events from the sources.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

pub mod dispatch;
pub mod local;
pub mod map;
#[cfg(feature = "midi")]
pub mod midi;
pub mod service;
#[doc(hidden)]
pub mod testing;

pub use dispatch::{DispatchError, DispatchOutcome, HotkeyDispatcher, HotkeyExtras, NoExtras};
pub use local::{consume_egui, matches_egui, poll_local, released_egui};
pub use map::{
    find_conflicts, find_conflicts_for, HotkeyConflict, HotkeyEvent, HotkeyMap, KeyEntry, KeyPhase,
    MapProblem, ProblemKind, Source,
};
pub use service::{
    HotkeyService, RegisterFailure, ServiceError, ServiceStatus, Started, SyncReport,
};

/// Prefix of a `[[hotkeys]].key` that names a MIDI trigger, not a keyboard key.
pub const MIDI_PREFIX: &str = "midi:";

/// Whether a `[[hotkeys]].key` string is a MIDI trigger (`"Midi:Ch1:Note60"`).
pub fn is_midi_key(text: &str) -> bool {
    text.trim()
        .get(..MIDI_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(MIDI_PREFIX))
}

// ---------------------------------------------------------------- platform --

/// The operating system family that decides what `Cmd` and `CmdOrCtrl` mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// macOS: `Cmd` (Command) and `Ctrl` (Control) are different keys.
    MacOs,
    /// Windows, Linux, others: `Cmd` is the Windows / Super key.
    Other,
}

impl Platform {
    /// The platform this binary runs on.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

// ------------------------------------------------------------------ errors --

/// Why a key string is not a [`Binding`]. The text is meant for the user.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// Nothing but whitespace.
    #[error("the key combination is empty")]
    Empty,
    /// `Ctrl++A`, `+A`, `A+`.
    #[error(
        "{0:?} has an empty part: write it as Modifier+Modifier+Key, for example Ctrl+Shift+1"
    )]
    EmptyPart(String),
    /// Only modifiers, like `Ctrl+Shift`.
    #[error("{0:?} has modifiers but no key: add one key, for example Ctrl+Shift+1")]
    MissingKey(String),
    /// `Ctrl+A+B`.
    #[error(
        "{text:?} has two keys ({first} and {second}): a combination is modifiers plus exactly one key"
    )]
    TooManyKeys {
        /// The whole string.
        text: String,
        /// First key found.
        first: String,
        /// Second key found.
        second: String,
    },
    /// A name that is not a key (see the module docs for the list).
    #[error("unknown key {0:?}: use a letter, a digit, F1-F24, Numpad0-9, or a name such as Space, Enter, Escape, Up, PageDown")]
    UnknownKey(String),
    /// A `Midi:...` string passed to the keyboard parser.
    #[error("{0:?} is a MIDI trigger, not a keyboard key")]
    Midi(String),
}

// -------------------------------------------------------------- modifiers --

/// Modifier keys of a combination.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    /// Control.
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// Command on macOS, Windows / Super key elsewhere.
    pub cmd: bool,
}

impl Mods {
    /// No modifier at all.
    pub const NONE: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: false,
        cmd: false,
    };

    /// Whether any modifier is down.
    pub fn any(self) -> bool {
        self.ctrl || self.alt || self.shift || self.cmd
    }
}

// ------------------------------------------------------------------- keys --

/// A key on the numeric keypad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumpadKey {
    /// `Numpad0`..`Numpad9`.
    Digit(u8),
    /// `NumpadAdd`.
    Add,
    /// `NumpadSubtract`.
    Subtract,
    /// `NumpadMultiply`.
    Multiply,
    /// `NumpadDivide`.
    Divide,
    /// `NumpadDecimal`.
    Decimal,
    /// `NumpadEnter`.
    Enter,
}

/// A named (non letter, digit, F or keypad) key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum NamedKey {
    Space,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Minus,
    Equals,
    Comma,
    Period,
    Slash,
    Backslash,
    Semicolon,
    Quote,
    Backtick,
    BracketLeft,
    BracketRight,
}

impl NamedKey {
    /// Every named key, for tables and tests.
    pub const ALL: [NamedKey; 26] = [
        Self::Space,
        Self::Enter,
        Self::Escape,
        Self::Tab,
        Self::Backspace,
        Self::Delete,
        Self::Insert,
        Self::Home,
        Self::End,
        Self::PageUp,
        Self::PageDown,
        Self::Up,
        Self::Down,
        Self::Left,
        Self::Right,
        Self::Minus,
        Self::Equals,
        Self::Comma,
        Self::Period,
        Self::Slash,
        Self::Backslash,
        Self::Semicolon,
        Self::Quote,
        Self::Backtick,
        Self::BracketLeft,
        Self::BracketRight,
    ];

    /// Canonical spelling.
    pub fn name(self) -> &'static str {
        match self {
            Self::Space => "Space",
            Self::Enter => "Enter",
            Self::Escape => "Escape",
            Self::Tab => "Tab",
            Self::Backspace => "Backspace",
            Self::Delete => "Delete",
            Self::Insert => "Insert",
            Self::Home => "Home",
            Self::End => "End",
            Self::PageUp => "PageUp",
            Self::PageDown => "PageDown",
            Self::Up => "Up",
            Self::Down => "Down",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Minus => "Minus",
            Self::Equals => "Equals",
            Self::Comma => "Comma",
            Self::Period => "Period",
            Self::Slash => "Slash",
            Self::Backslash => "Backslash",
            Self::Semicolon => "Semicolon",
            Self::Quote => "Quote",
            Self::Backtick => "Backtick",
            Self::BracketLeft => "BracketLeft",
            Self::BracketRight => "BracketRight",
        }
    }

    fn from_lower(lower: &str) -> Option<Self> {
        Some(match lower {
            "space" | "spacebar" => Self::Space,
            "enter" | "return" => Self::Enter,
            "escape" | "esc" => Self::Escape,
            "tab" => Self::Tab,
            "backspace" => Self::Backspace,
            "delete" | "del" => Self::Delete,
            "insert" | "ins" => Self::Insert,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" | "pgup" => Self::PageUp,
            "pagedown" | "pgdn" | "pgdown" => Self::PageDown,
            "up" | "arrowup" => Self::Up,
            "down" | "arrowdown" => Self::Down,
            "left" | "arrowleft" => Self::Left,
            "right" | "arrowright" => Self::Right,
            "minus" | "-" => Self::Minus,
            "equals" | "equal" | "=" => Self::Equals,
            "comma" | "," => Self::Comma,
            "period" | "dot" | "." => Self::Period,
            "slash" | "/" => Self::Slash,
            "backslash" | "\\" => Self::Backslash,
            "semicolon" | ";" => Self::Semicolon,
            "quote" | "apostrophe" | "'" => Self::Quote,
            "backtick" | "grave" | "`" => Self::Backtick,
            "bracketleft" | "openbracket" | "[" => Self::BracketLeft,
            "bracketright" | "closebracket" | "]" => Self::BracketRight,
            _ => return None,
        })
    }
}

/// The key of a combination (the part that is not a modifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// `A`..`Z` (stored upper case).
    Letter(char),
    /// `0`..`9` on the main row.
    Digit(u8),
    /// `F1`..`F24`.
    Function(u8),
    /// A numeric keypad key.
    Numpad(NumpadKey),
    /// A named key.
    Named(NamedKey),
}

impl Key {
    fn parse(token: &str) -> Result<Self, ParseError> {
        let lower = token.to_lowercase();
        let unknown = || ParseError::UnknownKey(token.to_string());

        let mut chars = lower.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_lowercase() {
                return Ok(Self::Letter(c.to_ascii_uppercase()));
            }
            if let Some(d) = c.to_digit(10) {
                return Ok(Self::Digit(d as u8));
            }
        }
        if let Some(named) = NamedKey::from_lower(&lower) {
            return Ok(Self::Named(named));
        }
        if let Some(n) = lower.strip_prefix('f') {
            if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
                return match n.parse::<u8>() {
                    Ok(n @ 1..=24) => Ok(Self::Function(n)),
                    _ => Err(unknown()),
                };
            }
        }
        for prefix in ["numpad", "kp", "num"] {
            if let Some(rest) = lower.strip_prefix(prefix) {
                let rest = rest.trim_start_matches('_');
                let numpad = match rest {
                    "add" | "plus" => NumpadKey::Add,
                    "subtract" | "minus" => NumpadKey::Subtract,
                    "multiply" | "asterisk" => NumpadKey::Multiply,
                    "divide" => NumpadKey::Divide,
                    "decimal" | "dot" => NumpadKey::Decimal,
                    "enter" => NumpadKey::Enter,
                    d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => {
                        NumpadKey::Digit(d.as_bytes()[0] - b'0')
                    }
                    _ => continue,
                };
                return Ok(Self::Numpad(numpad));
            }
        }
        Err(unknown())
    }

    /// Whether the key sits on the numeric keypad.
    pub fn is_numpad(self) -> bool {
        matches!(self, Self::Numpad(_))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Letter(c) => write!(f, "{c}"),
            Self::Digit(d) => write!(f, "{d}"),
            Self::Function(n) => write!(f, "F{n}"),
            Self::Numpad(NumpadKey::Digit(d)) => write!(f, "Numpad{d}"),
            Self::Numpad(NumpadKey::Add) => f.write_str("NumpadAdd"),
            Self::Numpad(NumpadKey::Subtract) => f.write_str("NumpadSubtract"),
            Self::Numpad(NumpadKey::Multiply) => f.write_str("NumpadMultiply"),
            Self::Numpad(NumpadKey::Divide) => f.write_str("NumpadDivide"),
            Self::Numpad(NumpadKey::Decimal) => f.write_str("NumpadDecimal"),
            Self::Numpad(NumpadKey::Enter) => f.write_str("NumpadEnter"),
            Self::Named(n) => f.write_str(n.name()),
        }
    }
}

// ---------------------------------------------------------------- binding --

/// A parsed key combination: modifiers plus one key.
///
/// `Display` prints the canonical form, which parses back to an equal value
/// (on the same platform). See the module docs for the syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Binding {
    /// Modifier keys.
    pub mods: Mods,
    /// The key.
    pub key: Key,
}

impl Binding {
    /// Parse for the platform this binary runs on.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        Self::parse_for(text, Platform::current())
    }

    /// Parse for an explicit platform (`CmdOrCtrl` resolves differently).
    pub fn parse_for(text: &str, platform: Platform) -> Result<Self, ParseError> {
        if text.trim().is_empty() {
            return Err(ParseError::Empty);
        }
        if is_midi_key(text) {
            return Err(ParseError::Midi(text.trim().to_string()));
        }
        let mut mods = Mods::NONE;
        let mut key: Option<(Key, &str)> = None;
        for part in text.split('+') {
            let part = part.trim();
            if part.is_empty() {
                return Err(ParseError::EmptyPart(text.trim().to_string()));
            }
            match part.to_lowercase().as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "alt" | "option" | "opt" => mods.alt = true,
                "shift" => mods.shift = true,
                "cmd" | "command" | "super" | "meta" | "win" | "windows" => mods.cmd = true,
                "cmdorctrl" | "commandorcontrol" | "cmdorcontrol" | "commandorctrl"
                | "ctrlorcmd" | "controlorcommand" | "controlorcmd" => match platform {
                    Platform::MacOs => mods.cmd = true,
                    Platform::Other => mods.ctrl = true,
                },
                _ => {
                    let parsed = Key::parse(part)?;
                    if let Some((_, first)) = key {
                        return Err(ParseError::TooManyKeys {
                            text: text.trim().to_string(),
                            first: first.to_string(),
                            second: part.to_string(),
                        });
                    }
                    key = Some((parsed, part));
                }
            }
        }
        match key {
            Some((key, _)) => Ok(Self { mods, key }),
            None => Err(ParseError::MissingKey(text.trim().to_string())),
        }
    }

    /// A key without modifiers.
    pub fn key_only(key: Key) -> Self {
        Self {
            mods: Mods::NONE,
            key,
        }
    }

    /// Whether the combination can be typed into a text field (a plain key or
    /// Shift+key): such a binding must not fire while the user is typing.
    /// Ctrl / Alt / Cmd combinations and F-keys are text-safe.
    pub fn is_text_safe(&self) -> bool {
        self.mods.ctrl || self.mods.alt || self.mods.cmd || matches!(self.key, Key::Function(_))
    }

    /// Whether the combination is allowed as a system-wide shortcut. Without
    /// Ctrl / Alt / Cmd a plain key (`Space`, `A`, `Shift+1`) would be stolen
    /// from every other application, so only text-safe combinations qualify.
    pub fn is_global_safe(&self) -> bool {
        self.is_text_safe()
    }

    /// The label for the user's own platform: `Cmd` is shown as `Win` on
    /// Windows and `Super` on Linux, `Alt` as `Option` on macOS.
    pub fn label_for(&self, platform: Platform) -> String {
        let mut out = String::new();
        let names: [(bool, &str); 4] = match platform {
            Platform::MacOs => [
                (self.mods.ctrl, "Ctrl"),
                (self.mods.alt, "Option"),
                (self.mods.shift, "Shift"),
                (self.mods.cmd, "Cmd"),
            ],
            Platform::Other => [
                (self.mods.ctrl, "Ctrl"),
                (self.mods.alt, "Alt"),
                (self.mods.shift, "Shift"),
                (self.mods.cmd, if cfg!(windows) { "Win" } else { "Super" }),
            ],
        };
        for (on, name) in names {
            if on {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(&self.key.to_string());
        out
    }
}

impl fmt::Display for Binding {
    /// The canonical form: `Ctrl+Alt+Shift+Cmd+Key`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.mods.ctrl, "Ctrl"),
            (self.mods.alt, "Alt"),
            (self.mods.shift, "Shift"),
            (self.mods.cmd, "Cmd"),
        ] {
            if on {
                f.write_str(name)?;
                f.write_str("+")?;
            }
        }
        write!(f, "{}", self.key)
    }
}

impl FromStr for Binding {
    type Err = ParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: Platform = Platform::MacOs;
    const OTHER: Platform = Platform::Other;

    fn p(text: &str) -> Binding {
        Binding::parse_for(text, OTHER).unwrap()
    }

    #[test]
    fn parses_modifiers_in_any_order_and_case() {
        let a = p("ctrl+SHIFT+1");
        let b = p("Shift + Ctrl + 1");
        assert_eq!(a, b);
        assert_eq!(a.to_string(), "Ctrl+Shift+1");
        assert!(a.mods.ctrl && a.mods.shift && !a.mods.alt && !a.mods.cmd);
        assert_eq!(a.key, Key::Digit(1));
    }

    #[test]
    fn modifier_aliases() {
        assert_eq!(p("control+a").to_string(), "Ctrl+A");
        assert_eq!(p("option+a").to_string(), "Alt+A");
        assert_eq!(p("opt+a").to_string(), "Alt+A");
        for alias in ["cmd", "command", "super", "meta", "win", "windows"] {
            assert_eq!(p(&format!("{alias}+k")).to_string(), "Cmd+K");
        }
        // Duplicates collapse.
        assert_eq!(p("ctrl+ctrl+a").to_string(), "Ctrl+A");
    }

    #[test]
    fn cmd_or_ctrl_follows_the_platform() {
        let mac = Binding::parse_for("CmdOrCtrl+K", MAC).unwrap();
        let other = Binding::parse_for("CmdOrCtrl+K", OTHER).unwrap();
        assert_eq!(mac.to_string(), "Cmd+K");
        assert_eq!(other.to_string(), "Ctrl+K");
        assert_ne!(mac, other);
        for alias in ["commandorcontrol", "CtrlOrCmd", "cmdorcontrol"] {
            assert_eq!(Binding::parse_for(&format!("{alias}+k"), MAC).unwrap(), mac);
        }
    }

    #[test]
    fn cmd_and_ctrl_are_different_keys() {
        assert_ne!(
            Binding::parse_for("Cmd+K", MAC).unwrap(),
            Binding::parse_for("Ctrl+K", MAC).unwrap()
        );
    }

    #[test]
    fn function_numpad_and_named_keys() {
        assert_eq!(p("alt+f5").key, Key::Function(5));
        assert_eq!(p("F24").key, Key::Function(24));
        assert_eq!(p("NumPad1").key, Key::Numpad(NumpadKey::Digit(1)));
        assert_eq!(p("numpad1").to_string(), "Numpad1");
        assert_eq!(p("num_7").key, Key::Numpad(NumpadKey::Digit(7)));
        assert_eq!(p("kp9").key, Key::Numpad(NumpadKey::Digit(9)));
        assert_eq!(p("NumpadPlus").to_string(), "NumpadAdd");
        assert_eq!(p("space").to_string(), "Space");
        assert_eq!(p("esc").to_string(), "Escape");
        assert_eq!(p("return").to_string(), "Enter");
        assert_eq!(p("pgdn").to_string(), "PageDown");
        assert_eq!(p("ArrowUp").to_string(), "Up");
        assert_eq!(p("ctrl+-").to_string(), "Ctrl+Minus");
        assert_eq!(p("ctrl+/").to_string(), "Ctrl+Slash");
        assert_eq!(p("`").to_string(), "Backtick");
    }

    #[test]
    fn every_key_round_trips_through_text() {
        let mut keys: Vec<Key> = ('A'..='Z').map(Key::Letter).collect();
        keys.extend((0..=9).map(Key::Digit));
        keys.extend((1..=24).map(Key::Function));
        keys.extend((0..=9).map(|d| Key::Numpad(NumpadKey::Digit(d))));
        keys.extend(
            [
                NumpadKey::Add,
                NumpadKey::Subtract,
                NumpadKey::Multiply,
                NumpadKey::Divide,
                NumpadKey::Decimal,
                NumpadKey::Enter,
            ]
            .map(Key::Numpad),
        );
        keys.extend(NamedKey::ALL.map(Key::Named));
        for key in keys {
            for mods in [
                Mods::NONE,
                Mods {
                    ctrl: true,
                    alt: true,
                    shift: true,
                    cmd: true,
                },
                Mods {
                    shift: true,
                    ..Mods::NONE
                },
            ] {
                let binding = Binding { mods, key };
                let text = binding.to_string();
                assert_eq!(p(&text), binding, "{text}");
                assert_eq!(p(&text).to_string(), text);
            }
        }
    }

    #[test]
    fn display_order_matches_config_canonical_form() {
        for text in [
            "shift+ctrl+a",
            "cmd+alt+f5",
            "alt+shift+ctrl+cmd+1",
            "space",
        ] {
            assert_eq!(
                Some(p(text).to_string()),
                crate::config::canonical_hotkey(text),
                "{text}"
            );
        }
    }

    #[test]
    fn errors_are_readable() {
        assert_eq!(Binding::parse(""), Err(ParseError::Empty));
        assert_eq!(Binding::parse("  "), Err(ParseError::Empty));
        assert!(matches!(
            Binding::parse("Ctrl+"),
            Err(ParseError::EmptyPart(_))
        ));
        assert!(matches!(
            Binding::parse("+A"),
            Err(ParseError::EmptyPart(_))
        ));
        assert!(matches!(
            Binding::parse("Ctrl++A"),
            Err(ParseError::EmptyPart(_))
        ));
        assert!(matches!(
            Binding::parse("Ctrl+Shift"),
            Err(ParseError::MissingKey(_))
        ));
        let two = Binding::parse("Ctrl+A+B").unwrap_err();
        assert!(matches!(two, ParseError::TooManyKeys { .. }));
        assert!(two.to_string().contains("exactly one key"));
        for bad in ["Ctrl+Banana", "F25", "F0", "Numpad10", "Ф", "Ctrl+Plus"] {
            let err = Binding::parse(bad).unwrap_err();
            assert!(matches!(err, ParseError::UnknownKey(_)), "{bad}: {err:?}");
            assert!(err.to_string().contains("unknown key"));
        }
        assert!(matches!(
            Binding::parse("Midi:Ch1:Note60"),
            Err(ParseError::Midi(_))
        ));
    }

    #[test]
    fn midi_keys_are_recognized() {
        assert!(is_midi_key("Midi:Ch1:Note60"));
        assert!(is_midi_key("  MIDI:cc7"));
        assert!(!is_midi_key("Ctrl+M"));
        assert!(!is_midi_key("Mi"));
    }

    #[test]
    fn text_and_global_safety() {
        assert!(!p("a").is_text_safe());
        assert!(!p("shift+1").is_text_safe());
        assert!(!p("space").is_text_safe());
        assert!(p("ctrl+a").is_text_safe());
        assert!(p("alt+1").is_text_safe());
        assert!(p("cmd+1").is_text_safe());
        assert!(p("f5").is_text_safe());
        assert!(!p("a").is_global_safe());
        assert!(p("ctrl+shift+1").is_global_safe());
    }

    #[test]
    fn labels_follow_the_platform() {
        let b = Binding::parse_for("Alt+Cmd+K", MAC).unwrap();
        assert_eq!(b.label_for(MAC), "Option+Cmd+K");
        assert!(b.label_for(OTHER).starts_with("Alt+"));
    }

    #[test]
    fn from_str_uses_the_current_platform() {
        let b: Binding = "ctrl+1".parse().unwrap();
        assert_eq!(b.to_string(), "Ctrl+1");
    }
}
