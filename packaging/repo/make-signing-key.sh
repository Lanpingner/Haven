#!/bin/sh
# Make the key the pacman repository is signed with, and show how to give it
# to GitHub Actions.
#
#     packaging/repo/make-signing-key.sh            # make it, print what to do
#     packaging/repo/make-signing-key.sh --push     # and set the secrets with gh
#
# A key used by CI is a key that can be stolen from CI, so this makes one
# that signs and does nothing else: it is not your identity, it certifies
# nobody, and if it leaks you revoke it and publish another. Keep it out of
# your usual keyring by giving it a home of its own, which is the default
# here.
set -eu

NAME=${HAVEN_KEY_NAME:-"Haven Terminal package signing"}
EMAIL=${HAVEN_KEY_EMAIL:-}
DIR=${HAVEN_KEY_DIR:-$HOME/.haven-signing}
PUSH=0
[ "${1:-}" = "--push" ] && PUSH=1

say() { printf '\033[1mhaven\033[0m %s\n' "$*"; }
die() { printf '\033[1mhaven\033[0m %s\n' "$*" >&2; exit 1; }

command -v gpg >/dev/null 2>&1 || die "gnupg is not installed"
[ -n "$EMAIL" ] || EMAIL=$(git config user.email 2>/dev/null) ||
    die "set HAVEN_KEY_EMAIL, or a git user.email to take it from"

if [ -e "$DIR" ]; then
    say "$DIR already exists — using the key in it"
else
    mkdir -p "$DIR"
    chmod 700 "$DIR"
    # No passphrase. The workflow would have to carry it in a second secret
    # anyway, which protects nothing: whoever can read one secret can read
    # both. A key this exposed earns its safety by being worth little —
    # signing only, revocable, and not your identity.
    GNUPGHOME="$DIR" gpg --batch --quick-generate-key \
        "$NAME <$EMAIL>" ed25519 sign never
    say "made a signing-only key in $DIR"
fi

fpr=$(GNUPGHOME="$DIR" gpg --list-secret-keys --with-colons |
      awk -F: '/^fpr:/ { print $10; exit }')
[ -n "$fpr" ] || die "no key in $DIR"

GNUPGHOME="$DIR" gpg --armor --export "$fpr" > "$DIR/haven-repo.pub"
GNUPGHOME="$DIR" gpg --armor --export-secret-keys "$fpr" > "$DIR/private.asc"
chmod 600 "$DIR/private.asc"

say "fingerprint $fpr"
say "public key   $DIR/haven-repo.pub"
say "private key  $DIR/private.asc  (this is the secret; do not commit it)"
echo

if [ "$PUSH" = 1 ]; then
    command -v gh >/dev/null 2>&1 || die "gh is not installed, so --push cannot set the secret"
    gh secret set GPG_PRIVATE_KEY < "$DIR/private.asc"
    say "set the GPG_PRIVATE_KEY secret on this repository"
else
    cat <<EOF
Give it to Actions, from the repository:

    gh secret set GPG_PRIVATE_KEY < $DIR/private.asc

or paste the file into Settings -> Secrets and variables -> Actions.
The release workflow exports the public half beside the packages, so nobody
has to be told where to find it.

Publish the fingerprint somewhere people can check it against — the README
is the usual place:

    $fpr
EOF
fi
