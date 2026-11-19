//! Music playlists: a queue of long tracks with shuffle, repeat, crossfades
//! and a position, played as the *music layer* of a scene.
//!
//! ```text
//!   Scene.playlist --activate_scene--> Director --PlaylistPlayer--> Playback
//!   AudioCommand::Playlist*  ---------> Director (playlist_command)
//! ```
//!
//! # Pieces
//!
//! * [`PlaylistPlayer`] is a state machine over the [`Playback`] trait. It
//!   owns no thread and no clock: it is driven by [`PlaylistPlayer::tick`]
//!   (called from [`Director::tick`]) and by commands, and it reads the
//!   engine's own position/duration/"track ended" signals through
//!   [`MusicPlayback`]. The mixer does all the fading, so a crossfade is just
//!   "start the next track with a fade-in of `crossfade_ms` and fade the
//!   current one out over the same time".
//! * [`MusicLayer`] is what the [`Director`] keeps: the playlists of the
//!   library, the player, and a `watch` channel with [`PlaylistState`].
//! * `impl Director` (bottom of this file) holds the scene hooks and the
//!   commands. The Director's own file only calls into it.
//!
//! # Transitions
//!
//! A track ends in one of two ways and both are handled in `tick`:
//!
//! 1. **Early (crossfade).** With `crossfade_ms > 0` and a known duration the
//!    next track starts when `crossfade_ms` (at most half of the track) is
//!    left, so the two overlap and the new one is fully in when the old one
//!    ends. Sparing the audio thread: only fade commands are sent.
//! 2. **At the end.** The engine reports the end as
//!    [`StreamEvent::TrackFinished`](crate::audio::StreamEvent) (streamed
//!    tracks) and the sound stops reporting as playing (every track). The next
//!    track then starts with the short click-protection ramp.
//!
//! A track that stopped far from its end and without an event was stopped by
//! somebody else (a stop button): the playlist stops too instead of jumping
//! to the next track.
//!
//! # Shuffle
//!
//! Every cycle is a permutation of the item list, so nothing repeats until
//! all tracks were played. The next cycle never starts with the track that
//! ended the previous one (compared by sound id, so a track listed twice
//! cannot play back to back either).
//!
//! # Repeat
//!
//! `Off` stops after the last track ([`PlaylistStatus::Finished`]), `All`
//! starts the next cycle, `One` loops the current track in the mixer
//! (gapless, no event). Manual next/prev leave the loop mode alone: they move
//! to the neighbouring track, which then loops.
//!
//! # Missing tracks
//!
//! Items that are not in the library, are not loaded or fail to start are
//! skipped (logged, listed in [`PlaylistState::error`]). When nothing in the
//! playlist can be played, the player stops and reports it; it never panics
//! and never loops forever.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::watch;
use tracing::warn;

use crate::audio::decoder::TrackMeta;
use crate::audio::director::{
    CatalogSound, Director, DirectorError, FadeSpec, Playback, PlaybackResult, SceneReport,
};
use crate::audio::engine::{AudioEngine, EngineError, SoundId};
use crate::audio::mixer::FadeCurve as MixerCurve;
use crate::config::{FadeCurve, PlaylistConfig, RepeatMode, SceneConfig};

/// A stopped track this close to its end counts as "played to the end" even
/// without an event (container durations are estimates for some formats).
const END_TOLERANCE_MS: u64 = 2000;

/// "Previous" restarts the current track when it already played longer than
/// this, and goes to the previous track otherwise.
pub const RESTART_THRESHOLD_MS: u64 = 3000;

/// Published position changes smaller than this are not worth a new state.
const POSITION_PUBLISH_STEP_MS: u64 = 500;

// ------------------------------------------------------------ extras ----

/// The engine operations a playlist needs on top of [`Playback`]: position,
/// seeking, pausing and the "track ended" signal. Reached through
/// [`Playback::music`]; an output that does not offer it (the plain mock)
/// still plays playlists, only without position, seek and pause.
pub trait MusicPlayback {
    /// Position of the sound's current track, `None` when it has no track.
    fn position_ms(&self, sound: SoundId) -> Option<u64>;
    /// Length of the sound's track, `None` when unknown.
    fn duration_ms(&self, sound: SoundId) -> Option<u64>;
    /// Jump to a position.
    fn seek_ms(&self, sound: SoundId, ms: u64) -> PlaybackResult;
    /// Pause, keeping the position.
    fn pause(&self, sound: SoundId) -> PlaybackResult;
    /// Continue after [`pause`](Self::pause).
    fn resume(&self, sound: SoundId) -> PlaybackResult;
    /// Title / artist / album tags (`None` when the sound keeps none).
    fn meta(&self, sound: SoundId) -> Option<TrackMeta>;
    /// Sounds whose track played to its end since the last call.
    fn take_finished(&self) -> Vec<SoundId>;
}

impl MusicPlayback for AudioEngine {
    fn position_ms(&self, sound: SoundId) -> Option<u64> {
        let track = self.tracks_of(sound).into_iter().next()?;
        self.track_position_ms(track).ok()
    }

    fn duration_ms(&self, sound: SoundId) -> Option<u64> {
        if let Some(track) = self.tracks_of(sound).into_iter().next() {
            if let Ok(Some(ms)) = self.track_duration_ms(track) {
                return Some(ms);
            }
        }
        // No track (yet): the length from the header / the decoded audio.
        let (secs, _, _) = self.sound_info(sound).ok()?;
        (secs > 0.0).then(|| (f64::from(secs) * 1000.0) as u64)
    }

    fn seek_ms(&self, sound: SoundId, ms: u64) -> PlaybackResult {
        for track in self.tracks_of(sound) {
            self.seek_track_ms(track, ms)?;
        }
        Ok(())
    }

    fn pause(&self, sound: SoundId) -> PlaybackResult {
        for track in self.tracks_of(sound) {
            self.pause_track_fade(track, 0, MixerCurve::EqualPower)?;
        }
        Ok(())
    }

    fn resume(&self, sound: SoundId) -> PlaybackResult {
        for track in self.tracks_of(sound) {
            self.resume_track_fade(track, 0, MixerCurve::EqualPower)?;
        }
        Ok(())
    }

