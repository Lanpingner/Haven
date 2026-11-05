# Packaging

Four pieces, and they are chained: the workflow builds what the installer
downloads and what the AUR packages checksum. Change an asset name in one and
the other two break.

```
 git tag v0.0.1 ─▶ .github/workflows/release.yml
                     │  builds on ubuntu-22.04 (glibc 2.35 floor)
                     ├─▶ haven-<ver>-x86_64-unknown-linux-gnu.tar.gz
                     ├─▶ haven-install.sh   (a copy of /install.sh)
                     └─▶ SHA256SUMS
                           │
     curl … | sh ──────────┤   install.sh reads SHA256SUMS first: it names
                           │   the tarball, so `latest` needs no API call
                           │
     yay -S … ─────────────┴─▶ aur/haven-terminal-bin   (unpacks the tarball)
                               aur/haven-terminal       (builds from source)
```

## Cutting a release

```sh
# 1. The tag and the workspace version must agree; the workflow refuses
#    otherwise, because a binary reporting a version other than its own name
#    is a bug report nobody can place.
git tag -a v0.0.1 -m 'Haven 0.0.1'
git push origin v0.0.1

# 2. Wait for the workflow, then point the AUR packages at what it published.
packaging/aur/refresh.sh v0.0.1     # pkgver, real checksums, .SRCINFO
packaging/aur/publish.sh            # pushes both to the AUR
```

`refresh.sh` is a separate step on purpose. The checksums in the tree are
`SKIP` until a release exists to hash, and a PKGBUILD carrying an invented
hash is worse than one that admits it has none.

`publish.sh` is not in CI and should not be: the AUR authenticates by SSH key
and nothing else, so automating it means putting a personal key in a secret.
It is one command, run by a person.

## The pacman repository

The AUR is one way to get updates; this is the other, and it needs no AUR
account at all. The release carries a signed one-package pacman repository,
so a machine that has added it treats Haven like any other package:

```sh
sudo packaging/repo/add-repo.sh      # key, then /etc/pacman.conf
sudo pacman -S haven-terminal-bin
sudo pacman -Syu                     # from then on
```

`add-repo.sh --remove` undoes the pacman.conf half and keeps a backup. It
adds the key *before* the repository on purpose: the other order leaves a
machine whose next `-Syu` fails on a signature it was never given the means
to check.

### Setting it up, once

The repository job signs what it publishes, so it needs a key:

```sh
packaging/repo/make-signing-key.sh --push    # makes one, sets GPG_PRIVATE_KEY
```

That key signs and nothing else: not an identity, certifying nobody, with no
passphrase. A key living in CI is a key that can be taken from CI, and the
answer to that is not a passphrase in a second secret — whoever can read one
secret can read both — but a key worth little, easily revoked and replaced.
Publish its fingerprint somewhere people can check it against.

Without that secret the `repo` job fails loudly rather than skipping, which
is deliberate: a release that quietly stopped shipping a repository is one
nobody notices until `pacman -Syu` says there is nothing new.

### How it is built

In an Arch container, because `makepkg`, `repo-add` and `pacman-key` live
nowhere else — and *after* the tarball job, because it packages what that
job published rather than compiling the binary twice. It sets `pkgver` from
the tag and recomputes the checksum from the asset it just downloaded, so
the PKGBUILD in the tree does not have to have been refreshed first.

One wrinkle worth knowing: `repo-add` makes `haven.db` a symlink to
`haven.db.tar.zst`, and a release asset cannot be a symlink. Each link is
uploaded as a copy of what it points at, which is what pacman asks for by
name anyway.

## The files


| | |
|---|---|
| `../install.sh` | what `curl … \| sh` runs. Downloads, verifies, installs under `~/.local` (or `/usr/local` as root). `--build`, `--uninstall`, `--prefix`. |
| `tarball-install.sh` | ships *inside* the tarball as `install.sh`, for someone who downloaded it by hand. Installs what is beside it; fetches nothing. |
| `haven.desktop` | the launcher. `Exec=haven gpu`, because a desktop entry has no terminal to draw into. |
| `haven.svg` | the icon, which is the site's logo. |
| `aur/haven-terminal/` | builds from the release's source tarball. `x86_64` and `aarch64`. |
| `aur/haven-terminal-bin/` | unpacks the release binary. `x86_64` only — that is all the workflow builds. |
| `repo/add-repo.sh` | adds the signed pacman repository to a machine, and `--remove` takes it off again. |
| `repo/make-signing-key.sh` | makes the signing-only key the repository is signed with, and offers to set the secret. |

## Why the runner is pinned to ubuntu-22.04

The binary links glibc and nothing else of consequence; Vulkan, libxkbcommon
and the display server's client library are all opened at runtime, which is
why a terminal-only install needs none of them. But a glibc-linked binary
will not start on a system older than the one it was built against, so the
runner is pinned rather than following whatever `ubuntu-latest` becomes. The
floor this sets is glibc 2.35.

musl would remove that floor and cannot be used here: a statically linked
musl binary cannot `dlopen`, and dlopen is exactly how the window finds
Vulkan.

## Testing it without a release

Everything here was verified before the first tag existed, by staging a
release locally and pointing the pieces at it:

```sh
# Build and lay out a release exactly as the workflow does.
cargo build --release --locked -p haven
V=0.0.1; D=haven-$V-x86_64-unknown-linux-gnu
mkdir -p /tmp/rel/$D && cp target/release/haven /tmp/rel/$D/ && strip /tmp/rel/$D/haven
cp README.md LICENSE-MIT LICENSE-APACHE packaging/haven.desktop packaging/haven.svg /tmp/rel/$D/
cp packaging/tarball-install.sh /tmp/rel/$D/install.sh
(cd /tmp/rel && tar czf $D.tar.gz $D && sha256sum $D.tar.gz > SHA256SUMS)

# The installer, against that staged release.
HAVEN_BASE_URL=file:///tmp/rel sh install.sh --prefix /tmp/prefix

# And the refusal that matters: corrupt the tarball and watch it stop.
printf x >> /tmp/rel/$D.tar.gz
HAVEN_BASE_URL=file:///tmp/rel sh install.sh --prefix /tmp/prefix   # exits 1
```

`HAVEN_BASE_URL` exists for this. It is the one seam that makes the download
path testable, and `file://` is the only reason the installer tolerates
anything but https.
