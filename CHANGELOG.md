# Changelog

All notable changes to Bardwave are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

The version in `Cargo.toml` is **1.5.0**. Pushing it to `main` makes the **Release** workflow publish the release for that version. The baseline for this changelog is v1.4.0 (the commit `cargo 1.4.0`).

## [Unreleased]

Nothing yet.

## [1.5.0] - 2026-11-19

### Not verified on real hardware

Read this before relying on the items below. They are implemented and covered by unit/integration tests and by CI builds, but have **not been run on real machines**:

- **Windows installer** (Inno Setup) and **Linux packages** (AppImage, `.deb`): CI smoke tests only; the release workflows have not been confirmed by a real release run, and nobody has installed and used the results on a real Windows or Linux desktop.
- **macOS**: the `.app` / `.dmg` scripts, the universal binary and the local-network prompt are unchecked on a Mac; signing and notarization have never been run. **Global (system-wide) hotkeys on macOS** are unverified.
- **MIDI**: the module is unit-tested on raw bytes and only compiled in CI (`--features midi`); it is off by default, **not connected to the console**, and has never been used with a controller.
- **Audio devices:** choosing the output and the headphone device was run on macOS only; names and enumeration on Windows (WASAPI) and Linux (ALSA/JACK) are unchecked.
- **HTTPS:** tested by automated handshake and renewal tests only; a self-signed certificate has not been trusted on a real iPhone or Android phone, and a custom certificate has not been tried with a real Let's Encrypt chain.
- **Update check:** never run against a published release (none is published from this branch). **Profiles, the first-run wizard, the interface scale and the WAV recorder** were run on macOS during development, not on Windows or Linux; the recorder's free-disk check is Unix only.
- Also only tested by code and unit tests, not in live sessions: global hotkeys on Windows and Linux/X11, the data-directory choice on every OS, UPnP against real routers, the listener page on iOS Safari and Android Chrome (Wake Lock, Media Session, home-screen install).

### Not done

- **No screenshots in the README.** They could not be captured in the environment where the documentation was written (no screen-recording permission), so none was added rather than one showing the wrong thing.
- **No light theme.** The palette is constants in `src/gui/theme.rs`; a light theme needs a separate rework.
- **No separate bus volume for players.** The listeners get a single mixed stream, so per-bus faders for players are impossible without separate streams; only the GM's faders exist.
- MIDI is still not wired into the console (see above).

### Known limitations

- In `--server-only` mode hotkeys and session timers (`[[timers]]`) do not run.
- HTTPS is **off by default** (`server.tls`): until it is turned on, the player key (`?k=`) and the GM key (`?g=`) travel in plain HTTP. Use a LAN, HTTPS, a VPN or a trusted tunnel for anything beyond that.
- Loudness normalization skips streamed (long) files; a positive `gain_db` is audible only while the sound's own volume is below 1.0.

### Added

**Audio devices**
- `[audio] output_device` and `preview_device` choose the sound card for the mix and a separate one (headphones) for previews; absent keeps the system default / the main output. The AUDIO OUTPUT section of the console switches them without restarting: the streams reopen, tracks keep playing, a missing device falls back to the default with a message. Previews still never reach the players' stream.
- Previews have their own fixed-gain bus (the Ambience fader and ducking no longer affect them), and the local limiter now recovers gradually when the last preview ends instead of snapping back.
- Not verified: device names and enumeration on Windows (WASAPI) and Linux (ALSA/JACK); a second output stream was run on macOS only.

**Session recording**
- REC button in the top bar saves the mix the players hear as a WAV file (48 kHz stereo, 16-bit PCM, or 32-bit float with `[gui] record_float32 = true`), with a time and size readout. Files are named `bardwave-YYYYMMDD-HHMMSS.wav` (UTC) in `recordings/` of the data directory, or in the folder chosen with the folder button (`[gui] recordings_dir`).
- The WAV header is refreshed every second, so a file left by a crash is still playable; long sessions continue in `_part2.wav`, ... near 1.8 GB (a size warning appears at 1.5 GB); starting is refused when the disk is known to be almost full; frames lost to a slow disk are counted and reported. Unix only for the free-space check; not verified on Windows.

**Profiles**
- Several campaigns with their own config and library: `--profile <name>` and a Profile menu in the top bar (switch by restart, new with optional library copy, rename, delete with typed confirmation, open data folder). The default profile is the existing root config, nothing is migrated; recent profiles are kept in `recent.toml`.

