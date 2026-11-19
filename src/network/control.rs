//! Remote control for `--server-only` mode.
//!
//! Without the console window there used to be no way to load or play
//! anything: the headless server broadcast silence. In that mode the library
//! from `config.toml` is loaded at start ([`Board`]) and a small control page
//! is served at `/control`, with a JSON API under `/api/control/…`.
//!
//! Access rules:
//! * only from this machine (loopback peers) — players reach the server over
//!   the LAN/internet and must never get the GM's controls;
//! * every action needs the `X-Bardwave-Control` header. A web page from
//!   another site cannot send a custom header to us without a CORS preflight,
//!   which this server never approves, so a malicious site open in the GM's
//!   browser cannot press buttons on the board.
//!
//! The audio engine lives on the main thread (its CPAL stream is `!Send`), so
//! the HTTP handlers only queue [`ControlCommand`]s and the main thread runs
//! them through [`Board::handle`].

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Instant;

use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

use crate::audio::director::{Director, DirectorState, Playback};
use crate::audio::{AudioEngine, CatalogSound, SoundId};
use crate::config::LibraryConfig;

/// Header every control action must carry (see the module docs).
pub const CONTROL_HEADER: &str = "x-bardwave-control";

/// One sound on the board, as the control page sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlSound {
    pub id: usize,
    pub name: String,
    pub group: Option<String>,
    pub playing: bool,
    pub looped: bool,
    pub volume: f32,
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

/// Board snapshot returned by every call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BoardState {
    pub sounds: Vec<ControlSound>,
    pub master_volume: f32,
    /// The library's scenes, in order.
    #[serde(default)]
    pub scenes: Vec<ControlScene>,
    /// Index (`ControlScene::id`) of the active scene.
    #[serde(default)]
    pub active_scene: Option<usize>,
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
}

struct BoardSound {
    name: String,
    group: Option<String>,
    sound_id: SoundId,
    looped: bool,
    volume: f32,
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
            catalog.push(CatalogSound {
                // An entry from a hand-built config may have no id yet.
                id: if entry.id.is_empty() {
                    format!("sound-{index}")
                } else {
                    entry.id.clone()
                },
                sound: sound_id,
                volume,
                looped: entry.looped,
                gain_db: entry.gain_db,
                bus: entry.bus,
                stinger: entry.oneshot_stinger,
            });
            sounds.push(BoardSound {
                name: display_name(entry),
                group,
                sound_id,
                looped: entry.looped,
                volume,
            });
        }
        let mut director = Director::new();
        director.sync_library(library.scenes.clone(), catalog);
        Self { sounds, director }
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
    pub fn tick<P: Playback + ?Sized>(&mut self, pb: &P) {
        self.director.tick(pb);
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
        }
        Ok(self.state(pb))
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
                })
                .collect(),
            master_volume: pb.master_volume(),
            scenes,
            active_scene,
        }
    }
}

/// Routes of the control page and API, to merge into the server router.
pub fn router(commands: Sender<ControlCommand>) -> Router {
    Router::new()
        .route("/control", get(control_page))
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
        .with_state(commands)
}

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

/// Only this machine may control the board.
fn is_local(peer: &Option<ConnectInfo<SocketAddr>>) -> bool {
    matches!(peer, Some(ConnectInfo(addr)) if addr.ip().is_loopback())
}

fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        "The control page is only available on the machine that runs Bardwave.",
    )
        .into_response()
}

async fn run(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
    commands: &Sender<ControlCommand>,
    action: ControlAction,
    needs_header: bool,
) -> Response {
    run_view(peer, headers, commands, action, needs_header, View::State).await
}

async fn run_view(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
    commands: &Sender<ControlCommand>,
    action: ControlAction,
    needs_header: bool,
    view: View,
) -> Response {
    if !is_local(&peer) {
        return forbidden();
    }
    if needs_header && !headers.contains_key(CONTROL_HEADER) {
        return (StatusCode::FORBIDDEN, "missing control header").into_response();
    }
    let (reply, rx) = oneshot::channel();
    if commands.send(ControlCommand { action, reply }).is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "audio engine stopped").into_response();
    }
    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(Ok(state))) => match view {
            View::State => Json(state).into_response(),
            View::Scenes => Json(ScenesResponse {
                scenes: state.scenes,
                active_scene: state.active_scene,
            })
            .into_response(),
        },
        Ok(Ok(Err(message))) => (StatusCode::BAD_REQUEST, message).into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "audio engine did not answer",
        )
            .into_response(),
    }
}

