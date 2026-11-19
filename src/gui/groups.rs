//! Sound groups and the persistent library model (RPG-Sounds style categories).
//!
//! This module is intentionally pure: it performs no I/O (the one exception
//! is [`Library::missing_sounds`], which stats the sound files and has a pure
//! twin, [`Library::missing_sounds_with`]), touches no audio device, and
//! imports nothing from `egui`. It is the single source of truth
//! for the ordered list of sounds, their per-sound volume, their group
//! membership, the scenes / playlists built from them and the keyboard
//! bindings; `crate::config::LibraryConfig` (plus `Config::hotkeys`) is its
//! persisted mirror.
//!
//! # Identity
//!
//! Sound paths are compared exactly as supplied. Paths are never
//! canonicalized, because a sound file may be missing or on a
//! currently-unavailable volume and the library must still round-trip. The
//! original path-keyed API is unchanged.
//!
//! Every sound additionally has a stable [`SoundId`] (UUID), and the library
//! keeps an `id <-> path` index ([`Library::sound_by_id`], [`Library::id_of`],
//! [`Library::path_of`]). Scenes, playlists and hotkeys refer to sounds by id
//! only, so re-locating a file (changing its path) never breaks them.
//!
//! # Deleting a sound: references are removed
//!
//! [`Library::remove_sound`] also removes every reference to the sound: its
//! items in scenes, its entries in emitters (an emitter left without sounds is
//! dropped), its entries in playlists and the hotkeys that fire it.
//! [`Library::remove_sound_report`] returns how much was cleaned. Removal
//! (not a "file missing" marker) was chosen because "remove from the library"
//! is an explicit user action, while a file that merely went missing stays in
//! the library and keeps all its references.
//!
//! References can still dangle after loading a hand-edited or damaged config
//! ([`Library::from_config`] keeps them so nothing is lost silently); the
//! editing methods refuse to create new dangling references and
//! [`Library::validate`] reports the existing ones.
//!
//! # Persistence
//!
//! `[[hotkeys]]` live at the top level of `config.toml`, not inside
//! `[library]`, so [`Library::to_config`] / [`Library::from_config`] (which
//! only see a `LibraryConfig`) do not carry them. Use
//! [`Library::from_full_config`] and [`Library::write_config`] to round-trip
//! everything, hotkeys included.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::config::{
    canonical_hotkey, Bus, Config, EmitterConfig, FadeCurve, GroupConfig, HotkeyAction,
    HotkeyConfig, LibraryConfig, PlaylistConfig, RepeatMode, SceneConfig, SceneItemConfig,
    SceneStart, SoundConfigEntry,
};

/// Stable identifier of a sound group.
pub type GroupId = Uuid;
/// Stable identifier of a registered sound (survives path changes).
pub type SoundId = Uuid;
/// Stable identifier of a scene.
pub type SceneId = Uuid;
/// Stable identifier of a playlist.
pub type PlaylistId = Uuid;

/// Maximum accepted length of a group name, in Unicode scalar values.
///
/// The name is trimmed before validation, so surrounding whitespace never
/// counts against the limit.
pub const MAX_GROUP_NAME_LEN: usize = 32;

/// Maximum accepted length of a scene or playlist name (trimmed, in chars).
pub const MAX_NAME_LEN: usize = 64;

/// Maximum accepted length of a tag (trimmed, in chars).
pub const MAX_TAG_LEN: usize = 32;

/// Lowest accepted per-sound correction, in dB.
pub const MIN_GAIN_DB: f32 = -48.0;

/// Highest accepted per-sound correction, in dB.
pub const MAX_GAIN_DB: f32 = 24.0;

/// A named sound group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Stable identifier, generated on creation and persisted.
    pub id: GroupId,
    /// Display name, stored trimmed and unique case-insensitively.
    pub name: String,
}

/// Which sounds the UI should show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupFilter {
    /// Every registered sound.
    #[default]
    All,
    /// Only sounds not assigned to any group.
    Ungrouped,
    /// Only sounds assigned to the given group.
    Group(GroupId),
}

/// A registered sound.
#[derive(Debug, Clone, PartialEq)]
pub struct LibrarySound {
    /// Stable identifier; scenes, playlists and hotkeys refer to it.
    pub id: SoundId,
    /// Path exactly as supplied by the user (never canonicalized).
    pub path: PathBuf,
    /// Display name; the UI falls back to the file name when this is empty.
    pub name: String,
    /// Per-sound volume in `0.0..=1.0`.
    pub volume: f32,
    /// Whether the sound restarts itself when it reaches the end.
    pub looped: bool,
    /// Group membership, or `None` when ungrouped.
    pub group: Option<GroupId>,
    /// Labels, lower-case, trimmed, unique (see [`normalize_tag`]).
    pub tags: Vec<String>,
    /// Fixed correction in dB on top of `volume`, `MIN_GAIN_DB..=MAX_GAIN_DB`.
    pub gain_db: f32,
    /// Mixer bus the sound plays on.
    pub bus: Bus,
    /// One-shot "over everything" sound that ignores the active scene.
    pub oneshot_stinger: bool,
}

/// A sound inside a scene.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneItem {
    /// The sound (by id).
    pub sound: SoundId,
    /// Level inside the scene, `0.0..=1.0`.
    pub volume: f32,
    /// Repeat while the scene is active.
    pub looped: bool,
    /// Automatic (on scene entry) or manual start.
    pub start: SceneStart,
    /// Delay after activation, in milliseconds.
    pub delay_ms: u32,
}

impl SceneItem {
    /// An item with the defaults of the config schema: full volume, no loop,
    /// starts on entry without delay.
    pub fn new(sound: SoundId) -> Self {
        Self {
            sound,
            volume: 1.0,
            looped: false,
            start: SceneStart::OnEnter,
            delay_ms: 0,
        }
    }
}

/// A random / periodic sound source of a scene.
#[derive(Debug, Clone, PartialEq)]
pub struct Emitter {
    /// Candidate sounds; one is picked at random each time.
    pub sounds: Vec<SoundId>,
    /// Shortest pause between plays, ms.
    pub min_interval_ms: u32,
    /// Longest pause between plays, ms (`>= min_interval_ms`).
    pub max_interval_ms: u32,
    /// Base level `0.0..=1.0`.
    pub volume: f32,
    /// Random level spread `0.0..=1.0`.
    pub volume_jitter: f32,
    /// Random pan spread `0.0..=1.0`.
    pub pan_jitter: f32,
    /// Maximum simultaneous instances, at least 1.
    pub max_overlap: u32,
}

impl Emitter {
    /// An emitter over `sounds` with the schema defaults (20-60 s, full level,
    /// no jitter, at most 2 overlapping).
    pub fn new(sounds: Vec<SoundId>) -> Self {
        let d = EmitterConfig::default();
        Self {
            sounds,
            min_interval_ms: d.min_interval_ms,
            max_interval_ms: d.max_interval_ms,
            volume: d.volume,
            volume_jitter: d.volume_jitter,
            pan_jitter: d.pan_jitter,
            max_overlap: d.max_overlap,
        }
    }
}

/// A scene: sounds, emitters and an optional playlist started together.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// Stable identifier.
    pub id: SceneId,
    /// Display name (unique case-insensitively when created through the API).
    pub name: String,
    /// `#rrggbb` / `#rgb` button color.
    pub color: Option<String>,
    /// Fade-in, ms.
    pub fade_in_ms: u32,
    /// Fade-out, ms.
    pub fade_out_ms: u32,
    /// Fade shape.
    pub curve: FadeCurve,
    /// Fade out sounds that are not part of the scene.
    pub stop_others: bool,
    /// The scene's sounds, in display order. A sound appears once.
    pub items: Vec<SceneItem>,
    /// Random sound sources.
    pub emitters: Vec<Emitter>,
    /// Music layer.
    pub playlist: Option<PlaylistId>,
}

/// A music playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    /// Stable identifier.
    pub id: PlaylistId,
    /// Display name (unique case-insensitively when created through the API).
    pub name: String,
    /// Tracks in play order (a sound may appear more than once).
    pub items: Vec<SoundId>,
    /// Random order.
    pub shuffle: bool,
    /// End-of-list behaviour.
    pub repeat: RepeatMode,
    /// Crossfade between tracks, ms.
    pub crossfade_ms: u32,
    /// Bus.
    pub bus: Bus,
}