**Scenes and transitions**
- Scenes (`[[library.scenes]]`): named sets of sounds with per-item volume, loop, automatic/manual start and start delay, a button colour, `fade_in_ms` / `fade_out_ms`, `stop_others`.
- Pan controls: a centred slider (notch at zero, double click resets, tooltip like `L 40%`) in the right-click menu of a sound and in every row of the scene editor. `pan` (`-1.0..=1.0`, default `0`, not written when centred) is stored per library sound and per scene item; a non-zero item value overrides the sound's pan inside that scene. The Director applies it when a sound starts and live while it plays.
- Crossfade between scenes with equal-power or linear curves; re-activating a scene mid-transition continues from the current levels. The mixer fades are now parametrised (previously a fixed 10 ms click guard).
- "Save current mix as scene", scene editor, scene deck with a shared transition control (*Auto* = each scene's own fades), *Stop scene* and *Stop all* with a fade.
- Stingers: one-shot sounds layered over everything, independent of the active scene (`oneshot_stinger`), with pads in the console.
- The `Director`: one control core used by the console, the headless server, hotkeys and the phone panel.
- Stable sound ids (UUIDs); scenes, playlists and hotkeys refer to ids, not paths. Configs from v1.4.0 load unchanged and get ids on first load.

**Timers**
- Session timers (`[[timers]]`: `id`, `name`, `trigger = { after_ms | every_ms }`, `action`, `enabled`): "in ten minutes activate the Alarm scene", "every half hour play the bell stinger". Actions: activate scene, stop scene, play stinger, stop all with a fade, start/stop playlist, next track, pause/resume music. Only the definitions are saved; timers always start stopped.
- Timers dialog (button under the mix controls): countdowns, Start / Pause timers / Reset, the nearest timer highlighted, a log of what fired and of timers that pointed at a deleted scene, sound or playlist (a warning, never an error).

**Emitters, music, mix**
- Emitters inside scenes: random or periodic sounds with a random pick from a set, volume and pan jitter, maximum overlap, seeded random generator; the deck shows when the next sound is due. Per-sound stereo pan in the mixer.
- Playlists (`[[library.playlists]]`): shuffle without repeats, repeat off/all/one, crossfade between tracks, seek, pause; a scene can use a playlist as its music layer. Transport bar and playlist manager in the console. Track names from file tags.
- Streaming decoder for long files (`[audio] stream_threshold_secs`, default 60 s): ring buffer fed by a worker thread, so an hour-long track no longer takes ~1.4 GB of RAM. Seeking and looping supported.
- Buses (`music`, `ambience`, `sfx`) with faders, and ducking (`[audio] duck_amount`, `duck_attack_ms`, `duck_release_ms`, `duck_buses`) from a DUCK button (click to latch, hold to duck while pressed), hotkeys and the phone panel.
- Loudness normalization (EBU R128, `[audio] normalize`, `normalize_target_lufs`, per-sound `gain_db`): off by default, measured in the background.

**HTTPS**
- `[server] tls = true` serves HTTPS and `wss://` on the same port (rustls); links the console copies start with `https://`. Without `tls_cert` / `tls_key` a self-signed certificate is generated into `tls/` of the data directory (397 days, covers the LAN addresses, renewed on expiry or on a network change, key mode 0600 on Unix); `tls_cert` and `tls_key` point at your own PEM files. The SHA-256 fingerprint is shown in the console (click to copy) and logged, with a "New certificate" button. A switch in the console is applied on the next Start. Off by default.

**Update check**
- *Check for updates* button asks the GitHub latest-release document and shows a link; nothing is downloaded or installed. `[gui] check_updates = true` repeats the check once a day at start. **Off by default**: a stock install makes no network request on its own.

**Console**
- First-run *Getting started* wizard (3 steps ticked by the real state of the app, a non-blocking card, reopened from the Help menu, `gui.onboarding_done`).
- Interface scale: the *Interface* slider in the MIX section and `[gui] ui_scale` (0.8 to 1.6, applied live).
- **Stop all** is the new name of the *Panic* button. It stops sounds, the active scene (delayed starts, emitters), the music layer and the headphone preview, whether it is pressed, bound to a hotkey, fired by a timer or reached through *Fade Out*.
- Spacing scale, empty states, a scene warning badge, and dialogs that fit the minimum window.

**Listener page**
- `now_playing`: the server sends the active scene and track name on connect and on every change (throttled) and the page shows them.

**Game-master panel in the console**
- The console window serves `/control` while the server is started (previously only `--server-only` did), through `gui::gm_bridge`; the phone panel also got the music playlist block and the real bus levels and ducking state, in both modes. Hotkeys and timers still run in the console only.

**Hotkeys**
- `[[hotkeys]]` bindings for sounds, scenes, playlists, stop, master volume, mute, duck (toggle/hold), next/previous track, pause music; a dialog with key capture and conflict detection.
- Window shortcuts, and system-wide shortcuts through `global-hotkey` behind `[gui] global_hotkeys` (off by default; X11 only on Linux).
- Optional MIDI module behind the `midi` cargo feature (off by default, not connected to the console, see above).

**Library**
- Tags, composite search (group + tags + text, Unicode case-insensitive), tag chips.
- Folder import: recursive scan, one group per first-level sub-folder on request, duplicates skipped by path and by content.
- Missing files are shown instead of silently dropped, with relink and a bulk "replace path prefix".
- Bundles: export/import the library as a `.zip` (`bundle.toml`, optional audio) with skip / rename / replace conflict policies, a dry run, and ids preserved on import.
- Local preview (headphones button): plays on this computer only, never into the stream.

**Game-master panel and security**
- Phone-friendly GM page (`/control`) with scenes, stingers, faders, buses, duck, *Stop all*, live state over `/ws/control` shared between devices; REST API under `/api/control/*`.
- A separate token `[server] gm_token`, generated on first start, with a link and a QR code in the console and a *New key* button. `gm_token = ""` turns remote access off (the page then answers on this PC only). The player key never opens the panel.
- Rate limit (10 failed attempts per minute per address, then `429`), constant-time comparison, `X-Bardwave-Control` header and `Origin` check for token-less local actions, a loopback `Host` required for token-less access (DNS rebinding), reverse-proxy headers (`Forwarded`, `X-Forwarded-*`, `Via`, …) make a request count as remote. Tokens are kept out of the request log.

**Listener page**
- Reconnect with exponential back-off and jitter, link-quality readout (latency, buffer, lost packets), Wake Lock, Media Session controls, per-player volume remembered in `localStorage`, an installable PWA (manifest, icons, Apple touch icon), handler for `now_playing` messages.

**Platforms and packaging**
- Data directory per OS (`src/paths.rs`): the executable's folder in portable mode (`config.toml` or `portable.flag` next to it, or a writable folder), otherwise `~/Library/Application Support/Bardwave` (macOS), `%APPDATA%\Bardwave` (Windows) or `$XDG_DATA_HOME/bardwave` / `~/.local/share/bardwave` (Linux). `config.toml`, the YouTube library, downloaded tools and the log all live there.
- LAN address detection filters VPN/TUN, link-local, benchmarking and loopback ranges.
- macOS: scripts for `Bardwave.app` (universal binary, `NSLocalNetworkUsageDescription`), `.dmg`, and optional Developer ID signing and notarization; a release workflow for the `.dmg`.
- Windows: Inno Setup installer with an optional firewall rule for TCP 8080 that leaves user data alone on uninstall.
- Linux: AppImage script and a `.deb` through `cargo-deb`.
- `release-extras.yml` attaches the installers and `SHA256SUMS` to a release; CI now also runs on macOS and checks that the `midi` feature compiles.

### Changed

- The GM control page and its API are no longer limited to this PC: with the generated `gm_token` they open from other devices through `?g=<gm_token>`. **Remote control is on by default**; set `gm_token = ""` to keep the old behaviour.
- `config.toml` and the other user files are looked up in the data directory (see above) instead of always next to the executable. Existing installs with a `config.toml` next to the executable keep working unchanged; on Windows a `config.toml` in a folder the user cannot write to (left by an elevated run in `Program Files`) no longer pins the data directory.

### Fixed

- Panned tracks start at their target pan instead of sliding in from the centre.
- Hotkey map and system-wide shortcuts are resynchronised when deleting sounds, scenes or playlists prunes bindings.
- Long files are streamed by the console decoder instead of being decoded into RAM first; adding many files no longer scans the pending list linearly for every file.
- A MIDI binding is consistently reported as unavailable until the module is wired in.
- Leftover backup files and a stale patch were removed from the repository.

### Documentation

- README and `Agent.md` brought up to date with Stop all, timers, pan, audio devices and the headphone output, session recording, profiles (`--profile`), HTTPS and the fingerprint, the opt-in update check, the first-run wizard, `gui.ui_scale`, the master panel in the windowed mode with playlists and buses, `now_playing`, and a `config.toml` reference with examples; the lists of what is not done and not verified are in this file and in the README.
- README rewritten from the code: features, configuration with the new sections, data directory per OS, macOS notes, packaging, token security, roadmap, and the list of what is not verified on real hardware. `Agent.md` describes the modules and the control flow; this changelog is new.
- Corrected two stale numbers: the stream uses **20 ms** Opus frames (960 samples per channel) and the default `buffer_size` is **960**, not 10 ms / 480 (the code already had it at v1.4.0).

## [1.4.0]

The baseline of this changelog: egui console with sound groups, per-sound volume and loop, master volume, YouTube import, Opus streaming over WebSocket to an embedded listener page, listener access key (`access_token`), per-player volume, UPnP port forwarding, a `--server-only` mode with a control page limited to this PC, and Windows and Linux archives from the release workflow. Earlier history is in the git log.
