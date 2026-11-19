//! MIDI controllers as hotkeys. Compiled only with the `midi` cargo feature
//! (`cargo build --features midi`); the default build does not contain this
//! module and does not link `midir`.
//!
//! A MIDI binding is an ordinary `[[hotkeys]]` entry whose `key` starts with
//! `Midi:`, so the config schema is unchanged and the same actions are used:
//!
//! ```toml
//! [[hotkeys]]
//! key = "Midi:Ch1:Note60"        # channel 1, note 60 (middle C), any velocity
//! [hotkeys.action]
//! kind = "scene"
//! target = "…"
//!
//! [[hotkeys]]
//! key = "Midi:CC7"               # any channel, controller 7
//! [hotkeys.action]
//! kind = "master_mute"
//! ```
//!
//! * `Midi:[Ch<1-16>:]Note<0-127>` fires on note-on (velocity above 0) and
//!   reports the release on note-off (or note-on with velocity 0), which is
//!   what `DuckHold` needs.
//! * `Midi:[Ch<1-16>:]CC<0-127>` is a button made of a controller: a value of
//!   64 or more presses, below 64 releases (the usual footswitch behaviour). A
//!   knob that sweeps through 64 therefore triggers once per crossing.
//! * Without `Ch<n>` the binding listens on every channel.
//!
//! [`MidiMap`] is plain data and decodes raw MIDI bytes, so it is testable
//! without a device. [`MidiService`] opens an input port with `midir`, decodes
//! in midir's callback thread (not the audio thread) and queues
//! [`HotkeyEvent`]s for [`MidiService::poll`], with the same wake callback as
//! the global-hotkey service.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver};
use midir::{Ignore, MidiInput, MidiInputConnection};
use parking_lot::Mutex;
use thiserror::Error;

use crate::config::{HotkeyAction, HotkeyConfig};

use super::map::{HotkeyEvent, KeyPhase, Source};
use super::service::Wake;
use super::MIDI_PREFIX;

/// A trigger text that is not a MIDI binding.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MidiParseError {
    /// Does not start with `Midi:`.
    #[error("{0:?} is not a MIDI trigger (write it as Midi:Ch1:Note60 or Midi:CC7)")]
    NotMidi(String),
    /// Channel outside 1..=16.
    #[error("MIDI channel in {0:?} must be 1 to 16")]
    BadChannel(String),
    /// Note / controller number outside 0..=127 or not a number.
    #[error("MIDI note or controller number in {0:?} must be 0 to 127")]
    BadNumber(String),
    /// Neither `Note<n>` nor `CC<n>`.
    #[error("{0:?}: after the channel write Note<0-127> or CC<0-127>")]
    BadKind(String),
}

/// What a MIDI binding listens for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidiKind {
    /// A note number.
    Note(u8),
    /// A controller number.
    Cc(u8),
}

/// A parsed `Midi:...` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiTrigger {
    /// Channel `1..=16`, `None` = any.
    pub channel: Option<u8>,
    /// Note or controller.
    pub kind: MidiKind,
}

impl MidiTrigger {
    /// Parse `Midi:Ch1:Note60`, `midi:cc7`, `Midi: Ch16 : Note 0`.
    pub fn parse(text: &str) -> Result<Self, MidiParseError> {
        let compact: String = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase();
        let Some(rest) = compact.strip_prefix(MIDI_PREFIX) else {
            return Err(MidiParseError::NotMidi(text.trim().to_string()));
        };
        let (channel, kind_text) = match rest.split_once(':') {
            Some((ch, kind)) => {
                let n = ch
                    .strip_prefix("ch")
                    .and_then(|n| n.parse::<u8>().ok())
                    .filter(|n| (1..=16).contains(n))
                    .ok_or_else(|| MidiParseError::BadChannel(text.trim().to_string()))?;
                (Some(n), kind)
            }
            None => (None, rest),
        };
        let number = |digits: &str| {
            digits
                .parse::<u8>()
                .ok()
                .filter(|n| *n <= 127)
                .ok_or_else(|| MidiParseError::BadNumber(text.trim().to_string()))
        };
        let kind = if let Some(n) = kind_text.strip_prefix("note") {
            MidiKind::Note(number(n)?)
        } else if let Some(n) = kind_text.strip_prefix("cc") {
            MidiKind::Cc(number(n)?)
        } else {
            return Err(MidiParseError::BadKind(text.trim().to_string()));
        };
        Ok(Self { channel, kind })
    }
}