/// Why a hotkey change was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyError {
    /// The key string is not `[modifiers+]key` (see `canonical_hotkey`).
    #[error("not a valid key combination: {0:?}")]
    InvalidKey(String),
    /// The action points at a sound / scene / playlist that does not exist.
    #[error("hotkey target does not exist: {0:?}")]
    UnknownTarget(String),
    /// No binding at that index.
    #[error("no hotkey at index {0}")]
    NoSuchHotkey(usize),
}

/// What [`Library::remove_sound_report`] cleaned up besides the sound itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RemovedRefs {
    /// Items removed from scenes.
    pub scene_items: usize,
    /// Sound entries removed from emitters.
    pub emitter_refs: usize,
    /// Emitters dropped because no sound was left in them.
    pub emitters_dropped: usize,
    /// Tracks removed from playlists.
    pub playlist_items: usize,
    /// Hotkeys removed.
    pub hotkeys: usize,
}

/// A problem found by [`Library::validate`]. These are warnings: the library
/// stays usable, the UI should show them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryWarning {
    /// The same key combination is bound more than once.
    HotkeyConflict {
        /// Canonical key combination.
        key: String,
        /// Indices into [`Library::hotkeys`] (at least two).
        indices: Vec<usize>,
    },
    /// A binding's key string cannot be parsed.
    HotkeyInvalidKey {
        /// Index into [`Library::hotkeys`].
        index: usize,
        /// The text as stored.
        key: String,
    },
    /// A binding points at a sound/scene/playlist that does not exist.
    HotkeyMissingTarget {
        /// Index into [`Library::hotkeys`].
        index: usize,
        /// The key combination as stored.
        key: String,
        /// The unresolved target as stored.
        target: String,
    },
    /// A scene item refers to a sound that is not in the library.
    SceneMissingSound {
        /// Scene id.
        scene: SceneId,
        /// Scene name.
        scene_name: String,
        /// The unknown sound id.
        sound: SoundId,
    },
    /// An emitter refers to a sound that is not in the library.
    EmitterMissingSound {
        /// Scene id.
        scene: SceneId,
        /// Scene name.
        scene_name: String,
        /// Index into [`Scene::emitters`].
        emitter: usize,
        /// The unknown sound id.
        sound: SoundId,
    },
    /// An emitter has no sounds at all.
    EmitterWithoutSounds {
        /// Scene id.
        scene: SceneId,
        /// Scene name.
        scene_name: String,
        /// Index into [`Scene::emitters`].
        emitter: usize,
    },
    /// A scene refers to a playlist that does not exist.
    SceneMissingPlaylist {
        /// Scene id.
        scene: SceneId,
        /// Scene name.
        scene_name: String,
        /// The unknown playlist id.
        playlist: PlaylistId,
    },
    /// A playlist refers to a sound that is not in the library.
    PlaylistMissingSound {
        /// Playlist id.
        playlist: PlaylistId,
        /// Playlist name.
        playlist_name: String,
        /// The unknown sound id.
        sound: SoundId,
    },
}

impl std::fmt::Display for LibraryWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HotkeyConflict { key, indices } => {
                write!(f, "{key} is bound {} times", indices.len())
            }
            Self::HotkeyInvalidKey { key, .. } => write!(f, "{key:?} is not a valid key"),
            Self::HotkeyMissingTarget { key, target, .. } => {
                write!(f, "{key} points at a missing item ({target})")
            }
            Self::SceneMissingSound {
                scene_name, sound, ..
            } => write!(f, "scene {scene_name:?} uses a missing sound ({sound})"),
            Self::EmitterMissingSound {
                scene_name,
                emitter,
                sound,
                ..
            } => write!(
                f,
                "emitter {} of scene {scene_name:?} uses a missing sound ({sound})",
                emitter + 1
            ),
            Self::EmitterWithoutSounds {
                scene_name,
                emitter,
                ..
            } => write!(
                f,
                "emitter {} of scene {scene_name:?} has no sounds",
                emitter + 1
            ),
            Self::SceneMissingPlaylist {
                scene_name,
                playlist,
                ..
            } => write!(
                f,
                "scene {scene_name:?} uses a missing playlist ({playlist})"
            ),
            Self::PlaylistMissingSound {
                playlist_name,
                sound,
                ..
            } => write!(
                f,
                "playlist {playlist_name:?} uses a missing sound ({sound})"
            ),
        }
    }
}

/// The ordered sound library plus its groups, scenes, playlists and hotkeys.
///
/// Insertion order is preserved for every list, and is what `to_config`
/// writes back, so a save/load cycle never reshuffles the UI.
#[derive(Debug, Clone, Default)]
pub struct Library {
    groups: Vec<Group>,
    sounds: Vec<LibrarySound>,
    scenes: Vec<Scene>,
    playlists: Vec<Playlist>,
    hotkeys: Vec<HotkeyConfig>,
    /// `path -> index into sounds`, kept in sync with `sounds`.
    by_path: HashMap<PathBuf, usize>,
    /// `sound id -> index into sounds`, kept in sync with `sounds`.
    by_sound_id: HashMap<SoundId, usize>,
    /// `group id -> index into groups`, kept in sync with `groups`.
    by_id: HashMap<GroupId, usize>,
}

impl Library {
    /// An empty library.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the model from persisted config. Invalid/duplicate group ids and
    /// dangling group references are repaired (dangling => ungrouped).
    ///
    /// Repair rules:
    /// - a `GroupConfig` whose `id` is not a UUID, or that repeats an earlier
    ///   id, is skipped;
    /// - a sound whose `path` is empty is skipped, and so is a repeated path;
    /// - a sound pointing at an unknown group id becomes ungrouped;
    /// - a sound, scene or playlist whose `id` is empty, not a UUID or
    ///   repeated gets a fresh one (this is the v1.4.0 migration; the new id is
    ///   persisted by the next `to_config`). `Config::load_from` already did
    ///   this, so ids are stable across loads;
    /// - scene items, emitter entries and playlist tracks whose id is not even
    ///   a UUID are dropped; ids that are valid but unknown are KEPT so
    ///   nothing is lost, and [`Library::validate`] reports them;
    /// - numbers are clamped into their legal ranges, tags are normalized.
    ///
    /// Hotkeys are not part of `LibraryConfig`: see [`Library::from_full_config`].
    pub fn from_config(cfg: &LibraryConfig) -> Self {
        let mut library = Self::new();

        for persisted in &cfg.groups {
            let Ok(id) = Uuid::parse_str(persisted.id.trim()) else {
                continue;
            };
            if library.by_id.contains_key(&id) {
                continue;
            }
            // Names are persisted verbatim; only surrounding whitespace is
            // dropped, since that is what `add_group` refuses to store.
            let name = persisted.name.trim().to_string();
            if name.is_empty() || name.chars().count() > MAX_GROUP_NAME_LEN {
                continue;
            }
            library.by_id.insert(id, library.groups.len());
            library.groups.push(Group { id, name });
        }

        for persisted in &cfg.sounds {
            if persisted.path.trim().is_empty() {
                continue;
            }
            let path = PathBuf::from(&persisted.path);
            if library.by_path.contains_key(&path) {
                continue;
            }
            let group = persisted
                .group
                .as_deref()
                .and_then(|raw| Uuid::parse_str(raw.trim()).ok())
                .filter(|id| library.by_id.contains_key(id));

            let id = fresh_if_unusable(&persisted.id, |id| library.by_sound_id.contains_key(id));
            let index = library.sounds.len();
            library.by_path.insert(path.clone(), index);
            library.by_sound_id.insert(id, index);
            library.sounds.push(LibrarySound {
                id,
                path,
                name: persisted.name.clone(),
                volume: persisted.volume,
                looped: persisted.looped,
                group,
                tags: normalize_tags(persisted.tags.iter().map(String::as_str)),
                gain_db: clamp_gain_db(persisted.gain_db),
                bus: persisted.bus,
                oneshot_stinger: persisted.oneshot_stinger,
            });
        }

        let mut playlist_ids: HashSet<PlaylistId> = HashSet::new();
        for persisted in &cfg.playlists {
            let id = fresh_if_unusable(&persisted.id, |id| playlist_ids.contains(id));
            playlist_ids.insert(id);
            library.playlists.push(Playlist {
                id,
                name: persisted.name.clone(),
                items: parse_ids(&persisted.items),
                shuffle: persisted.shuffle,
                repeat: persisted.repeat,
                crossfade_ms: persisted.crossfade_ms,
                bus: persisted.bus,
            });
        }

        let mut scene_ids: HashSet<SceneId> = HashSet::new();
        for persisted in &cfg.scenes {
            let id = fresh_if_unusable(&persisted.id, |id| scene_ids.contains(id));
            scene_ids.insert(id);
            library.scenes.push(Scene {
                id,
                name: persisted.name.clone(),
                color: persisted
                    .color
                    .as_deref()
                    .map(str::trim)
                    .filter(|c| is_hex_color(c))
                    .map(str::to_string),
                fade_in_ms: persisted.fade_in_ms,
                fade_out_ms: persisted.fade_out_ms,
                curve: persisted.curve,
                stop_others: persisted.stop_others,
                items: persisted
                    .items
                    .iter()
                    .filter_map(|item| {
                        Some(SceneItem {
                            sound: Uuid::parse_str(item.sound.trim()).ok()?,
                            volume: clamp_volume(item.volume),
                            looped: item.looped,
                            start: item.start,
                            delay_ms: item.delay_ms,
                        })
                    })
                    .collect(),
                emitters: persisted
                    .emitters
                    .iter()
                    .map(|e| {
                        sanitize_emitter(Emitter {
                            sounds: parse_ids(&e.sounds),
                            min_interval_ms: e.min_interval_ms,
                            max_interval_ms: e.max_interval_ms,
                            volume: e.volume,
                            volume_jitter: e.volume_jitter,
                            pan_jitter: e.pan_jitter,
                            max_overlap: e.max_overlap,
                        })
                    })
                    .collect(),
                playlist: persisted
                    .playlist
                    .as_deref()
                    .and_then(|raw| Uuid::parse_str(raw.trim()).ok()),
            });
        }

        library
    }

