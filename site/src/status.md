# Honest status

**Usable, with plenty still ahead.** `mc`, `vim` and `htop` render correctly;
sessions, splits, tabs, zoom, attach/detach, copy mode, the mouse, configurable
bindings, save and restore, shell integration, themes and a GPU window all
work.

The [design plan](./plan.md) tracks this milestone by milestone, updated as
work lands so the document cannot drift from the tree. 495 tests, zero clippy
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
- **Interface** — status bar with zoom, sync, activity and bell marks; command
  line with completion you can walk with the arrows; the session tree; a pane
  numbering overlay; `find-window`; scrollback reading; live theming and live
  key bindings from the daemon's config.
- **The mouse, where a pane wants it** — 1000/1002/1003 in both encodings,
  reported in the pane's own coordinates, so `vim` and `mc` respond to clicks.
  What a pane did not ask for stays with the mux: wheel to read history, click
  to focus, drag a rule to resize.
- **Rearranging** — swap, rotate, break a pane into its own tab, join one back,
  kill a tab, respawn a pane's program, cycle layouts, `last-window` and
  `last-pane`, rename anything.
- **Scripting** — `haven run` takes the same vocabulary a key resolves to, so
  there is no second language for scripts; `send-keys` with escapes, and
  `synchronize-panes` to type into every pane at once.
- **[Save and restore](./sessions.md)** — sessions, tabs, the pane tree and each
  pane's working directory survive a reboot, with an allowlist for the programs
  a restore may start again.
- **Copy mode** — a vi-keyed cursor over a pane's history, selection, search,
  and `OSC 52` clipboard writes. The mode is the client's; extraction and search
  are the daemon's, because only it holds the history.

## Not written yet

- **Plugins.** The WASM host and the WIT world are specified in §15 of the plan
  and nothing is built.
- **The modal key table** and a command palette — the rest of M4. Copy mode and
  configurable bindings have landed.
- **The interactive capability probe.** Host detection is environment-based
  guessing today; the probe with timeouts and per-`TERM` caching is still owed.
- **Live daemon upgrade.** A new binary currently needs the daemon restarted.
- **Italic faces, ligatures and colour emoji** in the GPU window.
- **Sixel and the kitty graphics protocol** — post-1.0, though the cell model
  reserves a per-pane placement list from the start, because retrofitting image
  placements into a scrolling, reflowing grid is the kind of change that touches
  every file.
- **Hooks, `run-shell` and `if-shell`** — a scripting language rather than a
  feature, and `haven run` covers the need from outside.
- **Status-line format strings**, session groups, linked windows, the clock.
  `base-index` has nothing to configure: windows and panes already count from
  one, which is what the option exists to arrange in tmux.

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
