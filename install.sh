#!/bin/sh
# Install Haven Terminal.
#
#     curl -fsSL https://github.com/haven-terminal/Haven/releases/latest/download/haven-install.sh | sh
#
# What it does, in order: work out which release asset fits this machine,
# download it with its checksum, refuse to install anything whose checksum
# does not match, and put the binary, the desktop entry and the icon where
# this user can reach them. `--uninstall` takes all of it out again.
#
# Three rules this script is written to:
#
#   * It never needs root. The default prefix is ~/.local for an ordinary
#     user, which is where a program nobody packaged belongs; run it as root
#     and it uses /usr/local instead. It does not call sudo on your behalf.
#   * It checks the checksum before it believes the download, and says so.
#     A pipe from the internet into a shell has earned no trust at all.
#   * It says what it did and where, including the PATH line you may need.
#     An installer whose output is a progress bar leaves you guessing.
#
# POSIX sh on purpose: this runs before Haven is installed, on a machine
# whose shell nobody has asked about.
set -eu

REPO=${HAVEN_REPO:-haven-terminal/Haven}
BASE_URL=${HAVEN_BASE_URL:-}
VERSION=${HAVEN_VERSION:-}
PREFIX=${HAVEN_PREFIX:-}
MODE=install

say() { printf '%s\n' "$*"; }
note() { printf '\033[1mhaven\033[0m %s\n' "$*"; }
die() { printf '\033[1mhaven\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
usage: install.sh [options]

  --prefix <dir>   install under this prefix (default: ~/.local, or
                   /usr/local when run as root)
  --version <tag>  install this release instead of the latest, e.g. v0.0.1
  --build          build from source with cargo instead of downloading
  --uninstall      remove what a previous run installed
  --help

environment: HAVEN_PREFIX, HAVEN_VERSION, HAVEN_REPO, HAVEN_BASE_URL
EOF
}

while [ $# -gt 0 ]; do
    case $1 in
        --prefix) PREFIX=${2:?--prefix needs a directory}; shift 2 ;;
        --version) VERSION=${2:?--version needs a tag}; shift 2 ;;
        --build) MODE=build; shift ;;
        --uninstall) MODE=uninstall; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option $1 (try --help)" ;;
    esac
done

if [ -z "$PREFIX" ]; then
    if [ "$(id -u)" = 0 ]; then PREFIX=/usr/local; else PREFIX=$HOME/.local; fi
fi

BIN=$PREFIX/bin/haven
DESKTOP=$PREFIX/share/applications/haven.desktop
ICON=$PREFIX/share/icons/hicolor/scalable/apps/haven.svg

if [ "$MODE" = uninstall ]; then
    removed=0
    for f in "$BIN" "$DESKTOP" "$ICON"; do
        if [ -e "$f" ]; then rm -f "$f" && say "removed $f" && removed=1; fi
    done
    [ "$removed" = 1 ] || note "nothing of Haven's was under $PREFIX"
    # Deliberately left alone: the config, the save file and the socket.
    # Uninstalling a program is not a reason to throw away the sessions it
    # would restore, or the theme someone spent an evening on.
    note "left alone: ~/.config/haven, \$XDG_DATA_HOME/haven/sessions.toml"
    exit 0
fi

# An Arch user has a better route than this script, and should hear about it
# before the downloading starts rather than after.
if [ -f /etc/arch-release ] && [ "$MODE" != build ] && [ -z "${HAVEN_QUIET_AUR:-}" ]; then
    note "Arch: 'yay -S haven-terminal-bin' is this same binary, with updates."
    note "Carrying on with the manual install; set HAVEN_QUIET_AUR=1 to hide this."
fi

have() { command -v "$1" >/dev/null 2>&1; }

fetch() {
    # $1 url, $2 destination
    if have curl; then
        curl -fsSL --proto '=https' --tlsv1.2 "$1" -o "$2"
    elif have wget; then
        wget -qO "$2" "$1"
    else
        die "neither curl nor wget is installed, so nothing can be downloaded"
    fi
}

# file:// is allowed for one reason: it is how this script is tested against
# a release that has been staged locally, before one exists on GitHub.
fetch_any() {
    case $1 in
        file://*) cp "${1#file://}" "$2" ;;
        *) fetch "$1" "$2" ;;
    esac
}

# `install -D` is a GNU extension and this script claims to work on macOS,
# where it is not there. Three portable lines instead of one clever flag.
put() { # mode, source, destination
    mkdir -p "$(dirname "$3")"
    cp -f "$2" "$3"
    chmod "$1" "$3"
}

# A prefix that cannot be written to is the likeliest first failure —
# `--prefix /usr/local` without root — and `install: Permission denied` does
# not suggest what to do about it.
check_prefix() {
    mkdir -p "$PREFIX/bin" 2>/dev/null || true
    [ -w "$PREFIX/bin" ] || die "cannot write to $PREFIX/bin.
       Install somewhere you own:  install.sh --prefix \$HOME/.local
       or run this as root:        sudo sh install.sh"
}

