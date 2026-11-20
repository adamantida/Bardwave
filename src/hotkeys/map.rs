//! The configured bindings as a lookup table, conflicts, and the event type
//! every source (window, system, MIDI) produces.

use std::collections::HashMap;

use crate::config::{HotkeyAction, HotkeyConfig};

use super::{is_midi_key, Binding, ParseError, Platform};

/// Whether the trigger went down or up. Only `DuckHold` cares about the
/// release; every other action fires on the press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyPhase {
    /// Key (or note) down.
    Pressed,
    /// Key (or note) up.
    Released,
}

/// Where an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// The console window had focus ([`poll_local`](super::poll_local)).
    Local,
    /// A system-wide shortcut ([`HotkeyService`](super::HotkeyService)).
    Global,
    /// A MIDI controller (feature `midi`).
    Midi,
}

/// A trigger fired: what to run and how the key moved.
#[derive(Debug, Clone, PartialEq)]
pub struct HotkeyEvent {
    /// Index of the `[[hotkeys]]` entry (position in `Config::hotkeys`).
    pub index: usize,
    /// The entry's action.
    pub action: HotkeyAction,
    /// Down or up.
    pub phase: KeyPhase,
    /// Which source produced it.
    pub source: Source,
}

/// A keyboard binding with its action.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyEntry {
    /// Index of the `[[hotkeys]]` entry.
    pub index: usize,
    /// The parsed combination.
    pub binding: Binding,
    /// The entry's action.
    pub action: HotkeyAction,
}

/// Why an entry is not in the map.
#[derive(Debug, Clone, PartialEq)]
pub enum ProblemKind {
    /// The key string does not parse.
    Invalid(ParseError),
    /// A MIDI trigger. Nothing in the app connects a MIDI device yet (the
    /// `midi` module is not wired into the GUI loop), so it never fires, with
    /// or without the `midi` cargo feature.
    MidiUnavailable,
    /// Another entry (index `by`) has the same combination and comes first,
    /// so this one never fires.
    Shadowed {
        /// The entry that wins.
        by: usize,
    },
}

/// One entry that did not make it into the map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapProblem {
    /// Index of the `[[hotkeys]]` entry.
    pub index: usize,
    /// Its key string as written.
    pub key: String,
    /// What is wrong.
    pub kind: ProblemKind,
}

impl MapProblem {
    /// A sentence for the settings dialog.
    pub fn message(&self) -> String {
        match &self.kind {
            ProblemKind::Invalid(e) => format!("hotkey {}: {e}", self.index + 1),
            ProblemKind::MidiUnavailable => format!(
                "hotkey {}: {:?} is a MIDI binding, but MIDI input is not connected in this build",
                self.index + 1,
                self.key
            ),
            ProblemKind::Shadowed { by } => format!(
                "hotkey {}: {:?} is already used by hotkey {}, so this one never fires",
                self.index + 1,
                self.key,
                by + 1
            ),
        }
    }
}

/// The keyboard bindings of a config, parsed. Invalid and duplicated entries
/// are skipped (and listed in [`problems`](Self::problems)); the first of
/// several equal combinations wins, which is also what a conflict check
/// ([`find_conflicts`]) reports to the user.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HotkeyMap {
    entries: Vec<KeyEntry>,
    problems: Vec<MapProblem>,
    by_binding: HashMap<Binding, usize>,
}

impl HotkeyMap {
    /// Build for the platform this binary runs on.
    pub fn build(hotkeys: &[HotkeyConfig]) -> Self {
        Self::build_for(hotkeys, Platform::current())
    }

    /// Build for an explicit platform.
    pub fn build_for(hotkeys: &[HotkeyConfig], platform: Platform) -> Self {
        let mut map = Self::default();
        for (index, hotkey) in hotkeys.iter().enumerate() {
            if is_midi_key(&hotkey.key) {
                // Keep reporting it until `MidiService` is wired into the GUI
                // loop: the `midi` feature alone does not make it fire.
                map.problems.push(MapProblem {
                    index,
                    key: hotkey.key.clone(),
                    kind: ProblemKind::MidiUnavailable,
                });
                continue;
            }
            let binding = match Binding::parse_for(&hotkey.key, platform) {
                Ok(binding) => binding,
                Err(e) => {
                    map.problems.push(MapProblem {
                        index,
                        key: hotkey.key.clone(),
                        kind: ProblemKind::Invalid(e),
                    });
                    continue;
                }
            };
            if let Some(&first) = map.by_binding.get(&binding) {
                map.problems.push(MapProblem {
                    index,
                    key: hotkey.key.clone(),
                    kind: ProblemKind::Shadowed {
                        by: map.entries[first].index,
                    },
                });
                continue;
            }
            map.by_binding.insert(binding, map.entries.len());
            map.entries.push(KeyEntry {
                index,
                binding,
                action: hotkey.action.clone(),
            });
        }
        map
    }

    /// The usable keyboard bindings, in config order.
    pub fn entries(&self) -> &[KeyEntry] {
        &self.entries
    }

    /// Entries that were skipped, with the reason.
    pub fn problems(&self) -> &[MapProblem] {
        &self.problems
    }

    /// The entry bound to a combination.
    pub fn find(&self, binding: &Binding) -> Option<&KeyEntry> {
        self.by_binding.get(binding).map(|&i| &self.entries[i])
    }