    /// Like [`Library::from_config`] but also loads `[[hotkeys]]` from the
    /// whole config. Bindings are kept as stored (even invalid or conflicting
    /// ones, see [`Library::validate`]); a target that is a UUID is
    /// re-spelled in the canonical lower-case form.
    pub fn from_full_config(config: &Config) -> Self {
        let mut library = Self::from_config(&config.library);
        library.hotkeys = config
            .hotkeys
            .iter()
            .map(|h| HotkeyConfig {
                key: h.key.clone(),
                action: normalize_action(&h.action),
            })
            .collect();
        library
    }

    /// Serialize back to config. Stable ordering (insertion order preserved).
    ///
    /// Does not include hotkeys (they are top-level in `config.toml`): use
    /// [`Library::write_config`] to save everything.
    pub fn to_config(&self) -> LibraryConfig {
        LibraryConfig {
            groups: self
                .groups
                .iter()
                .map(|group| GroupConfig {
                    id: group.id.to_string(),
                    name: group.name.clone(),
                })
                .collect(),
            sounds: self
                .sounds
                .iter()
                .map(|sound| SoundConfigEntry {
                    id: sound.id.to_string(),
                    path: sound.path.to_string_lossy().into_owned(),
                    name: sound.name.clone(),
                    volume: sound.volume,
                    looped: sound.looped,
                    group: sound.group.map(|id| id.to_string()),
                    tags: sound.tags.clone(),
                    gain_db: sound.gain_db,
                    bus: sound.bus,
                    oneshot_stinger: sound.oneshot_stinger,
                })
                .collect(),
            scenes: self
                .scenes
                .iter()
                .map(|scene| SceneConfig {
                    id: scene.id.to_string(),
                    name: scene.name.clone(),
                    color: scene.color.clone(),
                    fade_in_ms: scene.fade_in_ms,
                    fade_out_ms: scene.fade_out_ms,
                    curve: scene.curve,
                    stop_others: scene.stop_others,
                    items: scene
                        .items
                        .iter()
                        .map(|item| SceneItemConfig {
                            sound: item.sound.to_string(),
                            volume: item.volume,
                            looped: item.looped,
                            start: item.start,
                            delay_ms: item.delay_ms,
                        })
                        .collect(),
                    emitters: scene
                        .emitters
                        .iter()
                        .map(|e| EmitterConfig {
                            sounds: e.sounds.iter().map(Uuid::to_string).collect(),
                            min_interval_ms: e.min_interval_ms,
                            max_interval_ms: e.max_interval_ms,
                            volume: e.volume,
                            volume_jitter: e.volume_jitter,
                            pan_jitter: e.pan_jitter,
                            max_overlap: e.max_overlap,
                        })
                        .collect(),
                    playlist: scene.playlist.map(|id| id.to_string()),
                })
                .collect(),
            playlists: self
                .playlists
                .iter()
                .map(|playlist| PlaylistConfig {
                    id: playlist.id.to_string(),
                    name: playlist.name.clone(),
                    items: playlist.items.iter().map(Uuid::to_string).collect(),
                    shuffle: playlist.shuffle,
                    repeat: playlist.repeat,
                    crossfade_ms: playlist.crossfade_ms,
                    bus: playlist.bus,
                })
                .collect(),
        }
    }

    /// The bindings as they are persisted in `Config::hotkeys`.
    pub fn to_hotkeys(&self) -> Vec<HotkeyConfig> {
        self.hotkeys.clone()
    }

    /// Store the whole library (`[library]` and `[[hotkeys]]`) into `config`,
    /// leaving every other section untouched.
    pub fn write_config(&self, config: &mut Config) {
        config.library = self.to_config();
        config.hotkeys = self.to_hotkeys();
    }

    // ------------------------------------------------------------ groups ----

    /// All groups, in insertion order.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Look up a group by id.
    pub fn group(&self, id: GroupId) -> Option<&Group> {
        self.by_id
            .get(&id)
            .and_then(|index| self.groups.get(*index))
    }

    /// Look up a group's name by id.
    pub fn group_name(&self, id: GroupId) -> Option<&str> {
        self.group(id).map(|group| group.name.as_str())
    }

    /// Create a group. Returns None when the trimmed name is empty, longer than
    /// `MAX_GROUP_NAME_LEN`, or already used case-insensitively.
    pub fn add_group(&mut self, name: &str) -> Option<GroupId> {
        let name = normalize_group_name(name)?;
        if self.name_taken(&name, None) {
            return None;
        }
        let id = Uuid::new_v4();
        self.by_id.insert(id, self.groups.len());
        self.groups.push(Group { id, name });
        Some(id)
    }

    /// Rename. Returns false when id is unknown or the new name is invalid/duplicate.
    ///
    /// Renaming a group to its own current name (ignoring case and surrounding
    /// whitespace) succeeds and normalizes the stored spelling.
    pub fn rename_group(&mut self, id: GroupId, name: &str) -> bool {
        let Some(name) = normalize_group_name(name) else {
            return false;
        };
        if self.name_taken(&name, Some(id)) {
            return false;
        }
        let Some(index) = self.by_id.get(&id).copied() else {
            return false;
        };
        match self.groups.get_mut(index) {
            Some(group) => {
                group.name = name;
                true
            }
            None => false,
        }
    }

    /// Delete a group; its sounds become ungrouped. Returns false if unknown.
    pub fn remove_group(&mut self, id: GroupId) -> bool {
        let Some(index) = self.by_id.get(&id).copied() else {
            return false;
        };
        self.groups.remove(index);
        self.reindex_groups();

        for sound in &mut self.sounds {
            if sound.group == Some(id) {
                sound.group = None;
            }
        }
        true
    }

    // ------------------------------------------------------------ sounds ----

    /// All sounds, in insertion order.
    pub fn sounds(&self) -> &[LibrarySound] {
        &self.sounds
    }

    /// Look up a sound by its exact path.
    pub fn sound(&self, path: &Path) -> Option<&LibrarySound> {
        self.by_path
            .get(path)
            .and_then(|index| self.sounds.get(*index))
    }

    /// Look up a sound mutably by its exact path.
    pub fn sound_mut(&mut self, path: &Path) -> Option<&mut LibrarySound> {
        let index = *self.by_path.get(path)?;
        self.sounds.get_mut(index)
    }

    /// Look up a sound by its stable id.
    pub fn sound_by_id(&self, id: SoundId) -> Option<&LibrarySound> {
        self.by_sound_id
            .get(&id)
            .and_then(|index| self.sounds.get(*index))
    }

    /// Id of the sound registered at `path`.
    pub fn id_of(&self, path: &Path) -> Option<SoundId> {
        self.sound(path).map(|sound| sound.id)
    }

    /// Path of the sound with `id`.
    pub fn path_of(&self, id: SoundId) -> Option<&Path> {
        self.sound_by_id(id).map(|sound| sound.path.as_path())
    }