    fn meta(&self, sound: SoundId) -> Option<TrackMeta> {
        self.sound_meta(sound)
    }

    fn take_finished(&self) -> Vec<SoundId> {
        let events = self.stream_events();
        let tracks = self.inner.active_tracks.read();
        events
            .try_iter()
            .filter_map(|event| match event {
                crate::audio::StreamEvent::TrackFinished { track_id } => {
                    tracks.get(&track_id).copied()
                }
            })
            .collect()
    }
}

impl AudioEngine {
    fn tracks_of(&self, sound: SoundId) -> Vec<uuid::Uuid> {
        self.inner
            .active_tracks
            .read()
            .iter()
            .filter_map(|(track, owner)| (*owner == sound).then_some(*track))
            .collect()
    }
}

// ------------------------------------------------------------- types ----

/// What the player is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistStatus {
    /// Nothing plays (never started, stopped, or every track failed).
    #[default]
    Stopped,
    Playing,
    Paused,
    /// The last track ended and repeat is off.
    Finished,
}

/// Snapshot of the music layer for display (console, `/ws/control`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistState {
    /// Library id of the playlist, `None` before the first start.
    pub playlist: Option<String>,
    /// Display name of the playlist.
    pub name: Option<String>,
    pub status: PlaylistStatus,
    /// Number of items in the playlist.
    pub track_count: usize,
    /// Index of the current track in the playlist's item list.
    pub track_index: Option<usize>,
    /// Library id of the current track (the fallback label when it has no
    /// tags: the sound's name).
    pub track: Option<String>,
    /// Tags of the current track.
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub crossfade_ms: u32,
    /// Tracks that were skipped or why nothing plays, for the user.
    pub error: Option<String>,
}

impl PlaylistState {
    /// "Artist - Title" from the tags, `fallback` (the sound's name) when the
    /// file has none.
    pub fn label(&self, fallback: &str) -> String {
        track_label(self.title.as_deref(), self.artist.as_deref(), fallback)
    }
}

/// "Artist - Title", "Title" or `fallback`.
pub fn track_label(title: Option<&str>, artist: Option<&str>, fallback: &str) -> String {
    match (title, artist) {
        (Some(title), Some(artist)) => format!("{artist} - {title}"),
        (Some(title), None) => title.to_string(),
        _ => fallback.to_string(),
    }
}

/// Everything the music layer can be told to do.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaylistCommand {
    /// Start (or restart) a playlist by library id.
    Start(String),
    /// Fade out and stop.
    Stop,
    Next,
    Prev,
    /// Jump to a position in the current track.
    Seek(u64),
    Pause,
    Resume,
    /// Pause when playing, resume when paused.
    TogglePause,
    SetShuffle(bool),
    SetRepeat(RepeatMode),
}

/// Why a playlist operation did not happen.
#[derive(Debug, Error)]
pub enum PlaylistError {
    #[error("no playlist is playing")]
    NothingActive,
    #[error("the playlist has no tracks")]
    Empty,
    #[error("no track of the playlist can be played ({0})")]
    NothingPlayable(String),
    #[error("this output does not support {0}")]
    Unsupported(&'static str),
    #[error("{0}")]
    Playback(#[from] EngineError),
}

impl From<PlaylistError> for DirectorError {
    fn from(error: PlaylistError) -> Self {
        match error {
            PlaylistError::Playback(e) => DirectorError::Playback(e),
            other => DirectorError::Invalid(other.to_string()),
        }
    }
}

// --------------------------------------------------------------- rng ----

/// xorshift64*: enough for shuffling and needs no dependency.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Never zero (xorshift would stay there).
        Self(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }

    fn from_entropy() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let id = uuid::Uuid::new_v4().as_u128() as u64;
        Self::new(nanos ^ id)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-enough value in `0..n` (`n > 0`).
    fn below(&mut self, n: usize) -> usize {
        ((self.next_u64() >> 11) % n as u64) as usize
    }
}

// ------------------------------------------------------------ player ----

/// The track that plays now.
#[derive(Debug, Clone)]
struct Current {
    sound: SoundId,
    library_id: String,
    meta: TrackMeta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Forward,
    Backward,
}

/// See the module docs.
#[derive(Debug)]
pub struct PlaylistPlayer {
    /// The playlist as the library last gave it (to tell library edits from
    /// the runtime overrides in `cfg`).
    base: PlaylistConfig,
    /// The effective settings: `base` plus shuffle / repeat changed at runtime.
    cfg: PlaylistConfig,
    /// A playlist was started at least once.
    loaded: bool,
    /// Item indices in play order for the current cycle.
    queue: Vec<usize>,
    /// Position in `queue` of the current track (`None`: before the first).
    pos: Option<usize>,
    current: Option<Current>,
    status: PlaylistStatus,
    rng: Rng,
    state: PlaylistState,
    /// A library edit waiting for the next tick (needs the playback).
    pending: Option<PlaylistConfig>,
    /// When the early-transition point of the current track is reached.
    deadline: Option<Instant>,
    error: Option<String>,
}

impl Default for PlaylistPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaylistPlayer {
    /// A player with nothing loaded and a random shuffle seed.
    pub fn new() -> Self {
        Self::with_rng(Rng::from_entropy())
    }

    /// A player whose shuffle order is reproducible.
    pub fn with_seed(seed: u64) -> Self {
        Self::with_rng(Rng::new(seed))
    }

    fn with_rng(rng: Rng) -> Self {
        Self {
            base: PlaylistConfig::default(),
            cfg: PlaylistConfig::default(),
            loaded: false,
            queue: Vec::new(),
            pos: None,
            current: None,
            status: PlaylistStatus::Stopped,
            rng,
            state: PlaylistState::default(),
            pending: None,
            deadline: None,
            error: None,
        }
    }

    /// Reseed the shuffle.
    pub fn reseed(&mut self, seed: u64) {
        self.rng = Rng::new(seed);
    }

    /// Library id of the playlist that was started last.
    pub fn playlist_id(&self) -> Option<&str> {
        self.loaded.then_some(self.cfg.id.as_str())
    }

    pub fn status(&self) -> PlaylistStatus {
        self.status
    }

