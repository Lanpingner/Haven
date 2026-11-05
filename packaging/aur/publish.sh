#!/bin/sh
# Push the AUR packages.
#
#     packaging/aur/publish.sh                    both
#     packaging/aur/publish.sh haven-terminal-bin just that one
#
# Needs an AUR account with your SSH key on it: the AUR authenticates by key
# and nothing else, so this cannot run in CI without putting a personal key
# there. It is one command, run by a person, on purpose.
#
# The first push of a package creates it. After that the same command
# updates it, which is why there is nothing here about registering a name.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
packages=${*:-'haven-terminal haven-terminal-bin'}

for pkg in $packages; do
    dir=$here/$pkg
    [ -d "$dir" ] || { echo "no such package: $pkg" >&2; exit 1; }
    [ -f "$dir/.SRCINFO" ] || { echo "$pkg has no .SRCINFO — run refresh.sh first" >&2; exit 1; }
    if grep -q "SKIP" "$dir/PKGBUILD"; then
        echo "$pkg still has a SKIP checksum — run refresh.sh <tag> first" >&2
        exit 1
    fi
    ver=$(sed -n 's/^pkgver=//p' "$dir/PKGBUILD")
    rel=$(sed -n 's/^pkgrel=//p' "$dir/PKGBUILD")

    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    echo "== $pkg $ver-$rel"
    git clone -q "ssh://aur@aur.archlinux.org/$pkg.git" "$work/$pkg" 2>/dev/null ||
        { mkdir -p "$work/$pkg" && git -C "$work/$pkg" init -q &&
          git -C "$work/$pkg" remote add origin "ssh://aur@aur.archlinux.org/$pkg.git"; }

    cp "$dir/PKGBUILD" "$dir/.SRCINFO" "$work/$pkg/"
    cd "$work/$pkg"
    git add PKGBUILD .SRCINFO
    if git diff --cached --quiet; then
        echo "   nothing changed"
    else
        git commit -q -m "$pkg $ver-$rel"
        git push -q origin HEAD:master
        echo "   pushed — https://aur.archlinux.org/packages/$pkg"
    fi
    cd "$here"
    rm -rf "$work"
    trap - EXIT
done