    /// Whether there is nothing to listen for.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Two `[[hotkeys]]` entries with the same combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyConflict {
    /// Index of the earlier entry (the one that wins).
    pub first: usize,
    /// Index of the later entry.
    pub second: usize,
    /// The shared combination in canonical form (`"Ctrl+Shift+1"`,
    /// `"midi:ch1:note60"` for MIDI).
    pub key: String,
}

/// Every pair of entries that use the same combination, for the platform this
/// binary runs on. Three entries on one key give three pairs. Entries whose key
/// does not parse are ignored here (see [`HotkeyMap::problems`]). Pairs are
/// ordered by `(first, second)`.
pub fn find_conflicts(hotkeys: &[HotkeyConfig]) -> Vec<HotkeyConflict> {
    find_conflicts_for(hotkeys, Platform::current())
}

/// [`find_conflicts`] for an explicit platform (`CmdOrCtrl` resolves
/// differently, so `CmdOrCtrl+K` and `Ctrl+K` conflict only off macOS).
pub fn find_conflicts_for(hotkeys: &[HotkeyConfig], platform: Platform) -> Vec<HotkeyConflict> {
    let keys: Vec<Option<String>> = hotkeys
        .iter()
        .map(|h| {
            if is_midi_key(&h.key) {
                Some(
                    h.key
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect::<String>()
                        .to_lowercase(),
                )
            } else {
                Binding::parse_for(&h.key, platform)
                    .ok()
                    .map(|b| b.to_string())
            }
        })
        .collect();
    let mut out = Vec::new();
    for first in 0..keys.len() {
        let Some(a) = &keys[first] else { continue };
        for (second, b) in keys.iter().enumerate().skip(first + 1) {
            if b.as_ref() == Some(a) {
                out.push(HotkeyConflict {
                    first,
                    second,
                    key: a.clone(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(key: &str, action: HotkeyAction) -> HotkeyConfig {
        HotkeyConfig {
            key: key.to_string(),
            action,
        }
    }

    #[test]
    fn conflicts_are_pairs_in_any_spelling() {
        let list = vec![
            cfg("Ctrl+1", HotkeyAction::StopAll),
            cfg("F5", HotkeyAction::StopScene),
            cfg("control+1", HotkeyAction::MasterUp),
            cfg("Ctrl+1", HotkeyAction::MasterDown),
            cfg("nonsense+", HotkeyAction::MasterMute),
            cfg("nonsense+", HotkeyAction::MasterMute),
        ];
        let conflicts = find_conflicts_for(&list, Platform::Other);
        let pairs: Vec<(usize, usize)> = conflicts.iter().map(|c| (c.first, c.second)).collect();
        assert_eq!(pairs, vec![(0, 2), (0, 3), (2, 3)]);
        assert!(conflicts.iter().all(|c| c.key == "Ctrl+1"));
    }

    #[test]
    fn no_conflicts_when_all_differ() {
        let list = vec![
            cfg("Ctrl+1", HotkeyAction::StopAll),
            cfg("Ctrl+2", HotkeyAction::StopAll),
            cfg("Cmd+1", HotkeyAction::StopAll),
        ];
        assert!(find_conflicts_for(&list, Platform::MacOs).is_empty());
    }

    #[test]
    fn cmd_or_ctrl_conflicts_depend_on_platform() {
        let list = vec![
            cfg("CmdOrCtrl+K", HotkeyAction::StopAll),
            cfg("Ctrl+K", HotkeyAction::StopScene),
        ];
        assert_eq!(find_conflicts_for(&list, Platform::Other).len(), 1);
        assert!(find_conflicts_for(&list, Platform::MacOs).is_empty());
    }

    #[test]
    fn midi_triggers_conflict_by_text() {
        let list = vec![
            cfg("Midi:Ch1:Note60", HotkeyAction::StopAll),
            cfg("midi: ch1:note60", HotkeyAction::StopScene),
            cfg("Midi:Ch2:Note60", HotkeyAction::StopScene),
        ];
        let c = find_conflicts(&list);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].first, c[0].second), (0, 1));
    }

    #[test]
    fn map_first_wins_and_reports_problems() {
        let list = vec![
            cfg("Ctrl+1", HotkeyAction::StopAll),
            cfg("Ctrl+Banana", HotkeyAction::StopAll),
            cfg("control+1", HotkeyAction::StopScene),
            cfg("F5", HotkeyAction::MasterUp),
            cfg("Midi:Note60", HotkeyAction::MasterUp),
        ];
        let map = HotkeyMap::build_for(&list, Platform::Other);
        assert_eq!(map.entries().len(), 2);
        assert_eq!(map.entries()[0].index, 0);
        assert_eq!(map.entries()[1].index, 3);
        let b = Binding::parse_for("ctrl+1", Platform::Other).unwrap();
        assert_eq!(map.find(&b).unwrap().action, HotkeyAction::StopAll);

        let problems = map.problems();
        assert!(problems
            .iter()
            .any(|p| p.index == 1 && matches!(p.kind, ProblemKind::Invalid(_))));
        assert!(problems
            .iter()
            .any(|p| p.index == 2 && p.kind == ProblemKind::Shadowed { by: 0 }));
        // Reported in every build: MidiService is not connected to the GUI, so
        // a MIDI binding never fires, even with `--features midi`.
        assert!(problems
            .iter()
            .any(|p| p.index == 4 && p.kind == ProblemKind::MidiUnavailable));
        for p in problems {
            assert!(!p.message().is_empty());
        }
    }

    #[test]
    fn empty_map() {
        assert!(HotkeyMap::build(&[]).is_empty());
    }
}
