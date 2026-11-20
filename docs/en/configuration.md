# Configuration reference

This page lists the `config.toml` keys, command-line flags and environment variables that Bardwave reads. Defaults and ranges come from `src/config.rs`, `src/paths.rs`, `src/profiles.rs`, `src/main.rs` and the library loader in `src/gui/groups.rs`. Behaviour not checked on real hardware is marked **(not verified)**.

For HTTPS setup, see [HTTPS (secure connection)](./https.md). Related pages: [Scenes, music, hotkeys and timers](./scenes-music-hotkeys.md), [Soundboard and library](./soundboard-library.md), [Network and remote control](./network-remote.md), [Audio and session tools](./audio-session-tools.md), [Troubleshooting and security](./troubleshooting-security.md).

## Contents

- [File location](#file-location)
- [File format and migration](#file-format-and-migration)
- [Command-line flags](#command-line-flags)
- [Environment variables](#environment-variables)
- [\[server\]](#server)
- [\[audio\]](#audio)
- [\[logging\]](#logging)
- [\[gui\]](#gui)
- [\[\[hotkeys\]\]](#hotkeys)
- [\[\[timers\]\]](#timers)
- [\[library\]](#library)
- [Complete example](#complete-example)

## File location

The root data directory is the executable directory when any of the first three rows applies. Otherwise it is the per-user folder:

| Situation | Directory |
|---|---|
| `portable.flag` exists next to the executable | Executable directory |
| `config.toml` exists next to the executable (Windows: only if that folder is writable) | Executable directory |
| Executable directory is writable and not inside a macOS `.app` | Executable directory |
| Otherwise, macOS | `~/Library/Application Support/Bardwave` |
| Otherwise, Windows | `%APPDATA%\Bardwave` |
| Otherwise, Linux and other Unix | `$XDG_DATA_HOME/bardwave` or `~/.local/share/bardwave` |

If the per-user folder cannot be worked out (no `HOME`, `USERPROFILE` or `APPDATA`) or cannot be created, the executable directory is used. Empty environment variables count as unset.

**Portable mode:** create an empty file named `portable.flag` next to the executable. Only its existence is checked. Installs on Windows and Linux were not checked **(not verified)**.

With `--profile <name>`, files move:

| Path (under the root) | Contents |
|---|---|
| `config.toml`, `bardwave-library/` | Default profile |
| `profiles/<name>/config.toml`, `profiles/<name>/bardwave-library/` | Named profile |
| `bardwave.log`, `tools/`, `recent.toml` | Shared by all profiles |

Recordings go to `recordings/` and self-signed HTTPS certificates to `tls/`, both in the profile's data directory. `bardwave-library/` holds audio downloaded by Bardwave; `tools/` holds helper tools it downloaded. The folder of a named profile is created on first use.

A profile name is trimmed and has 1–32 characters: letters, digits, spaces, `-`, `_`. The name `default` (any letter case) means the default profile and is refused for a regular profile. Windows device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`) are refused. Switching profiles in the console restarts Bardwave into the other profile.

## File format and migration

`config.toml` is TOML. Every key is optional. A missing key takes the default shown below. Integer keys are unsigned: a negative or too large number (for example `port = 70000`) is a parse error.

- A missing file means all defaults. The file is created on the first start.
- Unknown keys are ignored on load and dropped on save.
- A file that does not parse (or cannot be read) stops the start with exit code 1. The dialog says `config.toml could not be read`; with `--server-only` there is no dialog and the error goes to stderr. There is no fallback to defaults.

> **Warning:** Bardwave rewrites `config.toml` with its own formatting on every save, including the first start (an access key is saved then) and library edits. Comments are lost, also those of the shipped example file. Most keys are written out in full, even at their default value. Optional keys and empty lists are not written when unset: `output_device`, `preview_device`, `recordings_dir`, `tls_cert`, `tls_key`, `tags`, `pan = 0`, `ui_scale = 1.0`, `record_float32 = false`, `last_update_check = 0`, `onboarding_done = false`, and empty `[[hotkeys]]`, `[[timers]]`, scenes and playlists. Inline tables such as `action = { ... }` and `trigger = { ... }` come back as `[hotkeys.action]` and `[timers.trigger]` tables (same meaning). Floats may gain digits (`0.8` is saved as `0.800000011920929`). Keep a copy of a hand-written file.

Migration of older data:

- Sounds, scenes and playlists without an `id` get a UUID on load, saved on the next save. This is how configs of v1.4.0, which had no ids, are upgraded.
- Hand-written ids such as `"rain"` become UUIDs. References in scene items, emitters, a scene's `playlist`, playlist `items` and `[[hotkeys]]` are rewritten to match. `[[timers]]` references are not rewritten: use UUIDs there.
- A duplicate id gets a new UUID. The first holder keeps its id.
- A missing `access_token` or `gm_token` is generated and saved at start.
- Missing sections or keys of older configs (`[gui]`, `[library]`, `output_device`, `tls` and so on) take the defaults, so an old file keeps working unchanged. A config that already has sounds is marked `onboarding_done = true` at the first console start.
- Existing installs keep their data where they are: a `config.toml` next to the executable keeps the executable directory as the data directory (see [File location](#file-location)). Nothing is moved.

## Command-line flags

Other arguments are ignored. There is no `--help` or `--version`.

| Flag | Effect |
|---|---|
| `--server-only` | Runs without a window. Loads the saved library, starts the listener server and logs the listener link and the control page link (at the default `info` level). Stop with Ctrl+C. `[[hotkeys]]` and `[[timers]]` are not run in this mode. Loudness measured in the background (`normalize`) is saved to `config.toml`. |
| `--profile <name>` | Uses that profile. `--profile=<name>` also works. `default` means the default profile. The last occurrence wins. |

A `--profile` without a value, or with an invalid name, prints `Error: ...` and exits with code 2. The same happens when the profile folder cannot be created. Without `--server-only` the message is also shown in a dialog.

```bash
./bardwave --server-only
./bardwave --server-only --profile "Camp 1"
```

In `--server-only` the log shows the remote control link as `/control?g=<gm_token from config.toml>`. The key itself is never logged.

## Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG` | If set to a valid filter, replaces `[logging] level`. Uses `tracing` filter syntax. An unset or invalid value falls back to `[logging] level`. |
| `SOUNDSHARED_REDUCED_MOTION` | `1`, `true`, `yes`, `on` turn decorative motion off; `0`, `false`, `no`, `off` turn it on. Not case sensitive. Other values (including an empty one) are ignored. Overrides `[gui] reduce_motion` for one session. |
| `HOME`, `USERPROFILE`, `APPDATA`, `XDG_DATA_HOME` | Used only to pick the per-user data directory. |

## [server]

| Key | Type | Default | Valid values | Effect |
|---|---|---|---|---|
| `port` | integer | `8080` | 1–65535 | HTTP and WebSocket port. The console refuses 0. A `0` in the file is not checked **(not verified)**. |
| `host` | string | `"0.0.0.0"` | IP address | Bind address. `0.0.0.0` means all IPv4 interfaces. The console accepts IP literals only. A value in the file that is not an IP address is replaced by `0.0.0.0` at start. |
| `upnp` | boolean | `true` | | Asks the router (UPnP IGD) to forward the port. |
| `access_token` | string | generated | any; `""` = open | Key in the listener link `/?k=<key>`. Generated and saved at start if missing: 16 characters `a-z` and `0-9`. |
| `gm_token` | string | generated | any; `""` = local only | Key of the control page `/control?g=<key>`. Generated if missing. `""` keeps the page on this computer. |
| `tls` | boolean | `false` | | HTTPS and `wss://` on the same port. Applies on the next Start. |
| `tls_cert` | string (path) | none | PEM chain | Your certificate. Set with `tls_key`, or neither (then a self-signed one is made in `tls/`). An empty string counts as unset. |
| `tls_key` | string (path) | none | PEM key | Private key for `tls_cert`. |

In the console, `port` (PORT) and `host` (BIND ADDRESS) in the CONFIGURATION block are editable only while the server is stopped. The access key is shown as ACCESS KEY in the SERVER panel, and the game-master key under GM panel link (**Create GM key**, **New key**, **Replace key**).

With `tls = true`, setting only one of `tls_cert` and `tls_key` makes Start fail with the error `tls_cert and tls_key must be set together`; the server does not start.

> **Warning:** `access_token = ""` lets anyone who reaches the port listen. Use it only on a trusted network.

> **Note:** `gm_token` is generated by default, so remote control is on. Anyone with the `?g=` link can press game-master buttons. Set `gm_token = ""` to keep the page local.

## [audio]

| Key | Type | Default | Valid values | Effect |
|---|---|---|---|---|
| `buffer_size` | integer | `960` | `0` or 64–8192 | Card buffer in frames. 960 is 20 ms at 48 kHz. `0` is the driver default. Other values warn and use 480 (10 ms). |
| `sample_rate` | integer | `48000` | 8000–192000 | Rate requested from the card. The player stream is always 48 kHz. Out of range warns and uses 48000. |
| `channels` | integer | `2` | 1–8 | Card channels. 1 is a mono downmix, more than 2 adds a centre mix on the extra channels. Out of range warns and uses 2. |
| `master_volume` | float | `0.8` | 0.0–1.0 | Volume at start. **A value outside this range stops the start with an error** (no dialog; the error goes to stderr). |
| `normalize` | boolean | `false` | | Measures loudness (EBU R128) of sounds whose `gain_db` is `0.0` and stores a correction in `gain_db`. Sounds streamed from disk are not measured. Console: **NORMALIZE LOUDNESS**. |
| `normalize_target_lufs` | float | `-18.0` | used within -60 to 0 | Normalization target. The console **Target** field offers -36 to -8 LUFS. A value that is not a number falls back to -18. |
| `duck_amount` | float | `0.3` | used within 0.0–1.0 | Linear level of ducked buses. `0.3` is about -10 dB. `1.0` disables ducking, `0.0` mutes them. Console: **Duck settings**, **Level**. |
| `duck_attack_ms` | integer | `200` | milliseconds | Fade-down time. The console allows 0–10000. |
| `duck_release_ms` | integer | `800` | milliseconds | Fade-up time. The console allows 0–30000. |
| `duck_buses` | string array | `["music", "ambience"]` | `music`, `ambience`, `sfx` | Buses lowered while ducking. Console: **Ducks** checkboxes. |
| `stream_threshold_secs` | integer | `60` | seconds; `0` = off | Longer files are read from disk while playing. |
| `output_device` | string | none | device name | Desktop output. A missing device falls back to the system default, and the console says so. Console: **AUDIO OUTPUT**, **Output**. |
| `preview_device` | string | none | device name | Device for previews only. None uses the main output. Previews never reach the players. Console: **Preview**. |

The **AUDIO OUTPUT** section applies a device change at once and saves it. Buffer size, sample rate and channels are shown there read-only. A blank device name counts as unset.

## [logging]

| Key | Type | Default | Valid values | Effect |
|---|---|---|---|---|
| `level` | string | `"info"` | `trace`, `debug`, `info`, `warn`, `error` | Log level. `RUST_LOG` overrides it. |
| `file_logging` | boolean | `false` | | Also writes `bardwave.log`, in append mode. The log always goes to the terminal too, when there is one. |

The log file is `bardwave.log` in the root directory (see [File location](#file-location)), shared by all profiles. If it cannot be opened, file logging is skipped without a message.

## [gui]

| Key | Type | Default | Valid values | Effect |
|---|---|---|---|---|
| `reduce_motion` | boolean | `false` | | Stops the live-dot pulse and backdrop scanline. Read at start. |
| `global_hotkeys` | boolean | `false` | | `[[hotkeys]]` also fire when the window is unfocused **(macOS permissions not verified)**. Console: Hotkeys dialog, **GLOBAL HOTKEYS (WHEN THE WINDOW IS NOT FOCUSED)**. |
| `recordings_dir` | string (path) | none | a folder | Folder for recordings (**Rec** button). None or blank means `recordings/` in the profile. The folder button next to **Rec** sets it. |
| `record_float32` | boolean | `false` | | 32-bit float WAV instead of 16-bit PCM (no console control; config only). |
| `check_updates` | boolean | `false` | | Checks GitHub once a day at start (when 24 hours have passed since the last check). Console: **Check once a day at start**. **Check for updates** works regardless. |
| `last_update_check` | integer | `0` | Unix seconds | Written by Bardwave. Not written when 0. |
| `ui_scale` | float | `1.0` | 0.8–1.6 | Interface scale, applied live. The **Scale** slider moves in steps of 0.05. **Reset** returns to 100 %. A value outside the range is clamped; one that is not a number is read as 1.0. |
| `onboarding_done` | boolean | `false` | | Set when **Getting started** is finished or dismissed, or when the config already has sounds. Help > **Getting started** opens it again. |

## [[hotkeys]]

Bindings sit at the top level of the file, not in `[library]`. They are used by the console window only.

- `key` (required): modifiers and one key joined by `+`, for example `Ctrl+Shift+A` or `F9`. Modifiers are `Ctrl`, `Alt`, `Shift`, `Cmd` (macOS Command, or the Windows / Super key), with the aliases `Control`, `Option`/`Opt`, `Command`/`Super`/`Meta`/`Win`. `CmdOrCtrl` means `Cmd` on macOS and `Ctrl` elsewhere. Keys: letters, digits, `F1`–`F24`, `Numpad0`–`Numpad9`, `Space`, `Enter`, `Escape`, `Tab`, `Backspace`, arrows (`Up`, `Down`, `Left`, `Right`) and similar named keys. Case and order do not matter.
- `action` (required): an inline table with `kind`, plus `target` for the first three kinds.

| `kind` | `target` | Console label | Effect |
|---|---|---|---|
| `sound` | sound id | Play sound / stinger | Plays. Stingers play once; others toggle. |
| `scene` | scene id | Activate scene | Activates the scene. |
| `playlist` | playlist id | Start playlist | Starts or restarts the playlist. |
| `stop_all` | none | Stop all | Stops everything. |
| `stop_scene` | none | Stop scene | Stops the active scene. |
| `master_up` | none | Master volume up | One step (0.05) up. |
| `master_down` | none | Master volume down | One step (0.05) down. |
| `master_mute` | none | Master mute | Mutes or unmutes. |
| `duck_toggle` | none | Duck background (toggle) | Ducks on one press, releases on the next. |
| `duck_hold` | none | Duck background (hold) | Ducks while held. |
| `next_track` | none | Next track | Next playlist track. |
| `prev_track` | none | Previous track | Previous playlist track. |
| `pause_music` | none | Pause / resume music | Pauses or resumes the music. |

> **Note:** Invalid bindings are kept and reported, not removed. If two entries have the same key, the first one wins and the other never fires. MIDI keys (`Midi:...`) never fire in this build, see [Scenes, music, hotkeys and timers](./scenes-music-hotkeys.md).

## [[timers]]

Timers are session timers. Only definitions are saved. They always start stopped and run only while the Bardwave console is open (not with `--server-only`). Console: the **Timers** button, with **Start**, **Pause timers**, **Reset** and **New timer**.

| Key | Type | Default | Effect |
|---|---|---|---|
| `id` | string | `""` | Unique id. The console makes a UUID; for a hand-written timer give each one its own, because countdowns are matched by id. |
| `name` | string | `""` | Name in the Timers dialog (up to 60 characters in the console). |
| `trigger` | inline table | `{ after_ms = 600000 }` | `after_ms = N` fires once, N ms after Start. `every_ms = N` repeats. Intervals under 100 ms cannot run. The console edits it in minutes (0–999) and seconds. |
| `action` | inline table | `{ kind = "stop_all", fade_ms = 2000 }` | What happens (below). |
| `enabled` | boolean | `true` | `false` keeps the timer unarmed. |

A timer with a trigger under 100 ms, or with an empty target, never runs.

| `kind` | Fields | Console label | Effect |
|---|---|---|---|
| `activate_scene` | `scene` | Activate scene | Switches to the scene. |
| `stop_scene` | none | Stop scene | Fades out the active scene. |
| `play_stinger` | `sound` | Play stinger | Plays the sound once over everything. |
| `stop_all` | `fade_ms` | Stop all (fade out) | Fades everything out. A `fade_ms` omitted in the file means `0`. The console offers 0–120 s. |
| `start_playlist` | `playlist` | Start playlist | Starts or restarts the playlist. |
| `stop_playlist` | none | Stop music | Fades out and stops the music. |
| `next_track` | none | Next track | Next playlist track. |
| `pause_music` | none | Pause music | Pauses the music. |
| `resume_music` | none | Resume music | Resumes the music. |

## [library]

Sounds, scenes and playlists have UUID `id`s, and references use ids, never paths. The console limits `volume` to 0.0–1.0, `pan` to -1.0–1.0 and `gain_db` to -48.0–24.0. On load, `pan`, `gain_db`, scene item `volume` and emitter levels are clamped to these ranges (a value that is not a number becomes 0). A sound's own `volume` is not clamped on load.

Repair rules when the library is loaded:

- A group is skipped if its `id` is not a UUID or repeats an earlier one, or its trimmed `name` is empty or longer than 32 characters.
- A sound is skipped if its `path` is empty or repeats an earlier sound's path. A `group` that does not exist makes the sound ungrouped.
- Scene items, emitter sounds and playlist tracks whose id is not a UUID are dropped. A UUID that matches nothing is kept and reported as a warning.
- A scene `color` that is not `#rgb` or `#rrggbb` is dropped. A scene `playlist` that is not a UUID is dropped.
- Scene and playlist names are limited to 64 characters in the console and are unique ignoring case there.

### [[library.groups]]

| Key | Type | Default | Effect |
|---|---|---|---|
| `id` | string | required | Group UUID. |
| `name` | string | required | Group name, 1–32 characters, unique ignoring case when created in the console. |

### [[library.sounds]]

| Key | Type | Default | Effect |
|---|---|---|---|
| `id` | string | generated | Sound UUID. |
| `path` | string | required | Audio file path. |
| `name` | string | `""` | Display name. Empty shows the file name. |
| `volume` | float | `1.0` | Level, 0.0–1.0. |
| `looped` | boolean | `false` | Restarts when it ends. |
| `group` | string | none | Group id. |
| `tags` | string array | `[]` | Trimmed, lower case, up to 32 characters each, no duplicates. Not written when empty. |
| `gain_db` | float | `0.0` | Fixed dB correction on top of `volume`. |
| `bus` | string | `"ambience"` | `music`, `ambience`, `sfx`. |
| `oneshot_stinger` | boolean | `false` | Plays once over everything, without stopping others. |
| `pan` | float | `0.0` | -1.0 (left) to 1.0 (right). Written only when not 0. |

### [[library.scenes]]

| Key | Type | Default | Effect |
|---|---|---|---|
| `id` | string | generated | Scene UUID. |
| `name` | string | `""` | Button label. |
| `color` | string | none | `#rrggbb` or `#rgb`. None uses the theme. |
| `fade_in_ms` | integer | `2000` | Fade-in of the scene's sounds. The console offers 0–60000. |
| `fade_out_ms` | integer | `2000` | Fade-out of sounds it stops. The console offers 0–60000. |
| `curve` | string | `"equal_power"` | `equal_power` or `linear`. |
| `stop_others` | boolean | `true` | Fades out sounds not in the scene. |
| `playlist` | string | none | Playlist id played as the music layer. |

### [[library.scenes.items]]

| Key | Type | Default | Effect |
|---|---|---|---|
| `sound` | string | required | Sound id. |
| `volume` | float | `1.0` | Level inside this scene, 0.0–1.0 (replaces the sound's own volume). |
| `looped` | boolean | `false` | Repeats while the scene is active. |
| `start` | string | `"on_enter"` | `on_enter` or `manual`. |
| `delay_ms` | integer | `0` | Delay after activation. The console offers 0–600000. |
| `pan` | float | `0.0` | Non-zero replaces the sound's pan. Written only when not 0. |

### [[library.scenes.emitters]]

| Key | Type | Default | Effect |
|---|---|---|---|
| `sounds` | string array | `[]` | Candidate sound ids. Repeats are removed. |
| `min_interval_ms` | integer | `20000` | Shortest pause. Effective minimum 100 ms. |
| `max_interval_ms` | integer | `60000` | Longest pause. Swapped with the minimum if smaller. The console offers 100 ms to 1 hour for both. |
| `volume` | float | `1.0` | Base level, 0.0–1.0. |
| `volume_jitter` | float | `0.0` | Random level spread, 0.0–1.0. |
| `pan_jitter` | float | `0.0` | Random stereo spread, 0.0–1.0. |
| `max_overlap` | integer | `2` | Simultaneous instances. Effective minimum 1. The console offers up to 16. |

### [[library.playlists]]

| Key | Type | Default | Effect |
|---|---|---|---|
| `id` | string | generated | Playlist UUID. |
| `name` | string | `""` | Display name. |
| `items` | string array | `[]` | Sound ids in play order. |
| `shuffle` | boolean | `false` | Random order, no repeats until all have played. |
| `repeat` | string | `"all"` | `off`, `all`, `one`. |
| `crossfade_ms` | integer | `2000` | `0` is a direct cut. The console offers 0–30000. |
| `bus` | string | `"music"` | `music`, `ambience`, `sfx`. |

Scenes, playlists, scene items and emitters are not written when the list is empty.

## Complete example

This file is valid TOML and covers every section. The UUIDs are examples. Replace them, or delete the `id` lines and let Bardwave generate them (not for timers, which need an `id` of your own). Every reference must match an `id` in the file.

```toml
# config.toml: annotated example. Omitted keys use the defaults above.

[server]
host = "0.0.0.0"
port = 8080
upnp = true
# access_token = "k3j9x0a7p2m5q8w1"   # generated at start when missing; "" = anyone can listen
# gm_token = "t4n8v2c6b0z3y7u5"       # generated at start when missing; "" = control page on this PC only
tls = false                 # HTTPS and wss://, applied on next Start
# tls_cert = "/etc/ssl/bardwave/fullchain.pem"   # set together with tls_key, or neither
# tls_key = "/etc/ssl/bardwave/privkey.pem"

[audio]
buffer_size = 960           # frames; 0 = driver default
sample_rate = 48000
channels = 2
master_volume = 0.8         # must be 0.0 to 1.0
normalize = false
normalize_target_lufs = -18.0
duck_amount = 0.3
duck_attack_ms = 200
duck_release_ms = 800
duck_buses = ["music", "ambience"]
stream_threshold_secs = 60  # 0 turns streaming off
# output_device = "USB Headphones"
# preview_device = "Built-in Output"

[logging]
level = "info"              # trace | debug | info | warn | error
file_logging = false

[gui]
reduce_motion = false
global_hotkeys = false
# recordings_dir = "/Users/you/Recordings"
# record_float32 = false
check_updates = false
ui_scale = 1.0              # 0.8 to 1.6

[[hotkeys]]
key = "Ctrl+1"
action = { kind = "scene", target = "c3d4e5f6-a7b8-4c9d-8e0f-2a3b4c5d6e7f" }

[[hotkeys]]
key = "F9"
action = { kind = "stop_all" }

[[timers]]
id = "f6a7b8c9-d0e1-4f2a-8b3c-5d6e7f8091a2"
name = "Last call"
trigger = { after_ms = 3600000 }        # once, one hour after Start
action = { kind = "stop_all", fade_ms = 5000 }
enabled = true

[[library.groups]]
id = "a7b8c9d0-e1f2-4a3b-9c4d-6e7f8091a2b3"
name = "Tavern"

[[library.sounds]]
id = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"
path = "C:/Bardwave/sounds/tavern-chatter.ogg"
volume = 0.6
looped = true
group = "a7b8c9d0-e1f2-4a3b-9c4d-6e7f8091a2b3"
tags = ["tavern", "crowd"]

[[library.sounds]]
id = "b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e"
path = "C:/Bardwave/sounds/thunder.wav"
bus = "sfx"
oneshot_stinger = true
pan = -0.2

[[library.sounds]]
id = "e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091"
path = "C:/Bardwave/music/lute.mp3"
bus = "music"

[[library.scenes]]
id = "c3d4e5f6-a7b8-4c9d-8e0f-2a3b4c5d6e7f"
name = "Tavern"
color = "#a04a4a"
fade_in_ms = 2000
fade_out_ms = 2000
curve = "equal_power"
stop_others = true
playlist = "d4e5f6a7-b8c9-4d0e-9f1a-3b4c5d6e7f80"

[[library.scenes.items]]
sound = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"
volume = 0.4
looped = true
start = "on_enter"
delay_ms = 0

[[library.scenes.emitters]]
sounds = ["b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e"]
min_interval_ms = 20000
max_interval_ms = 60000
volume = 0.7
volume_jitter = 0.1
pan_jitter = 0.5
max_overlap = 2

[[library.playlists]]
id = "d4e5f6a7-b8c9-4d0e-9f1a-3b4c5d6e7f80"
name = "Tavern music"
items = ["e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091"]
shuffle = true
repeat = "all"
crossfade_ms = 2000
bus = "music"
```
