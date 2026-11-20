#!/usr/bin/env bash
#==============================================================================
# Bardwave build script (macOS / Linux) with an interactive menu.
#
#   ./build.sh                       interactive menu
#   ./build.sh <command> [args]      same actions without the menu (CI, scripts)
#   ./build.sh --help                full usage
#
# Windows has the twin script build.bat with the same menu and flags.
# Works with the system bash 3.2 of macOS and with bash 4+ on Linux.
#==============================================================================
set -euo pipefail

#--- constants ----------------------------------------------------------------
APP_NAME="Bardwave"
CRATE_BIN="soundshared"
MIN_RUST_MAJOR=1
MIN_RUST_MINOR=85
SUPPORTED_TARGETS="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu x86_64-pc-windows-msvc"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/1.9.0/appimagetool-x86_64.AppImage"

# Project root = directory of this script (works from any cwd, with spaces).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIST="$ROOT/dist"
DIST_REL="dist" # commands run with cwd=$ROOT, so they get the short relative path

#--- state --------------------------------------------------------------------
YES=0
DRY_RUN=0
NO_COLOR_OPT=0
WANT_HELP=0
SUFFIX_RAW=""
SUFFIX=""
CMD=""
ARG1=""
ARG_EXTRA=0
MODE="cli" # cli | menu

VER=""
VER_TAG=""
HOST_OS=""
HOST_ARCH=""
HOST_TRIPLE=""
TARGET_DIR=""
SYSROOT=""
RUSTC_VER=""
NATIVE_BUILT=0
ARTIFACTS=""   # newline-separated list of artifacts produced by the current action
TMP_DIRS=""    # newline-separated list of temp dirs removed on exit
TMP_RESULT=""
REPLY_VAL=""
CHECK_ROWS=""
CHECK_FAILED=0
HINTS=""
DOC_FAIL=0

C_RESET=""; C_BOLD=""; C_DIM=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_CYAN=""

#--- cleanup ------------------------------------------------------------------
# shellcheck disable=SC2329 # invoked through trap
cleanup() {
    local d
    if [ -n "$TMP_DIRS" ]; then
        while IFS= read -r d; do
            if [ -n "$d" ]; then rm -rf -- "$d"; fi
        done <<EOF
$TMP_DIRS
EOF
    fi
    return 0
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

make_tmp() { # sets TMP_RESULT to a new temp dir that is removed on exit
    TMP_RESULT="$(mktemp -d "${TMPDIR:-/tmp}/bardwave-build.XXXXXX")" || return 1
    TMP_DIRS="$TMP_DIRS$TMP_RESULT"$'\n'
}

#--- output helpers -----------------------------------------------------------
init_colors() {
    if [ "$NO_COLOR_OPT" -eq 0 ] && [ -z "${NO_COLOR:-}" ] && [ -t 1 ] && [ "${TERM:-dumb}" != "dumb" ]; then
        C_RESET=$'\033[0m'
        C_BOLD=$'\033[1m'
        C_DIM=$'\033[2m'
        C_RED=$'\033[31m'
        C_GREEN=$'\033[32m'
        C_YELLOW=$'\033[33m'
        C_CYAN=$'\033[36m'
    fi
}

info() { printf '%s==>%s %s\n' "$C_CYAN" "$C_RESET" "$*"; }
good() { printf '%s==>%s %s\n' "$C_GREEN" "$C_RESET" "$*"; }
warn() { printf '%swarning:%s %s\n' "$C_YELLOW" "$C_RESET" "$*" >&2; }
err() { printf '%serror:%s %s\n' "$C_RED" "$C_RESET" "$*" >&2; }

have() { command -v "$1" >/dev/null 2>&1; }

quote_cmd() {
    local a out=""
    for a in "$@"; do out="$out $(printf '%q' "$a")"; done
    printf '%s' "${out# }"
}

# Print the command and run it (print only with --dry-run).
run() {
    printf '%s$ %s%s\n' "$C_DIM" "$(quote_cmd "$@")" "$C_RESET"
    if [ "$DRY_RUN" -eq 1 ]; then return 0; fi
    "$@"
}

# must "error message" cmd...   (run, report failure; the caller adds "|| return 1")
must() {
    local msg=$1
    shift
    if run "$@"; then return 0; fi
    err "$msg"
    return 1
}

human_size() { # bytes -> "1.2 MB"
    awk -v b="$1" 'BEGIN { split("B KB MB GB", u, " "); i = 1; while (b >= 1024 && i < 4) { b /= 1024; i++ }
        if (i == 1) printf "%d %s", b, u[i]; else printf "%.1f %s", b, u[i] }'
}

path_size() { # file or directory -> human readable size
    local bytes
    if [ -d "$1" ]; then
        bytes=$(du -sk "$1" 2>/dev/null | awk '{ print $1 * 1024 }')
    else
        bytes=$(wc -c <"$1" | tr -d ' ')
    fi
    human_size "${bytes:-0}"
}