    /// Register a sound path. Returns true if it was newly added, false if it
    /// already existed (in which case name/volume/group are left untouched).
    /// A new random id is assigned.
    ///
    /// An empty path, and a group id that is not a known group, are rejected the
    /// same way `assign` treats them: the entry is skipped / stored as ungrouped.
    pub fn insert_sound(
        &mut self,
        path: &Path,
        name: &str,
        volume: f32,
        group: Option<GroupId>,
    ) -> bool {
        self.insert_sound_with_id(Uuid::new_v4(), path, name, volume, group)
    }

    /// Like [`Library::insert_sound`] but with a caller-chosen id (bundle
    /// import keeps the ids of the exported set). Returns false when the path
    /// or the id is already registered, or the path is empty.
    pub fn insert_sound_with_id(
        &mut self,
        id: SoundId,
        path: &Path,
        name: &str,
        volume: f32,
        group: Option<GroupId>,
    ) -> bool {
        if path.as_os_str().is_empty()
            || self.by_path.contains_key(path)
            || self.by_sound_id.contains_key(&id)
        {
            return false;
        }
        let group = group.filter(|id| self.by_id.contains_key(id));
        let index = self.sounds.len();
        self.by_path.insert(path.to_path_buf(), index);
        self.by_sound_id.insert(id, index);
        self.sounds.push(LibrarySound {
            id,
            path: path.to_path_buf(),
            name: name.to_string(),
            volume,
            looped: false,
            group,
            tags: Vec::new(),
            gain_db: 0.0,
            bus: Bus::default(),
            oneshot_stinger: false,
        });
        true
    }

    /// Forget a sound and every reference to it (see the module docs).
    /// Returns false if it was not registered.
    pub fn remove_sound(&mut self, path: &Path) -> bool {
        self.remove_sound_report(path).is_some()
    }

    /// [`Library::remove_sound`] that tells what else was removed; `None` when
    /// the sound was not registered.
    pub fn remove_sound_report(&mut self, path: &Path) -> Option<RemovedRefs> {
        let index = self.by_path.remove(path)?;
        let removed = self.sounds.remove(index);
        self.reindex_paths();
        Some(self.purge_sound_refs(removed.id))
    }

    /// Change where a sound lives on disk, keeping its id (and so every scene,
    /// playlist and hotkey that uses it). Returns false when `old` is unknown,
    /// `new` is empty, or `new` is already registered.
    pub fn relocate_sound(&mut self, old: &Path, new: &Path) -> bool {
        if new.as_os_str().is_empty() || self.by_path.contains_key(new) {
            return false;
        }
        let Some(index) = self.by_path.remove(old) else {
            return false;
        };
        self.sounds[index].path = new.to_path_buf();
        self.by_path.insert(new.to_path_buf(), index);
        true
    }

    /// Assign (or clear with None) a sound's group. Ignores unknown group ids
    /// (treats them as None). Returns false if the sound is unknown.
    pub fn assign(&mut self, path: &Path, group: Option<GroupId>) -> bool {
        let group = group.filter(|id| self.by_id.contains_key(id));
        match self.sound_mut(path) {
            Some(sound) => {
                sound.group = group;
                true
            }
            None => false,
        }
    }

    /// Volume is clamped to 0.0..=1.0. Returns false if the sound is unknown.
    ///
    /// A NaN volume is treated as 0.0 rather than propagated into the mixer gain.
    pub fn set_volume(&mut self, path: &Path, volume: f32) -> bool {
        let volume = clamp_volume(volume);
        match self.sound_mut(path) {
            Some(sound) => {
                sound.volume = volume;
                true
            }
            None => false,
        }
    }

    /// Turn the "repeat" flag on or off. Returns false if the sound is unknown.
    ///
    /// The flag is part of the persisted library, so a sound that is set to
    /// repeat is still set to repeat after a restart.
    pub fn set_looped(&mut self, path: &Path, looped: bool) -> bool {
        match self.sound_mut(path) {
            Some(sound) => {
                sound.looped = looped;
                true
            }
            None => false,
        }
    }

    /// Set the dB correction, clamped to `MIN_GAIN_DB..=MAX_GAIN_DB` (NaN =>
    /// 0). Returns false if the sound is unknown.
    pub fn set_gain_db(&mut self, path: &Path, gain_db: f32) -> bool {
        let gain_db = clamp_gain_db(gain_db);
        match self.sound_mut(path) {
            Some(sound) => {
                sound.gain_db = gain_db;
                true
            }
            None => false,
        }
    }

    /// Route the sound to a bus. Returns false if the sound is unknown.
    pub fn set_bus(&mut self, path: &Path, bus: Bus) -> bool {
        match self.sound_mut(path) {
            Some(sound) => {
                sound.bus = bus;
                true
            }
            None => false,
        }
    }

    /// Mark / unmark the sound as a stinger. Returns false if the sound is
    /// unknown.
    pub fn set_oneshot_stinger(&mut self, path: &Path, stinger: bool) -> bool {
        match self.sound_mut(path) {
            Some(sound) => {
                sound.oneshot_stinger = stinger;
                true
            }
            None => false,
        }
    }

    // -------------------------------------------------------------- tags ----

    /// Add a tag (normalized by [`normalize_tag`]). Returns true only when the
    /// tag was newly added; false for an unknown sound, an unusable tag, or a
    /// tag the sound already has.
    pub fn add_tag(&mut self, path: &Path, tag: &str) -> bool {
        let Some(tag) = normalize_tag(tag) else {
            return false;
        };
        let Some(sound) = self.sound_mut(path) else {
            return false;
        };
        if sound.tags.contains(&tag) {
            return false;
        }
        sound.tags.push(tag);
        true
    }

    /// Remove a tag (compared after normalization). Returns true when it was
    /// present.
    pub fn remove_tag(&mut self, path: &Path, tag: &str) -> bool {
        let Some(tag) = normalize_tag(tag) else {
            return false;
        };
        let Some(sound) = self.sound_mut(path) else {
            return false;
        };
        let before = sound.tags.len();
        sound.tags.retain(|t| *t != tag);
        sound.tags.len() != before
    }

    /// Replace all tags of a sound (normalized, de-duplicated, order kept).
    /// Returns false if the sound is unknown.
    pub fn set_tags(&mut self, path: &Path, tags: &[&str]) -> bool {
        let tags = normalize_tags(tags.iter().copied());
        match self.sound_mut(path) {
            Some(sound) => {
                sound.tags = tags;
                true
            }
            None => false,
        }
    }

    /// Every tag used by any sound, sorted and unique.
    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .sounds
            .iter()
            .flat_map(|sound| sound.tags.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        tags.sort();
        tags
    }

    /// Sounds that carry `tag` (normalized first), in library order.
    pub fn sounds_with_tag(&self, tag: &str) -> Vec<&LibrarySound> {
        let Some(tag) = normalize_tag(tag) else {
            return Vec::new();
        };
        self.sounds
            .iter()
            .filter(|sound| sound.tags.contains(&tag))
            .collect()
    }

    // ----------------------------------------------------------- filters ----

    /// Does this sound belong to the given filter?
    ///
    /// An unregistered path belongs to no filter, including `All`.
    pub fn matches(&self, path: &Path, filter: GroupFilter) -> bool {
        let Some(sound) = self.sound(path) else {
            return false;
        };
        match filter {
            GroupFilter::All => true,
            GroupFilter::Ungrouped => sound.group.is_none(),
            GroupFilter::Group(id) => sound.group == Some(id),
        }
    }

    /// Number of sounds in the filter.
    pub fn count_for(&self, filter: GroupFilter) -> usize {
        self.sounds
            .iter()
            .filter(|s| filter_accepts(filter, s))
            .count()
    }

    /// Sounds in the filter, preserving library order.
    pub fn sounds_for(&self, filter: GroupFilter) -> Vec<&LibrarySound> {
        self.sounds
            .iter()
            .filter(|sound| filter_accepts(filter, sound))
            .collect()
    }

    /// Total number of registered sounds.
    pub fn len(&self) -> usize {
        self.sounds.len()
    }

    /// True when no sound is registered (groups may still exist).
    pub fn is_empty(&self) -> bool {
        self.sounds.is_empty()
    }

    // ----------------------------------------------------- missing files ----

    /// Sounds whose file is not on disk (a "file not found" marker for the UI).
    ///
    /// This asks the file system once per sound, so call it from a worker
    /// thread for big libraries (or use [`Library::missing_sounds_with`] with
    /// a cached answer). A path that cannot be inspected (no permission, the
    /// volume is not mounted) counts as missing. Missing sounds stay in the
    /// library with every scene / playlist reference intact.
    pub fn missing_sounds(&self) -> Vec<&LibrarySound> {
        self.missing_sounds_with(|path| path.exists())
    }

