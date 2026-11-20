//! Remote control for `--server-only` mode.
//!
//! Without the console window there used to be no way to load or play
//! anything: the headless server broadcast silence. In that mode the library
//! from `config.toml` is loaded at start ([`Board`]) and a small control page
//! is served at `/control`, with a JSON API under `/api/control/…`.
//!
//! # Access rules (`/control`, `/api/control/*`, `/ws/control`)
//!
//! Players reach the server over the LAN/internet and must never get the GM's
//! controls, so the whole panel sits behind [`GmAuth`]:
//! * a peer on loopback is let in without a token (the panel on this PC);
//! * everyone else needs the game-master token (`[server] gm_token`), sent as
//!   `Authorization: Bearer <token>` or as `?g=<token>` (WebSocket, QR link).
//!   The listener key (`access_token`) never opens the panel, and an empty
//!   `gm_token` disables remote access altogether;
//! * "local" is decided from the socket address, never from `X-Forwarded-For`;
//!   a request that carries reverse-proxy headers counts as remote, so a tunnel
//!   running on this PC does not turn the whole internet into "local". The
//!   `Host` must also be a loopback name, so a relay that adds no header but
//!   keeps the public name is refused too. A plain TCP tunnel (`ssh -R`, bore,
//!   frp, ...) or a proxy that rewrites `Host` to `127.0.0.1` cannot be told
//!   from the GM's own browser: point such tunnels at the listener port only
//!   with a `gm_token` set and never at a no-token panel, or avoid them;
//! * wrong or missing tokens all get the same 401; after
//!   [`GM_MAX_FAILURES`](crate::network::access::GM_MAX_FAILURES) failures in a
//!   minute the address gets 429 + `Retry-After`;
//! * a local request without a token must also send the `X-Bardwave-Control`
//!   header on every REST action. A web page from another site cannot send a
//!   custom header without a CORS preflight, which this server never approves,
//!   so a malicious site open in the GM's browser cannot press buttons. (A
//!   request with the token proves intent by itself and needs no header.) For
//!   the same reason a local `/ws/control` handshake with an `Origin` other
//!   than the page's own host is refused.
//!
//! # `/ws/control` protocol
//!
//! Text frames of JSON, at most [`MAX_WS_MESSAGE`] bytes each.
//!
//! Server to client:
//! * `{"type":"snapshot","rev":N,"state":BoardState}` right after connecting
//!   and after every change (full state; `rev` grows with every change, so a
//!   client may drop anything not newer than what it shows);
//! * `{"type":"ack","req":R,"ok":true}` / `{"type":"error","req":R,"message":"…"}`
//!   for a command (`req` echoes the client's `req`, when it sent one);
//! * `{"type":"pong","req":R}` for a client `ping`.
//!
//! Client to server, one object per frame with a `cmd` and an optional `req`:
//! `ping`, `state`, `play{id}`, `stop{id}`, `volume{id,value}`,
//! `loop{id,on}`, `master{value}`, `stop_all{fade_ms?}`, `scene{id}`,
//! `stop_scene`, `stinger{id}`, `duck{on}`, `bus{bus,value}` and the music
//! layer: `playlist_start{id}` (alias `playlist_play`), `playlist_stop`,
//! `playlist_pause`, `playlist_resume`, `playlist_toggle`, `playlist_next`,
//! `playlist_prev`, `playlist_seek{ms}`, `playlist_shuffle{on}`,
//! `playlist_repeat{mode}` (`off`, `all`, `one`). Anything else is answered
//! with an `error`; the connection stays open.
//!
//! The same playlist commands exist over REST as
//! `POST /api/control/playlist/<start|stop|pause|resume|toggle|next|prev|seek|shuffle|repeat>`
//! (`?id=`, `?ms=`, `?on=`, `?mode=` as the command needs).
//!
//! # The page and its data
//!
//! The page shell (`/control`, `/control.js`, `/control.css`) is static code
//! with no secret in it and is served to anyone, so a reload of the page on a
//! phone (whose address no longer carries the token) still opens. Everything
//! that reads or changes the board (`/api/control/*`, `/ws/control`) needs
//! the game-master token (or a local peer), as described above.
//!
//! The audio engine lives on the main thread (its CPAL stream is `!Send`), so
//! the HTTP handlers only queue [`ControlCommand`]s and the main thread runs
//! them through [`Board::handle`]. The board publishes every state change into
//! a [`ControlFeed`], which is what `/ws/control` clients follow.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot, watch};

use crate::audio::director::{Director, DirectorState, Playback};
use crate::audio::director_audio::{
    bus_name, parse_bus, DuckMode, DuckTarget, GainUpdate, MixControl,
};
use crate::audio::playlist::{PlaylistCommand, PlaylistState};
use crate::audio::{AudioEngine, CatalogSound, SoundId};
use crate::config::{AudioConfig, Bus, LibraryConfig, RepeatMode};
use crate::network::access::{
    GmAccess, GmAuth, GmDenied, GmGuard, CLOSE_GM_TOKEN_REVOKED, GM_PARAM,
};

/// Header every local control action must carry (see the module docs).
pub const CONTROL_HEADER: &str = "x-bardwave-control";

/// Largest `/ws/control` message accepted from a client, in bytes.
pub const MAX_WS_MESSAGE: usize = 4096;

/// How many `/ws/control` clients may be connected at once.
pub const MAX_WS_CLIENTS: usize = 16;

/// Commands one `/ws/control` client may send per second.
const MAX_WS_COMMANDS_PER_SEC: u32 = 100;

/// How often the server pings an idle `/ws/control` client.
const WS_PING_EVERY: Duration = Duration::from_secs(25);

/// A client that sends nothing (not even a pong) for this long is dropped.
const WS_IDLE_LIMIT: Duration = Duration::from_secs(75);

/// How long a send to a client may take before it is given up on.
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the engine thread may take to answer one command.
const ENGINE_TIMEOUT: Duration = Duration::from_secs(5);

/// One sound on the board, as the control page sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlSound {
    pub id: usize,
    pub name: String,
    pub group: Option<String>,
    pub playing: bool,
    pub looped: bool,
    pub volume: f32,
    /// A stinger (`stinger` command): one-shot over everything.
    #[serde(default)]
    pub stinger: bool,
}

/// One playlist of the library, as the control page sees it (listing only:
/// playback state belongs to the playlist engine).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlPlaylist {
    /// Stable library id of the playlist.
    pub id: String,
    pub name: String,
    /// Number of tracks.
    pub tracks: usize,
}

/// One scene on the board, as the control page sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlScene {
    /// Position in the scene list: what `/api/control/scene/activate?id=` takes.
    pub id: usize,
    /// Stable library id of the scene.
    pub uuid: String,
    pub name: String,
    /// `#rrggbb` button colour, if the scene has one.
    pub color: Option<String>,
    /// Number of sounds in the scene.
    pub sounds: usize,
    /// Whether this is the active scene.
    pub active: bool,
}

/// Volume of every bus, as last set (`bus` command), `0.0..=1.0`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ControlBuses {
    pub music: f32,
    pub ambience: f32,
    pub sfx: f32,
}

impl Default for ControlBuses {
    fn default() -> Self {
        Self {
            music: 1.0,
            ambience: 1.0,
            sfx: 1.0,
        }
    }
}

