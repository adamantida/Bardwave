//! Configuration management for SoundShared

use crate::error::{ConfigError, Result};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Network server configuration
    #[serde(default)]
    pub server: ServerConfig,

    /// Audio engine configuration
    #[serde(default)]
    pub audio: AudioConfig,

    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Persistent sound library and group configuration
    #[serde(default)]
    pub library: LibraryConfig,

    /// Desktop GUI options
    #[serde(default)]
    pub gui: GuiConfig,

    /// Keyboard bindings (`[[hotkeys]]`), in the order the operator made them.
    ///
    /// Top-level (not inside `[library]`) because a binding is a console
    /// setting, not part of a shareable sound set. Targets are ids of sounds,
    /// scenes or playlists from `[library]`. See [`HotkeyConfig`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hotkeys: Vec<HotkeyConfig>,

    /// Session timers (`[[timers]]`): "in ten minutes switch to the Alarm
    /// scene", "every half hour ring the bell". Only the definitions are saved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timers: Vec<TimerConfig>,
}

/// Network server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Port to bind the HTTP/WebSocket server to
    #[serde(default = "default_port")]
    pub port: u16,

    /// Host to bind to (0.0.0.0 for all interfaces)
    #[serde(default = "default_host")]
    pub host: String,

    /// Try to open this port on the local router via UPnP IGD so it is
    /// reachable from the global IP. Enabled by default.
    #[serde(default = "default_upnp")]
    pub upnp: bool,

    /// Key players need in the link (`/?k=…`) to hear the stream.
    ///
    /// Generated on first start when missing. Set it to `""` to let anyone
    /// who can reach the port listen (not recommended with UPnP on).
    #[serde(default)]
    pub access_token: Option<String>,

    /// Key of the remote game-master panel (`/control` from another device).
    ///
    /// Separate from [`ServerConfig::access_token`]: the listener key never
    /// opens the panel. `None` means "not generated yet"; the application
    /// creates one on first start (see [`ServerConfig::ensure_gm_token`]).
    /// `Some("")` is NOT "open to everyone" (unlike `access_token`): the panel
    /// stays reachable from this computer only.
    #[serde(default)]
    pub gm_token: Option<String>,

    /// Serve HTTPS (and `wss://`) on the same port instead of plain HTTP, so
    /// the game-master token and the listener key do not travel in clear text
    /// over the internet. Off by default: the LAN keeps working over HTTP.
    /// Takes effect on the next Start.
    #[serde(default)]
    pub tls: bool,

    /// Path to your own PEM certificate chain (e.g. from Let's Encrypt or a
    /// reverse-proxy tool). Needs [`ServerConfig::tls_key`] as well. When both
    /// are unset a self-signed certificate is generated and stored in the
    /// data directory (`tls/`).
    #[serde(default)]
    pub tls_cert: Option<String>,

    /// Path to the PEM private key that belongs to [`ServerConfig::tls_cert`].
    #[serde(default)]
    pub tls_key: Option<String>,
}

/// Audio engine configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Audio buffer size in samples (per channel)
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,

    /// Sample rate for audio processing
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,

    /// Number of output channels (2 for stereo)
    #[serde(default = "default_channels")]
    pub channels: u16,

    /// Master volume (0.0 to 1.0)
    #[serde(default = "default_master_volume")]
    pub master_volume: f32,

    /// Measure the integrated loudness (EBU R128) of newly added sounds and
    /// store the correction in [`SoundConfigEntry::gain_db`]. Off by default so
    /// existing libraries keep sounding the same.
    #[serde(default)]
    pub normalize: bool,

    /// Loudness the normalizer aims for, in LUFS (default -18.0).
    #[serde(default = "default_normalize_target_lufs")]
    pub normalize_target_lufs: f32,

    /// Ducking depth: the LEVEL (linear gain multiplier, `0.0..=1.0`) the
    /// ducked buses are lowered to while ducking is active. `0.3` is about
    /// -10 dB; `1.0` disables the effect, `0.0` mutes them.
    #[serde(default = "default_duck_amount")]
    pub duck_amount: f32,

    /// Time to go from full level to `duck_amount`, in milliseconds.
    #[serde(default = "default_duck_attack_ms")]
    pub duck_attack_ms: u32,

    /// Time to return from `duck_amount` to full level, in milliseconds.
    #[serde(default = "default_duck_release_ms")]
    pub duck_release_ms: u32,

    /// Buses that are lowered while ducking is active (default: music and
    /// ambience; sound effects and stingers stay at full level).
    #[serde(default = "default_duck_buses")]
    pub duck_buses: Vec<Bus>,

    /// Files longer than this many seconds are decoded on the fly (streamed)
    /// instead of being held in memory; shorter ones stay in memory for an
    /// instant start. `0` turns streaming off.
    #[serde(default = "default_stream_threshold_secs")]
    pub stream_threshold_secs: u32,

    /// Name of the output device the desktop plays on. `None` (the default,
    /// and what configs written before this existed mean) is the system
    /// default device. A name that is not found falls back to the default and
    /// the console says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_device: Option<String>,

    /// Name of a second device (headphones) for sound previews. `None` plays
    /// previews on the main output. Previews never reach the players' stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_device: Option<String>,
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Enable file logging in addition to stdout
    #[serde(default)]
    pub file_logging: bool,
}

