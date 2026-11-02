# Haven Terminal

A GPU-accelerated, keyboard-first terminal emulator in Rust with multiplexing —
sessions, window tabs and split panes — built into the core instead of layered
on via an external tmux process.

It runs three ways from one codebase: as a **server** you attach to and detach
from, as a **standalone** terminal with no daemon, and as a **TUI client** that
draws into any existing terminal over plain escape sequences.

**Status: usable, with plenty still ahead.** Sessions, splits, window tabs,
zoom, attach/detach, shell integration and a GPU window all work; `mc`, `vim`
and `htop` render correctly. Plugins, copy mode, configuration and the
interactive capability probe are not written yet. See
[docs/PLAN.md](docs/PLAN.md) for the design and
[its status section](docs/PLAN.md#20-implementation-status) for exactly what
exists.

## Try it

```sh
cargo test --workspace     # 295 tests, incl. mc/vim/less driven on real ptys

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

### Keys

`Ctrl+A` then:

| Key | Action |
|---|---|
| `%` / `"` | split into columns / rows |
| `h` `j` `k` `l` or arrows | move focus |
| `H` `J` `K` `L` | resize |
| `z` | zoom the active pane |
| `x` | close the pane |
| `c` / `n` / `p` / `1`-`9` | new tab / next / previous / select |
| `e` / `E` / `t` | even-horizontal / even-vertical / tiled layout |
| `d` | detach |
| `:` | open the command line |
| `s` | the session tree: sessions, windows, panes |
| `Ctrl+A` | send a literal `Ctrl+A` |

### Command line

`Ctrl+A` `:` opens a command line on the status bar. Commands resolve to the
same actions the keys do, so nothing is reachable one way and not the other.

| Command | Does |
| --- | --- |
| `split-h` / `split-v` | split into rows / columns |
| `new-window` (`new-tab`), `next`, `prev` | tabs |
| `select-window <n>` (`select-tab`) | go to tab *n* (numbered from 1) |
| `kill-pane`, `zoom`, `detach` | as the keys do |
| `even-horizontal`, `even-vertical`, `tiled` | layouts |

`Tab` completes as far as the candidates agree. A box above the bar lists what
matches as you type, with the first entry — what `Tab` would take — marked.
`Esc` cancels, `Backspace` on an empty line leaves the mode.

### Configuration and themes

The daemon owns the configuration, so one session looks the same through a
window and through a terminal, and an edit reaches every attached client
without reattaching — the file is re-read when its timestamp moves.

`$XDG_CONFIG_HOME/haven/config.toml` (or `~/.config/haven/config.toml`):

```toml
theme = "nord"        # haven, nord, gruvbox, daylight, slate, ansi, plain
```

Or a theme of your own, as a table, or as
`~/.config/haven/themes/<name>.toml` named the same way:

```toml
[theme]
base = "gruvbox"      # start here and change only what follows
name = "mine"
bar_session = { fg = "#1d2021", bg = "#83a598", bold = true }
selection   = { fg = 0, bg = 12 }     # palette indices work too
glyphs      = { here = "*" }
```

Every colour and drawing character the interface uses comes from the theme;
nothing below it hardcodes one. Colours are `"#rrggbb"`, a palette index
(`12` or `"12"`), or `"default"` to leave it to the terminal. `plain` uses no
colour and no box drawing, for a terminal that cannot manage either.

A broken theme is reported on the daemon's stderr and the interface keeps the
default rather than refusing to paint.

A theme is *data*. Plugins — code that computes what a status line says — are
a separate thing and are not built yet (`docs/PLAN.md` §15); when they are, a
plugin will supply one of these same tables.

### What the levels are

The word *window* means two things in a GPU terminal, which is worth stating
once because tmux never had to:

| | |
| --- | --- |
| **desktop window** | a `haven gpu` process. A *viewer*, not part of a session — several can watch one session, and closing one leaves it running. |
| **session** | lives in the daemon and outlives every viewer. |
| **tab** | owns a tree of panes. tmux calls this a *window*, and so do the protocol and the commands (`new-window`); the interface says *tab*, because the other meaning is taken. |
| **pane** | one shell, one pty. |

So the session tree has three levels, not four: the desktop window is the
thing looking at it. `new-tab` is accepted everywhere `new-window` is.

### Session tree

`Ctrl+A` `s` opens a chooser over the panes, listing every session and, for
the one you are attached to, its tabs and the panes inside them. It opens
on the pane you are in.

| Key | Does |
| --- | --- |
| `j` / `k` or arrows | move (wraps) |
| type anything | filter the list |
| `Backspace` | undo a filter character |
| `Enter` | go there — attaching to another session if that is what is selected |
| `Esc` | close |

Only the attached session can show its tabs and panes; for the others the
daemon reports counts alone, so that is what is listed.

### Status bar

The bottom row shows the session name, its windows with the active one marked
(`*`, plus `Z` when zoomed), the pane count, how many clients are attached
when more than one, and how long the session has been open. The client keeps
that row for itself and never offers it to the daemon, so no pane is ever laid
out underneath it.

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
| `haven-render` | The GPU frontend: wgpu pipelines, glyph atlas, procedural box drawing, the window. |
| `haven-daemon` | The mux server. |
| `haven` | Composition root: the CLI and the standalone mux. |

## License

MIT OR Apache-2.0.
