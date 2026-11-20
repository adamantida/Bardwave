# Troubleshooting, FAQ and security

This guide covers the problems operators meet most often, a short FAQ, and the security model of the listener stream and the remote control panel. Setting names, defaults and button labels come from the source code. Behaviour that has not been run on real hardware is marked **(not verified)**. The full list is at the end.

Related pages: [Network, players and the GM remote panel](./network-remote.md), [Configuration reference](./configuration.md), [Audio devices, recording, profiles and updates](./audio-session-tools.md), [HTTPS](./https.md).

## Contents

- [Troubleshooting](#troubleshooting)
- [FAQ](#faq)
- [Security model](#security-model)
- [Not verified on real hardware](#not-verified-on-real-hardware)

## Troubleshooting

### Start here

Look at the **Network Server** panel first. The state is shown in capitals:

| State | Line under it | Meaning |
|---|---|---|
| `Live` | `Broadcasting to N listeners` | The server is running. |
| `Starting` | `Binding the listen socket` | The start is in progress. |
| `Error` | The error text, cut to 34 characters | The server could not start. The full text is in the log. |
| `Off Air` | `Server stopped` | The server is not running. |

Under the **Listen URL** may sit a `Local ...` line, an `Internet ...` line and amber notes (`VPN active ...`, `Firewall: ...`). Port and bind address are edited in **CONFIGURATION**, only while the server is stopped.

**Where is the log?** By default the log goes to the terminal only, and a Windows release build has no console window. To keep it, set `file_logging = true` in `[logging]`. Bardwave then also writes `bardwave.log` in the data folder (the root data folder when profiles are used, shared by all profiles). `level` is `info` by default (`trace`, `debug`, `info`, `warn` or `error`). The `RUST_LOG` environment variable replaces `level` when it holds a valid filter. The log never holds the GM token, but the player key can appear in it (see [Rotating keys](#rotating-keys)).

```toml
[logging]
level = "info"
file_logging = true   # writes bardwave.log in the data folder
```

### No sound

Check the status line under the **Output** dropdown in the **AUDIO OUTPUT** section. It reads `Active: <device> - 48000 Hz, 2 ch`, or `Error: ...`.

| Symptom | Likely cause | Fix |
|---|---|---|
| It plays on the wrong card. The status line has a note such as `output device "X" not found; using the system default` | `[audio] output_device` names a device that was not found. Bardwave falls back to the system default. The match is exact, then ignoring case and surrounding spaces. | Pick the device again in the dropdown, or remove `output_device`. Plug the device in and press **Refresh devices**. |
| `Error: no output device opened (network only)` | No sound card could be opened. Players still get the stream. | Check the system output device, then press **Refresh devices**. |
| Nothing plays, no message | The master level is at zero (the console shows **Unmute** on the MASTER OUTPUT strip when muted). Or a bus fader (**Music**, **Ambience**, **Sfx**, MIX section) is at zero. Or **Duck** is latched (the button reads **Ducked**), which lowers music and ambience. | Press **Unmute**, raise the master and the faders, press **Ducked** again to release. |
| A sound row is amber with a warning | Its file is missing. | See [Missing files](#missing-files). |
| Players hear nothing, the console plays | Their link has no key, or they have not pressed **Establish Link**. The listener page needs a tap to start audio. | Send the full link with `?k=<key>` (**Copy Link**), and ask them to press **Establish Link**. |
| Headphone preview is missing from the stream | Previews never reach the players, by design. | Listen to previews on this computer only. |
| Previews play on the main output | `[audio] preview_device` is unset, or the device was not found (previews then play on the main output with a note). | Set it to the headphone device name. |

`master_volume` starts at `0.8` and is read from `config.toml` at start. The console's master fader and bus faders are not written back to the file. To change the start level, edit `master_volume` (a number from `0.0` to `1.0`).

```toml
[audio]
output_device = "USB Headphones"    # main mix; omit for the system default
preview_device = "Built-in Output"  # previews only
master_volume = 0.8
```

### Crackling and dropouts

| Symptom | Likely cause | Fix |
|---|---|---|
| Clicks or crackle on the console | The sound-card buffer is too small for this driver. | Raise `buffer_size`, for example to `1920`, and restart Bardwave (not verified). |
| The console ignores a value | `buffer_size` is outside `0` or `64`-`8192`. The log warns and `480` frames are used (not the `960` default of the file). | Use `0` (driver default) or a value in range. |
| `sample_rate` or `channels` ignored | Out of range (`8000`-`192000` Hz, `1`-`8` channels). The log warns and `48000` Hz stereo is used. | Use a value in range. |
| The status line says the device format differs | The card has no 48 kHz stereo mode. The mix is resampled or converted for the device. The players' stream is not affected. | Normally nothing. Choose another device if the sound is poor. |
| Listeners hear gaps on Wi-Fi | Network loss. The listener page reconnects after 1, 2, 4, 8 and then 15 seconds, shows `Reconnecting #N`, and shows link quality (latency, buffer, packet loss over 10 seconds) and a `Weak network` hint. The server drops frames for a client whose send buffer is full (100 packets, about 2 seconds). | Move closer to the router, or use a cable. |

The **AUDIO OUTPUT** section shows the active values, for example `buffer: 960 frames (20.0 ms) - 48000 Hz - 2 ch`. They are read-only there.

```toml
[audio]
buffer_size = 1920   # frames per channel; default 960 (20 ms at 48 kHz); 0 = driver default
```

> **Note:** A larger buffer adds latency to the console output. The key is read once at start, so change one value at a time and restart Bardwave. The packets sent to players are always 960 frames (20 ms) of Opus audio, whatever `buffer_size` is.

### Players cannot connect

| Symptom | Likely cause | Fix |
|---|---|---|
| `Off Air` / `Server stopped` | The server is not running. | Press **Start Server**. |
| `Error` state | The listen socket could not be bound, often because the port is in use. | Change **Port** in CONFIGURATION while stopped, then press **Retry Server**. |
| `Port must be 1–65535` or `Bind must be an IP address` under CONFIGURATION | The value is invalid. The last valid value stays in force. Bind Address takes an IP address only, not a host name. | Enter a valid value. |
| Page opens, `Access Denied`, `This link has no access key` | The link lacks the listener key. | Press **Copy Link** and send that link. |
| Page opens, `Access Denied`, `This link is no longer valid: the GM issued a new access key` | The key was replaced after the link was shared (close code `4001`). | Send the new link. |
| Page opens and is idle | The page waits for a tap. | Press **Establish Link**. |
| `Link Error`, `Connection failed. Check the server.` | The connection could not be made. | Check that the server is running and the address is right. |
| Page opens but looks unstyled, icons missing | The listener page loads Tailwind, icons and fonts from `cdn.tailwindcss.com`, `code.iconify.design` and `fonts.googleapis.com` in the player's browser. A device with no internet cannot fetch them (how the page then looks is not verified). | Give the device internet access, or use a device that has it. |
| Only this PC connects | `host` is `127.0.0.1` or `::1`. Loopback binds are never forwarded. | Set **Bind Address** to `0.0.0.0` (`host = "0.0.0.0"`) while the server is stopped. |
| Nobody on the LAN connects, macOS | The local-network prompt was answered "Don't Allow". | System Settings > Privacy & Security > Local Network (not verified on a Mac). |
| LAN works, internet players fail | UPnP, second NAT, VPN or firewall. | See the sections below. |

```toml
[server]
host = "0.0.0.0"   # every IPv4 network of this PC
port = 8080
```

### Wrong LAN address

Bardwave collects every address the machine owns and scores them. Private LAN ranges win (`192.168.x.x`, then `10.x.x.x`, then `172.16.0.0/12`, where Docker and WSL usually sit). Then come public addresses, then carrier-grade NAT (`100.64.0.0/10`, which Tailscale uses) as a last resort. Benchmarking (`198.18.0.0/15`, used by proxy TUN adapters), link-local, loopback and reserved ranges are never chosen. On macOS and Linux, adapters with virtual names (for example `utun`, `docker`, `veth`, `br-`, `tun`, `tap`, `wg`, `tailscale`, `zt`) are skipped. On Windows, tunnel and loopback adapters are skipped. A wildcard bind only advertises an address of its own family. If nothing qualifies, the link uses loopback, which players cannot open.

| Symptom | Likely cause | Fix |
|---|---|---|
| Link shows `127.0.0.1` | No usable LAN address was found, or `host` is a loopback address. | Check the network connection. Or type the LAN address of this PC into **Bind Address**. |
| Link shows an address that is not your LAN (for example `172.x.x.x` from Docker or WSL, `100.x.x.x` from Tailscale) | That was the best address found. | Type the address players should use into **Bind Address** while stopped. An explicit address is advertised as it is. Binding to it also restricts the UPnP request to that interface. |
| The link address is right, but players still fail | A firewall, a VPN, or devices on different networks (for example a guest Wi-Fi). | See [Firewall](#firewall) and [VPN and proxy tools](#vpn-and-proxy-tools). |

The `Local ...` line under the Listen URL is not the LAN address. It is always `127.0.0.1` with the port, for opening the page on this PC even when the detection is wrong. The detection sends no packets: it only asks the system which address it would use for a route. You can always build the link by hand: `http://<LAN address of this PC>:<port>/?k=<key>`.

### UPnP fails and second NAT

`[server] upnp = true` is the default. Bardwave asks the router to forward the TCP port in the background (UPnP only, IPv4 only). The LAN link appears at once, and the server never waits for the router. No `Internet` line appears when `upnp = false`, or when `host` is a loopback or IPv6 address.

| Internet line in the console | Meaning | Fix |
|---|---|---|
| `Internet: asking the router…` | The first attempt is running. | Wait a few seconds. |
| `Internet: no UPnP router` | No router answered, or it refused. Bardwave retries every 30 seconds. Hover the line to read the last reason. | Enable UPnP in the router. Forwarding by hand is possible (not verified). |
| `Internet: blocked by a second NAT (<address>)` | The address the router reports is not public (private, carrier-grade NAT and similar), so another NAT sits in front of it. | Forward the port on the upstream device, or ask the provider for a public address. |
| `Internet http://<address>:<port>` (`https://` when HTTPS is on) | The mapping is public. Click the line to copy. | Test from a phone on mobile data. Opening it from inside your own network may fail on routers without NAT loopback. |

The lease asked for is 2 hours. It is renewed at half of the lease the router grants (at least every 60 seconds). A permanent lease is rechecked every 30 minutes. If the router answers error `725` (only permanent leases), Bardwave asks again with a permanent lease. If it answers `718` (the port is already forwarded to another host), Bardwave tries another external port, up to four times, so the internet link may carry a port different from yours. The mapping is removed on **Stop Server**. These router replies are handled in code and in unit tests only (not verified against real routers).

```toml
[server]
upnp = false   # LAN play only; no router requests
```

### VPN and proxy tools

A full-tunnel VPN sends replies to internet players into the tunnel, so they cannot connect. The console shows an amber note when the default route does not leave through the chosen LAN address:

> `VPN active (<address>): internet players can't connect until it is off`

LAN play keeps working. Turn the VPN off, or exclude Bardwave from it, before inviting internet players (not verified with real VPN clients). TUN-mode proxies (Clash, xray, sing-box) use the benchmarking range, which Bardwave never advertises. The LAN link stays correct, but the same note is likely to appear (not verified with those tools). See [Wrong LAN address](#wrong-lan-address).

### Firewall

| Platform | What happens | Fix |
|---|---|---|
| Windows | When the server starts, Bardwave adds an inbound rule named `Bardwave TCP <port>` for TCP, profile `any`. The rule is replaced on each start. Creating it needs administrator rights. The installer can add an equivalent rule for TCP `8080` (private and public networks). | Without administrator rights the console shows `Firewall: port <port> not opened - allow Bardwave for public networks or run as admin`. Windows may ask on the first listen: allow Bardwave for **private and public** networks. |
| macOS | Bardwave adds no firewall rule. The app bundle declares local network use. | Allow incoming connections and local network access when asked (not verified on a Mac). |
| Linux | Bardwave adds no firewall rule. | If a firewall is active, open the listen TCP port yourself (8080 by default), for example with `ufw` or `firewalld` (not verified). |

> **Warning:** Do not switch the firewall off to test. Open only the listen port.

### YouTube download fails

The **YouTube** button in the library opens **Add From YouTube**. Press **Add** after pasting a link. Accepted forms are `watch?v=`, `youtu.be/`, `music.youtube.com/watch?v=`, `/shorts/`, `/embed/`, `/live/` and a bare 11-character id. Playlists, channel pages and other sites are refused, and a link with `&list=` stays one track. One download runs at a time. **Close** leaves the dialog while the download continues in the background. The audio is stored as MP3 (160K) in the `bardwave-library` folder of the data folder, named `<title> [<id>]`. Adding the same video again reuses the file.

| Message | Cause | Fix |
|---|---|---|
| `Not a YouTube video link` | The text is not an accepted form. | Paste a single video link. |
| `yt-dlp not found — downloading it once (~24 MB)…` | `yt-dlp` is fetched from GitHub on first use. | Wait. The download is cached in the `tools` folder of the data folder. |
| `Failed to download yt-dlp: …` or `Downloaded yt-dlp is only N bytes — aborted` | No network, or GitHub returned an error. | Check the connection and retry. |
| `Neither the Bardwave data folder nor the app-data tools folder is writable…` | The tool cannot be saved. | Make the data folder writable, or put `yt-dlp` next to Bardwave or on `PATH`. |
| `This platform has no prebuilt yt-dlp…` | No release asset exists for this OS or CPU. Assets exist for Windows, macOS and Linux. | Install `yt-dlp` and put it on `PATH`. |
| `ffmpeg not found on this system…` | ffmpeg is missing. It is downloaded automatically on Windows only (about 100 MB, BtbN builds). | `brew install ffmpeg` (macOS) or `apt install ffmpeg` (Linux). On Windows, place `ffmpeg.exe` next to `Bardwave.exe`. |
| `yt-dlp failed: …` | YouTube changed, or the video is unavailable or region-locked. The text is the end of yt-dlp's own error output. | Update `yt-dlp` and retry. |
| `Download finished, but no file for <id> appeared in <folder>` | yt-dlp reported success but wrote no file. | Check the `bardwave-library` folder and the log. |

Bardwave downloads `yt-dlp` only when it is missing, and never updates it. It looks in this order: next to the program, then `PATH`, then its own `tools` folder (and an older per-user tools folder from earlier builds). To update, replace that file with a newer build, or delete the copy in the `tools` folder so a fresh one is fetched. The progress lines read `Connecting…`, `Fetching video info…`, `Downloading audio… N%` and `Converting to MP3…`. The downloaded tools are checked only for a minimum size, not against a hash or signature. The download flow on macOS and Linux, and the Windows ffmpeg download, are not verified on real machines.

```bash
pip install -U yt-dlp   # if yt-dlp came from pip
brew install ffmpeg     # macOS, when ffmpeg is missing
```

### Missing files

Files that moved stay in the library and show as missing: the library shows an amber banner with the count, and each such sound has a warning. Press **Locate…** on a sound (or `LOCATE…` on its row) to pick its new file. Press **Replace Path Prefix…** in the banner to rewrite the start of every path when a whole folder moved (it shows how many paths will change). **Recheck** looks for the files again. The cross icon removes the sound from the library. Scenes, playlists and hotkeys refer to sound ids, not paths, so they keep working after a relink.

### Settings are not remembered

`config.toml` lives in the data folder, which depends on the platform:

| Situation | Folder |
|---|---|
| `config.toml` next to the program (on Windows only if that folder is writable), or a `portable.flag` file next to it, or the program's folder is writable (and not inside a macOS `.app`) | The program's folder |
| macOS, otherwise | `~/Library/Application Support/Bardwave` |
| Windows, otherwise (for example `Program Files`) | `%APPDATA%\Bardwave` |
| Linux and other Unix, otherwise | `$XDG_DATA_HOME/bardwave` or `~/.local/share/bardwave` |

The choice is made at start (not verified on every OS). With `--profile <name>`, `config.toml`, the library, `bardwave-library` and the `tls` folder move to `profiles/<name>/` inside that folder. The log, the `tools` folder and `recent.toml` stay in the root. A copy of the program in a writable folder (a download folder, a USB stick) keeps its own `config.toml` next to it, so another copy of the same program may show different settings.

Settings look reset? Open the data folder from the **Profile** menu in the top bar (**Open data folder**), and check which `config.toml` you edit. Keep one. A `portable.flag` file next to the program keeps the data beside it.

If `config.toml` cannot be read, Bardwave shows `config.toml could not be read` (a message on the console, an error on stderr with `--server-only`) and stops. It does not start with defaults, because the next save would overwrite your library. Fix the file, or move it away to start with defaults.

### Gatekeeper on macOS

An unsigned build is blocked the first time it is opened. The bundle gets an ad-hoc signature so it runs on Apple Silicon, but signing and notarization have not been run for this project (not verified).

1. Do not double-click `Bardwave.app`.
2. Right-click (Control-click) it, choose **Open**, then **Open** again.
3. On macOS Sequoia and newer, if it is still refused: try to open it once, then System Settings > Privacy & Security > **Open Anyway**.
4. Or clear the quarantine flag for a browser download:

```bash
xattr -dr com.apple.quarantine /Applications/Bardwave.app
```

More steps are in [packaging/macos/README.md](../../packaging/macos/README.md).

## FAQ

**Do I need HTTPS for a home game?**
No. `[server] tls` is `false` by default, and the LAN works over HTTP. Turn HTTPS on before you send links across the internet. See [HTTPS](#https).

**Why does the link carry `?k=`?**
That is the listener key. Without it the stream is refused.

**Can players open the GM panel with their link?**
No. The listener key never opens the panel's data. A player can load the empty page shell at `/control`, but every call that reads or changes anything needs the game-master token and is answered `401`.

**How do I cut off someone who has the link?**
Press the refresh icon in the ACCESS KEY row, then send the new link.

**Can I run with no listener key?**
Yes, with `access_token = ""`. The console then shows `ACCESS KEY OFF - ANYONE CAN LISTEN`. Use this only on a network you trust.

**Does Bardwave check for updates by itself?**
No. The check is off by default. **Check for updates** asks GitHub when you press it. The checkbox **Check once a day at start** (`[gui] check_updates = true`) turns on an automatic check at start, at most once a day. Nothing is downloaded or installed. The check has not been run against a published release (not verified).

**Does the master volume persist?**
Only the `[audio] master_volume` value in `config.toml` is used at start. The console fader does not save its position.

**Can I change the port while the server runs?**
No. **Port** and **Bind Address** are editable only while the server is stopped, and apply on the next start.

**Do players need internet access?**
Not for the stream on a LAN. Their browser does fetch styling, icons and fonts for the listener page from public CDNs (see [What leaves the machine](#what-leaves-the-machine)). How the page behaves with no internet at all is not verified.

**What do players see of my library?**
The audio, and the name of the active scene and the music track (cut to 120 characters, control characters removed). Never file paths or library ids.

**Is there a limit on listeners?**
The code has no cap on listeners. Behaviour with many listeners has not been tested (not verified).

**Can I run without a window?**
Yes: `--server-only` (also `--profile <name>`). Hotkeys and `[[timers]]` do not run there. The log prints the player link with its key and the local control page. See [Configuration reference](./configuration.md).

## Security model

### Two keys: listener key and game-master token

| | Listener key | Game-master token |
|---|---|---|
| Config key | `[server] access_token` | `[server] gm_token` |
| Created | On first start, if missing | On first start, if missing |
| Carried by | `http://<host>:<port>/?k=<key>` | `http://<host>:<port>/control?g=<token>`, or `Authorization: Bearer <token>` |
| Opens | The audio stream (`/ws/audio`) and the status API (`/api/status`) | The data and commands of the GM panel (`/api/control/*`, `/ws/control`) |
| Can do | Listen. The status API reports the listener count and the version. | Play, stop and loop sounds, set volumes, the master and the buses, scenes, stingers, ducking, Stop all, and the music playlist. Read the board state (names of sounds, scenes and playlists, volumes). |
| Cannot do | Anything on the panel. | Add or remove files, change settings, start or stop the server, change keys, read files or paths. |
| Empty value | `""` turns the check off: anyone can listen | `""` turns remote access off. The panel answers on this PC only. The console shows `Remote panel is off: it opens on this computer only` and a **Create GM key** button. |
| Replace | Refresh icon in the ACCESS KEY row | **New key**, then **Replace key** (**Cancel** keeps the old one) |

The page shells (`/`, `/control` and their scripts and styles) are public and hold no secret. Everything that reads or changes the board needs the token. Keys are 16 characters from `a-z` and `0-9`, about 82 bits, from the operating system's random source. Comparisons run in constant time. The console shows the GM link masked, with the warning **Do not share with players**, and offers **Show QR** for a phone. The responses of the panel carry `Cache-Control: no-store` and `Referrer-Policy: no-referrer`, because the token can sit in the URL. The GM panel is served by the console window while the server runs, and by `--server-only`.

```toml
[server]
access_token = "replace-with-generated-key"  # "" turns the listener check off
gm_token = "replace-with-generated-token"    # "" disables remote panel access
```

> **Warning:** Both keys sit in plain text in `config.toml`. Keep that file private, and never post a link that contains a key. Over plain HTTP the keys cross the network in clear text: turn on [HTTPS](#https) for the internet.

### Local access

A request counts as local only when its socket peer is loopback, it carries no proxy header, and its `Host` is `localhost`, `127.0.0.1` or `[::1]`. Local requests reach the panel without a token, with two extra checks:

- Every REST action must send the `X-Bardwave-Control` header. Without it the server answers `403 missing control header`. A page on another site cannot add that header without a CORS preflight, which the server never approves. A request with the token proves intent by itself and needs no header.
- A local WebSocket handshake that carries an `Origin` must have one that matches its `Host`. Otherwise the answer is `403 cross-origin request`. A handshake without `Origin` (not a browser page) passes.

The `Host` check blocks DNS rebinding: a page on another name cannot use this PC's no-token access. A phone on the same LAN is not local and needs the game-master token.

### Rate limiting

Only the game-master token is rate limited. The listener key has no lockout, so its strength is the 16 random characters.

| Rule | Value | Response |
|---|---|---|
| Wrong or missing token | Counted per address | `401` with `game master token required` |
| Failed attempts | 10 within 60 seconds | `429` with `Retry-After` |
| Through a proxy on this PC | 60 failures within 60 seconds | `429` |
| IPv6 | Counted per /64 prefix | As above |
| Locked address | Even the right token is refused until the window passes | `429` |

The answer is the same for a wrong token, a missing token and a disabled remote panel. On `/ws/control`, a message may be 4096 bytes, at most 16 clients may connect (the 17th gets `503 too many control clients`), and each client may send 100 commands per second (more are ignored, with one `too many commands, slow down` error per second). Idle clients are pinged every 25 seconds and dropped after about 75 seconds of silence.

### Proxies and tunnels

A request with a proxy header counts as remote, and so does one whose `Host` is not a loopback name. Sixteen headers are checked, among them `X-Forwarded-For`, `X-Forwarded-Host`, `X-Forwarded-Proto`, `X-Real-IP`, `Forwarded`, `CF-Connecting-IP`, `True-Client-IP`, `X-Client-IP` and `Via`. Forwarded addresses are never believed. They can only make a request less trusted.

A tunnel on this PC never gets local access, and with no token it gets nothing. Plain TCP tunnels (`ssh -R`, bore, frp) and proxies that rewrite `Host` to `127.0.0.1` look like the game master's own browser, so use them only with `gm_token` set. Clients behind one proxy share a rate-limit counter, so one hostile client can lock out the others. Rotating the token, or using the panel on this PC, is the way out. These rules are covered by unit tests with simulated headers. Real tunnel software was not tried (not verified).

### Rotating keys

- **Listener key:** press the refresh icon in the ACCESS KEY row. Listeners on the old key are disconnected at once (close code `4001`), and their page says the link is no longer valid.
- **Game-master token:** press **New key**, then **Replace key**. Remote panels on the old token are closed (close code `4003`). The players' link is not touched. A panel open on this PC without a token stays connected.
- Both new keys are saved to `config.toml` with the next config flush.
- Rotate both after a link leaks. A key can also leak through the log: with `--server-only`, the start-up line `Listener server available at ...` contains the player link with its key. The GM token is never printed.

### Bundle import

A library bundle (`.zip` with `bundle.toml`, format `bardwave-bundle`, written by **Export…** and read by **Import…**) can come from anyone, so the importer treats it as hostile:

- An entry with an absolute path, a `..` or `.` part, an empty part, a backslash, a colon, a control character or a symbolic link rejects the whole archive. The paths inside `bundle.toml` are checked the same strict way.
- Files are written only inside the chosen target folder, and each parent folder must resolve inside it.
- Only files named by the manifest are extracted, and only with the extensions `wav`, `flac`, `mp3`, `ogg`, `m4a` and `aac`. Any other file is skipped with a warning, and the sound is imported without audio.
- Limits: 20,000 entries, 100,000 items, a 16 MB manifest, 2 GB per file and 8 GB in total. Declared sizes are checked first, and the bytes actually read are counted.
- A conflict is resolved by the policy you choose: **Keep mine** (skip), **Keep both** (rename) or **Use the bundle's** (replace, which also overwrites existing audio files in the target folder). A dry run writes nothing. If an import fails or is cancelled, the files it created are removed and the library is untouched (a file replaced under **Use the bundle's** cannot be restored).
- Hotkeys are not part of a bundle.

### What leaves the machine

Nothing is sent to anyone by default, apart from what the server serves to the players you invite and the router request. The update check is off until you turn it on. Outbound web requests in the code come only from the update check and the tool downloads. There is no telemetry code.

| Request | When | Destination |
|---|---|---|
| Listener stream | Players connect | Devices that reach the port: the LAN, or the internet if the port is mapped or forwarded. Opus audio, plus the scene and track names. |
| UPnP discovery and mapping | `upnp = true` (default) | Multicast on the LAN, a probe to the usual gateway address, then your router |
| Update check | **Check for updates**, or `check_updates = true` | `api.github.com` latest-release document, User-Agent `Bardwave/<version>`, 8 second limit, 256 KiB cap. The release page opens in your browser only on click. |
| yt-dlp download | First YouTube use, if the tool is missing | GitHub release assets of yt-dlp |
| ffmpeg download | First YouTube use on Windows, if the tool is missing | GitHub release assets of the BtbN FFmpeg builds |
| YouTube audio | You add a YouTube link | YouTube, through yt-dlp |

Two more things to know. Address detection sends no packets. And the listener page makes the player's browser, not Bardwave, request `cdn.tailwindcss.com`, `code.iconify.design` and `fonts.googleapis.com`. Those hosts see the player's IP address. Whether the browser also sends the page address is a browser setting (not verified). The GM page loads only its own files.

### HTTPS

`[server] tls = true` serves HTTPS and `wss://` on the same port, and links start with `https://`. Without `tls_cert` and `tls_key`, Bardwave makes a self-signed certificate, valid for 397 days, in the `tls` folder of the data folder. At the next Start it is replaced when fewer than 30 days remain, or when none of this machine's current LAN addresses is covered. The SHA-256 fingerprint is shown in the console and logged, so you can compare it with the browser warning. **Regenerate certificate** makes a new one. The change applies on the next Start. Both `tls_cert` and `tls_key` must be set together. A certificate that cannot be read stops the server from starting, instead of falling back to HTTP.

```toml
[server]
tls = true
tls_cert = "/etc/ssl/bardwave/fullchain.pem"   # set both or neither
tls_key = "/etc/ssl/bardwave/privkey.pem"
```

> **Note:** Each phone must trust a self-signed certificate. That has not been tested on a real iPhone or Android phone. Full steps are in [./https.md](./https.md).

## Not verified on real hardware

The following are implemented and covered by tests or CI builds, but have not been run on real machines:

- Windows installer and Linux packages (AppImage, `.deb`).
- macOS `.app` and `.dmg`, the local-network prompt, Gatekeeper, signing and notarization.
- Global hotkeys on macOS, Windows and Linux/X11.
- MIDI: unit tests only, off by default, not connected to the console.
- Audio device names, enumeration and the second (preview) stream on Windows (WASAPI) and Linux (ALSA/JACK). Switching devices was run on macOS only.
- The choice of the data folder on each OS.
- HTTPS: automated tests only. Not trusted on a real phone, not tried with a real Let's Encrypt chain.
- UPnP against real routers (including the `718` and `725` replies and the second-NAT case), and Windows firewall rule creation.
- The VPN warning with real VPN clients and TUN-mode proxies.
- The proxy and tunnel rules with real tunnel software. Only simulated headers were tested.
- The listener page on iOS Safari and Android Chrome (including how it looks without internet access).
- The update check against a published release.
- The yt-dlp and ffmpeg downloads on macOS and Linux, and the ffmpeg download on Windows.
- Behaviour with many listeners at once.
- Buffer and dropout advice is derived from the code, not from live sessions.
- Profiles, the first-run wizard and the interface scale on Windows and Linux.