/// The music layer as the control page sees it: the playlist player's state
/// plus a label for the current track.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlPlaylistState {
    /// Status, current track (`title` / `artist` tags), position and length
    /// (in milliseconds, position rounded down to a second), shuffle, repeat.
    #[serde(flatten)]
    pub state: PlaylistState,
    /// "Artist - Title" from the tags, the sound's name when it has none.
    pub label: Option<String>,
}

/// Board snapshot returned by every call.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BoardState {
    pub sounds: Vec<ControlSound>,
    pub master_volume: f32,
    /// The library's scenes, in order.
    #[serde(default)]
    pub scenes: Vec<ControlScene>,
    /// Index (`ControlScene::id`) of the active scene.
    #[serde(default)]
    pub active_scene: Option<usize>,
    /// The library's playlists, in order.
    #[serde(default)]
    pub playlists: Vec<ControlPlaylist>,
    /// Whether the background buses are ducked right now (`duck` command).
    #[serde(default)]
    pub ducked: bool,
    /// Names (`music`, `ambience`, `sfx`) of the buses that are ducked now.
    #[serde(default)]
    pub ducked_buses: Vec<String>,
    /// Volume of every bus.
    #[serde(default)]
    pub buses: ControlBuses,
    /// The music layer (always present from a board; absent from an older
    /// server, which is how the page tells the two apart).
    #[serde(default)]
    pub playlist: Option<ControlPlaylistState>,
}

/// A state together with its revision, as the feed carries it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Published {
    /// Grows by one on every change of `state`; `0` = nothing published yet.
    pub rev: u64,
    pub state: BoardState,
}

/// Where the board publishes its state for `/ws/control` clients.
///
/// The board lives on the engine thread; the feed is the `Send` window onto it
/// (a `watch` channel, so a slow client only ever sees the latest state).
#[derive(Clone)]
pub struct ControlFeed {
    tx: Arc<watch::Sender<Published>>,
}

impl Default for ControlFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlFeed {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(Published::default());
        Self { tx: Arc::new(tx) }
    }

    /// Publish `state`; subscribers wake only when it differs from the last one.
    pub fn publish(&self, state: BoardState) {
        self.tx.send_if_modified(|current| {
            if current.state == state {
                return false;
            }
            current.rev += 1;
            current.state = state;
            true
        });
    }

    /// Follow the state.
    pub fn subscribe(&self) -> watch::Receiver<Published> {
        self.tx.subscribe()
    }

    /// The latest published state.
    pub fn latest(&self) -> Published {
        self.tx.borrow().clone()
    }
}

/// Answer of `GET /api/control/scenes`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScenesResponse {
    pub scenes: Vec<ControlScene>,
    pub active_scene: Option<usize>,
}

/// What the control API asks the main thread to do.
#[derive(Debug)]
pub enum ControlAction {
    List,
    Play(usize),
    Stop(usize),
    StopAll,
    /// Fade everything out over this many milliseconds.
    StopAllFade(u32),
    SetVolume(usize, f32),
    SetLoop(usize, bool),
    SetMaster(f32),
    /// Switch to the scene at this position of the scene list.
    ActivateScene(usize),
    /// Fade out the active scene.
    StopScene,
    /// Play the sound once over everything (a stinger).
    PlayStinger(usize),
    /// Duck (`true`) or release (`false`) the configured background buses.
    Duck(bool),
    /// A bus volume or ducking request.
    Mix(MixControl),
    /// A command for the music layer (start, pause, seek, ...).
    Playlist(PlaylistCommand),
}

/// One queued action plus where to send the resulting board state.
#[derive(Debug)]
pub struct ControlCommand {
    pub action: ControlAction,
    pub reply: oneshot::Sender<Result<BoardState, String>>,
}

/// The library loaded for headless playback.
///
/// All playback goes through the [`Director`], the same core the console uses,
/// so scenes behave identically in both modes.
pub struct Board {
    sounds: Vec<BoardSound>,
    director: Director,
    playlists: Vec<ControlPlaylist>,
    /// Where state changes are published (see [`Board::attach_feed`]).
    feed: Option<ControlFeed>,
}

struct BoardSound {
    /// Library id (what playlists list their tracks by).
    library_id: String,
    name: String,
    group: Option<String>,
    sound_id: SoundId,
    looped: bool,
    volume: f32,
    stinger: bool,
}

