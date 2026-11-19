# Developer guide

This guide is for people who build, test or change Bardwave. It describes the code as it is in the repository. For what the application does for players and the Game Master, read the top-level `README.md`. If this guide and the code disagree, the code wins.

Items marked **(not verified)** were not run on the hardware or OS named.

Related pages: [Configuration reference](./configuration.md) (every `config.toml` key and flag for users), [Network, players and the GM remote panel](./network-remote.md), [HTTPS](./https.md), [Troubleshooting, FAQ and security](./troubleshooting-security.md).

## Contents

- [Architecture](#architecture)
- [Threading model and audio-callback rules](#threading-model-and-audio-callback-rules)
- [Typed command channels](#typed-command-channels)
- [Module map](#module-map)
- [Wire protocol](#wire-protocol)
- [Data model and configuration](#data-model-and-configuration)
- [Build and run](#build-and-run)
- [Tests and the layout harness](#tests-and-the-layout-harness)
- [Packaging and releases](#packaging-and-releases)
- [Contributing and code style](#contributing-and-code-style)

## Architecture

The console (egui) and the headless server (`--server-only`) share one control core, the `Director`. Console actions, hotkeys, timers and the phone control page all end in `Director` operations, so a scene behaves the same everywhere.

```text
egui console ──AudioCommand──┐
hotkeys / timers ────────────┴─▶ Director ──Playback──▶ AudioEngine ──▶ cpal (speakers)
/control: ControlAction ─▶ Board (headless) or GmBridge (console) ─▶ Director
                                                    │ MixedOutput
                                                    ▼
                                  AudioProcessor ─▶ AudioBroadcaster (Opus)
                                                    │
                                       axum HTTP + WebSocket (/ws/audio)
```

- `Director` (`src/audio/director*.rs`) owns scenes, emitters, playlists, ducking and pan. It has no thread and does not own the engine; callers pass the engine as `&impl Playback`. Time-based work (delayed scene starts, emitters, playlist transitions, crossfade tails, loudness results) runs in `Director::tick`, and `Director::next_deadline` tells the owner when to wake up.
- `Playback` is the narrow engine interface. `AudioEngine` implements it, and `MockPlayback` (in `director.rs`) implements it for tests.
- `AudioEngine` (`engine.rs`) holds the sound catalog, opens the cpal output and runs previews. The mixer (`mixer.rs`) is pull-based: the device callback is the clock. If no output device can be opened, `main.rs` falls back to a headless mixer loop that mixes every 10 ms, so listeners still get the stream; the status strip then shows that local output is off or failed.
- `AudioProcessor` (a small struct in `main.rs`) reads `MixedOutput` buffers from the engine and hands them to the broadcaster.
- The broadcaster (`network/broadcast.rs`) re-blocks the mix into whole 20 ms frames, Opus-encodes it and fans it out to `/ws/audio` sessions. The server (`network/server.rs`) is an axum router; the same `build_router` serves the listener page, `/api/status` and `/ws/audio`, and `build_router_with_control` adds the GM panel routes. The panel is mounted in both modes, and its routes are reachable only while the server is started.

Signal path (from `mixer.rs`):

```text
source -> track gain (fade) -> pan -> bus gain (volume x duck) -> sum -> master -> limiter
       -> [broadcast copy] -> + local-only tracks (headphone previews) -> local limiter -> device
```

Local-only tracks are rendered after the broadcast copy is published, so listeners never hear them. Previews use a fixed-gain bus that no fader or ducking touches; when a second device is chosen, that device's callback renders the previews alone.

Startup order in `main.rs`: parse `--profile`, pick the data directory, load `config.toml` (a parse error stops here), set up logging, generate missing keys, create the engine on the main thread, spawn the server thread, the audio-processor thread and a listener-count thread, then run the console (`run_gui`) or the headless loop. On exit it asks the server thread to stop (waits up to 5 s) and stops the engine. A profile switch started from the console is launched after the old process has released its port and device.

## Threading model and audio-callback rules

| Thread or task | Owns or does |
|----------------|--------------|
| Main thread | `AudioEngine` (its cpal stream is `!Send`), `Director` calls, `tick`; the eframe console, or the headless loop of `--server-only` |
| cpal device callback | the mixer pass; the master clock |
| Headless mixer loop (`mixer.rs`) | the same mixing, deadline-paced, only while there is no output device |
| `AudioProcessor` thread (`main.rs`) | forwards mixed buffers to the broadcaster; a full channel drops the frame |
| `bardwave-stream` | decodes long files into an `rtrb` ring buffer |
| `bardwave-timeline`, `emitters` | timers and random or periodic sounds |
| Server thread | its own single-threaded tokio runtime: axum server, broadcaster, WebSocket sessions, UPnP task; `netsh` firewall rule on Windows goes to `spawn_blocking` |
| Short-lived workers | folder scans (`library-scan`), dialogs, `decoder` and `youtube-download` jobs, `audio-device-scan`, loudness (`normalize-measure`, `loudness-analyze`), recorder (`recorder`, `recorder-tee`), `update-check`, `hotkey-forwarder` |

Rules (from `Agent.md`, sections 2.1 and 4.1; review rules, not enforced by tooling):

- Never block the render thread. File I/O, `stat` calls and dialogs run on a worker that answers through a channel drained once per frame.
- In the audio callback, only mix. No locks on the streaming read path, no allocation, no I/O. Publishing the broadcast copy uses `try_send` on a bounded channel.
- Decode and measure loudness off the callback.
- Use the engine only on the thread that created it. Reach it from other threads through a command.

> **Warning:** Do not call `AudioEngine` from the server thread. Queue a `ControlCommand` and let the main thread run it, as the GM panel does.

> **Note:** `AppState` still holds a few shared `Arc<Mutex<…>>` values (listener count, UPnP status, network notes, the live `Config`). They are small status mirrors written by the server thread. Do not add new shared mutable state; add a command.

## Typed command channels

| Channel | Type | Capacity | Direction |
|---------|------|----------|-----------|
| audio commands | `AudioCommand` (crossbeam) | bounded 32 | GUI to engine and Director (drained by `process_audio_commands` once per frame) |
| server commands | `ServerCommand` (crossbeam) | bounded 32 | GUI to server thread; the thread polls it every 50 ms and relays it into a tokio `mpsc` of 32 |
| control commands | `ControlCommand` (crossbeam) | unbounded | HTTP handlers to main thread |
| mixer output | `MixedOutput` (crossbeam) | bounded 64 (`MixerConfig::channel_capacity`) | mixer to `AudioProcessor`; `try_send`, never waits |
| broadcaster input | `AudioFrame` (tokio `mpsc`) | 500 (`engine_channel_capacity`) | `AudioProcessor` to broadcaster; `try_send`, a full channel drops the frame |
| WebSocket send queue | tokio `mpsc` | 100 | broadcaster to one session; when it is full that frame is dropped for that client and the connection stays |
| Director state | tokio `watch` of `DirectorState` | latest value | Director to readers |
| Now playing | tokio `watch` of `NowPlaying` (`NowPlayingFeed`) | latest value | console or headless `Board` to broadcaster |
| Replies | crossbeam `bounded(1)` for `AudioCommand` and `ServerCommand`; tokio `oneshot` for `ControlCommand` | 1 | one answer per command |

## Module map

Paths are relative to `src/`. The crate and the binary are named `soundshared`; the product is Bardwave.

| Path | Responsibility |
|------|----------------|
| `main.rs` | entry point, `--server-only`, `--profile`, config load, engine and server threads, `AudioProcessor`, headless loop |
| `lib.rs`, `error.rs` | library root, `VERSION`; error types (`SoundSharedError` and its parts) |
| `config.rs` | `config.toml` schema, defaults, load and save, id minting, hotkey string normalization |
| `paths.rs` | data directory choice and file paths |
| `profiles.rs` | campaign profiles, `--profile`, `recent.toml` |
| `updates.rs` | opt-in update check, notification only |
| `library_scan.rs`, `library_query.rs` | folder scan with duplicate detection; search |
| `library_bundle.rs` | `.zip` bundles with `bundle.toml` and `audio/` |
| `hotkeys.rs`, `hotkeys/map.rs` | binding parser; conflict map |
| `hotkeys/dispatch.rs`, `hotkeys/local.rs` | runs actions on the Director; console-focused keys |
| `hotkeys/service.rs`, `hotkeys/midi.rs` | system-wide keys (`global-hotkey`); MIDI, `midi` feature only |
| `hotkeys/testing.rs` | hotkey test helpers |
| `audio/engine.rs`, `audio/devices.rs` | catalog, cpal output, previews; device listing and routing |
| `audio/decoder.rs`, `audio/stream_source.rs` | Symphonia decoding; streamed long files |
| `audio/mixer.rs` | tracks, fades, pan, buses, limiter, local-only tracks |
| `audio/director.rs` | scenes, snapshots, stingers, `Playback`, `MockPlayback` |
| `audio/director_pan.rs`, `audio/director_audio.rs` | pan; buses, ducking, normalization |
| `audio/director_timeline.rs`, `audio/timeline.rs` | timer actions; session timers |
| `audio/emitter.rs`, `audio/playlist.rs` | random and periodic sounds; music playlists |
| `audio/loudness.rs`, `audio/recorder.rs` | EBU R128; WAV recording |
| `audio/youtube.rs` | YouTube import through yt-dlp and ffmpeg |
| `network/server.rs` | axum router, `NetworkServer`, static files, `/api/status` |
| `network/broadcast.rs`, `network/opus_wire.rs` | client registry, frame headers, now-playing feed; Opus packetizing |
| `network/websocket.rs` | `/ws/audio` session |
| `network/access.rs` | listener key, GM token guard, rate limit |
| `network/control.rs` | GM panel: `Board`, `ControlAction`, REST, `/ws/control` |
| `network/tls.rs`, `network/upnp.rs`, `network/local_ip.rs` | HTTPS; router mapping; advertised LAN address |
| `network/static/` | listener page (`index.html`, `app.js`, `style.css`, `opus-decoder.min.js`), PWA files, GM page (`control.html/js/css`) |
| `gui/app.rs`, `gui/mod.rs`, `gui/types.rs` | frame loop, dialogs, `sync_director`; shared GUI types |
| `gui/groups.rs` | `Library` model |
| `gui/sound_list*.rs`, `gui/library_ui*`, `gui/top_bar.rs` | sound board, library toolbar and dialogs, top bar |
| `gui/scenes.rs`, `gui/music*.rs`, `gui/pan_ui.rs` | scene deck and editor; playlists and emitters (`music_tests.rs` holds their tests); pan slider |
| `gui/hotkeys_ui.rs`, `gui/timeline.rs` | Hotkeys and Timers dialogs |
| `gui/settings_extras*.rs`, `gui/server_panel.rs` | MIX section; NETWORK SERVER panel |
| `gui/https_ui.rs`, `gui/audio_settings.rs`, `gui/recorder_ui.rs` | HTTPS section; AUDIO OUTPUT; REC |
| `gui/profiles_ui.rs`, `gui/onboarding.rs`, `gui/dialog.rs` | profile menu; first-run card; modal dialogs (groups, move a sound, YouTube) |
| `gui/gm_bridge.rs` | runs GM commands in the console frame loop |
| `gui/widgets.rs`, `gui/theme.rs`, `gui/motion.rs` | widgets; palette constants; reduced motion |
| `gui/layout_check.rs`, `gui/layout_probe.rs` | layout checker and probes (`#[cfg(test)]` only) |

Outside `src/`: `build.rs` embeds the Windows icon (`assets/icon.rc`; skipped with a warning when no `rc.exe` is found), `examples/` holds `audio_probe.rs` and `yt_probe.rs`, `packaging/` holds the installers' sources.

## Wire protocol

### Listener stream: `/ws/audio?k=<key>`

A missing or wrong key gets HTTP 401 with a JSON body before the upgrade (`access_token = ""` means any key is accepted). `/api/status?k=<key>` is guarded the same way.

Each audio frame is one binary message with an 8-byte header:

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 bytes | `seq_no`, u32 little-endian, +1 per frame (the 64-bit counter is truncated to 32 bits) |
| 4 | 4 bytes | `server_ms`, u32 little-endian, ms since the broadcaster started |
| 8 | variable | one Opus packet |

Stream: 48 kHz, stereo, 960 samples per channel (20 ms), Opus at 64 kbit/s VBR. Whatever block size the device delivers is re-blocked into whole frames. After a pause of more than 120 ms the partial frame is dropped; a tail left for more than 40 ms is padded and sent. Do not change the header size without changing the listener page (`src/network/static/app.js`) and `tests/websocket_wire_format.rs`.

Text messages from the server:

| `type` | Shape | When |
|--------|-------|------|
| `welcome` | `{"type":"welcome","data":{"client_id":"…","message":"…"}}` | on connect |
| `format` | `{"type":"format","data":{"sample_rate":48000,"channels":2,"format":"opus","frame_size":960}}` | with the first frame; right after `welcome` if the stream has already started; on `get_format` once it has started (before that there is no answer) |
| `now_playing` | `{"type":"now_playing","scene":"…"\|null,"track":"…"\|null}` | on connect and on change, at most one per 200 ms; titles are names only, at most 120 characters |
| `pong` | `{"type":"pong"}` | answer to `ping` |

Client messages: `{"type":"ping"}` and `{"type":"get_format"}`. Unknown types and binary messages are ignored. The server also sends a WebSocket-level ping every 30 s. The session closes with code **4001** when the listener key is revoked, and closes without a code when the server stops. With `server.tls` the same path is `wss://`.

### GM control: `/ws/control?g=<token>`

Frames are JSON text objects of at most 4096 bytes. The client sends `cmd` and an optional `req`. The server sends `snapshot` (`rev`, `state`) right after connecting and after every change, `ack` (`{"type":"ack","req":…,"ok":true}`) and `error` (`message`; both echo `req`), and `pong` (echoes `req`). Anything unknown is answered with `error` and the socket stays open.

Client commands: `ping`, `state`, `play`, `stop`, `volume`, `loop`, `master`, `stop_all` (optional `fade_ms`), `scene`, `stop_scene`, `stinger`, `duck`, `bus`, and the `playlist_*` family (`playlist_start`, `playlist_stop`, `playlist_pause`, `playlist_resume`, `playlist_toggle`, `playlist_next`, `playlist_prev`, `playlist_seek`, `playlist_shuffle`, `playlist_repeat`). The REST equivalents are under `/api/control/*`. Limits: 16 clients at once (the 17th gets HTTP 503), 100 commands per second per client (extra ones get one `error` and are dropped), the server pings idle clients every 25 s and drops a client that is silent for 75 s. The socket closes with code **4003** when the GM token is revoked. The page shell (`/control`, `/control.js`, `/control.css`) is served to anyone; everything that reads or changes the board needs the GM token or a loopback peer. The access rules are in the module docs of `src/network/control.rs`.

## Data model and configuration

### Library model

`[library]` in `config.toml` mirrors the in-memory `Library` (`src/gui/groups.rs`):

- **groups**: `id`, `name`.
- **sounds**: `id`, `path` (required), `name`, `volume`, `looped`, `group`, `tags`, `gain_db`, `bus`, `oneshot_stinger`, `pan`.
- **scenes**: `id`, `name`, `color`, `fade_in_ms`, `fade_out_ms`, `curve`, `stop_others`, `items`, optional `emitters` and `playlist`.
- **playlists**: `id`, `name`, `items`, `shuffle`, `repeat`, `crossfade_ms`, `bus`.

Scenes, playlists and hotkeys refer to sounds by **id** (UUID string), never by path. The engine's own id of a loaded sound (`SoundId`) is separate and is mapped in `sync_director`. Bundles (`bundle.toml`) carry groups, sounds, scenes and playlists, and keep ids on import.

### Configuration reference

The schema is `src/config.rs`; defaults are the serde defaults. Every section is optional.

| Key | Default | Meaning |
|-----|---------|---------|
| `[server] host`, `port` | `"0.0.0.0"`, `8080` | bind address (an IP literal, not a host name) and port 1 to 65535 |
| `[server] upnp` | `true` | ask the router to map the port (IPv4 only; skipped for a loopback bind) |
| `[server] access_token` | generated on first start when the key is missing | listener key; `""` lets anyone who can reach the port listen |
| `[server] gm_token` | generated on first start when the key is missing | GM key; `""` keeps the panel reachable from this PC only |
| `[server] tls`, `tls_cert`, `tls_key` | `false`, unset, unset | HTTPS; own PEM pair, both or neither (one alone fails the start) |
| `[audio] buffer_size` | `960` | device buffer in frames; `0` lets the driver choose; 64 to 8192, other values fall back to 480 with a log warning |
| `[audio] sample_rate`, `channels` | `48000`, `2` | requested device rate (8000 to 192000) and channels (1 to 8); out-of-range values fall back to 48000 and 2. The mix and the stream stay 48 kHz stereo |
| `[audio] master_volume` | `0.8` | master level, must be 0.0 to 1.0; a value outside it stops startup with a mixer error |
| `[audio] normalize`, `normalize_target_lufs` | `false`, `-18.0` | loudness normalization; the target is clamped to -60 through 0 |
| `[audio] duck_amount` | `0.3` | level the ducked buses go to, clamped to 0.0 through 1.0 (`1.0` = no effect) |
| `[audio] duck_attack_ms`, `duck_release_ms` | `200`, `800` | ducking timing |
| `[audio] duck_buses` | `["music", "ambience"]` | buses that duck |
| `[audio] stream_threshold_secs` | `60` | longer files are streamed; `0` turns streaming off |
| `[audio] output_device`, `preview_device` | unset | device names; unset = system default / main output; a name not found falls back to the default |
| `[logging] level`, `file_logging` | `"info"`, `false` | `RUST_LOG` overrides level; `file_logging` also writes `bardwave.log` in the root data directory |
| `[gui] reduce_motion`, `global_hotkeys` | `false`, `false` | animations; system-wide hotkeys |
| `[gui] recordings_dir`, `record_float32` | unset, `false` | WAV folder (default `recordings/` in the data directory); 32-bit float |
| `[gui] check_updates`, `last_update_check` | `false`, `0` | daily update check; Unix time of the last check |
| `[gui] ui_scale` | `1.0` | clamped to 0.8 through 1.6 (a non-finite value gives 1.0) |
| `[gui] onboarding_done` | `false` | first-run card completed |

Per-entry defaults: sound `volume` `1.0`, `looped` `false`, `bus` `"ambience"`, `pan` `0.0`, `gain_db` `0.0`; scene `fade_in_ms` and `fade_out_ms` `2000`, `curve` `"equal_power"` (or `"linear"`), `stop_others` `true`; scene item `volume` `1.0`, `start` `"on_enter"` (or `"manual"`), `delay_ms` `0`; emitter `min_interval_ms` `20000`, `max_interval_ms` `60000`, `volume` `1.0`, `max_overlap` `2`, `volume_jitter` and `pan_jitter` `0.0`; playlist `repeat` `"all"` (`"off"`, `"one"`), `crossfade_ms` `2000`, `bus` `"music"`; timer `trigger` `{ after_ms = 600000 }`, `enabled` `true`. Buses are `music`, `ambience`, `sfx`.

Hotkey and timer actions are tagged by `kind`. Hotkey kinds: `sound`, `scene`, `playlist` (each with `target`), `stop_all`, `stop_scene`, `master_up`, `master_down`, `master_mute`, `duck_toggle`, `duck_hold`, `next_track`, `prev_track`, `pause_music`. Timer kinds: `activate_scene` (`scene`), `stop_scene`, `play_stinger` (`sound`), `stop_all` (`fade_ms`), `start_playlist` (`playlist`), `stop_playlist`, `next_track`, `pause_music`, `resume_music`. A timer trigger is `{ after_ms = … }` or `{ every_ms = … }`; the shortest interval is 100 ms. A hotkey `key` is modifiers and one key joined by `+`; `canonical_hotkey` normalizes it (`"shift+ctrl+a"` becomes `"Ctrl+Shift+A"`).

### Example

```toml
[server]
port = 8080
upnp = true
tls = false

[audio]
master_volume = 0.8
duck_buses = ["music", "ambience"]

[logging]
level = "info"

[[library.groups]]
id = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e01"
name = "Tavern"

[[library.sounds]]
id = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e02"
path = "/home/gm/sounds/tavern-crowd.ogg"
name = "Crowd"
looped = true
group = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e01"
tags = ["crowd"]
bus = "ambience"

[[library.scenes]]
id = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e03"
name = "Tavern evening"
fade_in_ms = 2000
fade_out_ms = 2000
curve = "equal_power"
items = [
  { sound = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e02", volume = 0.6, looped = true },
]

[[hotkeys]]
key = "Ctrl+1"
action = { kind = "scene", target = "7f0c1e52-0a4b-4c1e-9f1e-3a2b5c6d7e03" }

[[timers]]
name = "Last call"
trigger = { after_ms = 600000 }
action = { kind = "stop_all", fade_ms = 3000 }
enabled = true
```

### Compatibility rules

- Every new field has a serde default, so old files load unchanged. A schema change needs a case in `tests/config_compat.rs` (it keeps v1.4.0 files loading).
- A sound, scene or playlist whose `id` is missing, empty, not a UUID or repeated gets a fresh UUID at load time (`ensure_ids`), written on the next save. References inside the library, and hotkeys, follow a replaced hand-written id. Timers are not remapped.
- A missing `config.toml` gives the defaults. A `config.toml` that fails to parse stops the application with an error (a dialog in the console, stderr with `--server-only`). It is never replaced by defaults, because the next save would overwrite the library. (The doc comment on `Config::load` still says it falls back to defaults; `main.rs` does not.)
- Saving rewrites the whole file from the in-memory model (`toml::to_string_pretty`), so comments and key order of a hand-edited file are not kept.
- Timers are saved as definitions only and always start stopped.
- `[[hotkeys]]` and `[[timers]]` are console settings. They are not in bundles and do not run with `--server-only`.

### Data directory

`src/paths.rs` decides the directory with the pure function `choose_data_dir`:

| Situation | Directory |
|-----------|-----------|
| `portable.flag` next to the executable | executable's folder |
| `config.toml` next to the executable (on Windows only if that folder is writable) | executable's folder |
| the executable's folder is writable and not inside a macOS `.app` | executable's folder |
| otherwise, macOS | `~/Library/Application Support/Bardwave` |
| otherwise, Windows | `%APPDATA%\Bardwave` |
| otherwise, Linux and other Unix | `$XDG_DATA_HOME/bardwave` or `~/.local/share/bardwave` |

This is the root directory. If no system location can be derived (no `HOME` or `APPDATA`), the executable's folder is used. `--profile <name>` (or `--profile=<name>`) moves `config.toml` and `bardwave-library/` to `profiles/<name>/` under the root; the log, `tools/` and `recent.toml` stay in the root and are shared. `--profile default` means the root profile. Names are 1 to 32 characters: letters, digits, space, `-` and `_`; `default` and Windows device names such as `CON` are refused. An invalid name or an uncreatable profile folder shows an error and exits with code 2.

> **Note:** The per-OS choice is unit-tested on any host, but not checked on every OS **(not verified)**.

## Build and run

Requirements, as stated in `Agent.md` and `README.md`: Rust 1.85 or newer (`Cargo.toml` has no `rust-version`, so Cargo does not enforce it; not verified). Opus is pure Rust (`rusty-opus`), so no CMake or C compiler is needed. On Debian or Ubuntu, CI installs `pkg-config libgtk-3-dev libasound2-dev libxkbcommon-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libssl-dev`. `ffmpeg` is needed for YouTube import on macOS and Linux; on Windows it is downloaded on first use.

```bash
cargo build --release --locked
cargo run --release                              # console
cargo run --release -- --server-only             # headless, serves /control
cargo run --release -- --profile "Camp 1"        # campaign profile
cargo build --release --locked --features midi   # optional MIDI module
```

The only flags are `--server-only` and `--profile`; other arguments are ignored. The binary is `target/release/soundshared` (`soundshared.exe`). A Windows release build uses the GUI subsystem: started from a terminal it re-attaches to that terminal, and `--server-only` started from Explorer opens a console window of its own. On Windows, `build.bat` wraps common commands (`release`, `test`, `clean`, `fmt`, `clippy`).

## Tests and the layout harness

```bash
cargo test --all-targets --locked
cargo clippy --all-targets --locked
cargo clippy --all-targets --locked --features midi
cargo fmt --all --check
```

There are about 1,190 test attributes in `src/` and `tests/` (1,131 `#[test]` and 60 `#[tokio::test]`; a text count, not a run); `Agent.md` and `README.md` still say "~780". `tests/` holds 33 files, among them `config_compat.rs`, `websocket_wire_format.rs` and `gm_control_auth.rs`. `docs_gm_token.rs` reads `README.md`, `packaging/README.txt` and `packaging/config.toml`, so keep the `gm_token` and `?g=` wording true when you edit them.

The console layout is tested headless. `src/gui/layout_check.rs` runs one full console frame (`GuiApp::draw_frame`) through `egui::Context::run`, reads the painted shapes, and reports:

- `ClipOverflow`: text cut by its clip by more than 1 px;
- `Overlap`: texts overlapping by more than 2 px on both axes;
- `OffScreen`: painted content beyond the window;
- `IdClash`: egui's "two widgets share an id" warning;
- `Hidden`: a required control is cut off, scrolled away or not painted. The required controls are Stop all, Mute and Add Sound, plus Stop Server and Copy Link while the server runs, or Start Server while it is stopped.

A vertical cut at the edge of a scroll area is forgiven (a half-visible row is how scrolling works); a horizontal one never is. The test `the_console_has_no_layout_violations_in_any_size_scale_and_state` runs five window sizes (900 by 600 up to 1600 by 900) at interface scales 1.0 and 1.3 in five states (an empty or a long-named library, with 0 or 2 scenes, server stopped or running), so a regression fails `cargo test`. `layout_probe.rs` provides the text-box helpers that the top bar, music and scene deck tests use for their own width checks. The harness cannot see text spilling over a neighbouring plate, tooltips or open dialogs. It does not touch the audio device, the network or your config.

## Packaging and releases

| Workflow | Trigger | Output |
|----------|---------|--------|
| `ci.yml` | pushes and pull requests to `main`; manual | fmt, clippy, tests and MIDI clippy on Ubuntu, Windows, macOS; warnings are errors (`RUSTFLAGS` from `setup-rust-toolchain`) |
| `release.yml` | push to `main` when tag `v<Cargo.toml version>` is new; or manual with a matching version | tests; Windows x86_64 zip and Linux x86_64 tar.gz (ubuntu-22.04); tag and release; checks that `Cargo.lock` has the same version |
| `release-extras.yml` | release published, or after a successful `Release` run; or manual with a tag | Inno Setup installer (`packaging/windows/bardwave.iss`) and the bare `.exe`; AppImage and `.deb` (cargo-deb); smoke tests; `SHA256SUMS` |
| `release-macos.yml` | release published, or manual with a tag | universal (aarch64 and x86_64) `Bardwave.app` in a `.dmg` plus its `.sha256`; refreshes `SHA256SUMS` |

A release made by `release.yml` with the built-in token does not fire the `release: published` event, so `release-macos.yml` does not start by itself for it: run it by hand with the tag. `release-extras.yml` has its own `workflow_run` trigger for this case.

Sources in `packaging/`: `README.txt` (plain quick start) and `config.toml` (commented example) go into the zip and tar.gz; `windows/bardwave.iss`; `linux/` (`build-appimage.sh`, `bardwave.desktop`); `macos/` (`build-app.sh`, `make-dmg.sh`, `sign-notarize.sh`, `entitlements.plist`, `README.md`). The `.deb` is described by `[package.metadata.deb]` in `Cargo.toml` (package `bardwave`, binary installed as `/usr/bin/bardwave`).

> **Warning:** Do not bump `version` in `Cargo.toml` as part of unrelated work. A push to `main` publishes a release for that version, and the version is the owner's decision. `Cargo.lock` must carry the same version (run `cargo build` after a bump and commit both).

> **Note:** macOS signing and notarization run only when the Apple secrets are set; without them an ad-hoc signed build is attached. Installers, the macOS app and the release workflows have not been run on real machines **(not verified)**; see `CHANGELOG.md`.

## Contributing and code style

Keep the docs honest. A feature change updates `README.md`, `Agent.md` and `CHANGELOG.md` together. Open an issue before a large pull request.

- Use typed commands. Do not share mutable state between the GUI, the engine and the network.
- Never block the render thread, and do nothing but mix in the audio callback.
- Refer to sounds by id.
- New config fields get `#[serde(default)]`, and a schema change gets a test in `tests/config_compat.rs`.
- Use `anyhow::Result` and `thiserror`. Panics go through the hook in `main.rs`. Log with `tracing` (`RUST_LOG` overrides `[logging] level`); never log `?k=`, `?g=` or token values.
- Run `cargo fmt --all`. Keep clippy at zero warnings and keep the `midi` feature compiling.

Pull requests target `main`. Open pull requests run CI but do not create releases.

### Symptoms and causes

| Symptom | Cause | Where to look |
|---------|-------|---------------|
| Error dialog at start, app does not open | `config.toml` does not parse | the message names the file; fix it, the app never resets it |
| Error dialog (or stderr) and exit code 2 at start | invalid `--profile` name, or the profile folder cannot be created | `src/profiles.rs`, `src/paths.rs` |
| Desktop is silent but players hear the stream | no local output device could be opened; the engine runs headless | the status strip shows `Local off — headless (network only)` or `Local failed — <error>`; `main.rs`, `top_bar.rs` |
| Listener gets a close with code 4001 | listener key was regenerated | `network/access.rs`; send players the new link |
| GM page returns 401, then 429 | wrong `?g=` token; after 10 failures in a minute | `network/access.rs` (`GM_MAX_FAILURES`); wait a minute |
| `/ws/control` closes with 4003 | GM token revoked | reload the page with the current `?g=` link |
| Server does not start, status shows Error and the button becomes Retry Server | bind failure, for example a busy port, or an HTTPS setup failure | `main.rs` `start_listener` returns the error to the GUI through the command's reply channel |
| HTTPS start fails | certificate or key unreadable or mismatched, or only one of the two set | [HTTPS guide](./https.md), troubleshooting table |

### Known limitations

- Hotkeys and timers do not run with `--server-only`.
- HTTPS is off by default; until it is on, keys in links travel in plain HTTP. See the [HTTPS guide](./https.md).
- Loudness normalization skips streamed files.
- MIDI is behind the optional `midi` feature (off by default) and is not connected to the console; MIDI bindings are reported as unavailable.
- Global hotkeys work on X11 only on Linux, and are **(not verified)** on macOS. Audio-device switching on Windows and Linux is **(not verified)**.
- There is no light theme (the palette is constants in `gui/theme.rs`) and no per-bus volume for players (one shared stream).