/// Desktop GUI options.
///
/// These are console presentation choices, not features: the design system
/// requires the app to respect reduced motion, and egui 0.30 exposes no such
/// signal, so the operator states it here. Missing section in an older
/// `config.toml` means "animated" (the default), so nothing changes for configs
/// written before this existed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuiConfig {
    /// Disable the live-dot pulse and the backdrop scanline.
    ///
    /// State changes stay instant; only the two decorative motions stop.
    #[serde(default)]
    pub reduce_motion: bool,

    /// Let `[[hotkeys]]` fire while the console window is not focused
    /// (system-wide shortcuts). Off by default: it needs extra permissions on
    /// macOS and can steal keys from other programs.
    #[serde(default)]
    pub global_hotkeys: bool,

    /// Folder session recordings (REC button) are saved to. `None` means
    /// `recordings/` inside the data directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recordings_dir: Option<String>,

    /// Record 32-bit float WAV instead of 16-bit PCM.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub record_float32: bool,
    /// Check GitHub for a newer release once a day at start-up. Off by
    /// default: a stock install makes no network requests on its own (the
    /// "Check for updates" button still works on demand).
    #[serde(default)]
    pub check_updates: bool,

    /// Unix time (seconds) of the last update check; 0 = never.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub last_update_check: u64,

    /// Interface scale factor, `UI_SCALE_MIN..=UI_SCALE_MAX` (1.0 = native
    /// size). Applied live through egui's pixels-per-point; read it with
    /// [`GuiConfig::scale`], which also repairs hand-edited nonsense.
    #[serde(default = "default_ui_scale", skip_serializing_if = "is_default_scale")]
    pub ui_scale: f32,

    /// The first-run wizard ("Getting started") was finished or dismissed for
    /// good, or the config already had a library when the wizard was added.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub onboarding_done: bool,
}

/// Smallest interface scale the console offers.
pub const UI_SCALE_MIN: f32 = 0.8;
/// Largest interface scale the console offers.
pub const UI_SCALE_MAX: f32 = 1.6;

fn default_ui_scale() -> f32 {
    1.0
}

fn is_default_scale(value: &f32) -> bool {
    (*value - 1.0).abs() < f32::EPSILON
}

/// `scale` limited to the supported range; a non-finite value (a hand-edited
/// `ui_scale = nan`) falls back to 1.0.
pub fn clamp_ui_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(UI_SCALE_MIN, UI_SCALE_MAX)
    } else {
        1.0
    }
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            reduce_motion: false,
            global_hotkeys: false,
            recordings_dir: None,
            record_float32: false,
            check_updates: false,
            last_update_check: 0,
            ui_scale: default_ui_scale(),
            onboarding_done: false,
        }
    }
}

impl GuiConfig {
    /// The interface scale to use: [`GuiConfig::ui_scale`] clamped to the
    /// supported range.
    pub fn scale(&self) -> f32 {
        clamp_ui_scale(self.ui_scale)
    }
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// Persistent library + groups configuration
///
/// This mirrors the in-memory sound library model (`crate::gui::groups::Library`)
/// so a user's groups, sound paths, per-sound volumes and group membership
/// survive restarts. Everything is optional in the file so configs written by
/// older versions (which had no `[library]` section) still load.
///
/// Identifiers: every sound, scene and playlist has a UUID string `id`.
/// Scenes, playlists and hotkeys refer to sounds by that id (never by path), so
/// a sound can be re-located on disk without breaking them.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct LibraryConfig {
    /// Sound groups, in insertion order
    #[serde(default)]
    pub groups: Vec<GroupConfig>,

    /// Registered sounds, in insertion order
    #[serde(default)]
    pub sounds: Vec<SoundConfigEntry>,

    /// Scenes (`[[library.scenes]]`): named sets of sounds started together.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<SceneConfig>,

    /// Music playlists (`[[library.playlists]]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub playlists: Vec<PlaylistConfig>,
}

impl LibraryConfig {
    /// Give every sound, scene and playlist a valid, unique `id`.
    ///
    /// This is the v1.4.0 migration: those configs have no sound ids, so each
    /// sound gets a fresh random UUID. The ids are not derived from the path
    /// (they only need to stay stable from now on, and they do because they are
    /// written back on the next save). An id that is empty, not a UUID, or that
    /// repeats an earlier one in the same list is replaced too.
    ///
    /// A replaced id that was a non-empty non-UUID string (hand-written, such
    /// as `"rain"`) is not just forgotten: every reference to it inside the
    /// library (scene items, emitters, a scene's playlist, playlist tracks) is
    /// rewritten to the new id, so nothing is left dangling. Hotkeys live
    /// outside `[library]`; use [`LibraryConfig::normalize_ids`] and
    /// [`IdRemap::apply_to_hotkeys`], or [`Config::ensure_ids`], for those.
    ///
    /// Returns `true` when anything was changed, i.e. when the config should be
    /// saved to make the new ids permanent. [`Config::load_from`] calls this.
    pub fn ensure_ids(&mut self) -> bool {
        self.normalize_ids().changed
    }

    /// [`LibraryConfig::ensure_ids`], but reporting which ids were replaced so
    /// that references held elsewhere (hotkeys) can be rewritten too.
    pub fn normalize_ids(&mut self) -> IdRemap {
        let mut changed = false;
        let remap = IdRemap {
            sounds: fix_ids(self.sounds.iter_mut().map(|s| &mut s.id), &mut changed),
            scenes: fix_ids(self.scenes.iter_mut().map(|s| &mut s.id), &mut changed),
            playlists: fix_ids(self.playlists.iter_mut().map(|p| &mut p.id), &mut changed),
            changed,
        };

        if !remap.sounds.is_empty() {
            for scene in &mut self.scenes {
                for item in &mut scene.items {
                    remap_ref(&remap.sounds, &mut item.sound);
                }
                for emitter in &mut scene.emitters {
                    for id in &mut emitter.sounds {
                        remap_ref(&remap.sounds, id);
                    }
                }
            }
            for playlist in &mut self.playlists {
                for id in &mut playlist.items {
                    remap_ref(&remap.sounds, id);
                }
            }
        }
        if !remap.playlists.is_empty() {
            for scene in &mut self.scenes {
                if let Some(id) = scene.playlist.as_mut() {
                    remap_ref(&remap.playlists, id);
                }
            }
        }
        remap
    }
}

