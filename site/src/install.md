# Install it

## Arch

```sh
yay -S haven-terminal-bin   # the binary from the release
yay -S haven-terminal       # or build it yourself
```

Both install `haven`, a desktop entry and the icon. They conflict with each
other on purpose: one `haven` in `PATH`, whichever way you got it.

## Anywhere else, with one line

```sh
curl -fsSL https://github.com/Lanpingner/Haven/releases/latest/download/haven-install.sh | sh
```

It downloads the release for your machine, **checks the checksum before it
believes it**, and installs into `~/.local` — no root, and no `sudo` called
on your behalf. Run it as root and it uses `/usr/local` instead.

```sh
sh install.sh --prefix /opt/haven   # somewhere else
sh install.sh --version v0.0.1      # not the latest
sh install.sh --build               # compile it instead of downloading
sh install.sh --uninstall           # take it out again
```

`--uninstall` removes the binary, the desktop entry and the icon, and leaves
your config and your save file alone. Uninstalling a program is not a reason
to throw away the sessions it would have restored.

Piping a script from the internet into a shell is a thing worth being uneasy
about. [Read it first](https://github.com/Lanpingner/Haven/blob/master/install.sh)
— it is short, it is POSIX `sh`, and the only URL it touches is the one you
gave it.

## From source

```sh
git clone https://github.com/Lanpingner/Haven
cd Haven
cargo build --release --locked -p haven
./target/release/haven gpu
```

Rust 1.85 or newer. [Build it](./build.md) has the details, including what
the test suite needs.

## What it needs to run

The binary links glibc and little else. Everything the window wants — Vulkan,
libxkbcommon, your display server's client library — is loaded at runtime, so:

- `haven attach` and `haven tui`, in a terminal you already have, need
  **nothing** beyond glibc 2.35 or newer.
- `haven gpu`, the window, needs a Vulkan or GL driver, `libxkbcommon`, and
  `libx11`/`wayland` depending on your session. On a desktop that runs
  anything graphical these are already there.

The glibc floor comes from the runner the release is built on, pinned to
Ubuntu 22.04 rather than following whatever `ubuntu-latest` becomes. A
musl build would remove the floor and cannot be used: a static musl binary
cannot `dlopen`, and `dlopen` is how the window finds Vulkan.

## Which platforms have a binary

| | |
|---|---|
| Linux x86_64 | a release tarball, and `haven-terminal-bin` |
| Linux aarch64 | build from source — `haven-terminal` covers it, and the installer falls back to `cargo` by itself |
| macOS | builds and the tests run in CI; no release artifact yet, so `--build` |
| Windows | not yet. The PTY layer is Unix; ConPTY is a port nobody has written. |

## Updating

`yay -Syu` on Arch. Everywhere else, run the installer again — it replaces
what it put there, and nothing else.

## Releases, for the curious

A tag builds the tarball, the checksums and a copy of the installer, and
publishes them; the AUR packages are then pointed at that release by one
script and pushed by another. The whole chain, and why each link is shaped
the way it is, is in
[`packaging/README.md`](https://github.com/Lanpingner/Haven/blob/master/packaging/README.md).
