# Network, players and the GM remote panel

Bardwave streams the console's output to listeners over the local network or the internet. This page covers how the server is started and reached, how players join with the access key, what the listener page shows, and how the game master (GM) controls the console from another device through the remote panel. For the HTTPS setup, see [HTTPS](./https.md). Related pages: [Getting started](./getting-started.md), [Configuration](./configuration.md), [Troubleshooting and security](./troubleshooting-security.md).

Two secrets are involved. The **access key** lets players listen. The **GM token** lets someone control the console. The access key never opens the GM panel.

## Contents

- [Starting and stopping the server](#starting-and-stopping-the-server)
- [LAN or internet](#lan-or-internet)
- [UPnP port forwarding](#upnp-port-forwarding)
- [Player link and access key](#player-link-and-access-key)
- [Listener page](#listener-page)
- [GM remote panel](#gm-remote-panel)
- [Headless /control page](#headless-control-page)
- [HTTPS](#https)
- [Configuration reference](#configuration-reference)
- [Symptoms and fixes](#symptoms-and-fixes)
- [Not verified](#not-verified)

## Starting and stopping the server

The server does not start with the application. The GM starts it from the **Network Server** panel (the right-hand column of the console, headed `02 / Network`). In `--server-only` mode there is no window and the server starts by itself, see [Headless /control page](#headless-control-page).

1. Check **Port** and **Bind Address** in the **Configuration** block at the bottom of the panel while the server is stopped.
2. Press **Start Server**.
3. Press **Copy Link** (or the copy icon next to **Listen URL**) and send the link to players.
4. Press **Stop Server** to take the stream off air.

The status card shows the state in capital letters:

| Status | Meaning |
| --- | --- |
| `Live` | Running. Below it: `Broadcasting to N listeners`. |
| `Starting` | `Binding the listen socket`. The button is disabled. |
| `Error` | The last start failed. The reason is shown, shortened to 34 characters. The full text goes to the log. The button becomes **Retry Server**. |
| `Off Air` | Stopped. Below it: `Server stopped`. |

The **Listeners** block shows the current count and `Peak` (the highest count since the server was started).

**Port** accepts 1 to 65535. Port `0` is rejected (`Port must be 1–65535`). **Bind Address** accepts an IP literal (IPv4 or IPv6), not a host name (`Bind must be an IP address`). Invalid input keeps the last valid value and shows the error under the fields. A valid edit is applied at once and saved to `config.toml` (a `Saved` mark appears). Both fields are editable only while the server is stopped; the hint under them reads `Editable while stopped` or `Stop the server to edit`.

Stopping the server disconnects every listener and every remote GM session, removes the router port mapping and clears the VPN and firewall notes. Listener pages that were open keep retrying and rejoin when the server is started again.

## LAN or internet

The advertised address depends on the bind address:

| Bind address | Advertised address |
| --- | --- |
| `0.0.0.0` or `::` (default is `0.0.0.0`) | A detected LAN address of the same family as the bind address. If none is found, loopback (`127.0.0.1` or `[::1]`). |
| Loopback (`127.0.0.1`, `::1`) | Loopback only. Only this computer can connect. |
| Any other IP literal | That address, as typed. |

When the address is detected, private LAN addresses win over public ones. Among IPv4 LAN addresses `192.168.x.x` is preferred to `10.x.x.x`, and that to `172.16.0.0/12` (where Docker, WSL and many VPN adapters live). Benchmarking-range addresses (`198.18.0.0/15`, used by some VPN and proxy adapters), link-local addresses, loopback and other unusable ranges are never picked. Carrier-grade-NAT addresses such as Tailscale's (`100.64.0.0/10`) are a last resort before loopback.

Players on the same network use the LAN link. Players elsewhere need the **Internet** link, which exists only when the router forwards the port, see [UPnP port forwarding](#upnp-port-forwarding).

The **Listen URL** block shows the advertised link together with the access key (shortened to 30 characters on screen). When the advertised address is not loopback, a `Local` line below it shows the loopback link for this computer. Click it to copy that link with the access key.

> **Warning:** The listener key travels in the URL. Before you send players a link over the internet, enable [HTTPS](#https).

## UPnP port forwarding

With `upnp = true` (the default), the server asks the router to forward the listen port (TCP, UPnP IGD). The request runs in the background and never delays the start. The LAN link appears at once, and an internet line is added only if the router grants a mapping. Discovery is sent from every private IPv4 address of the computer, so it also works while a VPN owns the default route (not verified on real routers).

| Line in the Listen URL block | Meaning |
| --- | --- |
| `Internet: asking the router…` | The router is being asked. |
| `Internet http://<public-address>:<port>` | A mapping to a public address (`https://` when HTTPS is on). Click to copy it with the access key. The external port can differ from the local one when another machine on the router already forwards that port. |
| `Internet: blocked by a second NAT (<address>)` | The router's external address is private. |
| `Internet: no UPnP router` | No router answered, or none granted a mapping. Hover the line for the last reason. The server retries every 30 seconds. |

No `Internet` line at all means port forwarding is off or not applicable (see **Loopback or IPv6 bind** below).

Caveats:

- **Second NAT.** Carrier-grade NAT or a provider modem in front of the router makes the external address private. Ask the provider for a public IP, or forward the port on the upstream device.
- **VPN.** When a full-tunnel VPN takes the default route, the router still forwards the connection, but the reply leaves through the tunnel. The panel then shows `VPN active (<address>): internet players can't connect until it is off`. Turn the VPN off, or exclude Bardwave from it. The warning is raised when the default-route address differs from the advertised IPv4 LAN address.
- **Firewall.** On Windows the server tries to add an inbound allow rule for the port (`netsh`, all network profiles), which needs administrator rights. If that fails, the panel shows `Firewall: port <port> not opened - allow Bardwave for public networks or run as admin`. Windows often marks networks as Public, so allow Bardwave there too. On other systems Bardwave does not touch the firewall; if one is active, open the port yourself (not verified).
- **Lease.** Bardwave asks for a two-hour lease and renews it at half-time. If the router insists on permanent mappings, it re-checks every 30 minutes. **Stop Server** removes the mapping.
- **Loopback or IPv6 bind.** Port forwarding is skipped: routers forward IPv4 only, and a loopback socket is unreachable from outside. With a specific IPv4 bind address the mapping points at that address only.
- **Testing.** Routers without NAT loopback may refuse the internet link from inside your network. Test it on mobile data.
- **Turning it off.** Set `upnp = false` in `config.toml` and restart the application.

> **Note:** If UPnP fails, forwarding the port by hand on the router also works, but that path is not covered here (not verified).

## Player link and access key

The player link has this form (`https://` when HTTPS is on):

```text
http://<advertised-address>:<port>/?k=<access-key>
```

The access key comes from `[server] access_token` in `config.toml`:

- A key is generated and saved on the first launch if the config has none.
- `access_token = ""` (or only spaces) turns the check off. The panel then shows `ACCESS KEY OFF - ANYONE CAN LISTEN`, the link has no `?k=`, and the log warns that the stream is open.
- Generated keys are 16 characters from lowercase letters and digits. Keys are case-sensitive.
- The panel shows `ACCESS KEY` and the first characters of the key. The refresh icon next to it issues a new key at once, without a confirmation question (it also switches an empty key back on). Its tooltip warns that the current link stops working at once and everyone listening is disconnected. Share the new link afterwards. The new key is saved to `config.toml`.

The key protects the audio WebSocket (`/ws/audio`) and the status API (`/api/status`). The listener page and its static files (scripts, icons, the manifest) are public.

## Listener page

Players open the link in a browser. The page title is `Bardwave Listener`. It plays the same mix as the console, and nothing is installed on the player's side.

The page starts with a boot animation of 6 to 10 seconds (about 0.3 s when the device asks for reduced motion). Listening never starts by itself: the player has to press the connect button.

> **Note:** The page loads Tailwind CSS, the Iconify component and the JetBrains Mono font from public CDNs (`cdn.tailwindcss.com`, `code.iconify.design`, `fonts.googleapis.com`). A device without internet access may show a broken or unstyled page; how it looks on an offline LAN has not been tested (not verified).

### Connecting

The connect button changes with the state:

| Button text | When |
| --- | --- |
| `Establish Link` | Idle. Press to connect. |
| `Linking...` | Connecting. Disabled. |
| `Stop Retrying` | Waiting to reconnect. Press to stop. |
| `Terminate Link` | Connected. Press to disconnect. |

The status text shows `Node Offline`, `Linking...`, `Reconnecting #<n>`, `Node Online`, `Access Denied` or `Link Error` (and `Audio Error` when the browser refuses to start audio).

Before it opens the stream the page asks `/api/status` with the key. If the server rejects the key, the page shows `Access Denied`, stops retrying, and explains: after a key change `This link is no longer valid: the GM issued a new access key. Ask for the current link.`, and for a link without a key `This link has no access key. Open the full link from the GM (it ends with ?k=...).`

### Reconnecting

After a drop, the page retries with a delay of 1 s, 2 s, 4 s, 8 s, then 15 s. Each delay is reduced by up to 30 % at random. The backoff resets only after a link has stayed up for 10 s. A handshake longer than 10 s is abandoned, and a link silent for 30 s is dropped (the page pings every 10 s). Retries have no limit. When the tab becomes visible again or the browser reports that the network is back, the page retries at once instead of waiting out the delay. When the GM stops the server, pages show `Reconnecting #<n>` until it is started again.

### Personal volume

**My_Volume** sets this device's own volume from 0 % to 100 %. It does not change the GM's mix or any other player. Press the percentage button to mute and unmute; it shows `MUTED` while muted. The slider is perceptual: the gain is the square of the percentage. The setting is saved in the browser (`localStorage`, key `bardwave.volume`), so it applies only to that browser. The bus levels cannot be changed per player, because everyone gets one mixed stream.

### Link quality

The **[ LINK_QUALITY ]** panel shows **Quality**, **Latency (est.)**, **Buffer**, **Packet loss (10 s)** and **Screen stay-awake**. Quality reads `GOOD`, `FAIR` or `WEAK` while audio flows, and `IDLE`, `WAITING`, `RECONNECTING` or `OFFLINE` otherwise. Packet loss is counted over the last 10 seconds and shows `--` until at least 50 packets were seen.

`FAIR` appears on loss of 0.5 % or more, arrival jitter of 30 ms or more, or any buffer underrun in the window. `WEAK` needs sustained trouble, not a single burst: loss of 2 % or more spread over at least 3 seconds, jitter of 80 ms or more, or 3 or more underruns in the window, for 3 evaluations in a row (one per second). It clears after 5 clean evaluations. Then the page shows: `Weak network: audio may stutter or drop out. Move closer to the router or switch to a better connection.` The page also shows live level meters, a buffer display and counters for underruns, overruns and lost frames.

### Now playing

When the GM plays a scene or a music track, a **Now_Playing** line shows the scene name and, below it, the track title. It is hidden when nothing is playing. The same text goes to the phone's lock-screen media controls.

### Screen stay-awake, lock screen and PWA

The page asks the browser for a screen wake lock while listening. **Screen stay-awake** shows `ON`, `PAUSED` (the browser released it), `BLOCKED` (refused, for example by battery saver), `N/A (needs https)` on plain `http://`, `N/A` when the browser has no such API, and `OFF` or `--` when not listening. The browser releases the lock when the tab is hidden, and the page takes it again when the tab returns. How phones and tablets behave has not been tested (not verified).

The page registers lock-screen media controls (Media Session) with the scene and track as the title. A live stream cannot be paused in place: pause or stop on the lock screen leaves the link, and play joins it again. To make the widget appear, the page plays a silent looping sound; it carries no audio.

The page is installable as a home-screen app (name `Bardwave Listener`, standalone display, portrait). The page asks for the manifest with the same `?k=` key, and the manifest echoes it into its start URL, so the home-screen icon opens with the key. Only keys of letters, digits, `-` and `_` up to 64 characters are echoed; a hand-made key with other characters opens the icon without a key, and the player gets `Access Denied`. There is no service worker, so the page does not work offline. Installing may need a secure context, see [HTTPS](./https.md) (not verified).

## GM remote panel

The GM panel is the `/control` page (the phone tab title is `Bardwave GM`). It lets the GM operate the console from a phone or another computer while the server runs. The console window serves it, and so does headless mode. Commands from the phone run through the same path as a click in the console window, so both always show the same state.

### The GM token

The panel is protected by `gm_token` in `config.toml`, which is separate from the access key.

- A token is generated and saved on the first launch if the config has none. It has the same form as the access key (16 characters from lowercase letters and digits).
- `gm_token = ""` disables remote access, and the panel opens on this computer only. The console shows `Remote panel is off: it opens on this computer only` with a **Create GM key** button, which makes a key so the panel can be opened from a phone.

The console's **GM panel link** block shows the link without the scheme and with the token cut to four characters. Before the server runs it says `Start the server to get the link`. Clicking the link copies the full link. The caution text reads `Do not share with players: this link controls the console`. **Show QR** draws a QR code of the link (it is hidden by default, because the screen may be in view of the players, and the button is disabled while the server is stopped). **Hide QR** removes it. **New key** asks first: `The current link stops working at once and every phone using it is disconnected. Players' links are not affected.` **Replace key** issues a new token and saves it. The old link and its phone sessions stop at once. **Cancel** keeps the current key.

The link has this form (`https://` when HTTPS is on):

```text
http://<advertised-address>:<port>/control?g=<gm-token>
```

### Who can open the panel

| Request | Token needed | Result |
| --- | --- | --- |
| From this computer, without a proxy, with a loopback `Host` | No | Allowed. The page sends the `X-Bardwave-Control` header. |
| From anywhere else, or relayed by a proxy or tunnel | Yes | Allowed with the token. Otherwise `401`. |
| With the listener (player) key | Not accepted | `401`. |
| `gm_token = ""`, from anywhere else | Not possible | `401`. |

"Local" is decided from the socket address. `X-Forwarded-For` is never trusted. A request is treated as remote when it carries a reverse-proxy header (`X-Forwarded-*`, `Forwarded`, `X-Real-IP`, `Via`, `CF-Connecting-IP` and similar), or when its `Host` is not `localhost`, `127.0.0.1` or `[::1]`. Wrong, missing and disabled tokens all get the same `401`.

The token can be sent as `Authorization: Bearer <token>` or as `?g=<token>`. The phone page moves the token from the address into `sessionStorage` (key `bardwave.gm.token`) and removes it from the address bar at once. A reload in the same tab finds it again; a new tab needs the full link again. The page shell (`/control`, `/control.js`, `/control.css`) is served to anyone, because it holds no secret, so a reload without the token still opens it and then asks you to open the link from the console.

### What works from a phone

With the token, the phone page offers the console's controls:

- **Master volume**, **Duck** (it reads `Ducked` while ducking, with a `Ducking: <buses>` note) and **Stop all**;
- **Scenes** and **Stop scene**;
- **Stingers**;
- **Music**: the playlists, **Prev**, **Play** (it reads `Pause` while playing), **Next**, **Stop**, **Shuffle**, **Repeat** (`Repeat: off`, `all` or `one`), a track position bar, and a line with the current track;
- **Mixer**: the buses **Music**, **Ambience** and **Effects**;
- **Sounds**: per sound, a **Play** button (it becomes **Stop** while playing), a **Loop** toggle and a volume slider.

A status line shows `Connected`, `Connecting…` or `Offline, retrying…`. The page reconnects by itself with a delay of 0.5 s growing to 10 s. Several devices can be connected, and all of them see every change. If the token is replaced, the page says `The game master link was replaced. Open the new link from the server panel.` and stops. Other messages are `Access denied. Open the game master link from the console again.` and `Open the link from the console` (no token). The panel hides sections that have nothing to show (no scenes, no stingers).

### Why the player key does not give control

The listener key is checked only on `/ws/audio` and `/api/status`. The panel routes (`/api/control/*`, `/ws/control`) never accept it, in the header, in `?k=` or in `?g=`, and the GM token is not a player key either. A player who has the listener link can hear the stream but cannot press any control. Only someone holding the GM token, or sitting at this computer, can.

### Rate limits

- Each client address gets 10 failed attempts per 60 seconds (an IPv6 `/64` counts as one client). After that the server answers `429` with `Retry-After`, and even the right token is refused until the window clears. The phone page then shows `Too many attempts. Wait <n> s.` Requests from this computer without a proxy are not limited.
- Requests relayed by a proxy on this computer share one counter, which allows 60 failures per minute. A hostile client behind the same proxy can still delay others. Rotating the GM token, or using the panel locally, is the way out.
- `/ws/control` accepts up to 16 clients at once (the next one gets `503`), messages of 4096 bytes, and 100 commands per second per client (extra commands are dropped with a `too many commands, slow down` message). A client silent for 75 s is closed (the server pings every 25 s).
- A local WebSocket with an `Origin` that is not the page's own host is refused (`403`), and a local REST action without the `X-Bardwave-Control` header is refused (`403`), so a web page from another site cannot drive the panel from the GM's browser.

> **Warning:** Over plain HTTP, the GM token crosses the network in clear text. Enable [HTTPS](./https.md) before you use the panel from a phone over the internet.

> **Warning:** A plain TCP tunnel (`ssh -R`, bore, frp and similar), or a proxy that rewrites `Host` to `127.0.0.1`, looks like local access. Use such tunnels only when a `gm_token` is set.

## Headless /control page

With `--server-only` the application runs without a window. It loads the library saved in `config.toml`, starts the listener server by itself, and runs until Ctrl+C. The control page is then the only control surface. Hotkeys and session timers do not run in this mode, and a library changed in the console needs a restart of the server to be picked up.

```bash
soundshared --server-only
soundshared --server-only --profile <name>
```

`soundshared` is the name of the file a `cargo build` produces. The Linux `.deb` package installs it as `bardwave`. The README uses `Bardwave.exe` for the Windows release (not verified). `--profile <name>` (or `--profile=<name>`) selects a campaign profile with its own `config.toml` and library under `profiles/<name>/` in the data folder.

The log shows the addresses. The token itself is never logged:

```text
Listener server available at http://<advertised-address>:<port>/?k=<access-key>
Control page (this PC only): http://127.0.0.1:<port>/control
Remote control: http://<advertised-address>:<port>/control?g=<gm_token from config.toml>
```

The `Remote control` line appears only when a GM token is set. The scheme is `https` when HTTPS is on. When the router grants a mapping, a line `Router forwards ... internet URL ...` is added to the log.

`GET /control` serves the page to anyone, because the page holds no secret. The data routes need the token or a local request. A local request without a token must also send an `X-Bardwave-Control` header (any value) on every call, `GET` included; a request with the token needs no such header.

| Route | Purpose |
| --- | --- |
| `GET /api/control/state`, `GET /api/control/scenes` | Board state and scenes. |
| `POST /api/control/play`, `stop` (`?id=`), `stop_all` (`?fade_ms=` optional), `volume` (`?id=&value=`), `loop` (`?id=&on=`), `master` (`?value=`), `stinger` (`?id=`), `duck` (`?mode=` or `?on=`), `bus` (`?bus=music\|ambience\|sfx&value=`) | Sound and mixer commands. |
| `POST /api/control/scene/activate` (`?id=`), `scene/stop` | Scenes. |
| `POST /api/control/playlist/<verb>` | Music. `<verb>` is `start`, `stop`, `pause`, `resume`, `toggle`, `next`, `prev`, `seek`, `shuffle` or `repeat`. |
| `GET /ws/control` | Live state. Commands are sent as JSON frames with a `cmd` field, such as `play` or `volume`. |

## HTTPS

With `tls = true`, the same port serves HTTPS and `wss://` instead of plain HTTP, so the access key and the GM token are encrypted on the way. Without `tls_cert` and `tls_key`, a self-signed certificate is generated, and a certificate that cannot be read makes the start fail with `Error` instead of falling back to HTTP. The setting is the **Secure connection (HTTPS)** section of the panel, it takes effect on the next **Start Server**, and [HTTPS](./https.md) covers the certificate setup and the browser warning.

## Configuration reference

All keys are in the `[server]` table of `config.toml`.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `port` | integer | `8080` | Listen port, `1` to `65535`. |
| `host` | string | `"0.0.0.0"` | Bind address, an IP literal. A value that is not an IP literal falls back to `0.0.0.0`. |
| `upnp` | boolean | `true` | Ask the router to forward the port. |
| `access_token` | string | generated on first launch | Listener key. `""` opens the stream to anyone who can reach the port. |
| `gm_token` | string | generated on first launch | GM panel key. `""` allows access from this computer only. |
| `tls` | boolean | `false` | Serve HTTPS and `wss://` on the same port. |
| `tls_cert` | string | unset | Path to a PEM certificate chain. Needs `tls_key`. |
| `tls_key` | string | unset | Path to the PEM private key for `tls_cert`. |

```toml
[server]
port = 8080
host = "0.0.0.0"
upnp = true
access_token = "k3x9q2w7e1r4t6y0"
gm_token = "p8m2n5b1v7c3x9z4"
tls = false
```

With your own certificate:

```toml
[server]
port = 8443
host = "0.0.0.0"
upnp = true
tls = true
tls_cert = "/etc/ssl/bardwave/fullchain.pem"
tls_key = "/etc/ssl/bardwave/privkey.pem"
```

The application reads `config.toml` when it starts, so a hand edit of any key needs a restart. After that, `port` and `host` can be changed in the panel while the server is stopped (and are saved), the new values apply to the next **Start Server**, and `tls`, `tls_cert` and `tls_key` are read at every start from the settings the running application holds. `upnp` is read only at launch.

The config is stored in the data folder. If a `config.toml` already sits next to the executable (on Windows only when that folder is writable), or there is a `portable.flag` file there, or the executable's folder is writable and not inside a macOS `.app`, that folder is used. Otherwise it is `~/Library/Application Support/Bardwave` on macOS, `%APPDATA%\Bardwave` on Windows, and `$XDG_DATA_HOME/bardwave` or `~/.local/share/bardwave` on Linux. A named profile keeps its `config.toml` in `profiles/<name>/` inside that folder.

## Symptoms and fixes

| Symptom | Likely cause | What to do |
| --- | --- | --- |
| `Access Denied` on the listener page | Old or missing key in the link. | Copy the current link from **Copy Link**. |
| Listen URL shows `127.0.0.1` | No LAN address was detected, or the bind address is loopback. | Stop the server, set **Bind Address** to `0.0.0.0` or the LAN IP, then start it again. |
| Players on the LAN cannot connect | Firewall. | On Windows, allow Bardwave (for public networks too) or run as administrator. On other systems open the port in the firewall yourself (not verified). |
| `VPN active (...)` | The default route goes through a VPN. | Turn the VPN off for internet players. |
| `Internet: no UPnP router` | No UPnP on the router. | Enable UPnP, or forward the port by hand (not verified). |
| `Internet: blocked by a second NAT (...)` | Carrier-grade NAT or an extra modem. | Ask the provider for a public IP. |
| `Remote panel is off...` | `gm_token = ""`. | Press **Create GM key**. |
| `401` or `Access denied` on the phone panel | Missing or wrong GM token. | Open the GM panel link from the console again. |
| `429` or `Too many attempts` on the phone panel | Too many failed attempts. | Wait for the `Retry-After` time. |
| `The game master link was replaced` | **Replace key** was pressed. | Open the new GM link from the console. |
| `Weak network: ...` hint | Loss or jitter on the listener's connection. | Move closer to the router or change connection. |
| Stay-awake shows `N/A (needs https)` | Plain `http://` page. | Enable [HTTPS](./https.md). |

## Not verified

These points come from the code and tests but were not checked on real hardware or real networks:

- The Windows firewall rule on a real machine, and what other systems do with their own firewalls.
- UPnP on real routers, including the second-NAT case and routers that only accept permanent mappings.
- The listener page on iOS Safari and Android Chrome: wake lock, lock-screen controls, home-screen install, and how the page looks on a LAN without internet access.
- Scanning the QR code with a phone camera.
- The Windows executable name of a release build.