impl fmt::Display for MidiTrigger {
    /// Canonical: `Midi:Ch1:Note60`, `Midi:CC7`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Midi:")?;
        if let Some(ch) = self.channel {
            write!(f, "Ch{ch}:")?;
        }
        match self.kind {
            MidiKind::Note(n) => write!(f, "Note{n}"),
            MidiKind::Cc(n) => write!(f, "CC{n}"),
        }
    }
}

/// A decoded MIDI message that can be a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiInputEvent {
    /// Channel `1..=16`.
    pub channel: u8,
    /// Note or controller.
    pub kind: MidiKind,
    /// Velocity / controller value.
    pub value: u8,
    /// Note-on (velocity > 0) is a press, note-off a release; for a
    /// controller the value is compared to 64 by the map.
    pub is_note_on: bool,
}

/// Decode raw MIDI bytes. Only note on / note off / control change are of
/// interest; everything else (clock, pitch bend, sysex) is `None`.
pub fn decode(message: &[u8]) -> Option<MidiInputEvent> {
    let [status, data1, data2] = *message else {
        return None;
    };
    if data1 > 127 || data2 > 127 {
        return None;
    }
    let channel = (status & 0x0F) + 1;
    match status & 0xF0 {
        0x90 => Some(MidiInputEvent {
            channel,
            kind: MidiKind::Note(data1),
            value: data2,
            is_note_on: data2 > 0,
        }),
        0x80 => Some(MidiInputEvent {
            channel,
            kind: MidiKind::Note(data1),
            value: data2,
            is_note_on: false,
        }),
        0xB0 => Some(MidiInputEvent {
            channel,
            kind: MidiKind::Cc(data1),
            value: data2,
            is_note_on: false,
        }),
        _ => None,
    }
}

/// One MIDI binding with its action.
#[derive(Debug, Clone, PartialEq)]
pub struct MidiEntry {
    /// Index of the `[[hotkeys]]` entry.
    pub index: usize,
    /// What it listens for.
    pub trigger: MidiTrigger,
    /// What it does.
    pub action: HotkeyAction,
}

/// The MIDI bindings of a config. Tracks which controllers are "down" so a
/// sweeping knob press / releases once per crossing of 64.
#[derive(Debug, Clone, Default)]
pub struct MidiMap {
    entries: Vec<MidiEntry>,
    /// Invalid `Midi:` entries: (index, error).
    problems: Vec<(usize, MidiParseError)>,
    cc_down: HashSet<(u8, u8)>,
}

impl MidiMap {
    /// Collect the `Midi:` entries of `hotkeys`; keyboard entries are skipped
    /// (see [`HotkeyMap`](super::HotkeyMap)).
    pub fn build(hotkeys: &[HotkeyConfig]) -> Self {
        let mut map = Self::default();
        for (index, hotkey) in hotkeys.iter().enumerate() {
            if !super::is_midi_key(&hotkey.key) {
                continue;
            }
            match MidiTrigger::parse(&hotkey.key) {
                Ok(trigger) => map.entries.push(MidiEntry {
                    index,
                    trigger,
                    action: hotkey.action.clone(),
                }),
                Err(e) => map.problems.push((index, e)),
            }
        }
        map
    }

    /// The usable MIDI bindings.
    pub fn entries(&self) -> &[MidiEntry] {
        &self.entries
    }

    /// Invalid `Midi:` entries.
    pub fn problems(&self) -> &[(usize, MidiParseError)] {
        &self.problems
    }