    /// [`Library::missing_sounds`] with the existence check supplied by the
    /// caller (pure; used by tests and by callers that cache `stat` results).
    pub fn missing_sounds_with(&self, exists: impl Fn(&Path) -> bool) -> Vec<&LibrarySound> {
        self.sounds
            .iter()
            .filter(|sound| !exists(&sound.path))
            .collect()
    }

    /// Point the sound registered at `path_old` at `path_new` ("Find again").
    ///
    /// The id, and so every scene, playlist and hotkey that uses the sound, is
    /// kept. Returns false when `path_old` is unknown, `path_new` is empty, or
    /// `path_new` already belongs to a different sound. Relinking a sound to
    /// its own path succeeds without changing anything.
    pub fn relink(&mut self, path_old: &Path, path_new: &Path) -> bool {
        if path_old == path_new {
            return self.by_path.contains_key(path_old);
        }
        self.relocate_sound(path_old, path_new)
    }

    /// Which sounds [`Library::remap_prefix`] would rewrite and to what,
    /// without changing anything (for a "N paths will change" preview).
    ///
    /// A sound is rewritten when its path lies under `old_prefix` (matching
    /// whole path components, so `/music` does not match `/music2/a.wav`; both
    /// `/` and `\` separate components when the prefix is written with
    /// backslashes, e.g. a Windows library opened on another system). The part
    /// below the prefix is appended to `new_prefix`, converting separators when
    /// the two prefixes use different styles. Sounds whose new path would be
    /// empty, equal to the path of a sound that stays, or equal to another
    /// rewritten path are left out, so the plan never merges two sounds.
    /// An empty prefix (or one that is only separators) plans nothing.
    pub fn remap_prefix_plan(&self, old_prefix: &str, new_prefix: &str) -> Vec<(SoundId, PathBuf)> {
        let mut planned: Vec<(SoundId, PathBuf)> = Vec::new();
        for sound in &self.sounds {
            let Some(text) = sound.path.to_str() else {
                continue;
            };
            if let Some(new_text) = remap_path_string(text, old_prefix, new_prefix) {
                if new_text != text {
                    planned.push((sound.id, PathBuf::from(new_text)));
                }
            }
        }
        if planned.is_empty() {
            return planned;
        }
        let moving: HashSet<SoundId> = planned.iter().map(|(id, _)| *id).collect();
        let staying: HashSet<&Path> = self
            .sounds
            .iter()
            .filter(|sound| !moving.contains(&sound.id))
            .map(|sound| sound.path.as_path())
            .collect();
        let mut uses: HashMap<&Path, usize> = HashMap::new();
        for (_, path) in &planned {
            *uses.entry(path.as_path()).or_insert(0) += 1;
        }
        let keep: Vec<bool> = planned
            .iter()
            .map(|(_, path)| !staying.contains(path.as_path()) && uses[path.as_path()] == 1)
            .collect();
        planned
            .into_iter()
            .zip(keep)
            .filter_map(|(entry, keep)| keep.then_some(entry))
            .collect()
    }

    /// Rewrite the path prefix of every sound under `old_prefix` to
    /// `new_prefix` (bulk "my sound folder moved"). Ids are never touched.
    /// Returns the number of rewritten paths; see
    /// [`Library::remap_prefix_plan`] for the matching rules.
    pub fn remap_prefix(&mut self, old_prefix: &str, new_prefix: &str) -> usize {
        let plan = self.remap_prefix_plan(old_prefix, new_prefix);
        if plan.is_empty() {
            return 0;
        }
        let count = plan.len();
        let new_paths: HashMap<SoundId, PathBuf> = plan.into_iter().collect();
        for sound in &mut self.sounds {
            if let Some(path) = new_paths.get(&sound.id) {
                sound.path = path.clone();
            }
        }
        self.reindex_paths();
        count
    }

    /// Replace the persisted content (groups, sounds, scenes, playlists) with
    /// `cfg`, going through the same repair rules as [`Library::from_config`],
    /// but keep the keyboard bindings. Bundle import builds the merged
    /// `LibraryConfig` and applies it here in one step, so a failed import
    /// never leaves the library half-changed.
    pub fn apply_library_config(&mut self, cfg: &LibraryConfig) {
        let rebuilt = Self::from_config(cfg);
        let hotkeys = std::mem::take(&mut self.hotkeys);
        *self = rebuilt;
        self.hotkeys = hotkeys;
    }

    // ------------------------------------------------------------ scenes ----

    /// All scenes, in display order.
    pub fn scenes(&self) -> &[Scene] {
        &self.scenes
    }

    /// Look up a scene by id.
    pub fn scene(&self, id: SceneId) -> Option<&Scene> {
        self.scenes.iter().find(|scene| scene.id == id)
    }

    /// Create an empty scene with the schema defaults (2 s fades, equal-power,
    /// stops other sounds). Returns None when the trimmed name is empty, longer
    /// than `MAX_NAME_LEN` or already used by another scene (case-insensitive).
    pub fn add_scene(&mut self, name: &str) -> Option<SceneId> {
        let name = normalize_name(name, MAX_NAME_LEN)?;
        if name_in_use(
            self.scenes.iter().map(|s| (s.id, s.name.as_str())),
            &name,
            None,
        ) {
            return None;
        }
        let defaults = SceneConfig::default();
        let id = Uuid::new_v4();
        self.scenes.push(Scene {
            id,
            name,
            color: None,
            fade_in_ms: defaults.fade_in_ms,
            fade_out_ms: defaults.fade_out_ms,
            curve: defaults.curve,
            stop_others: defaults.stop_others,
            items: Vec::new(),
            emitters: Vec::new(),
            playlist: None,
        });
        Some(id)
    }

    /// Rename a scene. False when unknown or the name is invalid / taken.
    pub fn rename_scene(&mut self, id: SceneId, name: &str) -> bool {
        let Some(name) = normalize_name(name, MAX_NAME_LEN) else {
            return false;
        };
        if name_in_use(
            self.scenes.iter().map(|s| (s.id, s.name.as_str())),
            &name,
            Some(id),
        ) {
            return false;
        }
        match self.scene_mut_internal(id) {
            Some(scene) => {
                scene.name = name;
                true
            }
            None => false,
        }
    }

    /// Delete a scene and the hotkeys that activate it. False when unknown.
    pub fn remove_scene(&mut self, id: SceneId) -> bool {
        let Some(index) = self.scenes.iter().position(|scene| scene.id == id) else {
            return false;
        };
        self.scenes.remove(index);
        self.retain_hotkeys(|action| !target_is(action, "scene", id));
        true
    }

    /// Move a scene to position `to` (clamped to the last index). False when
    /// unknown.
    pub fn reorder_scene(&mut self, id: SceneId, to: usize) -> bool {
        match self.scenes.iter().position(|scene| scene.id == id) {
            Some(from) => {
                move_to(&mut self.scenes, from, to);
                true
            }
            None => false,
        }
    }

    /// Set the button color: `#rrggbb` or `#rgb`, or `None` to clear. False
    /// when the scene is unknown or the color is malformed.
    pub fn set_scene_color(&mut self, id: SceneId, color: Option<&str>) -> bool {
        let color = match color.map(str::trim) {
            Some(text) if is_hex_color(text) => Some(text.to_string()),
            Some(_) => return false,
            None => None,
        };
        match self.scene_mut_internal(id) {
            Some(scene) => {
                scene.color = color;
                true
            }
            None => false,
        }
    }

    /// Set the fade times, in ms. False when the scene is unknown.
    pub fn set_scene_fades(&mut self, id: SceneId, fade_in_ms: u32, fade_out_ms: u32) -> bool {
        match self.scene_mut_internal(id) {
            Some(scene) => {
                scene.fade_in_ms = fade_in_ms;
                scene.fade_out_ms = fade_out_ms;
                true
            }
            None => false,
        }
    }

    /// Set the fade shape. False when the scene is unknown.
    pub fn set_scene_curve(&mut self, id: SceneId, curve: FadeCurve) -> bool {
        match self.scene_mut_internal(id) {
            Some(scene) => {
                scene.curve = curve;
                true
            }
            None => false,
        }
    }

    /// Set whether the scene fades out sounds that are not part of it. False
    /// when the scene is unknown.
    pub fn set_scene_stop_others(&mut self, id: SceneId, stop_others: bool) -> bool {
        match self.scene_mut_internal(id) {
            Some(scene) => {
                scene.stop_others = stop_others;
                true
            }
            None => false,
        }
    }

