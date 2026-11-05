#!/bin/sh
# Install the files sitting next to this script.
#
# This is the one inside the tarball, for someone who downloaded the release
# by hand rather than piping the other installer into a shell. It downloads
# nothing and verifies nothing, because there is nothing left to fetch and
# the checksum was the download's problem.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ "${1:-}" = "--prefix" ]; then
    PREFIX=${2:?--prefix needs a directory}
elif [ -n "${HAVEN_PREFIX:-}" ]; then
    PREFIX=$HAVEN_PREFIX
elif [ "$(id -u)" = 0 ]; then
    PREFIX=/usr/local
else
    PREFIX=$HOME/.local
fi

# `install -D` is a GNU extension; this has to work wherever the tarball was
# unpacked, which includes macOS.
put() { # mode, source, destination
    mkdir -p "$(dirname "$3")"
    cp -f "$2" "$3"
    chmod "$1" "$3"
}

put 755 "$here/haven" "$PREFIX/bin/haven"
[ -f "$here/haven.desktop" ] &&
    put 644 "$here/haven.desktop" "$PREFIX/share/applications/haven.desktop"
[ -f "$here/haven.svg" ] &&
    put 644 "$here/haven.svg" "$PREFIX/share/icons/hicolor/scalable/apps/haven.svg"

printf '\033[1mhaven\033[0m installed %s\n' "$PREFIX/bin/haven"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) printf '\033[1mhaven\033[0m %s is not on your PATH\n' "$PREFIX/bin" ;;
esac