/// Replace unusable ids in place; returns `old (trimmed, non-empty) -> new`
/// for the ones that were non-UUID strings, so references can follow them.
fn fix_ids<'a>(
    ids: impl Iterator<Item = &'a mut String>,
    changed: &mut bool,
) -> std::collections::HashMap<String, String> {
    let mut seen = std::collections::HashSet::new();
    let mut map = std::collections::HashMap::new();
    for id in ids {
        match uuid::Uuid::parse_str(id.trim()) {
            Ok(parsed) if seen.insert(parsed) => {}
            result => {
                let fresh = uuid::Uuid::new_v4();
                seen.insert(fresh);
                let old = id.trim().to_string();
                *id = fresh.to_string();
                *changed = true;
                // Only a hand-written, non-UUID id can be referenced under its
                // old spelling; a repeated UUID keeps pointing at its first
                // owner. The first holder of a repeated bad id wins too.
                if result.is_err() && !old.is_empty() {
                    map.entry(old).or_insert_with(|| fresh.to_string());
                }
            }
        }
    }
    map
}

fn remap_ref(map: &std::collections::HashMap<String, String>, id: &mut String) {
    if let Some(new) = map.get(id.trim()) {
        *id = new.clone();
    }
}

/// Which ids [`LibraryConfig::normalize_ids`] replaced: `old id -> new id`, per
/// kind. Only non-empty, non-UUID ids appear (those are the ones other entries
/// can refer to).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdRemap {
    /// Sound ids.
    pub sounds: std::collections::HashMap<String, String>,
    /// Scene ids.
    pub scenes: std::collections::HashMap<String, String>,
    /// Playlist ids.
    pub playlists: std::collections::HashMap<String, String>,
    /// Whether any id at all was replaced (also empty or repeated ones).
    pub changed: bool,
}

impl IdRemap {
    /// Point hotkeys that named a replaced id at the new one.
    pub fn apply_to_hotkeys(&self, hotkeys: &mut [HotkeyConfig]) {
        for hotkey in hotkeys {
            match &mut hotkey.action {
                HotkeyAction::Sound { target } => remap_ref(&self.sounds, target),
                HotkeyAction::Scene { target } => remap_ref(&self.scenes, target),
                HotkeyAction::Playlist { target } => remap_ref(&self.playlists, target),
                _ => {}
            }
        }
    }
}

/// A single sound group (persisted form)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupConfig {
    /// Group identifier (UUID string)
    pub id: String,

    /// Human-readable group name
    pub name: String,
}

/// Mixer bus a sound or playlist is routed to.
///
/// Buses carry their own level and take part in ducking
/// ([`AudioConfig::duck_buses`]).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum Bus {
    /// Background music and playlists.
    Music,
    /// Looped atmosphere (default for sounds, so old libraries behave as before).
    #[default]
    Ambience,
    /// Short effects and stingers.
    Sfx,
}

/// Shape of a scene fade.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum FadeCurve {
    /// sin/cos pair: constant summed power across a crossfade (default).
    #[default]
    EqualPower,
    /// Straight line in gain; dips in the middle of a crossfade.
    Linear,
}

/// When a scene item starts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum SceneStart {
    /// Automatically when the scene is activated (after `delay_ms`).
    #[default]
    OnEnter,
    /// Only when the operator starts it; the scene just lists it.
    Manual,
}

/// What a playlist does when it reaches the end.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    /// Stop after the last track.
    Off,
    /// Start over from the first track (default: background music keeps going).
    #[default]
    All,
    /// Repeat the current track.
    One,
}

/// A single registered sound (persisted form)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SoundConfigEntry {
    /// Stable identifier (UUID string). Scenes, playlists and hotkeys refer to
    /// the sound by it. Missing/empty in configs written before v1.5.0: a new
    /// one is generated at load time ([`LibraryConfig::ensure_ids`]) and
    /// persisted on the next save.
    #[serde(default)]
    pub id: String,

    /// Absolute or user-supplied path to the audio file
    pub path: String,

    /// Display name; falls back to the file name when empty
    #[serde(default)]
    pub name: String,

    /// Per-sound volume, 0.0..=1.0
    #[serde(default = "default_sound_volume")]
    pub volume: f32,

    /// Whether the sound restarts from the beginning when it ends
    #[serde(default)]
    pub looped: bool,

    /// Group this sound belongs to, if any (UUID string)
    #[serde(default)]
    pub group: Option<String>,

    /// Free-form labels for searching and filtering. Stored lower-case,
    /// trimmed, without duplicates (the library model normalizes them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Fixed correction in decibels applied on top of `volume` (loudness
    /// normalization writes it; `0.0` = untouched).
    #[serde(default)]
    pub gain_db: f32,

    /// Mixer bus the sound plays on.
    #[serde(default)]
    pub bus: Bus,

    /// Treat the sound as a stinger: a one-shot played over everything, which
    /// does not stop other sounds and does not depend on the active scene.
    #[serde(default)]
    pub oneshot_stinger: bool,

    /// Stereo position, `-1.0` (left) ..= `1.0` (right); `0.0` = centre (the
    /// sound is untouched). Written only when it differs from the centre.
    #[serde(default, skip_serializing_if = "is_centre_pan")]
    pub pan: f32,
}

/// `serde` helper: a centred pan is not written, so configs of sounds that
/// were never panned stay byte-identical to the ones older versions wrote.
fn is_centre_pan(pan: &f32) -> bool {
    *pan == 0.0
}

impl Default for SoundConfigEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            path: String::new(),
            name: String::new(),
            volume: default_sound_volume(),
            looped: false,
            group: None,
            tags: Vec::new(),
            gain_db: 0.0,
            bus: Bus::default(),
            oneshot_stinger: false,
            pan: 0.0,
        }
    }
}