fn display_name(entry: &crate::config::SoundConfigEntry) -> String {
    if entry.name.trim().is_empty() {
        PathBuf::from(&entry.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Sound")
            .to_string()
    } else {
        entry.name.clone()
    }
}

impl Board {
    /// Decode every sound of `[library]` that is still on disk.
    ///
    /// Missing or broken files are skipped with a warning, like the console
    /// does on start, so one bad entry does not take the server down.
    pub fn load(engine: &AudioEngine, library: &LibraryConfig) -> Self {
        let mut loaded = Vec::new();
        for (index, entry) in library.sounds.iter().enumerate() {
            let path = PathBuf::from(&entry.path);
            if !path.exists() {
                tracing::warn!("Library sound missing on disk, skipped: {}", path.display());
                continue;
            }
            match engine.load_sound(&path) {
                Ok(sound_id) => loaded.push((index, sound_id)),
                Err(e) => tracing::warn!("Could not load {}: {e}", path.display()),
            }
        }
        let mut board = Self::assemble(library, &loaded);
        // Push the saved levels into the engine (with the sound's fixed gain).
        for sound in &board.sounds {
            let _ = board
                .director
                .set_sound_volume(engine, sound.sound_id, sound.volume);
        }
        board
    }

    /// Build a board from sounds that are already loaded: `loaded` pairs the
    /// index of an entry of `library.sounds` with the engine id it got. This
    /// is what [`load`](Self::load) uses after decoding, and what tests use
    /// with a mock playback.
    pub fn assemble(library: &LibraryConfig, loaded: &[(usize, SoundId)]) -> Self {
        let mut sounds = Vec::new();
        let mut catalog = Vec::new();
        for &(index, sound_id) in loaded {
            let Some(entry) = library.sounds.get(index) else {
                continue;
            };
            let volume = entry.volume.clamp(0.0, 1.0);
            let group = entry.group.as_ref().and_then(|id| {
                library
                    .groups
                    .iter()
                    .find(|g| &g.id == id)
                    .map(|g| g.name.clone())
            });
            // An entry from a hand-built config may have no id yet.
            let library_id = if entry.id.is_empty() {
                format!("sound-{index}")
            } else {
                entry.id.clone()
            };
            catalog.push(CatalogSound {
                id: library_id.clone(),
                sound: sound_id,
                volume,
                looped: entry.looped,
                gain_db: entry.gain_db,
                bus: entry.bus,
                stinger: entry.oneshot_stinger,
                pan: crate::audio::director_pan::sanitize_pan(entry.pan),
            });
            sounds.push(BoardSound {
                library_id,
                name: display_name(entry),
                group,
                sound_id,
                looped: entry.looped,
                volume,
                stinger: entry.oneshot_stinger,
            });
        }
        let mut director = Director::new();
        director.sync_library(library.scenes.clone(), catalog);
        director.set_playlists(library.playlists.clone());
        let playlists = library
            .playlists
            .iter()
            .map(|p| ControlPlaylist {
                id: p.id.clone(),
                name: p.name.clone(),
                tracks: p.items.len(),
            })
            .collect();
        Self {
            sounds,
            director,
            playlists,
            feed: None,
        }
    }

    /// Publish every state change of this board into `feed`, for
    /// `/ws/control` clients. The current state is published at once.
    pub fn attach_feed<P: Playback + ?Sized>(&mut self, pb: &P, feed: ControlFeed) {
        feed.publish(self.state(pb));
        self.feed = Some(feed);
    }

    fn publish<P: Playback + ?Sized>(&self, pb: &P) -> BoardState {
        let state = self.state(pb);
        if let Some(feed) = &self.feed {
            feed.publish(state.clone());
        }
        state
    }

    /// Take the ducking defaults and the normalization switch from `[audio]`.
    pub fn configure_audio(&mut self, audio: &AudioConfig) {
        self.director.configure_audio(audio);
    }

    /// Start measuring the loudness of every loaded sound that has no gain yet
    /// (a no-op unless `audio.normalize` is on). Returns how many
    /// measurements were started; the results arrive through
    /// [`tick`](Self::tick) and [`take_gain_updates`](Self::take_gain_updates).
    pub fn normalize_missing(&mut self, engine: &AudioEngine) -> usize {
        self.director.normalize_catalog(engine)
    }

    /// Gains measured since the last call, for the caller to store in the
    /// config (see `audio::director_audio::apply_gain_updates`).
    pub fn take_gain_updates(&mut self) -> Vec<GainUpdate> {
        self.director.take_gain_updates()
    }

    /// Number of playable sounds.
    pub fn len(&self) -> usize {
        self.sounds.len()
    }

    /// Whether nothing could be loaded.
    pub fn is_empty(&self) -> bool {
        self.sounds.is_empty()
    }

    /// The control core behind the board.
    pub fn director(&self) -> &Director {
        &self.director
    }

    /// Follow the Director's state (active scene, playing sounds).
    pub fn subscribe(&self) -> watch::Receiver<DirectorState> {
        self.director.subscribe()
    }

    /// Run due delayed starts of the active scene. Call it regularly from the
    /// thread that owns the engine.
    ///
    /// Also publishes the state when something changed on its own (a sound
    /// that ended, a delayed start that fired).
    pub fn tick<P: Playback + ?Sized>(&mut self, pb: &P) {
        self.director.tick(pb);
        if self.feed.is_some() {
            self.publish(pb);
        }
    }

    /// When [`tick`](Self::tick) next has work, if ever.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.director.next_deadline()
    }

    /// Run one action on the engine and report the new board state.
    pub fn handle<P: Playback + ?Sized>(
        &mut self,
        pb: &P,
        action: ControlAction,
    ) -> Result<BoardState, String> {
        let sound = |sounds: &Vec<BoardSound>, id: usize| {
            sounds
                .get(id)
                .map(|s| s.sound_id)
                .ok_or_else(|| format!("no sound #{id}"))
        };
        match action {
            ControlAction::List => {}
            ControlAction::Play(id) => {
                let sound_id = sound(&self.sounds, id)?;
                let looped = self.sounds[id].looped;
                self.director
                    .play_sound(pb, sound_id, looped)
                    .map_err(|e| e.to_string())?;
            }
            ControlAction::Stop(id) => {
                let sound_id = sound(&self.sounds, id)?;
                self.director
                    .stop_sound(pb, sound_id)
                    .map_err(|e| e.to_string())?;
            }
            ControlAction::StopAll => self.director.stop_all(pb).map_err(|e| e.to_string())?,
            ControlAction::StopAllFade(ms) => self
                .director
                .stop_all_with_fade(pb, ms)
                .map_err(|e| e.to_string())?,
            ControlAction::SetVolume(id, value) => {
                let value = value.clamp(0.0, 1.0);
                let sound_id = sound(&self.sounds, id)?;
                self.director
                    .set_sound_volume(pb, sound_id, value)
                    .map_err(|e| e.to_string())?;
                self.sounds[id].volume = value;
            }
            ControlAction::SetLoop(id, on) => {
                let sound_id = sound(&self.sounds, id)?;
                self.director
                    .set_sound_looped(pb, sound_id, on)
                    .map_err(|e| e.to_string())?;
                self.sounds[id].looped = on;
            }
            ControlAction::SetMaster(value) => self
                .director
                .set_master_volume(pb, value.clamp(0.0, 1.0))
                .map_err(|e| e.to_string())?,
            ControlAction::ActivateScene(index) => {
                let id = self
                    .director
                    .scenes()
                    .get(index)
                    .map(|scene| scene.id.clone())
                    .ok_or_else(|| format!("no scene #{index}"))?;
                let report = self
                    .director
                    .activate_scene(pb, &id)
                    .map_err(|e| e.to_string())?;
                for problem in &report.errors {
                    tracing::warn!("Scene {id}: {problem}");
                }
            }
            ControlAction::StopScene => {
                self.director.stop_scene(pb).map_err(|e| e.to_string())?;
            }
            ControlAction::PlayStinger(id) => {
                let sound_id = sound(&self.sounds, id)?;
                self.director
                    .play_stinger(pb, sound_id)
                    .map_err(|e| e.to_string())?;
            }
            ControlAction::Duck(on) => {
                let mode = if on { DuckMode::On } else { DuckMode::Off };
                self.director
                    .run_mix_control(
                        pb,
                        MixControl::Duck {
                            target: DuckTarget::Configured,
                            mode,
                            amount: None,
                            attack_ms: None,
                            release_ms: None,
                        },
                    )
                    .map_err(|e| e.to_string())?;
            }
            ControlAction::Mix(control) => self
                .director
                .run_mix_control(pb, control)
                .map_err(|e| e.to_string())?,
            ControlAction::Playlist(command) => self
                .director
                .playlist_command(pb, command)
                .map_err(|e| e.to_string())?,
        }
        Ok(self.publish(pb))
    }

    fn scenes_view(&self) -> (Vec<ControlScene>, Option<usize>) {
        let active = self.director.active_scene();
        let scenes: Vec<ControlScene> = self
            .director
            .scenes()
            .iter()
            .enumerate()
            .map(|(id, scene)| ControlScene {
                id,
                uuid: scene.id.clone(),
                name: scene.name.clone(),
                color: scene.color.clone(),
                sounds: scene.items.len(),
                active: active == Some(scene.id.as_str()),
            })
            .collect();
        let active_index = scenes.iter().position(|s| s.active);
        (scenes, active_index)
    }

    fn state<P: Playback + ?Sized>(&self, pb: &P) -> BoardState {
        let (scenes, active_scene) = self.scenes_view();
        let ducked_buses: Vec<String> = [Bus::Music, Bus::Ambience, Bus::Sfx]
            .into_iter()
            .filter(|bus| self.director.is_ducked(*bus))
            .map(|bus| bus_name(bus).to_string())
            .collect();
        BoardState {
            sounds: self
                .sounds
                .iter()
                .enumerate()
                .map(|(id, s)| ControlSound {
                    id,
                    name: s.name.clone(),
                    group: s.group.clone(),
                    playing: pb.is_playing(s.sound_id),
                    looped: s.looped,
                    volume: s.volume,
                    stinger: s.stinger,
                })
                .collect(),
            master_volume: pb.master_volume(),
            scenes,
            active_scene,
            playlists: self.playlists.clone(),
            ducked: !ducked_buses.is_empty(),
            ducked_buses,
            buses: ControlBuses {
                music: self.director.bus_volume(Bus::Music),
                ambience: self.director.bus_volume(Bus::Ambience),
                sfx: self.director.bus_volume(Bus::Sfx),
            },
            playlist: Some(self.playlist_view()),
        }
    }

    /// The music layer for the page. The position is rounded down to a second
    /// so a playing track publishes once a second, not on every tick.
    fn playlist_view(&self) -> ControlPlaylistState {
        let mut state = self.director.playlist_state();
        state.position_ms -= state.position_ms % 1000;
        if state.playlist.is_none() {
            // The player's idle defaults settle after its first tick; do not
            // let that read as a change of state.
            state.crossfade_ms = 0;
        }
        let label = state.track.as_ref().map(|track| {
            let fallback = self
                .sounds
                .iter()
                .find(|s| &s.library_id == track)
                .map_or(track.as_str(), |s| s.name.as_str());
            state.label(fallback)
        });
        ControlPlaylistState { state, label }
    }
}