#--- usage --------------------------------------------------------------------
usage() {
    cat <<'EOF'
Bardwave build script (macOS / Linux)

Usage:
  ./build.sh                          interactive menu
  ./build.sh [options] <command> [args]

Commands:
  run                 cargo run --release
  release             build release for this machine, copy the binary to dist/
  debug               cargo build (debug)
  check               fmt --check, clippy -D warnings, test (stops at the first failure)
  package [kind]      build distributable packages into dist/ (default kind: all)
                        macOS:  app | dmg | tar | all
                        Linux:  tar | deb | appimage | all
  target <triple>     release build of another target into dist/; supported:
                        aarch64-apple-darwin  x86_64-apple-darwin
                        x86_64-unknown-linux-gnu  x86_64-pc-windows-msvc
  doctor              check the build environment, print install hints
  clean               cargo clean and remove dist/ (asks for confirmation)
  dist                open the dist/ folder (open / xdg-open)

Options:
  -y, --yes                  no confirmation prompts (also installs missing rustup
                             targets / cargo-deb automatically)
      --no-color             no ANSI colors (also honors NO_COLOR)
      --dry-run              print the commands, do not run them
      --version-suffix <s>   append -<s> to artifact names (not applied to
                             Bardwave.app and to the .deb file name)
  -h, --help                 this help

Artifacts are named like CI: Bardwave-v<version>-<os>-<arch>.<ext>; dist/ also
gets SHA256SUMS. The version comes from Cargo.toml. CARGO_TARGET_DIR is honored.
Exit codes: 0 ok, 1 a step failed, 2 usage error.
EOF
}

usage_error() {
    err "$1"
    printf '\n' >&2
    usage >&2
    exit 2
}

#--- argument parsing ---------------------------------------------------------
parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            -h | --help) WANT_HELP=1 ;;
            -y | --yes) YES=1 ;;
            --no-color) NO_COLOR_OPT=1 ;;
            --dry-run) DRY_RUN=1 ;;
            --version-suffix)
                if [ $# -lt 2 ]; then usage_error "--version-suffix needs a value"; fi
                SUFFIX_RAW=$2
                shift
                ;;
            --version-suffix=*) SUFFIX_RAW=${1#*=} ;;
            -*) usage_error "unknown option '$1'" ;;
            *)
                if [ -z "$CMD" ]; then
                    CMD=$1
                elif [ -z "$ARG1" ]; then
                    ARG1=$1
                else
                    ARG_EXTRA=$((ARG_EXTRA + 1))
                fi
                ;;
        esac
        shift
    done
    if [ -n "$SUFFIX_RAW" ]; then
        case "$SUFFIX_RAW" in
            *[!A-Za-z0-9._+-]*) usage_error "--version-suffix may only contain letters, digits, . _ + -" ;;
        esac
        case "$SUFFIX_RAW" in
            -*) SUFFIX=$SUFFIX_RAW ;;
            *) SUFFIX="-$SUFFIX_RAW" ;;
        esac
    fi
}

#--- environment discovery ----------------------------------------------------
detect_host() {
    case "$(uname -s)" in
        Darwin) HOST_OS="macos" ;;
        Linux) HOST_OS="linux" ;;
        MINGW* | MSYS* | CYGWIN*)
            err "this is a Windows shell: use build.bat instead."
            return 1
            ;;
        *)
            err "unsupported host OS: $(uname -s) (macOS and Linux only)"
            return 1
            ;;
    esac
    if have rustc; then
        HOST_TRIPLE="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')" || HOST_TRIPLE=""
        RUSTC_VER="$(rustc --version 2>/dev/null | cut -d' ' -f1-2)" || RUSTC_VER=""
    fi
    if [ -n "$HOST_TRIPLE" ]; then
        HOST_ARCH="${HOST_TRIPLE%%-*}"
    else
        case "$(uname -m)" in
            arm64 | aarch64) HOST_ARCH="aarch64" ;;
            x86_64 | amd64) HOST_ARCH="x86_64" ;;
            *) HOST_ARCH="$(uname -m)" ;;
        esac
        if [ "$HOST_OS" = "macos" ]; then
            HOST_TRIPLE="$HOST_ARCH-apple-darwin"
        else
            HOST_TRIPLE="$HOST_ARCH-unknown-linux-gnu"
        fi
    fi
    return 0
}

