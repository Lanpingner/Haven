# Haven Terminal

**A GPU terminal with the multiplexer already inside it.**

Haven is a keyboard-first terminal emulator in Rust. Sessions, window tabs and
recursive pane splits are part of the core — not a tmux process layered on top,
translating escape sequences twice.

<div class="pills">
<span class="pill"><b>Rust</b> · 12 crates · ~21k lines</span>
<span class="pill"><b>wgpu</b> Vulkan / Metal / DX12 / GL</span>
<span class="pill"><b>400</b> tests, zero clippy warnings</span>
<span class="pill"><b>MIT</b> OR Apache-2.0</span>
<span class="pill">v0.0.1 · <b>pre-release</b></span>
</div>

```text
┌──────────────────────────────────────────────────────────┐
│ ~/src/haven ❯ cargo test --workspace │ htop — 8 cores    │
│    Compiling haven-core v0.0.1       │ 1 [|||||  22%]    │
│    Compiling haven-mux  v0.0.1       │ 2 [||||   18%]    │
│ test result: ok. 400 passed          │ 3 [||||||| 41%]   │
│──────────────────────────────────────┼───────────────────│
│   1 fn main() -> ExitCode {          │ $ haven ls        │
│   2     let (cmd, args) = parse()?;  │ haven-0: 3 windows│
│   3     match cmd.as_str() {         │ haven-1: 1 window │
│   4         "gpu" => gpu(args),      │ daemon: 44631b5   │
├──────────────────────────────────────────────────────────┤
│ haven-0  1:cargo* 2:htop 3:vim · 4 panes · 2 clients     │
└──────────────────────────────────────────────────────────┘
```

Sketch of the interface, not a screenshot — but the pane rules, the status bar
and the command-named tabs are what Haven draws today.

[Jump to the comparison](./comparison.md) ·
[build it](./build.md) ·
[source on GitHub](https://github.com/Lanpingner/Haven)

## One codebase, one parser, one keymap

The usual stack is a GPU terminal that knows nothing about panes, with tmux
inside it re-parsing and re-emitting every byte. That costs a second VT
translation layer, a second keymap, a nested-escape-sequence tax, and two
programs that disagree about what a cell is. Haven removes the layer by owning
both halves.

**The daemon owns the PTYs.** Shells survive client exit, GPU crashes, SSH drops
and logout. Several clients can watch one session at once, each with its own
size policy and its own scroll position.

**The GPU does the drawing.** Instanced cell quads, a shelf-packed glyph atlas,
damage-driven redraws, vsync-paced frames, sRGB-correct colour, and box drawing
computed rather than looked up in a font.

**The core is a pure state machine.** `haven-core` is cells, styles, a grid and
VT interpretation — no rendering, no pty, no protocol. You drive it with byte
slices, which is why it is testable at all. CI fails the build if it ever links
`wgpu` or `winit`.

**The serializer is the parser, inverted.** `haven-vtwrite` turns cells back
into escape sequences: damage diffing, SGR coalescing, a per-row cost model,
scored scroll detection, synchronized output and OKLab colour downsampling.
That is what lets the same client draw into another terminal.

**Themes are data, not code.** The daemon owns the configuration, so one session
looks the same through a window and through SSH, and an edit repaints every
attached client without a reattach. Every colour and drawing character comes
from the theme; nothing below it hardcodes one.

**Correctness is measured by real programs.** `mc`, `vim` and `less` are driven
on real ptys in the test suite — including mc under `LANG=C`, mc's `Ctrl+O`
subshell toggle, and `haven tui` running inside `haven`.

## It runs three ways from one build

Two frontends over one mux core. Nothing is a degraded fallback: the TUI client
is a full client that happens to render with escape sequences instead of
triangles. See [run modes](./modes.md).

| | What it is |
|---|---|
| `haven gpu` | A desktop window attached to the daemon, which it starts if needed. Close the window; the session keeps running. Open two; they watch the same panes. |
| `haven attach` | The same session drawn into whatever terminal you are already in — over SSH, in a Linux console, inside another emulator. The tmux-replacement mode. |
| `haven tui` | The whole multiplexer in-process, no daemon, no socket, nothing left behind. |

## Status

**Usable, with plenty still ahead.** Sessions, splits, tabs, zoom,
attach/detach, scrollback, copy mode, shell integration, themes and a GPU window
all work; `mc`, `vim` and `htop` render correctly. Plugins, configurable
bindings and the interactive capability probe are not written yet.

The [status page](./status.md) lists what exists and what does not, and the
[design plan](./plan.md) tracks it milestone by milestone so the document cannot
drift from the tree.