/// Everything the control routes share: the engine's command queue, the access
/// rules, the state feed and the `/ws/control` client budget.
#[derive(Clone)]
pub struct ControlContext {
    commands: Sender<ControlCommand>,
    auth: GmAuth,
    feed: ControlFeed,
    clients: Arc<AtomicUsize>,
    max_clients: usize,
    /// Closes `/ws/control` sessions when the server stops (the server's own
    /// shutdown signal; see `build_router_with_control`).
    shutdown: Option<broadcast::Sender<()>>,
}

impl ControlContext {
    /// A context with the given access rules and state feed.
    pub fn new(commands: Sender<ControlCommand>, auth: GmAuth, feed: ControlFeed) -> Self {
        Self {
            commands,
            auth,
            feed,
            clients: Arc::new(AtomicUsize::new(0)),
            max_clients: MAX_WS_CLIENTS,
            shutdown: None,
        }
    }

    /// The panel as it was before remote access: this computer only.
    pub fn local_only(commands: Sender<ControlCommand>) -> Self {
        Self::new(
            commands,
            GmAuth::new(GmGuard::default()),
            ControlFeed::new(),
        )
    }

    /// Allow at most `max` simultaneous `/ws/control` clients.
    pub fn with_max_clients(mut self, max: usize) -> Self {
        self.max_clients = max;
        self
    }

    /// Close `/ws/control` sessions when `shutdown` fires.
    pub fn with_shutdown(mut self, shutdown: broadcast::Sender<()>) -> Self {
        self.shutdown = Some(shutdown);
        self
    }

    /// The access rules.
    pub fn auth(&self) -> &GmAuth {
        &self.auth
    }

    /// The state feed (hand a clone to [`Board::attach_feed`]).
    pub fn feed(&self) -> &ControlFeed {
        &self.feed
    }

    /// How many `/ws/control` clients are connected right now.
    pub fn client_count(&self) -> usize {
        self.clients.load(Ordering::Relaxed)
    }
}

/// The phone panel's page (`control.html`; its script and styles are served
/// from [`PANEL_ASSETS`]).
pub const CONTROL_HTML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/network/static/control.html"
));

/// Static files of the phone panel, served without authentication (they hold
/// no secrets and are fetched by `<script src>` / `<link href>` tags that
/// cannot carry the token): `(route, content type, body)`.
pub const PANEL_ASSETS: &[(&str, &str, &str)] = &[
    (
        "/control.js",
        "application/javascript; charset=utf-8",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/network/static/control.js"
        )),
    ),
    (
        "/control.css",
        "text/css; charset=utf-8",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/network/static/control.css"
        )),
    ),
];

/// Routes of the control page and API, for this computer only (no remote
/// access). Kept for callers that predate the game-master token.
pub fn router(commands: Sender<ControlCommand>) -> Router {
    routes(ControlContext::local_only(commands))
}

/// Routes of the control page, the API and `/ws/control`, to merge into the
/// server router.
pub fn routes(ctx: ControlContext) -> Router {
    let guarded = Router::new()
        .route("/ws/control", get(control_ws))
        .route("/api/control/state", get(control_state))
        .route("/api/control/play", post(control_play))
        .route("/api/control/stop", post(control_stop))
        .route("/api/control/stop_all", post(control_stop_all))
        .route("/api/control/scenes", get(control_scenes))
        .route("/api/control/scene/activate", post(control_scene_activate))
        .route("/api/control/scene/stop", post(control_scene_stop))
        .route("/api/control/volume", post(control_volume))
        .route("/api/control/loop", post(control_loop))
        .route("/api/control/master", post(control_master))
        .route("/api/control/stinger", post(control_stinger))
        .route("/api/control/duck", post(control_duck))
        .route("/api/control/bus", post(control_bus))
        .route("/api/control/playlist/:verb", post(control_playlist))
        .route_layer(middleware::from_fn_with_state(ctx.clone(), gm_gate))
        // The token can sit in the URL: keep it out of caches and Referer.
        .layer(middleware::map_response(private_headers))
        .with_state(ctx);

    // The shell holds code only; the token never reaches it (see the module
    // docs), so it needs no gate. Its data does.
    let mut assets = Router::new().route("/control", get(control_page));
    for &(path, content_type, body) in PANEL_ASSETS {
        assets = assets.route(
            path,
            get(move || async move { ([(header::CONTENT_TYPE, content_type)], body) }),
        );
    }
    let assets = assets.layer(middleware::map_response(private_headers));
    guarded.merge(assets)
}

async fn private_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

// ----------------------------------------------------------------- access ---

/// Headers a reverse proxy or tunnel adds. A browser on this PC never sends
/// them, so their presence means the loopback peer is a relay, not the client.
const PROXY_HEADERS: &[&str] = &[
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
    "forwarded",
    "cf-connecting-ip",
    "true-client-ip",
    "x-client-ip",
    "via",
    "x-forwarded-port",
    "x-forwarded-server",
    "x-forwarded-scheme",
    "x-original-forwarded-for",
    "x-cluster-client-ip",
    "fastly-client-ip",
    "cdn-loop",
];

fn has_proxy_headers(headers: &HeaderMap) -> bool {
    PROXY_HEADERS.iter().any(|name| headers.contains_key(*name))
}

/// Whether the `Host` header names this machine (`localhost`, `127.0.0.1`,
/// `[::1]`, with or without a port). Under DNS rebinding the browser sends the
/// attacker's name here while the socket peer is loopback, so no-token local
/// access must not be granted for any other name.
fn host_is_loopback(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((ip, tail)) if tail.is_empty() || tail.starts_with(':') => {
                return ip == "::1";
            }
            _ => return false,
        }
    } else {
        host.split_once(':').map_or(host, |(name, _)| name)
    };
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1"
}

/// The token of `Authorization: Bearer <token>`.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(char::is_whitespace)?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
}

/// Percent-decode (`%XX`; `+` stays a plus, tokens are not form data).
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The value of query parameter `name`, if present.
fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| percent_decode(value))
    })
}

/// A browser sends `Origin` on a WebSocket handshake. A page from another site
/// must not be able to talk to the panel with this PC's no-token access.
fn origin_matches_host(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true; // not a browser page
    };
    let (Some(origin), Some(host)) = (
        origin.to_str().ok(),
        headers.get(header::HOST).and_then(|h| h.to_str().ok()),
    ) else {
        return false;
    };
    let authority = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    authority.trim_end_matches('/').eq_ignore_ascii_case(host)
}

fn denied_response(denied: GmDenied) -> Response {
    match denied {
        // One answer for "no token", "wrong token" and "remote access is off".
        GmDenied::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({
                "status": "unauthorized",
                "message": "game master token required"
            })),
        )
            .into_response(),
        GmDenied::TooManyAttempts { retry_after_secs } => (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, retry_after_secs.to_string())],
            Json(serde_json::json!({
                "status": "too_many_attempts",
                "message": "too many failed attempts, try again later",
                "retry_after": retry_after_secs
            })),
        )
            .into_response(),
    }
}