read_version() {
    VER="$(awk '/^\[package\]/ { p = 1; next } /^\[/ { p = 0 }
        p && /^version[ \t]*=/ { gsub(/^[^"]*"|".*$/, ""); print; exit }' "$ROOT/Cargo.toml" 2>/dev/null)" || VER=""
    if [ -z "$VER" ]; then
        err "cannot read [package] version from $ROOT/Cargo.toml"
        return 1
    fi
    VER_TAG="v$VER$SUFFIX"
}

resolve_target_dir() {
    local x md
    if [ -n "$TARGET_DIR" ]; then return 0; fi
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        x="$CARGO_TARGET_DIR"
    else
        x=""
        if have cargo; then
            md="$(cargo metadata --no-deps --format-version 1 --offline 2>/dev/null |
                sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')" || md=""
            x="$md"
        fi
        if [ -z "$x" ]; then x="$ROOT/target"; fi
    fi
    case "$x" in
        /*) ;;
        *) x="$ROOT/$x" ;;
    esac
    TARGET_DIR="$x"
}

sysroot() {
    if [ -z "$SYSROOT" ] && have rustc; then
        SYSROOT="$(rustc --print sysroot 2>/dev/null)" || SYSROOT=""
    fi
    printf '%s' "$SYSROOT"
}

git_info() { # prints "branch @ hash" or nothing outside a git repo
    local b h
    if have git && git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        b="$(git -C "$ROOT" symbolic-ref --short -q HEAD 2>/dev/null)" || b="detached HEAD"
        h="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null)" || h="?"
        printf '%s @ %s' "$b" "$h"
    fi
}

#--- interaction --------------------------------------------------------------
prompt() { # prompt text; sets REPLY_VAL; returns 1 on EOF
    printf '%s' "$1"
    if IFS= read -r REPLY_VAL; then
        return 0
    fi
    REPLY_VAL=""
    printf '\n'
    return 1
}

confirm() { # confirm "question" -> 0 on yes (default: no)
    if [ "$DRY_RUN" -eq 1 ]; then
        info "[dry-run] would ask: $1"
        return 0
    fi
    if [ "$YES" -eq 1 ]; then
        info "$1 [yes, --yes]"
        return 0
    fi
    prompt "$1 [y/N] " || return 1
    case "$REPLY_VAL" in
        y | Y | yes | YES | Yes) return 0 ;;
    esac
    return 1
}

cancelled() {
    if [ "$MODE" = "menu" ]; then
        info "Cancelled."
        return 0
    fi
    err "cancelled (pass --yes to skip confirmations)"
    return 1
}

pause() {
    if [ -t 0 ]; then
        printf '\nPress Enter to continue...'
        IFS= read -r REPLY_VAL || true
    fi
    return 0
}

#--- naming -------------------------------------------------------------------
target_os() {
    case "$1" in
        *apple-darwin) echo macos ;;
        *linux*) echo linux ;;
        *windows*) echo windows ;;
        *) echo unknown ;;
    esac
}

art_name() { # os arch ext -> Bardwave-v<ver>[-suffix]-<os>-<arch><ext>
    printf '%s-%s-%s-%s%s' "$APP_NAME" "$VER_TAG" "$1" "$2" "${3:-}"
}

add_artifact() { ARTIFACTS="$ARTIFACTS$1"$'\n'; }

#--- toolchain ----------------------------------------------------------------
require_cargo() {
    if have cargo; then return 0; fi
    if [ "$DRY_RUN" -eq 1 ]; then
        warn "cargo not found (continuing because of --dry-run)"
        return 0
    fi
    err "cargo not found. Install Rust from https://rustup.rs (macOS: brew install rustup)."
    return 1
}

check_rust_version() { # 0 if rustc >= MIN, 1 otherwise; prints nothing
    local v maj min
    v="$(rustc --version 2>/dev/null | sed -n 's/^rustc \([0-9]*\)\.\([0-9]*\).*/\1 \2/p')" || v=""
    [ -n "$v" ] || return 1
    maj="${v% *}"
    min="${v#* }"
    if [ "$maj" -gt "$MIN_RUST_MAJOR" ]; then return 0; fi
    if [ "$maj" -eq "$MIN_RUST_MAJOR" ] && [ "$min" -ge "$MIN_RUST_MINOR" ]; then return 0; fi
    return 1
}

target_installed() {
    local list
    if [ "$1" = "$HOST_TRIPLE" ]; then return 0; fi
    if have rustup; then
        list="$(rustup target list --installed 2>/dev/null)" || list=""
        case $'\n'"$list"$'\n' in
            *$'\n'"$1"$'\n'*) return 0 ;;
        esac
        return 1
    fi
    [ -d "$(sysroot)/lib/rustlib/$1" ]
}

linker_var() { # CARGO_TARGET_<TRIPLE>_LINKER
    printf 'CARGO_TARGET_%s_LINKER' "$(printf '%s' "$1" | tr 'a-z-' 'A-Z_')"
}

target_buildable() { # can this host plausibly build the target?
    local t=$1 v
    if [ "$t" = "$HOST_TRIPLE" ]; then return 0; fi
    # Xcode's clang builds both macOS architectures from either Mac.
    if [ "$HOST_OS" = "macos" ] && [ "$(target_os "$t")" = "macos" ]; then return 0; fi
    # Everything else needs a cross linker (and SDK / sysroot) supplied by the user.
    v="$(printenv "$(linker_var "$t")" 2>/dev/null)" || v=""
    [ -n "$v" ]
}

add_target() { # rustup target add, with confirmation; 0 ok, 3 declined/unavailable
    local t=$1
    if target_installed "$t"; then return 0; fi
    if ! have rustup && [ "$DRY_RUN" -eq 0 ]; then
        err "rust target $t is not installed and rustup was not found."
        err "Install rustup (https://rustup.rs, macOS: brew install rustup) and run: rustup target add $t"
        return 3
    fi
    if ! confirm "Rust target $t is not installed. Run 'rustup target add $t'?"; then
        warn "skipped: target $t is not installed"
        return 3
    fi
    must "rustup target add $t failed" rustup target add "$t" || return 1
}

# Build the native release binary once per action.
build_native() {
    if [ "$NATIVE_BUILT" -eq 1 ]; then return 0; fi
    require_cargo || return 1
    info "Building release (native: $HOST_TRIPLE)"
    must "cargo build --release failed" cargo build --release || return 1
    resolve_target_dir
    if [ "$DRY_RUN" -eq 0 ] && [ ! -f "$(native_bin)" ]; then
        err "build finished but the binary is missing: $(native_bin)"
        return 1
    fi
    NATIVE_BUILT=1
}

native_bin() { printf '%s/release/%s' "$TARGET_DIR" "$CRATE_BIN"; }

target_bin() { # triple
    local ext=""
    if [ "$(target_os "$1")" = "windows" ]; then ext=".exe"; fi
    printf '%s/%s/release/%s%s' "$TARGET_DIR" "$1" "$CRATE_BIN" "$ext"
}

copy_to_dist() { # src os arch ext
    local src=$1 dst
    dst="$DIST/$(art_name "$2" "$3" "${4:-}")"
    must "cannot create $DIST_REL/" mkdir -p "$DIST_REL" || return 1
    must "copy to $dst failed" cp "$src" "$dst" || return 1
    if [ "$DRY_RUN" -eq 0 ]; then chmod 755 "$dst"; fi
    add_artifact "$dst"
}


#--- summary / checksums ------------------------------------------------------
sha256_of() {
    if have shasum; then
        shasum -a 256 "$1"
    elif have sha256sum; then
        sha256sum "$1"
    else
        return 1
    fi
}

write_sums() { # SHA256SUMS for every regular file in dist/
    local f n=0 tmp="$DIST/.SHA256SUMS.tmp"
    if ! have shasum && ! have sha256sum; then
        warn "neither shasum nor sha256sum found: SHA256SUMS not written"
        return 0
    fi
    : >"$tmp"
    for f in "$DIST"/*; do
        if [ -f "$f" ] && [ "$(basename "$f")" != "SHA256SUMS" ]; then
            (cd "$DIST" && sha256_of "$(basename "$f")") >>"$tmp" || {
                rm -f "$tmp"
                err "checksum failed for $f"
                return 1
            }
            n=$((n + 1))
        fi
    done
    if [ "$n" -gt 0 ]; then
        mv "$tmp" "$DIST/SHA256SUMS"
        info "SHA256SUMS written ($n file(s))"
    else
        rm -f "$tmp"
    fi
}

summarize() {
    local p
    if [ -z "$ARTIFACTS" ]; then return 0; fi
    printf '\n%sSummary%s\n' "$C_BOLD" "$C_RESET"
    if [ "$DRY_RUN" -eq 1 ]; then
        info "[dry-run] SHA256SUMS would be written for all files in $DIST_REL/"
        while IFS= read -r p; do
            if [ -n "$p" ]; then printf '  would build  %s\n' "$p"; fi
        done <<EOF
$ARTIFACTS
EOF
        return 0
    fi
    write_sums || return 1
    while IFS= read -r p; do
        if [ -n "$p" ]; then
            if [ -e "$p" ]; then
                printf '  %-9s  %s\n' "$(path_size "$p")" "$p"
            else
                printf '  %-9s  %s\n' "MISSING" "$p"
            fi
        fi
    done <<EOF
$ARTIFACTS
EOF
    if [ -f "$DIST/SHA256SUMS" ]; then
        printf '  %-9s  %s\n' "$(path_size "$DIST/SHA256SUMS")" "$DIST/SHA256SUMS"
    fi
}

#--- actions: run / build -----------------------------------------------------
do_run() {
    require_cargo || return 1
    info "cargo run --release"
    must "cargo run failed" cargo run --release || return 1
}

do_release() {
    build_native || return 1
    copy_to_dist "$(native_bin)" "$HOST_OS" "$HOST_ARCH" "" || return 1
    summarize
}

do_debug() {
    require_cargo || return 1
    info "Building debug"
    must "cargo build failed" cargo build || return 1
    resolve_target_dir
    good "Debug binary: $TARGET_DIR/debug/$CRATE_BIN"
}

#--- actions: check -----------------------------------------------------------
check_step() { # name cmd...
    local name=$1 st t0 t1
    shift
    if [ "$CHECK_FAILED" -eq 1 ]; then
        CHECK_ROWS="$CHECK_ROWS$name|SKIP|-"$'\n'
        return 0
    fi
    info "$name"
    t0=$(date +%s)
    if run "$@"; then
        st="PASS"
        if [ "$DRY_RUN" -eq 1 ]; then st="DRY"; fi
    else
        st="FAIL"
        CHECK_FAILED=1
    fi
    t1=$(date +%s)
    CHECK_ROWS="$CHECK_ROWS$name|$st|$((t1 - t0))s"$'\n'
}

do_check() {
    local name st tm color
    require_cargo || return 1
    CHECK_ROWS=""
    CHECK_FAILED=0
    check_step "fmt" cargo fmt --check
    check_step "clippy" cargo clippy --all-targets -- -D warnings
    check_step "test" cargo test --all-targets
    printf '\n%sCheck results%s\n' "$C_BOLD" "$C_RESET"
    printf '  %-8s %-6s %s\n' "STEP" "RESULT" "TIME"
    while IFS='|' read -r name st tm; do
        if [ -z "$name" ]; then continue; fi
        case "$st" in
            PASS) color="$C_GREEN" ;;
            FAIL) color="$C_RED" ;;
            *) color="$C_YELLOW" ;;
        esac
        printf '  %-8s %s%-6s%s %s\n' "$name" "$color" "$st" "$C_RESET" "$tm"
    done <<EOF
$CHECK_ROWS
EOF
    if [ "$CHECK_FAILED" -eq 1 ]; then
        err "check failed (see the first FAIL above)"
        return 1
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        good "Dry run: nothing was executed"
    else
        good "All checks passed"
    fi
}

#--- actions: package ---------------------------------------------------------
sign_if_configured() { # kind path
    if [ -n "${CODESIGN_IDENTITY:-}" ]; then
        must "sign-notarize.sh $1 failed" packaging/macos/sign-notarize.sh "$1" "$2" || return 1
    fi
}

pkg_tar() {
    local base stage
    build_native || return 1
    base="$(art_name "$HOST_OS" "$HOST_ARCH" "")"
    info "Packaging $base.tar.gz"
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '%s$ tar -czf %s/%s.tar.gz %s/  (bardwave, README.md, LICENSE, config.toml, README.txt)%s\n' \
            "$C_DIM" "$DIST_REL" "$base" "$base" "$C_RESET"
    else
        make_tmp || return 1
        stage="$TMP_RESULT/$base"
        mkdir -p "$stage" "$DIST"
        cp "$(native_bin)" "$stage/bardwave"
        cp "$ROOT/README.md" "$ROOT/LICENSE" "$ROOT/packaging/config.toml" "$ROOT/packaging/README.txt" "$stage/"
        # COPYFILE_DISABLE: no AppleDouble (._*) files in the archive on macOS.
        COPYFILE_DISABLE=1 tar -czf "$DIST/$base.tar.gz" -C "$TMP_RESULT" "$base" || {
            err "tar failed"
            return 1
        }
    fi
    add_artifact "$DIST/$base.tar.gz"
}

pkg_app() {
    build_native || return 1
    info "Packaging Bardwave.app"
    must "build-app.sh failed" packaging/macos/build-app.sh "$(native_bin)" "$DIST_REL" || return 1
    sign_if_configured app "$DIST_REL/Bardwave.app" || return 1
    add_artifact "$DIST/Bardwave.app"
}

pkg_dmg() {
    local t rc stage dmg
    require_cargo || return 1
    info "Packaging universal .dmg (aarch64 + x86_64)"
    for t in aarch64-apple-darwin x86_64-apple-darwin; do
        rc=0
        add_target "$t" || rc=$?
        if [ "$rc" -ne 0 ]; then return "$rc"; fi
    done
    resolve_target_dir
    for t in aarch64-apple-darwin x86_64-apple-darwin; do
        info "Building release for $t"
        must "cargo build --release --target $t failed" cargo build --release --target "$t" || return 1
        if [ "$DRY_RUN" -eq 0 ] && [ ! -f "$(target_bin "$t")" ]; then
            err "binary missing: $(target_bin "$t")"
            return 1
        fi
    done
    if [ "$DRY_RUN" -eq 1 ]; then
        stage="${TMPDIR:-/tmp}/bardwave-build.XXXXXX"
    else
        make_tmp || return 1
        stage="$TMP_RESULT"
    fi
    dmg="$DIST/$(art_name macos universal ".dmg")"
    must "cannot create $DIST_REL/" mkdir -p "$DIST_REL" || return 1
    # The universal app is built in a temp dir so dist/Bardwave.app (native) is left alone.
    must "build-app.sh (universal) failed" packaging/macos/build-app.sh \
        "$(target_bin aarch64-apple-darwin)" "$(target_bin x86_64-apple-darwin)" "$stage" || return 1
    sign_if_configured app "$stage/Bardwave.app" || return 1
    must "make-dmg.sh failed" packaging/macos/make-dmg.sh "$stage/Bardwave.app" "$dmg" || return 1
    sign_if_configured dmg "$dmg" || return 1
    add_artifact "$dmg"
}

pkg_deb() {
    build_native || return 1
    if ! have cargo-deb && [ "$DRY_RUN" -eq 0 ]; then
        if ! confirm "cargo-deb is not installed. Run 'cargo install cargo-deb --locked'?"; then
            warn "skipped: cargo-deb is not installed"
            return 3
        fi
        must "cargo install cargo-deb failed" cargo install cargo-deb --locked || return 1
    elif ! have cargo-deb; then
        run cargo install cargo-deb --locked
    fi
    info "Packaging .deb"
    must "cannot create $DIST_REL/" mkdir -p "$DIST_REL" || return 1
    if [ "$DRY_RUN" -eq 0 ]; then rm -f "$DIST"/bardwave_*.deb; fi
    must "cargo deb failed" cargo deb --no-build --output "$DIST_REL/" || return 1
    if [ "$DRY_RUN" -eq 1 ]; then
        add_artifact "$DIST/bardwave_${VER}-1_<arch>.deb"
    else
        local f
        for f in "$DIST"/bardwave_*.deb; do
            if [ -f "$f" ]; then add_artifact "$f"; fi
        done
    fi
}

pkg_appimage() {
    local tool="${APPIMAGETOOL:-}"
    build_native || return 1
    if [ -z "$tool" ]; then tool="$(command -v appimagetool || true)"; fi
    if [ -z "$tool" ] || [ ! -x "$tool" ]; then
        if [ "$DRY_RUN" -eq 0 ]; then
            err "appimagetool not found (put it in PATH or set APPIMAGETOOL). To install:"
            err "  curl -fsSL -o ~/.local/bin/appimagetool $APPIMAGETOOL_URL && chmod +x ~/.local/bin/appimagetool"
            return 3
        fi
        warn "appimagetool not found (continuing because of --dry-run)"
    fi
    info "Packaging AppImage"
    must "build-appimage.sh failed" packaging/linux/build-appimage.sh \
        --binary "$(native_bin)" --version "$VER$SUFFIX" --output "$DIST_REL" || return 1
    add_artifact "$DIST/Bardwave-v$VER$SUFFIX-linux-x86_64.AppImage"
}

pkg_one() {
    case "$1" in
        app) pkg_app ;;
        dmg) pkg_dmg ;;
        tar) pkg_tar ;;
        deb) pkg_deb ;;
        appimage) pkg_appimage ;;
    esac
}

do_package() {
    local kind=${1:-all} valid items k rc failed="" skipped=""
    case "$HOST_OS" in
        macos) valid="app dmg tar all"; items="app dmg tar" ;;
        *) valid="tar deb appimage all"; items="tar deb appimage" ;;
    esac
    case " $valid " in
        *" $kind "*) ;;
        *)
            err "unknown package kind '$kind' for $HOST_OS (valid: $valid)"
            return 2
            ;;
    esac
    if [ "$kind" != "all" ]; then items="$kind"; fi
    for k in $items; do
        rc=0
        pkg_one "$k" || rc=$?
        case "$rc" in
            0) ;;
            3) skipped="$skipped $k" ;;
            *) failed="$failed $k" ;;
        esac
    done
    summarize || failed="$failed summary"
    if [ -n "$skipped" ]; then warn "skipped (unavailable):$skipped"; fi
    if [ -n "$failed" ]; then
        err "failed:$failed"
        return 1
    fi
    # A single explicitly requested kind that was skipped is not a success.
    if [ "$kind" != "all" ] && [ -n "$skipped" ]; then return 1; fi
    if [ "$kind" = "all" ] && [ -z "$ARTIFACTS" ]; then
        err "nothing was built"
        return 1
    fi
    return 0
}

# shellcheck disable=SC2329 # invoked through menu_action
package_menu() {
    local choice kind=""
    printf '\n%sPackage%s (%s)\n' "$C_BOLD" "$C_RESET" "$HOST_OS"
    if [ "$HOST_OS" = "macos" ]; then
        printf '  [1] Bardwave.app            (native arch)\n'
        printf '  [2] universal .dmg          (arm64 + x86_64, lipo)\n'
        printf '  [3] .tar.gz                 (binary + docs)\n'
    else
        printf '  [1] .tar.gz                 (binary + docs)\n'
        printf '  [2] .deb                    (cargo-deb)\n'
        printf '  [3] AppImage                (appimagetool)\n'
    fi
    printf '  [a] everything available\n'
    printf '  [0] back\n'
    prompt "Select: " || return 0
    case "$REPLY_VAL" in
        1) if [ "$HOST_OS" = "macos" ]; then kind=app; else kind=tar; fi ;;
        2) if [ "$HOST_OS" = "macos" ]; then kind=dmg; else kind=deb; fi ;;
        3) if [ "$HOST_OS" = "macos" ]; then kind=tar; else kind=appimage; fi ;;
        a | A) kind=all ;;
        0 | "") return 0 ;;
        *)
            warn "unknown choice '$REPLY_VAL'"
            return 0
            ;;
    esac
    choice=$kind
    do_package "$choice"
}

#--- actions: targets ---------------------------------------------------------
target_label() { # triple -> status text (plain)
    if ! target_buildable "$1"; then
        printf 'not buildable on this host'
    elif target_installed "$1"; then
        printf 'installed'
    else
        printf 'not installed'
    fi
}

print_targets() {
    local i=0 t label color mark
    printf '\n%sTargets%s\n' "$C_BOLD" "$C_RESET"
    for t in $SUPPORTED_TARGETS; do
        i=$((i + 1))
        label="$(target_label "$t")"
        case "$label" in
            installed) color="$C_GREEN" ;;
            "not installed") color="$C_YELLOW" ;;
            *) color="$C_RED" ;;
        esac
        mark=""
        if [ "$t" = "$HOST_TRIPLE" ]; then mark=" (this machine)"; fi
        printf '  [%d] %-28s %s%s%s%s\n' "$i" "$t" "$color" "$label" "$C_RESET" "$mark"
    done
    printf '\n  "not installed": offered via rustup target add. "not buildable": cross-compiling to\n'
    printf '  another OS needs a linker and SDK/sysroot (set CARGO_TARGET_<TRIPLE>_LINKER).\n'
}

do_target() { # triple
    local t=$1 rc ext="" os arch
    case " $SUPPORTED_TARGETS " in
        *" $t "*) ;;
        *)
            err "unknown target '$t' (supported: $SUPPORTED_TARGETS)"
            return 2
            ;;
    esac
    if ! target_buildable "$t"; then
        err "target $t is not buildable on this host ($HOST_TRIPLE): cross-compiling to $(target_os "$t")"
        err "needs a linker and SDK/sysroot. If you have them, export $(linker_var "$t")=<linker> and retry."
        return 1
    fi
    require_cargo || return 1
    if [ "$t" = "$HOST_TRIPLE" ]; then
        do_release
        return
    fi
    rc=0
    add_target "$t" || rc=$?
    if [ "$rc" -ne 0 ]; then return 1; fi
    resolve_target_dir
    info "Building release for $t"
    must "cargo build --release --target $t failed" cargo build --release --target "$t" || return 1
    if [ "$DRY_RUN" -eq 0 ] && [ ! -f "$(target_bin "$t")" ]; then
        err "binary missing: $(target_bin "$t")"
        return 1
    fi
    os="$(target_os "$t")"
    arch="${t%%-*}"
    if [ "$os" = "windows" ]; then ext=".exe"; fi
    copy_to_dist "$(target_bin "$t")" "$os" "$arch" "$ext" || return 1
    summarize
}

# shellcheck disable=SC2329 # invoked through menu_action
targets_menu() {
    local i=0 t pick=""
    print_targets
    printf '  [0] back\n'
    prompt "Select target: " || return 0
    case "$REPLY_VAL" in
        "" | 0) return 0 ;;
    esac
    for t in $SUPPORTED_TARGETS; do
        i=$((i + 1))
        if [ "$REPLY_VAL" = "$i" ]; then pick=$t; fi
    done
    if [ -z "$pick" ]; then
        warn "unknown choice '$REPLY_VAL'"
        return 0
    fi
    do_target "$pick"
}

#--- actions: doctor ----------------------------------------------------------
pkg_mgr() {
    if have apt-get; then echo apt
    elif have dnf; then echo dnf
    elif have pacman; then echo pacman
    elif have zypper; then echo zypper
    else echo none
    fi
}

linux_hint() { # key -> install command
    case "$(pkg_mgr):$1" in
        apt:pkgconfig) echo "sudo apt install pkg-config" ;;
        apt:gtk) echo "sudo apt install libgtk-3-dev" ;;
        apt:alsa) echo "sudo apt install libasound2-dev" ;;
        apt:xkb) echo "sudo apt install libxkbcommon-dev" ;;
        apt:ssl) echo "sudo apt install libssl-dev" ;;
        apt:git) echo "sudo apt install git" ;;
        dnf:pkgconfig) echo "sudo dnf install pkgconf-pkg-config" ;;
        dnf:gtk) echo "sudo dnf install gtk3-devel" ;;
        dnf:alsa) echo "sudo dnf install alsa-lib-devel" ;;
        dnf:xkb) echo "sudo dnf install libxkbcommon-devel" ;;
        dnf:ssl) echo "sudo dnf install openssl-devel" ;;
        dnf:git) echo "sudo dnf install git" ;;
        pacman:pkgconfig) echo "sudo pacman -S pkgconf" ;;
        pacman:gtk) echo "sudo pacman -S gtk3" ;;
        pacman:alsa) echo "sudo pacman -S alsa-lib" ;;
        pacman:xkb) echo "sudo pacman -S libxkbcommon" ;;
        pacman:ssl) echo "sudo pacman -S openssl" ;;
        pacman:git) echo "sudo pacman -S git" ;;
        zypper:pkgconfig) echo "sudo zypper install pkg-config" ;;
        zypper:gtk) echo "sudo zypper install gtk3-devel" ;;
        zypper:alsa) echo "sudo zypper install alsa-devel" ;;
        zypper:xkb) echo "sudo zypper install libxkbcommon-devel" ;;
        zypper:ssl) echo "sudo zypper install libopenssl-devel" ;;
        zypper:git) echo "sudo zypper install git" ;;
        *) echo "install the development package that provides '$1' with your package manager" ;;
    esac
}

doc_row() { # name status detail
    local color="$C_GREEN"
    if [ "$2" != "OK" ]; then color="$C_RED"; fi
    printf '  %-26s %s%-8s%s %s\n' "$1" "$color" "$2" "$C_RESET" "$3"
}

# doc_cmd label command required(1|0) "install hint"
doc_cmd() {
    local p
    if have "$2"; then
        p="$(command -v "$2")"
        doc_row "$1" "OK" "$p"
    else
        doc_row "$1" "MISSING" "$([ "$3" = "1" ] && echo required || echo optional)"
        HINTS="$HINTS  - $1: $4"$'\n'
        if [ "$3" = "1" ]; then DOC_FAIL=1; fi
    fi
}

# doc_pc label pc-module hint-key
doc_pc() {
    if ! have pkg-config; then
        doc_row "$1" "MISSING" "cannot check: pkg-config is missing"
        HINTS="$HINTS  - $1: $(linux_hint "$3")"$'\n'
        DOC_FAIL=1
    elif pkg-config --exists "$2" 2>/dev/null; then
        doc_row "$1" "OK" "$2 $(pkg-config --modversion "$2" 2>/dev/null || true)"
    else
        doc_row "$1" "MISSING" "pkg-config module '$2' not found"
        HINTS="$HINTS  - $1: $(linux_hint "$3")"$'\n'
        DOC_FAIL=1
    fi
}

do_doctor() {
    local rustup_hint="curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   (macOS: brew install rustup)"
    local xcode_hint="xcode-select --install"
    HINTS=""
    DOC_FAIL=0
    printf '\n%sDoctor%s\n' "$C_BOLD" "$C_RESET"
    if have rustc; then
        if check_rust_version; then
            doc_row "rustc >= $MIN_RUST_MAJOR.$MIN_RUST_MINOR" "OK" "$RUSTC_VER"
        else
            doc_row "rustc >= $MIN_RUST_MAJOR.$MIN_RUST_MINOR" "MISSING" "$RUSTC_VER is too old"
            HINTS="$HINTS  - rustc: rustup update stable"$'\n'
            DOC_FAIL=1
        fi
    else
        doc_row "rustc >= $MIN_RUST_MAJOR.$MIN_RUST_MINOR" "MISSING" "required"
        HINTS="$HINTS  - rustc: $rustup_hint"$'\n'
        DOC_FAIL=1
    fi
    doc_cmd "cargo" cargo 1 "$rustup_hint"
    if [ "$HOST_OS" = "macos" ]; then
        doc_cmd "git" git 1 "$xcode_hint"
        doc_cmd "sips" sips 1 "$xcode_hint (ships with macOS)"
        doc_cmd "iconutil" iconutil 1 "$xcode_hint (ships with macOS)"
        doc_cmd "hdiutil" hdiutil 1 "ships with macOS; the system looks damaged"
        doc_cmd "lipo" lipo 1 "$xcode_hint"
        doc_cmd "codesign" codesign 1 "$xcode_hint"
        doc_cmd "rustup (optional)" rustup 0 "brew install rustup && rustup-init   (needed for rustup target add / universal .dmg)"
    else
        doc_cmd "git" git 1 "$(linux_hint git)"
        doc_cmd "pkg-config" pkg-config 1 "$(linux_hint pkgconfig)"
        doc_pc "libgtk-3" "gtk+-3.0" gtk
        doc_pc "alsa" alsa alsa
        doc_pc "xkbcommon" xkbcommon xkb
        doc_pc "openssl (libssl)" openssl ssl
        doc_cmd "cargo-deb (optional)" cargo-deb 0 "cargo install cargo-deb --locked"
        doc_cmd "appimagetool (optional)" appimagetool 0 "curl -fsSL -o ~/.local/bin/appimagetool $APPIMAGETOOL_URL && chmod +x ~/.local/bin/appimagetool"
        doc_cmd "rustup (optional)" rustup 0 "$rustup_hint"
    fi
    if [ -n "$HINTS" ]; then
        printf '\n%sHow to fix%s\n%s' "$C_BOLD" "$C_RESET" "$HINTS"
    fi
    printf '\n'
    if [ "$DOC_FAIL" -eq 1 ]; then
        err "required tools are missing (see above)"
        return 1
    fi
    good "Environment is ready"
}

#--- actions: clean / dist ----------------------------------------------------
do_clean() {
    require_cargo || return 1
    resolve_target_dir
    info "This runs 'cargo clean' (removes $TARGET_DIR) and deletes $DIST"
    if ! confirm "Proceed?"; then cancelled; return; fi
    must "cargo clean failed" cargo clean || return 1
    if [ "$DIST" != "$ROOT/dist" ] || [ -z "$DIST" ]; then
        err "refusing to remove unexpected path: $DIST"
        return 1
    fi
    must "cannot remove $DIST" rm -rf -- "$DIST" || return 1
    good "Cleaned"
}

do_dist() {
    local opener
    if [ "$HOST_OS" = "macos" ]; then opener=open; else opener=xdg-open; fi
    if [ ! -d "$DIST" ] && [ "$DRY_RUN" -eq 0 ]; then
        info "$DIST does not exist yet (nothing has been built)"
        return 0
    fi
    if ! have "$opener" && [ "$DRY_RUN" -eq 0 ]; then
        err "'$opener' not found; the folder is: $DIST"
        return 1
    fi
    must "$opener failed" "$opener" "$DIST" || return 1
}

#--- menu ---------------------------------------------------------------------
draw_box() { # title line...
    local title=$1 w l pad dashes
    w=${#title}
    for l in "$@"; do
        if [ "${#l}" -gt "$w" ]; then w=${#l}; fi
    done
    dashes="$(printf '%*s' $((w + 2)) '' | tr ' ' '-')"
    printf '+%s+\n' "$dashes"
    pad="$(printf '%*s' $((w - ${#title})) '')"
    printf '| %s%s%s%s |\n' "$C_BOLD$C_CYAN" "$title" "$C_RESET" "$pad"
    printf '+%s+\n' "$dashes"
    shift
    for l in "$@"; do
        pad="$(printf '%*s' $((w - ${#l})) '')"
        printf '| %s%s |\n' "$l" "$pad"
    done
    printf '+%s+\n' "$dashes"
}

print_header() {
    local g
    g="$(git_info)"
    if [ -z "$g" ]; then g="(not a git repository)"; fi
    printf '\n'
    draw_box "$APP_NAME build   v$VER" \
        "Host : $HOST_OS / $HOST_ARCH ($HOST_TRIPLE)" \
        "Rust : ${RUSTC_VER:-rustc not found}" \
        "Git  : $g" \
        "Dist : $DIST"
}

print_menu() {
    printf '\n'
    printf '  1) Run               cargo run --release\n'
    printf '  2) Build release     native binary -> dist/\n'
    printf '  3) Build debug\n'
    printf '  4) Check             fmt, clippy, test\n'
    printf '  5) Package           app / dmg / tar.gz / deb / AppImage\n'
    printf '  6) Targets           build for another platform\n'
    printf '  7) Doctor            check the environment\n'
    printf '  8) Clean             cargo clean + remove dist/\n'
    printf '  9) Open dist folder\n'
    printf '  q) Quit\n\n'
}

# Run one menu action: reset per-action state, report the result, wait for Enter.
menu_action() {
    local rc=0
    ARTIFACTS=""
    NATIVE_BUILT=0
    "$@" || rc=$?
    if [ "$rc" -eq 0 ]; then
        printf '\n%sDone.%s\n' "$C_GREEN" "$C_RESET"
    else
        printf '\n%sFAILED%s (exit code %d)\n' "$C_RED" "$C_RESET" "$rc"
    fi
    pause
}

main_menu() {
    local choice
    MODE="menu"
    while true; do
        print_header
        print_menu
        if ! prompt "Select [1-9, q]: "; then return 0; fi
        choice=$REPLY_VAL
        case "$choice" in
            "") continue ;;
            1) menu_action do_run ;;
            2) menu_action do_release ;;
            3) menu_action do_debug ;;
            4) menu_action do_check ;;
            5) menu_action package_menu ;;
            6) menu_action targets_menu ;;
            7) menu_action do_doctor ;;
            8) menu_action do_clean ;;
            9) menu_action do_dist ;;
            q | Q | quit | exit) return 0 ;;
            *) warn "unknown choice '$choice'" ;;
        esac
    done
}

#--- entry point --------------------------------------------------------------
main() {
    local rc=0
    parse_args "$@"
    init_colors
    if [ "$WANT_HELP" -eq 1 ]; then
        usage
        exit 0
    fi
    detect_host || exit 1
    cd "$ROOT"
    read_version || exit 1

    if [ -n "$CMD" ] && [ "$ARG_EXTRA" -gt 0 ]; then usage_error "too many arguments"; fi
    case "$CMD" in
        run | release | debug | check | doctor | clean | dist)
            if [ -n "$ARG1" ]; then usage_error "'$CMD' takes no arguments (got '$ARG1')"; fi
            ;;
    esac

    case "$CMD" in
        "") main_menu; exit 0 ;;
        run) do_run || rc=$? ;;
        release) do_release || rc=$? ;;
        debug) do_debug || rc=$? ;;
        check) do_check || rc=$? ;;
        package) do_package "${ARG1:-all}" || rc=$? ;;
        target)
            if [ -z "$ARG1" ]; then
                print_targets >&2
                usage_error "'target' needs a triple"
            fi
            do_target "$ARG1" || rc=$?
            ;;
        doctor) do_doctor || rc=$? ;;
        clean) do_clean || rc=$? ;;
        dist) do_dist || rc=$? ;;
        *) usage_error "unknown command '$CMD'" ;;
    esac

    if [ "$rc" -eq 2 ]; then
        printf '\n' >&2
        usage >&2
    elif [ "$rc" -ne 0 ]; then
        err "'$CMD' failed (exit code $rc)"
    fi
    exit "$rc"
}

main "$@"
