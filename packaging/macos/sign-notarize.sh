#!/usr/bin/env bash
# Developer ID signing + notarization. Everything is optional and driven by env:
#
#   CODESIGN_IDENTITY  e.g. "Developer ID Application: Name (TEAMID)"; required to sign
#   APPLE_ID           Apple ID e-mail          } all three required
#   TEAM_ID            Apple Developer team id  } to notarize
#   APP_PASSWORD       app-specific password    }
#   ENTITLEMENTS       entitlements file (default: entitlements.plist next to this script)
#
# Usage:
#   sign-notarize.sh app <Bardwave.app>   # sign; notarize + staple when notary vars are set
#   sign-notarize.sh dmg <Bardwave.dmg>   # sign the dmg; notarize + staple when notary vars are set
#
# Without CODESIGN_IDENTITY the script prints a notice and exits 0 (unsigned build).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENTITLEMENTS="${ENTITLEMENTS:-$SCRIPT_DIR/entitlements.plist}"

if [ $# -ne 2 ] || { [ "$1" != "app" ] && [ "$1" != "dmg" ]; }; then
    echo "Usage: $0 app|dmg <path>" >&2
    exit 2
fi
KIND="$1"
TARGET="${2%/}"
[ -e "$TARGET" ] || { echo "sign-notarize.sh: not found: $TARGET" >&2; exit 1; }

if [ -z "${CODESIGN_IDENTITY:-}" ]; then
    echo "==> CODESIGN_IDENTITY not set: skipping signing/notarization (unsigned build)."
    echo "    Users open it once via right-click > Open (see packaging/macos/README.md)."
    exit 0
fi

echo "==> codesign ($KIND) with: $CODESIGN_IDENTITY"
if [ "$KIND" = "app" ]; then
    codesign --force --timestamp --options runtime \
        --entitlements "$ENTITLEMENTS" \
        --sign "$CODESIGN_IDENTITY" "$TARGET"
    codesign --verify --strict --verbose=2 "$TARGET"
else
    codesign --force --timestamp --sign "$CODESIGN_IDENTITY" "$TARGET"
fi

if [ -z "${APPLE_ID:-}" ] || [ -z "${TEAM_ID:-}" ] || [ -z "${APP_PASSWORD:-}" ]; then
    echo "==> APPLE_ID / TEAM_ID / APP_PASSWORD not all set: signed, but not notarized."
    exit 0
fi

SUBMIT="$TARGET"
if [ "$KIND" = "app" ]; then
    # notarytool takes zip/dmg/pkg, not a bare bundle.
    CLEAN="$(mktemp -d "${TMPDIR:-/tmp}/bardwave-notary.XXXXXX")"
    trap 'rm -rf "$CLEAN"' EXIT
    SUBMIT="$CLEAN/Bardwave.zip"
    ditto -c -k --keepParent "$TARGET" "$SUBMIT"
fi

echo "==> notarytool submit (waiting for Apple)"
xcrun notarytool submit "$SUBMIT" \
    --apple-id "$APPLE_ID" --team-id "$TEAM_ID" --password "$APP_PASSWORD" \
    --wait

echo "==> stapler"
xcrun stapler staple "$TARGET"
xcrun stapler validate "$TARGET"