    /// Attach (or detach with `None`) the scene's music playlist. False when
    /// the scene is unknown or the playlist does not exist.
    pub fn set_scene_playlist(&mut self, id: SceneId, playlist: Option<PlaylistId>) -> bool {
        if let Some(playlist) = playlist {
            if self.playlist(playlist).is_none() {
                return false;
            }
        }
        match self.scene_mut_internal(id) {
            Some(scene) => {
                scene.playlist = playlist;
                true
            }
            None => false,
        }
    }

    /// Add the item, or replace the existing item of the same sound (a sound
    /// appears once per scene; a new one goes to the end). Volume is clamped.
    /// False when the scene or the sound is unknown.
    pub fn set_item(&mut self, scene: SceneId, item: SceneItem) -> bool {
        if self.sound_by_id(item.sound).is_none() {
            return false;
        }
        let item = SceneItem {
            volume: clamp_volume(item.volume),
            ..item
        };
        let Some(scene) = self.scene_mut_internal(scene) else {
            return false;
        };
        match scene.items.iter_mut().find(|i| i.sound == item.sound) {
            Some(existing) => *existing = item,
            None => scene.items.push(item),
        }
        true
    }

    /// Remove a sound from a scene. False when the scene is unknown or does not
    /// contain it.
    pub fn remove_item(&mut self, scene: SceneId, sound: SoundId) -> bool {
        let Some(scene) = self.scene_mut_internal(scene) else {
            return false;
        };
        let before = scene.items.len();
        scene.items.retain(|item| item.sound != sound);
        scene.items.len() != before
    }

    /// Move an item of a scene to position `to` (clamped). False when the
    /// scene is unknown or does not contain the sound.
    pub fn reorder_item(&mut self, scene: SceneId, sound: SoundId, to: usize) -> bool {
        let Some(scene) = self.scene_mut_internal(scene) else {
            return false;
        };
        match scene.items.iter().position(|item| item.sound == sound) {
            Some(from) => {
                move_to(&mut scene.items, from, to);
                true
            }
            None => false,
        }
    }

    /// Replace all emitters of a scene, atomically.
    ///
    /// Numbers are sanitized (levels and jitters clamped to `0..=1`, min/max
    /// interval swapped when reversed, `max_overlap >= 1`) and repeated sounds
    /// inside an emitter are collapsed. The whole call is refused (false, the
    /// scene is unchanged) when the scene is unknown, an emitter has no sounds
    /// or an emitter names a sound that is not in the library.
    pub fn set_emitters(&mut self, scene: SceneId, emitters: Vec<Emitter>) -> bool {
        let all_known = emitters.iter().all(|emitter| {
            !emitter.sounds.is_empty()
                && emitter
                    .sounds
                    .iter()
                    .all(|sound| self.sound_by_id(*sound).is_some())
        });
        if !all_known {
            return false;
        }
        let Some(scene) = self.scene_mut_internal(scene) else {
            return false;
        };
        scene.emitters = emitters.into_iter().map(sanitize_emitter).collect();
        true
    }

    // --------------------------------------------------------- playlists ----

    /// All playlists, in display order.
    pub fn playlists(&self) -> &[Playlist] {
        &self.playlists
    }

    /// Look up a playlist by id.
    pub fn playlist(&self, id: PlaylistId) -> Option<&Playlist> {
        self.playlists.iter().find(|playlist| playlist.id == id)
    }

    /// Create an empty playlist (no shuffle, repeat all, 2 s crossfade, music
    /// bus). Name rules as for [`Library::add_scene`].
    pub fn add_playlist(&mut self, name: &str) -> Option<PlaylistId> {
        let name = normalize_name(name, MAX_NAME_LEN)?;
        if name_in_use(
            self.playlists.iter().map(|p| (p.id, p.name.as_str())),
            &name,
            None,
        ) {
            return None;
        }
        let defaults = PlaylistConfig::default();
        let id = Uuid::new_v4();
        self.playlists.push(Playlist {
            id,
            name,
            items: Vec::new(),
            shuffle: defaults.shuffle,
            repeat: defaults.repeat,
            crossfade_ms: defaults.crossfade_ms,
            bus: defaults.bus,
        });
        Some(id)
    }

    /// Rename a playlist. False when unknown or the name is invalid / taken.
    pub fn rename_playlist(&mut self, id: PlaylistId, name: &str) -> bool {
        let Some(name) = normalize_name(name, MAX_NAME_LEN) else {
            return false;
        };
        if name_in_use(
            self.playlists.iter().map(|p| (p.id, p.name.as_str())),
            &name,
            Some(id),
        ) {
            return false;
        }
        match self.playlist_mut_internal(id) {
            Some(playlist) => {
                playlist.name = name;
                true
            }
            None => false,
        }
    }

    /// Delete a playlist; scenes that used it lose their music layer and the
    /// hotkeys that start it are removed. False when unknown.
    pub fn remove_playlist(&mut self, id: PlaylistId) -> bool {
        let Some(index) = self.playlists.iter().position(|p| p.id == id) else {
            return false;
        };
        self.playlists.remove(index);
        for scene in &mut self.scenes {
            if scene.playlist == Some(id) {
                scene.playlist = None;
            }
        }
        self.retain_hotkeys(|action| !target_is(action, "playlist", id));
        true
    }

    /// Move a playlist to position `to` (clamped). False when unknown.
    pub fn reorder_playlist(&mut self, id: PlaylistId, to: usize) -> bool {
        match self.playlists.iter().position(|p| p.id == id) {
            Some(from) => {
                move_to(&mut self.playlists, from, to);
                true
            }
            None => false,
        }
    }

    /// Replace the tracks. False (unchanged) when the playlist is unknown or
    /// any sound is not in the library.
    pub fn set_playlist_items(&mut self, id: PlaylistId, items: Vec<SoundId>) -> bool {
        if !items.iter().all(|sound| self.sound_by_id(*sound).is_some()) {
            return false;
        }
        match self.playlist_mut_internal(id) {
            Some(playlist) => {
                playlist.items = items;
                true
            }
            None => false,
        }
    }

    /// Insert a track at `at` (clamped; `None` = end). False when the playlist
    /// or the sound is unknown.
    pub fn add_playlist_item(&mut self, id: PlaylistId, sound: SoundId, at: Option<usize>) -> bool {
        if self.sound_by_id(sound).is_none() {
            return false;
        }
        match self.playlist_mut_internal(id) {
            Some(playlist) => {
                let at = at.unwrap_or(playlist.items.len()).min(playlist.items.len());
                playlist.items.insert(at, sound);
                true
            }
            None => false,
        }
    }

    /// Remove the track at `position`. False when out of range or unknown.
    pub fn remove_playlist_item(&mut self, id: PlaylistId, position: usize) -> bool {
        match self.playlist_mut_internal(id) {
            Some(playlist) if position < playlist.items.len() => {
                playlist.items.remove(position);
                true
            }
            _ => false,
        }
    }

    /// Set shuffle and end-of-list behaviour. False when unknown.
    pub fn set_playlist_mode(&mut self, id: PlaylistId, shuffle: bool, repeat: RepeatMode) -> bool {
        match self.playlist_mut_internal(id) {
            Some(playlist) => {
                playlist.shuffle = shuffle;
                playlist.repeat = repeat;
                true
            }
            None => false,
        }
    }

    /// Set the crossfade in ms. False when unknown.
    pub fn set_playlist_crossfade(&mut self, id: PlaylistId, crossfade_ms: u32) -> bool {
        match self.playlist_mut_internal(id) {
            Some(playlist) => {
                playlist.crossfade_ms = crossfade_ms;
                true
            }
            None => false,
        }
    }

    /// Set the bus. False when unknown.
    pub fn set_playlist_bus(&mut self, id: PlaylistId, bus: Bus) -> bool {
        match self.playlist_mut_internal(id) {
            Some(playlist) => {
                playlist.bus = bus;
                true
            }
            None => false,
        }
    }

    // ----------------------------------------------------------- hotkeys ----

    /// All bindings, in the order they were made.
    pub fn hotkeys(&self) -> &[HotkeyConfig] {
        &self.hotkeys
    }

    /// Bind `key` to `action`; returns the index of the new binding.
    ///
    /// The key is stored in canonical spelling (see `canonical_hotkey`) and a
    /// target is stored as a lower-case UUID. Refused when the key is not a
    /// key combination or the target does not exist. A key that is already
    /// bound is NOT refused (so a dialog can offer "replace"): check
    /// [`Library::find_hotkey`] first, and [`Library::validate`] reports the
    /// conflicts that remain.
    pub fn add_hotkey(&mut self, key: &str, action: HotkeyAction) -> Result<usize, HotkeyError> {
        let binding = self.checked_hotkey(key, action)?;
        self.hotkeys.push(binding);
        Ok(self.hotkeys.len() - 1)
    }