    /// Playing or paused.
    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            PlaylistStatus::Playing | PlaylistStatus::Paused
        )
    }

    /// The sound of the current track.
    pub fn current_sound(&self) -> Option<SoundId> {
        self.current.as_ref().map(|c| c.sound)
    }

    /// The effective settings of the loaded playlist.
    pub fn config(&self) -> &PlaylistConfig {
        &self.cfg
    }

    /// The latest snapshot (refreshed by every command and tick).
    pub fn state(&self) -> &PlaylistState {
        &self.state
    }

    /// When [`tick`](Self::tick) next has to run for a transition (the early
    /// crossfade point of the current track).
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    // ---------------------------------------------------- start / stop ----

    /// Start `config` from its beginning (first track, or a random one with
    /// shuffle). A playlist that plays now is replaced: its track fades out
    /// while the new one fades in over `config.crossfade_ms`, or over
    /// `min_fade_ms` when that is longer.
    pub fn start<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        catalog: &[CatalogSound],
        config: PlaylistConfig,
        min_fade_ms: u32,
    ) -> Result<(), PlaylistError> {
        let fade = config.crossfade_ms.max(min_fade_ms);
        self.pending = None;
        self.base = config.clone();
        self.cfg = config;
        self.loaded = true;
        self.error = None;
        self.queue = self.new_cycle(None);
        self.pos = None;
        let result = if self.queue.is_empty() {
            self.stop_current(pb, fade);
            self.status = PlaylistStatus::Stopped;
            self.error = Some("the playlist is empty".to_string());
            Err(PlaylistError::Empty)
        } else {
            self.play_sequence(pb, catalog, Some(0), Dir::Forward, fade)
        };
        self.refresh_state(pb);
        result
    }

    /// Fade out and stop. The playlist stays loaded.
    pub fn stop<P: Playback + ?Sized>(&mut self, pb: &P, fade_ms: u32) {
        self.stop_current(pb, fade_ms);
        self.halt();
        self.refresh_state(pb);
    }

    /// Forget the current track without touching the playback (the caller
    /// stopped everything itself).
    pub fn halt(&mut self) {
        self.current = None;
        self.deadline = None;
        self.status = PlaylistStatus::Stopped;
        self.state.status = PlaylistStatus::Stopped;
        self.state.track = None;
        self.state.track_index = None;
        self.state.title = None;
        self.state.artist = None;
        self.state.album = None;
        self.state.position_ms = 0;
        self.state.duration_ms = None;
    }

    // ----------------------------------------------------- navigation ----

    /// Skip to the next track (crossfaded). At the end of a playlist with
    /// repeat off it stops.
    pub fn next<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        catalog: &[CatalogSound],
    ) -> Result<(), PlaylistError> {
        self.apply_pending(pb);
        self.require_active()?;
        let target = self.next_after(self.pos);
        let result = self.play_sequence(pb, catalog, target, Dir::Forward, self.cfg.crossfade_ms);
        self.refresh_state(pb);
        result
    }

    /// Go back: restart the current track when it played for more than
    /// [`RESTART_THRESHOLD_MS`] (or there is nothing before it), else play the
    /// previous track (crossfaded).
    pub fn prev<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        catalog: &[CatalogSound],
    ) -> Result<(), PlaylistError> {
        self.apply_pending(pb);
        self.require_active()?;
        let sound = self.current.as_ref().map(|c| c.sound);
        let played = sound
            .and_then(|s| pb.music().and_then(|m| m.position_ms(s)))
            .unwrap_or(0);
        let target = self.prev_pos();
        let result = match target {
            Some(target) if played <= RESTART_THRESHOLD_MS => self.play_sequence(
                pb,
                catalog,
                Some(target),
                Dir::Backward,
                self.cfg.crossfade_ms,
            ),
            _ => self.restart_current(pb, catalog),
        };
        self.refresh_state(pb);
        result
    }

    /// Jump to `ms` in the current track (clamped to its end).
    pub fn seek<P: Playback + ?Sized>(&mut self, pb: &P, ms: u64) -> Result<(), PlaylistError> {
        self.require_active()?;
        let sound = self.current_sound().ok_or(PlaylistError::NothingActive)?;
        let music = pb.music().ok_or(PlaylistError::Unsupported("seeking"))?;
        let ms = match music.duration_ms(sound) {
            Some(duration) if duration > 0 => ms.min(duration.saturating_sub(1)),
            _ => ms,
        };
        music.seek_ms(sound, ms)?;
        self.refresh_state(pb);
        Ok(())
    }

    /// Pause the current track (keeps the position).
    pub fn pause<P: Playback + ?Sized>(&mut self, pb: &P) -> Result<(), PlaylistError> {
        if self.status != PlaylistStatus::Playing {
            return if self.status == PlaylistStatus::Paused {
                Ok(())
            } else {
                Err(PlaylistError::NothingActive)
            };
        }
        let sound = self.current_sound().ok_or(PlaylistError::NothingActive)?;
        let music = pb.music().ok_or(PlaylistError::Unsupported("pausing"))?;
        music.pause(sound)?;
        self.status = PlaylistStatus::Paused;
        self.deadline = None;
        self.refresh_state(pb);
        Ok(())
    }

    /// Continue after [`pause`](Self::pause).
    pub fn resume<P: Playback + ?Sized>(&mut self, pb: &P) -> Result<(), PlaylistError> {
        if self.status != PlaylistStatus::Paused {
            return if self.status == PlaylistStatus::Playing {
                Ok(())
            } else {
                Err(PlaylistError::NothingActive)
            };
        }
        let sound = self.current_sound().ok_or(PlaylistError::NothingActive)?;
        let music = pb.music().ok_or(PlaylistError::Unsupported("pausing"))?;
        music.resume(sound)?;
        self.status = PlaylistStatus::Playing;
        self.refresh_state(pb);
        Ok(())
    }

    /// Pause when playing, resume when paused.
    pub fn toggle_pause<P: Playback + ?Sized>(&mut self, pb: &P) -> Result<(), PlaylistError> {
        match self.status {
            PlaylistStatus::Paused => self.resume(pb),
            _ => self.pause(pb),
        }
    }

    /// Turn shuffle on or off for the rest of the run. The current track
    /// stays; with shuffle the remaining order is drawn anew (the track that
    /// plays counts as played), without it the list continues in order from
    /// the current track.
    pub fn set_shuffle(&mut self, shuffle: bool) {
        self.cfg.shuffle = shuffle;
        self.state.shuffle = shuffle;
        if self.loaded {
            let item = self.pos.and_then(|p| self.queue.get(p)).copied();
            self.rebuild_queue(item);
        }
    }

    /// Set what happens at the end of the list. Takes effect at once for the
    /// track that plays (`One` loops it).
    pub fn set_repeat<P: Playback + ?Sized>(&mut self, pb: &P, repeat: RepeatMode) {
        self.cfg.repeat = repeat;
        if let Some(current) = &self.current {
            if let Err(e) = pb.set_looped(current.sound, repeat == RepeatMode::One) {
                warn!("Playlist: cannot change repeat of {}: {e}", current.sound);
            }
        }
        self.refresh_state(pb);
    }

    // -------------------------------------------------------- library ----

    /// The library edited the playlist that is loaded: apply it at the next
    /// tick. Only what differs from the version the player already knows is
    /// applied, so shuffle / repeat changed at runtime survive a re-sync of
    /// an unchanged library.
    pub fn queue_update(&mut self, config: PlaylistConfig) {
        if self.loaded && config != self.base {
            self.pending = Some(config);
        }
    }

    fn apply_pending<P: Playback + ?Sized>(&mut self, pb: &P) {
        let Some(new) = self.pending.take() else {
            return;
        };
        let old = std::mem::replace(&mut self.base, new.clone());
        self.cfg.name = new.name.clone();
        self.cfg.bus = new.bus;
        self.cfg.crossfade_ms = new.crossfade_ms;
        if old.repeat != new.repeat {
            self.set_repeat(pb, new.repeat);
        }
        if old.shuffle != new.shuffle {
            self.cfg.shuffle = new.shuffle;
        }
        if old.items != new.items {
            let current_id = self.current.as_ref().map(|c| c.library_id.clone());
            self.cfg.items = new.items;
            let item = current_id.and_then(|id| self.cfg.items.iter().position(|i| *i == id));
            self.rebuild_queue(item);
        } else if old.shuffle != new.shuffle {
            let item = self.pos.and_then(|p| self.queue.get(p)).copied();
            self.rebuild_queue(item);
        }
    }

    // ------------------------------------------------------------ tick ----

    /// Drive the player: start the next track when the current one reached its
    /// crossfade point or ended. Returns `true` when a track change happened.
    /// `now` anchors [`deadline`](Self::deadline).
    pub fn tick<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        catalog: &[CatalogSound],
        now: Instant,
    ) -> bool {
        self.apply_pending(pb);
        let finished = pb.music().map(|m| m.take_finished()).unwrap_or_default();
        self.deadline = None;
        let mut changed = false;
        if self.status == PlaylistStatus::Playing {
            if let Some(sound) = self.current_sound() {
                changed = self.drive(pb, catalog, now, sound, &finished);
            }
        }
        self.refresh_state(pb);
        changed
    }

    fn drive<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        catalog: &[CatalogSound],
        now: Instant,
        sound: SoundId,
        finished: &[SoundId],
    ) -> bool {
        let music = pb.music();
        let position = music.and_then(|m| m.position_ms(sound));
        let duration = music.and_then(|m| m.duration_ms(sound)).filter(|d| *d > 0);

        if !pb.is_playing(sound) {
            // Ended, or stopped by somebody else.
            let natural = finished.contains(&sound)
                || match (position, duration) {
                    (Some(p), Some(d)) => p + END_TOLERANCE_MS >= d,
                    (Some(_), None) => true,
                    // The track is gone: an ended one stays registered until
                    // the next start prunes it, a stopped one does not. Without
                    // position support there is nothing to tell them apart.
                    (None, _) => music.is_none(),
                };
            if !natural {
                self.halt();
                return false;
            }
            let target = if self.cfg.repeat == RepeatMode::One {
                self.pos
            } else {
                self.next_after(self.pos)
            };
            let _ = self.play_sequence(pb, catalog, target, Dir::Forward, 0);
            return true;
        }

        let crossfade = u64::from(self.cfg.crossfade_ms);
        if crossfade == 0 || self.cfg.repeat == RepeatMode::One || !self.has_next() {
            return false;
        }
        let (Some(position), Some(duration)) = (position, duration) else {
            return false;
        };
        let overlap = crossfade.min(duration / 2);
        let trigger = duration.saturating_sub(overlap);
        if position < trigger {
            self.deadline = Some(now + Duration::from_millis((trigger - position).max(10)));
            return false;
        }
        // Whatever is left of the track is the overlap (a late tick shortens
        // it, so the fade still ends with the track).
        let fade = duration.saturating_sub(position).min(overlap) as u32;
        let target = self.next_after(self.pos);
        let _ = self.play_sequence(pb, catalog, target, Dir::Forward, fade);
        true
    }

    // --------------------------------------------------------- private ----

    fn require_active(&self) -> Result<(), PlaylistError> {
        if self.is_active() {
            Ok(())
        } else {
            Err(PlaylistError::NothingActive)
        }
    }

    fn has_next(&self) -> bool {
        self.cfg.repeat != RepeatMode::Off || self.pos.is_none_or(|p| p + 1 < self.queue.len())
    }

    /// Play order of one cycle. With shuffle, a permutation that does not
    /// start with the sound of `avoid_first` (when something else exists).
    fn new_cycle(&mut self, avoid_first: Option<&str>) -> Vec<usize> {
        let n = self.cfg.items.len();
        let mut queue: Vec<usize> = (0..n).collect();
        if !self.cfg.shuffle {
            return queue;
        }
        for i in (1..n).rev() {
            let j = self.rng.below(i + 1);
            queue.swap(i, j);
        }
        if let Some(avoid) = avoid_first {
            if n > 1 && self.cfg.items[queue[0]] == avoid {
                let others: Vec<usize> = (1..n)
                    .filter(|&j| self.cfg.items[queue[j]] != avoid)
                    .collect();
                if !others.is_empty() {
                    let j = others[self.rng.below(others.len())];
                    queue.swap(0, j);
                }
            }
        }
        queue
    }

    /// Rebuild the queue around the current track (`item`).
    fn rebuild_queue(&mut self, item: Option<usize>) {
        if self.cfg.shuffle {
            self.queue = self.new_cycle(None);
            match item.and_then(|i| self.queue.iter().position(|q| *q == i)) {
                Some(at) => {
                    self.queue.swap(0, at);
                    self.pos = Some(0);
                }
                None => self.pos = None,
            }
        } else {
            self.queue = (0..self.cfg.items.len()).collect();
            self.pos = item.filter(|i| *i < self.queue.len());
        }
    }

    /// Queue position after `pos`, starting a new cycle at the end (`None`
    /// with repeat off).
    fn next_after(&mut self, pos: Option<usize>) -> Option<usize> {
        let n = self.queue.len();
        match pos {
            _ if n == 0 => None,
            None => Some(0),
            Some(p) if p + 1 < n => Some(p + 1),
            Some(p) => {
                if self.cfg.repeat == RepeatMode::Off {
                    return None;
                }
                let last = self.cfg.items.get(self.queue[p]).cloned();
                self.queue = self.new_cycle(last.as_deref());
                Some(0)
            }
        }
    }

    /// Queue position before the current one: the list wraps around only in
    /// order mode with repeat on.
    fn prev_pos(&self) -> Option<usize> {
        let n = self.queue.len();
        match self.pos {
            Some(p) if p > 0 => Some(p - 1),
            Some(_) if !self.cfg.shuffle && self.cfg.repeat != RepeatMode::Off && n > 1 => {
                Some(n - 1)
            }
            _ => None,
        }
    }

    fn step(&mut self, pos: usize, dir: Dir) -> Option<usize> {
        match dir {
            Dir::Forward => self.next_after(Some(pos)),
            Dir::Backward => {
                let n = self.queue.len();
                if pos > 0 {
                    Some(pos - 1)
                } else if !self.cfg.shuffle && self.cfg.repeat != RepeatMode::Off && n > 1 {
                    Some(n - 1)
                } else {
                    None
                }
            }
        }
    }

    fn restart_current<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        catalog: &[CatalogSound],
    ) -> Result<(), PlaylistError> {
        if let (Some(sound), Some(music)) = (self.current_sound(), pb.music()) {
            if music.seek_ms(sound, 0).is_ok() {
                return Ok(());
            }
        }
        let pos = self.pos;
        self.play_sequence(pb, catalog, pos, Dir::Forward, 0)
    }

    fn fade_spec(ms: u32) -> FadeSpec {
        FadeSpec::new(ms, FadeCurve::EqualPower)
    }

    fn stop_current<P: Playback + ?Sized>(&mut self, pb: &P, fade_ms: u32) {
        if let Some(old) = self.current.take() {
            if let Err(e) = pb.stop(old.sound, Self::fade_spec(fade_ms)) {
                warn!("Playlist: cannot stop {}: {e}", old.sound);
            }
        }
    }

    /// Start the first playable track at or after `first` (walking in `dir`,
    /// at most one lap), crossfading from the current track. `first == None`
    /// is "the end of the list": everything stops.
    fn play_sequence<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        catalog: &[CatalogSound],
        first: Option<usize>,
        dir: Dir,
        fade_ms: u32,
    ) -> Result<(), PlaylistError> {
        let old = self.current.take();
        let mut old_stopped = false;
        let mut skipped: Vec<String> = Vec::new();
        let mut at = first;
        let mut tried = 0;
        let laps = self.queue.len();
        while let Some(queue_pos) = at {
            if tried >= laps {
                break;
            }
            tried += 1;
            let item = self.queue[queue_pos];
            match self.try_play(pb, catalog, item, fade_ms, old.as_ref(), &mut old_stopped) {
                Ok(current) => {
                    if let (Some(old), false) = (&old, old_stopped) {
                        if let Err(e) = pb.stop(old.sound, Self::fade_spec(fade_ms)) {
                            warn!("Playlist: cannot fade out {}: {e}", old.sound);
                        }
                    }
                    self.current = Some(current);
                    self.pos = Some(queue_pos);
                    self.status = PlaylistStatus::Playing;
                    self.error =
                        (!skipped.is_empty()).then(|| format!("skipped: {}", skipped.join("; ")));
                    return Ok(());
                }
                Err(reason) => {
                    warn!("Playlist {}: skipping a track: {reason}", self.cfg.name);
                    skipped.push(reason);
                    self.pos = Some(queue_pos);
                    at = self.step(queue_pos, dir);
                }
            }
        }
        // Nothing (more) to play.
        if let (Some(old), false) = (&old, old_stopped) {
            let _ = pb.stop(old.sound, Self::fade_spec(fade_ms));
        }
        self.deadline = None;
        if skipped.is_empty() {
            self.status = PlaylistStatus::Finished;
            self.error = None;
            self.current = None;
            Ok(())
        } else {
            self.status = PlaylistStatus::Stopped;
            let reason = skipped.join("; ");
            self.error = Some(format!("nothing to play: {reason}"));
            self.current = None;
            Err(PlaylistError::NothingPlayable(reason))
        }
    }

    fn try_play<P: Playback + ?Sized>(
        &self,
        pb: &P,
        catalog: &[CatalogSound],
        item: usize,
        fade_ms: u32,
        old: Option<&Current>,
        old_stopped: &mut bool,
    ) -> Result<Current, String> {
        let id = &self.cfg.items[item];
        let entry = catalog
            .iter()
            .find(|e| &e.id == id)
            .ok_or_else(|| format!("{id}: not in the library"))?;
        pb.set_bus(entry.sound, self.cfg.bus)
            .map_err(|e| format!("{id}: {e}"))?;
        // The same sound again (a one-track playlist, a track listed twice):
        // its old track has to be faded out first, or the stop would take
        // the new one too.
        if let Some(old) = old {
            if old.sound == entry.sound && !*old_stopped {
                let _ = pb.stop(old.sound, Self::fade_spec(fade_ms));
                *old_stopped = true;
            }
        }
        let looped = self.cfg.repeat == RepeatMode::One;
        pb.play(
            entry.sound,
            looped,
            Some(entry.level(entry.volume)),
            Self::fade_spec(fade_ms),
        )
        .map_err(|e| format!("{id}: {e}"))?;
        Ok(Current {
            sound: entry.sound,
            library_id: entry.id.clone(),
            meta: pb
                .music()
                .and_then(|m| m.meta(entry.sound))
                .unwrap_or_default(),
        })
    }

    fn refresh_state<P: Playback + ?Sized>(&mut self, pb: &P) {
        let music = pb.music();
        let current = self.current.as_ref();
        let (position, duration) = match (current, music) {
            (Some(c), Some(m)) => (m.position_ms(c.sound), m.duration_ms(c.sound)),
            _ => (None, None),
        };
        self.state = PlaylistState {
            playlist: self.loaded.then(|| self.cfg.id.clone()),
            name: self.loaded.then(|| self.cfg.name.clone()),
            status: self.status,
            track_count: self.cfg.items.len(),
            track_index: current
                .and(self.pos)
                .and_then(|p| self.queue.get(p))
                .copied(),
            track: current.map(|c| c.library_id.clone()),
            title: current.and_then(|c| c.meta.title.clone()),
            artist: current.and_then(|c| c.meta.artist.clone()),
            album: current.and_then(|c| c.meta.album.clone()),
            position_ms: position.unwrap_or(0),
            duration_ms: duration.filter(|d| *d > 0),
            shuffle: self.cfg.shuffle,
            repeat: self.cfg.repeat,
            crossfade_ms: self.cfg.crossfade_ms,
            error: self.error.clone(),
        };
    }
}

