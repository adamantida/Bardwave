# HTTPS (secure connection)

Bardwave can serve its pages and streams over HTTPS and `wss://` instead of plain HTTP and `ws://`. It is **off by default**. This page says what it protects, how to switch it on, and what to do when a browser complains.

The setting lives in the **SECURE CONNECTION (HTTPS)** section of the NETWORK column of the console (the panel titled *Network Server*; below it is called the SERVER panel), and in `config.toml` as `[server] tls`, `tls_cert` and `tls_key`. Russian version: [../ru/https.md](../ru/https.md).

<a id="why-https"></a>
## Why HTTPS

The player's access key (`?k=...`) and the game master's key (`?g=...`) travel in the link, and each key is sent again in the query of its WebSocket handshake (`/ws/audio` for players, `/ws/control` for the game master). Over plain HTTP they cross the network in clear text. On the internet anyone on the path (a shared Wi-Fi, a provider, a tunnel host) can read them and then listen to your stream, or press the buttons of your game-master panel.

With HTTPS the same port speaks TLS, so the keys and the audio are encrypted on the way.

There is a second reason. Browsers only offer some features to a *secure context*: **Wake Lock** (keeps the phone awake while listening), installing the page as a **PWA** (home-screen app), and **AudioWorklet**. Over plain HTTP at a LAN address the listener page still plays, because it falls back to an older audio path (ScriptProcessor), but its **Screen stay-awake** row shows `N/A (needs https)` once you start listening (`--` before that). Over HTTPS the page is a secure context and can use all three (home-screen install itself: not verified).

To be honest about it: **on a home network over plain HTTP everything works.** HTTPS is for the internet, and for the extras above.

<a id="when-you-do-not-need-it"></a>
## When you do not need it

- Everybody sits in the same room or on the same home network (the `http://192.168.x.x:8080` link). Nobody outside can see that traffic.
- You reach players through a VPN or a trusted tunnel that already encrypts the connection.
- You only use the console on this computer.

Leave HTTPS off then. It adds a certificate warning for a self-signed certificate and nothing else you need.

<a id="quick-start"></a>
## Quick start

1. In the SERVER panel (scroll down to **SECURE CONNECTION (HTTPS)**) press **On**. The rest of the section opens. Leave **Self-signed (automatic)** selected.
2. Press **Start Server** (or **Stop Server** and then **Start Server** again if it was already running; the section says *Restart the server to apply*).
3. Copy the player link or the GM link from the panel. It now begins with `https://`. The first time, each player's browser shows a certificate warning: see [Self-signed certificate](#self-signed-certificate) for what to press, and compare the fingerprint the section shows.

The setting is saved in `config.toml` and applies at the next Start. Nothing is encrypted until the server is started again.

<a id="self-signed-certificate"></a>
## Self-signed certificate

With **Self-signed (automatic)** Bardwave makes a certificate itself, once, and keeps it in the `tls/` folder of the data directory (`cert.pem`, `key.pem` and `meta.json`). The data directory is the program's own folder when it is writable (portable layout, `portable.flag`, or an existing `config.toml` there); otherwise `~/Library/Application Support/Bardwave` on macOS, `%APPDATA%\Bardwave` on Windows, `$XDG_DATA_HOME/bardwave` or `~/.local/share/bardwave` on Linux (see *Where the data lives* in the README). A named profile has its own `profiles/<name>/tls/`.

What the section shows, from what is stored:

- the **SHA-256 fingerprint** of the certificate, with a **Copy** button;
- its **expiry date** and the days left. It is valid for 397 days. At the next Start, Bardwave replaces it when less than 30 days are left (or its start date is in the future);
- **Issued for**: the names and addresses the certificate covers. These are `localhost`, `127.0.0.1`, `::1`, this computer's host name (and `<host>.local` when the host name has no dot; the host name on Windows is read from `COMPUTERNAME`, not verified), and the LAN addresses of this machine at the time it was made. If the computer later joins another network and none of its current LAN addresses is covered, a new certificate is made at the next Start;
- **Regenerate certificate** (asks first: press **Regenerate** to confirm or **Cancel**). It replaces the certificate with a new one at once, but a running server keeps using the old one until it is restarted. Every player who trusted the old one gets the warning again. The button exists only in **Self-signed (automatic)** mode.

