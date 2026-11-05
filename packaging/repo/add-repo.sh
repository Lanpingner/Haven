#!/bin/sh
# Add Haven's pacman repository to this machine.
#
#     sudo packaging/repo/add-repo.sh
#     curl -fsSL https://github.com/Lanpingner/Haven/raw/master/packaging/repo/add-repo.sh | sudo sh
#
# After this, `pacman -Syu` (or `yay -Syu`) keeps Haven up to date like any
# other package: no AUR account, no compiling, no reinstalling by hand.
#
# It needs root, because it writes to /etc/pacman.conf and the pacman
# keyring. Everything it does is reversible and printed as it happens; the
# last section of this file is the undo.
set -eu

REPO=${HAVEN_REPO:-Lanpingner/Haven}
NAME=haven
SERVER="https://github.com/$REPO/releases/latest/download"
CONF=${PACMAN_CONF:-/etc/pacman.conf}

say() { printf '\033[1mhaven\033[0m %s\n' "$*"; }
die() { printf '\033[1mhaven\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" = 0 ] || die "this writes to $CONF and the pacman keyring, so run it with sudo"
command -v pacman-key >/dev/null 2>&1 || die "this is for Arch and its derivatives"

if [ "${1:-}" = "--remove" ]; then
    # Undo, in the reverse order of doing.
    if grep -q "^\[$NAME\]" "$CONF"; then
        cp "$CONF" "$CONF.haven-backup"
        # Delete the section: its header, and every line to the next header
        # or the end of the file.
        awk -v name="$NAME" '
            $0 == "[" name "]" { skip = 1; next }
            skip && /^\[/ { skip = 0 }
            !skip { print }
        ' "$CONF.haven-backup" > "$CONF"
        say "removed [$NAME] from $CONF (kept $CONF.haven-backup)"
    else
        say "[$NAME] was not in $CONF"
    fi
    say "the signing key is left in the keyring; remove it with"
    say "    pacman-key --delete <fingerprint>"
    exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# The key first. A repository added before its key is trusted is a
# repository whose next `pacman -Syu` fails with a signature error, and that
# error is a worse introduction than this download.
say "fetching the signing key"
curl -fsSL "$SERVER/haven-repo.pub" -o "$tmp/haven-repo.pub" ||
    die "no signing key at $SERVER/haven-repo.pub — has a signed release been published yet?"

fpr=$(gpg --show-keys --with-colons "$tmp/haven-repo.pub" | awk -F: '/^fpr:/ { print $10; exit }')
[ -n "$fpr" ] || die "that file is not an OpenPGP key"
say "key $fpr"
say "verify it against the one the project publishes before trusting it"

pacman-key --add "$tmp/haven-repo.pub" >/dev/null
# Locally signing is what moves a key from "known" to "trusted by me".
# Without it every package from this repository is refused.
pacman-key --lsign-key "$fpr" >/dev/null 2>&1
say "trusted in the pacman keyring"

if grep -q "^\[$NAME\]" "$CONF"; then
    say "[$NAME] is already in $CONF, leaving it alone"
else
    cp "$CONF" "$CONF.haven-backup"
    cat >> "$CONF" <<EOF

# Haven Terminal — https://github.com/$REPO
# Added by packaging/repo/add-repo.sh; remove it with --remove.
[$NAME]
SigLevel = Required
Server = $SERVER
EOF
    say "added [$NAME] to $CONF (kept $CONF.haven-backup)"
fi

# Deliberately not `pacman -Sy`: refreshing the databases and then
# installing against them, without upgrading, is the partial-upgrade trap —
# and doing it to someone else's machine on their behalf is worse than
# telling them the command.
say "done. Now:"
say "    sudo pacman -Syu                   # picks up the new repository"
say "    sudo pacman -S haven-terminal-bin"

