//! Shortcuts while the console window has focus (egui).
//!
//! # How it fits a frame
//!
//! Call [`poll_local`] at the very start of `App::update`, before any widget
//! is drawn: it looks for the bindings of a [`HotkeyMap`] in this frame's key
//! events and **removes** the events it used ([`consume_egui`]), so the key
//! does not also type a character or press a focused button. Then feed the
//! returned [`HotkeyEvent`]s to
//! [`HotkeyDispatcher::handle`](super::HotkeyDispatcher::handle).
//!
//! # Text fields
//!
//! While a widget wants the keyboard (`Context::wants_keyboard_input`, true
//! when a text field has focus), only [text-safe](Binding::is_text_safe)
//! bindings fire (those with Ctrl / Alt / Cmd, and F-keys). A plain `1`,
//! `Space`, `Shift+A` or `Escape` stays with the text field.
//!
//! # What egui cannot see
//!
//! * The numeric keypad: egui reports keypad keys as the main-row digits, so
//!   `Numpad…` bindings never match here ([`Binding::is_locally_matchable`]);
//!   they work as global shortcuts only. (Keypad keys would otherwise fire the
//!   main-row `1` binding.)
//! * The Windows / Super key: egui has no such modifier, so `Cmd` bindings
//!   match locally on macOS only.
//!
//! # Layouts
//!
//! A key matches either by the logical key egui reports or by the physical
//! position (`physical_key`), so `Shift+1` still matches although the logical
//! key is `!`, and a binding keeps working on a non-Latin layout.
//!
//! # Together with global shortcuts
//!
//! A registered global shortcut is also delivered while the window has focus
//! (and the operating system swallows the key). To avoid firing twice, pass the
//! [`HotkeyService`] to [`poll_local`]: bindings it has registered are skipped.

use egui::{Event, InputState, Key as EKey, Modifiers};

use super::map::{HotkeyEvent, HotkeyMap, KeyPhase, Source};
use super::{Binding, HotkeyService, Key, NamedKey, Platform};

const LETTERS: [EKey; 26] = [
    EKey::A,
    EKey::B,
    EKey::C,
    EKey::D,
    EKey::E,
    EKey::F,
    EKey::G,
    EKey::H,
    EKey::I,
    EKey::J,
    EKey::K,
    EKey::L,
    EKey::M,
    EKey::N,
    EKey::O,
    EKey::P,
    EKey::Q,
    EKey::R,
    EKey::S,
    EKey::T,
    EKey::U,
    EKey::V,
    EKey::W,
    EKey::X,
    EKey::Y,
    EKey::Z,
];

const DIGITS: [EKey; 10] = [
    EKey::Num0,
    EKey::Num1,
    EKey::Num2,
    EKey::Num3,
    EKey::Num4,
    EKey::Num5,
    EKey::Num6,
    EKey::Num7,
    EKey::Num8,
    EKey::Num9,
];

const FUNCTION: [EKey; 24] = [
    EKey::F1,
    EKey::F2,
    EKey::F3,
    EKey::F4,
    EKey::F5,
    EKey::F6,
    EKey::F7,
    EKey::F8,
    EKey::F9,
    EKey::F10,
    EKey::F11,
    EKey::F12,
    EKey::F13,
    EKey::F14,
    EKey::F15,
    EKey::F16,
    EKey::F17,
    EKey::F18,
    EKey::F19,
    EKey::F20,
    EKey::F21,
    EKey::F22,
    EKey::F23,
    EKey::F24,
];

