# Build it

Rust 1.85 or newer, and a GPU backend wgpu can reach for the window.

If you only want to *use* it, [install it](./install.md) instead: there is an
Arch package and a one-line installer, and neither needs a toolchain.

```sh
git clone https://github.com/Lanpingner/Haven
cd Haven

cargo test --workspace     # 608 tests, incl. mc/vim/less driven on real ptys
```

## Run it

```sh
# A GPU window, attached to the daemon (which it starts if needed).
cargo run --release -p haven -- gpu

# The same session in your current terminal.
cargo run -p haven -- attach
cargo run -p haven -- ls
cargo run -p haven -- server stop

# Standalone: the whole mux, no daemon. Ctrl+A is the prefix.
cargo run -p haven -- tui

# Headless: print or render a program's final screen.
cargo run -p haven -- dump --cols 80 --rows 20 -- mc -u
cargo run -p haven -- screenshot --out mc.ppm -- mc -u
cargo run -p haven -- shell-info
```

Use `--release` for the GPU window. A debug build renders correctly but spends
enough per frame to be worth avoiding.

## If the window misbehaves

The GPU window defaults to the **integrated** GPU, deliberately — a terminal has
no use for a discrete one, and presenting across PRIME on a hybrid laptop can
[block the X server itself](./bug-log.md). `--gpu` and `--gpu-power` override
the choice, and `--backend gl` picks the GL backend if Vulkan is unhappy.

```sh
cargo run -p haven-render --example limits   # what the adapter actually allows
```

## Contributing to these docs

This site is an [mdBook](https://rust-lang.github.io/mdBook/) under `site/`, so
a page is a markdown file and nothing else:

```sh
cargo install mdbook
mdbook serve site --open     # live reload on :3000
mdbook build site            # writes site/book/
```

Add a page by dropping a `.md` file in `site/src/` and listing it in
`site/src/SUMMARY.md` — that file is the sidebar. The last chapter,
[the design plan](./plan.md), is `{{#include}}`d straight from
`docs/PLAN.md`, so it is never a copy that can go stale.

Pushing to `master` deploys the book to GitHub Pages.

## CI

Every push runs fmt, clippy with `-D warnings`, and the full test suite on Linux
and macOS, plus the [layering gate](./architecture.md) asserting that
`haven-core` cannot link `wgpu` or `winit`.