// ---------------------------------------------------------- the layer ----

/// What the [`Director`] keeps for music.
pub struct MusicLayer {
    pub(crate) player: PlaylistPlayer,
    pub(crate) playlists: Vec<PlaylistConfig>,
    /// The playlist that plays was started by a scene (a scene without a
    /// playlist then stops it; one started by hand is left alone).
    pub(crate) from_scene: bool,
    tx: watch::Sender<PlaylistState>,
}

impl Default for MusicLayer {
    fn default() -> Self {
        let (tx, _) = watch::channel(PlaylistState::default());
        Self {
            player: PlaylistPlayer::new(),
            playlists: Vec::new(),
            from_scene: false,
            tx,
        }
    }
}

fn same_for_display(old: &PlaylistState, new: &PlaylistState) -> bool {
    let mut compare = new.clone();
    if old.position_ms.abs_diff(new.position_ms) < POSITION_PUBLISH_STEP_MS {
        compare.position_ms = old.position_ms;
    }
    *old == compare
}

// ------------------------------------------------------ Director side ----

impl Director {
    // ------------------------------------------------------- state ----

    /// The music layer as it is now (position refreshed on every tick).
    pub fn playlist_state(&self) -> PlaylistState {
        self.music.player.state().clone()
    }

    /// Follow the music layer: the receiver wakes when the track, status or
    /// settings change, and when the position moved by half a second.
    pub fn subscribe_playlist(&self) -> watch::Receiver<PlaylistState> {
        self.music.tx.subscribe()
    }