/// A scene: a named set of sounds (plus random emitters and an optional
/// music playlist) that starts with one action.
///
/// ```toml
/// [[library.scenes]]
/// id = "…"
/// name = "Tavern"
/// color = "#a04a4a"
/// fade_in_ms = 2000
/// fade_out_ms = 2000
/// curve = "equal_power"
/// stop_others = true
/// playlist = "…"            # optional playlist id
///
/// [[library.scenes.items]]
/// sound = "…"               # sound id
/// volume = 0.4
/// looped = true
/// start = "on_enter"
/// delay_ms = 0
///
/// [[library.scenes.emitters]]
/// sounds = ["…", "…"]
/// min_interval_ms = 20000
/// max_interval_ms = 60000
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SceneConfig {
    /// Scene identifier (UUID string); generated when missing.
    #[serde(default)]
    pub id: String,

    /// Name shown on the scene button.
    #[serde(default)]
    pub name: String,

    /// Button color as `#rrggbb` (or `#rgb`); `None` = theme default.
    #[serde(default)]
    pub color: Option<String>,

    /// Fade-in time of the sounds this scene starts, in milliseconds.
    #[serde(default = "default_scene_fade_ms")]
    pub fade_in_ms: u32,

    /// Fade-out time of the sounds this scene stops, in milliseconds.
    #[serde(default = "default_scene_fade_ms")]
    pub fade_out_ms: u32,

    /// Fade shape used for both directions.
    #[serde(default)]
    pub curve: FadeCurve,

    /// Fade out (and stop) playing sounds that are not part of this scene.
    #[serde(default = "default_true")]
    pub stop_others: bool,

    /// The scene's sounds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<SceneItemConfig>,

    /// Random / periodic sound sources that run while the scene is active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emitters: Vec<EmitterConfig>,

    /// Playlist (id) played as the scene's music layer.
    #[serde(default)]
    pub playlist: Option<String>,
}

impl Default for SceneConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            color: None,
            fade_in_ms: default_scene_fade_ms(),
            fade_out_ms: default_scene_fade_ms(),
            curve: FadeCurve::default(),
            stop_others: true,
            items: Vec::new(),
            emitters: Vec::new(),
            playlist: None,
        }
    }
}

/// One sound inside a scene (`[[library.scenes.items]]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SceneItemConfig {
    /// Id of the sound ([`SoundConfigEntry::id`]).
    pub sound: String,

    /// Level inside this scene, `0.0..=1.0` (replaces the sound's own volume).
    #[serde(default = "default_sound_volume")]
    pub volume: f32,

    /// Repeat the sound while the scene is active.
    #[serde(default)]
    pub looped: bool,

    /// Automatic or manual start.
    #[serde(default)]
    pub start: SceneStart,

    /// Delay after activation before the sound starts, in milliseconds.
    #[serde(default)]
    pub delay_ms: u32,

    /// Stereo position inside this scene, `-1.0..=1.0`. When it is non-zero it
    /// replaces the sound's own `pan`; `0.0` (the default) means "use the
    /// sound's pan". Written only when non-zero.
    #[serde(default, skip_serializing_if = "is_centre_pan")]
    pub pan: f32,
}

/// A random/periodic sound source of a scene (`[[library.scenes.emitters]]`):
/// "crows every 20-60 seconds". Each time it fires, one of `sounds` is chosen
/// at random.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmitterConfig {
    /// Ids of the candidate sounds.
    #[serde(default)]
    pub sounds: Vec<String>,

    /// Shortest pause between two plays, in milliseconds.
    #[serde(default = "default_emitter_min_ms")]
    pub min_interval_ms: u32,

    /// Longest pause between two plays, in milliseconds (`>= min_interval_ms`).
    #[serde(default = "default_emitter_max_ms")]
    pub max_interval_ms: u32,

    /// Base level, `0.0..=1.0`.
    #[serde(default = "default_sound_volume")]
    pub volume: f32,

    /// Random level spread, `0.0..=1.0` (fraction of `volume`).
    #[serde(default)]
    pub volume_jitter: f32,

    /// Random stereo position spread, `0.0..=1.0` (0 = centre, 1 = anywhere).
    #[serde(default)]
    pub pan_jitter: f32,

    /// Never play more than this many instances at once (at least 1).
    #[serde(default = "default_emitter_max_overlap")]
    pub max_overlap: u32,
}

impl Default for EmitterConfig {
    fn default() -> Self {
        Self {
            sounds: Vec::new(),
            min_interval_ms: default_emitter_min_ms(),
            max_interval_ms: default_emitter_max_ms(),
            volume: default_sound_volume(),
            volume_jitter: 0.0,
            pan_jitter: 0.0,
            max_overlap: default_emitter_max_overlap(),
        }
    }
}

/// A music playlist (`[[library.playlists]]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaylistConfig {
    /// Playlist identifier (UUID string); generated when missing.
    #[serde(default)]
    pub id: String,

    /// Display name.
    #[serde(default)]
    pub name: String,

    /// Ids of the tracks, in play order.
    #[serde(default)]
    pub items: Vec<String>,

    /// Play in random order without repeating a track until all were played.
    #[serde(default)]
    pub shuffle: bool,

    /// End-of-list behaviour.
    #[serde(default)]
    pub repeat: RepeatMode,

    /// Crossfade between tracks, in milliseconds (0 = gapless cut).
    #[serde(default = "default_playlist_crossfade_ms")]
    pub crossfade_ms: u32,

    /// Bus the playlist plays on (default `music`).
    #[serde(default = "default_playlist_bus")]
    pub bus: Bus,
}

impl Default for PlaylistConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            items: Vec::new(),
            shuffle: false,
            repeat: RepeatMode::default(),
            crossfade_ms: default_playlist_crossfade_ms(),
            bus: default_playlist_bus(),
        }
    }
}

/// A keyboard binding (`[[hotkeys]]`).
///
/// ```toml
/// [[hotkeys]]
/// key = "Ctrl+1"
/// [hotkeys.action]
/// kind = "scene"
/// target = "…"            # scene id
///
/// [[hotkeys]]
/// key = "F5"
/// [hotkeys.action]
/// kind = "stop_all"
/// ```
///
/// `key` is modifiers and one key joined by `+`: `Ctrl`, `Alt`, `Shift`,
/// `Cmd` (macOS Command / Windows key), then a letter, digit, `F1`..`F24`, or a
/// named key (`Space`, `Enter`, `Escape`, `Up`…). Matching is case-insensitive
/// and modifier order does not matter; see
/// [`HotkeyConfig::canonical_key`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotkeyConfig {
    /// Key combination, e.g. `"Ctrl+1"`, `"Cmd+K"`, `"F5"`.
    pub key: String,

    /// What the combination does.
    pub action: HotkeyAction,
}