    /// Replace the binding at `index` (same checks as [`Library::add_hotkey`]).
    pub fn set_hotkey(
        &mut self,
        index: usize,
        key: &str,
        action: HotkeyAction,
    ) -> Result<(), HotkeyError> {
        if index >= self.hotkeys.len() {
            return Err(HotkeyError::NoSuchHotkey(index));
        }
        self.hotkeys[index] = self.checked_hotkey(key, action)?;
        Ok(())
    }

    /// Remove the binding at `index`. False when out of range.
    pub fn remove_hotkey(&mut self, index: usize) -> bool {
        if index < self.hotkeys.len() {
            self.hotkeys.remove(index);
            true
        } else {
            false
        }
    }

    /// Index of the first binding of `key` (compared canonically), if any.
    pub fn find_hotkey(&self, key: &str) -> Option<usize> {
        let wanted = canonical_hotkey(key)?;
        self.hotkeys
            .iter()
            .position(|h| h.canonical_key().as_deref() == Some(wanted.as_str()))
    }

    /// Key combinations bound more than once: `(canonical key, indices)`, in
    /// order of first appearance. Bindings with an unparsable key are ignored
    /// here (they are reported separately by [`Library::validate`]).
    pub fn hotkey_conflicts(&self) -> Vec<(String, Vec<usize>)> {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (index, hotkey) in self.hotkeys.iter().enumerate() {
            let Some(key) = hotkey.canonical_key() else {
                continue;
            };
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, indices)) => indices.push(index),
                None => groups.push((key, vec![index])),
            }
        }
        groups.retain(|(_, indices)| indices.len() > 1);
        groups
    }

    // -------------------------------------------------------- validation ----

    /// Everything wrong with the library that the UI should warn about:
    /// conflicting or malformed hotkeys, hotkeys pointing at missing items, and
    /// scene / emitter / playlist references to sounds or playlists that do not
    /// exist. Empty when all is well. The editing methods never create such
    /// problems; they come from edited or damaged config files.
    pub fn validate(&self) -> Vec<LibraryWarning> {
        let mut warnings = Vec::new();

        for (key, indices) in self.hotkey_conflicts() {
            warnings.push(LibraryWarning::HotkeyConflict { key, indices });
        }
        for (index, hotkey) in self.hotkeys.iter().enumerate() {
            if hotkey.canonical_key().is_none() {
                warnings.push(LibraryWarning::HotkeyInvalidKey {
                    index,
                    key: hotkey.key.clone(),
                });
            }
            if let Some(target) = hotkey.action.target() {
                if !self.target_exists(&hotkey.action) {
                    warnings.push(LibraryWarning::HotkeyMissingTarget {
                        index,
                        key: hotkey.key.clone(),
                        target: target.to_string(),
                    });
                }
            }
        }

        for scene in &self.scenes {
            for item in &scene.items {
                if self.sound_by_id(item.sound).is_none() {
                    warnings.push(LibraryWarning::SceneMissingSound {
                        scene: scene.id,
                        scene_name: scene.name.clone(),
                        sound: item.sound,
                    });
                }
            }
            for (emitter_index, emitter) in scene.emitters.iter().enumerate() {
                if emitter.sounds.is_empty() {
                    warnings.push(LibraryWarning::EmitterWithoutSounds {
                        scene: scene.id,
                        scene_name: scene.name.clone(),
                        emitter: emitter_index,
                    });
                }
                for sound in &emitter.sounds {
                    if self.sound_by_id(*sound).is_none() {
                        warnings.push(LibraryWarning::EmitterMissingSound {
                            scene: scene.id,
                            scene_name: scene.name.clone(),
                            emitter: emitter_index,
                            sound: *sound,
                        });
                    }
                }
            }
            if let Some(playlist) = scene.playlist {
                if self.playlist(playlist).is_none() {
                    warnings.push(LibraryWarning::SceneMissingPlaylist {
                        scene: scene.id,
                        scene_name: scene.name.clone(),
                        playlist,
                    });
                }
            }
        }

        for playlist in &self.playlists {
            for sound in &playlist.items {
                if self.sound_by_id(*sound).is_none() {
                    warnings.push(LibraryWarning::PlaylistMissingSound {
                        playlist: playlist.id,
                        playlist_name: playlist.name.clone(),
                        sound: *sound,
                    });
                }
            }
        }

        warnings
    }

    // ----------------------------------------------------------- private ----

    fn name_taken(&self, candidate: &str, except: Option<GroupId>) -> bool {
        let lowered = candidate.to_lowercase();
        self.groups
            .iter()
            .any(|group| Some(group.id) != except && group.name.to_lowercase() == lowered)
    }

    fn reindex_groups(&mut self) {
        self.by_id = self
            .groups
            .iter()
            .enumerate()
            .map(|(index, group)| (group.id, index))
            .collect();
    }

    fn reindex_paths(&mut self) {
        self.by_path = self
            .sounds
            .iter()
            .enumerate()
            .map(|(index, sound)| (sound.path.clone(), index))
            .collect();
        self.by_sound_id = self
            .sounds
            .iter()
            .enumerate()
            .map(|(index, sound)| (sound.id, index))
            .collect();
    }

    fn scene_mut_internal(&mut self, id: SceneId) -> Option<&mut Scene> {
        self.scenes.iter_mut().find(|scene| scene.id == id)
    }

    fn playlist_mut_internal(&mut self, id: PlaylistId) -> Option<&mut Playlist> {
        self.playlists.iter_mut().find(|p| p.id == id)
    }

    fn retain_hotkeys(&mut self, keep: impl Fn(&HotkeyAction) -> bool) -> usize {
        let before = self.hotkeys.len();
        self.hotkeys.retain(|h| keep(&h.action));
        before - self.hotkeys.len()
    }

    /// Drop every reference to a sound that has just been removed.
    fn purge_sound_refs(&mut self, id: SoundId) -> RemovedRefs {
        let mut report = RemovedRefs::default();

        for scene in &mut self.scenes {
            let before = scene.items.len();
            scene.items.retain(|item| item.sound != id);
            report.scene_items += before - scene.items.len();

            for emitter in &mut scene.emitters {
                let before = emitter.sounds.len();
                emitter.sounds.retain(|sound| *sound != id);
                report.emitter_refs += before - emitter.sounds.len();
            }
            let before = scene.emitters.len();
            scene.emitters.retain(|emitter| !emitter.sounds.is_empty());
            report.emitters_dropped += before - scene.emitters.len();
        }

        for playlist in &mut self.playlists {
            let before = playlist.items.len();
            playlist.items.retain(|sound| *sound != id);
            report.playlist_items += before - playlist.items.len();
        }

        report.hotkeys = self.retain_hotkeys(|action| !target_is(action, "sound", id));
        report
    }

    fn target_exists(&self, action: &HotkeyAction) -> bool {
        let Some(target) = action.target() else {
            return true;
        };
        let Ok(id) = Uuid::parse_str(target.trim()) else {
            return false;
        };
        match action {
            HotkeyAction::Sound { .. } => self.sound_by_id(id).is_some(),
            HotkeyAction::Scene { .. } => self.scene(id).is_some(),
            HotkeyAction::Playlist { .. } => self.playlist(id).is_some(),
            _ => true,
        }
    }

    fn checked_hotkey(&self, key: &str, action: HotkeyAction) -> Result<HotkeyConfig, HotkeyError> {
        let canonical =
            canonical_hotkey(key).ok_or_else(|| HotkeyError::InvalidKey(key.to_string()))?;
        if !self.target_exists(&action) {
            return Err(HotkeyError::UnknownTarget(
                action.target().unwrap_or_default().to_string(),
            ));
        }
        Ok(HotkeyConfig {
            key: canonical,
            action: normalize_action(&action),
        })
    }
}

/// Parse `raw` as a UUID, or mint a fresh one when it is empty, not a UUID or
/// already taken according to `taken`.
fn fresh_if_unusable(raw: &str, taken: impl Fn(&Uuid) -> bool) -> Uuid {
    match Uuid::parse_str(raw.trim()) {
        Ok(id) if !taken(&id) => id,
        _ => loop {
            let id = Uuid::new_v4();
            if !taken(&id) {
                break id;
            }
        },
    }
}

/// Parse every string as a UUID, dropping the ones that are not.
fn parse_ids(raw: &[String]) -> Vec<Uuid> {
    raw.iter()
        .filter_map(|text| Uuid::parse_str(text.trim()).ok())
        .collect()
}

