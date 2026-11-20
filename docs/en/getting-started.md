# Getting started

Bardwave is a desktop ambience console for tabletop role-playing games. You load your sounds, build scenes, and press buttons to play them. The mix is streamed to each player's browser over the network, so players only open a link. This guide takes you from installation to your first session. The full configuration reference is in [../../README.md](../../README.md).

> **Note:** Bardwave is a work in progress. Some features described here (profiles, HTTPS, the first-run wizard, the interface scale) come from the `swarm/integration` branch and may be newer than the latest published release. Items that have not been tested on real hardware are marked **(not verified)**.

## Contents

- [What Bardwave is](#what-bardwave-is)
- [Requirements](#requirements)
- [Install](#install)
- [First launch](#first-launch)
- [Add your first sounds](#add-your-first-sounds)
- [Build a scene](#build-a-scene)
- [Start the server](#start-the-server)
- [Give players the link](#give-players-the-link)
- [Headless mode](#headless-mode)
- [A tour of the console](#a-tour-of-the-console)
- [Interface scale](#interface-scale)
- [Where the data lives](#where-the-data-lives)
- [Common problems](#common-problems)
- [Not verified on real hardware](#not-verified-on-real-hardware)
- [Next steps](#next-steps)

## What Bardwave is

Bardwave runs on the game master's computer. It plays music, ambience, and sound effects from your own files, mixes them live in the console, and streams the result to every player's browser as one Opus stream (20 ms frames, about 64 kbit/s). Players install nothing: they open a link and press a button. Your files stay on your computer; players hear only the mix, so you can change a sound or its level in the middle of a session and nobody downloads anything.

There are no accounts and no cloud service. A stock install makes no network request of its own: the update check is off by default, and the downloads for YouTube sounds happen only when you use that feature.

The main building blocks are:

- a **sound library** with groups and tags;
- **scenes**: saved sets of sounds with levels and fades, plus **stingers** (one-shots) and **emitters** (random sounds at random intervals);
- a **music bar** with playlists;
- three **buses** (music, ambience, sfx) with ducking;
- a **network server** with a player link, and a phone control page for the game master.

## Requirements

| Platform | Run a release | Build from source |
|---|---|---|
| Windows 10 or 11 | A sound card. | Rust 1.85 or newer, from [rustup.rs](https://rustup.rs). |
| macOS 11 or newer | A sound card. | Rust 1.85 or newer. No system libraries are needed. |
| Linux | A sound card, GTK 3, ALSA, and libxkbcommon. | Rust 1.85 or newer, plus the build packages below. |

> **Note:** The Rust version comes from the README. `Cargo.toml` does not pin it.

If no output device can be opened at start, Bardwave logs a warning and keeps streaming to players. The status strip at the bottom says why the desktop is silent.

Linux runtime libraries (Debian and Ubuntu):

```bash
sudo apt install libgtk-3-0 libasound2 libxkbcommon0
```

Linux build packages (Debian and Ubuntu). The CI workflow installs these, plus three `libxcb-*-dev` packages that this guide does not list:

```bash
sudo apt install pkg-config libgtk-3-dev libasound2-dev libxkbcommon-dev libssl-dev
```

For YouTube links you also need `ffmpeg` on Linux and macOS. See [Add your first sounds](#add-your-first-sounds).

## Install

### From a release

Releases are published on the [GitHub releases page](https://github.com/adamantida/Bardwave/releases). Choose the file for your system:

| System | File | Notes |
|---|---|---|
| Windows | `Bardwave-v<version>-windows-x86_64.zip` | Portable. Unpacks to a folder with `Bardwave.exe`, a commented `config.toml`, `README.md`, `README.txt`, and `LICENSE`. |
| Windows | `Bardwave-v<version>-windows-x86_64-setup.exe` | Installer. Installs to Program Files and can add a firewall rule for TCP port 8080. |
| Windows | `Bardwave-v<version>-windows-x86_64.exe` | The bare program, with no `config.toml` next to it. |
| macOS | `Bardwave-v<version>-macos-universal.dmg` | Unsigned unless the release was signed. |
| Linux | `Bardwave-v<version>-linux-x86_64.tar.gz` | Unpack, then run `bardwave`. |
| Linux | `Bardwave-v<version>-linux-x86_64.AppImage` | **(not verified)** |
| Linux | `bardwave_<version>-1_amd64.deb` | **(not verified)** |
| all | `SHA256SUMS` | Checksums of the release files. |

**Windows, portable zip:**

1. Unpack the zip to a folder you can write to, for example `Documents\Bardwave`. Do not use `Program Files`.
2. Run `Bardwave.exe`. The `config.toml` next to it keeps your data in that folder.

**Windows, installer:**

1. Run the `-setup.exe` file. It asks for administrator rights.
2. Keep **Allow Bardwave through Windows Firewall (inbound TCP 8080, private and public networks) so players can connect** ticked if players will connect. **(not verified)**
3. The installer adds a Start menu entry and an optional desktop shortcut. It does not install a `config.toml`, so your data goes to `%APPDATA%\Bardwave`. Uninstalling never deletes your data.

**macOS:**

1. Open the `.dmg` and drag **Bardwave** to **Applications**.
2. Do not double-click the app the first time. Right-click (Control-click) **Bardwave**, then choose **Open**, then **Open** again.
3. If macOS still refuses, open the app once, then go to **System Settings > Privacy & Security** and choose **Open Anyway**. **(not verified)**
4. On first start macOS may ask whether Bardwave may find devices on the local network. Allow it, or players on the LAN cannot connect. **(not verified)**

**Linux, tarball:**

```bash
tar -xzf Bardwave-v<version>-linux-x86_64.tar.gz
cd Bardwave-v<version>-linux-x86_64
./bardwave
```

**Linux, `.deb`:** `sudo apt install ./bardwave_<version>-1_amd64.deb`, then start **Bardwave** from the menu or run `bardwave`. **(not verified)**

### Build from source

Clone the repository and run it in release mode:

```bash
git clone https://github.com/adamantida/Bardwave.git
cd Bardwave
cargo run --release
```

On Windows you can use `build.bat` instead. Without an argument it makes a debug build; `release` makes a release build, and `test` runs all tests:

```bat
build.bat release
build.bat test
```

The binary is `target\release\soundshared.exe` on Windows and `target/release/soundshared` elsewhere. You can rename it to `Bardwave.exe` or `bardwave`.

> **Note:** A binary run from `target/release` keeps its data next to the binary, inside `target/`. Running `cargo clean` deletes that data. See [Where the data lives](#where-the-data-lives).

## First launch

The console window opens at 1280 × 800 points with a dark broadcast layout: a top bar, the sound library on the left, the network panel on the right, a music bar, and a status strip at the bottom. The window cannot be made smaller than 960 × 640 points.

On a fresh install with an empty library, the first-run wizard opens as a card in the middle of the window. It does not block the console, and it steps aside while a dialog is open. It has three pages:

| Page title | Buttons on the page |
|---|---|
| Add your sounds | **Add sound**, **Add folder**, **YouTube**. |
| Build a scene | **Save current mix as scene**. It is enabled once the library has sounds. |
| Share with players | **Start server** (**Retry server** after a failure). When the server runs: the player link with **Copy**, and **Show QR code**. |

Each page ticks off by itself when the app state matches it: the library has sounds, a scene exists, the server runs.

- **Next** and **Back** move between pages. The last page has **Done** instead of **Next**.
- **Done** closes the card and stops the wizard from opening on later starts.
- **Skip** closes the card. Tick **Don't show again** first if you want the wizard not to come back on the next start. Reopen it any time from **Help > Getting started**.

> **Note:** The wizard opens only while the library is empty and it was not dismissed for good. If the config already has a library, it is marked as done at once and does not open.

## Add your first sounds

Folder import reads these audio formats: `wav`, `flac`, `mp3`, `ogg`, `m4a`, and `aac`. Files longer than 60 seconds (`stream_threshold_secs` in `config.toml`) are decoded on the fly instead of being held in memory.

> **Note:** The file chooser behind **Add Sound** has an "Audio Files" filter that lists `mp3`, `wav`, `ogg`, `flac`, and `opus`. Picking `m4a` or `aac` files that way, and playing `opus` files, are **(not verified)**. A folder import takes `m4a` and `aac`.

1. Click **Add Sound** in the **Sound Library** panel. Choose one or more files. Or click **Add folder** in the wizard, or **Folder…** in the library toolbar, to scan a whole folder with its sub-folders. Bardwave keeps the path to each file; it does not copy it.
2. Give sounds groups or tags. The **Groups** rail on the left of the list shows **All Sounds**, **Ungrouped**, and your own groups under **Categories**. The **+** button in the rail creates a group. Click the group chip on a sound row (tooltip: **Change group**) to move the sound, and click the tags text (tooltip: **Edit tags**) to edit its tags. Use the search field (**Search name or tag…**) to filter.
3. Right-click a short sound and choose **Use As Stinger**. A stinger is a one-shot hit, such as a thunderclap, that plays over the scene and gets a pad in the **Scenes** deck. **Remove Stinger Flag** undoes it.
4. Set looped sounds, such as rain, to repeat with the loop toggle on the row (tooltip: **Repeat: on** or **Repeat: off**).

Each row also has a headphones button that plays the sound on this computer only, a volume slider, and **✕** to remove it from the library.

If a file moves or is deleted, the row shows **File not found**. Use **Locate…** to point to the file again, or **Recheck** to scan again.

### Sounds from YouTube

1. Click **YouTube** next to **Add Sound**.
2. Paste a video or music link into **Video or music link** and click **Add**.
3. Wait for the download. Only the audio is downloaded, once. It is converted to MP3, saved in `bardwave-library/` in the data directory, and then plays like any local sound. Closing the dialog does not stop a running download.

> **Warning:** The first download needs `yt-dlp` and `ffmpeg`. On Windows, Bardwave downloads both into `tools/` (about 24 MB and about 230 MB). On Linux and macOS, `yt-dlp` is fetched automatically, but you must install `ffmpeg` yourself:
>
> ```bash
> sudo apt install ffmpeg    # Debian or Ubuntu
> brew install ffmpeg        # macOS, Homebrew
> ```
>
> The dialog reports `ffmpeg not found on this system` when it is missing. A copy of `yt-dlp` or `ffmpeg` next to the program or on `PATH` is used before any downloaded one.

## Build a scene

A scene is a saved set of sounds with their levels. Scenes appear in the **Scenes** deck, which sits in the library panel above the sound list.

1. Play a few sounds by hand. Set their levels with the sliders.
2. Click **Save Mix As Scene**. The button is enabled only while something plays. In a narrow layout its label shortens to **Save Mix** or to an icon. The wizard calls the same action **Save current mix as scene**.
3. Name the scene in the **Scene name** field. Click its button in the deck later to switch to it. The transition uses a fade.
4. Use **Stop Scene** to fade out the active scene. **Fade Out** fades out everything that plays, over the transition time.

Scenes can also hold stingers, looped pads, and emitters. An emitter plays a random sound at random intervals. You set emitters, and a scene's music playlist, in the scene's **Emitters & music** editor.

## Start the server

The server sends the mix to players. Start it before the session.

1. Open the **Network Server** panel (headed **02 / Network**) on the right.
2. Click **Start Server**. The headline changes to **Live** when it runs.
3. The **Listen URL** shows the address. Click **Copy Link** to copy the player link.

The default port is `8080`. The panel shows these addresses:

| Address | Who uses it |
|---|---|
| `http://192.168.x.x:8080` | Players on the same network. |
| `http://127.0.0.1:8080` | You, to test the listener page on this computer. The panel shows it on a **Local** line. |
| `http://<global-ip>:8080` | Players over the internet. Shown on an **Internet** line when the router forwards the port. |

To stop the server, click **Stop Server**.

If the server does not start, the panel shows **Error** with the message, and the top bar shows **Server failed — see panel**. Check the message, then click **Retry Server**.

### Automatic port forwarding (UPnP)

When the server starts, Bardwave asks the router to forward the port with UPnP. The mapping is removed when the server stops. Set `upnp = false` in `config.toml` to turn this off. The **Internet** line is then hidden.

The **Internet** line shows the result. Common messages:

- **Internet: no UPnP router**: enable UPnP on the router, or forward the port by hand.
- **Internet: blocked by a second NAT**: the router has a private outside address. Forward the port on the upstream device, or ask your provider for a public address.
- **VPN active**: a VPN on this computer routes all traffic. Turn it off for internet players.

On Windows the panel can also show **Firewall: port 8080 not opened**. Allow Bardwave for public networks too, or run it once as administrator.

## Give players the link

The player link has this form:

```text
http://<address>:8080/?k=<access key>
```

If you turn on HTTPS, the link starts with `https://`. See [HTTPS](./https.md).

1. Send the link to every player with **Copy Link**.
2. Players open it in any browser and press **Establish Link**.

The link ends with `?k=<access key>`. Without the key, nobody can listen, even if the port is open to the internet.

> **Warning:** Click the refresh button next to **ACCESS KEY** only when you want to revoke the link. A new key stops the old link at once and disconnects everyone who used it. Send the new link again afterwards.

### The game-master panel

The **GM panel link** opens a phone-friendly control page at `/control`. It has its own key, and the player link never opens it. The panel shows the link masked. Click it to copy the full link, or use **Show QR** for a camera scan. **New key** replaces the key (it asks first).

> **Warning:** Remote control is on by default. Anyone with the `?g=` link can press the GM's buttons. Keep this link private. To allow the control page on this computer only, set `gm_token = ""` in `config.toml`. The panel then shows **Remote panel is off: it opens on this computer only**, with a **Create GM key** button to turn remote access on again.

## Headless mode

Headless mode runs the server without the console window. Use it for a spare computer next to the router, or for a session run from a phone.

From a release, run this in a terminal:

```bash
Bardwave.exe --server-only    # Windows
./bardwave --server-only      # Linux
```

From source:

```bash
cargo run --release -- --server-only
```

Headless mode:

- loads the library saved in `config.toml`;
- starts the listener server at once;
- logs the player link and the control links.

The log shows these lines:

- `Listener server available at …` gives the player link.
- `Control page (this PC only): …/control` gives the control page for this computer.
- `Remote control: …/control?g=<gm_token from config.toml>` gives the remote panel link. It is logged only while remote control is on.

On the same computer, `http://127.0.0.1:8080/control` opens without a key. From another device, use `http://<address>:8080/control?g=<gm_token>`. The token is in `config.toml` under `[server]`.

Press **Ctrl+C** to stop. On Windows, if you start it from Explorer or a shortcut, it opens its own console window for the log, and you can close that window to stop it.

`--profile` and the HTTPS setting work in headless mode too.

> **Note:** Headless mode does not run hotkeys or session timers. To change the library, edit it in the console, then restart the server.

### Profiles

Each campaign can have its own config and library. Start one with `--profile`, for example `cargo run --release -- --profile "Curse of Strahd"` (`--profile=<name>` also works, and `--profile default` is the default profile). Names may contain letters, digits, spaces, `-`, and `_`, up to 32 characters. The **Profile** menu in the top bar switches between them. A switch saves your work and restarts Bardwave with the other profile.

## A tour of the console

### Top bar

| Control | What it does |
|---|---|
| Wordmark and **Broadcast Console** | Brand and caption. The caption is dropped in a narrow window. |
| **Master** slider and percentage | Master volume. Tooltip: **Master volume**. |
| **STOP ALL** | Stops everything at once: sounds, the active scene, the music, and the headphone preview. Tooltip: **Stop all sounds immediately (panic)**. It looks dimmed while nothing plays, but it still works. |
| **REC** | Records the mix the players hear to a WAV file. Shows **Stop rec** while recording. The folder button next to it chooses the recordings folder. |
| **Profile** | The current profile name. Its menu lists recent profiles, plus **New profile…**, **Manage profiles…**, and **Open data folder**. |
| **Help** | Contains **Getting started**, which reopens the wizard. |
| Status | **Live**, **Starting**, **Error**, or **Off Air**, with a second line such as **Server running** or **Server failed — see panel**. |

The buttons are painted in capitals, but the labels in this guide match the code. In a narrow window the bar shortens its labels and drops some parts.

### Sound library

The left panel, **01 / Library** (titled **Sound Library**), lists sounds and groups. **Add Sound** adds files, **YouTube** adds a link, **Folder…** scans a folder, and **Export…** and **Import…** handle `.zip` bundles. Below the list, a pinned **Master Output** strip has a volume slider and a **Mute** / **Unmute** button.

### Scenes deck

The **Scenes** deck sits above the sound list. It holds scene buttons, the **Stingers** pads, **Stop Scene**, **Fade Out**, and the **Transition** time (**Auto** uses each scene's own fades). Use **New** to create an empty scene, **Save Mix As Scene** to save the current mix, and **Show** or **Hide** to collapse the deck.

### Music bar

The music bar sits above the status strip.

- Buttons for **Previous track**, **Play** / **Pause** / **Resume**, **Stop**, and **Next track** control the current playlist.
- The now-playing line shows the track and its position, with a seek bar. The idle state shows **No music playing**.
- **Shuffle** and **Repeat** (off, all, or one) set the playback mode.
- **Music…** opens the **Music & scenes** dialog with **Playlists** and **Scenes** tabs. Playlists play as background music. A scene can start a playlist, and the **Scenes** tab leads to each scene's **Emitters & music** editor.

### Network panel

The right panel, **02 / Network** (titled **Network Server**), holds from top to bottom:

- the server status with **Start Server** / **Stop Server** and **Copy Link**;
- the **Listen URL**, the **Local** and **Internet** lines, and any network warnings;
- the **ACCESS KEY** line with its refresh button, and the **GM panel link**;
- the **Listeners** count with its peak;
- the **Mix** section: bus faders, **Duck**, **Hotkeys**, **Timers**, duck settings, loudness normalization, and **Interface** (the scale slider);
- **Audio output**, for the sound card and a separate headphone device;
- **Updates**, with **Check for updates**;
- **Secure connection (HTTPS)**;
- **Configuration**: **Port** and **Bind Address**, editable only while the server is stopped.

The status strip at the very bottom shows the version, the local output, the format and library counts, and an **Update available** link when an update check found one.

## Interface scale

Use this to make the console larger or smaller.

1. Open the **Network** panel and scroll to the end of the **Mix** section.
2. Find **Interface**, then the **Scale** slider.
3. Drag to set the scale. It ranges from 80% to 160%, in steps of 5%.
4. Click **Reset** to return to 100%. The button is dimmed at 100%.

The scale is applied when you release the slider, and it is remembered in `config.toml`. Keyboard zoom (Ctrl with + or -) is switched off, so the slider is the only control.

The config key is `[gui] ui_scale`. It is written only when it differs from 1.0:

```toml
[gui]
ui_scale = 1.3   # 1.0 = 100 %; allowed range 0.8 to 1.6
```

A hand-edited value outside the range is clamped to it. An invalid value falls back to 1.0.

The window keeps a minimum size of 960 × 640 points, and it grows if a larger scale would make it too small.

## Where the data lives

Bardwave keeps `config.toml`, the library, the recordings, and the downloaded tools in a **data directory**. The location is chosen once, at start. Use the first rule below that matches:

| Situation | Data directory |
|---|---|
| `config.toml` is already next to the program (on Windows only if that folder is writable), or a file named `portable.flag` is there | The program's folder. |
| The program's folder is writable, and it is not inside a macOS `.app` | The program's folder. |
| macOS `.app` | `~/Library/Application Support/Bardwave` |
| Windows, for example installed into Program Files | `%APPDATA%\Bardwave` |
| Linux | `$XDG_DATA_HOME/bardwave`, or `~/.local/share/bardwave` |

If the chosen system folder cannot be created, or the home variables are missing, Bardwave uses the program's folder instead.

| Item | Location |
|---|---|
| `config.toml`, downloaded sounds `bardwave-library/`, HTTPS files `tls/` | The data directory of the active profile. |
| Recordings `recordings/` | The data directory of the active profile, unless `recordings_dir` is set. |
| Log `bardwave.log` (only with `file_logging = true`), downloaded tools `tools/`, recent profiles `recent.toml` | The shared root data directory. |

For a named profile, the config and library are in `profiles/<name>/` inside the data directory. The default profile uses the data directory itself.

### Portable mode and reset

To keep all data next to the program, create an empty file named `portable.flag` in the program's folder. The Windows zip does the same job with the `config.toml` that it contains. Inside a macOS `.app`, the program's folder is `Contents/MacOS` **(not verified)**; changing the bundle can break its signature.

To reset Bardwave, delete the data directory. This also deletes your other profiles, which live inside it. Uninstalling never deletes it.

## Common problems

| Symptom | Likely cause | What to do |
|---|---|---|
| Players cannot connect over the LAN | A firewall blocks the port. | Allow Bardwave through the firewall for private and public networks. On Linux, open TCP port 8080. On macOS, allow **Local Network** for Bardwave in **System Settings > Privacy & Security**. **(not verified)** |
| The **Internet** line is missing | `upnp = false` in `config.toml`. | Set `upnp = true`, or forward the port by hand. |
| **Internet: no UPnP router** | UPnP is off on the router, or no router answered. | Enable UPnP, or forward the port by hand. |
| **Internet: blocked by a second NAT** | The router's outside address is private. | Forward the port on the upstream device, or get a public address. |
| **VPN active** | A VPN routes this computer's traffic. | Turn off the VPN for internet players. |
| **Firewall: port 8080 not opened** (Windows) | Bardwave could not add its firewall rule. | Allow Bardwave for public networks too, or run it once as administrator. |
| The console does not start, and shows `config.toml could not be read` | `config.toml` has invalid TOML. | Fix the file, or move it away to start with defaults. |
| Hotkeys do not fire while another window is focused | Global hotkeys are off. | Tick the global hotkeys box in the **Hotkeys** dialog, or set `global_hotkeys = true` in `[gui]`. On macOS and Linux **(not verified)**; on Linux they work on X11 only. |
| YouTube reports `ffmpeg not found on this system` | `ffmpeg` is missing on Linux or macOS. | Install `ffmpeg` with your package manager. |
| A sound shows **File not found** | The file moved. | Click **Locate…**, or **Recheck**. |
| The panel shows **Error** | The server could not start, for example the port is taken. | Read the message in the panel, change the port under **Configuration** while stopped, then click **Retry Server**. |

## Not verified on real hardware

The following items are implemented and covered by tests or CI builds. They have not been run on real machines:

- The Windows installer and the Linux packages (AppImage, `.deb`).
- The macOS `.app`, the `.dmg`, the universal binary, the Local Network prompt, and signing.
- Global hotkeys on macOS, and on Windows and Linux (X11).
- MIDI. It is off by default, is not connected to the console, and needs `--features midi`.
- Audio device names on Windows (WASAPI) and Linux (ALSA or JACK).
- UPnP with real routers.
- The listener page on iOS Safari and Android Chrome, including home-screen install.
- HTTPS with a self-signed certificate on a real phone.
- The data-directory choice on every OS, and the profiles, the first-run wizard, and the interface scale on Windows and Linux.

## Next steps

- Read [HTTPS](./https.md) before you play with players outside your network.
- Set up a second profile for another campaign with `--profile`.
- Read [Scenes, music and hotkeys](./scenes-music-hotkeys.md), [Sound library](./soundboard-library.md), and [Network, players and the GM remote panel](./network-remote.md) for each part in depth.
- Read [Audio devices, recording, profiles and updates](./audio-session-tools.md) for the audio output, REC, and profiles.
- Read [Configuration reference](./configuration.md), and [../../README.md](../../README.md) for the full configuration reference.
- Read [Troubleshooting, FAQ and security](./troubleshooting-security.md) when something does not work.