/// Middleware of every guarded route: decides who is let in (the verdict goes
/// to the handler as a [`GmAccess`] extension).
async fn gm_gate(State(ctx): State<ControlContext>, mut request: Request, next: Next) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let headers = request.headers();
    // A request that does not address this PC by a loopback name (proxy
    // headers, or a rebound hostname) is never "local": it needs the token.
    let proxied = has_proxy_headers(headers) || !host_is_loopback(headers);
    let bearer = bearer_token(headers);
    let query = request.uri().query().and_then(|q| query_param(q, GM_PARAM));

    match ctx
        .auth
        .authorize(peer, proxied, bearer.as_deref(), query.as_deref())
    {
        Ok(access) => {
            request.extensions_mut().insert(access);
            next.run(request).await
        }
        Err(denied) => {
            // The address only: never the token or the URL.
            tracing::debug!(
                "control: request from {} refused ({denied:?})",
                peer.map_or_else(|| "unknown".to_string(), |p| p.ip().to_string())
            );
            denied_response(denied)
        }
    }
}

// ------------------------------------------------------------------- REST ---

#[derive(Deserialize)]
struct IdQuery {
    id: usize,
}

#[derive(Deserialize)]
struct IdValueQuery {
    id: usize,
    value: f32,
}

#[derive(Deserialize)]
struct IdOnQuery {
    id: usize,
    on: bool,
}

#[derive(Deserialize)]
struct ValueQuery {
    value: f32,
}

#[derive(Deserialize)]
struct FadeQuery {
    /// Fade-out time of `stop_all`; absent = stop at once.
    fade_ms: Option<u32>,
}

/// What a request wants back from the board state.
#[derive(Clone, Copy)]
enum View {
    /// The whole [`BoardState`].
    State,
    /// Only the scene list ([`ScenesResponse`]).
    Scenes,
}

/// Why a command did not produce a state.
enum AskError {
    /// The engine thread is gone.
    Stopped,
    /// The engine thread did not answer in time.
    NoAnswer,
    /// The engine refused the command (bad id, playback error).
    Rejected(String),
}

/// Queue `action` for the engine thread and wait for its answer.
async fn ask(
    commands: &Sender<ControlCommand>,
    action: ControlAction,
) -> Result<BoardState, AskError> {
    let (reply, rx) = oneshot::channel();
    commands
        .send(ControlCommand { action, reply })
        .map_err(|_| AskError::Stopped)?;
    match tokio::time::timeout(ENGINE_TIMEOUT, rx).await {
        Ok(Ok(Ok(state))) => Ok(state),
        Ok(Ok(Err(message))) => Err(AskError::Rejected(message)),
        _ => Err(AskError::NoAnswer),
    }
}

async fn run(
    ctx: &ControlContext,
    access: GmAccess,
    headers: &HeaderMap,
    action: ControlAction,
) -> Response {
    run_view(ctx, access, headers, action, View::State).await
}

async fn run_view(
    ctx: &ControlContext,
    access: GmAccess,
    headers: &HeaderMap,
    action: ControlAction,
    view: View,
) -> Response {
    // Without a token the caller is only "from this PC": make sure a script on
    // some web page did not send the request on the GM's behalf.
    if access == GmAccess::Local && !headers.contains_key(CONTROL_HEADER) {
        return (StatusCode::FORBIDDEN, "missing control header").into_response();
    }
    match ask(&ctx.commands, action).await {
        Ok(state) => {
            ctx.feed.publish(state.clone());
            match view {
                View::State => Json(state).into_response(),
                View::Scenes => Json(ScenesResponse {
                    scenes: state.scenes,
                    active_scene: state.active_scene,
                })
                .into_response(),
            }
        }
        Err(AskError::Rejected(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(AskError::Stopped) => {
            (StatusCode::SERVICE_UNAVAILABLE, "audio engine stopped").into_response()
        }
        Err(AskError::NoAnswer) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "audio engine did not answer",
        )
            .into_response(),
    }
}

async fn control_page() -> Response {
    Html(CONTROL_HTML).into_response()
}

async fn control_state(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
) -> Response {
    run(&ctx, access, &headers, ControlAction::List).await
}

async fn control_play(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<IdQuery>,
) -> Response {
    run(&ctx, access, &headers, ControlAction::Play(q.id)).await
}

async fn control_stop(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<IdQuery>,
) -> Response {
    run(&ctx, access, &headers, ControlAction::Stop(q.id)).await
}

async fn control_stop_all(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<FadeQuery>,
) -> Response {
    run(&ctx, access, &headers, stop_all_action(q.fade_ms)).await
}

fn stop_all_action(fade_ms: Option<u32>) -> ControlAction {
    match fade_ms {
        Some(ms) if ms > 0 => ControlAction::StopAllFade(ms.min(MAX_FADE_MS)),
        _ => ControlAction::StopAll,
    }
}

/// Longest fade a remote client may ask for.
const MAX_FADE_MS: u32 = 60_000;

async fn control_scenes(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
) -> Response {
    run_view(&ctx, access, &headers, ControlAction::List, View::Scenes).await
}

async fn control_scene_activate(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<IdQuery>,
) -> Response {
    run(&ctx, access, &headers, ControlAction::ActivateScene(q.id)).await
}

async fn control_scene_stop(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
) -> Response {
    run(&ctx, access, &headers, ControlAction::StopScene).await
}

async fn control_volume(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<IdValueQuery>,
) -> Response {
    let action = ControlAction::SetVolume(q.id, q.value);
    run(&ctx, access, &headers, action).await
}

async fn control_loop(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<IdOnQuery>,
) -> Response {
    let action = ControlAction::SetLoop(q.id, q.on);
    run(&ctx, access, &headers, action).await
}

async fn control_master(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<ValueQuery>,
) -> Response {
    run(&ctx, access, &headers, ControlAction::SetMaster(q.value)).await
}

async fn control_stinger(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<IdQuery>,
) -> Response {
    run(&ctx, access, &headers, ControlAction::PlayStinger(q.id)).await
}

// -------------------------------------------------------------- WebSocket ---

/// A command from a `/ws/control` client (the `cmd` field names it).
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum WsCommand {
    Ping,
    State,
    Play {
        id: usize,
    },
    Stop {
        id: usize,
    },
    Volume {
        id: usize,
        value: f32,
    },
    Loop {
        id: usize,
        on: bool,
    },
    Master {
        value: f32,
    },
    StopAll {
        #[serde(default)]
        fade_ms: Option<u32>,
    },
    Scene {
        id: usize,
    },
    StopScene,
    Stinger {
        id: usize,
    },
    Duck {
        on: bool,
    },
    Bus {
        bus: Bus,
        value: f32,
    },
    #[serde(alias = "playlist_play")]
    PlaylistStart {
        id: String,
    },
    PlaylistStop,
    PlaylistPause,
    PlaylistResume,
    PlaylistToggle,
    PlaylistNext,
    PlaylistPrev,
    PlaylistSeek {
        ms: u64,
    },
    PlaylistShuffle {
        on: bool,
    },
    PlaylistRepeat {
        mode: RepeatMode,
    },
}

impl WsCommand {
    /// The engine action behind the command (`None` for `ping`).
    fn into_action(self) -> Option<ControlAction> {
        Some(match self {
            WsCommand::Ping => return None,
            WsCommand::State => ControlAction::List,
            WsCommand::Play { id } => ControlAction::Play(id),
            WsCommand::Stop { id } => ControlAction::Stop(id),
            WsCommand::Volume { id, value } => ControlAction::SetVolume(id, value),
            WsCommand::Loop { id, on } => ControlAction::SetLoop(id, on),
            WsCommand::Master { value } => ControlAction::SetMaster(value),
            WsCommand::StopAll { fade_ms } => stop_all_action(fade_ms),
            WsCommand::Scene { id } => ControlAction::ActivateScene(id),
            WsCommand::StopScene => ControlAction::StopScene,
            WsCommand::Stinger { id } => ControlAction::PlayStinger(id),
            WsCommand::Duck { on } => ControlAction::Duck(on),
            WsCommand::Bus { bus, value } => bus_action(bus, value),
            WsCommand::PlaylistStart { id } => playlist_action(PlaylistCommand::Start(id)),
            WsCommand::PlaylistStop => playlist_action(PlaylistCommand::Stop),
            WsCommand::PlaylistPause => playlist_action(PlaylistCommand::Pause),
            WsCommand::PlaylistResume => playlist_action(PlaylistCommand::Resume),
            WsCommand::PlaylistToggle => playlist_action(PlaylistCommand::TogglePause),
            WsCommand::PlaylistNext => playlist_action(PlaylistCommand::Next),
            WsCommand::PlaylistPrev => playlist_action(PlaylistCommand::Prev),
            WsCommand::PlaylistSeek { ms } => playlist_action(PlaylistCommand::Seek(ms)),
            WsCommand::PlaylistShuffle { on } => playlist_action(PlaylistCommand::SetShuffle(on)),
            WsCommand::PlaylistRepeat { mode } => playlist_action(PlaylistCommand::SetRepeat(mode)),
        })
    }
}

fn playlist_action(command: PlaylistCommand) -> ControlAction {
    ControlAction::Playlist(command)
}

/// A bus volume request. A value outside `0..=1` is refused by the Director
/// (and so answered with an error), not silently clamped.
fn bus_action(bus: Bus, value: f32) -> ControlAction {
    ControlAction::Mix(MixControl::SetBusVolume(bus, value))
}

/// One slot of the client budget, given back on drop.
struct ClientSlot(Arc<AtomicUsize>);

impl ClientSlot {
    fn acquire(clients: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        let mut current = clients.load(Ordering::Acquire);
        loop {
            if current >= max {
                return None;
            }
            match clients.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self(clients.clone())),
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// `GET /ws/control`.
async fn control_ws(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // A no-token (local) socket is open to any web page in the GM's browser
    // unless the page's origin is checked: WebSockets ignore CORS.
    if access == GmAccess::Local && !origin_matches_host(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request").into_response();
    }
    let Some(slot) = ClientSlot::acquire(&ctx.clients, ctx.max_clients) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "too many control clients").into_response();
    };
    // A session that got in with the token ends when the token is replaced.
    let revoked = (access == GmAccess::Token).then(|| ctx.auth.guard().subscribe());
    let shutdown = ctx.shutdown.as_ref().map(|tx| tx.subscribe());
    ws.max_message_size(MAX_WS_MESSAGE)
        .max_frame_size(MAX_WS_MESSAGE)
        .on_upgrade(move |socket| ws_session(socket, ctx, slot, revoked, shutdown))
}

#[derive(Serialize)]
struct SnapshotMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    rev: u64,
    state: &'a BoardState,
}