    /// The playlists the Director knows.
    pub fn playlists(&self) -> &[PlaylistConfig] {
        &self.music.playlists
    }

    /// Replace the playlists (the library's, whenever it changed). A playlist
    /// that plays picks up the edit at the next tick.
    pub fn set_playlists(&mut self, playlists: Vec<PlaylistConfig>) {
        if let Some(id) = self.music.player.playlist_id() {
            if let Some(updated) = playlists.iter().find(|p| p.id == id) {
                self.music.player.queue_update(updated.clone());
            }
        }
        self.music.playlists = playlists;
    }

    /// Make the shuffle order reproducible (tests).
    pub fn seed_music(&mut self, seed: u64) {
        self.music.player.reseed(seed);
    }

    // ----------------------------------------------------- commands ----

    /// Run a [`PlaylistCommand`].
    pub fn playlist_command<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        command: PlaylistCommand,
    ) -> Result<(), DirectorError> {
        let result = self.run_playlist_command(pb, command);
        self.publish_music();
        result
    }

    fn run_playlist_command<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        command: PlaylistCommand,
    ) -> Result<(), DirectorError> {
        let player = &mut self.music.player;
        let catalog = &self.catalog;
        match command {
            PlaylistCommand::Start(id) => {
                let config = self
                    .music
                    .playlists
                    .iter()
                    .find(|p| p.id == id)
                    .cloned()
                    .ok_or_else(|| DirectorError::Invalid(format!("no playlist with id {id}")))?;
                self.music.from_scene = false;
                player.start(pb, catalog, config, 0)?;
            }
            PlaylistCommand::Stop => {
                let fade = player.config().crossfade_ms;
                player.stop(pb, fade);
            }
            PlaylistCommand::Next => player.next(pb, catalog)?,
            PlaylistCommand::Prev => player.prev(pb, catalog)?,
            PlaylistCommand::Seek(ms) => player.seek(pb, ms)?,
            PlaylistCommand::Pause => player.pause(pb)?,
            PlaylistCommand::Resume => player.resume(pb)?,
            PlaylistCommand::TogglePause => player.toggle_pause(pb)?,
            PlaylistCommand::SetShuffle(on) => player.set_shuffle(on),
            PlaylistCommand::SetRepeat(mode) => player.set_repeat(pb, mode),
        }
        Ok(())
    }

    /// Start a playlist by library id.
    pub fn start_playlist<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        id: &str,
    ) -> Result<(), DirectorError> {
        self.playlist_command(pb, PlaylistCommand::Start(id.to_string()))
    }

    /// Fade the music out and stop it.
    pub fn stop_playlist<P: Playback + ?Sized>(&mut self, pb: &P) -> Result<(), DirectorError> {
        self.playlist_command(pb, PlaylistCommand::Stop)
    }

    // -------------------------------------------------- scene hooks ----

    /// A scene was activated: bring the music layer in line with its
    /// `playlist`. Called by `activate_scene` after the plan ran.
    pub(crate) fn music_on_scene<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        report: &mut SceneReport,
    ) {
        let Some(scene) = self.active_scene().and_then(|id| self.scene(id)).cloned() else {
            return;
        };
        // The scene's `stop_others` fade-out took the music's track along
        // (a playlist the scene does not own): forget it, do not advance.
        let stopped = self
            .music
            .player
            .current_sound()
            .is_some_and(|sound| report.stopped.contains(&sound));
        if stopped {
            self.music.player.halt();
        }
        match scene.playlist.as_deref().filter(|id| !id.is_empty()) {
            Some(id) => {
                let same =
                    self.music.player.playlist_id() == Some(id) && self.music.player.is_active();
                if same {
                    // Already playing it: neither restarted nor interrupted.
                    self.music.from_scene = true;
                } else if let Some(config) =
                    self.music.playlists.iter().find(|p| p.id == id).cloned()
                {
                    let result =
                        self.music
                            .player
                            .start(pb, &self.catalog, config, scene.fade_in_ms);
                    match result {
                        Ok(()) => self.music.from_scene = true,
                        Err(e) => report.errors.push(format!("playlist {id}: {e}")),
                    }
                } else {
                    report.errors.push(format!("playlist {id}: not found"));
                    self.music_stop_scene_layer(pb, scene.fade_out_ms);
                }
            }
            None => self.music_stop_scene_layer(pb, scene.fade_out_ms),
        }
        self.publish_music();
    }

    /// The scene was stopped: its music goes with it.
    pub(crate) fn music_on_scene_stopped<P: Playback + ?Sized>(&mut self, pb: &P, fade_ms: u32) {
        self.music_stop_scene_layer(pb, fade_ms);
        self.publish_music();
    }

    fn music_stop_scene_layer<P: Playback + ?Sized>(&mut self, pb: &P, fade_ms: u32) {
        if self.music.from_scene && self.music.player.is_active() {
            self.music.player.stop(pb, fade_ms);
        }
        self.music.from_scene = false;
    }

    /// Everything was stopped by the caller: the music layer just forgets.
    pub(crate) fn music_halt(&mut self) {
        self.music.player.halt();
        self.music.from_scene = false;
        self.publish_music();
    }

    /// Whether `sound` is the track of the playlist `scene` wants to keep
    /// playing (so a scene change must not fade it out).
    pub(crate) fn music_reserves(&self, scene: &SceneConfig, sound: SoundId) -> bool {
        let player = &self.music.player;
        player.is_active()
            && player.current_sound() == Some(sound)
            && scene.playlist.is_some()
            && scene.playlist.as_deref() == player.playlist_id()
    }

    /// Time-driven part, called from `tick`. `true` when the track changed.
    pub(crate) fn music_tick<P: Playback + ?Sized>(&mut self, pb: &P, now: Instant) -> bool {
        let changed = self.music.player.tick(pb, &self.catalog, now);
        self.publish_music();
        changed
    }

    /// When the music layer next needs a tick.
    pub(crate) fn music_deadline(&self) -> Option<Instant> {
        self.music.player.deadline()
    }

    fn publish_music(&self) {
        let state = self.music.player.state();
        self.music.tx.send_if_modified(|old| {
            if same_for_display(old, state) {
                return false;
            }
            *old = state.clone();
            true
        });
    }
}