Before the first Start the section says *A certificate is made on the next Start*. Choosing **My certificate** with no files entered still uses the generated certificate until you give both files (the section says so).

### What a player sees

A self-signed certificate is not signed by an authority the browser knows, so the first visit shows a warning. This is expected. The wording differs between browsers and versions; this is the usual path (not verified: these screens have not been checked on real phones or in current browser versions for this project, see the README's *Not verified on real hardware* list):

| Browser | What you see | How to continue |
|---|---|---|
| Chrome (desktop and Android) | "Your connection is not private", `NET::ERR_CERT_AUTHORITY_INVALID` | **Advanced**, then **Proceed to ... (unsafe)** |
| Safari (macOS) | "This Connection Is Not Private" | **Show Details**, then **visit this website** |
| Safari (iOS) | "This Connection Is Not Private" | **Show Details**, then **visit this website**, confirm |
| Firefox (desktop and Android) | "Warning: Potential Security Risk Ahead" | **Advanced...**, then **Accept the Risk and Continue** |

Two things can add to it. A certificate that does not cover the address the player typed (for example your public IP or a domain name, which a generated certificate does not list) shows a name-mismatch message on top of the authority one; the same buttons continue. And the browser asks again after the certificate is regenerated or replaced.

### Comparing the fingerprint

The warning is only worth clicking through if the certificate is yours. On the warning page, open the certificate details (Chrome: the padlock or **Not secure** > certificate; Firefox: **Advanced...** > **View Certificate**; Safari: **Show Details** > **view the certificate**; menu names not verified) and compare its **SHA-256** fingerprint with the one in the SERVER panel (or in the log line `HTTPS enabled ... SHA-256 ...`). The panel prints it as upper-case hex pairs separated by colons. Some phone browsers do not show the fingerprint; then check it from a computer, or send the players the Copy text through a channel you trust.

<a id="using-your-own-certificate"></a>
## Using your own certificate

A certificate from a real authority (for example **Let's Encrypt**) is trusted by every browser, so players get no warning. It needs a domain name that points to your computer or router; the certificate is issued for that name, and players must open the link with that name.

1. Get the certificate and its private key as **PEM** files. Tools such as Caddy, certbot or acme.sh keep them as files; use the full chain file (for example `fullchain.pem`) as the certificate. The key can be RSA, ECDSA or Ed25519 (what rustls with the `ring` provider can sign with). A passphrase-protected key is not supported (not verified).
2. In the section choose **My certificate**. Type the two paths into **Certificate file (PEM)** and **Private key file (PEM)**, or press **Browse...** next to each.
3. Bardwave checks the files in the background and says what is wrong under the field that is at fault (file not found, not a certificate, key does not belong to it). When both are fine, the fingerprint appears.
4. **Stop Server** and **Start Server** (or just **Start Server** if it was stopped). The paths are saved as `tls_cert` and `tls_key`:

```toml
[server]
tls = true
tls_cert = "/etc/ssl/bardwave/fullchain.pem"
tls_key  = "/etc/ssl/bardwave/privkey.pem"
```

Notes:

- Both paths are needed. With only one set, the server **does not start** with HTTPS (it never falls back quietly to a self-signed certificate or to plain HTTP). The section warns about it. Empty or blank paths count as unset. If you switch back to **Self-signed (automatic)**, the two paths are removed from `config.toml`.
- **Renewal:** Bardwave reads the files only when the server starts. After your tool renews them, press **Stop Server** and **Start Server**. The section checks the files every few seconds and says *Restart the server to apply* when their fingerprint differs from the running one. Bardwave does not read the expiry date of your own certificate, so remember the renewal.
- **Behind Caddy or another reverse proxy:** the simpler setup is to let the proxy hold the certificate and talk to Bardwave over plain HTTP on the same machine, with Bardwave's HTTPS off. The proxy has to pass WebSockets (`/ws/audio` and `/ws/control`). In that setup the links the console shows start with `http://`; share the proxy's `https://` address instead. Keep a `gm_token` set: Bardwave decides "this PC" from the socket address, and requests that carry proxy headers or a non-loopback `Host` are treated as remote and need the key; a plain TCP tunnel that adds no such marks looks like your own browser (see *Security and tokens* in the README). The alternative is to give the proxy's certificate files to Bardwave as above.

<a id="what-changes"></a>
## What changes

With HTTPS on and the server (re)started:

- The **LISTEN URL**, **Copy Link**, the **LOCAL** address and the **INTERNET** address from the router mapping (UPnP, when it is on) begin with `https://`.
- The **GM panel link** and its QR code (**Show QR**) are built from the same address, so they begin with `https://` too.
- The listener page and the phone panel open the stream and the control socket as `wss://` on the same page address and port. There is no second port and no redirect.
- **Plain `http://` on the same port is not answered.** A player who types `http://` gets no page. Links shared earlier with `http://` stop working; share the new ones.
- With `--server-only` (headless), `server.tls` applies the same way, and the log shows the certificate fingerprint. If the certificate cannot be loaded, the log says `Failed to start listener server` and no server is started.

Nothing else changes: the keys, the library and the audio are the same.

<a id="turning-it-off"></a>
## Turning it off

Press **Off** in the section (or set `tls = false`), then **Stop Server** and **Start Server**. The links go back to `http://`. The section then shows one line again.

Your settings are kept. The generated certificate stays in the `tls/` folder, and your paths stay in `config.toml`; **On** again reuses them. Delete the `tls/` folder yourself if you want it gone.

<a id="troubleshooting"></a>
## Troubleshooting

| Symptom | Cause | What to do |
|---|---|---|
| The server panel shows **Error** at Start (the button becomes **Retry Server**) and the server does not start | The certificate or key file cannot be read, is not a certificate, or the key does not belong to it. Or only one of the two paths is set. | Open the section, read the message under the field, fix the path or the file. The status line shows only the start of the message; the log has the full text. Or choose **Self-signed (automatic)**. |
| The section says *Restart the server to apply* | The setting or the certificate changed while the server runs. | **Stop Server** and **Start Server**. |
| The phone shows a certificate warning | The certificate is self-signed. | Compare the fingerprint, then continue as in the table above. Or use your own certificate. |
| The warning mentions the name or address | The certificate does not list the address in the link (a public IP, a domain name, or a LAN address that was not there when it was made). | A computer that moved to another network gets a new certificate at the next Start (or press **Regenerate certificate**). For a domain name, use your own certificate for that name. |
| Warning came back after it was accepted | The certificate was regenerated, replaced, or renewed, or the browser forgot the exception. | Compare the new fingerprint and continue again. |
| `http://` link shows nothing or an error | HTTPS is on: the port only answers TLS. | Use the `https://` link from the panel. |
| Link starts with `http://` although HTTPS is on | The server was not restarted after the switch. | **Stop Server** and **Start Server**. |
| Chrome says the certificate has expired / is not yet valid | The player's clock is wrong, or the certificate is old. | Fix the clock. The generated certificate starts a day early to allow for small errors. An expired generated certificate is replaced at the next Start (or press **Regenerate certificate**). |
| Players on the internet cannot connect | HTTPS does not open ports. | See *Players outside your network* in the README (router mapping, firewall). |

<a id="limitations-and-security-notes"></a>
## Limitations and security notes

- HTTPS protects the link **on the way**. It does not protect a link **that reached the wrong person**. The player key and the GM key are still in the URL; anyone who gets the link (a chat, a screenshot, a shared screen, a QR code on camera) gets the access. Treat both as secrets, and never give the GM link or its QR code to players. The ⟳ button next to *Access key* replaces the player key, and **New key** next to the GM panel link replaces the GM key.
- A self-signed certificate only proves something if you compare the fingerprint. Without that check, a player who clicks through the warning could be talking to someone else.
- The generated private key is stored in `tls/key.pem`; on macOS and Linux the file is `0600` and the folder `0700`. On Windows Bardwave sets no extra restriction on it, so it relies on the access rights of the data directory. For your own key, restrict the file yourself (on Linux and macOS, for example `chmod 600`).
- `config.toml` stores the keys in plain text, and the paths to your certificate and key. Protect that file too.
- Bardwave serves HTTP/1.1 over TLS with the standard safe protocol versions of its TLS library (rustls). There is no client-certificate login and no option to add other names to the generated certificate.
- HTTPS covers the traffic between the browser and Bardwave only. It does not hide that someone is connecting, and it does not change who can reach the port.
- The behaviour with a real Let's Encrypt chain, and the warning screens on real iPhone and Android phones, are covered by automated tests and code review but have not been tried on real devices (see *Not verified on real hardware* in the README).
