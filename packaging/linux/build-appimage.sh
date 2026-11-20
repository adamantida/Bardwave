#!/usr/bin/env bash
# Builds a Bardwave AppImage from an already built release binary.
#
#   packaging/linux/build-appimage.sh [options]
#
#   -b, --binary <path>    release binary      (default: target/release/soundshared, honours CARGO_TARGET_DIR)
#   -v, --version <x.y.z>  version for the file name (default: read from Cargo.toml)
#   -o, --output <dir>     output directory    (default: target/dist)
#   -t, --tool <path>      appimagetool        (default: $APPIMAGETOOL, then `appimagetool` in PATH)
#   -h, --help
#
# Result: <output>/Bardwave-v<version>-linux-x86_64.AppImage
#
# The AppImage does NOT bundle GTK, ALSA or X11/Wayland libraries (same as the
# tar.gz release): the target system needs GTK 3, ALSA and libxkbcommon
# (Debian/Ubuntu: sudo apt install libgtk-3-0 libasound2 libxkbcommon0).
# Build on the oldest supported distro (CI uses ubuntu-22.04) so the binary
# runs on older glibc versions too.
#
# appimagetool is itself an AppImage; on machines without FUSE (CI containers)
# it is run in extract-and-run mode, which this script enables automatically.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"

binary=""
version=""
output="$repo_root/target/dist"
tool="${APPIMAGETOOL:-}"

usage() { sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        -b | --binary) binary="${2:?missing value for $1}"; shift 2 ;;
        -v | --version) version="${2:?missing value for $1}"; shift 2 ;;
        -o | --output) output="${2:?missing value for $1}"; shift 2 ;;
        -t | --tool) tool="${2:?missing value for $1}"; shift 2 ;;
        -h | --help) usage; exit 0 ;;
        *) echo "error: unknown option '$1'" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$binary" ]; then
    binary="${CARGO_TARGET_DIR:-$repo_root/target}/release/soundshared"
fi
if [ -z "$version" ]; then
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo_root/Cargo.toml" | head -n1)"
fi
version="${version#v}"
if [ -z "$tool" ]; then
    tool="$(command -v appimagetool || true)"
fi

[ -f "$binary" ] || { echo "error: binary not found: $binary (run cargo build --release --locked)" >&2; exit 1; }
[ -n "$version" ] || { echo "error: could not determine the version, pass --version" >&2; exit 1; }
[ -n "$tool" ] && [ -x "$tool" ] || { echo "error: appimagetool not found: set APPIMAGETOOL or use --tool" >&2; exit 1; }

arch="$(uname -m)"
if [ "$arch" != "x86_64" ]; then
    echo "warning: host is $arch, but the output file name says x86_64 (only x86_64 is built by CI)" >&2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
appdir="$work/Bardwave.AppDir"

mkdir -p \
    "$appdir/usr/bin" \
    "$appdir/usr/share/applications" \
    "$appdir/usr/share/icons/hicolor/256x256/apps" \
    "$appdir/usr/share/doc/bardwave"

install -m 755 "$binary" "$appdir/usr/bin/bardwave"
install -m 644 "$script_dir/bardwave.desktop" "$appdir/usr/share/applications/bardwave.desktop"
install -m 644 "$repo_root/assets/icon.png" "$appdir/usr/share/icons/hicolor/256x256/apps/bardwave.png"
install -m 644 "$repo_root/LICENSE" "$appdir/usr/share/doc/bardwave/LICENSE"
install -m 644 "$repo_root/packaging/README.txt" "$appdir/usr/share/doc/bardwave/QUICKSTART.txt"

# appimagetool wants the .desktop file and the icon in the AppDir root, plus
# AppRun and (optionally) .DirIcon.
install -m 644 "$script_dir/bardwave.desktop" "$appdir/bardwave.desktop"
install -m 644 "$repo_root/assets/icon.png" "$appdir/bardwave.png"
ln -s bardwave.png "$appdir/.DirIcon"

cat >"$appdir/AppRun" <<'EOF'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
exec "$here/usr/bin/bardwave" "$@"
EOF
chmod 755 "$appdir/AppRun"

mkdir -p "$output"
out_file="$output/Bardwave-v$version-linux-x86_64.AppImage"
rm -f "$out_file"

# No FUSE in CI containers: let appimagetool unpack itself and run from there.
# ARCH is required by appimagetool when it cannot guess it from the binary.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" "$tool" --no-appstream "$appdir" "$out_file"

chmod 755 "$out_file"
echo "Built $out_file"