// -------------------------------------------------------------- mock ----

/// A [`Playback`] with positions, durations, tags, pause and "track ended"
/// events on an artificial clock, for the tests. Wraps the Director's
/// [`MockPlayback`](crate::audio::director::mock::MockPlayback) (gains and
/// fades) and adds what a music track needs.
pub mod mock {
    use std::collections::HashMap;

    use parking_lot::Mutex;

    use super::*;
    use crate::audio::director::mock::{MockCall, MockPlayback};
    use crate::config::Bus;

    #[derive(Debug, Clone)]
    struct Song {
        duration_ms: u64,
        meta: TrackMeta,
        position_ms: u64,
        /// A track exists (started and not stopped; an ended one stays).
        has_track: bool,
        active: bool,
        paused: bool,
        looped: bool,
    }

    #[derive(Default)]
    struct State {
        songs: HashMap<SoundId, Song>,
        finished: Vec<SoundId>,
        clock_ms: u64,
        starts: Vec<SoundId>,
    }

    /// See the module docs.
    pub struct MusicMock {
        inner: MockPlayback,
        state: Mutex<State>,
        base: Instant,
        /// Hide [`MusicPlayback`] (the plain-output case).
        no_music: bool,
    }

    impl Default for MusicMock {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MusicMock {
        pub fn new() -> Self {
            Self {
                inner: MockPlayback::new(),
                state: Mutex::new(State::default()),
                base: Instant::now(),
                no_music: false,
            }
        }

