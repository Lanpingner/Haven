# Honest status

**Usable, with plenty still ahead.** `mc`, `vim` and `htop` render correctly;
sessions, splits, tabs, zoom, attach/detach, shell integration, themes and a GPU
window all work.

The [design plan](./plan.md) tracks this milestone by milestone, updated as
work lands so the document cannot drift from the tree. 400 tests, zero clippy
warnings, `cargo test --workspace` green.

## Working

- **Terminal core** — byte-budgeted scrollback, reflow with cursor tracking, the
  alternate screen, scroll regions, insert/delete, erase modes, tab stops,
  `REP`, `DECSCUSR`, `DECSTR`, SCS and DEC Special Graphics, grapheme clusters.
- **Full colour** — 16 ANSI, the 256-colour cube and 24-bit truecolor in both
  the `;` and `:` SGR forms, OSC 4/10/11/12/104/110-112 set *and* query, OSC
  7/52.
- **Mux core** — n-ary pane trees with fractional weights, a layout solver with
  eight proptest properties, windows, zoom-as-override, named layouts,
  geometric focus movement, and a runtime that gives every pane its own pty and
  geometry.
- **Daemon, attach and detach** — framed postcard over a unix socket, per-client
  row-revision sync, style and cluster table shipping, `Smallest`/`Latest`
  resize policies, autostart, and a protocol-version check at the handshake.
- **GPU frontend** — wgpu pipelines, a shelf-packed glyph atlas, font fallback
  with a last-resort system scan, sRGB-correct colour, procedural box drawing, a
  winit window with vsync, and `haven screenshot` so the renderer stays
  reviewable without a display.
- **Shells** — bash, zsh, fish and a generic implementation behind one `Shell`
  trait, with correct job control and signal delivery, correct environment,
  native-capability detection, and `ZDOTDIR` / `--rcfile` shims.
- **Interface** — status bar, command line with completion, the session tree,
  scrollback reading, live theming from the daemon's config.
- **Copy mode** — a vi-keyed cursor over a pane's history, selection, search,
  and `OSC 52` clipboard writes. The mode is the client's; extraction and search
  are the daemon's, because only it holds the history.

## Not written yet

- **Plugins.** The WASM host and the WIT world are specified in §15 of the plan
  and nothing is built.
- **The modal key table**, configurable bindings and a command palette — the
  rest of M4. Copy mode itself has landed.
- **The interactive capability probe.** Host detection is environment-based
  guessing today; the probe with timeouts and per-`TERM` caching is still owed.
- **Live daemon upgrade.** A new binary currently needs the daemon restarted.
- **Italic faces, ligatures and colour emoji** in the GPU window.
- **Sixel and the kitty graphics protocol** — post-1.0, though the cell model
  reserves a per-pane placement list from the start, because retrofitting image
  placements into a scrolling, reflowing grid is the kind of change that touches
  every file.
- **Mouse reporting** is parsed and tracked, but not encoded back from a client.

## Platforms

Target order is Linux (X11 and Wayland) → macOS → Windows (ConPTY). Linux/X11 is
what is actually exercised; CI runs fmt, clippy with `-D warnings` and the full
test suite on Linux and macOS, plus a layering gate asserting that `haven-core`
cannot link `wgpu` or `winit`.

## Testing

The floor is `vttest` and `esctest`. The gate is real programs driven on real
ptys: mc in UTF-8 *and* under `LANG=C`, mc's `Ctrl+O` subshell toggle, vim,
less, bash prompt marks, a fish double-mark guard, and `haven tui` running
inside `haven`.

Two things make that possible. The core is a state machine you feed byte slices,
so it needs no terminal to test. And the client is frontend-agnostic — a
`NullFrontend`, a `ScriptedFrontend` and a `ScriptedTransport` mean the whole
client loop runs in tests with no window, no terminal and no daemon.

The [bug log](./bug-log.md) is where the interesting part is.