async fn control_page(peer: Option<ConnectInfo<SocketAddr>>) -> Response {
    if !is_local(&peer) {
        return forbidden();
    }
    Html(CONTROL_HTML).into_response()
}

async fn control_state(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
) -> Response {
    run(peer, &headers, &commands, ControlAction::List, true).await
}

async fn control_play(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
    Query(q): Query<IdQuery>,
) -> Response {
    run(peer, &headers, &commands, ControlAction::Play(q.id), true).await
}

async fn control_stop(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
    Query(q): Query<IdQuery>,
) -> Response {
    run(peer, &headers, &commands, ControlAction::Stop(q.id), true).await
}

async fn control_stop_all(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
    Query(q): Query<FadeQuery>,
) -> Response {
    let action = match q.fade_ms {
        Some(ms) if ms > 0 => ControlAction::StopAllFade(ms),
        _ => ControlAction::StopAll,
    };
    run(peer, &headers, &commands, action, true).await
}

async fn control_scenes(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
) -> Response {
    run_view(
        peer,
        &headers,
        &commands,
        ControlAction::List,
        true,
        View::Scenes,
    )
    .await
}

async fn control_scene_activate(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
    Query(q): Query<IdQuery>,
) -> Response {
    let action = ControlAction::ActivateScene(q.id);
    run(peer, &headers, &commands, action, true).await
}

async fn control_scene_stop(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
) -> Response {
    run(peer, &headers, &commands, ControlAction::StopScene, true).await
}

async fn control_volume(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
    Query(q): Query<IdValueQuery>,
) -> Response {
    let action = ControlAction::SetVolume(q.id, q.value);
    run(peer, &headers, &commands, action, true).await
}

async fn control_loop(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
    Query(q): Query<IdOnQuery>,
) -> Response {
    run(
        peer,
        &headers,
        &commands,
        ControlAction::SetLoop(q.id, q.on),
        true,
    )
    .await
}

async fn control_master(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    State(commands): State<Sender<ControlCommand>>,
    Query(q): Query<ValueQuery>,
) -> Response {
    run(
        peer,
        &headers,
        &commands,
        ControlAction::SetMaster(q.value),
        true,
    )
    .await
}