        /// An output without position / seek / pause.
        pub fn without_music_support() -> Self {
            Self {
                no_music: true,
                ..Self::new()
            }
        }

        /// Register a track of `duration_ms`.
        pub fn add_track(&self, duration_ms: u64) -> SoundId {
            self.add_track_with_meta(duration_ms, TrackMeta::default())
        }

        /// Register a track with tags.
        pub fn add_track_with_meta(&self, duration_ms: u64, meta: TrackMeta) -> SoundId {
            let id = self.inner.add_sound();
            self.state.lock().songs.insert(
                id,
                Song {
                    duration_ms,
                    meta,
                    position_ms: 0,
                    has_track: false,
                    active: false,
                    paused: false,
                    looped: false,
                },
            );
            id
        }

        /// The wrapped mock (gains, fade-outs, recorded calls).
        pub fn inner(&self) -> &MockPlayback {
            &self.inner
        }

        /// Every call the wrapped mock recorded.
        pub fn calls(&self) -> Vec<MockCall> {
            self.inner.calls()
        }

        /// Make every call that starts this sound fail (a missing file).
        pub fn fail_on(&self, sound: SoundId) {
            self.inner.fail_on(sound);
        }

        /// How many times `play` started this sound.
        pub fn starts_of(&self, sound: SoundId) -> usize {
            self.state
                .lock()
                .starts
                .iter()
                .filter(|s| **s == sound)
                .count()
        }

        /// Every sound started so far, in order.
        pub fn start_log(&self) -> Vec<SoundId> {
            self.state.lock().starts.clone()
        }