impl Key {
    /// The egui key, `None` for keypad keys (egui cannot tell them apart from
    /// the main row).
    pub fn to_egui(self) -> Option<EKey> {
        Some(match self {
            Key::Letter(c) => *LETTERS.get((c as u8).checked_sub(b'A')? as usize)?,
            Key::Digit(d) => *DIGITS.get(d as usize)?,
            Key::Function(n) => *FUNCTION.get((n as usize).checked_sub(1)?)?,
            Key::Numpad(_) => return None,
            Key::Named(named) => match named {
                NamedKey::Space => EKey::Space,
                NamedKey::Enter => EKey::Enter,
                NamedKey::Escape => EKey::Escape,
                NamedKey::Tab => EKey::Tab,
                NamedKey::Backspace => EKey::Backspace,
                NamedKey::Delete => EKey::Delete,
                NamedKey::Insert => EKey::Insert,
                NamedKey::Home => EKey::Home,
                NamedKey::End => EKey::End,
                NamedKey::PageUp => EKey::PageUp,
                NamedKey::PageDown => EKey::PageDown,
                NamedKey::Up => EKey::ArrowUp,
                NamedKey::Down => EKey::ArrowDown,
                NamedKey::Left => EKey::ArrowLeft,
                NamedKey::Right => EKey::ArrowRight,
                NamedKey::Minus => EKey::Minus,
                NamedKey::Equals => EKey::Equals,
                NamedKey::Comma => EKey::Comma,
                NamedKey::Period => EKey::Period,
                NamedKey::Slash => EKey::Slash,
                NamedKey::Backslash => EKey::Backslash,
                NamedKey::Semicolon => EKey::Semicolon,
                NamedKey::Quote => EKey::Quote,
                NamedKey::Backtick => EKey::Backtick,
                NamedKey::BracketLeft => EKey::OpenBracket,
                NamedKey::BracketRight => EKey::CloseBracket,
            },
        })
    }
}

impl Binding {
    /// Whether [`matches_egui`] can ever match this binding on `platform`:
    /// not for keypad keys, and not for `Cmd` off macOS (see the module docs).
    pub fn is_locally_matchable(&self, platform: Platform) -> bool {
        self.key.to_egui().is_some() && (platform == Platform::MacOs || !self.mods.cmd)
    }
}

/// Whether the event's modifiers are exactly the binding's (`Cmd` is the
/// Command key on macOS; Ctrl is always the Control key, as on macOS they are
/// different keys).
fn mods_match(binding: &Binding, event: &Modifiers, platform: Platform) -> bool {
    let cmd_ok = match platform {
        Platform::MacOs => event.mac_cmd == binding.mods.cmd,
        Platform::Other => !binding.mods.cmd,
    };
    cmd_ok
        && event.ctrl == binding.mods.ctrl
        && event.alt == binding.mods.alt
        && event.shift == binding.mods.shift
}

fn key_matches(target: EKey, key: EKey, physical: Option<EKey>) -> bool {
    key == target || physical == Some(target)
}

/// Position of the first key-down event (not an auto-repeat) of the binding.
fn find_press(events: &[Event], binding: &Binding, platform: Platform) -> Option<usize> {
    let target = binding.key.to_egui()?;
    events.iter().position(|event| {
        matches!(
            event,
            Event::Key { key, physical_key, pressed: true, repeat: false, modifiers }
                if key_matches(target, *key, *physical_key)
                    && mods_match(binding, modifiers, platform)
        )
    })
}

/// Whether the binding was pressed this frame (see the module docs for what
/// "pressed" excludes). Does not consume the event.
pub fn matches_egui(input: &InputState, binding: &Binding) -> bool {
    matches_egui_for(input, binding, Platform::current())
}

/// [`matches_egui`] for an explicit platform.
pub fn matches_egui_for(input: &InputState, binding: &Binding, platform: Platform) -> bool {
    find_press(&input.events, binding, platform).is_some()
}

/// Like [`matches_egui`], and when it matched, remove the key-down event so no
/// widget sees it. Returns whether it matched.
pub fn consume_egui(input: &mut InputState, binding: &Binding) -> bool {
    consume_egui_for(input, binding, Platform::current())
}

/// [`consume_egui`] for an explicit platform.
pub fn consume_egui_for(input: &mut InputState, binding: &Binding, platform: Platform) -> bool {
    match find_press(&input.events, binding, platform) {
        Some(at) => {
            input.events.remove(at);
            true
        }
        None => false,
    }
}

/// Whether the binding's key went up this frame, whatever the modifiers are
/// by then (releasing Ctrl a moment before the letter must still end a
/// `DuckHold`). The event is consumed. Returns whether it did.
pub fn released_egui(input: &mut InputState, binding: &Binding) -> bool {
    let Some(target) = binding.key.to_egui() else {
        return false;
    };
    let at = input.events.iter().position(|event| {
        matches!(
            event,
            Event::Key { key, physical_key, pressed: false, .. }
                if key_matches(target, *key, *physical_key)
        )
    });
    match at {
        Some(at) => {
            input.events.remove(at);
            true
        }
        None => false,
    }
}