/// The control page: a plain list of the library with play/stop, repeat and
/// volume, in the listener page's colours. Self-contained (no CDN).
const CONTROL_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Bardwave | Control</title>
<style>
  :root { --bg:#0A0A0A; --panel:#111111; --line:#27272A; --muted:#71717A; --text:#E4E4E7;
          --rose:#E11D48; --rose-l:#FB7185; }
  * { box-sizing: border-box; }
  body { margin:0; background:var(--bg); color:var(--text);
         font: 14px/1.4 system-ui, -apple-system, "Segoe UI", sans-serif; }
  main { max-width: 760px; margin: 0 auto; padding: 24px 16px; }
  h1 { font-size: 13px; letter-spacing: .25em; text-transform: uppercase; color: var(--rose); margin: 0 0 4px; }
  .sub { color: var(--muted); font-size: 12px; margin-bottom: 20px; }
  .bar { display:flex; gap:12px; align-items:center; padding:12px 16px; background:var(--panel);
         border:1px solid var(--line); margin-bottom:12px; }
  .bar label { font-size:11px; letter-spacing:.2em; text-transform:uppercase; color:var(--muted); }
  .scenes { display:flex; flex-wrap:wrap; gap:8px; align-items:center; margin-bottom:12px; }
  .scenes:empty { display:none; }
  button.scene { padding:10px 14px; background:var(--panel); }
  button.scene.active { background:var(--rose); border-color:var(--rose); color:#fff; }
  .row { display:flex; gap:12px; align-items:center; padding:10px 16px; background:var(--panel);
         border:1px solid var(--line); margin-bottom:8px; }
  .row.playing { border-color: var(--rose); }
  .name { flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
  .group { font-size:11px; color:var(--muted); text-transform:uppercase; letter-spacing:.1em; }
  button { background:transparent; color:var(--text); border:1px solid var(--line); padding:6px 12px;
           font: 600 11px/1 system-ui, sans-serif; letter-spacing:.12em; text-transform:uppercase; cursor:pointer; }
  button:hover { border-color: var(--muted); }
  button.play { background: var(--rose); border-color: var(--rose); color:#fff; min-width: 70px; }
  .row.playing button.play { background: transparent; color: var(--rose-l); }
  button.loop.on { color: var(--rose-l); border-color: var(--rose); }
  input[type=range] { accent-color: var(--rose); width: 110px; }
  .pct { width: 38px; text-align:right; font-variant-numeric: tabular-nums; color: var(--muted); font-size:12px; }
  .err { color: var(--rose-l); font-size: 12px; min-height: 16px; }
  .empty { color: var(--muted); padding: 24px 16px; border:1px dashed var(--line); }
</style>
</head>
<body>
<main>
  <h1>Bardwave control</h1>
  <div class="sub">Server-only mode. Sounds come from the [library] section of config.toml.</div>
  <div class="bar">
    <label for="master">Master</label>
    <input id="master" type="range" min="0" max="100">
    <span class="pct" id="masterPct"></span>
    <span style="flex:1"></span>
    <button id="stopAll">Stop all</button>
  </div>
  <div class="scenes" id="scenes"></div>
  <div class="err" id="err"></div>
  <div id="list"></div>
</main>
<script>
(function () {
  'use strict';
  const H = { 'X-Bardwave-Control': '1' };
  const $ = (id) => document.getElementById(id);
  let dragging = false;

  async function call(path, method) {
    try {
      const res = await fetch(path, { method: method || 'POST', headers: H, cache: 'no-store' });
      if (!res.ok) throw new Error(await res.text());
      $('err').textContent = '';
      render(await res.json());
    } catch (e) {
      $('err').textContent = String(e.message || e);
    }
  }

  function renderScenes(scenes) {
    const box = $('scenes');
    box.textContent = '';
    if (!scenes.length) return;
    for (const sc of scenes) {
      const b = document.createElement('button');
      b.className = 'scene' + (sc.active ? ' active' : '');
      b.textContent = sc.name;
      if (sc.color && /^#[0-9a-fA-F]{3,6}$/.test(sc.color) && !sc.active) b.style.borderColor = sc.color;
      b.onclick = () => call('/api/control/scene/activate?id=' + sc.id);
      box.appendChild(b);
    }
    const stop = document.createElement('button');
    stop.textContent = 'Stop scene';
    stop.onclick = () => call('/api/control/scene/stop');
    box.appendChild(stop);
  }

  function render(state) {
    if (!dragging) {
      $('master').value = Math.round(state.master_volume * 100);
      $('masterPct').textContent = Math.round(state.master_volume * 100) + '%';
    }
    renderScenes(state.scenes || []);
    const list = $('list');
    if (!state.sounds.length) {
      list.innerHTML = '<div class="empty">The library is empty. Add sounds in the console (or in config.toml) and restart.</div>';
      return;
    }
    list.textContent = '';
    for (const s of state.sounds) {
      const row = document.createElement('div');
      row.className = 'row' + (s.playing ? ' playing' : '');
      const play = document.createElement('button');
      play.className = 'play';
      play.textContent = s.playing ? 'Stop' : 'Play';
      play.onclick = () => call('/api/control/' + (s.playing ? 'stop' : 'play') + '?id=' + s.id);
      const name = document.createElement('div');
      name.className = 'name';
      name.textContent = s.name;
      if (s.group) {
        const g = document.createElement('div');
        g.className = 'group';
        g.textContent = s.group;
        name.appendChild(g);
      }
      const loop = document.createElement('button');
      loop.className = 'loop' + (s.looped ? ' on' : '');
      loop.textContent = 'Repeat';
      loop.onclick = () => call('/api/control/loop?id=' + s.id + '&on=' + !s.looped);
      const vol = document.createElement('input');
      vol.type = 'range'; vol.min = 0; vol.max = 100; vol.value = Math.round(s.volume * 100);
      const pct = document.createElement('span');
      pct.className = 'pct';
      pct.textContent = vol.value + '%';
      vol.oninput = () => { dragging = true; pct.textContent = vol.value + '%'; };
      vol.onchange = () => { dragging = false; call('/api/control/volume?id=' + s.id + '&value=' + vol.value / 100); };
      row.append(play, name, loop, vol, pct);
      list.appendChild(row);
    }
  }

  $('master').oninput = () => { dragging = true; $('masterPct').textContent = $('master').value + '%'; };
  $('master').onchange = () => { dragging = false; call('/api/control/master?value=' + $('master').value / 100); };
  $('stopAll').onclick = () => call('/api/control/stop_all');

  // Follow sounds that end by themselves.
  function poll() { if (!dragging) call('/api/control/state', 'GET'); }
  poll();
  setInterval(poll, 1000);
})();
</script>
</body>
</html>
"##;

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
}