/// What a hotkey does. Serialized as `{ kind = "<snake_case name>", target = … }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HotkeyAction {
    /// Play the sound (stingers fire one-shot; others toggle). `target` = sound id.
    Sound {
        /// Sound id.
        target: String,
    },
    /// Activate the scene. `target` = scene id.
    Scene {
        /// Scene id.
        target: String,
    },
    /// Start (or restart) the playlist. `target` = playlist id.
    Playlist {
        /// Playlist id.
        target: String,
    },
    /// Stop every sound (with the scene fade-out when one is active).
    StopAll,
    /// Stop the active scene.
    StopScene,
    /// Raise the master volume by one step.
    MasterUp,
    /// Lower the master volume by one step.
    MasterDown,
    /// Mute / unmute the master output.
    MasterMute,
    /// Press once to duck the background buses, again to release.
    DuckToggle,
    /// Duck while the key is held down, release on key-up.
    DuckHold,
    /// Skip to the next playlist track.
    NextTrack,
    /// Go back to the previous playlist track.
    PrevTrack,
    /// Pause / resume the music playlist.
    PauseMusic,
}

impl HotkeyAction {
    /// Id the action points at, for the three actions that have a target.
    pub fn target(&self) -> Option<&str> {
        match self {
            Self::Sound { target } | Self::Scene { target } | Self::Playlist { target } => {
                Some(target)
            }
            _ => None,
        }
    }
}

impl HotkeyConfig {
    /// Canonical spelling of [`HotkeyConfig::key`] for comparing bindings
    /// (`"ctrl+shift+a"` and `"Shift+Ctrl+A"` both become `"Ctrl+Shift+A"`).
    /// See [`canonical_hotkey`].
    pub fn canonical_key(&self) -> Option<String> {
        canonical_hotkey(&self.key)
    }
}

/// Normalize a key-combination string, or `None` when it is not one.
///
/// Rules: parts are split on `+` and trimmed; modifiers (case-insensitive,
/// aliases `control`->`Ctrl`, `option`/`opt`->`Alt`, `command`/`super`/`meta`/
/// `win`->`Cmd`) are de-duplicated and written in the order `Ctrl+Alt+Shift+Cmd`;
/// exactly one non-modifier key must remain. A single character is upper-cased
/// (`a` -> `A`), `f5` -> `F5`, other names get a capital first letter
/// (`space` -> `Space`) with a few aliases (`esc` -> `Escape`, `return` ->
/// `Enter`, `del` -> `Delete`). It does not check that the key exists on any
/// keyboard: that is the input layer's job.
pub fn canonical_hotkey(text: &str) -> Option<String> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut cmd = false;
    let mut key: Option<String> = None;

    for part in text.split('+') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" | "opt" => alt = true,
            "shift" => shift = true,
            "cmd" | "command" | "super" | "meta" | "win" | "windows" => cmd = true,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(canonical_key_name(other));
            }
        }
    }

    let key = key?;
    let mut out = String::new();
    for (on, name) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (cmd, "Cmd")] {
        if on {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&key);
    Some(out)
}

/// Capitalize a lower-cased key token (see [`canonical_hotkey`]).
fn canonical_key_name(lower: &str) -> String {
    let mut chars = lower.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let rest: String = chars.collect();
    if rest.is_empty() {
        return first.to_uppercase().collect();
    }
    match lower {
        "esc" => return "Escape".to_string(),
        "return" => return "Enter".to_string(),
        "del" => return "Delete".to_string(),
        "ins" => return "Insert".to_string(),
        "pgup" => return "PageUp".to_string(),
        "pgdn" => return "PageDown".to_string(),
        _ => {}
    }
    if first == 'f' && rest.bytes().all(|b| b.is_ascii_digit()) {
        return format!("F{rest}");
    }
    let mut out: String = first.to_uppercase().collect();
    out.push_str(&rest);
    out
}

fn default_port() -> u16 {
    8080
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_upnp() -> bool {
    true
}

fn default_buffer_size() -> usize {
    // 20 ms at 48 kHz: the wire now ships 20 ms Opus frames; the engine
    // publishes blocks of this size.
    960
}

fn default_sample_rate() -> u32 {
    48000
}

fn default_channels() -> u16 {
    2
}

fn default_master_volume() -> f32 {
    0.8
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Default per-sound volume used when a `SoundConfigEntry` omits `volume`
pub fn default_sound_volume() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_scene_fade_ms() -> u32 {
    2000
}

fn default_emitter_min_ms() -> u32 {
    20_000
}

fn default_emitter_max_ms() -> u32 {
    60_000
}

fn default_emitter_max_overlap() -> u32 {
    2
}

fn default_playlist_crossfade_ms() -> u32 {
    2000
}

fn default_playlist_bus() -> Bus {
    Bus::Music
}

fn default_normalize_target_lufs() -> f32 {
    -18.0
}

fn default_duck_amount() -> f32 {
    0.3
}

fn default_duck_attack_ms() -> u32 {
    200
}

fn default_duck_release_ms() -> u32 {
    800
}

fn default_stream_threshold_secs() -> u32 {
    crate::audio::stream_source::DEFAULT_STREAM_THRESHOLD_SECS
}

fn default_duck_buses() -> Vec<Bus> {
    vec![Bus::Music, Bus::Ambience]
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            upnp: default_upnp(),
            access_token: None,
            gm_token: None,
            tls: false,
            tls_cert: None,
            tls_key: None,
        }
    }
}

