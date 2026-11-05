# Honest status

**Usable, with plenty still ahead.** `mc`, `vim` and `htop` render correctly;
sessions, splits, tabs, zoom, attach/detach, copy mode, the mouse, configurable
bindings, save and restore, shell integration, container panes, themes and a
GPU window all work.

The [design plan](./plan.md) tracks this milestone by milestone, updated as
work lands so the document cannot drift from the tree. 635 tests, zero clippy
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
- **[History and grep](./finding.md)** — a pane's commands merged from its
  own `OSC 133` marks and the shell's history file, with `whatis`/`man` in a
  panel beside them; and a grep across every pane in the session, each hit
  carrying the window and row it is at.
- **Shells** — bash, zsh, fish and a generic implementation behind one `Shell`
  trait, with correct job control and signal delivery, correct environment,
  native-capability detection, and `ZDOTDIR` / `--rcfile` shims.
- **[Containers as panes](./containers.md)** — the shell chooser also lists
  running containers, your images and a few distros that are not pulled yet;
  a pull happens in the pane and says so, a kept container is reused by later
  panes, and every failure leaves a working shell and a reason.
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
- **Key tables and a command palette** — named tables, a table of your own that
  stays armed, and every command as a list you narrow by typing.
- **Real bold and italic faces**, with an oblique shear for families that have
  no italic cut, and **colour glyphs** — emoji and any `COLR`/`CBDT`/`sbix`
  face — in their own colours rather than tinted with the foreground.
- **An interactive capability probe** — one batch of queries ending in `DA1`,
  so a terminal that implements none of them costs one reply rather than one
  timeout per capability, cached per `TERM`.
- **Copy mode** — a vi-keyed cursor over a pane's history, selection, search,
  and `OSC 52` clipboard writes. The mode is the client's; extraction and search
  are the daemon's, because only it holds the history.

## Not written yet

- **Plugins.** The WASM host and the WIT world are specified in §15 of the plan
  and nothing is built.
- **Live daemon upgrade.** A new binary currently needs the daemon restarted.
- **Ligatures** in the GPU window, which need a shaping engine. Bold, italic
  and colour faces are in.
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
