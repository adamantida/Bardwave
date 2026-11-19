# Audio devices, recording, profiles and updates

This page describes four parts of the Bardwave console: choosing the sound cards, recording the session to WAV, running separate campaign profiles, and the optional update check. It follows the code in `src/audio`, `src/gui`, `src/profiles.rs`, `src/paths.rs` and `src/updates.rs`. Button and label text is quoted in the case used in the source. The console itself draws buttons and block captions in capitals (for example `REC`, `REFRESH DEVICES`, `AUDIO OUTPUT`).

Device handling is written against cpal and exercised on macOS (CoreAudio) only. Windows (WASAPI) and Linux (ALSA/JACK) behaviour is not verified.

## Contents

- [Audio output devices](#audio-output-devices)
  - [Choosing the output and preview devices](#choosing-the-output-and-preview-devices)
  - [Fallback and error messages](#fallback-and-error-messages)
  - [Buffer size, sample rate and channels](#buffer-size-sample-rate-and-channels)
- [Session recording](#session-recording)
  - [What is recorded](#what-is-recorded)
  - [Where files go and how they are named](#where-files-go-and-how-they-are-named)
  - [Size limits and splitting](#size-limits-and-splitting)
  - [Recording settings](#recording-settings)
  - [Recording troubleshooting](#recording-troubleshooting)
- [Profiles](#profiles)
  - [Starting with a profile](#starting-with-a-profile)
  - [Switching profiles](#switching-profiles)
  - [Managing profiles](#managing-profiles)
  - [Recent profiles](#recent-profiles)
- [Update check](#update-check)

## Audio output devices

The **AUDIO OUTPUT** block sits in the server panel, between the mix settings and the **Updates** block. It has two dropdowns, each with a status line under it:

| Dropdown | Label | Purpose |
|---|---|---|
| Main output | `Output` | The device that plays the mix. Its "none" entry is `System default (<name>)`, or just `System default` when no default device is known. |
| Preview output | `Preview` | The device that plays sound previews (usually headphones). Its "none" entry is `Same as output`. |

A change applies at once. The engine stops its streams and opens them again on the new selection without restarting the application, and the tracks keep playing. The choice is also saved to `config.toml` at once.

### Choosing the output and preview devices

1. Open the **AUDIO OUTPUT** block.
2. Open the `Output` dropdown and choose a device. To return to the default, choose `System default (<name>)`.
3. Open the `Preview` dropdown and choose headphones. To play previews on the main output, choose `Same as output`.
4. Read the status line under each dropdown.
5. Press `Refresh devices` after plugging a device in or out.

Hover over a device in the list to see its summary, for example `2 ch · 44.1 / 48 / 96 kHz · system default`. The summary is built from the largest channel count the device offers and the supported rates out of this list: 22.05, 44.1, 48, 88.2, 96 and 192 kHz. Long device names are shortened with an ellipsis in the dropdown.

The selection is saved in `[audio]`:

```toml
[audio]
output_device = "USB Headphones"   # main output; omit for the system default
preview_device = "Built-in Output" # previews; omit to play them on the main output
```

Rules that the code applies:

- A blank name (spaces only) counts as unset.
- Names match exactly first, then ignoring case and surrounding spaces.
- If `preview_device` is the same device (by name) as the one the main output actually opened, the device is opened once. Previews then share the main output.
- Previews on a separate device never reach the players' stream. They are also not ducked and ignore the bus faders.

> **Note:** Device names are the host's own strings. A name saved on one operating system does not exist on another. On some hosts two devices can share a name; the first one wins.

A device that is configured but not in the list is still shown in its dropdown, with the suffix `(not connected)`. The configuration is kept, so plugging the device back in and pressing `Refresh devices` is enough.

`Refresh devices` reads the device list again on a background thread (the button shows `Refreshing...` meanwhile, so the console does not freeze on a sleeping Bluetooth device). When the scan ends, Bardwave applies the stored selection again if that could help: a configured device is present but not in use, a preview device that failed is present, or the output failed or has no device while devices exist.

### Fallback and error messages

The main output is opened in this order:

1. The configured `output_device`, when it is found in the list.
2. The system default device.

If the named device is missing, the engine uses the system default and says so. If the named device is found but cannot be opened, the engine also tries the system default. The status line shows the reason in brackets, for example:

```text
Active: Built-in Output - 48000 Hz, 2 ch (output device "Bluetooth Speaker" not found; using the system default)
```

```text
Active: Built-in Output - 48000 Hz, 2 ch (could not open "USB Headphones" (<error>); using the system default)
```

If no device can be opened at all (no default device, or the stream cannot be built or started), the engine continues without a sound card ("headless"). Listeners still get the stream, but nothing plays on the desktop. The status line then reads `Error: no output device opened (network only)` followed by the reason in brackets, for example `(local output unavailable: <error>)`.

If an open stream fails while it runs (for example the device is unplugged), the main output switches to the `Error: <message>` state. It does not recover by itself. Plug the device in and press `Refresh devices`, or choose a device again.

The status lines:

| Status line | Meaning |
|---|---|
| `Active: <device> - <rate> Hz, <ch> ch` | The output runs. A reason in brackets is added when a fallback or a format conversion was used. |
| `Output not started` | No output has been opened yet. |
| `Error: no output device opened (network only)` | No sound card opened. The server still streams to listeners. |
| `Error: <message>` | The output failed. The message is the reason. |
| `Previews play on the main output` | No separate preview device is in use. |
| `Active: <device> - <rate> Hz, <ch> ch` (under `Preview`) | Previews play on their own device. |
| `Error: preview device "<name>" not found; previews play on the main output` | The configured preview device is missing. Previews fall back to the main output. |
| `Error: could not open the preview device: <error>; previews play on the main output` | The preview device exists but cannot be opened. Same fallback. |
| `Error: could not start the preview stream: <error>; previews play on the main output` | The preview stream was built but did not start. Same fallback. |
| `Error: <error>; previews play on the main output` | The preview stream failed while running (for example the headphones were unplugged). Previews go back to the main output. |

A preview device is tried even when the main output has no device (headless).

Other messages you may see:

| Message | Where | Meaning |
|---|---|---|
| `<name> (not connected)` | Dropdown entry | The configured device is not in the list. |
| `Error: ...` under the block | Audio section | A device scan failed (for example `could not list output devices: ...`), or a device change could not be applied (for example `the audio engine is not running`). |
| `Refreshing...` | Button | A device scan is running. |

### Buffer size, sample rate and channels

These three keys are not in the dropdowns. The block only shows them, in a line such as `buffer: 960 frames (20.0 ms) - 48000 Hz - 2 ch`. With `buffer_size = 0` the line starts with `buffer: driver default`.

| Key | Default | Allowed values | Notes |
|---|---|---|---|
| `sample_rate` | `48000` | `8000` to `192000` | Out-of-range values are logged and replaced by `48000`. |
| `channels` | `2` | `1` to `8` | Out-of-range values are logged and replaced by `2`. `1` downmixes to mono. With more than 2, the extra channels get a mono downmix of left and right. |
| `buffer_size` | `960` | `0` or `64` to `8192` | Frames per device buffer. `0` lets the driver choose. Out-of-range values are logged and replaced by `480` (10 ms at 48 kHz), not by `960`. |

```toml
[audio]
sample_rate = 48000
channels = 2
buffer_size = 960   # 20 ms at 48 kHz
```

These keys only choose how the desktop sound card is opened. The mix, the network stream and the session recording are always 48 kHz stereo.

The values are read when the application starts. A device change, or a later edit of `config.toml`, does not re-read them. The info line under the dropdowns shows the values from the config as written, even when an out-of-range value was replaced.

The engine tries to open the device in this order:

1. The configured rate and channel count in 32-bit float.
2. The mix format: 48 kHz stereo, 32-bit float.
3. Stereo 32-bit float at another rate (the device default rate when it fits, otherwise the closest rate to 48 kHz), resampled inside the audio callback.
4. The device's own default format (any rate, channel count and sample format), with resampling and conversion.

When the device cannot run the configured rate and channels, or the mix has to be converted, the status line shows the reason, for example `device has no 48 kHz stereo f32 mode; using 44100 Hz stereo f32 and resampling`. If the device refuses a fixed `buffer_size`, the engine retries with the device default buffer size (no message in the console; it is logged).

> **Note:** The engine source comments still mention a 10 ms default buffer. The configured default is `960` frames, which is 20 ms at 48 kHz. The 10 ms value (`480` frames) is only the fallback for an invalid `buffer_size`.

## Session recording

The **Rec** button in the top bar records the session to a WAV file. The **folder** button next to it chooses the recordings folder.

| Control | Label or tooltip | Behaviour |
|---|---|---|
| REC button, idle | `Rec` (tooltip: `Record the mix the players hear to a WAV file`) | Starts a recording. |
| REC button, recording | `Stop rec`, or `Stop` when the bar is narrow (tooltip: `Stop and save the recording`) | Stops and saves the file. |
| Folder button | Tooltip: `Recordings folder: <path>` followed by `Click to change (applies to the next recording)` | Opens the folder chooser, titled `Recordings folder`. A click while recording does nothing. The bar drops this button first when the window is narrow. |
| Readout | `HH:MM:SS  ·  <size>` | Elapsed time and size on disk (all parts, headers included) while recording. Shown when the bar has enough room. |
| `<n> lost` | Amber text | Frames were dropped because the disk could not keep up. |

The REC button is disabled while a recording is starting or while the previous one is being finalized. Starting and stopping run on short background threads, so the console stays responsive. The file is finalized by the writer thread after `Stop rec`. The console then shows `Saved <file name> (<duration>, <size>)`, followed by ` in <n> files` when the recording was split, and by `; <n> frames were lost (disk too slow)` when frames were dropped. The size is shown in 1024-based units (`KB`, `MB`, `GB`).

### What is recorded

The recording is the mix the players hear:

- It is taken after the mixer's master volume and limiter, at the point where the mix goes to the network broadcaster.
- It is 48 kHz stereo, whatever the sound card's rate is, and it does not depend on which device plays.
- Sound previews are not in it. Previews play on their own path and never reach the players' stream.
- The recording is never blocked by the audio callback. If the writer falls behind a slow disk, whole buffers are dropped and counted as lost frames.
- While nothing is playing, the writer pads silence from the wall clock. The file keeps the real session time.

Whether the recording runs while the server is stopped and nobody is connected has not been tested on a real session (not verified). The recorder is attached to the mixer output before the server starts.

### Where files go and how they are named

The default folder is `recordings/` inside the data directory of the running profile. The data directory is described in [Profiles](#profiles). For the default profile it is the root data folder.

Files are named with the UTC time at which the recording starts:

```text
bardwave-20261119-203015.wav
```

- If the name is taken, `-2`, `-3` and so on are added (up to `-999`). A recording is never overwritten.
- A recording that needs more than one file continues as `bardwave-20261119-203015_part2.wav`, `_part3.wav` and so on. After a `-2` rename, the parts follow it, for example `bardwave-20261119-203015-2_part2.wav`.

To change the folder, press the folder button. The chosen folder is saved to `[gui] recordings_dir` in the config of the running profile. It applies to the next recording. A blank value counts as unset. The folder is created if it is missing.

```toml
[gui]
recordings_dir = "/Users/you/Recordings"
```

### Size limits and splitting

| Setting | Value | Effect |
|---|---|---|
| Split size | `1800000000` bytes (about 1.8 GB) of audio data per file | Recording continues in the next `_partN.wav` file. |
| Size warning | More than `1500000000` bytes (about 1.5 GB) in one file | Shows `Large recording: <x> GB in this file; it continues in a new file at <y> GB`. |
| Minimum free space | 200 MB (`200 * 1024 * 1024` bytes) | Recording does not start below this. |
| Low-space warning | Less than one hour of free space at the current format, checked when the recording starts | Shows `Low disk space: <n> MB free, about <n> min of recording`. |
| Header update | About once per second | A file left behind by a crash is a valid WAV up to the last update. |

The free-space check works on Unix systems (macOS, Linux). On other systems (Windows) the check is skipped, and a full disk ends the recording with a write error instead (not verified on Windows).

The WAV format cannot exceed 4 GB per file, so the split size is capped just below that (about 4.29 GB). The default split size is far below the cap.

Approximate file sizes, computed from 48 kHz stereo:

| Format | Per hour | Parts per hour |
|---|---|---|
| 16-bit PCM (default) | about 691 MB | under one part |
| 32-bit float | about 1.38 GB | about 0.8 part |

### Recording settings

The format is set in `config.toml`. The console has no control for it.

```toml
[gui]
record_float32 = false   # false: 16-bit PCM (default); true: 32-bit float
```

### Recording troubleshooting

| Symptom | Cause | What to do |
|---|---|---|
| `Cannot start recording: not enough free disk space: <n> MB free, at least 200 MB needed` | Too little free space. | Free space, or choose another folder. |
| `Cannot start recording: cannot create the recordings folder <path>: ...` | The folder cannot be created. | Choose a folder you can write to with the folder button. |
| `Cannot start recording: cannot create <path>: ...` | The first file cannot be created. | Check that the folder is writable. |
| `Cannot start recording: a recording is already in progress` | A recording is already running. | Stop it first. |
| `Low disk space: ...` | Less than an hour of space. | Free space or stop soon. |
| `<n> lost` in amber, or `frames were lost (disk too slow)` | The disk could not keep up. | Record to a faster disk. |
| `Recording stopped: ...` | A write failed. | Check the disk and the message. The file up to that point is kept. |

## Profiles

A profile is a separate campaign with its own `config.toml` and library. The default profile is the root data folder. A named profile lives in its own folder:

```text
<root>/                       default profile: config.toml, bardwave-library/
<root>/profiles/<name>/       named profile: config.toml, bardwave-library/
<root>/recent.toml            shared: recently used profiles
<root>/bardwave.log           shared: log (written when [logging] file_logging = true)
<root>/tools/                 shared: downloaded tools
```

The root is decided by the platform rules in `src/paths.rs`. In short: the folder of the executable when it already holds a `config.toml` or a `portable.flag` file, or when it is writable (and not inside a macOS `.app` bundle). Otherwise it is `~/Library/Application Support/Bardwave` on macOS, `%APPDATA%\Bardwave` on Windows, and `$XDG_DATA_HOME/bardwave` (or `~/.local/share/bardwave`) on Linux. Config, library and recordings belong to each profile. The log, the tools and `recent.toml` are shared by all profiles.

### Starting with a profile

Start Bardwave with the `--profile` option:

```bash
soundshared --profile "Campaign A"
soundshared --profile=Campaign-A
soundshared --profile default
```

The executable is called `soundshared` when you build it with cargo (for example `cargo run --release -- --profile "Campaign A"`); the Linux `.deb` package installs it as `bardwave`. `--profile default` is the same as no option. The option also works together with `--server-only`.

Name rules:

| Rule | Example of a refused name |
|---|---|
| 1 to 32 characters (surrounding spaces are trimmed) | an empty name, or a 33-character name |
| Letters, digits, spaces, `-` and `_` (letters of any alphabet) | `Campaign/A`, `camp.a` |
| Not `default` (any case) | `Default` |
| Not a Windows reserved name | `CON`, `PRN`, `AUX`, `NUL`, `COM1` to `COM9`, `LPT1` to `LPT9` |
| Unique, ignoring case | `campaign a` when `Campaign A` exists |

If the name is invalid, Bardwave does not start. It prints `Error: invalid profile name: ...` to stderr and exits with code 2. Unless `--server-only` is used, it also shows a message box. A `--profile` with no value gives `--profile needs a profile name`. The same exit code 2 is used when the profile folder cannot be created. When the option is repeated, the last one wins.

A profile that does not exist yet is created by `--profile` (its folder is made at start). Its first start uses the default settings.

### Switching profiles

Open the profile name in the top bar (the `PROFILE` caption in front of it is drawn when the bar has room). The window title of a named profile ends with ` — <name>`. The menu has the heading `Switch to (restarts Bardwave)`.

1. Choose a profile from the list. The default profile is listed as `Default`. The running profile is marked `(running)` and cannot be chosen.
2. In the **Switch Profile** dialog, read the text: `Switch to "<name>"? The server and all sounds stop, the current profile is saved.`
3. Press `Switch` (or `Cancel`).

Bardwave saves the current profile, closes the window, stops the server and the audio, and then starts a new process with `--profile <name>` (`--profile default` for the default profile). The new process is detached. If it cannot be started, the error is logged and Bardwave simply ends. Switching exists only in the console window; `--server-only` has no menu.

### Managing profiles

The profile menu has these items:

| Item | Result |
|---|---|
| `New profile…` | Opens **New Profile**. |
| `Manage profiles…` | Opens **Profiles**, with `Rename` and `Delete` for each other profile. The default and the running profile have no buttons. The dialog also has `New profile…` and `Close`. |
| `Open data folder` | Opens the data folder of the running profile in the file manager. |

**New Profile** has a `Profile name` field (hint `Campaign name`), the checkbox `Copy the current library`, and the button `Create`. Under the field it shows `Letters, digits, space, - and _`, or the reason a name was refused. The copy option:

- copies the running profile's `config.toml` and `bardwave-library/` (nothing else, so recordings are not copied; symbolic links are skipped);
- copies the whole config, so other settings such as the server port are copied too;
- rewrites the library paths in the copied config so they point to the new folder;
- runs in the background, and the dialog shows `Copying the library…`. The dialog cannot be closed until the copy ends.

A partly copied profile is deleted again if the copy fails. Without the copy option, an empty folder is created. After `Create` the **Switch Profile** dialog opens with the note `Profile created`. The new profile does not start until you press `Switch`.

**Rename Profile** renames the folder, takes a new name with the same rules (the field is labelled `New name`), and changes the library paths in its config. The button is `Rename`. Changing only the letter case is allowed. After a rename the list shows `Renamed <old> to <new>`.

**Delete Profile** deletes the profile folder for good. There is no trash.

- An empty profile (a folder with no files) is deleted without questions. The dialog says `This profile is empty.`
- A non-empty profile needs its exact name typed in the box `Type "<name>" to confirm`. The dialog says `Its config and library are deleted for good. There is no trash.` The button stays disabled until the name matches (case-sensitive).
- The button is `Delete`. After a delete the list shows `Deleted <name>`.

> **Warning:** Deleting a profile removes its whole folder: config, library and the default `recordings/` folder. This cannot be undone. Use `Open data folder` and copy the folder first if you need a backup.

The running profile cannot be renamed or deleted. The console does not offer the buttons, and the code refuses with `The running profile cannot be changed`. Switch to another profile first.

### Recent profiles

`recent.toml` in the root folder keeps up to 8 profile names, newest first. Each start of Bardwave (including `--server-only`) moves the running profile to the front.

- The profile menu lists recent profiles first, then `Default` if it is not among them, then the other profiles in alphabetical order.
- A profile that no longer exists is not shown.
- Renaming or deleting a profile removes its old name from the recent list.
- A missing or unreadable `recent.toml` is treated as an empty list.

```toml
# <root>/recent.toml
recent = ["Campaign A", "default"]
```

The default profile is stored as `default`.

## Update check

The check looks for a newer release. It downloads nothing and installs nothing. It is in the **Updates** block of the server panel.

| Control | Label or text | Behaviour |
|---|---|---|
| Button | `Check for updates` (tooltip: `Asks GitHub for the newest release. Nothing else is sent or downloaded.`) | Runs one check now. Disabled while a check runs. |
| Checkbox | `Check once a day at start` (tooltip: `Off by default: Bardwave makes no network requests on its own.`) | Enables the automatic check. |
| Result line | `Checking...`, `Up to date (v<version>)`, `Update available: v<version>` | Shows the result. |
| Link | `Open release page` | Opens the release page in the browser. Shown when an update is available. |

When an update is available, the status strip also shows a link `Update available: v<version>` that opens the same page.

Failures are shown in amber:

| Message | Cause |
|---|---|
| `Network error: could not reach the update server` | No connection, DNS or TLS failure, or the 8-second timeout. |
| `Update server answered HTTP <code>` | The server answered with an error status (for example a rate limit). |
| `Update server sent an oversized reply` | The reply was larger than 256 KiB. |
| `Update server sent an unreadable reply` | The reply was not a release document. |
| `Could not start the update check` | The worker thread could not be started. |

The check is off by default:

```toml
[gui]
check_updates = false      # daily check at start; default false
last_update_check = 0      # unix time of the last check; written by Bardwave
```

When `check_updates` is on, Bardwave checks once per start of the console if it has never checked, or if at least 24 hours have passed since `last_update_check` (a clock that went backwards also counts as due). A manual check updates `last_update_check` too. The manual button works whether or not the daily check is on. Only stable releases are offered: drafts and pre-releases are ignored, and a build whose own version does not parse as `major.minor.patch` is never told to update.

What is sent: one `GET` request to the GitHub "latest release" API (`https://api.github.com/repos/adamantida/Bardwave/releases/latest`). The code sets only two headers:

| Header | Value |
|---|---|
| `User-Agent` | `Bardwave/<version>` |
| `Accept` | `application/vnd.github+json` |

The request carries no cookies, no authorization and no referrer. The HTTP library adds its own transport headers (such as `Host`), and GitHub sees the IP address the request comes from, as with any web request. The request times out after 8 seconds. A reply larger than 256 KiB is refused. The check does not send settings, library contents or profile names. The release page link is built by Bardwave from the version number, never taken from the reply.

> **Note:** The request and the parsing are tested against a local test listener. A real request to GitHub from a release build is not verified in this document.
