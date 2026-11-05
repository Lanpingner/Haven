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

# The AUR authenticates by SSH key and nothing else, so the first run of this
# script on a new machine fails — and it fails inside `git clone`, as
# "Permission denied (publickey)", which says nothing about what to do. Ask
# the AUR directly first, and answer the question that error does not.
aur_reachable() {
    out=$(ssh -o BatchMode=yes -o ConnectTimeout=10 aur@aur.archlinux.org help 2>&1) && return 0
    case $out in
        *"Host key verification failed"*|*"authenticity of host"*)
            cat >&2 <<'EOF'
publish.sh: this machine has not met aur.archlinux.org before.

Connect once and check the fingerprint against the one the AUR publishes on
its own front page, https://aur.archlinux.org/ — Ed25519 is
SHA256:RFzBCUItH9LZS0cKB5UE6ceAYhBD5C8GeOBip8Z11+4

    ssh aur@aur.archlinux.org help
EOF
            ;;
        *"Permission denied"*)
            cat >&2 <<'EOF'
publish.sh: the AUR refused this machine's SSH key.

It authenticates by key alone — no password, no token — so the key you push
with has to be registered on an AUR account:

  1. Register at https://aur.archlinux.org/register. It is its own account,
     separate from the wiki, the forums and GitHub.
  2. Make a key for it, if you have none worth using:
         ssh-keygen -t ed25519 -f ~/.ssh/aur -C aur
  3. Paste ~/.ssh/aur.pub into My Account -> SSH Public Key, and save.
  4. Point SSH at it, in ~/.ssh/config:
         Host aur.archlinux.org
           User aur
           IdentityFile ~/.ssh/aur
           IdentitiesOnly yes
  5. Check it: `ssh aur@aur.archlinux.org help` should greet you by name.

An old RSA key may be refused on its own: ed25519 is what the AUR expects.
EOF
            ;;
        *) printf 'publish.sh: cannot reach the AUR: %s\n' "$out" >&2 ;;
    esac
    return 1
}

aur_reachable || exit 1

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