    /// The events a raw message triggers (a message may match several
    /// bindings, for example one per channel).
    pub fn on_message(&mut self, message: &[u8]) -> Vec<HotkeyEvent> {
        let Some(event) = decode(message) else {
            return Vec::new();
        };
        let phase = match event.kind {
            MidiKind::Note(_) => {
                if event.is_note_on {
                    KeyPhase::Pressed
                } else {
                    KeyPhase::Released
                }
            }
            MidiKind::Cc(controller) => {
                let down = event.value >= 64;
                let key = (event.channel, controller);
                if down && self.cc_down.insert(key) {
                    KeyPhase::Pressed
                } else if !down && self.cc_down.remove(&key) {
                    KeyPhase::Released
                } else {
                    return Vec::new();
                }
            }
        };
        self.entries
            .iter()
            .filter(|entry| {
                entry.trigger.kind == event.kind
                    && entry.trigger.channel.is_none_or(|ch| ch == event.channel)
            })
            .map(|entry| HotkeyEvent {
                index: entry.index,
                action: entry.action.clone(),
                phase,
                source: Source::Midi,
            })
            .collect()
    }
}

/// What can go wrong opening MIDI.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MidiError {
    /// The MIDI system could not be initialised.
    #[error("MIDI is not available: {0}")]
    Init(String),
    /// No input port matches.
    #[error("no MIDI input port named {0:?}")]
    NoSuchPort(String),
    /// There is no input port at all.
    #[error("no MIDI input devices found")]
    NoPorts,
    /// Connecting to the port failed.
    #[error("cannot open the MIDI port: {0}")]
    Connect(String),
}

/// Names of the available MIDI input ports.
pub fn list_ports() -> Result<Vec<String>, MidiError> {
    let input = MidiInput::new("Bardwave ports").map_err(|e| MidiError::Init(e.to_string()))?;
    Ok(input
        .ports()
        .iter()
        .filter_map(|port| input.port_name(port).ok())
        .collect())
}

/// An open MIDI input feeding [`HotkeyEvent`]s.
pub struct MidiService {
    // Dropping the connection closes the port.
    _connection: MidiInputConnection<()>,
    map: Arc<Mutex<MidiMap>>,
    events: Receiver<HotkeyEvent>,
}

impl MidiService {
    /// Open the input port called `port_name` (a substring match; `None` = the
    /// first port) and listen with the bindings of `hotkeys`.
    pub fn connect(
        port_name: Option<&str>,
        hotkeys: &[HotkeyConfig],
        wake: Option<Wake>,
    ) -> Result<Self, MidiError> {
        let mut input =
            MidiInput::new("Bardwave hotkeys").map_err(|e| MidiError::Init(e.to_string()))?;
        // Timing clock and active sensing arrive constantly and mean nothing here.
        input.ignore(Ignore::TimeAndActiveSense);
        let ports = input.ports();
        let port = match port_name {
            None => ports.first().ok_or(MidiError::NoPorts)?,
            Some(wanted) => ports
                .iter()
                .find(|p| {
                    input
                        .port_name(p)
                        .is_ok_and(|name| name.to_lowercase().contains(&wanted.to_lowercase()))
                })
                .ok_or_else(|| MidiError::NoSuchPort(wanted.to_string()))?,
        }
        .clone();

        let map = Arc::new(Mutex::new(MidiMap::build(hotkeys)));
        let (tx, events) = unbounded();
        let callback_map = map.clone();
        let connection = input
            .connect(
                &port,
                "bardwave-hotkeys",
                move |_stamp, bytes, _| {
                    let fired = callback_map.lock().on_message(bytes);
                    if fired.is_empty() {
                        return;
                    }
                    for event in fired {
                        let _ = tx.send(event);
                    }
                    if let Some(wake) = &wake {
                        wake();
                    }
                },
                (),
            )
            .map_err(|e| MidiError::Connect(e.to_string()))?;
        Ok(Self {
            _connection: connection,
            map,
            events,
        })
    }

    /// Replace the bindings (after the user edited them).
    pub fn sync(&self, hotkeys: &[HotkeyConfig]) {
        *self.map.lock() = MidiMap::build(hotkeys);
    }

    /// Events since the last call, oldest first. Never blocks.
    pub fn poll(&self) -> Vec<HotkeyEvent> {
        self.events.try_iter().collect()
    }
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
    fn trigger_parse_and_print() {
        let t = MidiTrigger::parse("Midi:Ch1:Note60").unwrap();
        assert_eq!(t.channel, Some(1));
        assert_eq!(t.kind, MidiKind::Note(60));
        assert_eq!(t.to_string(), "Midi:Ch1:Note60");
        let t = MidiTrigger::parse(" midi: cc7 ").unwrap();
        assert_eq!(t.channel, None);
        assert_eq!(t.to_string(), "Midi:CC7");
        let t = MidiTrigger::parse("MIDI:CH16:CC127").unwrap();
        assert_eq!(MidiTrigger::parse(&t.to_string()).unwrap(), t);
    }

