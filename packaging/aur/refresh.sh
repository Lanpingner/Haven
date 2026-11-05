#!/bin/sh
# Point both AUR packages at a published release.
#
#     packaging/aur/refresh.sh v0.0.1
#
# Sets pkgver, fetches the real checksums, and regenerates .SRCINFO. The
# checksums in the tree are `SKIP` until this has run against a release that
# exists — a PKGBUILD carrying a made-up hash is worse than one that admits
# it has none.
set -eu

tag=${1:?usage: refresh.sh <tag>, e.g. refresh.sh v0.0.1}
ver=${tag#v}
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

have() { command -v "$1" >/dev/null 2>&1; }
have makepkg || { echo "refresh.sh needs makepkg (run it on Arch)" >&2; exit 1; }
have updpkgsums || { echo "refresh.sh needs updpkgsums: pacman -S pacman-contrib" >&2; exit 1; }

for pkg in haven-terminal haven-terminal-bin; do
    dir=$here/$pkg
    [ -d "$dir" ] || continue
    echo "== $pkg"
    sed -i "s/^pkgver=.*/pkgver=$ver/" "$dir/PKGBUILD"
    # pkgrel goes back to 1 for a new version; a bumped pkgrel means a fix to
    # the packaging of a version that is already out.
    sed -i "s/^pkgrel=.*/pkgrel=1/" "$dir/PKGBUILD"

    # updpkgsums downloads each source and rewrites the checksum arrays in
    # place. Doing it by hand with sed is possible and has exactly one job:
    # being subtly wrong about an array that spans lines.
    ( cd "$dir" && updpkgsums )

    ( cd "$dir" && makepkg --printsrcinfo > .SRCINFO )
    grep -q "SKIP" "$dir/PKGBUILD" && {
        echo "   a checksum is still SKIP — does the release have that asset?" >&2
        exit 1
    }
    echo "   pkgver=$ver, checksums and .SRCINFO updated"
done

echo
echo "Now: packaging/aur/publish.sh   (pushes to the AUR)"
