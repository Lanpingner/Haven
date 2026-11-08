<div class="hero">

<div class="hero-copy">

<img class="mark" src="./logo.svg" alt="" width="64" height="64">

# A terminal with the multiplexer inside it

<p class="tagline">Sessions, tabs and recursive splits are part of the core —
not a tmux process layered on top, translating escape sequences twice.</p>

Haven is a GPU-accelerated, keyboard-first terminal emulator in Rust. One
parser, one grid model, one keymap, and a daemon that keeps your shells alive
when the window goes away.

<div class="cta">
<a class="primary" href="./install.md">Install it</a>
<a href="./comparison.md">How it compares</a>
<a href="https://github.com/Lanpingner/Haven">GitHub</a>
</div>

<div class="pills">
<span class="pill"><b>wgpu</b> Vulkan / Metal / DX12 / GL</span>
<span class="pill"><b>MIT</b> OR Apache-2.0</span>
<span class="pill">v0.0.4</span>
</div>

</div>

<div class="scene" data-scene>
<div class="scene-inner">
<div class="glow"></div>
<div class="pane pane-a"><img src="./img/pane-shell.png" width="620" height="440" alt="A shell pane showing cargo tree and git log output."><span class="tag">bash</span></div>
<div class="pane pane-b"><img src="./img/pane-htop.png" width="620" height="440" alt="A pane running htop."><span class="tag">htop</span></div>
<div class="pane pane-c"><img src="./img/pane-vim.png" width="620" height="440" alt="A pane running vim with Rust syntax highlighting."><span class="tag">vim</span></div>
<div class="statusbar"><span class="session">haven-0</span> <span class="tab-active">1:cargo*</span> <span>2:htop</span> <span>3:vim</span> <span>· 3 panes · 2 clients · 3:12:07</span></div>
<div class="chip chip-a"><span class="chip-dot"></span> <span><b>Ctrl+A d</b> — detached, still running</span></div>
<div class="chip chip-b"><span><b>haven attach</b> — same session, over SSH</span></div>
</div>
</div>

</div>

<p class="caption">Three panes, three programs, one process — each plane above
is a real capture from <code>haven screenshot</code>, which renders through the
same GPU pipeline the window uses. Move the pointer, or scroll, and they
assemble into the single window they actually are.</p>

<div class="band reveal">
<div class="stat"><span class="n">686</span><span class="l">tests, zero clippy warnings</span></div>
<div class="stat"><span class="n">13</span><span class="l">crates, ~30k lines of Rust</span></div>
<div class="stat"><span class="n">0%</span><span class="l">idle CPU, no wakeups</span></div>
<div class="stat"><span class="n">3</span><span class="l">ways to run one binary</span></div>
</div>

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
