# Bardwave for macOS: packaging

Scripts that turn the Rust release binary into `Bardwave.app` and a `.dmg`.
macOS only (they use `sips`, `iconutil`, `lipo`, `hdiutil`, `codesign`). Requires macOS 11.0+ at run time.

| File | Purpose |
|---|---|
| `build-app.sh` | binary (or two binaries -> universal via `lipo`) -> `Bardwave.app` with `Info.plist` and `Bardwave.icns` |
| `make-dmg.sh` | `.app` -> compressed `.dmg` with an `Applications` symlink |
| `sign-notarize.sh` | Developer ID signing (hardened runtime + entitlements), `notarytool`, `stapler`; skipped silently without credentials |
| `entitlements.plist` | `network.server` and `network.client`; no microphone entitlement (Bardwave only plays audio) |

## Build locally

```sh
# 1. Release binary (current architecture only)
cargo build --release
packaging/macos/build-app.sh target/release/soundshared dist
packaging/macos/make-dmg.sh dist/Bardwave.app dist/Bardwave.dmg
```

Universal binary (Apple Silicon + Intel):

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
packaging/macos/build-app.sh \
  target/aarch64-apple-darwin/release/soundshared \
  target/x86_64-apple-darwin/release/soundshared \
  dist
packaging/macos/make-dmg.sh dist/Bardwave.app dist/Bardwave.dmg
```

The version in `Info.plist` is read from `Cargo.toml` (override with `VERSION=...`).
The icon is built from `assets/icons/icon-512.png` (falls back to `assets/icon.png`; override with `ICON_SRC=...`);
the iconset covers 16 to 1024 px, larger sizes are upscaled by `sips`.
The bundle gets an ad-hoc signature so that it runs on Apple Silicon even without a Developer ID.

`Info.plist` includes `NSLocalNetworkUsageDescription`: on first start macOS asks whether Bardwave may use the
local network. Players cannot connect over the LAN if you answer "Don't Allow" (change it later in
System Settings > Privacy & Security > Local Network).

## Without signing (default)

Without an Apple Developer account the build is unsigned for Gatekeeper, which is fine for a hobby tool:

1. Open the `.dmg`, drag `Bardwave` to `Applications`.
2. The first time, do not double-click. Right-click (Control-click) `Bardwave.app` > **Open** > **Open**.
3. If macOS still refuses (Sequoia and newer): try to open it once, then System Settings > Privacy & Security >
   **Open Anyway**.

For a copy downloaded in a browser you can also clear the quarantine flag:
`xattr -dr com.apple.quarantine /Applications/Bardwave.app`.

## Signing and notarization

Needs a paid Apple Developer account and a "Developer ID Application" certificate in your keychain.
Everything is controlled by environment variables; without `CODESIGN_IDENTITY` the script does nothing and exits 0.

| Variable | Meaning |
|---|---|
| `CODESIGN_IDENTITY` | `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | Apple ID e-mail used for notarization |
| `TEAM_ID` | 10-character team id |
| `APP_PASSWORD` | app-specific password from appleid.apple.com (not the account password) |
| `ENTITLEMENTS` | optional, defaults to `packaging/macos/entitlements.plist` |

```sh
export CODESIGN_IDENTITY="Developer ID Application: Your Name (ABCDE12345)"
export APPLE_ID="you@example.com" TEAM_ID="ABCDE12345" APP_PASSWORD="xxxx-xxxx-xxxx-xxxx"

packaging/macos/build-app.sh <binaries...> dist
packaging/macos/sign-notarize.sh app dist/Bardwave.app     # sign, notarize, staple the app
packaging/macos/make-dmg.sh dist/Bardwave.app dist/Bardwave.dmg
packaging/macos/sign-notarize.sh dmg dist/Bardwave.dmg     # sign, notarize, staple the dmg
```

If only `CODESIGN_IDENTITY` is set the app is signed but not notarized (Gatekeeper still warns on other machines).

## GitHub Actions

`.github/workflows/release-macos.yml` runs on `release: published` and on manual `workflow_dispatch` (input `tag`, e.g. `v1.4.0`).
It builds both targets, makes the universal `.app` and `.dmg`, computes sha256 and uploads
`Bardwave-<tag>-macos-universal.dmg` and `.dmg.sha256` to the release with `gh release upload`.

Note: a release created by another workflow with the default `GITHUB_TOKEN` (as `release.yml` does) does not
fire the `release` event. In that case run "Release macOS" manually (Actions > Run workflow > tag), or create
the release with a personal access token / GitHub App token.

Signing runs only when the repository secrets exist:
`MACOS_CERTIFICATE` (base64 of the `.p12`: `base64 -i cert.p12 | pbcopy`), `MACOS_CERTIFICATE_PASSWORD`,
`CODESIGN_IDENTITY`, and for notarization also `APPLE_ID`, `TEAM_ID`, `APP_PASSWORD`.
Without them an unsigned build is published.

## Data directory

The `.app` bundle is read-only in practice (and gets replaced on update), so user data must not live next to the
binary. The app stores config and library under `~/Library/Application Support/Bardwave` (implemented in
`src/paths.rs`, stage 0 of PLAN.md). Delete that folder to reset Bardwave.