fn snapshot_json(published: &Published) -> String {
    serde_json::to_string(&SnapshotMessage {
        kind: "snapshot",
        rev: published.rev,
        state: &published.state,
    })
    .unwrap_or_else(|_| r#"{"type":"error","message":"state unavailable"}"#.to_string())
}

fn reply_json(kind: &str, req: Option<&serde_json::Value>, extra: serde_json::Value) -> String {
    let mut message = serde_json::json!({ "type": kind });
    if let Some(req) = req {
        message["req"] = req.clone();
    }
    if let (Some(target), serde_json::Value::Object(extra)) = (message.as_object_mut(), extra) {
        target.extend(extra);
    }
    message.to_string()
}

/// Run one text frame from a client and build the reply.
async fn handle_ws_text(ctx: &ControlContext, text: &str) -> String {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => {
            return reply_json(
                "error",
                None,
                serde_json::json!({"message": "invalid JSON"}),
            );
        }
    };
    // `req` is echoed back; only a short string or a number qualifies.
    let req = value
        .get("req")
        .filter(|r| r.is_number() || r.as_str().is_some_and(|s| s.len() <= 64))
        .cloned();
    let command: WsCommand = match serde_json::from_value(value) {
        Ok(command) => command,
        Err(_) => {
            return reply_json(
                "error",
                req.as_ref(),
                serde_json::json!({"message": "unknown or malformed command"}),
            );
        }
    };
    let Some(action) = command.into_action() else {
        return reply_json("pong", req.as_ref(), serde_json::json!({}));
    };
    match ask(&ctx.commands, action).await {
        Ok(state) => {
            // Everyone follows the feed, the sender included.
            ctx.feed.publish(state);
            reply_json("ack", req.as_ref(), serde_json::json!({"ok": true}))
        }
        Err(AskError::Rejected(message)) => reply_json(
            "error",
            req.as_ref(),
            serde_json::json!({"message": message}),
        ),
        Err(AskError::Stopped) => reply_json(
            "error",
            req.as_ref(),
            serde_json::json!({"message": "audio engine stopped"}),
        ),
        Err(AskError::NoAnswer) => reply_json(
            "error",
            req.as_ref(),
            serde_json::json!({"message": "audio engine did not answer"}),
        ),
    }
}

async fn send_message(socket: &mut WebSocket, message: Message) -> bool {
    matches!(
        tokio::time::timeout(WS_SEND_TIMEOUT, socket.send(message)).await,
        Ok(Ok(()))
    )
}

