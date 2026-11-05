<div class="hero">

<img class="mark" src="./logo.svg" alt="" width="84" height="84">

# Haven Terminal

<p class="tagline">A GPU terminal with the multiplexer already inside it.</p>

Haven is a keyboard-first terminal emulator in Rust. Sessions, window tabs and
recursive pane splits are part of the core — not a tmux process layered on top,
translating escape sequences twice.

<div class="pills">
<span class="pill"><b>Rust</b> · 13 crates · ~30k lines</span>
<span class="pill"><b>wgpu</b> Vulkan / Metal / DX12 / GL</span>
<span class="pill"><b>636</b> tests, zero clippy warnings</span>
<span class="pill"><b>MIT</b> OR Apache-2.0</span>
<span class="pill">v0.0.1 · <b>pre-release</b></span>
</div>

<div class="cta">
<a class="primary" href="./build.md">Build it</a>
<a href="./comparison.md">How it compares</a>
<a href="https://github.com/Lanpingner/Haven">GitHub</a>
</div>

</div>

<div class="shot angled reveal" data-tilt>
<div class="frame">
<img src="./img/panes.png" width="1180" height="693" alt="One Haven window with three panes: cargo tree at the top left, git log below it, and vim with Rust syntax highlighting on the right, under a status bar.">
</div>
</div>

<p class="caption">One window, three panes, one process. Captured with
<code>haven screenshot</code>, which renders through the same GPU pipeline the
window uses — so this is the real renderer, not a mock-up.</p>

## One codebase, one parser, one keymap

The usual stack is a GPU terminal that knows nothing about panes, with tmux
inside it re-parsing and re-emitting every byte. That costs a second VT
translation layer, a second keymap, a nested-escape-sequence tax, and two
programs that disagree about what a cell is. Haven removes the layer by owning
both halves.

<div class="cards reveal">

<div class="card">

<p class="card-title">The daemon owns the PTYs</p>

Shells survive client exit, GPU crashes, SSH drops and logout. Several clients
can watch one session at once, each with its own size policy and its own scroll
position.

</div>
<div class="card">

<p class="card-title">The GPU does the drawing</p>

Instanced cell quads, a shelf-packed glyph atlas, damage-driven redraws,
vsync-paced frames, sRGB-correct colour, and box drawing computed rather than
looked up in a font.

</div>
<div class="card">

<p class="card-title">The core is a pure state machine</p>

`haven-core` is cells, styles, a grid and VT interpretation — no rendering, no
pty, no protocol. You drive it with byte slices, which is why it is testable at
all. CI fails the build if it ever links `wgpu` or `winit`.

</div>
<div class="card">

<p class="card-title">The serializer is the parser, inverted</p>

`haven-vtwrite` turns cells back into escape sequences: damage diffing, SGR
coalescing, a per-row cost model, scored scroll detection, synchronized output
and OKLab colour downsampling. That is what lets the same client draw into
another terminal.

</div>
<div class="card">

<p class="card-title">Themes and bindings are data</p>

The daemon owns the configuration, so one session looks the same through a
window and through SSH, and an edit reaches every attached client without a
reattach. A binding is a line in a file rather than a code change.

</div>
<div class="card">

<p class="card-title">Correctness measured by real programs</p>

`mc`, `vim` and `less` are driven on real ptys in the test suite — including mc
under `LANG=C`, mc's <kbd>Ctrl+O</kbd> subshell toggle, and `haven tui` running
inside `haven`.

</div>

</div>

## It runs three ways from one build

Two frontends over one mux core. Nothing is a degraded fallback: the TUI client
is a full client that happens to render with escape sequences instead of
triangles. See [run modes](./modes.md).

| | What it is |
|---|---|
| `haven gpu` | A desktop window attached to the daemon, which it starts if needed. Close the window; the session keeps running. Open two; they watch the same panes. |
| `haven attach` | The same session drawn into whatever terminal you are already in — over SSH, in a Linux console, inside another emulator. The tmux-replacement mode. |
| `haven tui` | The whole multiplexer in-process, no daemon, no socket, nothing left behind. |

## What it looks like

<div class="gallery reveal">

<div class="shot" data-tilt>
<div class="frame">
<img src="./img/palette.png" width="1100" height="630" loading="lazy" alt="The Haven command palette open over two panes, listing commands, with the status bar below showing the session name, the tab and the pane count.">
</div>
</div>

<div class="shot" data-tilt>
<div class="frame">
<img src="./img/mc.png" width="1100" height="630" loading="lazy" alt="Midnight Commander running in a Haven pane, both of its panels drawn correctly.">
</div>
</div>

</div>

The palette and the session tree are panels the client draws, themed from the
same table as everything else. `mc` is there because it is the hardest common
program to get right — box drawing, the alternate screen, a subshell and the
mouse — and it is in the test suite for that reason, driven on a real pty.

## Status

**Usable, with plenty still ahead.** Sessions, splits, tabs, zoom,
attach/detach, scrollback, copy mode, the mouse, configurable bindings, save
and restore, [container panes](./containers.md), shell integration, themes and a
GPU window all work; `mc`, `vim` and `htop` render correctly. Plugins and live
daemon upgrade are not written yet.

The [status page](./status.md) lists what exists and what does not, and the
[design plan](./plan.md) tracks it milestone by milestone so the document cannot
drift from the tree.