impl ServerConfig {
    /// Generate [`ServerConfig::gm_token`] when it is missing (`None`).
    ///
    /// An explicit `Some("")` is kept: it means "panel is local-only". Returns
    /// `true` when a token was created and the config should be saved.
    pub fn ensure_gm_token(&mut self) -> bool {
        if self.gm_token.is_none() {
            self.gm_token = Some(crate::network::generate_key());
            return true;
        }
        false
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            buffer_size: default_buffer_size(),
            sample_rate: default_sample_rate(),
            channels: default_channels(),
            master_volume: default_master_volume(),
            normalize: false,
            normalize_target_lufs: default_normalize_target_lufs(),
            duck_amount: default_duck_amount(),
            duck_attack_ms: default_duck_attack_ms(),
            duck_release_ms: default_duck_release_ms(),
            duck_buses: default_duck_buses(),
            stream_threshold_secs: default_stream_threshold_secs(),
            output_device: None,
            preview_device: None,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file_logging: false,
        }
    }
}

/// Parse a port typed into the CONFIGURATION field.
///
/// Only `1..=65535` is accepted: empty text, `0`, out-of-range values and
/// anything that is not plain ASCII digits are rejected, so the caller can keep
/// the last valid port in force instead of applying garbage.
pub fn parse_port(text: &str) -> Option<u16> {
    let trimmed = text.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match trimmed.parse::<u16>() {
        Ok(0) | Err(_) => None,
        Ok(port) => Some(port),
    }
}

/// Parse a bind address typed into the CONFIGURATION field.
///
/// Only IP literals are accepted (`0.0.0.0`, `127.0.0.1`, `192.168.1.245`,
/// `::`, `::1`). Host names are rejected because the value is handed straight to
/// `TcpListener::bind`, which takes a numeric address.
pub fn parse_bind_host(text: &str) -> Option<IpAddr> {
    text.trim().parse::<IpAddr>().ok()
}

impl Config {
    /// Load configuration from config.toml next to the executable
    /// Falls back to defaults if file doesn't exist or is invalid
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::config_path()?)
    }

    /// Load configuration from an explicit path.
    ///
    /// Split out of [`Config::load`] so tests (and any caller that knows its own
    /// config location) can round-trip a temporary file instead of the real
    /// `config.toml` that sits next to the executable.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::info!("Config file not found at {:?}, using defaults", path);
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::NotFound(format!("{:?}: {}", path, e)))?;

        let mut config: Config = toml::from_str(&content)
            .map_err(|e| ConfigError::Parse(format!("{:?}: {}", path, e)))?;

        // v1.4.0 files have no sound ids: mint them now, they are written out
        // by the next save.
        if config.ensure_ids() {
            tracing::info!("Assigned ids to library entries of {:?}", path);
        }

        tracing::info!("Loaded configuration from {:?}", path);
        Ok(config)
    }

    /// Give library entries valid ids and keep every reference to a replaced
    /// id (library and `[[hotkeys]]`) pointing at its new one. See
    /// [`LibraryConfig::ensure_ids`]. True when anything changed.
    pub fn ensure_ids(&mut self) -> bool {
        let remap = self.library.normalize_ids();
        remap.apply_to_hotkeys(&mut self.hotkeys);
        remap.changed
    }

    /// Save configuration to config.toml next to the executable
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::config_path()?)
    }

    /// Save configuration to an explicit path (see [`Config::load_from`]).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::Write(format!("Serialization failed: {}", e)))?;

        std::fs::write(path, content)
            .map_err(|e| ConfigError::Write(format!("{:?}: {}", path, e)))?;

        tracing::info!("Saved configuration to {:?}", path);
        Ok(())
    }

    /// Get the path to config.toml (inside [`crate::paths::data_dir`]: next to
    /// the executable as in v1.4.0, or the per-user data folder).
    fn config_path() -> Result<PathBuf> {
        Ok(crate::paths::config_path())
    }

    /// Get the socket address for the server
    pub fn server_addr(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }
}

/// Shortest interval a timer may have, in milliseconds. The timeline thread
/// steps every 100 ms, so anything below that could not be honoured anyway.
pub const MIN_TIMER_MS: u64 = 100;

/// When a timer fires (the `trigger` of a `[[timers]]` entry).
///
/// ```toml
/// trigger = { after_ms = 600000 }   # once, ten minutes after Start
/// trigger = { every_ms = 1800000 }  # every thirty minutes
/// ```
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimerTrigger {
    /// Once, this many milliseconds after the timers were started.
    AfterMs(u64),
    /// Repeatedly, with this many milliseconds between firings.
    EveryMs(u64),
}

impl TimerTrigger {
    /// The delay or interval in milliseconds.
    pub fn ms(&self) -> u64 {
        match *self {
            Self::AfterMs(ms) | Self::EveryMs(ms) => ms,
        }
    }

    /// Whether the timer fires more than once.
    pub fn repeats(&self) -> bool {
        matches!(self, Self::EveryMs(_))
    }
}

impl Default for TimerTrigger {
    fn default() -> Self {
        Self::AfterMs(10 * 60 * 1000)
    }
}

/// What a timer does when it fires. Every variant maps onto an existing
/// Director command; ids are the library ids (UUID strings).
///
/// Serialized as `{ kind = "<snake_case name>", … }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimerAction {
    /// Switch to a scene.
    ActivateScene {
        /// Scene id.
        scene: String,
    },
    /// Fade out the active scene.
    StopScene,
    /// Play a sound once over everything.
    PlayStinger {
        /// Sound id.
        sound: String,
    },
    /// Fade out everything.
    StopAll {
        /// Fade length in milliseconds.
        #[serde(default)]
        fade_ms: u32,
    },
    /// Start (or restart) a playlist.
    StartPlaylist {
        /// Playlist id.
        playlist: String,
    },
    /// Fade the music out and stop it.
    StopPlaylist,
    /// Skip to the next playlist track.
    NextTrack,
    /// Pause the music playlist.
    PauseMusic,
    /// Resume the music playlist.
    ResumeMusic,
}