build_from_source() {
    have cargo || die "cargo is not installed; see https://rustup.rs"
    check_prefix
    note "building from source — this takes a few minutes the first time"
    # The scratch directory and the source directory are deliberately two
    # variables. They were one, and the trap — which expands when it *runs* —
    # would then have deleted whatever `src` had become: running this from a
    # checkout would have been `rm -rf .` on that checkout.
    scratch=$(mktemp -d)
    trap 'rm -rf "$scratch"' EXIT
    if [ -f Cargo.toml ] && [ -d crates/haven ]; then
        src=$(pwd)
    else
        have git || die "git is needed to fetch the source"
        note "cloning $REPO"
        git clone --depth 1 "https://github.com/$REPO.git" "$scratch/haven" >/dev/null 2>&1 ||
            die "could not clone https://github.com/$REPO.git"
        src=$scratch/haven
    fi
    ( cd "$src" && cargo build --release --locked -p haven )
    put 755 "$src/target/release/haven" "$BIN"
    [ -f "$src/packaging/haven.desktop" ] && put 644 "$src/packaging/haven.desktop" "$DESKTOP"
    [ -f "$src/packaging/haven.svg" ] && put 644 "$src/packaging/haven.svg" "$ICON"
    finish
}

finish() {
    note "installed $BIN"
    [ -f "$DESKTOP" ] && say "        $DESKTOP"
    [ -f "$ICON" ] && say "        $ICON"
    if have update-desktop-database; then
        update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
    fi
    case ":$PATH:" in
        *":$PREFIX/bin:"*) ;;
        *)
            say ""
            note "$PREFIX/bin is not on your PATH. Add it:"
            say "        fish:  fish_add_path $PREFIX/bin"
            say "        bash:  echo 'export PATH=\"$PREFIX/bin:\$PATH\"' >> ~/.bashrc"
            ;;
    esac
    say ""
    say "  haven gpu      a window, with the daemon started for you"
    say "  haven attach   the same sessions, inside this terminal"
    say "  haven tui      standalone: the multiplexer, no daemon"
    exit 0
}

[ "$MODE" = build ] && build_from_source

check_prefix

os=$(uname -s)
arch=$(uname -m)
case "$os-$arch" in
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    Linux-*)
        note "no prebuilt binary for $arch yet — building from source instead"
        build_from_source
        ;;
    Darwin-*)
        note "macOS has no prebuilt binary yet — building from source instead"
        build_from_source
        ;;
    *) die "$os is not supported" ;;
esac

if [ -z "$BASE_URL" ]; then
    if [ -n "$VERSION" ]; then
        BASE_URL="https://github.com/$REPO/releases/download/$VERSION"
    else
        BASE_URL="https://github.com/$REPO/releases/latest/download"
    fi
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# The checksum file names the tarball, so it is also how the version is
# learned when `latest` was asked for: no API call, no jq, no rate limit.
note "fetching $BASE_URL"
fetch_any "$BASE_URL/SHA256SUMS" "$tmp/SHA256SUMS" ||
    die "no release found at $BASE_URL — has one been published yet? (--build compiles it instead)"

tarball=$(awk -v t="$target" '$2 ~ t && $2 ~ /\.tar\.gz$/ { print $2 }' "$tmp/SHA256SUMS" | head -n1)
[ -n "$tarball" ] || die "that release has no $target build in it"

fetch_any "$BASE_URL/$tarball" "$tmp/$tarball" || die "could not download $tarball"

# Verified before it is believed, and loudly: this is the step the whole
# shape of the script exists for.
if have sha256sum; then
    ( cd "$tmp" && grep " $tarball\$" SHA256SUMS | sha256sum -c - >/dev/null ) ||
        die "checksum mismatch on $tarball — refusing to install it"
elif have shasum; then
    want=$(awk -v f="$tarball" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
    got=$(shasum -a 256 "$tmp/$tarball" | cut -d' ' -f1)
    [ "$want" = "$got" ] || die "checksum mismatch on $tarball — refusing to install it"
else
    die "no sha256sum or shasum, so the download cannot be verified"
fi
note "checksum ok"

tar xzf "$tmp/$tarball" -C "$tmp"
dir=$tmp/${tarball%.tar.gz}
[ -x "$dir/haven" ] || die "the tarball does not contain a haven binary"

put 755 "$dir/haven" "$BIN"
[ -f "$dir/haven.desktop" ] && put 644 "$dir/haven.desktop" "$DESKTOP"
[ -f "$dir/haven.svg" ] && put 644 "$dir/haven.svg" "$ICON"

finish