        /// The artificial clock.
        pub fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.state.lock().clock_ms)
        }

        /// Let `ms` pass: positions and fades advance, tracks that reach
        /// their end stop and report [`MusicPlayback::take_finished`].
        pub fn advance(&self, ms: u64) {
            self.inner.advance(ms as u32);
            let mut state = self.state.lock();
            state.clock_ms += ms;
            let mut ended = Vec::new();
            for (sound, song) in state.songs.iter_mut() {
                if !song.active || song.paused {
                    continue;
                }
                song.position_ms += ms;
                if song.position_ms >= song.duration_ms {
                    if song.looped {
                        song.position_ms %= song.duration_ms.max(1);
                    } else {
                        song.position_ms = song.duration_ms;
                        song.active = false;
                        ended.push(*sound);
                    }
                }
            }
            for sound in ended {
                let _ = self.inner.stop(sound, FadeSpec::NONE);
                state.finished.push(sound);
            }
        }

        /// The loudest gain of the sound's tracks.
        pub fn gain(&self, sound: SoundId) -> f32 {
            self.inner.gain(sound)
        }

        /// Tracks of the sound that still sound (fading tails included).
        pub fn track_count(&self, sound: SoundId) -> usize {
            self.inner.track_count(sound)
        }

        /// The bus the sound was routed to.
        pub fn bus_of(&self, sound: SoundId) -> Option<Bus> {
            self.inner.bus_of(sound)
        }

        fn with_song<R>(&self, sound: SoundId, f: impl FnOnce(&mut Song) -> R) -> Option<R> {
            self.state.lock().songs.get_mut(&sound).map(f)
        }
    }

    impl MusicPlayback for MusicMock {
        fn position_ms(&self, sound: SoundId) -> Option<u64> {
            self.with_song(sound, |s| s.has_track.then_some(s.position_ms))
                .flatten()
        }

        fn duration_ms(&self, sound: SoundId) -> Option<u64> {
            self.with_song(sound, |s| s.duration_ms)
        }

        fn seek_ms(&self, sound: SoundId, ms: u64) -> PlaybackResult {
            self.with_song(sound, |s| s.position_ms = ms.min(s.duration_ms))
                .ok_or(EngineError::SoundNotFound(sound))
        }

        fn pause(&self, sound: SoundId) -> PlaybackResult {
            self.with_song(sound, |s| s.paused = s.active)
                .ok_or(EngineError::SoundNotFound(sound))
        }

        fn resume(&self, sound: SoundId) -> PlaybackResult {
            self.with_song(sound, |s| s.paused = false)
                .ok_or(EngineError::SoundNotFound(sound))
        }

        fn meta(&self, sound: SoundId) -> Option<TrackMeta> {
            self.with_song(sound, |s| s.meta.clone())
        }

        fn take_finished(&self) -> Vec<SoundId> {
            std::mem::take(&mut self.state.lock().finished)
        }
    }

    impl Playback for MusicMock {
        fn music(&self) -> Option<&dyn MusicPlayback> {
            (!self.no_music).then_some(self as &dyn MusicPlayback)
        }

        fn playing_sounds(&self) -> Vec<SoundId> {
            let paused: Vec<SoundId> = self
                .state
                .lock()
                .songs
                .iter()
                .filter(|(_, s)| s.paused)
                .map(|(id, _)| *id)
                .collect();
            self.inner
                .playing_sounds()
                .into_iter()
                .filter(|s| !paused.contains(s))
                .collect()
        }

        fn is_playing(&self, sound: SoundId) -> bool {
            let paused = self.with_song(sound, |s| s.paused).unwrap_or(false);
            !paused && self.inner.is_playing(sound)
        }

        fn volume(&self, sound: SoundId) -> Option<f32> {
            self.inner.volume(sound)
        }

        fn is_looped(&self, sound: SoundId) -> bool {
            self.inner.is_looped(sound)
        }

        fn set_volume(&self, sound: SoundId, volume: f32) -> PlaybackResult {
            self.inner.set_volume(sound, volume)
        }

        fn set_looped(&self, sound: SoundId, looped: bool) -> PlaybackResult {
            self.inner.set_looped(sound, looped)?;
            self.with_song(sound, |s| s.looped = looped);
            Ok(())
        }

        fn set_pan(&self, sound: SoundId, pan: f32) -> PlaybackResult {
            self.inner.set_pan(sound, pan)
        }

        fn set_bus(&self, sound: SoundId, bus: Bus) -> PlaybackResult {
            self.inner.set_bus(sound, bus)
        }

        fn play(
            &self,
            sound: SoundId,
            looped: bool,
            volume: Option<f32>,
            fade_in: FadeSpec,
        ) -> PlaybackResult {
            self.inner.play(sound, looped, volume, fade_in)?;
            let mut state = self.state.lock();
            state.starts.push(sound);
            if let Some(song) = state.songs.get_mut(&sound) {
                song.position_ms = 0;
                song.has_track = true;
                song.active = true;
                song.paused = false;
                song.looped = looped;
            }
            Ok(())
        }

        fn play_one_shot(&self, sound: SoundId, volume: f32) -> PlaybackResult {
            self.inner.play_one_shot(sound, volume)
        }

        fn stop(&self, sound: SoundId, fade_out: FadeSpec) -> PlaybackResult {
            self.inner.stop(sound, fade_out)?;
            self.with_song(sound, |s| {
                s.active = false;
                s.has_track = false;
                s.paused = false;
            });
            Ok(())
        }

        fn fade_to(&self, sound: SoundId, volume: f32, fade: FadeSpec) -> PlaybackResult {
            self.inner.fade_to(sound, volume, fade)
        }

        fn stop_all(&self, fade_out: FadeSpec) -> PlaybackResult {
            self.inner.stop_all(fade_out)?;
            for song in self.state.lock().songs.values_mut() {
                song.active = false;
                song.has_track = false;
                song.paused = false;
            }
            Ok(())
        }

        fn master_volume(&self) -> f32 {
            self.inner.master_volume()
        }

        fn set_master_volume(&self, volume: f32) -> PlaybackResult {
            self.inner.set_master_volume(volume)
        }
    }
}
