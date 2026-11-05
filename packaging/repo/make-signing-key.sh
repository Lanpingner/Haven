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
#
# It gets a random passphrase, kept beside it and given to Actions as a
# second secret. That is not a security measure — whoever can read one
# secret can read both — it is GnuPG: an exported secret key is always
# protected, and gpg then refuses the empty passphrase it was protected
# with, so a key with no passphrase cannot be moved to another machine at
# all. Found by trying it.
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
    head -c 32 /dev/urandom | base64 | tr -d '\n' > "$DIR/passphrase"
    chmod 600 "$DIR/passphrase"
    GNUPGHOME="$DIR" gpg --batch --pinentry-mode loopback \
        --passphrase-file "$DIR/passphrase" --quick-generate-key \
        "$NAME <$EMAIL>" ed25519 sign never
    say "made a signing-only key in $DIR"
fi

fpr=$(GNUPGHOME="$DIR" gpg --list-secret-keys --with-colons |
      awk -F: '/^fpr:/ { print $10; exit }')
[ -n "$fpr" ] || die "no key in $DIR"

[ -f "$DIR/passphrase" ] || die "$DIR has a key but no passphrase file beside it"

GNUPGHOME="$DIR" gpg --armor --export "$fpr" > "$DIR/haven-repo.pub"
GNUPGHOME="$DIR" gpg --batch --pinentry-mode loopback \
    --passphrase-file "$DIR/passphrase" --armor --export-secret-keys "$fpr" \
    > "$DIR/private.asc"
chmod 600 "$DIR/private.asc"

# Prove it can be used somewhere else before telling anyone it works: an
# export that cannot be imported and signed with is the whole failure this
# script exists to avoid.
check=$(mktemp -d)
chmod 700 "$check"
GNUPGHOME="$check" gpg --batch --import "$DIR/private.asc" 2>/dev/null
echo check > "$check/f"
if ! GNUPGHOME="$check" gpg --batch --yes --pinentry-mode loopback \
        --passphrase-file "$DIR/passphrase" --local-user "$fpr" \
        --detach-sign --no-armor "$check/f" 2>/dev/null; then
    rm -rf "$check"
    die "the exported key cannot sign on another machine, so CI could not either"
fi
rm -rf "$check"
say "the exported key imports and signs elsewhere" 

say "fingerprint $fpr"
say "public key   $DIR/haven-repo.pub"
say "private key  $DIR/private.asc  (secret; do not commit it)"
say "passphrase   $DIR/passphrase   (secret; CI needs both)"
echo

if [ "$PUSH" = 1 ]; then
    command -v gh >/dev/null 2>&1 || die "gh is not installed, so --push cannot set the secret"
    gh secret set GPG_PRIVATE_KEY < "$DIR/private.asc"
    gh secret set GPG_PASSPHRASE < "$DIR/passphrase"
    say "set GPG_PRIVATE_KEY and GPG_PASSPHRASE on this repository"
else
    cat <<EOF
Give it to Actions, from the repository — both halves, or signing stops at
a passphrase prompt nobody is there to answer:

    gh secret set GPG_PRIVATE_KEY < $DIR/private.asc
    gh secret set GPG_PASSPHRASE  < $DIR/passphrase

or paste the files into Settings -> Secrets and variables -> Actions.
The release workflow exports the public half beside the packages, so nobody
has to be told where to find it.

Publish the fingerprint somewhere people can check it against — the README
is the usual place:

    $fpr
EOF
fi