/// Collect this frame's local hotkeys and consume their key events. See the
/// module docs. `global`: skip bindings the system-wide service owns.
pub fn poll_local(
    ctx: &egui::Context,
    map: &HotkeyMap,
    global: Option<&HotkeyService>,
) -> Vec<HotkeyEvent> {
    poll_local_for(ctx, map, global, Platform::current())
}

/// [`poll_local`] for an explicit platform.
pub fn poll_local_for(
    ctx: &egui::Context,
    map: &HotkeyMap,
    global: Option<&HotkeyService>,
    platform: Platform,
) -> Vec<HotkeyEvent> {
    if map.is_empty() {
        return Vec::new();
    }
    let typing = ctx.wants_keyboard_input();
    let mut fired = Vec::new();
    ctx.input_mut(|input| {
        for entry in map.entries() {
            if typing && !entry.binding.is_text_safe() {
                continue;
            }
            if global.is_some_and(|service| service.covers(&entry.binding)) {
                continue;
            }
            let phase = if consume_egui_for(input, &entry.binding, platform) {
                KeyPhase::Pressed
            } else if matches!(entry.action, crate::config::HotkeyAction::DuckHold)
                && released_egui(input, &entry.binding)
            {
                KeyPhase::Released
            } else {
                continue;
            };
            fired.push(HotkeyEvent {
                index: entry.index,
                action: entry.action.clone(),
                phase,
                source: Source::Local,
            });
        }
    });
    fired
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HotkeyAction, HotkeyConfig};
    use egui::RawInput;

    const MAC: Platform = Platform::MacOs;
    const OTHER: Platform = Platform::Other;

    fn key_event(key: EKey, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    fn mods(ctrl: bool, shift: bool, alt: bool, mac_cmd: bool) -> Modifiers {
        Modifiers {
            alt,
            ctrl,
            shift,
            mac_cmd,
            command: ctrl || mac_cmd,
        }
    }

    fn b(text: &str, platform: Platform) -> Binding {
        Binding::parse_for(text, platform).unwrap()
    }

    /// Run one frame with these events and return what `f` returns.
    fn frame<R>(ctx: &egui::Context, events: Vec<Event>, f: impl FnOnce(&egui::Context) -> R) -> R {
        let mut out = None;
        let mut f = Some(f);
        let raw = RawInput {
            events,
            ..RawInput::default()
        };
        let _ = ctx.run(raw, |ctx| {
            if let Some(f) = f.take() {
                out = Some(f(ctx));
            }
        });
        out.unwrap()
    }

    #[test]
    fn matches_exact_modifiers_only() {
        let ctx = egui::Context::default();
        let ev = vec![key_event(EKey::Num1, true, mods(true, true, false, false))];
        frame(&ctx, ev, |ctx| {
            ctx.input(|i| {
                assert!(matches_egui_for(i, &b("Ctrl+Shift+1", OTHER), OTHER));
                assert!(!matches_egui_for(i, &b("Ctrl+1", OTHER), OTHER));
                assert!(!matches_egui_for(i, &b("Ctrl+Shift+Alt+1", OTHER), OTHER));
                assert!(!matches_egui_for(i, &b("Ctrl+Shift+2", OTHER), OTHER));
            });
        });
    }

    #[test]
    fn cmd_and_ctrl_stay_distinct_on_macos() {
        let ctx = egui::Context::default();
        let ev = vec![key_event(EKey::K, true, mods(false, false, false, true))];
        frame(&ctx, ev, |ctx| {
            ctx.input(|i| {
                assert!(matches_egui_for(i, &b("Cmd+K", MAC), MAC));
                assert!(!matches_egui_for(i, &b("Ctrl+K", MAC), MAC));
            });
        });
        // A fresh context: in this one K is still down, so a second press
        // would be an auto-repeat.
        let ctx = egui::Context::default();
        let ev = vec![key_event(EKey::K, true, mods(true, false, false, false))];
        frame(&ctx, ev, |ctx| {
            ctx.input(|i| {
                assert!(matches_egui_for(i, &b("Ctrl+K", MAC), MAC));
                assert!(!matches_egui_for(i, &b("Cmd+K", MAC), MAC));
            });
        });
    }

    #[test]
    fn cmd_bindings_cannot_match_off_macos_and_numpad_never() {
        let cmd = b("Cmd+K", OTHER);
        let pad = b("Numpad1", OTHER);
        assert!(!cmd.is_locally_matchable(OTHER));
        assert!(cmd.is_locally_matchable(MAC));
        assert!(!pad.is_locally_matchable(MAC));
        assert!(b("Ctrl+1", OTHER).is_locally_matchable(OTHER));
        let ctx = egui::Context::default();
        let ev = vec![key_event(EKey::Num1, true, Modifiers::NONE)];
        frame(&ctx, ev, |ctx| {
            ctx.input(|i| assert!(!matches_egui_for(i, &pad, OTHER)));
        });
    }

    #[test]
    fn physical_key_matches_a_shifted_symbol() {
        let ctx = egui::Context::default();
        // Shift+1 on a US layout: the logical key is another symbol.
        let ev = vec![Event::Key {
            key: EKey::Questionmark,
            physical_key: Some(EKey::Num1),
            pressed: true,
            repeat: false,
            modifiers: mods(false, true, false, false),
        }];
        frame(&ctx, ev, |ctx| {
            ctx.input(|i| assert!(matches_egui_for(i, &b("Shift+1", OTHER), OTHER)));
        });
    }

    #[test]
    fn repeat_and_release_do_not_press() {
        let ctx = egui::Context::default();
        // egui decides what a repeat is itself: a key that is already down.
        let first = vec![key_event(EKey::A, true, Modifiers::NONE)];
        frame(&ctx, first, |ctx| {
            ctx.input(|i| assert!(matches_egui_for(i, &b("A", OTHER), OTHER)));
        });
        let again = vec![
            key_event(EKey::A, true, Modifiers::NONE),
            key_event(EKey::B, false, Modifiers::NONE),
        ];
        frame(&ctx, again, |ctx| {
            ctx.input(|i| {
                assert!(!matches_egui_for(i, &b("A", OTHER), OTHER), "auto-repeat");
                assert!(!matches_egui_for(i, &b("B", OTHER), OTHER), "release");
            });
        });
    }

    #[test]
    fn consuming_removes_the_event_and_the_text_it_would_type() {
        let ctx = egui::Context::default();
        let ev = vec![
            key_event(EKey::Num1, true, Modifiers::NONE),
            Event::Text("1".into()),
        ];
        frame(&ctx, ev, |ctx| {
            let binding = b("1", OTHER);
            ctx.input_mut(|i| {
                assert!(consume_egui_for(i, &binding, OTHER));
                // A second look finds nothing: the event is gone.
                assert!(!consume_egui_for(i, &binding, OTHER));
                assert!(!matches_egui_for(i, &binding, OTHER));
            });
        });
    }

    #[test]
    fn consuming_leaves_other_keys_alone() {
        let ctx = egui::Context::default();
        let ev = vec![
            key_event(EKey::A, true, Modifiers::NONE),
            key_event(EKey::B, true, Modifiers::NONE),
        ];
        frame(&ctx, ev, |ctx| {
            ctx.input_mut(|i| {
                assert!(consume_egui_for(i, &b("A", OTHER), OTHER));
                assert!(matches_egui_for(i, &b("B", OTHER), OTHER));
            });
        });
    }

    fn map_of(list: &[(&str, HotkeyAction)], platform: Platform) -> HotkeyMap {
        let configs: Vec<HotkeyConfig> = list
            .iter()
            .map(|(k, a)| HotkeyConfig {
                key: k.to_string(),
                action: a.clone(),
            })
            .collect();
        HotkeyMap::build_for(&configs, platform)
    }

    #[test]
    fn poll_local_fires_and_consumes() {
        let ctx = egui::Context::default();
        let map = map_of(
            &[
                ("Ctrl+1", HotkeyAction::StopAll),
                ("F5", HotkeyAction::MasterUp),
            ],
            OTHER,
        );
        let ev = vec![key_event(EKey::Num1, true, mods(true, false, false, false))];
        let (fired, left) = frame(&ctx, ev, |ctx| {
            let fired = poll_local_for(ctx, &map, None, OTHER);
            let left = ctx.input(|i| i.events.len());
            (fired, left)
        });
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].action, HotkeyAction::StopAll);
        assert_eq!(fired[0].index, 0);
        assert_eq!(fired[0].phase, KeyPhase::Pressed);
        assert_eq!(fired[0].source, Source::Local);
        assert_eq!(left, 0, "the key event must be consumed");
    }

    #[test]
    fn typing_blocks_plain_keys_but_not_ctrl_or_f_keys() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("note");
        let mut text = String::new();
        // Frame 1: a text field takes focus.
        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut text).id(id));
                r.request_focus();
            });
        });
        let map = map_of(
            &[
                ("1", HotkeyAction::StopAll),
                ("Space", HotkeyAction::StopScene),
                ("Ctrl+2", HotkeyAction::MasterUp),
                ("F5", HotkeyAction::MasterDown),
            ],
            OTHER,
        );
        let ev = vec![
            key_event(EKey::Num1, true, Modifiers::NONE),
            key_event(EKey::Space, true, Modifiers::NONE),
            key_event(EKey::Num2, true, mods(true, false, false, false)),
            key_event(EKey::F5, true, Modifiers::NONE),
        ];
        let (typing, fired, left) = frame(&ctx, ev, |ctx| {
            let typing = ctx.wants_keyboard_input();
            let fired = poll_local_for(ctx, &map, None, OTHER);
            let left: Vec<EKey> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        Event::Key { key, .. } => Some(*key),
                        _ => None,
                    })
                    .collect()
            });
            (typing, fired, left)
        });
        assert!(typing, "the text field should hold the keyboard");
        let actions: Vec<_> = fired.iter().map(|e| e.action.clone()).collect();
        assert_eq!(
            actions,
            vec![HotkeyAction::MasterUp, HotkeyAction::MasterDown]
        );
        // The plain keys stay in the queue for the text field.
        assert_eq!(left, vec![EKey::Num1, EKey::Space]);
    }

    #[test]
    fn plain_keys_fire_when_nothing_is_being_typed() {
        let ctx = egui::Context::default();
        let map = map_of(&[("1", HotkeyAction::StopAll)], OTHER);
        let ev = vec![key_event(EKey::Num1, true, Modifiers::NONE)];
        let fired = frame(&ctx, ev, |ctx| poll_local_for(ctx, &map, None, OTHER));
        assert_eq!(fired.len(), 1);
    }

    #[test]
    fn duck_hold_reports_the_release() {
        let ctx = egui::Context::default();
        let map = map_of(&[("Ctrl+D", HotkeyAction::DuckHold)], OTHER);
        let press = vec![key_event(EKey::D, true, mods(true, false, false, false))];
        let fired = frame(&ctx, press, |ctx| poll_local_for(ctx, &map, None, OTHER));
        assert_eq!(fired[0].phase, KeyPhase::Pressed);
        // Ctrl is let go first, then D: modifiers no longer match.
        let release = vec![key_event(EKey::D, false, Modifiers::NONE)];
        let fired = frame(&ctx, release, |ctx| poll_local_for(ctx, &map, None, OTHER));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].phase, KeyPhase::Released);
    }

    #[test]
    fn release_of_other_actions_is_not_reported() {
        let ctx = egui::Context::default();
        let map = map_of(&[("Ctrl+D", HotkeyAction::StopAll)], OTHER);
        let release = vec![key_event(EKey::D, false, Modifiers::NONE)];
        let fired = frame(&ctx, release, |ctx| poll_local_for(ctx, &map, None, OTHER));
        assert!(fired.is_empty());
    }

    #[test]
    fn egui_key_table_is_complete() {
        for c in 'A'..='Z' {
            assert!(Key::Letter(c).to_egui().is_some());
        }
        for d in 0..=9 {
            assert!(Key::Digit(d).to_egui().is_some());
        }
        for n in 1..=24 {
            assert!(Key::Function(n).to_egui().is_some());
        }
        for named in NamedKey::ALL {
            assert!(Key::Named(named).to_egui().is_some(), "{named:?}");
        }
        assert_eq!(Key::Letter('K').to_egui(), Some(EKey::K));
        assert_eq!(Key::Function(5).to_egui(), Some(EKey::F5));
        assert_eq!(Key::Digit(0).to_egui(), Some(EKey::Num0));
    }
}