/// Re-spell a UUID target in canonical lower-case hyphenated form; anything
/// else is kept untouched so [`Library::validate`] can report it.
fn normalize_action(action: &HotkeyAction) -> HotkeyAction {
    let norm = |target: &str| {
        Uuid::parse_str(target.trim())
            .map(|id| id.to_string())
            .unwrap_or_else(|_| target.to_string())
    };
    match action {
        HotkeyAction::Sound { target } => HotkeyAction::Sound {
            target: norm(target),
        },
        HotkeyAction::Scene { target } => HotkeyAction::Scene {
            target: norm(target),
        },
        HotkeyAction::Playlist { target } => HotkeyAction::Playlist {
            target: norm(target),
        },
        other => other.clone(),
    }
}

/// Does `action` have the given kind (`"sound"`, `"scene"`, `"playlist"`) and
/// point at `id`?
fn target_is(action: &HotkeyAction, kind: &str, id: Uuid) -> bool {
    let (matches_kind, target) = match action {
        HotkeyAction::Sound { target } => (kind == "sound", target),
        HotkeyAction::Scene { target } => (kind == "scene", target),
        HotkeyAction::Playlist { target } => (kind == "playlist", target),
        _ => return false,
    };
    matches_kind && Uuid::parse_str(target.trim()).ok() == Some(id)
}

/// Clamp the numbers of an emitter into their legal ranges and collapse
/// repeated sounds.
fn sanitize_emitter(mut emitter: Emitter) -> Emitter {
    let mut seen = HashSet::new();
    emitter.sounds.retain(|sound| seen.insert(*sound));
    if emitter.min_interval_ms > emitter.max_interval_ms {
        std::mem::swap(&mut emitter.min_interval_ms, &mut emitter.max_interval_ms);
    }
    emitter.volume = clamp_volume(emitter.volume);
    emitter.volume_jitter = clamp_volume(emitter.volume_jitter);
    emitter.pan_jitter = clamp_volume(emitter.pan_jitter);
    emitter.max_overlap = emitter.max_overlap.max(1);
    emitter
}

/// Move `items[from]` to index `to` (clamped to the last index).
fn move_to<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if items.is_empty() {
        return;
    }
    let to = to.min(items.len() - 1);
    let item = items.remove(from);
    items.insert(to, item);
}

/// Is `candidate` (case-insensitively) the name of another entry?
fn name_in_use<'a>(
    mut names: impl Iterator<Item = (Uuid, &'a str)>,
    candidate: &str,
    except: Option<Uuid>,
) -> bool {
    let lowered = candidate.to_lowercase();
    names.any(|(id, name)| Some(id) != except && name.to_lowercase() == lowered)
}

/// Trim, then reject empty names and names over `max` chars.
fn normalize_name(name: &str, max: usize) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > max {
        return None;
    }
    Some(trimmed.to_string())
}

/// Normalize a tag: trimmed and lower-cased; `None` when empty or longer than
/// `MAX_TAG_LEN` characters.
pub fn normalize_tag(tag: &str) -> Option<String> {
    let trimmed = tag.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_TAG_LEN {
        return None;
    }
    Some(trimmed.to_lowercase())
}

/// Normalize a list of tags, dropping unusable and repeated ones (first
/// occurrence wins, order kept).
fn normalize_tags<'a>(tags: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags.filter_map(normalize_tag) {
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

/// `#rgb` or `#rrggbb`.
fn is_hex_color(text: &str) -> bool {
    let Some(digits) = text.strip_prefix('#') else {
        return false;
    };
    matches!(digits.len(), 3 | 6) && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Clamp a dB correction, folding NaN to 0 dB.
fn clamp_gain_db(db: f32) -> f32 {
    if db.is_nan() {
        0.0
    } else {
        db.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
    }
}

/// Trim, then reject empty / over-long names.
fn normalize_group_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_GROUP_NAME_LEN {
        return None;
    }
    Some(trimmed.to_string())
}

/// Clamp a gain into the legal range, folding NaN to silence.
fn clamp_volume(volume: f32) -> f32 {
    if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, 1.0)
    }
}

fn filter_accepts(filter: GroupFilter, sound: &LibrarySound) -> bool {
    match filter {
        GroupFilter::All => true,
        GroupFilter::Ungrouped => sound.group.is_none(),
        GroupFilter::Group(id) => sound.group == Some(id),
    }
}

/// How a path prefix spells its separators.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PathStyle {
    Slash,
    Backslash,
}

/// A prefix that uses `\` and never `/` is a Windows-style path.
fn path_style(prefix: &str) -> PathStyle {
    if prefix.contains('\\') && !prefix.contains('/') {
        PathStyle::Backslash
    } else {
        PathStyle::Slash
    }
}

/// Rewrite `path` when it lies under `old_prefix`, giving the same location
/// below `new_prefix`; `None` when it does not, or a prefix is unusable.
///
/// Matches whole components only. `\` counts as a separator when the old
/// prefix is Windows-style (or on Windows); elsewhere it is an ordinary file
/// name character. The remainder is converted when the two prefixes use
/// different separator styles.
fn remap_path_string(path: &str, old_prefix: &str, new_prefix: &str) -> Option<String> {
    let old_style = path_style(old_prefix);
    let new_style = path_style(new_prefix);
    let backslash_separates = old_style == PathStyle::Backslash || cfg!(windows);
    let is_sep = |c: char| c == '/' || (backslash_separates && c == '\\');

    let old = old_prefix.trim_end_matches(is_sep);
    let new = new_prefix.trim_end_matches(['/', '\\']);
    if old.trim().is_empty() || new.trim().is_empty() {
        return None;
    }
    let head = path.get(..old.len())?;
    let same = if cfg!(windows) {
        head.eq_ignore_ascii_case(old)
    } else {
        head == old
    };
    if !same {
        return None;
    }
    let rest = &path[old.len()..];
    if rest.is_empty() {
        return Some(new.to_string());
    }
    if !rest.starts_with(is_sep) {
        return None;
    }
    let rest = rest.trim_start_matches(is_sep);
    if rest.is_empty() {
        return Some(new.to_string());
    }
    let (new_sep, rest) = match (old_style, new_style) {
        (PathStyle::Backslash, PathStyle::Slash) => ('/', rest.replace('\\', "/")),
        (PathStyle::Slash, PathStyle::Backslash) => ('\\', rest.replace('/', "\\")),
        (_, PathStyle::Backslash) => ('\\', rest.to_string()),
        (_, PathStyle::Slash) => ('/', rest.to_string()),
    };
    Some(format!("{new}{new_sep}{rest}"))
}

#[cfg(test)]
mod remap_tests {
    use super::remap_path_string as remap;

    #[test]
    fn matches_whole_components_only() {
        assert_eq!(
            remap("/music/a.wav", "/music", "/mnt/music").as_deref(),
            Some("/mnt/music/a.wav")
        );
        assert_eq!(remap("/music2/a.wav", "/music", "/mnt"), None);
        assert_eq!(remap("/other/a.wav", "/music", "/mnt"), None);
    }

    #[test]
    fn trailing_separators_are_ignored() {
        assert_eq!(
            remap("/music/sub/a.wav", "/music/", "/new//").as_deref(),
            Some("/new/sub/a.wav")
        );
    }

    #[test]
    fn converts_between_windows_and_unix_styles() {
        assert_eq!(
            remap(r"C:\Sounds\Tavern\a.wav", r"C:\Sounds", "/Users/me/Sounds").as_deref(),
            Some("/Users/me/Sounds/Tavern/a.wav")
        );
        assert_eq!(
            remap(
                "/Users/me/Sounds/Tavern/a.wav",
                "/Users/me/Sounds",
                r"D:\Snd"
            )
            .as_deref(),
            Some(r"D:\Snd\Tavern\a.wav")
        );
    }

    #[test]
    fn rejects_unusable_prefixes() {
        assert_eq!(remap("/a/b.wav", "", "/x"), None);
        assert_eq!(remap("/a/b.wav", "/", "/x"), None);
        assert_eq!(remap("/a/b.wav", "/a", ""), None);
        assert_eq!(remap("/a/b.wav", "/a", "///"), None);
    }

    #[test]
    fn does_not_split_inside_a_multibyte_character() {
        assert_eq!(remap("/музыка/a.wav", "/муз", "/x"), None);
        assert_eq!(
            remap("/музыка/a.wav", "/музыка", "/звук").as_deref(),
            Some("/звук/a.wav")
        );
    }
}
