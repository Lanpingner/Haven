# Haven Terminal

A GPU-accelerated, keyboard-first terminal emulator in Rust with multiplexing —
sessions, window tabs and split panes — built into the core instead of layered
on via an external tmux process.

It runs three ways from one codebase: as a **server** you attach to and detach
from, as a **standalone** terminal with no daemon, and as a **TUI client** that
draws into any existing terminal over plain escape sequences.

**Status: the multiplexer works; the GPU frontend does not exist yet.** You can
run real sessions today through the TUI frontend — splits, window tabs, zoom,
attach/detach, shell integration, and `mc`/`vim`/`htop` all render correctly.
See [docs/PLAN.md](docs/PLAN.md) for the design and
[its status section](docs/PLAN.md#20-implementation-status) for exactly what
exists.

## Try it

```sh
cargo test --workspace     # 251 tests, incl. mc/vim/less driven on real ptys

# Standalone: the whole mux, no daemon. Ctrl+A is the prefix.
cargo run -p haven -- tui

# Server mode, like tmux:
cargo run -p haven -- attach          # starts a daemon if none is running
cargo run -p haven -- ls
cargo run -p haven -- server stop

# Headless: render a program and print its final screen.
cargo run -p haven -- dump --cols 80 --rows 20 -- mc -u
cargo run -p haven -- shell-info
```

### Keys

`Ctrl+A` then:

| Key | Action |
|---|---|
| `%` / `"` | split into columns / rows |
| `h` `j` `k` `l` or arrows | move focus |
| `H` `J` `K` `L` | resize |
| `z` | zoom the active pane |
| `x` | close the pane |
| `c` / `n` / `p` / `1`-`9` | new window / next / previous / select |
| `e` / `E` / `t` | even-horizontal / even-vertical / tiled layout |
| `d` | detach |
| `Ctrl+A` | send a literal `Ctrl+A` |

## Layout

| Crate | Role |
|---|---|
| `haven-core` | Cells, styles, grid, VT interpretation. No rendering, pty or protocol — a pure state machine you drive with byte slices. |
| `haven-pty` | PTY allocation, child launch, job control, window sizing. |
| `haven-shell` | Shell detection and per-shell integration behind a `Shell` trait. |
| `haven-vtwrite` | Cells back to escape sequences: the inverse of the parser. |
| `haven-mux` | Sessions, windows, pane trees, the layout solver, the pane runtime. |
| `haven-proto` | Wire types, framing and transports. |
| `haven-client` | Frontend-agnostic client: frame model, input encoding, event loop. |
| `haven-tui` | The TUI frontend: ANSI output, raw mode, input decoding. |
| `haven-daemon` | The mux server. |
| `haven` | Composition root: the CLI and the standalone mux. |

## License

MIT OR Apache-2.0.
