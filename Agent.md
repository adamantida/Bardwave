# Agent.md — Bardwave (SoundShared)

> **Project:** Bardwave — free, open-source ambience, music and sound effects for tabletop RPG
> **Repo:** https://github.com/adamantida/Bardwave
> **Package:** `soundshared` (version in `Cargo.toml`, currently 1.4.0; the binary is shipped as `Bardwave`)
> **License:** MIT © 2026 adamantida

This file describes what the code **is**. User-facing documentation is `README.md`, the change history is `CHANGELOG.md`, the development plan (in Russian) is `PLAN.md`.

---

## 1. Project Overview

Bardwave is a desktop TTRPG soundboard that streams an ambient mix to players' browsers over the network. The Game Master runs the desktop app (egui console), builds scenes, plays music and effects, and the mix is broadcast over WebSocket as Opus. Players open one URL in any browser and hear the mix — no accounts, no subscriptions, no cloud.

```
┌─────────────────────────── Bardwave ───────────────────────────┐
│  egui console ──AudioCommand──┐                                │
│  hotkeys ─────────────────────┴▶ Director ──Playback──▶ AudioEngine
│  /control: ControlAction ─▶ Board (headless) or GmBridge (console) ─▶ Director │
│                                   scenes, emitters,    ├─ decoder / stream_source
│                                   playlists, ducking   ├─ mixer (tracks, buses, pan,
│                                                        │   fades, duck, limiter)
│                                                        └─ cpal ─▶ speakers
│                                                        │ MixedOutput
│                                                        ▼
│                                     AudioProcessor ─▶ broadcaster (Opus)
│                                                        │
│                                           axum HTTP + WebSocket
└────────────────────────────────────────────────────────┼───────┘
                                       ws://<lan-ip>:8080/ws/audio?k=<key>
                                    ┌───────────────┼───────────────┐
                                    ▼               ▼               ▼
                                 browser         browser         browser
                              (Web Audio API → speakers)
```

**Key dependencies:** egui/eframe 0.30, rfd, cpal, symphonia (WAV/FLAC/MP3/OGG/AAC), rusty-opus (pure Rust Opus), tokio, axum (WebSocket), tokio-tungstenite, rustls / tokio-rustls / rcgen (HTTPS), ebur128 (loudness), rtrb (lock-free ring buffer), zip (bundles), qrcode (GM link QR), global-hotkey, optional `midir` (feature `midi`), socket2 + roxmltree (UPnP), ureq (yt-dlp download), serde/toml/serde_json, tracing, anyhow, thiserror, uuid, crossbeam-channel, parking_lot.

**Build requirements:** Rust 1.85+. Windows 10/11, macOS 11+ and Linux are built in CI (ubuntu, windows, macOS). Linux needs GTK 3, ALSA, xkbcommon development packages. ffmpeg (YouTube import only) is auto-downloaded on Windows; on macOS/Linux it must be installed.

---

## 2. Modules and Architecture

### 2.1 Threads and ownership

* The **audio engine is `!Send`** (its cpal stream). It lives on the thread that created it: the GUI thread in console mode, the main thread in `--server-only`. Everything that touches it runs there.
* The **Director** does not own a thread and does not own the engine. Every operation takes `&impl Playback`; time-based work (delayed scene starts, emitters, playlist transitions, crossfade tails, loudness results) is driven by `Director::tick`, and `Director::next_deadline` tells the owner when to wake up.
* The **network server** runs in its own thread with a tokio runtime. The GUI talks to it with `ServerCommand`s; HTTP handlers of the GM panel only queue `ControlCommand`s, which the main thread executes.
* **Nothing blocks the render thread or the audio callback.** File dialogs, folder scans, bundle jobs and `stat` calls run on short-lived threads and answer through a channel drained once per frame; streamed files are decoded on a worker thread into an SPSC ring buffer; loudness is measured in a worker.

### 2.2 `src/audio`

