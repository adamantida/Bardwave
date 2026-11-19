#!/usr/bin/env bash
# Pack Bardwave.app into a compressed .dmg with an /Applications shortcut.
#
# Usage: make-dmg.sh <path/to/Bardwave.app> <output.dmg>
#   Optional env: VOLNAME (default "Bardwave")
set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
    echo "make-dmg.sh: macOS only (needs hdiutil)." >&2
    exit 1
fi
if [ $# -ne 2 ]; then
    echo "Usage: $0 <Bardwave.app> <output.dmg>" >&2
    exit 2
fi

APP="${1%/}"
DMG="$2"
VOLNAME="${VOLNAME:-Bardwave}"
[ -d "$APP" ] || { echo "make-dmg.sh: app bundle not found: $APP" >&2; exit 1; }

mkdir -p "$(dirname "$DMG")"
DMG="$(cd "$(dirname "$DMG")" && pwd)/$(basename "$DMG")"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/bardwave-dmg.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

# ditto keeps the code signature, xattrs and symlinks of the bundle intact.
ditto "$APP" "$STAGE/Bardwave.app"
ln -s /Applications "$STAGE/Applications"

rm -f "$DMG"
hdiutil create -quiet -volname "$VOLNAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
echo "==> $DMG"