impl TimerAction {
    /// Id the action points at, for the actions that have a target.
    pub fn target(&self) -> Option<&str> {
        match self {
            Self::ActivateScene { scene } => Some(scene),
            Self::PlayStinger { sound } => Some(sound),
            Self::StartPlaylist { playlist } => Some(playlist),
            _ => None,
        }
    }
}

impl Default for TimerAction {
    fn default() -> Self {
        Self::StopAll { fade_ms: 2000 }
    }
}

/// A session timer (`[[timers]]`).
///
/// The definition is saved; whether it is running is not (timers always start
/// stopped, see `audio::timeline`).
///
/// ```toml
/// [[timers]]
/// id = "…"
/// name = "Alarm in ten minutes"
/// trigger = { after_ms = 600000 }
/// action = { kind = "activate_scene", scene = "…" }
/// enabled = true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimerConfig {
    /// Timer identifier (UUID string); generated by the GUI.
    #[serde(default)]
    pub id: String,

    /// Display name.
    #[serde(default)]
    pub name: String,

    /// When it fires.
    #[serde(default)]
    pub trigger: TimerTrigger,

    /// What it does.
    #[serde(default)]
    pub action: TimerAction,

    /// Disabled timers are kept but never armed.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            trigger: TimerTrigger::default(),
            action: TimerAction::default(),
            enabled: true,
        }
    }
}

