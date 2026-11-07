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
[![Platform](https://img.shields.io/badge/platform-Windows-3b3b3b.svg?style=flat-square&logo=windows&logoColor=white)](#requirements)
[![PRs welcome](https://img.shields.io/badge/PRs-welcome-a04a4a.svg?style=flat-square)](#contributing)

[Why](#why) · [Features](#features) · [Getting started](#getting-started) · [Configuration](#configuration) · [How it works](#how-it-works) · [Roadmap](#roadmap)

</div>

---

> A tavern taproom. A rain-soaked alley at midnight. The low drone before the dragon wakes.
> You hit play. Your four players, on four different laptops, hear exactly what you hear — no Discord screen-share, no file sharing, no "wait, send me that MP3 again".

## Why

Running ambience in a tabletop game is still surprisingly awkward. The paid options tie you to a subscription and their own sound packs. The free options make you share files, run a second voice channel, or host something in the cloud you do not control.

**Bardwave** takes the opposite approach:

- your sound library stays **on your machine**, in whatever format you already own (MP3, FLAC, OGG, WAV, …);
- the mix is built **live** on your desktop, not pre-rendered;
- players need **nothing but a browser** — they open one URL and press Connect;
- *you* stay in control of every fader while the scene plays.

## Features

<table>
<tr><td width="50%" valign="top">

**Console**
- Desktop soundboard built with [egui](https://github.com/emilk/egui) — native, single binary, no runtime
- Sound **groups** (tavern, dungeon, combat, …) with live filtering
- **Per-sound volume**, master volume, and a live level meter
- **Loop** toggle per sound for pads and drones
- Drag-free dialogs for new/rename/delete group and moving sounds between groups
- Player/stopped state per sound, with duration readout
- Reduced-motion mode for the decorative effects

</td><td width="50%" valign="top">

**Streaming**
- WebSocket broadcast of the **final mix** — players hear what the GM hears
- Low-latency: small mixing buffers, compact binary frames
- Browser listener page is **embedded in the binary**, so there is no static folder to ship
- **Live listener counter** in the console
- **Access key** in the shared link, and a **per-player volume** slider on the listener page
- Automatic local address detection that ignores VPN/TUN adapters
- Graceful start/stop that does not leave the server thread hung

</td></tr>
<tr><td valign="top">

**Library**
- WAV, FLAC, MP3, OGG-Vorbis and AAC/MP4 decoding via [Symphonia](https://github.com/pdeljanov/Symphonia)
- Library, groups, per-sound volume and loop flags **persist across restarts**
- Everything stored in a plain, human-readable `config.toml` next to the executable

</td><td valign="top">

**Engineering**
- Rust, one binary, one config file
- 150+ tests (`cargo test --all-targets`), including end-to-end broadcast and WebSocket wire-format tests
- Typed commands between GUI, audio engine and network — no shared mutable soup
- Streaming resampled to 48 kHz / stereo f32 in the mix pipeline, encoded to **Opus** (~64 kbit/s VBR) for the network wire

</td></tr>
</table>

## Getting started

### Requirements

- **Windows 10/11** (the audio and network code is portable Rust; only the local-IP detection is Windows-specific today — see [Roadmap](#roadmap)).
- [Rust toolchain](https://rustup.rs) 1.85 or newer.
- A sound card. No, really — this is the point of the app.

### Build & run

```bash
git clone https://github.com/adamantida/Bardwave.git
cd Bardwave
cargo run --release
```

The console window opens. Click **Add sound** (or the ⨁ button) and pick a few audio files from your library, then press **Start** in the SERVER panel.

> Built binary, if you do not want a terminal hanging around: `cargo build --release` puts `target/release/soundshared.exe` — rename it to `Bardwave.exe` if you like. Keep `config.toml` in the **same folder** as the executable.

### Connect your players

The SERVER panel shows two addresses:

| Address | Who uses it |
|---|---|
| `http://192.168.x.x:8080` | **Your players.** Open it on any device in the same network. |
| `http://127.0.0.1:8080` | **You**, to test the listener page on your own machine. |
| `http://<global-ip>:8080` | **Remote players**, over the internet. Shown as INTERNET once the router forwards the port. |

#### Players outside your network

On **Start** the console asks your router to forward the port to this PC (UPnP IGD) and shows the resulting `INTERNET` address (click to copy). Discovery is sent from every LAN address of the PC, so it also works while a VPN owns the default route. The mapping is renewed while the server runs and removed on **Stop**. Requirements and caveats:

- UPnP must be enabled on the router. If no router answers, the panel says *no UPnP router* and the LAN address keeps working; forward the port by hand instead.
- If the router's external address is private (carrier-grade NAT, or a provider modem in front of your router), the panel says *blocked by a second NAT*: the port is not reachable from the internet until you forward it on the upstream device or get a public IP from your provider.
- **VPN:** with a full-tunnel VPN on the GM's PC, the router forwards the player's connection, but the reply leaves through the tunnel and never arrives. The panel warns about it (*VPN active*); turn the VPN off, or exclude Bardwave from it, for internet players.
- **Windows firewall:** Windows marks many networks as *Public*, and the "allow access" prompt only opens *Private* ones by default. If the panel says *Firewall: port not opened*, allow Bardwave for public networks too (or run it once as administrator so it can add the rule itself).
- Opening the internet URL from inside your own network may fail on routers without NAT loopback. Test it from a phone on mobile data.
- On Windows the app adds an inbound firewall rule for the port when it runs as administrator; otherwise allow it when Windows asks.
- Set `upnp = false` in `config.toml` to turn this off.

The LISTEN URL is click-to-copy. Players open it, hit **Establish Link**, and the stream starts.

#### Access key

Every link the console copies ends with `/?k=<key>`: the stream (and `/api/status`) refuse connections without it, so an open port does not mean anyone on the internet can listen. The key is generated on first start and kept in `config.toml` (`[server] access_token`). The ⟳ button next to **Access key** in the SERVER panel issues a new one: the old link stops working at once and everyone who joined with it is disconnected — share the new link afterwards. `access_token = ""` turns the check off.

### Headless

```bash
soundshared.exe --server-only
```

Runs without the console window, for a spare PC next to the router or a session you drive from a phone-sized screen. The saved library (`[library]` in `config.toml`, i.e. whatever you set up in the console) is loaded at start, the listener server starts at once, and the log shows both links:

- the **player link** (with the access key) to share, as in the console;
- a **control page**, `http://127.0.0.1:<port>/control`: play/stop, repeat and volume for every sound, master volume and *Stop all*.

The control page only answers on the machine that runs Bardwave (players reach the same port over the network and must not get the GM's buttons). Started from Explorer or a shortcut, the server opens a console window with its log; close it or press Ctrl+C to stop.

## Configuration

`config.toml` lives **next to the executable** and is created by the app when settings change. Everything is optional; missing keys fall back to the defaults below.

```toml
[server]
host = "0.0.0.0"   # bind address; "0.0.0.0" = all IPv4 interfaces
port = 8080
upnp = true        # ask the router to forward the port for players outside the LAN
access_token = "…" # key players need in the link (/?k=…); generated on first start, "" = open

[audio]
buffer_size = 480  # device buffer in frames (480 = 10 ms); 0 = driver default
sample_rate = 48000 # device rate to ask for; the mix and the stream stay 48 kHz
channels = 2        # device channels: 1 = mono downmix, 2 = stereo
master_volume = 0.8

[logging]
level = "info"     # trace | debug | info | warn | error
file_logging = false # true: also write bardwave.log next to the executable

[gui]
reduce_motion = false  # stop the pulsing dot and backdrop animation

[library]              # managed by the app — your sounds and groups
```

Environment variable `RUST_LOG` overrides `logging.level`.

## Using it at the table

1. Group your sounds by scene — **Crowd**, **Nature**, **Combat**, **Effects**. Grouping is by hand; a group is just a filter over the library.
2. Add looping pads (rain, fire, tavern murmur) with **loop** on and a low per-sound volume.
3. Add one-shot stingers (door slam, sword hit) with loop off at full volume.
4. Start the server before the session. Players open the URL on their phones and laptops.
5. Ride the faders live: drop the ambience under a monologue, bring it back on a scene change.

Because players hear the **mix** and not the files, you can edit, replace or re-level anything mid-session and nobody sees a download.

## How it works

```
┌─────────────────────── Bardwave.exe ────────────────────────┐
│                                                             │
│   egui console ──AudioCommand──▶  audio engine              │
│   (sounds, groups,     ◀─status─  ├─ decoder  (Symphonia)   │
│    server panel)                  ├─ mixer    (48k/stereo)  │
│                                   └─ cpal output ─▶ speakers │
│                                          │                  │
│                                          │ MixedOutput      │
│                                          ▼                  │
│                              broadcaster (tokio broadcast)  │
│                                          │                  │
│                              axum HTTP + WebSocket          │
└──────────────────────────────────────────┼──────────────────┘
                                           │ ws://<lan-ip>:8080/ws/audio?k=<key>
                          ┌────────────────┼────────────────┐
                          ▼                ▼                ▼
                     🧝 browser        🧙 browser       🧟 browser
                     (Web Audio API ring buffer → speakers)
```

- `src/audio` — decoding, mixing, resampling, device output.
- `src/network` — axum server, WebSocket sessions, broadcaster, embedded listener page, local-IP detection.
- `src/gui` — the console: theme, widgets, sound list, groups, server panel, dialogs.
- `src/config.rs` — `config.toml` schema and persistence.

### Opus wire format

The broadcaster encodes the 48 kHz / stereo f32 mix to Opus before sending it over WebSocket. This reduces the wire payload from ~3,840 bytes per 10 ms frame (raw f32 PCM) to ~100–300 bytes at 64–96 kbit/s VBR, which prevents jitter-buffer overruns on weak Wi-Fi links.

**Wire frame (binary WebSocket message):**

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 bytes | `seq_no` — u32 little-endian, monotonically increasing per frame |
| 4 | 4 bytes | `server_ms` — u32 little-endian, stream time since broadcaster start |
| 8 | variable | Opus packet (one 10 ms frame, 480 samples per channel) |

**Format announcement (text WebSocket message, sent on first frame):**

```json
{"type":"format","data":{"sample_rate":48000,"channels":2,"format":"opus","frame_size":480}}
```

**Client decode:** The browser listener decodes each Opus packet with the bundled `opus-decoder` WASM module, which returns per-channel `Float32Array`s (`channelData`). The listener interleaves them into a single stereo buffer before pushing to the Web Audio ring buffer.

**Build requirement:** Opus is a pure-Rust implementation (`rusty-opus`, RFC 6716) with no C code, no FFI, and no build-time toolchain. `cargo build` and `cargo build --release` work without CMake; Opus is always compiled in.

## Building & testing

```bash
cargo test --all-targets     # 150+ tests
cargo clippy --all-targets   # lint
cargo build --release
```

### Releasing

Bump `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock` picks up the new version, and commit both. The **Release** workflow checks the version on every push to `main` (including merged pull requests; open pull requests only run CI):

- tag `v<version>` already exists: nothing to do;
- new version: tests, builds Windows and Linux, creates tag `v<version>` and publishes a GitHub release with `Bardwave-v<version>-windows-x86_64.zip` and `Bardwave-v<version>-linux-x86_64.tar.gz`. Versions with a suffix (`1.2.0-beta.1`) are marked as pre-releases.

To retry a failed release, run it by hand: Actions → **Release** → **Run workflow** with the same version. The Linux binary needs GTK 3 and ALSA (`libgtk-3-0`, `libasound2`) installed.

## Roadmap

Honest list of what is **not** done yet — this is an early build, not a finished competitor:

- [ ] Cross-platform local-IP detection (macOS/Linux)
- [ ] Crossfade between scenes
- [ ] Packaging: an installer, a macOS build, maybe a Steam/itch release (releases already ship a portable Windows zip and a Linux tar.gz, each with a commented `config.toml`)

Ideas, bug reports and pull requests are welcome.

## Contributing

- Open an issue describing the bug or the feature before a large PR.
- Keep the console buildable and the tests green (`cargo test --all-targets`).
- Match the existing style: typed commands over shared state, no blocking work on the render thread.

## License

[MIT](LICENSE) © 2026 adamantida

Bardwave is not affiliated with, endorsed by, or sponsored by Syrinscape, RPG Sounds, or Wizards of the Coast. Those names are mentioned only to describe what this project is an alternative to. D&D, Pathfinder and other game systems are trademarks of their respective owners.
