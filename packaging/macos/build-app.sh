#!/usr/bin/env bash
# Build Bardwave.app from a release binary (or two, merged into a universal one).
#
# Usage:
#   build-app.sh <binary> [<output-dir>]
#   build-app.sh <aarch64-binary> <x86_64-binary> [<output-dir>]   # universal via lipo
#
# If the second argument is an existing file it is the second binary (lipo),
# otherwise it is the output directory.
#
# Environment:
#   OUT_DIR   output directory when none is given (default: ./dist)
#   ICON_SRC  source PNG for the icon (default: assets/icons/icon-512.png, else assets/icon.png)
#   VERSION   override the version (default: read from Cargo.toml)
#
# Result: <output-dir>/Bardwave.app
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [ "$(uname -s)" != "Darwin" ]; then
    echo "build-app.sh: macOS only (needs sips, iconutil, lipo)." >&2
    exit 1
fi

if [ $# -lt 1 ]; then
    echo "Usage: $0 <binary> [<output-dir>]  |  $0 <aarch64-bin> <x86_64-bin> [<output-dir>]" >&2
    exit 2
fi

BIN_A="$1"
BIN_B=""
OUT="${OUT_DIR:-$PWD/dist}"
if [ $# -ge 2 ]; then
    if [ -f "$2" ]; then
        BIN_B="$2"
        if [ $# -ge 3 ]; then OUT="$3"; fi
    else
        OUT="$2"
    fi
fi

[ -f "$BIN_A" ] || { echo "build-app.sh: binary not found: $BIN_A" >&2; exit 1; }

VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -n1)}"
[ -n "$VERSION" ] || { echo "build-app.sh: cannot read version from Cargo.toml" >&2; exit 1; }
# CFBundleVersion must be numeric dotted; drop any pre-release suffix.
BUILD_VERSION="${VERSION%%-*}"

ICON_SRC="${ICON_SRC:-}"
if [ -z "$ICON_SRC" ]; then
    if [ -f "$REPO_ROOT/assets/icons/icon-512.png" ]; then
        ICON_SRC="$REPO_ROOT/assets/icons/icon-512.png"
    else
        ICON_SRC="$REPO_ROOT/assets/icon.png"
    fi
fi
[ -f "$ICON_SRC" ] || { echo "build-app.sh: icon not found: $ICON_SRC" >&2; exit 1; }

mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
APP="$OUT/Bardwave.app"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/bardwave-app.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# --- executable ---------------------------------------------------------
if [ -n "$BIN_B" ]; then
    echo "==> lipo: universal binary from $BIN_A + $BIN_B"
    lipo -create "$BIN_A" "$BIN_B" -output "$APP/Contents/MacOS/Bardwave"
else
    cp "$BIN_A" "$APP/Contents/MacOS/Bardwave"
fi
chmod 755 "$APP/Contents/MacOS/Bardwave"
echo "    architectures: $(lipo -archs "$APP/Contents/MacOS/Bardwave" 2>/dev/null || echo '?')"

# --- icon ---------------------------------------------------------------
echo "==> Bardwave.icns from $ICON_SRC"
ICONSET="$WORK/Bardwave.iconset"
mkdir -p "$ICONSET"
# name:pixel-size. Sizes above the source resolution are upscaled by sips.
for spec in \
    icon_16x16:16 icon_16x16@2x:32 \
    icon_32x32:32 icon_32x32@2x:64 \
    icon_128x128:128 icon_128x128@2x:256 \
    icon_256x256:256 icon_256x256@2x:512 \
    icon_512x512:512 icon_512x512@2x:1024; do
    name="${spec%%:*}"
    px="${spec##*:}"
    sips -z "$px" "$px" "$ICON_SRC" --out "$ICONSET/$name.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Bardwave.icns"

# --- Info.plist ---------------------------------------------------------
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>Bardwave</string>
    <key>CFBundleIconFile</key>
    <string>Bardwave</string>
    <key>CFBundleIdentifier</key>
    <string>com.adamantida.bardwave</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Bardwave</string>
    <key>CFBundleDisplayName</key>
    <string>Bardwave</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${BUILD_VERSION}</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.music</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>Bardwave streams the sound mix to your players' browsers over the local network. / Bardwave транслирует звук игрокам в браузер по локальной сети.</string>
    <key>NSHumanReadableCopyright</key>
    <string>MIT License</string>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Ad-hoc signature so the bundle is internally consistent (arm64 refuses to run
# unsigned code). sign-notarize.sh replaces it with a Developer ID signature.
codesign --force --sign - "$APP" >/dev/null 2>&1 || echo "    (ad-hoc codesign skipped)"

echo "==> $APP (version $VERSION)"