    #[test]
    fn trigger_errors() {
        assert!(matches!(
            MidiTrigger::parse("Ctrl+1"),
            Err(MidiParseError::NotMidi(_))
        ));
        assert!(matches!(
            MidiTrigger::parse("Midi:Ch0:Note1"),
            Err(MidiParseError::BadChannel(_))
        ));
        assert!(matches!(
            MidiTrigger::parse("Midi:Ch17:Note1"),
            Err(MidiParseError::BadChannel(_))
        ));
        assert!(matches!(
            MidiTrigger::parse("Midi:Note128"),
            Err(MidiParseError::BadNumber(_))
        ));
        assert!(matches!(
            MidiTrigger::parse("Midi:Note"),
            Err(MidiParseError::BadNumber(_))
        ));
        assert!(matches!(
            MidiTrigger::parse("Midi:Pitch1"),
            Err(MidiParseError::BadKind(_))
        ));
    }

    #[test]
    fn decoding() {
        let on = decode(&[0x90, 60, 100]).unwrap();
        assert_eq!(
            (on.channel, on.kind, on.is_note_on),
            (1, MidiKind::Note(60), true)
        );
        let zero = decode(&[0x93, 60, 0]).unwrap();
        assert_eq!(zero.channel, 4);
        assert!(!zero.is_note_on, "note-on with velocity 0 is a note-off");
        assert!(!decode(&[0x80, 60, 64]).unwrap().is_note_on);
        let cc = decode(&[0xB1, 7, 90]).unwrap();
        assert_eq!((cc.channel, cc.kind, cc.value), (2, MidiKind::Cc(7), 90));
        assert!(decode(&[0xE0, 0, 64]).is_none(), "pitch bend");
        assert!(decode(&[0xF8]).is_none(), "clock");
        assert!(decode(&[0x90, 200, 1]).is_none());
        assert!(decode(&[]).is_none());
    }

    #[test]
    fn notes_map_to_actions_with_phases() {
        let mut map = MidiMap::build(&[
            cfg("Ctrl+1", HotkeyAction::StopAll),
            cfg("Midi:Ch1:Note60", HotkeyAction::DuckHold),
            cfg("Midi:Note61", HotkeyAction::MasterMute),
        ]);
        assert_eq!(map.entries().len(), 2);
        let down = map.on_message(&[0x90, 60, 100]);
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].index, 1);
        assert_eq!(down[0].action, HotkeyAction::DuckHold);
        assert_eq!(down[0].phase, KeyPhase::Pressed);
        assert_eq!(down[0].source, Source::Midi);
        let up = map.on_message(&[0x80, 60, 0]);
        assert_eq!(up[0].phase, KeyPhase::Released);
        // Wrong channel.
        assert!(map.on_message(&[0x91, 60, 100]).is_empty());
        // Any-channel binding.
        assert_eq!(map.on_message(&[0x95, 61, 100])[0].index, 2);
    }

    #[test]
    fn controller_is_a_button_with_edge_detection() {
        let mut map = MidiMap::build(&[cfg("Midi:CC7", HotkeyAction::MasterMute)]);
        assert!(map.on_message(&[0xB0, 7, 10]).is_empty(), "never pressed");
        assert_eq!(map.on_message(&[0xB0, 7, 100])[0].phase, KeyPhase::Pressed);
        assert!(map.on_message(&[0xB0, 7, 120]).is_empty(), "still down");
        assert_eq!(map.on_message(&[0xB0, 7, 0])[0].phase, KeyPhase::Released);
        assert!(map.on_message(&[0xB0, 7, 0]).is_empty());
        // Other controller: nothing.
        assert!(map.on_message(&[0xB0, 8, 127]).is_empty());
    }

    #[test]
    fn invalid_midi_entries_are_reported() {
        let map = MidiMap::build(&[cfg("Midi:Ch99:Note1", HotkeyAction::StopAll)]);
        assert!(map.entries().is_empty());
        assert_eq!(map.problems().len(), 1);
        assert_eq!(map.problems()[0].0, 0);
    }
}