impl TimerConfig {
    /// Why the timer cannot run, when it cannot.
    pub fn problem(&self) -> Option<String> {
        if self.trigger.ms() < MIN_TIMER_MS {
            return Some(format!("interval below {MIN_TIMER_MS} ms"));
        }
        if self.action.target().is_some_and(|t| t.trim().is_empty()) {
            return Some("no target chosen".to_string());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch path for round-trip tests. It lives under `target/`, which the
    /// running test binary can always write, and it never matches the real
    /// `config.toml` next to the executable.
    fn temp_config_path(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-config");
        std::fs::create_dir_all(&dir).expect("create test config dir");
        dir.join(format!("{}-{}.toml", name, std::process::id()))
    }

    /// The commented config.toml shipped in the release archives must load,
    /// and must not carry an access key (each install generates its own).
    #[test]
    fn shipped_example_config_loads() {
        let config: Config = toml::from_str(include_str!("../packaging/config.toml"))
            .expect("packaging/config.toml");
        assert_eq!(config.server.port, 8080);
        assert!(config.server.access_token.is_none());
        assert_eq!(config.audio.buffer_size, 960);
        assert!(config.library.sounds.is_empty());
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.audio.buffer_size, 960);
        assert_eq!(config.audio.sample_rate, 48000);
        assert_eq!(config.audio.channels, 2);
        assert_eq!(config.audio.master_volume, 0.8);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn test_server_addr() {
        let config = Config::default();
        assert_eq!(config.server_addr(), "0.0.0.0:8080");
    }

    #[test]
    fn test_default_library_is_empty() {
        let config = Config::default();
        assert!(config.library.groups.is_empty());
        assert!(config.library.sounds.is_empty());
        assert_eq!(default_sound_volume(), 1.0);
    }

    /// Backwards compatibility: a config.toml written before the `[library]`
    /// section existed must still parse, defaulting the library to empty.
    #[test]
    fn test_legacy_config_without_library_section_parses() {
        let legacy = r#"
[server]
port = 9000
host = "127.0.0.1"

[audio]
buffer_size = 512
sample_rate = 44100
channels = 2
master_volume = 0.5

[logging]
level = "debug"
file_logging = true
"#;

        let config: Config = toml::from_str(legacy).expect("legacy config must parse");
        assert_eq!(config.server.port, 9000);
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.audio.buffer_size, 512);
        assert_eq!(config.logging.level, "debug");
        assert!(config.logging.file_logging);
        assert!(config.library.groups.is_empty());
        assert!(config.library.sounds.is_empty());
    }

    /// A document that carries only empty sections plus `[library]` must parse
    /// with every individual setting at its default.
    #[test]
    fn test_config_with_only_library_parses_with_defaults() {
        let text = r#"
[server]
[audio]
[logging]

[[library.groups]]
id = "6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22"
name = "Combat"
"#;

        let config: Config = toml::from_str(text).expect("library-only config must parse");
        assert_eq!(config.server.port, default_port());
        assert_eq!(config.audio.master_volume, default_master_volume());
        assert_eq!(config.logging.level, default_log_level());
        assert_eq!(config.library.groups.len(), 1);
        assert_eq!(config.library.groups[0].name, "Combat");
        assert!(config.library.sounds.is_empty());
    }

    #[test]
    fn test_library_config_roundtrip() {
        let config = Config {
            library: LibraryConfig {
                groups: vec![
                    GroupConfig {
                        id: "6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22".to_string(),
                        name: "Combat".to_string(),
                    },
                    GroupConfig {
                        id: "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f".to_string(),
                        name: "Ambience".to_string(),
                    },
                ],
                sounds: vec![
                    SoundConfigEntry {
                        path: r"C:\fake\hit.wav".to_string(),
                        name: "Hit".to_string(),
                        volume: 0.75,
                        looped: true,
                        group: Some("6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22".to_string()),
                        ..SoundConfigEntry::default()
                    },
                    SoundConfigEntry {
                        path: r"C:\fake\wind.ogg".to_string(),
                        name: String::new(),
                        volume: 1.0,
                        looped: false,
                        group: None,
                        ..SoundConfigEntry::default()
                    },
                ],
                ..LibraryConfig::default()
            },
            ..Config::default()
        };

        let text = toml::to_string_pretty(&config).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");

        assert_eq!(back.library.groups, config.library.groups);
        assert_eq!(back.library.sounds, config.library.sounds);
        assert_eq!(back.server.port, config.server.port);
    }

    /// `volume` in a stored entry must default to 1.0 when omitted, and
    /// `group`/`name` must default to None/empty.
    #[test]
    fn test_sound_entry_defaults_when_fields_omitted() {
        let text = r#"
[server]
port = 8080
host = "0.0.0.0"

[audio]
[logging]

[[library.sounds]]
path = 'C:\fake\hit.wav'

[[library.groups]]
id = "6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22"
name = "Combat"
"#;

        let config: Config = toml::from_str(text).expect("parse");
        assert_eq!(config.library.sounds.len(), 1);
        let entry = &config.library.sounds[0];
        assert_eq!(entry.volume, 1.0);
        assert_eq!(entry.name, "");
        assert_eq!(entry.group, None);
        // Configs written before the repeat toggle existed must load as "off".
        assert!(!entry.looped);
    }

    /// A scene / playlist written with only the essentials takes the documented
    /// defaults for everything else.
    #[test]
    fn scene_and_playlist_defaults_apply_when_fields_are_omitted() {
        let text = r#"
[[library.scenes]]
name = "Tavern"

[[library.scenes.items]]
sound = "11111111-1111-4111-8111-111111111111"

[[library.scenes.emitters]]
sounds = ["11111111-1111-4111-8111-111111111111"]

[[library.playlists]]
name = "Music"
"#;
        let config: Config = toml::from_str(text).expect("parse");
        let scene = &config.library.scenes[0];
        assert_eq!(scene.id, "");
        assert_eq!((scene.fade_in_ms, scene.fade_out_ms), (2000, 2000));
        assert_eq!(scene.curve, FadeCurve::EqualPower);
        assert!(scene.stop_others);
        assert_eq!(scene.color, None);
        assert_eq!(scene.playlist, None);
        let item = &scene.items[0];
        assert_eq!(item.volume, 1.0);
        assert!(!item.looped);
        assert_eq!(item.start, SceneStart::OnEnter);
        assert_eq!(item.delay_ms, 0);
        let emitter = &scene.emitters[0];
        assert_eq!(emitter.max_overlap, 2);
        assert!(emitter.min_interval_ms <= emitter.max_interval_ms);
        let playlist = &config.library.playlists[0];
        assert_eq!(playlist.repeat, RepeatMode::All);
        assert_eq!(playlist.bus, Bus::Music);
        assert!(!playlist.shuffle);
    }

    // ===== GUI options =====

    /// A `config.toml` written before `[gui]` existed must load with the console
    /// animating, so upgrading never changes how the app looks.
    #[test]
    fn missing_gui_section_defaults_to_animated() {
        let text = r#"
[server]
port = 8080
host = "0.0.0.0"

[audio]
[logging]
"#;

        let config: Config = toml::from_str(text).expect("parse");

        assert!(!config.gui.reduce_motion);
    }

    #[test]
    fn reduce_motion_round_trips_through_toml() {
        let mut config = Config::default();
        config.gui.reduce_motion = true;

        let text = toml::to_string_pretty(&config).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");

        assert!(back.gui.reduce_motion);
        assert!(text.contains("reduce_motion = true"));
    }

    // ===== CONFIGURATION field validation =====

    #[test]
    fn parse_port_accepts_the_valid_range() {
        assert_eq!(parse_port("1"), Some(1));
        assert_eq!(parse_port("8080"), Some(8080));
        assert_eq!(parse_port(" 65535 "), Some(65535));
    }

    #[test]
    fn parse_port_rejects_invalid_text() {
        for bad in [
            "", "   ", "0", "65536", "99999", "80a", "-1", "+80", "80.5", "0x1f",
        ] {
            assert_eq!(parse_port(bad), None, "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn parse_bind_host_accepts_ip_literals() {
        assert_eq!(parse_bind_host("0.0.0.0"), "0.0.0.0".parse::<IpAddr>().ok());
        assert_eq!(
            parse_bind_host(" 127.0.0.1 "),
            "127.0.0.1".parse::<IpAddr>().ok()
        );
        assert_eq!(
            parse_bind_host("192.168.1.245"),
            "192.168.1.245".parse::<IpAddr>().ok()
        );
        assert_eq!(parse_bind_host("::"), "::".parse::<IpAddr>().ok());
        assert_eq!(parse_bind_host("::1"), "::1".parse::<IpAddr>().ok());
        assert_eq!(parse_bind_host("fe80::1"), "fe80::1".parse::<IpAddr>().ok());
    }

    #[test]
    fn parse_bind_host_rejects_non_literals() {
        for bad in [
            "",
            "   ",
            "localhost",
            "192.168.1",
            "192.168.1.256",
            "8080",
            "soundshared.local",
            "0.0.0.0:8080",
        ] {
            assert_eq!(
                parse_bind_host(bad),
                None,
                "expected {bad:?} to be rejected"
            );
        }
    }

    /// An edited host + port must survive the exact save/load pair the GUI uses
    /// to persist the CONFIGURATION fields.
    #[test]
    fn save_to_and_load_from_roundtrip_edited_host_and_port() {
        let path = temp_config_path("server-edit");
        let mut config = Config::default();
        config.server.host = "192.168.1.245".to_string();
        config.server.port = 9000;
        config.audio.master_volume = 0.42;

        config.save_to(&path).expect("save_to");
        let back = Config::load_from(&path).expect("load_from");

        assert_eq!(back.server.host, "192.168.1.245");
        assert_eq!(back.server.port, 9000);
        assert_eq!(back.audio.master_volume, 0.42);
        assert_eq!(back.library, config.library);

        // IPv6 literals must round-trip too; the value is re-serialised as a
        // plain TOML string and handed straight to `TcpListener::bind`.
        config.server.host = "::1".to_string();
        config.server.port = 65535;
        config.save_to(&path).expect("save_to ipv6");
        let back = Config::load_from(&path).expect("load_from ipv6");
        assert_eq!(back.server.host, "::1");
        assert_eq!(back.server.port, 65535);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_missing_path_falls_back_to_defaults() {
        let path = temp_config_path("missing");
        let _ = std::fs::remove_file(&path);

        let config = Config::load_from(&path).expect("defaults");

        assert_eq!(config.server.port, default_port());
        assert_eq!(config.server.host, default_host());
    }
}