| File | Responsibility |
|------|----------------|
| `engine.rs` | `AudioEngine`: sound catalog, cpal output (headless fallback when no device), master volume, previews (`play_preview`, local-only), `impl Playback` |
| `devices.rs` | Output device list (`OutputDeviceInfo`, `list_output_devices` for a worker thread), pure picking by name with fallback (`pick_output_device`, `DevicePick`), preview routing (`plan_preview_route`), `OutputSelection` read from `[audio] output_device` / `preview_device`; the engine reopens streams live without stopping tracks |
| `decoder.rs` | Symphonia decoding to 48 kHz / stereo f32 (`DecodedAudio`), file probe, tags (`TrackMeta`) |
| `stream_source.rs` | Streaming decoder for long files: worker thread + `rtrb` ring buffer, seek by generation counter, loop, `StreamEvent::TrackFinished`. Threshold `[audio] stream_threshold_secs` (default 60 s) |
| `mixer.rs` | `AudioMixer`: pull-based mixing driven by the device callback; per-track gain with parametrised fades (equal-power or linear), pan, buses (music / ambience / sfx) with volume and ducking, master volume, limiter, local-only tracks (rendered after the broadcast copy is published, on a fixed-gain preview bus that no fader or ducking touches; a second device can render them alone via `mix_preview_into`) |
| `director.rs` | `Director`: scenes (`ActivationPlan`: start / retune / stop with the scene's fades), snapshots to scene, stingers, delayed starts, state `watch` channel (`DirectorState`), the `Playback` trait and an in-memory `MockPlayback` for tests |
| `director_pan.rs` | `impl Director`: per-sound and per-scene-item pan (scene override), applied at start and live |
| `director_timeline.rs` | `impl Director`: `run_timer_action` maps every `TimerAction` onto an existing Director command |
| `timeline.rs` | Session timers: pure `Timeline` core (`after_ms` / `every_ms`, start / pause / reset, never armed from config), `TimelineRunner` thread, `TimerEvent`s drained by the GUI |
| `recorder.rs` | Session recording: tap of the broadcast mix, WAV writer thread (16-bit PCM or 32-bit float), header refresh every second, split near 1.8 GB, disk-space check on Unix |
| `director_audio.rs` | `impl Director`: bus volumes, ducking (latch / hold / toggle, `[audio] duck_*`), loudness normalization (`CatalogSound::gain_db`, background measurement, `GainUpdate`) |
| `emitter.rs` | Random / periodic sounds of the active scene: pure `Scheduler` (seeded xorshift), `EmitterRunner`, `EmitterHost` embedded in the Director |
| `playlist.rs` | `PlaylistPlayer` (shuffle without repeats, repeat off/all/one, crossfade, seek, pause), `MusicLayer`, `PlaylistCommand`, `PlaylistState` |
| `loudness.rs` | EBU R128 integrated loudness (`ebur128`) and gain computation |
| `youtube.rs` | YouTube import through yt-dlp (+ ffmpeg); self-downloading tools into `tools/`; library files in `bardwave-library/` |

### 2.3 `src/network`

| File | Responsibility |
|------|----------------|
| `server.rs` | axum router (`build_router`, `build_router_with_control`), `NetworkServer`, `ServerHandle`, `/api/status`, embedded static files and PWA assets, request log that keeps `?k=` / `?g=` out |
| `broadcast.rs` | `AudioBroadcaster` / `BroadcasterHandle`: client registry, listener count, format announcement, 20 ms Opus frames with `seq_no` and `server_ms`; `NowPlayingFeed` (watch channel) and the throttled `now_playing` text message sent on connect and on change |
| `opus_wire.rs` | `WireEncoder`: re-blocks any device block size into whole 960-frame packets, 64 kbit/s VBR |
| `websocket.rs` | `/ws/audio` session: welcome, format replay, heartbeat, key-revocation close (code 4001) |
| `access.rs` | `AccessGuard` (player key `?k=`), `GmGuard` / `GmAuth` (GM token `?g=` / Bearer, loopback rule, proxy-header rule, rate limit: 10 failures per minute, close code 4003), `generate_key`, constant-time compare |
| `tls.rs` | Optional HTTPS (`server.tls`): rustls over the existing listener (`serve` replaces `axum::serve`), own PEM (`tls_cert` / `tls_key`) or a self-signed certificate kept in `<data dir>/tls/` (renewed on expiry or when the LAN addresses change), SHA-256 fingerprint (`TlsInfo`, logged, shown in the console) |
| `control.rs` | GM panel (mounted in both modes): `Board` (library loaded into a Director), `ControlAction` / `ControlCommand`, REST `/api/control/*`, `/ws/control` (snapshots with `rev`, commands with `req` acks), `ControlFeed`, `X-Bardwave-Control` and `Origin` checks; playlist commands (`playlist_*`, `/api/control/playlist/*`), bus levels and ducking in the snapshot |
| `local_ip.rs` | LAN address choice: adapter table (`GetAdaptersAddresses` / `getifaddrs`) scored, VPN/TUN/benchmark/link-local ranges never win |
| `upnp.rs` | Own UPnP IGD client: SSDP from every LAN address, renewal, `UpnpStatus` (incl. double NAT) |
| `static/` | `index.html`, `app.js`, `style.css`, `opus-decoder.min.js` (listener page, reconnect back-off, link quality, Wake Lock, Media Session, per-player volume in `localStorage`, `now_playing` handler), `manifest.webmanifest` + icons (PWA), `control.html/js/css` (phone GM page) |

### 2.4 `src/gui`

| File | Responsibility |
|------|----------------|
| `mod.rs`, `app.rs` | `GuiApp`, `SoundSharedApp`: frame loop, dialogs, command execution against the engine and the Director, `sync_director` |
| `types.rs` | `AppState`, `AudioCommand`, `ServerCommand`, `ServerStatus`, `SoundInfo` |
| `groups.rs` | `Library` model (groups, sounds, tags, scenes, playlists, hotkeys, buses), `to_config` / from config, `GroupFilter` |
| `sound_list.rs`, `top_bar.rs`, `dialog.rs`, `widgets.rs`, `theme.rs`, `motion.rs` | list, top bar, dialogs, widgets, theme, reduced motion |
| `scenes.rs` | scene deck, editor, save-mix flow, stinger pads, transition control, Stop scene / Stop all |
| `music.rs` | transport bar, playlist manager, "Emitters & music" dialog |
| `hotkeys_ui.rs` | `poll_hotkeys` once per frame, Hotkeys dialog (capture, conflicts, global switch) |
| `settings_extras.rs` + `settings_extras/` | MIX section: bus faders, DUCK button, normalization, interface scale slider (`gui.ui_scale`), update-check block, hotkey and Timers entries |
| `server_panel.rs` | SERVER panel: addresses, UPnP status, access key, GM link, QR |
| `https_ui.rs` | SECURE CONNECTION (HTTPS) section: On/Off, Self-signed / My certificate (paths, Browse…, background validation), fingerprint + Copy, expiry, Regenerate (asks first), apply/restart notices; pure view logic + tests (`tests/https_setting.rs`); guide in `docs/en/https.md`, `docs/ru/https.md` |
| `audio_settings.rs` | AUDIO OUTPUT section: output and preview device choice with status |
| `recorder_ui.rs` | REC button, time / size readout, recordings folder |
| `profiles_ui.rs` | Profile menu: switch (restart), new, rename, delete with typed confirmation |
| `timeline.rs` | `TimersUi`: Timers dialog and the glue running fired timers against the Director |
| `pan_ui.rs` | pan slider (right-click menu, scene editor rows) |
| `onboarding.rs` | first-run "Getting started" card (pure launch / step rules, Help menu entry) |
| `gm_bridge.rs` | console side of the phone panel: runs `ControlCommand`s in the window's frame loop and feeds `/ws/control` with the window's state |
| `library_ui.rs` + `library_ui/` | search / tag chips, folder import, missing-file repair, bundle dialogs, preview; `logic` (pure), `state`, `tasks` (background), `ops`, `toolbar`, `dialogs` |

### 2.5 Other modules

| Path | Responsibility |
|------|----------------|
| `src/main.rs` | entry point; `--server-only`; config load, key generation (`access_token`, `gm_token`), engine and server threads, `AudioProcessor` (engine output → broadcaster), headless loop (`Board`, `tick`, gain updates) |
| `src/lib.rs` | library root, `VERSION` |
| `src/profiles.rs` | campaign profiles: name validation, `--profile` parsing, create / copy / rename / delete, `recent.toml`; `paths::set_active_profile` points `config_path` and `library_dir` at `profiles/<name>/` (the log, tools and `recent.toml` stay in the root) |
| `src/updates.rs` | opt-in update check: pure version logic, bounded `fetch_latest` against the GitHub latest release, `UpdateChecker` thread; notification only |
| `src/config.rs` | `config.toml` schema and persistence, `ensure_ids`, `ensure_gm_token`, hotkey string normalization |
| `src/paths.rs` | data directory: `choose_data_dir` (pure, per-OS), `data_dir()`, `config_path`, `library_dir`, `tools_dir`, `log_path` |
| `src/hotkeys.rs`, `src/hotkeys/` | `Binding` parser, `map` (conflicts), `dispatch` (`HotkeyDispatcher` → Director), `local` (egui keys), `service` (system-wide through `global-hotkey`), `midi` (only with `--features midi`; not connected to the GUI, MIDI bindings are reported as unavailable) |
| `src/library_scan.rs` | recursive folder scan, duplicate detection (path and content fingerprint) |
| `src/library_query.rs` | composite search: group + tags + text |
| `src/library_bundle.rs` | `.zip` bundles: `bundle.toml` + optional `audio/`, conflict policies (skip / rename / replace), dry run |
| `src/error.rs` | `SoundSharedError` and friends |
| `build.rs` | Windows icon resource (`assets/icon.rc`) |
| `examples/` | `audio_probe.rs`, `yt_probe.rs` |
| `tests/` | integration tests: scenes, emitters, playlists, ducking/loudness, hotkeys, library bundle / UI flow, GM auth and page, listener assets, WebSocket wire format, stream pipeline, config compatibility, docs (`docs_gm_token.rs`) |
| `packaging/` | `README.txt`, example `config.toml`, `windows/bardwave.iss`, `linux/` (AppImage script, `.desktop`), `macos/` (app, dmg, signing scripts, README) |
| `.github/workflows/` | `ci.yml` (fmt, clippy, tests on 3 OSes, clippy with `midi`), `release.yml`, `release-extras.yml`, `release-macos.yml` |

### 2.6 Control flow

* **One control core.** Console `AudioCommand`s, hotkeys and the headless `/control` panel all end in the same `Director` operations, so scenes behave identically everywhere. `Playback` is the narrow interface to the engine (and to a mock in tests).
* **Scene activation:** `Director::activate_scene` diffs "playing now" against "scene wants" and executes start / retune / stop with the scene's `fade_in_ms` / `fade_out_ms` / `curve`; re-activating mid-transition continues from current gains. Emitters of the scene start; its playlist (music layer) starts.
* **Ids, not paths:** scenes, playlists and hotkeys reference sound ids (UUID strings); the engine id of a loaded sound is separate (`SoundId`) and mapped in `sync_director`.
* **Console vs headless:** both sync scenes **and playlists** into the Director (the console in `sync_from_library`, the headless `Board` through `set_playlists`), so music layers and the GM page's Music block work in both. The GM panel is mounted in both modes: with `--server-only` the commands run on the main thread, in the console `GmBridge` runs them in the window's frame loop (`AppState::gm_panel_served`). Hotkeys and session timers run in the console only.
* **Stop all:** one routine silences sounds, the active scene (delayed starts, emitters), the music layer and the headphone preview; the button, hotkey, timer action and *Fade Out* all end there.
* **Profiles:** chosen before any path is used (`--profile` in `main.rs`); switching restarts the process with the other profile.
* **Stream:** the engine publishes the broadcast copy of the mix (before local-only previews), `AudioProcessor` forwards it to the broadcaster, which Opus-encodes and fans it out to `/ws/audio` sessions.

### 2.7 Configuration

`config.toml` lives in the data directory (see `paths.rs`). The full schema with examples is in `README.md` ("Configuration"); the source of truth is `src/config.rs`. Sections: `[server]` (`host`, `port`, `upnp`, `access_token`, `gm_token`), `[audio]` (`buffer_size` 960, `sample_rate`, `channels`, `master_volume`, `normalize`, `normalize_target_lufs`, `duck_amount`, `duck_attack_ms`, `duck_release_ms`, `duck_buses`, `stream_threshold_secs`, `output_device`, `preview_device`), `[logging]`, `[server]` also has `tls`, `tls_cert`, `tls_key`; `[gui]` (`reduce_motion`, `global_hotkeys`, `recordings_dir`, `record_float32`, `check_updates`, `last_update_check`, `ui_scale` 0.8 to 1.6, `onboarding_done`), `[library]` (`groups`, `sounds`, `scenes` with `items` and `emitters`, `playlists`; `pan` on sounds and scene items), top-level `[[hotkeys]]` and `[[timers]]` (`trigger = { after_ms | every_ms }`, `action = { kind = … }`, `enabled`). Every new field has a serde default; `tests/config_compat.rs` keeps v1.4.0 configs loading. A config that fails to parse stops the app (it is never replaced by defaults).

### 2.8 Audio pipeline

* Decode: Symphonia (WAV, FLAC, MP3, OGG-Vorbis, AAC/MP4) → 48 kHz stereo f32, in memory for short files, streamed for long ones.
* Mix: `source → track gain (fade) → pan → bus gain (volume × duck) → sum → master → limiter → broadcast copy`, then `+ local-only tracks → local limiter → device`.
* Network encode: Opus, 20 ms frames (960 samples per channel), ~64 kbit/s VBR.

### 2.9 WebSocket protocols

**`/ws/audio?k=<key>`, binary frame:**

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 bytes | `seq_no` — u32 LE, monotonically increasing |
| 4 | 4 bytes | `server_ms` — u32 LE, stream time since broadcaster start |
| 8 | variable | Opus packet (one 20 ms frame) |

Text messages: `{"type":"welcome",…}`, `{"type":"format","data":{"sample_rate":48000,"channels":2,"format":"opus","frame_size":960}}` (first frame, replayed to late joiners), `ping` / `pong`. The server sends `{"type":"now_playing","scene":…|null,"track":…|null}` on connect and when the scene or track changes (throttled); the listener page shows it. With `server.tls` the same paths are served as `wss://`. Clients ignore unknown types.

**`/ws/control?g=<token>`:** JSON text frames ≤ 4096 bytes. Server → client: `snapshot{rev,state}`, `ack{req}`, `error{req,message}`, `pong`. Client → server: `ping`, `state`, `play`, `stop`, `volume`, `loop`, `master`, `stop_all{fade_ms?}`, `scene`, `stop_scene`, `stinger`, `duck`, `bus`. At most 16 clients, 100 commands per second each. Details: the module docs of `control.rs`.

### 2.10 Inter-component commands

* **`AudioCommand`** (GUI → engine/Director, each with a one-slot reply channel that is never awaited on the render thread): load / play / stop / volume / loop / master / remove sound, `StopAll`, `StopAllFade`, `ActivateScene`, `StopScene`, `SnapshotScene`, `PlayStinger`, `Playlist*` (next, prev, seek, pause, resume, shuffle, repeat…), `SetBusVolume`, `Duck`, `DuckRelease`, `DuckHold`, normalization switch. Device choice and recording are driven directly by the console (`audio_settings.rs`, `recorder_ui.rs`) against the engine and the recorder tap, not through `AudioCommand`.
* **`ServerCommand`** (GUI → server thread): `Start`, `Stop`, `SetBind { host, port }`, `GetStatus`.
* **`ControlAction`** (HTTP → main thread, headless; the console receives the same `ControlCommand`s through `GmBridge`): `List`, `Play`, `Stop`, `StopAll`, `StopAllFade`, `SetVolume`, `SetLoop`, `SetMaster`, `ActivateScene`, `StopScene`, `PlayStinger`, `Duck`, `Mix`.
* **`ServerStatus`**: `running`, `address`, `listener_count`.

---

## 3. Task Management Protocol (agent teams)

Work in this repository may be coordinated through an **AgentTeams** task board. When it is used, follow this protocol:

### 3.1 Claiming a Task

1. Call `agent_teams_claim_task` for the task — it returns an `attempt_id`.
2. Include `attempt_id` in **every** `agent_teams_update_task` call.
3. If the claim is rejected as stale, stop work — the task was reassigned.

### 3.2 Task Lifecycle

```
pending → in_progress → completed (or failed)
   ↑          ↓              ↓
   └── release ←───────────┘
```

- **`claimed` cannot jump to `completed`** — mark `in_progress` first, then `completed` or `failed`.
- Include `attempt_id` on every update call.
- After work and verification finish, call `agent_teams_update_task` immediately — do not wait for captain confirmation.
- Do not approve your own implementation.
- Mail is not a formal next review.

### 3.3 Completion Output Requirements

For **implementation/repair/verification/integration** tasks, include structured fields:

- `acceptanceResults`: array of `{ status: "passed" | "failed", description }`
- `commandsRun`: array of commands executed and their results
- `inScopeChangedPaths`: list of files modified (implementation/repair only)
- `evidenceNote`: concise summary

For **review/requirements** tasks: `verdict: "pass"` to complete; `needs_revision` or `reject` must fail with findings.

### 3.4 Reporting and messaging

After completion, send a message to your parent agent with `source_task_id`, `source_attempt_id` and a self-contained result summary; the parent shares your workspace but does not receive your transcript. Use `send_message({ target, message })` to message teammates and `list_agents({})` to find them.

---

## 4. Development Guidelines

### 4.1 Code Style

- **Typed commands over shared mutable state** — no shared mutable soup between GUI, audio engine and network.
- **No blocking work on the render thread, none in the audio callback beyond mixing** (no locks on the streaming read path, no allocation, no I/O).
- **Back-compatible config:** new `config.toml` fields get `#[serde(default)]`; a change of the schema needs a test in `tests/config_compat.rs`.
- **Ids, not paths,** in anything that refers to a sound.
- **Errors:** `anyhow::Result` and `thiserror`. Panics go through the hook in `main.rs`.
- **Logging:** `tracing`; `RUST_LOG` overrides `[logging] level`. Never log `?k=` / `?g=` or token values.
- **Formatting:** `cargo fmt` clean.
- **Docs that tests read:** `tests/docs_gm_token.rs` reads `README.md`, `packaging/README.txt` and `packaging/config.toml`; keep the `gm_token` / `?g=` / `gm_token = ""` wording true when editing them.

### 4.2 Build & Test Commands

```bash
cargo test --all-targets --locked     # ~780 tests
cargo clippy --all-targets --locked
cargo clippy --all-targets --locked --features midi   # optional MIDI module must keep compiling
cargo fmt --all --check
cargo build --release
cargo run --release                   # console
cargo run --release -- --server-only  # headless, serves /control
```

### 4.3 Releases

The **Release** workflow publishes a release for the version in `Cargo.toml` on every push to `main` when the tag does not exist yet. **Do not bump `version` as part of unrelated work:** that is the owner's decision. `release-extras.yml` and `release-macos.yml` attach installers; see `README.md` ("Packaging", "Releasing") and `packaging/`.

---

## 5. File Structure

```
Bardwave/
├── Agent.md                     # this file
├── README.md                    # user-facing documentation
├── CHANGELOG.md                 # change history (Unreleased section)
├── PLAN.md                      # development plan, in Russian
├── OPUS_MIGRATION.md            # notes on the move from raw PCM to Opus (wire frames are 20 ms now)
├── LICENSE, Cargo.toml, Cargo.lock, build.rs, build.bat
├── assets/                      # icons, icon generator tools
├── examples/                    # audio_probe.rs, yt_probe.rs
├── packaging/                   # README.txt, config.toml, windows/, linux/, macos/
├── .github/workflows/           # ci, release, release-extras, release-macos
├── tests/                       # integration tests (see 2.5)
└── src/
    ├── main.rs, lib.rs, config.rs, error.rs, paths.rs, profiles.rs, updates.rs
    ├── hotkeys.rs, hotkeys/{dispatch,local,map,midi,service,testing}.rs
    ├── library_scan.rs, library_query.rs, library_bundle.rs
    ├── audio/{decoder,devices,stream_source,engine,mixer,director,director_audio,director_pan,director_timeline,emitter,playlist,timeline,recorder,loudness,youtube}.rs
    ├── network/{server,broadcast,opus_wire,websocket,access,control,tls,local_ip,upnp}.rs
    │   └── static/              # index.html, app.js, style.css, opus-decoder.min.js, manifest.webmanifest,
    │                            # icons, control.html/js/css
    └── gui/{app,types,groups,sound_list,top_bar,dialog,widgets,theme,motion,scenes,music,hotkeys_ui,
             settings_extras,server_panel,library_ui,audio_settings,recorder_ui,profiles_ui,timeline,pan_ui,
             onboarding,https_ui,gm_bridge}.rs, gui/library_ui/{logic,state,tasks,ops,toolbar,dialogs}.rs
```

---

## 6. Quality Gates

| Check | Command | Required |
|-------|---------|----------|
| Format | `cargo fmt --all --check` | Must pass (0 diffs) |
| Clippy | `cargo clippy --all-targets --locked` (CI builds with `-D warnings`) | Must pass (0 warnings) |
| Clippy, MIDI | `cargo clippy --all-targets --locked --features midi` | Must pass |
| Test | `cargo test --all-targets --locked` | All tests must pass |
| Build | `cargo build --release` | Must compile cleanly |

CI runs these on ubuntu, windows and macOS. **Not verified on real hardware:** the Windows and Linux installers, the macOS app and global hotkeys on macOS, MIDI, audio-device switching on Windows and Linux, HTTPS with real phones (details in `README.md`). **Not done:** a light theme (the palette is constants in `gui/theme.rs`), per-bus volume for players (one shared stream), README screenshots (no screen-recording permission when the docs were written).

---

## 7. Onboarding for New Agents

1. Read this `Agent.md`, `README.md` and the module docs of `director.rs`, `control.rs`, `paths.rs`, `hotkeys.rs`.
2. Build and test: `cargo build --release`, `cargo test --all-targets`.
3. Run the app: `cargo run --release` — check that the console opens and the SERVER panel works.
4. If a task board is used, claim a task as described in section 3.
5. Keep the docs honest: if you add or remove a feature, update `README.md`, this file and `CHANGELOG.md` in the same change.

---

## 8. Revision History

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | Initial | Agent team documentation for Bardwave |
| 2.0 | 2026-11 | Rewritten from the code: scenes, Director, buses, playlists and streaming, hotkeys, GM panel, library tools, data directory, packaging; removed stale team roles and pre-existing-issues list that belonged to another project |
| 3.0 | 2026-11 | Added from the code: Stop all, timers, pan, audio devices and headphone preview, WAV recording, profiles, HTTPS, update check, first-run wizard, interface scale, GM panel in the console and playlists / buses in it, `now_playing`; corrected the headless and `now_playing` statements |