async fn shutdown_signal(rx: &mut Option<broadcast::Receiver<()>>) {
    match rx {
        Some(rx) => {
            let _ = rx.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

async fn token_replaced(rx: &mut Option<watch::Receiver<Option<String>>>) {
    if let Some(rx) = rx {
        if rx.changed().await.is_ok() {
            return;
        }
    }
    std::future::pending::<()>().await
}

/// One `/ws/control` client: a snapshot now, a snapshot per change, commands
/// in, pings out.
async fn ws_session(
    mut socket: WebSocket,
    ctx: ControlContext,
    _slot: ClientSlot,
    mut revoked: Option<watch::Receiver<Option<String>>>,
    mut shutdown: Option<broadcast::Receiver<()>>,
) {
    let mut feed = ctx.feed.subscribe();
    // The feed may not know the board yet (nothing happened since start):
    // ask the engine once so the first snapshot is the real state.
    if let Ok(state) = ask(&ctx.commands, ControlAction::List).await {
        ctx.feed.publish(state);
    }
    let first = feed.borrow_and_update().clone();
    if !send_message(&mut socket, Message::Text(snapshot_json(&first))).await {
        return;
    }

    let mut ping = tokio::time::interval(WS_PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.reset();
    let mut last_heard = Instant::now();
    let mut window_start = Instant::now();
    let mut in_window = 0u32;
    let mut warned = false;

    loop {
        tokio::select! {
            _ = shutdown_signal(&mut shutdown) => {
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
            _ = token_replaced(&mut revoked) => {
                let frame = CloseFrame {
                    code: CLOSE_GM_TOKEN_REVOKED,
                    reason: "game master token changed".into(),
                };
                let _ = socket.send(Message::Close(Some(frame))).await;
                break;
            }
            changed = feed.changed() => {
                if changed.is_err() {
                    break;
                }
                let latest = feed.borrow_and_update().clone();
                if !send_message(&mut socket, Message::Text(snapshot_json(&latest))).await {
                    break;
                }
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > WS_IDLE_LIMIT {
                    break;
                }
                if !send_message(&mut socket, Message::Ping(Vec::new())).await {
                    break;
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break };
                last_heard = Instant::now();
                let text = match message {
                    Message::Text(text) => text,
                    Message::Close(_) => break,
                    // Pings are answered by the socket itself; a pong only
                    // proves the client is alive.
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Binary(_) => {
                        let reply = reply_json(
                            "error",
                            None,
                            serde_json::json!({"message": "binary frames are not supported"}),
                        );
                        if !send_message(&mut socket, Message::Text(reply)).await {
                            break;
                        }
                        continue;
                    }
                };
                if window_start.elapsed() >= Duration::from_secs(1) {
                    window_start = Instant::now();
                    in_window = 0;
                    warned = false;
                }
                in_window += 1;
                if in_window > MAX_WS_COMMANDS_PER_SEC {
                    if !warned {
                        warned = true;
                        let reply = reply_json(
                            "error",
                            None,
                            serde_json::json!({"message": "too many commands, slow down"}),
                        );
                        if !send_message(&mut socket, Message::Text(reply)).await {
                            break;
                        }
                    }
                    continue;
                }
                let reply = handle_ws_text(&ctx, &text).await;
                if !send_message(&mut socket, Message::Text(reply)).await {
                    break;
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct DuckQuery {
    /// Shorthand for `mode=on` / `mode=off` (used when `mode` is absent).
    on: Option<bool>,
    /// `on`, `off`, `toggle`, `start` (hold) or `end`; default `toggle`.
    mode: Option<String>,
    /// `music`, `ambience`, `sfx`, `all` or `default` (the configured buses).
    bus: Option<String>,
    /// Level the buses are lowered to, `0.0..=1.0` (`on` only).
    amount: Option<f32>,
    attack_ms: Option<u32>,
    release_ms: Option<u32>,
}

#[derive(Deserialize)]
struct BusQuery {
    bus: String,
    value: f32,
}

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, message).into_response()
}

/// `POST /api/control/duck?mode=on|off|toggle|start|end&bus=...&amount=...`
/// (or `?on=true|false`).
async fn control_duck(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<DuckQuery>,
) -> Response {
    let default_mode = match q.on {
        Some(true) => "on",
        Some(false) => "off",
        None => "toggle",
    };
    let mode_text = q.mode.as_deref().unwrap_or(default_mode);
    let Some(mode) = DuckMode::parse(mode_text) else {
        return bad_request(format!("unknown duck mode \"{mode_text}\""));
    };
    let bus_text = q.bus.as_deref().unwrap_or("default");
    let Some(target) = DuckTarget::parse(bus_text) else {
        return bad_request(format!("unknown bus \"{bus_text}\""));
    };
    let action = ControlAction::Mix(MixControl::Duck {
        target,
        mode,
        amount: q.amount,
        attack_ms: q.attack_ms,
        release_ms: q.release_ms,
    });
    run(&ctx, access, &headers, action).await
}

/// `POST /api/control/bus?bus=music|ambience|sfx&value=0.0..1.0`
async fn control_bus(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    Query(q): Query<BusQuery>,
) -> Response {
    let Some(bus) = parse_bus(&q.bus) else {
        return bad_request(format!(
            "unknown bus \"{}\" (try {}, {} or {})",
            q.bus,
            bus_name(Bus::Music),
            bus_name(Bus::Ambience),
            bus_name(Bus::Sfx)
        ));
    };
    run(&ctx, access, &headers, bus_action(bus, q.value)).await
}

#[derive(Deserialize)]
struct PlaylistQuery {
    /// Playlist library id (`start`).
    id: Option<String>,
    /// Position in milliseconds (`seek`).
    ms: Option<u64>,
    /// Switch (`shuffle`).
    on: Option<bool>,
    /// `off`, `all` or `one` (`repeat`).
    mode: Option<String>,
}

/// The playlist command a REST `verb` and its query stand for.
fn playlist_from_rest(verb: &str, q: PlaylistQuery) -> Result<PlaylistCommand, String> {
    let missing = |what: &str| format!("playlist/{verb} needs {what}");
    Ok(match verb {
        "start" | "play" => PlaylistCommand::Start(q.id.ok_or_else(|| missing("?id="))?),
        "stop" => PlaylistCommand::Stop,
        "pause" => PlaylistCommand::Pause,
        "resume" => PlaylistCommand::Resume,
        "toggle" => PlaylistCommand::TogglePause,
        "next" => PlaylistCommand::Next,
        "prev" => PlaylistCommand::Prev,
        "seek" => PlaylistCommand::Seek(q.ms.ok_or_else(|| missing("?ms="))?),
        "shuffle" => PlaylistCommand::SetShuffle(q.on.ok_or_else(|| missing("?on="))?),
        "repeat" => {
            let mode = q.mode.ok_or_else(|| missing("?mode="))?;
            let parsed = serde_json::from_value::<RepeatMode>(serde_json::Value::String(
                mode.to_ascii_lowercase(),
            ))
            .map_err(|_| format!("unknown repeat mode \"{mode}\" (try off, all or one)"))?;
            PlaylistCommand::SetRepeat(parsed)
        }
        other => return Err(format!("unknown playlist command \"{other}\"")),
    })
}

/// `POST /api/control/playlist/<verb>` (see the module docs).
async fn control_playlist(
    State(ctx): State<ControlContext>,
    Extension(access): Extension<GmAccess>,
    headers: HeaderMap,
    axum::extract::Path(verb): axum::extract::Path<String>,
    Query(q): Query<PlaylistQuery>,
) -> Response {
    match playlist_from_rest(&verb, q) {
        Ok(command) => run(&ctx, access, &headers, playlist_action(command)).await,
        Err(message) => bad_request(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GroupConfig, SoundConfigEntry};

    fn write_wav(path: &std::path::Path) {
        // 0.2 s of a quiet 48 kHz stereo tone, 16-bit PCM.
        let frames = 9_600u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + frames * 4).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(frames * 4).to_le_bytes());
        for i in 0..frames {
            let v = ((i as f32 * 0.05).sin() * 3000.0) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn board_loads_the_library_and_runs_actions() {
        let dir = std::env::temp_dir().join(format!("bardwave-board-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        write_wav(&wav);

        let library = LibraryConfig {
            groups: vec![GroupConfig {
                id: "g".into(),
                name: "Tavern".into(),
            }],
            sounds: vec![
                SoundConfigEntry {
                    path: wav.to_string_lossy().into_owned(),
                    name: "Tone".into(),
                    volume: 0.5,
                    looped: true,
                    group: Some("g".into()),
                    ..Default::default()
                },
                SoundConfigEntry {
                    path: dir.join("missing.wav").to_string_lossy().into_owned(),
                    name: "Missing".into(),
                    volume: 1.0,
                    looped: false,
                    group: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let engine = AudioEngine::new().unwrap();
        let mut board = Board::load(&engine, &library);
        assert_eq!(board.len(), 1, "the missing file is skipped");

        let state = board.handle(&engine, ControlAction::Play(0)).unwrap();
        assert!(state.sounds[0].playing);
        assert_eq!(state.sounds[0].group.as_deref(), Some("Tavern"));
        assert_eq!(state.sounds[0].volume, 0.5);

        let state = board
            .handle(&engine, ControlAction::SetVolume(0, 0.25))
            .unwrap();
        assert_eq!(state.sounds[0].volume, 0.25);
        let state = board
            .handle(&engine, ControlAction::SetLoop(0, false))
            .unwrap();
        assert!(!state.sounds[0].looped);

        let state = board.handle(&engine, ControlAction::StopAll).unwrap();
        assert!(!state.sounds[0].playing);
        assert!(board.handle(&engine, ControlAction::Play(7)).is_err());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn query_param_finds_and_decodes_the_token() {
        assert_eq!(query_param("g=abc", "g").as_deref(), Some("abc"));
        assert_eq!(query_param("id=3&g=a%2Fb%3d", "g").as_deref(), Some("a/b="));
        assert_eq!(query_param("id=3", "g"), None);
        assert_eq!(query_param("gg=1&g", "g"), None);
        // A broken escape stays as it is instead of panicking.
        assert_eq!(query_param("g=%zz%", "g").as_deref(), Some("%zz%"));
        assert_eq!(query_param("g=a+b", "g").as_deref(), Some("a+b"));
    }

    #[test]
    fn bearer_token_is_read_case_insensitively() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer_token(&headers), None);
        headers.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(bearer_token(&headers).as_deref(), Some("abc"));
        headers.insert(header::AUTHORIZATION, "bearer   abc ".parse().unwrap());
        assert_eq!(bearer_token(&headers).as_deref(), Some("abc"));
        headers.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
        headers.insert(header::AUTHORIZATION, "Bearer".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
    }

    #[test]
    fn websocket_origin_must_match_the_host() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:8080".parse().unwrap());
        assert!(origin_matches_host(&headers), "no Origin: not a browser");
        headers.insert(header::ORIGIN, "http://127.0.0.1:8080".parse().unwrap());
        assert!(origin_matches_host(&headers));
        headers.insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        assert!(!origin_matches_host(&headers));
        headers.insert(header::ORIGIN, "null".parse().unwrap());
        assert!(!origin_matches_host(&headers));
        headers.remove(header::HOST);
        assert!(!origin_matches_host(&headers));
    }

    #[test]
    fn only_loopback_host_names_are_local() {
        let host = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, value.parse().unwrap());
            headers
        };
        for ok in [
            "127.0.0.1",
            "127.0.0.1:8080",
            "localhost",
            "LocalHost:8080",
            "[::1]",
            "[::1]:8080",
        ] {
            assert!(host_is_loopback(&host(ok)), "{ok}");
        }
        for bad in [
            "evil.example:8080",
            "localhost.evil.example",
            "127.0.0.1.evil.example",
            "192.168.1.5:8080",
            "[::2]:8080",
            "[::1]x",
        ] {
            assert!(!host_is_loopback(&host(bad)), "{bad}");
        }
        assert!(!host_is_loopback(&HeaderMap::new()), "no Host");
    }

    #[test]
    fn proxy_headers_are_detected() {
        let mut headers = HeaderMap::new();
        assert!(!has_proxy_headers(&headers));
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        assert!(has_proxy_headers(&headers));
        for name in [
            "via",
            "x-forwarded-port",
            "cdn-loop",
            "x-original-forwarded-for",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(name, "1".parse().unwrap());
            assert!(has_proxy_headers(&headers), "{name}");
        }
    }

    #[test]
    fn ws_commands_parse_and_garbage_is_rejected() {
        let parse = |text: &str| serde_json::from_str::<WsCommand>(text);
        assert!(matches!(
            parse(r#"{"cmd":"play","id":3,"req":9}"#),
            Ok(WsCommand::Play { id: 3 })
        ));
        assert!(matches!(
            parse(r#"{"cmd":"stop_all"}"#),
            Ok(WsCommand::StopAll { fade_ms: None })
        ));
        assert!(matches!(parse(r#"{"cmd":"ping"}"#), Ok(WsCommand::Ping)));
        for bad in [
            "",
            "null",
            "[]",
            r#"{"cmd":"launch_missiles"}"#,
            r#"{"cmd":"play"}"#,
            r#"{"cmd":"play","id":-1}"#,
            r#"{"cmd":"volume","id":1,"value":"loud"}"#,
            r#"{"id":1}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must be rejected");
        }
        // A huge fade is capped.
        assert!(matches!(
            stop_all_action(Some(u32::MAX)),
            ControlAction::StopAllFade(MAX_FADE_MS)
        ));
        assert!(matches!(stop_all_action(Some(0)), ControlAction::StopAll));
    }

    #[test]
    fn feed_bumps_the_revision_only_on_change() {
        let feed = ControlFeed::new();
        assert_eq!(feed.latest().rev, 0);
        let state = BoardState {
            master_volume: 0.5,
            ..Default::default()
        };
        feed.publish(state.clone());
        assert_eq!(feed.latest().rev, 1);
        feed.publish(state.clone());
        assert_eq!(feed.latest().rev, 1, "same state, same revision");
        feed.publish(BoardState {
            master_volume: 0.6,
            ..state
        });
        assert_eq!(feed.latest().rev, 2);
    }

    #[test]
    fn client_slots_are_limited_and_given_back() {
        let clients = Arc::new(AtomicUsize::new(0));
        let a = ClientSlot::acquire(&clients, 2).expect("first");
        let _b = ClientSlot::acquire(&clients, 2).expect("second");
        assert!(ClientSlot::acquire(&clients, 2).is_none());
        drop(a);
        assert!(ClientSlot::acquire(&clients, 2).is_some());
    }

    #[test]
    fn board_publishes_stingers_ducking_and_changes_to_the_feed() {
        use crate::audio::director::mock::{MockCall, MockPlayback};

        let library = LibraryConfig {
            sounds: vec![
                SoundConfigEntry {
                    id: "a".into(),
                    path: "/fake/a.wav".into(),
                    name: "Wind".into(),
                    volume: 1.0,
                    ..Default::default()
                },
                SoundConfigEntry {
                    id: "b".into(),
                    path: "/fake/b.wav".into(),
                    name: "Gong".into(),
                    volume: 1.0,
                    oneshot_stinger: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let pb = MockPlayback::new();
        let ids = [pb.add_sound(), pb.add_sound()];
        let mut board = Board::assemble(&library, &[(0, ids[0]), (1, ids[1])]);
        let feed = ControlFeed::new();
        board.attach_feed(&pb, feed.clone());
        let rev = feed.latest().rev;
        assert_eq!(rev, 1);
        assert!(feed.latest().state.sounds[1].stinger);

        // A tick with nothing new publishes nothing.
        board.tick(&pb);
        assert_eq!(feed.latest().rev, rev);

        let state = board.handle(&pb, ControlAction::PlayStinger(1)).unwrap();
        assert!(pb
            .calls()
            .iter()
            .any(|c| matches!(c, MockCall::OneShot { sound, .. } if *sound == ids[1])));
        assert!(state.sounds[1].playing);
        assert!(feed.latest().rev > rev, "the change reached the feed");

        let state = board.handle(&pb, ControlAction::Duck(true)).unwrap();
        assert!(state.ducked);
        assert!(feed.latest().state.ducked);
        let state = board.handle(&pb, ControlAction::Duck(false)).unwrap();
        assert!(!state.ducked);

        assert!(board.handle(&pb, ControlAction::PlayStinger(9)).is_err());
    }
}
