<!--
  Bardwave — free, open-source ambience for your table.
  Repo: https://github.com/adamantida/Bardwave
-->

<div align="center">

# 🎲 Bardwave

**Free, open-source ambience, music and sound effects for your tabletop RPG.**

Run the mix on your own PC and let every player hear it in their browser over the local network.
No subscriptions, no accounts, no cloud. A self-hosted alternative to Syrinscape and RPG Sounds.

[![License: MIT](https://img.shields.io/badge/license-MIT-a04a4a.svg?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-7a3b3b.svg?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-3b3b3b.svg?style=flat-square&logo=windows&logoColor=white)](#requirements)
[![PRs welcome](https://img.shields.io/badge/PRs-welcome-a04a4a.svg?style=flat-square)](#contributing)

[Why](#why) · [Features](#features) · [Getting started](#getting-started) · [Game-master panel](#game-master-panel-phone) · [Configuration](#configuration) · [Security](#security-and-tokens) · [How it works](#how-it-works) · [Not verified](#not-verified-on-real-hardware) · [Roadmap](#roadmap)

</div>

---

> A tavern taproom. A rain-soaked alley at midnight. The low drone before the dragon wakes.
> You hit one scene button. Your four players, on four different laptops, hear exactly what you hear — no Discord screen-share, no file sharing, no "wait, send me that MP3 again".

## Why

Running ambience in a tabletop game is still surprisingly awkward. The paid options tie you to a subscription and their own sound packs. The free options make you share files, run a second voice channel, or host something in the cloud you do not control.

**Bardwave** takes the opposite approach:

- your sound library stays **on your machine**, in whatever format you already own (MP3, FLAC, OGG, WAV, …);
- the mix is built **live** on your desktop, not pre-rendered;
- players need **nothing but a browser** — they open one URL and press Connect;
- *you* stay in control of every fader while the scene plays.

> Status: the changes since v1.4.0 (scenes, music, hotkeys, phone panel, library tools, packaging) are listed in the [CHANGELOG](CHANGELOG.md) under *Unreleased*. The version in `Cargo.toml` is still 1.4.0: it has not been bumped, so no release has been cut from them yet.

## Features

### Scenes and transitions

- A **scene** is a named set of sounds that starts with one click: per-scene volume, loop, automatic or manual start, start delay, colour on the button.
- **Crossfade between scenes**: the scene's `fade_in_ms` / `fade_out_ms` and an equal-power (sin/cos) or linear curve. Switching again in the middle of a transition continues from the current levels instead of jumping. A scene can leave other sounds playing (`stop_others = false`).
- **Save current mix as scene**: play things by hand, press one button, get a scene.
- A shared **transition control** over the scene deck (*Auto* = each scene's own fades), **Stop scene** and **Stop all** (with a fade).
- **Stingers**: sounds marked as stingers are one-shots layered over everything; they do not depend on the active scene and are spared by scene changes. Pads for them sit under the scene deck.
- **Emitters** inside a scene: random or periodic sounds ("crows every 20–60 s"), with a random pick from a set, volume and stereo-position jitter and a maximum overlap. The deck shows when the next sound is due. Per-sound **pan** exists in the mixer.

### Music

- **Playlists** with shuffle (no repeats until every track played), repeat off / all / one, and a crossfade between tracks.
- A transport bar (play/pause, next/previous, seek, position) and a playlist manager. A scene can have a playlist as its **music layer**.
- **Long files are streamed**: anything longer than `stream_threshold_secs` (60 s by default) is decoded on the fly into a ring buffer instead of being held in RAM (an hour of stereo f32 is ~1.4 GB). Short effects stay in memory for an instant start. The track name in the transport bar comes from the file's tags (title, artist) when it has them.

### Mix: buses, ducking, loudness

- Three **buses** — `music`, `ambience`, `sfx` — with their own faders in the MIX section. Each sound and playlist is routed to one.
- **Ducking**: the DUCK button (a click latches it, holding it ducks only while pressed) lowers the chosen buses to a level and brings them back to exactly the fader value. Depth, attack/release time and which buses are ducked are set in `[audio]`.
- **Loudness normalization** (EBU R128): off by default. When on, newly loaded sounds are measured in the background and a fixed correction (`gain_db`) is stored per sound; your own volume is never rewritten. Streamed (long) files are not measured.

### Hotkeys

- Bindings for sounds, scenes, playlists, *Stop all*, *Stop scene*, master volume up/down/mute, duck toggle / duck hold, next/previous track, pause music.
- Work while the console window is focused. **System-wide** shortcuts (window in the background) are an opt-in setting, `gui.global_hotkeys`.
- A dialog captures a key ("press a combination") and shows conflicts and broken bindings. See [Hotkeys](#hotkeys-1) below.

### Library

- **Tags and search**: free-text search over name, file name and tags (case-insensitive, all of Unicode), tag chips, combined with the group filter.
- **Import a folder**: recursive scan, one group per first-level sub-folder on request, duplicates skipped by path and by content.
- **Missing files** are marked instead of silently dropped, with a relink button and a bulk "replace path prefix".
- **Bundles**: export the library (groups, sounds with volumes and tags, scenes, playlists — optionally the audio files) to one `.zip` with `bundle.toml`; import with an explicit policy for name/id/path conflicts (skip, keep both, replace) and a dry-run report. Ids are preserved so scenes keep pointing at the right sounds. Hotkeys are a console setting and are not part of a bundle.
- **Preview** (headphones button): plays a sound on this computer only; it never reaches the players' stream.
- WAV, FLAC, MP3, OGG-Vorbis and AAC/MP4 via [Symphonia](https://github.com/pdeljanov/Symphonia).
- **YouTube links**: paste a video/music URL — yt-dlp (and, on Windows, ffmpeg) download themselves on first use, the audio is saved once into the data directory and plays like any local file. See [Sounds from YouTube](#sounds-from-youtube).
- Sounds have stable ids; scenes, playlists and hotkeys refer to ids, not paths, so moving a file does not break them. A `config.toml` from v1.4.0 loads without changes and gets ids on first load.

### Streaming and the listener page

- WebSocket broadcast of the **final mix** — players hear what the GM hears — as 20 ms **Opus** frames (~64 kbit/s VBR).
- The listener page is **embedded in the binary**. It reconnects on its own (exponential back-off with jitter), shows a **link-quality** readout (latency, buffer, lost packets) with a hint when the network is weak, keeps the phone awake (**Wake Lock**), integrates with the lock-screen controls (**Media Session**), remembers the player's own volume, and is an installable **PWA** (manifest and icons; no service worker, the stream needs none).
- **Access key** in the shared link, **live listener counter** in the console, automatic LAN address detection that ignores VPN/TUN adapters, optional UPnP port forwarding for remote players.
- The page also understands a `now_playing` text message (scene / track name). The server does **not** send it yet, so no scene name appears on players' screens today.

### Phone control panel

- A mobile page, `/control`, with scene buttons, stinger pads, per-sound faders, master volume, bus faders, *Duck*, *Stop scene* and *Stop all*, live through a WebSocket (several devices see the same state).
- Has its **own token** (`gm_token`) that is never the player key. The console shows the link with a **QR code**.
- **Limitation:** the console window does not serve this page yet — it is only served in `--server-only` mode. In that mode playlists are not loaded into the control core, so the page's Music block is disabled and a scene's music layer does not start. See [Game-master panel](#game-master-panel-phone).

### Engineering

- Rust, one binary, one config file; egui console, no runtime to install.
- About 780 tests (`cargo test --all-targets`), including end-to-end broadcast, WebSocket wire-format, GM-auth and config-compatibility tests; CI on Linux, Windows and macOS.
- Typed commands between GUI, audio engine and network, and one control core (the `Director`) shared by the console, the headless server, hotkeys and the phone panel.
- Mix pipeline at 48 kHz / stereo f32, encoded to Opus for the wire. Opus is pure Rust (no CMake, no C toolchain).

## Getting started

### Requirements

- **Windows 10/11**, **macOS 11+** or **Linux**. The audio, network and local-IP detection code is portable Rust: Windows reads the adapter table via `GetAdaptersAddresses`, macOS/Linux via `getifaddrs`.
- To build from source: [Rust toolchain](https://rustup.rs) 1.85 or newer.
- A sound card. No, really — this is the point of the app.

### Install

Releases are published on the [GitHub releases page](https://github.com/adamantida/Bardwave/releases) when the version in `Cargo.toml` changes on `main` (see [Releasing](#releasing)). What the pipeline is set up to attach:

| Platform | Files | Notes |
|---|---|---|
| Windows | `Bardwave-v<ver>-windows-x86_64.zip` (portable, with a commented `config.toml`), `…-setup.exe` (Inno Setup installer), `…-windows-x86_64.exe` | The installer puts Bardwave into Program Files and can add the firewall rule. |
| Linux | `…-linux-x86_64.tar.gz`, `.AppImage`, `.deb` | GTK 3, ALSA and libxkbcommon must be installed. |
| macOS | `…-macos-universal.dmg` | Unsigned unless signing secrets are configured. See [packaging/macos](packaging/macos/README.md). |
| all | `SHA256SUMS` | |

The installers and the macOS app are **built by CI and have not been run on real machines** — see [Not verified on real hardware](#not-verified-on-real-hardware). Details of every package: [`packaging/`](packaging/) (`README.txt`, `windows/bardwave.iss`, `linux/`, `macos/README.md`).

### Build & run

```bash
git clone https://github.com/adamantida/Bardwave.git
cd Bardwave
cargo run --release
```

The console window opens. Add sounds (**Add sound**, or **Folder…** for a whole directory), group or tag them, then press **Start** in the SERVER panel.

> Built binary, if you do not want a terminal hanging around: `cargo build --release` puts `target/release/soundshared` (`soundshared.exe`) — rename it to `Bardwave.exe` if you like.

#### macOS

- **Build:** `cargo run --release` works as above; no system libraries are needed.
- **First start:** macOS asks whether Bardwave may find devices on the local network. Allow it, otherwise players on the LAN cannot connect (System Settings → Privacy & Security → Local Network).
- **App and DMG:** `packaging/macos/build-app.sh` and `make-dmg.sh` turn the binary into `Bardwave.app` and a `.dmg` (universal arm64 + x86_64 with `lipo`); `sign-notarize.sh` does Developer ID signing and notarization when the credentials are present. An unsigned build is opened with right-click → **Open** the first time. Full steps and the signing variables are in [packaging/macos/README.md](packaging/macos/README.md).
- **Data:** inside an `.app` the config and the library live in `~/Library/Application Support/Bardwave`, not next to the binary (see [Where the data lives](#where-the-data-lives)).
- **ffmpeg** (YouTube only) is not downloaded here: `brew install ffmpeg`. `yt-dlp` is fetched automatically.
- **Global hotkeys** and **MIDI** are not verified on a Mac — see [Not verified](#not-verified-on-real-hardware).

#### Linux

- Build packages (Debian/Ubuntu): `pkg-config libgtk-3-dev libasound2-dev libxkbcommon-dev libssl-dev` (as in the CI workflow). To run a prebuilt binary: `libgtk-3-0 libasound2 libxkbcommon0`.
- **ffmpeg** (YouTube only): `apt install ffmpeg`. `yt-dlp` is fetched automatically.
- If a firewall is active, open the listen port (8080 by default).
- Global hotkeys work on X11 only; under Wayland they are unavailable (window shortcuts still work).
- Automatic router port forwarding (UPnP) works as on Windows; only the Windows firewall rule is Windows-specific.
- `.deb`: `cargo install cargo-deb && cargo build --release && cargo deb --no-build`; AppImage: `packaging/linux/build-appimage.sh`.

#### Sounds from YouTube

Click **YouTube** next to *Add Sound*, paste a video or music link and press **Add**. The audio track is downloaded once (the video itself is not), transcoded to MP3 and stored in `bardwave-library/` inside the data directory; the row appears under its title like any local sound.

- **No manual setup** on Windows. On the first **Add** the console fetches the current [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) release (~24 MB) and a static `ffmpeg.exe` (~230 MB) from BtbN's stable builds — progress runs in the dialog. They are saved into `tools/` of the data directory. A `yt-dlp`/`ffmpeg` next to the executable or on the `PATH` always wins over the download.
- On Linux/macOS `yt-dlp` is fetched automatically, but `ffmpeg` must be installed the usual way — the dialog says so when it is missing.
- Playlists are not a board feature — a link pasted with `&list=…` still adds just that one track.
- Re-adding the same link is free: the download already exists, so it is only re-registered.

### Where the data lives

`config.toml`, `bardwave-library/`, `tools/` and `bardwave.log` live in one **data directory**, chosen once at start:

| Situation | Data directory |
|---|---|
| `config.toml` already next to the executable (an old v1.4.0 layout, or the portable zip). On Windows only if that folder is writable | the executable's folder |
| a file named `portable.flag` next to the executable | the executable's folder |
| the executable's folder is writable and it is not inside a macOS `.app` | the executable's folder |
| otherwise, macOS | `~/Library/Application Support/Bardwave` |
| otherwise, Windows (e.g. installed into Program Files) | `%APPDATA%\Bardwave` |
| otherwise, Linux / other Unix | `$XDG_DATA_HOME/bardwave`, or `~/.local/share/bardwave` |

The Windows installer deliberately does not install a `config.toml` (its presence would switch to portable mode), and a `config.toml` that an elevated run left in a read-only `Program Files` does not pin the directory either. Existing v1.4.0 folders keep working unchanged. To reset Bardwave, delete the data directory. Uninstalling never deletes it.

### Profiles (several campaigns)

Each campaign can have its own `config.toml` and library. The **default profile** is the data directory above, exactly as before; nothing is migrated. Any other profile lives in `<data directory>/profiles/<name>/` with its own `config.toml` and `bardwave-library/`. The log, the downloaded tools and the list of recent profiles (`recent.toml`) stay shared in the data directory.

- Start a profile from the command line: `soundshared --profile "Curse of Strahd"` (`--profile default` is the default profile). Names are letters, digits, spaces, `-` and `_`, up to 32 characters.
- In the console, the **Profile** menu in the top bar lists the recent profiles, switches (Bardwave saves, closes and starts again with the other profile), creates a profile (optionally copying the current library), renames and deletes other profiles, and opens the data folder. Deleting a profile that is not empty asks you to type its name; there is no trash.
- Only one profile runs at a time, so two profiles may use the same server port.

### Connect your players

The SERVER panel shows these addresses:

| Address | Who uses it |
|---|---|
| `http://192.168.x.x:8080` | **Your players.** Open it on any device in the same network. |
| `http://127.0.0.1:8080` | **You**, to test the listener page on your own machine. |
| `http://<global-ip>:8080` | **Remote players**, over the internet. Shown as INTERNET once the router forwards the port. |

The LISTEN URL is click-to-copy. Players open it, hit **Establish Link**, and the stream starts. On a phone they can add the page to the home screen.

#### Players outside your network

On **Start** the console asks your router to forward the port to this PC (UPnP IGD) and shows the resulting `INTERNET` address (click to copy). Discovery is sent from every LAN address of the PC, so it also works while a VPN owns the default route. The mapping is renewed while the server runs and removed on **Stop**. Requirements and caveats:

- UPnP must be enabled on the router. If no router answers, the panel says *no UPnP router* and the LAN address keeps working; forward the port by hand instead.
- If the router's external address is private (carrier-grade NAT, or a provider modem in front of your router), the panel says *blocked by a second NAT*: the port is not reachable from the internet until you forward it on the upstream device or get a public IP from your provider.
- **VPN:** with a full-tunnel VPN on the GM's PC, the router forwards the player's connection, but the reply leaves through the tunnel and never arrives. The panel warns about it (*VPN active*); turn the VPN off, or exclude Bardwave from it, for internet players.
- **Windows firewall:** Windows marks many networks as *Public*, and the "allow access" prompt only opens *Private* ones by default. If the panel says *Firewall: port not opened*, allow Bardwave for public networks too (or run it once as administrator so it can add the rule itself). The installer can add the rule for you.
- Opening the internet URL from inside your own network may fail on routers without NAT loopback. Test it from a phone on mobile data.
- Set `upnp = false` in `config.toml` to turn this off.
- The stream travels over plain HTTP/WebSocket (no TLS). See [Security and tokens](#security-and-tokens).

### Game-master panel (phone)

`/control` is a mobile page for driving the session from a phone. **It is served only in headless mode (`--server-only`)**; the console window shows the link and a QR code but does not serve the page yet and says so.

```bash
Bardwave.exe --server-only        # soundshared --server-only from a source build
```

Runs without the console window, for a spare PC next to the router or a session you drive from a phone-sized screen. The saved library (`[library]` in `config.toml`, i.e. whatever you set up in the console) is loaded at start, the listener server starts at once, and the log shows both links:

- the **player link** (with the access key) to share, as in the console;
- a **control page**, `http://127.0.0.1:<port>/control`, on this PC.

On the machine that runs Bardwave the control page opens without a key. From another device (a phone, a laptop) it needs the **GM key**: open `http://<address>:<port>/control?g=<gm_token>`. The key is generated on first start and kept in `config.toml` (`[server] gm_token`); the log only says `Remote control: …/control?g=<gm_token from config.toml>`, and the console's *GM panel link* shows the full link (token masked) with a **Show QR** button for the phone camera. It is separate from the player key: the player link (`?k=`) never opens the control page. The *New key* button next to the GM link issues a new one; phones using the old one are disconnected. **Remote control is therefore on by default**: anyone who has the `?g=` link can press the GM's buttons. `gm_token = ""` turns remote access off, and the page then answers on this PC only.

What the page does: scenes, stingers, *Stop scene*, *Stop all* (with fade), master and bus volume, *Duck*, and per-sound play/stop, loop and volume, over a WebSocket (`/ws/control`) that sends the full state after every change, so several devices stay in sync. The same actions exist as a REST API under `/api/control/…`.

Limits of headless mode today: no hotkeys, no playlist control (the page's Music block is disabled and a scene's music layer is not started because playlists are not loaded into the headless control core), and the library is the one saved in `config.toml` — edit it in the console, then restart the server.

Started from Explorer or a shortcut, the server opens a console window with its log; close it or press Ctrl+C to stop.

### Hotkeys

Open the **Hotkeys** dialog in the MIX section of the network column. Bindings are stored as `[[hotkeys]]` (see [Configuration](#configuration)).

- A key is modifiers and one key joined by `+`, case-insensitive, any order: `Ctrl+Shift+1`, `alt+f5`, `Space`, `Numpad1`, `Cmd+K`. Modifiers: `Ctrl`, `Alt` (`Option`), `Shift`, `Cmd` (macOS Command, the Windows/Super key elsewhere). Keys: `A`–`Z`, `0`–`9`, `F1`–`F24`, keypad keys, `Space`, `Enter`, `Escape`, arrows, `Home`/`End`/`PageUp`/`PageDown`, punctuation names.
- Actions: `sound`, `scene`, `playlist` (with a `target` id), `stop_all`, `stop_scene`, `master_up`, `master_down`, `master_mute`, `duck_toggle`, `duck_hold`, `next_track`, `prev_track`, `pause_music`. A binding to a stinger fires it as a one-shot; to another sound it toggles it.
- **Window shortcuts** work while the console is focused. **System-wide** shortcuts need `gui.global_hotkeys = true` and only cover keys with Ctrl, Alt or Cmd (and F-keys), so a bare `A` is never stolen from other programs. If the OS refuses one shortcut (taken by another app), the others still work. On Linux this means X11 only. On macOS the dialog explains the permission to grant if the OS asks for one.
- **MIDI** (`Midi:Ch1:Note60`, `Midi:CC7`) exists as an opt-in cargo feature (`cargo build --features midi`) but is **not connected to the console**: MIDI bindings are listed as unavailable in every build. See [Not verified](#not-verified-on-real-hardware).

### Using it at the table

1. Import your sounds and give them **tags** or **groups** (tavern, dungeon, combat…). Mark short hits as **stingers**; put music on the `music` bus.
2. Build a **scene** per place or mood: the looping pads with low levels, an emitter or two for the random life (crows, dripping), a playlist as the music layer. The fastest way: play a mix by hand and press **Save mix as scene**.
3. Add a hotkey or two for the scenes and stingers you use every session.
4. Start the server before the session and send the link. Players open it on their phones and laptops.
5. Switch scenes with a click (the transition does the crossfade), fire stingers, press **DUCK** under a monologue.

Because players hear the **mix** and not the files, you can edit, replace or re-level anything mid-session and nobody sees a download.

### Access key

Every link the console copies ends with `/?k=<key>`: the stream (and `/api/status`) refuse connections without it, so an open port does not mean anyone on the internet can listen. The key is generated on first start and kept in `config.toml` (`[server] access_token`). The ⟳ button next to **Access key** in the SERVER panel issues a new one: the old link stops working at once and everyone who joined with it is disconnected — share the new link afterwards. `access_token = ""` turns the check off.

## Configuration

`config.toml` lives in the [data directory](#where-the-data-lives) (next to the executable in portable mode) and is created and updated by the app. Everything is optional; missing keys fall back to the defaults below. Old files keep loading: every new key has a default.

```toml
[server]
host = "0.0.0.0"   # bind address; "0.0.0.0" = all IPv4 interfaces
port = 8080
upnp = true        # ask the router to forward the port for players outside the LAN
access_token = "…" # key players need in the link (/?k=…); generated on first start, "" = open
gm_token = "…"     # key for the control page from another device (/control?g=…); generated on first start, "" = this PC only

[audio]
buffer_size = 960   # device buffer in frames (960 = 20 ms); 0 = driver default
sample_rate = 48000 # device rate to ask for; the mix and the stream stay 48 kHz
channels = 2        # device channels: 1 = mono downmix, 2 = stereo
master_volume = 0.8
normalize = false           # measure new sounds (EBU R128) and store a fixed gain_db for each
normalize_target_lufs = -18.0
duck_amount = 0.3           # level the ducked buses go down to (0.3 ~ -10 dB, 1.0 = no ducking)
duck_attack_ms = 200        # full level -> duck_amount
duck_release_ms = 800       # duck_amount -> full level
duck_buses = ["music", "ambience"]  # which buses duck; "sfx" stays untouched by default
stream_threshold_secs = 60  # files longer than this are streamed from disk; 0 = never
# output_device = "USB DAC"      # sound card for the mix; absent = system default (set it in AUDIO OUTPUT)
# preview_device = "Headphones"  # separate card for sound previews; absent = the main output

[logging]
level = "info"     # trace | debug | info | warn | error
file_logging = false # true: also write bardwave.log into the data directory

[gui]
reduce_motion = false    # stop the pulsing dot and backdrop animation
global_hotkeys = false   # let [[hotkeys]] fire while the window is not focused

[library]              # managed by the app — your sounds, groups, scenes, playlists
```

The `[library]` tables are written by the console, but are plain TOML and can be edited by hand (close the app first). Ids are UUID strings; scenes, playlists and hotkeys point at sound ids, not paths.

```toml
[[library.groups]]
id = "…"
name = "Tavern"

[[library.sounds]]
id = "…"                 # stable id, generated for old configs
path = "/home/me/sounds/fire.ogg"
name = "Fireplace"
volume = 0.5
looped = true
group = "…"              # group id, optional
tags = ["tavern", "warm"]
gain_db = 0.0            # fixed correction written by normalization
bus = "ambience"         # music | ambience | sfx
oneshot_stinger = false  # true: one-shot over everything, independent of scenes

[[library.scenes]]
id = "…"
name = "Tavern"
color = "#a04a4a"        # optional button colour
fade_in_ms = 2000
fade_out_ms = 2000
curve = "equal_power"    # equal_power | linear
stop_others = true       # fade out sounds that are not in this scene
playlist = "…"           # optional playlist id: the scene's music layer

[[library.scenes.items]]
sound = "…"              # sound id
volume = 0.4             # level inside this scene
looped = true
start = "on_enter"       # on_enter | manual
delay_ms = 0

[[library.scenes.emitters]]
sounds = ["…", "…"]      # one is picked at random each time
min_interval_ms = 20000
max_interval_ms = 60000
volume = 0.5
volume_jitter = 0.15     # 0..1, fraction of volume
pan_jitter = 0.4         # 0 = centre, 1 = anywhere in the stereo field
max_overlap = 2

[[library.playlists]]
id = "…"
name = "Travel music"
items = ["…", "…"]       # sound ids in play order
shuffle = false
repeat = "all"           # off | all | one
crossfade_ms = 2000
bus = "music"

[[hotkeys]]              # top-level, not part of a bundle
key = "Ctrl+1"
[hotkeys.action]
kind = "scene"           # sound | scene | playlist (these three need a target) ...
target = "…"

[[hotkeys]]
key = "F5"
[hotkeys.action]
kind = "stop_all"        # ... or stop_all | stop_scene | master_up | master_down | master_mute |
                         #     duck_toggle | duck_hold | next_track | prev_track | pause_music
```

Environment variable `RUST_LOG` overrides `logging.level`. A `config.toml` that cannot be parsed is never silently replaced by defaults: the app tells you what is wrong and does not start, so the next save cannot overwrite your library.

## Security and tokens

Bardwave has two separate secrets in `config.toml`. Both are random 16-character strings generated on first start.

| | Player key `access_token` | GM key `gm_token` |
|---|---|---|
| Link | `http://<address>:<port>/?k=<key>` | `http://<address>:<port>/control?g=<key>` |
| Opens | the stream and `/api/status` | the phone panel (`/control`, `/api/control/*`, `/ws/control`) |
| Give to | players | nobody but yourself |
| Empty (`""`) | **opens the stream to anyone who can reach the port** | **turns remote control off**; the panel then answers on this PC only |
| Replace | ⟳ next to *Access key*: old link stops, listeners are disconnected | *New key* next to the GM link: phones using the old one are disconnected |

- The player key never opens the GM panel, and the GM key is not a player key.
- Token comparison is constant-time. After 10 failed GM attempts within a minute an address gets `429` and is locked out (even a right token is refused meanwhile); a wrong, missing or disabled token all look the same (`401`).
- A request from this PC needs no token — but "this PC" is decided from the socket address, never from `X-Forwarded-For`; a request carrying reverse-proxy headers or a non-loopback `Host` is treated as remote and needs the token. Local actions also need an `X-Bardwave-Control` header, and a local WebSocket handshake with a foreign `Origin` is refused, so a web page open in your browser cannot press your buttons.
- **No HTTPS.** Everything, tokens included, is plain HTTP/WebSocket. That is fine on your LAN. Over the internet a `?k=` or `?g=` link can be sniffed on the way: do not hand the GM link to anything outside your network, and prefer a VPN or a trusted tunnel for remote GM access. A plain TCP tunnel (`ssh -R`, bore, frp…) or a proxy that rewrites `Host` to `127.0.0.1` cannot be told from your own browser, so with such a tunnel keep a `gm_token` set and point it at the listener port only.
- Both keys are kept out of the request log (only the method and path are logged), and the headless log prints `…?g=<gm_token from config.toml>` instead of the key.
- `config.toml` holds the keys in plain text. It is yours to protect.

## How it works

```
┌─────────────────────────── Bardwave ───────────────────────────┐
│                                                                │
│  egui console ──AudioCommand──┐                                │
│  hotkeys, GM panel ───────────┴▶ Director ──Playback──▶ audio engine
│  (--server-only: ControlAction)  scenes, emitters,      ├─ decoder / stream
│                                  playlists, ducking     ├─ mixer: buses, pan,
│                                                         │   fades, duck, limiter
│                                                         └─ cpal ─▶ speakers
│                                                         │
│                                                         │ MixedOutput
│                                                         ▼
│                                       broadcaster (Opus, tokio broadcast)
│                                                         │
│                                           axum HTTP + WebSocket
└─────────────────────────────────────────────────────────┼──────┘
                                                          │ ws://<lan-ip>:8080/ws/audio?k=<key>
                                         ┌────────────────┼────────────────┐
                                         ▼                ▼                ▼
                                    🧝 browser        🧙 browser       🧟 browser
                                    (Web Audio API ring buffer → speakers)
```

- `src/audio` — decoding (in memory or streamed), mixing, resampling, device output, the `Director` (scenes, crossfade, stingers), emitters, playlists, ducking, loudness, YouTube import.
- `src/network` — axum server, WebSocket sessions, broadcaster and Opus encoder, embedded listener page and PWA assets, local-IP detection, UPnP, access keys, the GM panel (`control.rs` and its page).
- `src/gui` — the console: theme, sound list, groups, scenes, music, hotkeys dialog, library UI, server panel, MIX section.
- `src/hotkeys*` — key parsing, conflicts, dispatch, window and system-wide shortcuts, optional MIDI.
- `src/library_*` — folder scan, composite search, bundles. `src/config.rs` — `config.toml` schema. `src/paths.rs` — data directory.

[Agent.md](Agent.md) describes the modules and the control flow in more detail.

### Opus wire format

The broadcaster encodes the 48 kHz / stereo f32 mix to Opus before sending it over WebSocket: 20 ms frames (960 samples per channel), ~64 kbit/s VBR, instead of raw PCM (~7,700 bytes per frame), which prevents jitter-buffer overruns on weak Wi-Fi links. Whatever block size the sound card delivers is re-blocked into whole 20 ms frames.

**Wire frame (binary WebSocket message):**

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 bytes | `seq_no` — u32 little-endian, monotonically increasing per frame |
| 4 | 4 bytes | `server_ms` — u32 little-endian, stream time since broadcaster start |
| 8 | variable | Opus packet (one 20 ms frame, 960 samples per channel) |

**Format announcement (text WebSocket message, sent on the first frame and replayed to listeners that join later):**

```json
{"type":"format","data":{"sample_rate":48000,"channels":2,"format":"opus","frame_size":960}}
```

Other text messages: `welcome` on connect, `ping`/`pong` heartbeats, and `now_playing` (understood by the page, not yet sent by the server). Clients must ignore types they do not know.

**Client decode:** The browser decodes each Opus packet with the bundled `opus-decoder` WASM module, which returns per-channel `Float32Array`s. The listener interleaves them into a stereo buffer before pushing to the Web Audio ring buffer.

**Build requirement:** Opus is a pure-Rust implementation (`rusty-opus`, RFC 6716) with no C code, no FFI, and no build-time toolchain. `cargo build` works without CMake; Opus is always compiled in.

## Building & testing

```bash
cargo fmt --all --check
cargo clippy --all-targets --locked
cargo test --all-targets --locked    # ~780 tests
cargo clippy --all-targets --locked --features midi   # keeps the optional MIDI module compiling
cargo build --release
```

### Packaging

| Platform | How | Where |
|---|---|---|
| Windows | Inno Setup script (Program Files, Start menu, optional firewall rule for TCP 8080, never touches user data) | [`packaging/windows/bardwave.iss`](packaging/windows/bardwave.iss) |
| Linux | AppImage script and `cargo-deb` metadata in `Cargo.toml` | [`packaging/linux/`](packaging/linux/) |
| macOS | `.app`, universal binary, `.dmg`, optional signing and notarization | [`packaging/macos/`](packaging/macos/README.md) |
| all | plain-text quick start and a commented example config | [`packaging/README.txt`](packaging/README.txt), [`packaging/config.toml`](packaging/config.toml) |

The workflows are `release.yml` (tests, zip and tar.gz, tag and release), `release-extras.yml` (Windows installer, AppImage, `.deb`, `SHA256SUMS`) and `release-macos.yml` (universal `.dmg`).

### Releasing

Bump `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock` picks up the new version, and commit both. The **Release** workflow checks the version on every push to `main` (including merged pull requests; open pull requests only run CI):

- tag `v<version>` already exists: nothing to do;
- new version: tests, builds Windows and Linux, creates tag `v<version>` and publishes a GitHub release with `Bardwave-v<version>-windows-x86_64.zip` and `Bardwave-v<version>-linux-x86_64.tar.gz`; **Release extras** then adds the installers and checksums. Versions with a suffix (`1.2.0-beta.1`) are marked as pre-releases.

A release created by `release.yml` with the built-in token does not fire the `release: published` event that **Release macOS** listens for, so run it by hand (Actions → **Release macOS** → tag) to attach the `.dmg`. To retry a failed release, run **Release** by hand with the same version.

Pushing a version bump to `main` therefore publishes a release: it is a decision for the owner, and the version in `Cargo.toml` has deliberately not been changed together with the documentation.

## Not verified on real hardware

This is an honest list of what the code and the CI cover on paper, but nobody has run on a real machine yet:

- **Windows installer** (`…-setup.exe`) and **Linux packages** (AppImage, `.deb`): built and smoke-tested by workflows (silent install/uninstall, library resolution), never installed and used on a real Windows or Linux desktop; the workflows themselves have not been confirmed by an actual release run.
- **macOS**: the `.app`/`.dmg` scripts, the universal build and the local-network prompt have not been checked on a Mac; signing and notarization have never been run (they need a paid Apple Developer account). **Global (system-wide) hotkeys on macOS** are unverified: the registration is main-thread-bound and the permission the OS may ask for is only described, not tested.
- **MIDI**: the module decodes MIDI bytes and is covered by unit tests, and CI only checks that it compiles. It is off by default, not connected to the console, and has never seen a real controller.
- Global hotkeys on Windows and Linux/X11, the data-directory choice on each OS, UPnP against different routers, and the listener page on iOS Safari and Android Chrome (Wake Lock, Media Session, home-screen install) are likewise only covered by unit tests and code review, not by hands-on sessions.

If you try any of these, a bug report with the OS version is very welcome.

## Roadmap

Honest list of what is **not** done yet:

- [x] Cross-platform local-IP detection (Windows, macOS/Linux via `getifaddrs`)
- [x] Crossfade between scenes
- [x] Scenes, stingers, emitters, playlists with streaming of long files
- [x] Hotkeys (window and system-wide), phone GM panel with its own token, QR link
- [x] Tags, search, folder import, bundles, ducking and normalization, PWA listener page
- [x] Packaging scripts and workflows: Windows installer, AppImage and `.deb`, macOS `.app`/`.dmg`
- [ ] Verify the installers, the macOS app, global hotkeys on macOS and MIDI on real hardware (above)
- [ ] Serve the GM panel from the console window too, not only with `--server-only`; load playlists into the headless control core so the phone's Music block works
- [ ] Wire the MIDI module into the console (devices, learn mode)
- [ ] Send `now_playing` (scene / track name) to the listener page
- [ ] HTTPS or another encrypted path for remote GM access
- [ ] Update check through the GitHub API, screenshots and a short demo GIF in this README, a FAQ (firewall, VPN, UPnP)
- [ ] Code-signed Windows build and a signed/notarized macOS build (needs certificates)

Ideas, bug reports and pull requests are welcome.

## Contributing

- Open an issue describing the bug or the feature before a large PR.
- Keep the console buildable and the tests green (`cargo test --all-targets`).
- Match the existing style: typed commands over shared state, no blocking work on the render thread, no work in the audio callback beyond mixing.

## License

[MIT](LICENSE) © 2026 adamantida

Bardwave is not affiliated with, endorsed by, or sponsored by Syrinscape, RPG Sounds, or Wizards of the Coast. Those names are mentioned only to describe what this project is an alternative to. D&D, Pathfinder and other game systems are trademarks of their respective owners.
