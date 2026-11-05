# Haven Terminal — Design & Implementation Plan

A GPU-accelerated, keyboard-first terminal emulator in Rust with multiplexing
(sessions / window-tabs / split panes) built into the core rather than bolted on
via an external tmux process.

It runs three ways from one codebase: as a **server** you attach to and detach
from like tmux, as a **standalone** GPU terminal with no daemon, and as a **TUI
client** that renders into any existing terminal over plain escape sequences —
so `ssh box && haven attach` works, and so does a GPU window on your desktop,
against the same live session.

Target platform order: Linux (X11 + Wayland) → macOS → Windows (ConPTY).
Dev box for phase 1: Arch Linux, X11, Vulkan 1.4.

---

## 1. Goals and non-goals

### Goals
- **Native multiplexing.** Detachable sessions, window tabs, recursive pane splits.
  No tmux dependency, no double VT translation layer, no nested escape-sequence
  rewriting. One parser, one grid model, one keymap.
- **GPU rendering.** Everything on the GPU: instanced cell quads, a glyph atlas,
  damage-driven redraws, vsync-paced frames. Idle cost ≈ 0% CPU and no wakeups.
- **Server / attach model.** A long-lived daemon owns the PTYs. Clients attach
  and detach freely, several at once, from a GPU window or from inside another
  terminal. Shells survive client exit, GPU crashes, SSH drops and logout.
- **Standalone too.** No daemon required: `haven` alone spawns an in-process mux
  and behaves like an ordinary terminal emulator.
- **A real TUI frontend.** Not a degraded fallback — a full client that draws the
  same sessions, panes, tabs, copy mode and plugin widgets using ANSI output, for
  use over SSH, in tty consoles, and inside other terminals.
- **Keyboard-first, at full fidelity.** Every action reachable from the keyboard;
  mouse optional everywhere. tmux-style key tables (modal layers), chords, a
  command palette, no mandatory prefix key — and an input path that negotiates
  for *real* key events (kitty keyboard protocol) instead of accepting the 1978
  control-code collapse.
- **Throughput.** `cat` of a 100 MB file should not stall the UI; parsing and
  rendering are decoupled so output bursts drop frames, never input latency.
- **Correctness measured by real programs.** `vttest` and `esctest` are the floor;
  the gate is a tested matrix of full-screen apps — **mc**, vim, neovim, htop,
  less, tig, lazygit, ranger, emacs -nw, nano, and tmux-inside-Haven — each
  snapshot-tested per release, not spot-checked by hand.
- **Full xterm color.** 16 ANSI + 256-color cube + 24-bit truecolor (both `;` and
  `:` SGR forms), OSC 4/10/11/12 palette query and set, bold-as-bright,
  dim, and perceptual downsampling when a client can't show what an app asked for.
- **First-class shell support.** bash, fish and zsh: correct job control and
  signal delivery, correct environment, and opt-in shell integration (OSC 133
  semantic prompts, OSC 7 cwd) with a maintained per-shell script for each.

- **Native plugins.** A first-class, sandboxed extension system: WASM components
  for logic plus a declarative surface for UI, driven by the same action registry
  and event stream the built-in features use. Plugins are not a scripting
  afterthought — the core features are written against the same API.

### Non-goals (v1)
- Plugins rendering arbitrary GPU content (shaders, custom draw calls). They get
  cells, widgets and overlays, not a `wgpu::Device`.
- Native (dylib) plugins. ABI-unstable, unsandboxable, crash the host. WASM only.
- Tabs-in-titlebar window decorations / full client-side decoration theming.
- Sixel and the kitty graphics protocol (reserve hooks, implement post-1.0).
- tmux wire-protocol compatibility (`-CC` control mode clients, `tmux` CLI verbs
  byte-for-byte). We offer a tmux *mental model* and our own control mode.
- Running a Haven client inside a Haven pane as a *nested mux* with its own
  daemon. Detected and refused with a pointer to `haven switch-client`.

---

## 2. Positioning: what this is, in one table

Haven sits in the Alacritty/kitty class — a native, GPU-accelerated terminal, not
an Electron app and not a TUI toolkit — and takes the detachable-server model
from tmux rather than expecting you to run tmux inside it.

| | Alacritty | kitty | tmux | **Haven** |
|---|---|---|---|---|
| GPU rendering | yes (GL) | yes (GL) | n/a | yes (wgpu: Vulkan/Metal/DX12/GL) |
| Tabs / splits | **no, by design** | yes (tabs, windows, layouts) | yes | yes |
| Detachable server | no | no (`kitty @` controls a live GUI only) | **yes** | **yes** — survives GPU crash, logout, SSH drop |
| Runs inside another terminal | no | no | yes | **yes** — full TUI frontend |
| Plugins | no | kittens (Python, in-process, unsandboxed) | shell scripts | **WASM components, capability-sandboxed** |
| Ligatures | no | yes | n/a | yes (opt-in, run-uniform styles) |
| Keyboard protocol | partial CSI u | **invented it** | passthrough | full kitty protocol, both directions, with fidelity tiers |
| Config | TOML, reload | Python-ish conf, reload | `.tmux.conf` | TOML, live reload, `config check` |

What we take from each, deliberately:
- **From Alacritty:** a small, fast, boring core. A grid, a parser, a renderer,
  no feature that costs idle CPU. Its *absence* of multiplexing is the gap we fill.
- **From kitty:** feature breadth without apology — tabs, layouts, the keyboard
  protocol, remote control, graphics. Where kitty's kittens are unsandboxed
  in-process Python, ours are WASM with declared capabilities (§15).
- **From tmux:** the server/attach model and the key-table mental model, without
  the second VT translation layer or the nested-escape-sequence tax.

Two consequences for scope:
- **Remote control is not a separate feature** — `haven -C` control mode (§14.2)
  is our `kitty @`, and the CLI is built on it.
- **Kitty graphics protocol** stays post-1.0, but the cell model reserves a
  per-pane placement list from the start (`Vec<ImagePlacement>` keyed to grid
  coords with z-order), because retrofitting image placements into a scrolling,
  reflowing grid is the kind of change that touches every file. Sixel likewise.

---

## 3. Run modes

One binary, one mux core, two frontends, two transports. The matrix is the whole
product surface:

|  | **GPU frontend** (`haven-render`) | **TUI frontend** (`haven-tui`) |
|---|---|---|
| **Attached** (daemon owns panes) | `haven attach` — the primary desktop mode. Detachable, survives client death. | `haven attach --tui` — inside any terminal, over SSH, in a tty. The tmux-replacement mode. |
| **Standalone** (mux in-process) | `haven` — plain GPU terminal, no daemon, no socket. | `haven --tui --solo` — a pure terminal multiplexer with no GPU and no daemon. |

```
haven                      # standalone GPU terminal
haven attach [-t name]     # attach GPU client to daemon (autostarts it)
haven attach --tui -t work # attach from inside another terminal
haven server start|stop    # explicit daemon lifecycle
haven ls | kill-session | new-session | rename
haven send-keys -t work:1.2 'make test' Enter
haven -C                   # control mode: line protocol on stdio, for scripts
```

Three invariants that make this tractable rather than a combinatorial mess:

1. **The daemon never knows which frontend it is talking to**, beyond a
   `ClientCaps` struct. It ships cells, layout and widget trees; presentation is
   entirely the client's problem.
2. **Standalone is the same code path with a different `Transport`.** `trait
   Transport { fn send(ClientMsg); fn recv() -> ServerMsg }` has a socket impl and
   an in-process channel impl. Standalone is not a special case; it is a transport.
3. **The frontend is a trait, not an `if`.**
   ```rust
   trait Frontend {
       fn caps(&self) -> ClientCaps;
       fn next_input(&mut self) -> Option<InputEvent>;   // keys, mouse, resize, focus
       fn present(&mut self, frame: &FrameModel) -> Result<()>;
       fn size(&self) -> CellSize;
   }
   ```
   `FrameModel` is renderer-agnostic: cell spans, cursor, pane rects, widget
   trees, selection. The GPU frontend rasterizes it; the TUI frontend serializes
   it to escape sequences. Neither can reach into mux state.

---

## 4. Architecture

Two processes, like tmux: a **daemon** that owns PTYs and terminal state, and a
**client** that owns the window and the GPU. Sessions survive client death,
crashes and reboots-of-the-GUI; multiple clients can attach to one session.

```
                      unix socket (framed, length-prefixed postcard)
  ┌──────────────────┐        ┌─────────────────────────────────────────┐
  │  haven (client)  │ <────> │  havend (daemon / mux server)           │
  │                  │        │                                         │
  │  winit window    │        │  Session ─ Window(tab) ─ PaneTree        │
  │  keymap + input  │        │     └─ Pane: Pty + Vte + Grid + Scroll   │
  │  wgpu renderer   │        │  per-pane reader+parser task             │
  │  glyph atlas     │        │  dirty-region diffing → client updates   │
  └──────────────────┘        └─────────────────────────────────────────┘
```

Why split: GPU driver crashes and font-stack bugs cannot take down your shells;
attach/detach is free; a future headless/SSH client reuses the same protocol.
Cost: one serialization hop per frame. Mitigated by sending **cell diffs**, not
full grids, and by an in-process fast path (see §4.1).

### 4.1 Why the daemon is not optional-by-omission
The daemon is the default, autostarted on first attach via socket activation, and
`--solo` is a deliberate opt-out rather than a second architecture. Everything —
standalone GPU, standalone TUI — runs the attached code path over an in-process
transport. One code path means attach/detach bugs can't hide in a mode nobody
tests, and `--solo` stays useful as a protocol-free performance baseline.

### 4.2 Workspace layout

```
crates/
  haven-core      # grid, cell, styles, scrollback, VT state machine, modes
  haven-pty       # pty alloc, fork/exec, winsize, job control, signal plumbing
  haven-shell     # shell detection/launch env, integration scripts, OSC 133/7
  haven-terminfo  # generated terminfo source + tic install, capability tables
  haven-mux       # Session/Window/PaneTree model, layout solver, daemon logic
  haven-proto     # wire types + framing, versioned, serde/postcard
  haven-font      # font discovery, fallback chain, shaping, rasterization
  haven-render    # GPU frontend: wgpu device/pipelines/atlas/damage, decorations
  haven-tui       # TUI frontend: ANSI serializer, host-capability probe, termios
  haven-vtwrite   # cells → escape sequences (the inverse of the parser)
  haven-input     # key encoding (legacy + kitty CSI u), keymap resolution
  haven-config    # config parse/validate/reload, theme model
  haven-plugin    # host runtime (wasmtime), capability broker, lifecycle
  haven-plugin-api# the WIT world + a Rust guest SDK crate plugins depend on
  havend          # daemon binary
  haven           # client binary
  xtask           # codegen, conformance runners, perf harness
```

Dependency rules, enforced in CI by a `cargo-deny`-style check:
- `haven-core` knows nothing about rendering, fonts, or the protocol. It is a pure
  state machine you can drive from a test with byte slices.
- `haven-mux` knows nothing about either frontend. It cannot link `wgpu` or
  `haven-tui`; a `use` of them fails the build.
- `haven-vtwrite` depends only on `haven-core` types, so it is reusable by the
  TUI frontend, `capture-pane`, session recording, and the round-trip tests.

---

## 5. Key technology choices

| Concern | Choice | Rationale / alternatives rejected |
|---|---|---|
| GPU API | **wgpu** | One backend story for Vulkan/Metal/DX12/GL. Raw Vulkan (ash) is faster to tune but 3× the platform work. |
| Windowing | **winit 0.30** (`ApplicationHandler`) | Only mature cross-platform option; gives us `KeyEvent.key_without_modifiers`, IME, raw-window-handle. |
| VT parsing | **`vte` crate** for the DEC/ANSI state machine; our own `Performer` | Battle-tested (Alacritty). Writing the byte-level state machine ourselves buys nothing. |
| Grid/scrollback | hand-rolled in `haven-core` | This *is* the product; no crate fits. |
| Font discovery | **fontdb** | fontconfig without the FFI; cache to disk. |
| Shaping | **swash** (+ our monospace run shaper) | Needs ligature/combining support inside a fixed grid; rustybuzz as fallback if swash shaping gaps appear. |
| Rasterization | **swash** scaler → RGBA/alpha bitmaps, subpixel positioning | Avoids FreeType FFI; gives us hinting + gamma control in pure Rust. |
| PTY | own `haven-pty` on **rustix** | `portable-pty` is fine but drags in extra surface; we need tight control over signals, `TIOCSWINSZ`, and session leaders. ConPTY shim later. |
| Serialization | **postcard** over length-prefixed frames | Compact, no-std friendly, fast. JSON only for the control CLI. |
| Config | **TOML** (+ a small binding DSL in strings) | Familiar. KDL considered; not worth the parse-error UX cost. |
| TUI output | hand-rolled serializer in `haven-vtwrite` | Needs cursor-motion-vs-rewrite cost modelling, SGR run coalescing and synchronized output. `ratatui`/`crossterm` are the wrong shape: we already *have* a cell grid and a damage model, and we need to emit the minimum byte sequence, not own a layout engine. `crossterm` used only for termios/raw-mode on Windows. |
| Color math | hand-rolled OKLab in `haven-core` (~80 lines) | Needed for downsampling and minimum-contrast. `palette` is a large dep for two functions. |
| Terminfo | generated source compiled with `tic` at install; `TERM=xterm-256color` by default | Shipping a `TERM` nobody's ncurses knows is how terminals break `mc` over SSH. |
| Host-terminal probing | `XTGETTCAP` + `DA1`/`DA2` + `CSI ?u` + env heuristics, with timeouts | Never trust `TERM` alone; never hang waiting for a reply that won't come. |
| Plugins | **wasmtime** + the **Component Model** (WIT-defined world) | Sandboxed by construction, stable ABI across host versions, any guest language. Lua/mlua rejected: no isolation, no capability story, one language. Native dylibs rejected outright. |
| Async | **tokio** in the daemon only | Client stays a plain event loop + threads; no async in the render path. |

---

## 6. Core data model (`haven-core`)

```rust
struct Cell {            // 12 bytes, packed
    text: CellText,      // char | grapheme-table index | wide-spacer marker
    style: StyleId,      // u16 index into an interned style table
    flags: CellFlags,    // wide, wide-spacer, dirty, hyperlink-start, ...
}

struct Row { cells: Vec<Cell>, wrapped: bool, dirty: bool, rev: u32 }

struct Grid {
    screen: VecDeque<Row>,       // viewport rows
    scrollback: RingBuffer<Row>, // bounded, byte-budgeted not row-budgeted
    cursor: Cursor, saved_cursor: Cursor,
    scroll_region: Range<usize>,
    modes: TermModes,            // DECAWM, DECOM, bracketed paste, mouse, ...
    alt_screen: Option<Box<Grid>>,
}
```

Decisions that matter later and are expensive to retrofit:
- **Style interning.** Cells hold a `StyleId`, not an inline fg/bg/attrs blob.
  Keeps `Cell` small (cache-friendly scroll + diff) and makes diffing cheap.
- **Grapheme side table.** Single `char` inline; clusters (emoji + ZWJ,
  combining marks) spill into an arena keyed by index. Avoids a `String` per cell.
- **Scrollback budgeted in bytes**, not lines, so a pane full of 4000-column
  lines can't eat a gigabyte.
- **Revision counters per row** (`rev`) rather than boolean dirty flags: the
  renderer and each attached client track "last rev seen", which makes multi-client
  and dropped-frame cases trivially correct.
- Reflow on resize is a `haven-core` operation over `wrapped` flags; it must be
  correct for the alt screen (no reflow) vs primary screen (reflow).

---

## 7. Color

### 7.1 Model
A resolved cell color is one of four things, and the distinction must survive all
the way to the frontend — collapsing it early is a classic bug source:

```rust
enum Color {
    Default,              // SGR 39/49: whatever the theme's fg/bg is *now*
    Indexed(u8),          // 0-15 ANSI (theme-controlled), 16-255 (cube+grays)
    Rgb(u8, u8, u8),      // SGR 38;2 / 48;2
}
```
`Default` is stored as `Default`, never eagerly resolved to the theme's RGB: OSC
10/11 and live config reload must repaint existing cells, and an app that writes
`SGR 39` then changes the palette expects the new color. Likewise `Indexed(0..16)`
stays indexed so OSC 4 palette changes retroactively apply.

### 7.2 SGR surface (all of it, not the common subset)
- `30-37`/`40-47`, `90-97`/`100-107` bright, `39`/`49` default.
- `38;5;N` / `48;5;N` indexed; `38;2;R;G;B` / `48;2;R;G;B` truecolor.
- **Colon sub-parameter form**: `38:2::R:G:B`, `38:5:N`, `58:2::R:G:B`. This is
  the ITU T.416 form, real apps emit it, and parsers that only split on `;` get
  it wrong. `vte`'s parser hands us sub-params; we must actually use them.
- `58`/`59` underline color, `4:0-4:5` underline styles (straight, double,
  curly, dotted, dashed).
- Attributes with their fiddly interactions: `1` bold, `2` dim, `22` resets both,
  `3`/`23` italic, `5`/`25` blink, `7`/`27` reverse, `8`/`28` conceal, `9`/`29`
  strike, `53`/`55` overline. Unknown params are skipped, not fatal — a malformed
  SGR must not desync the rest of the sequence.

### 7.3 OSC color control
`OSC 4;N;spec` set/query indexed (`?` query replies in the exact `rgb:RRRR/GGGG/BBBB`
form xterm uses — apps string-match it), `OSC 104` reset; `OSC 10/11/12` fg/bg/cursor
set and query, `OSC 110/111/112` reset; `OSC 17/19` selection colors. Queries need
replies: `mc`, vim's background detection and many `$COLORFGBG` consumers decide
light-vs-dark theme from them, and a terminal that stays silent gets a
dark-on-dark UI.

### 7.4 Rendering policy
- `bold-is-bright = true|false` (default false; `bold-font-weight` instead).
- `minimum-contrast` — optionally lift fg luminance until an APCA/WCAG threshold
  against the actual bg is met, which rescues the "dark grey on black" comment
  color that plagues 16-color-era configs.
- Dim implemented as alpha toward bg, not a fixed palette swap.
- Reverse video composes with selection and cursor-inversion in a defined order,
  specified once in a table in the code and snapshot-tested, because every
  terminal gets some cell of that matrix wrong.
- `COLORTERM=truecolor` exported; terminfo advertises `RGB` and `Tc`.

### 7.5 Downsampling (TUI frontend)
A 24-bit cell going to a 256- or 16-color host is matched in **OKLab**, not naive
RGB Euclidean distance, and 16-color matching is done against the *host's* actual
palette when `OSC 4` queries succeed, falling back to the standard xterm values.
Separate nearest-match caches per target depth; results memoized per `StyleId`.

---

## 8. The legacy VT surface that real apps actually use

This section exists because "support mc" is 80% unglamorous features that a
from-scratch terminal typically skips, then spends a month retrofitting.

- **Character sets / SCS.** *(Implemented ahead of M2 — see the status section.)*
  `ESC ( 0` (DEC Special Graphics) through G0-G3 with
  `SI`/`SO` locking shifts and `SS2`/`SS3` single shifts. Non-UTF-8 `mc`, `dialog`,
  and plenty of ncurses builds draw every box and line through DEC graphics.
  Internally we map DEC graphics to the Unicode box-drawing equivalents at
  *print* time and keep the charset state in `TermModes`.
- **Alternate screen** (`DECSET 1049`, plus legacy `47`/`1047`/`1048`) with the
  primary screen fully preserved — this is exactly what `mc`'s `Ctrl+O` subshell
  toggle exercises, and it's an easy way to lose a user's scrollback.
- **Mouse protocols:** X10 (`9`), normal (`1000`), button-event (`1002`),
  any-event (`1003`), focus (`1004`), UTF-8 (`1005`), **SGR (`1006`)** and
  urxvt (`1015`) encodings. `mc`, `htop` and `vim` all want mouse, and SGR-1006
  is mandatory past 223 columns.
- **Reports apps block on:** `DA1`, `DA2`, `DA3`, `DSR 5`, `DSR 6` (CPR),
  `DECRQM`/`DECRPM` mode queries, `XTGETTCAP`, `XTVERSION`, `DECRQSS` (reply to
  SGR/DECSTBM queries). Missing replies don't look like a missing feature; they
  look like a hang.
- **F-keys and keypad:** `SS3` vs `CSI` forms for F1-F4, `CSI n~` for F5-F12,
  shifted/ctrl'd variants (`CSI 1;5P`), DECCKM application cursor keys, DECKPAM
  application keypad. `mc` leans on F1-F10 and its `Esc`-digit fallbacks.
- **Terminfo.** We ship a `haven`/`haven-direct` entry and install it via `tic`,
  but **default `TERM=xterm-256color`** until the entry is in ncurses upstream and
  in distro packages — otherwise every `ssh` into a machine without our entry
  breaks `mc` and `vim`. `haven` is opt-in via config with a warning explaining
  the tradeoff. (`$TERMINFO` is also exported so local apps find our entry.)
- Also: tab stops (`HTS`/`TBC`/`CHT`), insert/delete line and char
  (`IL`/`DL`/`ICH`/`DCH`), `DECSTBM` scroll regions with origin mode, `DECSC`/`DECRC`,
  `DECALN`, soft and hard reset (`DECSTR`/`RIS`), `DECSCUSR` cursor shapes,
  margins-aware `ECH`/`EL`/`ED`, `REP`, bracketed paste, OSC 8 hyperlinks,
  OSC 0/1/2 titles, OSC 7 cwd, OSC 9/777 and OSC 99 notifications, OSC 52 clipboard.

---

## 9. Shells, PTY and shell integration (`haven-pty`, `haven-shell`)

### 9.1 Being a correct controlling terminal
Everything interactive in bash/fish/zsh — `Ctrl+C`, `Ctrl+Z`, `fg`, pipelines,
`^C` inside `less` — depends on this being right, and it is all in the first 200
lines of the PTY layer:
`setsid()` → `TIOCSCTTY` → `tcsetpgrp` so the child is a session leader with a
controlling terminal; signals (`SIGINT`/`SIGQUIT`/`SIGTSTP`) delivered to the
*foreground process group*, never to the direct child only; `SIGHUP` to the pgrp
on pane close with a grace period then `SIGKILL`; `TIOCSWINSZ` on every resize,
**including `ws_xpixel`/`ws_ypixel`** (image protocols and some TUIs need them);
`SIGCHLD` reaped without blocking the reader.

### 9.2 Launch semantics
- Shell resolution order: config → `$SHELL` → `getpwuid()` shell → `/bin/sh`.
  The passwd lookup matters: `$SHELL` is frequently stale or absent under
  display managers.
- **Login shells** launched with `argv[0] = "-fish"` (leading dash), not `-l`,
  because that is what fish and bash actually key off; `login-shell = auto` treats
  the first pane of a session as a login shell and splits as non-login, matching
  what users expect from tmux.
- Environment: set `TERM`, `COLORTERM=truecolor`, `TERM_PROGRAM=haven`,
  `TERM_PROGRAM_VERSION`, `TERMINFO`, `HAVEN_PANE`/`HAVEN_SESSION`;
  **unset `LINES`/`COLUMNS`** (stale values break resize for curses apps) and
  scrub our own internals. `haven-mux` keeps the daemon's env snapshot from
  attach time so a pane inherits a sane environment even when the daemon was
  socket-activated with a minimal one — `update-environment` list, like tmux.
- `close-on-exit = always | never | on-success`, with the exit status and signal
  shown in the pane when it's held open.

### 9.3 Shell integration (opt-in, per shell, maintained)
Integration buys: jump-to-previous-prompt, select-command-output, command
duration and exit status in the status line, "new pane in the same directory",
and correct "something is still running" close confirmation. The mechanism is
standard escape sequences, so it degrades to nothing when absent:
- **OSC 133** semantic marks: `A` prompt start, `B` prompt end / input start,
  `C` command start, `D;<exit>` command end.
- **OSC 7** `file://host/path` cwd reporting — the only reliable way to open a
  split in the right directory (reading `/proc/<pid>/cwd` of the foreground pgrp
  is the fallback, and it's wrong the moment a program `chdir`s).

Per-shell reality, which is where the work is:

| Shell | Hooks | Injection |
|---|---|---|
| **fish** | **none needed on >= 3.7** — fish emits OSC 133 A/B/C/D and OSC 7 itself (verified on 4.9.3). Older: `fish_prompt`/`fish_preexec`/`fish_postexec` events | detect the version and install *nothing* when native; otherwise a `vendor_conf.d/haven.fish` via `XDG_DATA_DIRS`, no rc edits |
| **zsh** | `precmd`/`preexec` hooks | `ZDOTDIR` shim that chains the user's real `.zshrc` |
| **bash** | `PROMPT_COMMAND` + `PS0` + a `DEBUG` trap (bash-preexec approach) | `--rcfile` chaining, or document a one-line `.bashrc` addition — bash has no clean injection point and we will not pretend otherwise |

Default: **detect-then-inject**, with `shell-integration = off|detect|<shell>`
and a hard rule that injection failure is silent and harmless. Detection is not
optional politeness: adding our marks to a shell that already emits them
produces two command-end marks per command, and a consumer cannot tell which `D`
belongs to which command. `Injection` therefore reports `Native` distinctly from
`Auto` — "it was already there" and "we did it" are different facts to debug.

Two findings from building this, both the kind that only surface against a real
shell:
- **fish 4.9.3 needs no integration at all.** It emits A, B, C and D (with
  `click_events` and `cmdline_url` extensions) plus OSC 7 natively. Our first
  implementation duplicated `C` and `D` on every command.
- **A `conf.d` wrapper around `fish_prompt` is inert.** `conf.d` runs *before*
  the user's `config.fish`, so any `functions --copy fish_prompt` wrapper is
  overwritten the moment the user defines their own prompt. For fish < 3.7 the
  marks must come from events, which also means `B` (prompt end) is
  unavailable there — there is no post-prompt hook. Documented, not papered over.

### 9.4 fish specifically (it's the dev box's login shell)
fish 4.x probes the terminal at startup and on every prompt: `DA1`, cursor
position (`CPR`), `XTGETTCAP` for capabilities, and the kitty-keyboard query
`CSI ? u`. Each probe has a timeout, so an unanswered one costs visible startup
latency and silently disables features. So those replies are **M1 work, not M2
polish** — the first shell we run will exercise them on day one. fish also uses
bracketed paste heavily, sets titles via OSC 0, and will enable the kitty
keyboard protocol if we advertise it, which is the fast path to T3 fidelity.

---

### 9.5 Virtual shells (`haven-backend`)

A pane's shell need not live on this machine's filesystem. `haven-shell`
answers *which shell, and how does its integration load*; `haven-backend`
answers the question one level out: *where does it run at all*. Containers
first, because that is the one people ask for; the shape is built so a VM, an
ssh host or a namespace is an impl and a registry line rather than a design.

The trait is deliberately the one `Shell` already earned — `name`, `targets`,
`launch`, `rejoinable`, one impl per thing, nothing else branching on which
one it is.

**It crosses no new seams.** A virtual shell is named by a string:

```
docker://run/archlinux:latest?keep&mount=cwd
docker://exec/web
```

which travels as the pane's **program** — the field that already existed, that
the daemon already interprets on its own machine, that the save file already
records and that the command line already accepts. So
`haven run -- new-window 'docker://run/alpine:latest?scratch'` works for free,
and nothing in the protocol, the mux or the emulator knows what a container
is. The one addition to the wire is a `ContainerTarget` in the shell listing,
described rather than named: a label, a second column, and a spec the client
hands back untouched.

**`launch` cannot fail.** It returns a `PtyConfig` that runs a short shell
script, and every way the thing can go wrong — no CLI, a failed pull, a
container that has stopped — is handled *inside* that script, in the pane,
ending in `exec $SHELL` with a line saying why. Two reasons. A pane that
closed before its error could be read takes the explanation with it; and a
spawn error on this path reaches the daemon's event loop, which should not
have to care whether Docker is installed.

**The pull is in the pane.** `docker run` would fetch an absent image by
itself, after one terse line and then several hundred megabytes of
undifferentiated progress. Doing it explicitly means the pane says what is
happening and that it happens once — the second Arch pane is instant, and
nobody who was not told that would guess it.

**Two questions are asked rather than configured.** Which host directories a
container pane gets (none by default: a distro pane is usually opened to be
*away* from your files) and whether what you install in it survives the pane.
The second is asked per pane because the answer differs per pane — scratch to
try one command, kept to build in all week — so a config key would force one
afternoon's answer on the other. The menu asks it as a second step rather
than listing every distro twice.

**The restore line.** A save file decides what a machine runs when it comes
back, which is why `restore_programs` is an allowlist. The same reasoning
here: a restored pane may *rejoin* a container that is already running and may
never start one. That is self-limiting — no file can cause a container to be
created — and a pane whose container is gone is an ordinary shell, which is
what every restored pane is anyway.

**Integration is offered, not installed.** The script is bind-mounted
read-only at `/haven`; bash loads it via `--rcfile` after the container's own
rc files; anything else gets the environment and no marks. Which shell runs is
decided *inside*, by `command -v`, because it cannot be decided outside:
alpine has ash and no bash, and guessing from an image name is wrong for
every image nobody curated.

**`docker` is a setting.** podman takes every argument used here unchanged, so
`cli = "podman"` is the whole of podman support, and a test asserts the word
`docker` appears nowhere in a launch built with another CLI.

## 10. Multiplexing model (`haven-mux`)

```
Session { id, name, windows: Vec<Window>, active: usize, clients: Vec<ClientId> }
Window  { id, title, layout: PaneNode, active_pane: PaneId, zoomed: Option<PaneId> }

enum PaneNode {
    Leaf(PaneId),
    Split { dir: Horizontal|Vertical, children: Vec<(PaneNode, f32)> }, // fractional
}
```

- **N-ary splits with fractional weights**, not a binary tree: makes "three even
  columns" exact and resize monotonic (binary trees drift).
- **Layout solver** converts weights → integer cell rects in one pass, distributing
  rounding remainder deterministically left-to-right/top-to-bottom, reserving 1 cell
  for separators. Pure function, trivially unit-testable; snapshot-test it.
- **Zoom** is a window-level override, not a layout mutation, so unzoom is lossless.
- Named layouts (`even-horizontal`, `main-vertical`, `tiled`) are just functions
  producing a `PaneNode`.
- **Detach/attach:** the daemon keeps full `Grid`s. On attach, a client gets a
  snapshot (RLE-compressed rows) then incremental diffs. Panes are resized to the
  *smallest* attached client, per-session configurable (`resize-policy = smallest | latest`).
- Pane-level persistence of *output* only. Process restoration (tmux-resurrect
  style) is out of scope.

---

## 11. Input: keymap and fidelity tiers (`haven-input`)

Three layers, deliberately separated:

1. **Acquisition** — where the key event comes from, and how much of it survived.
   A GPU client gets rich events from winit (keycode, modifiers, press *and*
   release, repeat). A TUI client gets whatever bytes the host terminal chose to
   send, which may be far less. This is the layer that differs by frontend.
2. **Host layer** — does this keypress belong to Haven? Resolved against key
   tables before anything reaches a pane.
3. **Pane layer** — if not consumed, encode for the child process: legacy
   xterm/DEC encodings by default, **kitty keyboard protocol (CSI u)** when the
   app enables it, plus `modifyOtherKeys` compatibility.

### 11.1 Input fidelity tiers — the central constraint of TUI mode

Inside a host terminal, legacy input is lossy in ways that break keyboard-first
design: `Ctrl+I` is indistinguishable from `Tab`, `Ctrl+M` from `Enter`, `Esc`
from `Alt+` anything (and from the start of every escape sequence), `Ctrl+Shift+X`
is usually just `Ctrl+X`, and there are no key-release or modifier-only events at
all. So fidelity is modelled explicitly, as a tier negotiated at attach:

| Tier | Source | What you get |
|---|---|---|
| **T3 — full** | GPU frontend, or TUI host with kitty keyboard protocol (`CSI > 1 u`, flags `0b11111`) | Distinct keycodes, all modifier combos, press/release/repeat, modifier-only keys. Everything is bindable. |
| **T2 — extended** | TUI host with `modifyOtherKeys=2` (xterm) or partial CSI-u | Most `Ctrl`/`Shift` combos disambiguated. No release events, no modifier-only keys. |
| **T1 — legacy** | TUI host with neither | Control codes + xterm function keys only. `Ctrl+Shift+*` and `Ctrl+I`-vs-`Tab` unavailable. |

Consequences the design must carry, not paper over:
- **Bindings declare their required tier** (derived automatically from the key
  spec). `haven config check` reports which bindings are dead in which tier, and
  the command palette greys them out with the reason — "needs kitty keyboard;
  your terminal is T1".
- **A shipped `tier1` key table** supplies prefix-based equivalents for every
  default binding that needs T2+, so a legacy host is usable out of the box
  rather than mysteriously half-broken. This is where the optional `ctrl+b`
  prefix earns its place.
- **Escape disambiguation** in T1/T2: a bounded `esc-timeout` (default 25 ms,
  configurable) plus "if the next byte is already buffered, it's a sequence" —
  never a blind timeout, which is what makes vim-in-tmux feel soggy.
- Negotiation is **progressive enhancement with a cleanup guarantee**: push the
  kitty flags on attach, pop them on detach, and restore them from an `atexit` +
  `SIGTERM`/`SIGHUP` handler. Leaving a user's terminal in a weird input mode
  after a crash is unforgivable, so the restore path is tested by killing the
  client with every signal in CI.
- Haven's *own* panes advertise T3 upward, so a nested Haven, or neovim, or
  anything speaking CSI u gets full fidelity regardless of the outer host.

### 11.2 Key tables (modal, tmux-like but prefix-optional)
```toml
[keys.root]
"ctrl+shift+enter" = "pane.split vertical"
"alt+h"            = "pane.focus left"
"alt+1".."alt+9"   = "window.select $n"
"ctrl+shift+p"     = "palette.open"
"ctrl+b"           = "keytable.push prefix"   # opt-in tmux muscle memory

[keys.prefix]          # one-shot table, auto-pops
"c" = "window.new"
"%" = "pane.split vertical"
'"' = "pane.split horizontal"
"[" = "mode.copy"

[keys.copy]            # sticky table until mode.exit
"h" = "copy.left"
"v" = "copy.select_toggle"
"y" = "copy.yank"
```
Resolution order: active table → `root` → pane. Tables push/pop on a stack with
`one_shot` and `timeout_ms`. This one mechanism gives prefix keys, copy mode,
resize mode and a vim-ish leader without special cases.

### 11.3 Keyboard-first details that decide whether it feels good
- Physical-key matching (`key_without_modifiers`) so bindings survive Dvorak/AZERTY.
- A **command palette** over the same action enum the keymap uses — there is
  exactly one action registry, derived via a macro so help text, palette entries
  and config validation can't drift.
- **Copy mode** with vim motions, visual/block select, incremental search,
  `OSC 52` + Wayland/X11 clipboard, and quick-select hint labels for URLs/paths.

---

## 12. GPU frontend (`haven-render`, `haven-font`)

### 12.1 Pipeline
Per frame, per visible pane, build an instance buffer and issue few draws:
1. **Backgrounds** — one instanced quad pass; adjacent same-bg cells merged into
   runs so a solid screen is a handful of instances.
2. **Glyphs** — instanced textured quads against a glyph atlas. Instance =
   `{grid_pos: u16x2, atlas_rect: u16x4, color: u32, flags: u32}` = 16 bytes.
3. **Decorations** — underline/undercurl/strikethrough/overline, cursor,
   pane separators, selection, hint labels. Undercurl drawn procedurally in the
   fragment shader (no atlas entries per width).

### 12.2 Glyph atlas
- One `R8Unorm` array texture for alpha masks + one `Rgba8UnormSrgb` array for
  color emoji; 2048² layers, shelf (skyline) packer, grow by adding layers.
- Key: `(font_id, glyph_id, px_size, subpixel_x_bucket, synthetic_flags)`.
- LRU eviction with a per-frame pin set; on overflow, evict unpinned and re-raster.
- Shaping cache keyed by `(run bytes, style)` → glyph list, so repeated lines
  (prompts, status bars) skip shaping entirely.

### 12.3 Text correctness inside a grid
- Compute `cell_width` from the primary font's advance; snap advances to integer
  multiples so columns stay aligned even with a fallback font.
- Ligature support only when a run has uniform style and no cursor inside it;
  otherwise fall back to per-cell shaping. (This is the pragmatic rule that
  avoids both "no ligatures" and "cursor corrupts the ligature".)
- Font fallback chain resolved per grapheme cluster, cached in a hash map.
- Correct linear-space alpha blending and configurable gamma/contrast boost —
  this is the single biggest "why does text look wrong" bug class.

### 12.4 Frame scheduling (where latency is won or lost)
- The PTY reader thread drains up to N bytes or T µs, parses into the grid, and
  marks row revisions. It **does not** request a frame per chunk.
- The client redraws at most once per vsync, rendering whatever state exists;
  bursty output coalesces into one frame instead of starving input.
- Zero output and no input = zero frames, zero timers, no polling.
- Optional **immediate-present on keypress** path: when a frame is only an input
  echo, skip the coalescing delay (Ghostty-style) to cut perceived latency.
- `--vsync=off` plus a frame-time HUD behind a debug flag for measurement.

---

## 13. TUI frontend (`haven-tui`, `haven-vtwrite`)

A first-class client, not a fallback. It draws the same `FrameModel`: panes,
separators, tab bar, status line, copy mode, palette, plugin widgets.

### 13.1 The serializer is the inverse of the parser
`haven-vtwrite` turns a cell region into the minimum escape sequence that makes
a host terminal show it. This symmetry pays for itself three times: the TUI
client, `haven capture-pane`/session recording, and the round-trip property test
in §17 (`core → write → parse → core` must be the identity).

Emission rules that matter for both bytes-on-wire and flicker:
- **Damage-driven.** Row revision counters give us exactly the changed rows; we
  diff cell runs within them and emit per-run `CUP` + SGR + text.
- **Cost model per row:** compare `CUP`-and-patch against rewrite-whole-row
  against `EL`-then-write, pick the cheapest in bytes. A row that changed in 20
  scattered cells is usually cheaper to rewrite wholesale.
- **SGR run coalescing** with a running style cursor: only emit the delta from
  the current pen, and prefer `SGR 0` resets when that is shorter.
- **Scroll detection.** If the new state is the old state shifted by N rows, emit
  an index/scroll-region sequence instead of repainting — the difference between
  smooth and horrible when tailing a log over SSH.
- **Synchronized output** (`DECSET 2026`) wrapped around every frame when the host
  supports it; this alone removes nearly all tearing in TUI mode.
- One `write_all` per frame into a `BufWriter` sized to avoid partial-frame
  flushes on a slow link.

### 13.2 Host capability probe
On attach, probe with hard timeouts and cache the result per `TERM`+version:
truecolor vs 256 vs 16, underline styles + colors (`SGR 4:3`, `58:2`),
synchronized output, kitty keyboard, OSC 52 clipboard, OSC 8 hyperlinks,
bracketed paste, focus reporting, SGR-1006 mouse, Unicode width agreement,
graphics passthrough. Everything degrades: truecolor → nearest 256 → nearest 16;
undercurl → underline; widgets drop to ASCII box-drawing when the host's
line-drawing or width handling is suspect.

### 13.3 Re-encoding for the host
Two conversions the TUI frontend owns, both easy to get subtly wrong:
- **Color**: OKLab nearest-match down to the host's depth (§7.5), against the
  host's real palette when `OSC 4` queries answer.
- **Line drawing**: we normalized DEC Special Graphics to Unicode at parse time,
  so for a UTF-8 host we emit UTF-8 directly; for a non-UTF-8 or
  ambiguous-width-hostile host we re-encode box-drawing back to DEC graphics via
  `SO`/`SI`. Never pass through the app's raw charset state.

### 13.4 Degradations we accept, and state plainly in the docs
No ligature control, no subpixel positioning, no custom font or padding (the host
owns those), cell size fixed by the host, per-pane font size unavailable, image
protocols only by passthrough. In exchange: zero GPU, works in a Linux console,
works over a 200 ms SSH link, works in whatever terminal the server has.

### 13.5 Nesting and the inception problems
- **Clipboard:** copy mode yanks via OSC 52 through the host when available;
  otherwise the daemon writes to its own display's clipboard and says which.
- **Resize:** `SIGWINCH` → re-probe size → `Resize` message. With heterogeneous
  clients attached, `resize-policy = smallest | latest | per-client` decides;
  `per-client` keeps independent viewports onto the same panes, which is the
  honest answer and the hardest to implement (scheduled for M7, not M6).
- **Mouse/focus passthrough:** forwarded as events, never as raw bytes.
- **Nested Haven** is detected by an env marker; a client run inside a Haven pane
  offers `switch-client` instead of starting a second daemon, and the inner
  instance's key-table stack is explicitly addressable so one keypress can be
  routed to the inner or outer instance deterministically.

---

## 14. Protocol and server lifecycle (`haven-proto`, `havend`)

Versioned, length-prefixed postcard frames over `$XDG_RUNTIME_DIR/haven/<id>.sock`.

```rust
enum ClientMsg {
  Attach { session: SessionRef, caps: ClientCaps },   // frontend kind, size,
                                                      // color depth, key tier
  Key(KeyEvent), Paste(String), Mouse(MouseEvent),
  Action(Action),                       // same enum the keymap produces
  Resize { client_size: CellSize }, Detach,
}
enum ServerMsg {
  Hello { proto: u32, features: Features },
  Snapshot { pane: PaneId, rows: Vec<RowRle>, cursor: Cursor, rev: u32 },
  Delta    { pane: PaneId, rows: Vec<(u16, RowRle)>, cursor: Cursor, rev: u32 },
  Layout(WindowLayout), Bell(PaneId), Title { pane: PaneId, title: String },
  PaneExited { pane: PaneId, status: ExitStatus },
}
```
`ClientCaps` is what keeps the daemon frontend-blind: it carries the frontend
kind, cell size, color depth, key-fidelity tier and feature bits, and the daemon
tailors only *content* (how many colors to resolve, whether to send widget trees
as cells or as a tree) — never behaviour.

Handshake negotiates a `proto` number; the daemon refuses mismatches with a clear
"restart your daemon" message instead of a deserialization panic. Deltas are
capped per frame; overflow triggers a snapshot resync (bounded memory under a
pathological producer).

### 14.1 Server lifecycle
- Socket at `$XDG_RUNTIME_DIR/haven/<uid>-<name>.sock`, systemd socket activation
  supported; first `haven attach` autostarts a daemon and waits on a readiness
  handshake rather than sleeping.
- `haven server stop` is graceful (SIGHUP semantics per session: configurable
  `on-last-detach = keep | kill`), with `--force` for the blunt version.
- **Live upgrade:** on `haven server upgrade` the daemon serializes session state
  (grids, scrollback, PTY fds via `SCM_RIGHTS`) and execs the new binary,
  re-adopting fds. Panes keep running across a Haven update. This is the one
  genuinely hard piece of the daemon and it is scheduled explicitly, not assumed.
- Crash resilience: scrollback and layout journalled to `$XDG_STATE_HOME` so a
  daemon kill loses output but never the session *shape*; PTYs die with the
  daemon (no re-parenting magic promised).

### 14.2 Control mode (`haven -C`)
A line-oriented JSON protocol on stdio over the same `ClientMsg`/`ServerMsg`
types: subscribe to events, invoke actions, read pane content. This is how
scripts, editors, test harnesses and the `haven` CLI verbs themselves talk to the
daemon — the CLI is a control-mode client, so anything the CLI can do, a script
can do. Plugins get the same surface in-process.

---

## 15. Native plugin system (`haven-plugin`, `haven-plugin-api`)

The rule that keeps this honest: **the status line, the tab bar, copy-mode hints
and the command palette are written against the plugin API.** If the API can't
express them, it's not finished. No private back door for built-ins.

### 15.1 Runtime
- **wasmtime** with the Component Model; one `Store` per plugin instance, its own
  linear memory, fuel-metered and epoch-interrupted so a runaway plugin is
  killed, not tolerated. Host stays responsive by construction.
- Plugins run **in the daemon** (they need pane state and must survive client
  detach). UI-only plugins get their draw output forwarded to clients over
  `haven-proto` as widget trees — clients never execute plugin code.
- Lifecycle: `load → init(caps) → subscribe(events) → …events… → shutdown`.
  Hot reload on file change in dev mode; a panicking plugin is unloaded, logged
  and reported in the UI, never retried in a loop.

### 15.2 The WIT world (sketch)

```wit
package haven:plugin@0.1.0;

world plugin {
  import terminal;   // read grid cells, cursor, selection, scrollback ranges
  import panes;      // enumerate/split/focus/close/zoom; read titles, cwd, pids
  import actions;    // invoke any registry action; register new ones
  import keys;       // claim bindings & key tables; push/pop modal layers
  import ui;         // status segments, overlays, pickers, popups (widget tree)
  import notify;     // notifications, bell handling
  import store;      // per-plugin scoped KV persistence
  import net;        // opt-in, host-gated, allowlisted
  import proc;       // opt-in, host-gated: spawn with argv, no shell

  export init: func(cfg: config) -> result<_, string>;
  export on-event: func(ev: event) -> list<command>;
  export render: func(ctx: render-ctx) -> widget;   // pure, called on demand
}
```

Design constraints baked into the shape above:
- **`on-event` returns commands**, it does not mutate the host. Effects are data,
  applied by the host, which makes plugins replayable and testable offline.
- **`render` is pure** and may be called at any time; plugins cannot drive frames.
  No plugin can cause a redraw storm.
- Events are **coalesced and rate-limited per plugin** (e.g. `output-chunk` is
  delivered as a batched row-revision summary, never per byte).

### 15.3 Capabilities and trust
Declared in a manifest, granted by the user, enforced by the host:

```toml
# ~/.config/haven/plugins/gitline/plugin.toml
name = "gitline"; version = "0.2.1"
entry = "gitline.wasm"
events = ["pane-focus", "cwd-change", "timer:2s"]
[capabilities]
read-pane-content = false       # grid contents are sensitive; off by default
exec = ["git"]                  # argv allowlist, no shell interpolation
net = []                        # empty = denied
fs = ["$CWD:ro"]
```
- Default deny. Grants are per-plugin and shown in a diff-style prompt on install
  and on any manifest change — an update that newly asks for `read-pane-content`
  requires re-consent.
- `read-pane-content` is the dangerous one (passwords, tokens scroll through a
  terminal). It's opt-in, audited in the log, and indicated in the UI while active.
- No `exec` through a shell, ever. argv arrays only.

### 15.4 Distribution & authoring
- A plugin is a directory with a manifest + `.wasm`, installed from a path, a git
  URL or an archive. Lockfile with content hashes; no auto-update.
- `haven-plugin-api` is the Rust guest SDK: `#[haven::plugin]` macro, typed event
  enums, a widget builder. `cargo component build` and you're done.
- `xtask plugin-test` runs a plugin headless against a recorded event stream and
  snapshots its returned commands/widgets — plugin tests need no GPU and no shell.
- Ship 3 reference plugins that exercise distinct corners: a git status segment
  (`exec` + timer), a session/file picker (UI + actions + keys), and a URL/path
  hint provider (`read-pane-content`).

### 15.5 What this costs
Plugins are the second-largest subsystem after the terminal core, and building
the built-ins on top of the API means M3–M4 can't be finished until the API
shape is right. That ordering is the point: it's cheap now and near-impossible
to retrofit once the status line has a private path into the host.

---

## 16. Milestones

Each milestone ends with something you can *run*, plus its own tests.

**M0 — Skeleton (3–4 d)**
Workspace, CI (fmt/clippy/test, Linux + macOS), `xtask`, logging/tracing, crash
handler. `haven` opens an empty wgpu window that clears to the theme background.
*Done when:* window opens on X11 and Wayland, resizes cleanly, exits on `ctrl+q`.

**M0.5 — Frontend + transport seams (3 d)**
Define `trait Frontend`, `trait Transport`, `FrameModel` and `ClientCaps` *before*
either frontend exists, with a `NullFrontend` (records frames) and a
`ScriptedFrontend` (replays input) for tests.
*Done when:* a test drives the whole client loop with no window and no terminal.
This is 3 days that prevents a 2-week refactor at M6.

**M1 — One pane, one shell (2 wk)**
`haven-pty` with full job control (§9.1) + `vte` Performer + `Grid` (no
scrollback) + glyph atlas + the monospace shaper. Legacy key encoding. No mux.
Includes the query replies shells block on — `DA1`/`DA2`, `DSR 5`/`6`,
`XTVERSION`, `XTGETTCAP` — because fish probes for them at every prompt and a
missing reply reads as a hang, not a missing feature.
*Done when:* `fish`, `bash` and `zsh` start with no probe-timeout stall;
`Ctrl+C`/`Ctrl+Z`/`fg` behave; `vim`, `htop` and `less` are usable;
`vttest` menus 1–2 pass.

**M2 — Terminal correctness: the whole VT surface (2.5 wk)**
Scrollback + reflow, alt screen (`1049` + legacy `47`/`1047`/`1048`), scroll
regions + origin mode, insert/delete line and char, tab stops, `DECSTR`/`RIS`,
`DECSCUSR`, `REP`. **Color in full** (§7): 16/256/truecolor, colon sub-param SGR,
underline styles and colors, OSC 4/10/11/12 set *and query with correct reply
formats*, `COLORTERM`. **Charsets/SCS** (G0-G3, `SI`/`SO`, `SS2`/`SS3`, DEC
Special Graphics → Unicode) — the thing non-UTF-8 `mc` and `dialog` draw every
frame with. **All mouse protocols** incl. SGR-1006. Wide chars and grapheme
clusters. `DECRQM`/`DECRQSS`. Bracketed paste, OSC 0/7/8/52/104, hyperlinks.
Terminfo source + `tic` install, with `TERM=xterm-256color` as the default.
*Done when:* `vttest` 1–6 pass; curated `esctest` subset green; the recorded
session corpus is committed and byte-stable; `mc` in a non-UTF-8 locale draws
unbroken frames.

**M2.5 — The serializer + a crude TUI client (1 wk)**
`haven-vtwrite` (damage diffing, SGR coalescing, cost model, synchronized output)
and a minimal TUI frontend: one pane, raw mode, `SIGWINCH`, legacy input only.
*Done when:* `haven --tui --solo` gives a working single-pane terminal inside
another terminal, and the `core → write → parse → core` round-trip test passes on
the whole M2 corpus. Early placement is deliberate: the serializer is a test
oracle and a debugging tool for everything after it, and it proves the
`Frontend` seam is real rather than aspirational.

**M2.7 — Application compatibility gate (4 d, then permanent)**
Build the headless app-compat harness and make it a CI gate rather than a
one-time check: drive each app on a PTY with a scripted key sequence and snapshot
the resulting grid. First cohort: **mc** (incl. `Ctrl+O` subshell toggle, F-keys,
mouse, non-UTF-8 line drawing), vim, neovim, htop, less, tig, nano, emacs -nw,
ranger, lazygit, and tmux-inside-Haven.
*Done when:* every app in the cohort has a committed golden screenshot and a
regression in any of them fails CI. Placed here because every later milestone
(reflow, mux, daemon, TUI) can silently break these, and finding that out at M9
is what makes a terminal feel unfinished at 1.0.

**M3 — Mux core (2 wk)**
`haven-mux` in-process (`--solo`): PaneTree, layout solver, splits, focus
movement, zoom, close, window tabs with a tab bar and a status line.
*Done when:* splitting, focusing, resizing and zooming work and the layout solver
has property tests (weights always sum to the available cells; no 0-size panes).

**M3.5 — Plugin API shape (1 wk, before the built-in UI hardens)**
Write the WIT world, the host skeleton (wasmtime, store-per-plugin, fuel/epoch
limits, capability broker), and the guest SDK. Then **port the status line and
tab bar to it**.
*Done when:* the built-in status line is a plugin loaded by default, and deleting
it from the config leaves a working terminal with no status line.

**M3.7 — Shells and shell integration (1 wk)**
Shell resolution (config → `$SHELL` → `getpwuid`), login-shell `argv[0]`
handling, environment construction and `update-environment`, `close-on-exit`
policy, foreground-pgrp inspection for titles and close-confirmation. OSC 133 +
OSC 7 consumption, and the three integration scripts: `conf.d/haven.fish`,
a `ZDOTDIR` shim for zsh, `--rcfile` chaining plus documented manual setup for
bash.
*Done when:* jump-to-prompt, select-command-output, command duration/exit status
and "split in the same directory" all work in fish, zsh and bash; integration
failure is silent and harmless; `shell-integration = off` changes nothing else.

**M4 — Keyboard system + palette (1.5 wk)**
Key tables, chords, action registry macro, command palette, copy mode with vim
motions and search, quick-select hints, clipboard integration, kitty keyboard
protocol both directions. Palette and hints are built as plugins.
*Done when:* every action is bindable and discoverable from the palette; a
third-party plugin can register its own actions and claim bindings; `:help keys`
is generated from the registry, not hand-written.

**M5 — Daemon + attach/detach (2 wk)**
Split `havend` out, implement `haven-proto`, snapshot/delta sync, multi-client
attach, `resize-policy = smallest|latest`, socket activation and autostart,
`haven ls | attach | kill-session | new-session`, control mode (`haven -C`) with
the CLI rebuilt as a control-mode client.
*Done when:* `pkill -9 haven` loses no shell state; a GPU client and a TUI client
attached to the same session stay in sync; a 100 MB `cat` keeps both under
budget; every CLI verb is expressible in control mode.

**M5.5 — Plugin hardening & distribution (1.5 wk)**
Manifest/consent flow, lockfile + hashes, hot reload, crash isolation and
reporting, event coalescing and rate limits, widget forwarding to clients over
`haven-proto`, `xtask plugin-test`, the three reference plugins, and docs.
*Done when:* a plugin that infinite-loops, allocates 4 GB or panics is killed
without affecting any shell; plugin tests run in CI with no GPU.

**M5.9 — Live daemon upgrade (4 d, can slip past 1.0)**
State serialization + PTY fd handoff via `SCM_RIGHTS` + exec of the new binary.
*Done when:* `haven server upgrade` across two builds keeps a running `vim`
alive. Isolated as its own milestone because it is the single riskiest daemon
feature and must be droppable without blocking release.

**M6 — TUI frontend, for real (2 wk)**
Full parity: panes, separators, tab bar, status line, copy mode, palette, plugin
widgets. Host capability probe with timeouts and caching. Kitty keyboard
negotiation with push/pop and signal-safe restore. Color/underline/width
degradation ladders. Scroll-detection emission. OSC 52 clipboard, bracketed
paste, focus and mouse passthrough. Nested-Haven detection + `switch-client`.
*Done when:* a 4-pane session is fully operable over a 200 ms SSH link and in a
bare Linux console; killing the client with `SIGKILL`/`SIGTERM`/`SIGHUP` always
leaves the host terminal's modes restored (tested for each signal in CI).

**M6.5 — Fidelity tiers end-to-end (4 d)**
Tier detection, per-binding tier requirements, the shipped `tier1` table,
palette/`config check` reporting of unavailable bindings, escape disambiguation.
*Done when:* the default keymap is fully usable at T1, T2 and T3, and
`haven config check --tier=1` names every binding that degrades and to what.

**M7 — Config & theming (1 wk)**
TOML config, live reload on change, theme files, per-pane font size, padding,
cursor styles, bell behaviour, `haven config check` with good diagnostics.

**M8 — Performance pass (1.5–2 wk)**
`vtebench` + `alacritty/vtebench` scenarios in CI with regression thresholds;
flamegraphs on parse/shape/upload; atlas churn metrics; input-latency measurement
(keypress→present, with a photodiode-free `--latency-probe` harness).
Plus TUI-specific work: bytes-per-frame budget, per-client `per-client` resize
viewports, and latency over a simulated high-RTT link (`tc netem`).
*Targets:* idle 0% CPU, zero wakeups · >1 GB/s parse on `cat /dev/urandom | base64` ·
GPU keypress→present < 1 frame + 2 ms · TUI added latency < 3 ms over the host
terminal's own · < 8 KB/frame for a typical TUI redraw · memory < 60 MB for
10 panes × 10k scrollback.

**M9 — Platform & polish (2 wk)**
macOS (Metal, IME, Cmd bindings), Windows ConPTY behind a feature flag,
HiDPI/multi-monitor DPI changes, ligature polish, notifications, packaging
(AUR PKGBUILD + static-ish release builds), docs site.

Rough total: **23–27 weeks** of focused solo work to a usable 1.0 — terminal core
~5, VT surface + color + charsets ~2.5, app-compat harness ~1, shells ~1,
mux ~2, plugins ~3, daemon ~3, TUI frontend ~3.5, keyboard ~2, perf/platform ~3.5. The TUI frontend and the daemon are each about as much work as the GPU
renderer; budgeting them as "a fallback mode" is the most common way this kind of
project ends up with a bad one.

---

## 17. Testing strategy

- **Unit/property:** layout solver (proptest), reflow, style interning, ring buffer.
- **Golden screens:** feed recorded byte streams into `haven-core`, snapshot the
  grid as text + style map. Catches 90% of VT regressions with no GPU involved.
- **Conformance:** `vttest` manual checklist per release; scripted `esctest` subset in CI.
- **App compatibility matrix (the gate that actually matters):** each app in the
  M2.7 cohort driven headlessly on a PTY with a scripted key sequence; grid
  snapshotted and diffed. Run in both UTF-8 and `LANG=C` locales, since the
  second is what exercises DEC Special Graphics.
- **Shell matrix:** fish, bash, zsh × {login, non-login} × {integration on, off}
  — assert startup emits no probe timeout, `Ctrl+C`/`Ctrl+Z`/`fg` reach the right
  process group, OSC 133/7 marks land where expected, and no stray `LINES`/`COLUMNS`.
- **Color tests:** SGR parsing table-driven over both `;` and `:` forms incl.
  malformed input; OSC query reply strings byte-compared against xterm's format;
  downsampling snapshots per target depth; the reverse/selection/cursor
  composition matrix.
- **Fuzzing:** `cargo-fuzz` on the Performer (arbitrary bytes must never panic,
  never allocate unboundedly, never index out of range) and on protocol decoding.
- **Renderer:** headless wgpu + image-diff snapshots per platform, with a
  tolerance mask (drivers differ in the last bit).
- **Round-trip identity:** for every golden stream, `core → haven-vtwrite →
  vte parser → core` must produce an identical grid. One property test that
  validates the serializer, the parser and the cell model against each other.
- **TUI client under test:** run the TUI frontend on a PTY we own, parse its
  output with `haven-core` itself, and assert on the resulting grid. The TUI
  client is therefore fully testable headlessly — no terminal, no GPU, no human.
- **Host-terminal matrix:** replay recorded capability-probe responses for xterm,
  foot, kitty, alacritty, ghostty, wezterm, VTE/gnome, tmux, screen, `linux`
  console and Apple Terminal; assert the chosen degradation ladder for each.
- **Signal/restore tests:** kill the TUI client with each signal, assert the host
  terminal's termios + DEC modes + kitty flags are restored.
- **Attach/detach soak:** randomized attach/detach/resize/kill of N clients
  against one daemon for M iterations; assert no desync (client grid == daemon
  grid), no leaked fds, no unbounded memory.
- **Perf gates:** vtebench numbers tracked per commit; CI fails on >10% regression.

---

## 18. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Reflow-on-resize is the classic bug swamp | Isolate in `haven-core` behind a pure function; build the golden-screen corpus *before* implementing it. |
| Shaping-in-a-grid (ligatures, emoji, fallback metrics) eats weeks | Ship M1 with per-cell shaping; make ligatures an opt-in flag added in M8. |
| Protocol diffing becomes the bottleneck | Row revision counters + RLE from day one; keep `--solo` as a measurement baseline. |
| wgpu portability surprises | Stay on the conservative feature set (no bindless, no push-descriptors); test GL backend in CI. |
| Shipping `TERM=haven` breaks `mc`/`vim` on every machine without our terminfo | Default to `xterm-256color`; `haven` entry opt-in with a warning; export `$TERMINFO`; upstream the entry to ncurses early since that lead time is months. |
| Charsets/SCS skipped as "legacy", then `mc` draws `qqqq` instead of lines | In M2 scope with an explicit non-UTF-8 `mc` acceptance test in the compat gate. |
| App compat regresses quietly as reflow/mux/TUI land | The compat matrix is a CI gate from M2.7, not a release checklist. |
| bash shell-integration injection breaks someone's `.bashrc` | No silent rc rewriting for bash: `--rcfile` chaining or documented opt-in; failure is a no-op. |
| Image/graphics protocols retrofitted later touch every file | Placement list reserved in the pane model now (§2), even though the protocols ship post-1.0. |
| Scope creep into "tmux clone" | Freeze the action registry at M4; anything beyond it ships as a plugin, not core. |
| Plugin API ossifies around v0.1 mistakes | Version the WIT world; host supports N and N-1. Dogfood by building the status line, tab bar, palette and hints on it before any third party exists. |
| A plugin leaks terminal contents (tokens, passwords) | `read-pane-content` default-deny, re-consent on manifest change, audit log, in-UI indicator while a plugin holds it, `net` denied unless separately granted. |
| Plugins become the latency story | They run in the daemon, never in the render path; `render` is pure and host-scheduled; events coalesced and fuel-metered. |
| Daemon/client version skew | Explicit `proto` handshake with a human-readable refusal; live upgrade (M5.9) isolated so it can be dropped. |
| TUI mode treated as a fallback and shipping bad | It is a milestone with parity acceptance criteria (M6), it is testable headlessly, and the serializer lands at M2.5 as a test oracle so it is exercised continuously from early on. |
| Lossy keyboard input makes the keyboard-first design collapse in TUI mode | Fidelity tiers are explicit in the keymap model, a `tier1` table ships by default, and `config check` tells the user exactly what degrades. |
| Leaving a user's terminal wrecked after a crash | Mode push/pop with signal-safe restore, verified per-signal in CI. |
| Double-emulation latency (our grid → host terminal → screen) | Damage diffing, scroll detection, synchronized output, bytes-per-frame budget, and `tc netem` latency gates in M8. |
| `Frontend`/`Transport` seams drift and the modes fork | Seams defined at M0.5 with Null/Scripted impls; `haven-mux` is forbidden by build rule from linking either frontend. |

---

## 19. First week, concretely

1. `cargo new --lib` the workspace; wire CI, `xtask`, and the `haven-mux`
   must-not-link-a-frontend build check.
2. `haven-pty`: open a PTY, `setsid` + `TIOCSCTTY` + `tcsetpgrp`, fork `$SHELL`,
   echo raw bytes to stdout. ~250 lines, proves the platform layer — and get job
   control right here, not later, because every signal bug downstream looks like
   an emulator bug.
3. Answer `DA1`, `DSR 5`/`6` and `XTGETTCAP` with hardcoded replies. Ten minutes
   of work that stops fish stalling on every prompt and saves a day of
   "why is startup slow" confusion.
4. `haven-core`: `vte::Parser` + a `Performer` that handles `print`, `LF`, `CR`,
   `CUP`, `ED`, `EL`, SGR 0/1/30-37/38;2. Render the grid as text with `println!`.
5. `haven-render`: wgpu init, one instanced quad pass, hardcoded white squares
   where glyphs will go. Prove 60 fps and vsync pacing before any font code.
6. `haven-font`: fontdb load of a monospace family, swash raster of `'A'`,
   upload to the atlas, draw it.
7. Write `trait Frontend` / `trait Transport` / `FrameModel` and make the thing
   you just built in steps 4–6 the *first* `Frontend` impl rather than the only
   possible one — then add `NullFrontend` and assert on a frame in a unit test.

Steps 1–6 are M1's spine; step 7 is what makes M2.5 and M6 cheap instead of a
rewrite.

---

## 20. Implementation status

Updated as milestones land, so the plan above does not drift from the tree.
**645 tests, zero clippy warnings, `cargo test --workspace` green.**

| Milestone | State |
|---|---|
| **M0** workspace, CI, lints | Done. fmt/clippy/test on Linux + macOS, plus a layering gate so `haven-core` can never link a frontend. |
| **M0.5** frontend/transport seams | Done. `Frontend`, `Transport`, `FrameModel`, `ClientCaps`, `Intercepted`/`KeyFilter`, with `NullFrontend`/`ScriptedFrontend`/`ScriptedTransport`. The whole client loop runs in tests with no window, terminal or daemon. |
| **M1** one pane, one shell | Done. Job control, login `argv[0]`, pixel-aware `TIOCSWINSZ`, `SIGHUP`-then-`SIGKILL`, hangup-on-drop, and the query replies shells block on. |
| **M2** terminal correctness | Done bar graphics: scrollback (byte-budgeted), reflow with cursor tracking, alternate screen, scroll regions, insert/delete, erase modes, tab stops, `REP`, `DECSCUSR`, `DECSTR`, SCS + DEC Special Graphics, grapheme clusters, full SGR incl. colon sub-parameters, OSC 4/10/11/12/104/110-112 set *and* query, OSC 7/52. |
| **M2.5** serializer + TUI client | Done. `haven-vtwrite` (damage diffing, SGR coalescing, per-row cost model, scored scroll detection, synchronized output, OKLab downsampling) and `haven-tui` (raw mode, signal-safe restore, input decoding, compositing). |
| **M2.7** app-compatibility gate | Running: mc in UTF-8 *and* `LANG=C`, mc's `Ctrl+O` subshell toggle, vim, less, bash marks, a fish double-mark guard, and `haven tui` inside `haven`. |
| **M3** mux core | Done. N-ary pane trees with fractional weights, the layout solver (+8 proptest properties), windows, zoom-as-override, named layouts, geometric focus movement, and a runtime that gives every pane its own pty and geometry. |
| **M3.7** shells | Done. `Shell` trait with fish/zsh/bash/generic impls, native-capability detection, `ZDOTDIR` shim, `--rcfile` chaining. |
| **M5** daemon + attach/detach | Done bar live upgrade. `havend` as `haven server run`, framed postcard over a unix socket, per-client row-revision sync, style/cluster table shipping, `Smallest`/`Latest` resize policies, autostart, `haven attach \| ls \| kill-session \| server stop`. Verified end to end: a session outlives its client and reattach restores the screen. |
| **M1** GPU frontend | Done bar polish. wgpu pipelines, shelf-packed glyph atlas, font fallback with a last-resort system scan, sRGB-correct colour, procedural box drawing, a winit window with vsync, and `haven screenshot` so the renderer stays reviewable without a display. No italic faces, no ligatures, no colour emoji yet. |
| **M4** keyboard | Partial. A prefix key table resolving to a declarative `Action`, shared by the standalone and attached paths. The modal stack, configurable bindings, palette and copy mode are still to come. |

**A resize at a prompt clears the prompt's rows** before reflowing, where
shell integration has reported where the prompt began (`OSC 133;A`). A shell
repaints on `SIGWINCH` and clears only as many rows as its own model says the
prompt occupies — fish moves up exactly one row — so a rewrap that made the
prompt taller leaves its first row orphaned for good. Blanking the region the
shell is about to redraw costs nothing and removes the debris; it is guarded
on a reported mark, on no command running, and on not being the alternate
screen, because it discards screen content. The cleared rows lose their
`wrapped` flags too, or the reflow joins a blank row to the one after it and
the debris becomes one very long line.

**Resizes are coalesced.** A size change is drawn at once and told to the
daemon after 90ms of stillness, and not at all when it settles on the size
the daemon already has. A resize is a `SIGWINCH` and a shell answers one by
redrawing its prompt, so the uncoalesced version turned a held zoom key or a
dragged window edge into a screenful of prompts over a scrollback reflowed
at a dozen widths. Twenty resize events now reach a pane as one signal.

**The history and grep modals** are the daemon answering questions a client
cannot: both live where the shells and the scrollback are. A pane's history
is merged from two sources with different knowledge — `OSC 133` marks, which
are this pane's and carry exit statuses, and the shell's own history file,
which is global, older and statusless — and the difference is shown rather
than smoothed over. Picking one *types* it at the prompt: a command is not
idempotent across an hour and a directory change, and the one time that
matters is the one nobody would have chosen. The help panel is `whatis` and
a man page's opening, looked up on a thread so the event loop never waits on
`man`, cached, and matched to the line that asked for it so a late answer is
dropped rather than shown against the wrong command. Nothing is executed to
describe it. Grep spans every pane in the session, since the line you are
looking for is in the window you are not looking at, and each hit carries
window, pane and row because none of the three implies the others.

**Zoom** (`Ctrl+=` / `Ctrl+-` / `Ctrl+0`) is a frontend capability, not a mux
one: `Frontend::zoom` returns the new pixel size, or `None` from a frontend
whose font is not its own — a client drawing into somebody else's terminal
cannot resize that terminal's font, and the key says so rather than doing
nothing. The window answers by re-measuring the already-loaded faces (a
zoom costs a measurement, not another scan of every font on the system),
emptying the atlas, and pushing itself a resize: the same pixels now hold
fewer, larger cells, and the session is told so through the path a window
resize already uses. The new size is handed to a saver the composition root
supplies, because the config file is not the client's to know about, and
written as a single line so that pressing `Ctrl+=` cannot reformat a file
full of someone's comments.

**Releases and packaging** are in `packaging/`, chained so that one link
cannot drift from the next: a tag runs `.github/workflows/release.yml`, which
builds on a pinned `ubuntu-22.04` (a glibc-linked binary will not start on an
older system than it was built against, and `ubuntu-latest` moves), and
uploads the tarball, a copy of the installer and `SHA256SUMS`. `install.sh`
reads `SHA256SUMS` first — it names the tarball, so `latest` resolves without
an API call — and refuses anything whose checksum does not match. The two AUR
packages, one from the binary and one from source, are pointed at the
published release by `packaging/aur/refresh.sh` and pushed by
`publish.sh`, which is deliberately not in CI: the AUR authenticates by SSH
key alone, so automating it would mean a personal key in a secret. A musl
build would remove the glibc floor and cannot be used — a static musl binary
cannot `dlopen`, which is how the window finds Vulkan.

**Crates:** `haven-core`, `haven-pty`, `haven-shell`, `haven-backend`,
`haven-vtwrite`, `haven-proto`, `haven-client`, `haven-tui`, `haven-mux`,
`haven-daemon`, `haven`.

**Deviations from the plan above, deliberately:**
- **The daemon uses a poll loop, not tokio.** The pane pump was already
  poll-based and nothing here benefits from task-per-pane concurrency.
  Revisit if per-pane parsing ever needs its own core.
- **The daemon was built before the plugin API** (M5 before M3.5), because
  plugins are specified to *run in the daemon* — building the host first would
  have meant building it in the wrong place and moving it.

**Not started:** plugins (M3.5), live daemon upgrade (M5.9), fidelity tiers
end-to-end (M6.5), sixel/kitty graphics. Ligatures need a shaping engine;
bold, italic and colour faces are in.

Containers as panes are in (§9.5), inside the shell chooser rather than beside
it: which shell and where it runs are one decision, and a second key would
have meant choosing between them before seeing either list.

**Still missing against tmux**, deliberately and with reasons: hooks and the
`run-shell`/`if-shell` family, which are a scripting language rather than a
feature and which `haven run` covers from outside; status-line format strings,
which are a templating language; session groups and linked windows; the clock.
`base-index` has nothing to configure — windows and panes are numbered from
one already, which is what the option exists to arrange in tmux.

**Known gaps:**
- The status line and tab bar are built-in rather than plugins (M3.5 inverts
  that), though both are already pure functions of state.
- `haven attach` has no detach-and-leave-running key beyond `Ctrl+A d`, and no
  session naming yet (`-t` takes an id).

Status line and command line (part of M4) are in: the client keeps the bottom
row, builds the bar as a `WidgetKind::StatusLine` — the same mechanism a
plugin will use — and the daemon sends a `SessionStatus` whenever it changes.
Window names fall back to the active pane's title, which is why the status is
sent on change rather than on layout change: titles arrive later than layouts
and move while the layout does not.

The command line now completes (`Tab`, plus a box of candidates), and
`Ctrl+A s` opens a session tree. The tree lives in `haven-client` rather than
behind the key filter because it is the only place holding both halves of
what it shows — the session listing and the attached session's windows — and
a filter owning it would be a second place that knows what a session is. It
is a flat list of indented rows, not a real tree: expand/collapse state would
buy nothing when the point is to see everything at once.

A naming decision worth recording, because it caused a real misunderstanding:
*window* means two things here. `haven-mux/src/window.rs` is a tab — it owns a
pane tree — and `haven-render/src/window.rs` is the thing on the desktop,
which is a client and not part of a session at all. tmux never had the second
sense and so could call the first one a window without ambiguity; this program
cannot. The interface now says *tab*, while the protocol, the mux types and the
command names keep tmux's `window` (with `new-tab` and friends accepted as
aliases). The session tree therefore has three levels, not four: the desktop
window is what is looking at it.

The interface is themed, and a theme is *data* rather than code, which is why
it did not wait for the plugin host. The daemon owns the config file and sends
`ServerMsg::Theme` on attach and whenever the file's timestamp moves, so an
edit repaints every attached client without a reattach. Two types, not one:
the wire `Theme` is a fixed struct of plain `Color`s, and the daemon's
`ThemeFile` is all-optional with string colours. They were briefly the same
type, which does not work — postcard is not self-describing and cannot
deserialize the untagged colour that makes a config file pleasant to write.

Tabs are named after what they are running. `OSC 133` was being *installed*
by the shell integration and then ignored by the emulator; it is parsed now,
and the `C` mark's command line is what names a tab, falling back to the
title and then the number. Getting the command out of each shell differs:
fish sends it natively, zsh's `preexec` receives it, and bash exposes it only
in a `DEBUG` trap or in history — the trap is the wrong place, because it
also fires for every `PROMPT_COMMAND` function and names the tab after the
prompt, so `PS0` plus `history 1` is used instead. It is expanded exactly
once per interactive command line, which is the gate wanted.

There was no way to make a second session. `Attach { session: None }` calls
`first_or_create`, which joins the first session that exists and only creates
one when there are none, and nothing else created one at all — so a daemon
held exactly one session for its whole life, and the chooser's session level
could never have more than one row. `ClientMsg::NewSession { name, caps }`
creates and attaches in one step, bound to `Ctrl+A` `C` (capital, because `c`
is the new tab) and typed as `new-session`. It is a client-side action like
detaching, because the daemon needs this client's size and only the frontend
knows it.

Scrollback was kept but could not be read. The grid has held rows that
scrolled off since M2; nothing exposed them. `Grid::view(offset)` is a
screenful taken from `scrollback ++ live`, so at offset 0 it is exactly the
live rows and the rest of the daemon does not have to know whether anyone is
scrolled. The offset lives in `PaneSync`, per client and per pane, because two
clients on one session must read at their own pace.

Row revisions cannot diff a moving view: `rev` is a per-row counter, so two
unrelated rows can share one, and comparing by index is only sound while the
view sits still. The view is therefore resent whole whenever the offset moves
or history grows underneath it, which is cheap — a client that is reading is
not a client watching a build scroll past.

Copy mode is the client's, like the chooser, because it needs the view the
frontend is showing *and* the pane it belongs to. Its coordinates are the
awkward part and are worth stating: the cursor is in view coordinates because
that is where it is drawn, while a selection's anchor is a `HistoryPoint`,
absolute in `scrollback ++ live`. Selecting more than a screenful means
scrolling, and a view-relative anchor slides out from under the selection as
soon as it does.

The cursor keeps its *view* row across a scroll rather than its history row.
Re-pinning it, which looks more correct, undoes the move the scroll was for:
`k` at the top of the view scrolls one row to show the row above, and a cursor
that followed its old history row would land straight back where it started.

Extraction and search are the daemon's, not the client's: only the daemon has
the whole history, and joining a wrapped row to the next without a newline —
which is what makes a copied command paste back as one command — needs the
`wrapped` flag that never reaches a client.

The mouse reaches panes now. The emulator had tracked 1000/1002/1003 and 1006
since M2 and no client ever encoded an event back, so clicking in vim did
nothing. The pane's modes ride along on `ServerMsg::Modes`, the client encodes
in the pane's own coordinates, and what a pane did not ask for stays with the
multiplexer: wheel to read history, click to focus, drag a rule to resize.
The terminal client also had to learn to *decode* mouse reports — `raw.rs` has
been asking hosts for 1002 and 1006 all along, and the replies were falling
through the key path.

Key bindings are data now, and come from the same config the theme does. The
built-in table is the default *value* of that data rather than a `match`, so
a configured binding and a built-in one go through one path — the alternative
is a table config can only partly override, which is how bindings end up with
two behaviours. That forced the command vocabulary to cover every bound key:
there were no names for focus, resize or the literal prefix, so the claim that
"anything a binding can do, a typed command can do" was not true until now.
A test walks the default table and parses every command in it, because a typo
in data is a key that silently does nothing rather than a compile error.

A pane can be asked to run something other than the session's shell, which
is what `Alt+C` is for: the menu lists what the *daemon's* machine has, since
that is where the pane is spawned and a client's idea of where `fish` lives
means nothing across a network. The spawner therefore takes a `PaneRequest`
rather than a size. The alternative — smuggling the program through shared
state the way the session environment is — would have hidden a one-shot
argument inside something that looks like persistent state.

The menu itself knows nothing about shells. It holds labels paired with
command strings and asks the key filter to resolve them, exactly as a typed
line is resolved, so a menu cannot offer anything the command line could not
do and the next menu is data rather than code.

Sessions survive a reboot now. What is saved is the shape of the work —
sessions, tabs, the pane tree with its weights, and each pane's working
directory — never the processes, which are gone. The directory is read from
`/proc/<pid>/cwd` rather than from `OSC 7`: the kernel answers whatever is
running and whether or not shell integration is installed, which is the case
a save has to cover.

A program is started again only if the config names it, and it is typed at
the restored shell rather than exec'd so that quitting it leaves a shell.
The allowlist is not caution for its own sake: a save file is a file on disk
that decides what a machine runs when it comes back, and replaying whatever
happened to be in the foreground is how a restore becomes an exploit.

The save format is its own set of types rather than the mux's, because a file
written by one version is read by another and must not change shape every
time the pane tree does. Leaves refer to panes by position, so the file never
names an id that will not exist again.

The key table is modal now. `root` and `prefix` always exist; any other name
is a table of your own that *stays armed* until something switches away,
which is what makes one prefix and six resizes possible. `prefix` keeps its
one-shot behaviour. `Escape` always leaves a sticky table, and `switch-table`
refuses a name no table answers to — arming one would swallow every key and
answer none.

The capability probe writes every query in one batch ending with `DA1`. That
is the whole design: `DA1` is answered by every terminal, so its reply means
every other reply that was coming has come, and the deadline is only reached
by a terminal that answers nothing at all. A per-query timeout would instead
cost one timeout per capability the host lacks, on exactly the terminals that
lack the most. Silence is a *no* only for the keyboard protocol, where being
wrong means keys that do nothing and where every implementing terminal
answers; elsewhere silence keeps the environment's guess, because answering
`DA1` and nothing else is common and reading that as "no styled underlines"
takes a working feature away.

Colour glyphs render in their own colours. The rasterizer already asked for
`ColorOutline` and `ColorBitmap` sources and then flattened the result to
coverage, so an emoji came out as a grey blob in the foreground colour. The
atlas is RGBA now and a coverage mask is stored as white with the coverage as
alpha, which lets one texture serve both: the shader's only question is
whether to tint, carried as one float per quad. A second texture for colour
glyphs would have meant a second bind group and a pipeline switch in the
middle of a line of text, to save four megabytes.

The atlas is `Rgba8UnormSrgb`, so a colour glyph's bytes are converted to
linear on sampling exactly as the target converts back on write. A mask is
unaffected: white is white in both, and alpha is never gamma-encoded.

**Bugs this work surfaced, worth remembering:**
- `$SHELL` is inherited from whatever started the daemon, so a pane opened
  as bash in a session started under fish reported fish — and everything
  that spawns a subshell through `$SHELL`, from `vim :sh` to `git`, got the
  wrong one. Corrected only for a program that *is* a shell: `haven -- mc`
  must not claim `SHELL=/usr/bin/mc`.
- The shell menu split the window while `c` made a tab everywhere else, so
  one letter meant two things. It makes a tab now, and what to do with the
  pick travels with the request rather than being baked into the menu.
- A root binding was blocked while a table was armed. `Ctrl+A` and then a
  change of mind left the prefix armed, so `Alt+C` was looked up in the
  prefix table, missed, and was swallowed — doing nothing and reporting
  nothing, which is indistinguishable from a broken binding. A root binding
  means "this acts with no prefix"; an armed table must not disable it.
- `FrameModel::notice` was set in five places and drawn in none. Every "say
  so" written for a search that found nothing, a tab that did not match, or
  a save that failed was true only in the test. It takes the bar now, and
  the next keystroke is where it stops being true.
- The modes raw mode pushes were a `OnceLock`, which was correct while raw
  mode was entered exactly once. The capability probe enters it first, so
  every later restore used the probe's modes and `haven tui` never left the
  alternate screen. An atomic is both updatable and safe to read from the
  signal handler, which a mutex is not.
- `c` was bound twice in the default key table. Bindings resolve last-wins,
  which is a feature for a config and a bug in the built-in list.
- `haven restore` restored nothing on a fresh daemon. The CLI attached before
  asking, and attaching with no session named calls `first_or_create` — so it
  made an empty `haven-1`, and the restore then skipped the saved session of
  that name because one was already there. Save and restore do not attach.
- `OSC 52` payloads were kept base64-encoded and handed to the client as if
  they were text, so honouring one would have put `aGVsbG8=` on the clipboard.
- `haven server run -- prog -x` dropped `-x`. Every other subcommand does
  `cfg.args.extend(args.rest)`; the daemon's spawner never did, so the program
  ran with no arguments at all and looked like it had simply ignored them.
- Panes stacked one above another had nothing between them. The layout
  reserves a cell between neighbours in both directions (`SEPARATOR`), but
  the client's compositor only ever drew the vertical rule, so a row split
  showed as a blank line and read as two panes running together. Standalone
  mode had drawn both all along, which is why it never came up. Rules are now
  decided from a map of which cells a pane covers: a gap is a rule only where
  there is a pane on each side of it — which is also what keeps the row the
  status line sits on from growing one — and the arms come from the
  neighbouring gap cells, so four panes meet at a `┼` however the splits were
  nested. The glyphs and the colour come from the theme, as everything the
  interface draws must; `plain` therefore gets `+`, not a hole.
- Requesting `Limits::downlevel_defaults()` caps textures at 2048 *whatever
  the hardware can do*, so a window 2088 pixels tall could not be given a
  surface and wgpu aborted the process — a validation error cannot be caught.
  It never showed while the GPU default was the discrete card, because that
  window happened to be smaller; switching to the integrated one and
  maximising found it. The resolution limits now come from the adapter
  (`using_resolution`), and sizes are clamped besides, because a window is
  whatever size the user dragged it to. Measured, not assumed: the limit goes
  from 2048 to 16384 on this machine, which `cargo run -p haven-render
  --example limits` prints.
- Box drawing vanished wherever a panel gave it a background. Procedural box
  drawing and background rectangles shared one quad buffer, and a background
  run is flushed when it *ends* — after the cells it covers have been drawn —
  so it painted over every line beneath it. Panes never showed it because
  their lines sit on the default background, which emits no quad at all.
  Backgrounds are collected separately now and drawn underneath.
- Rounded corners are outside `U+2500-259F`, so they fall back to the font and
  leave a gap where they should meet the line the renderer drew itself. The
  default glyphs are square for that reason.
- `PROTOCOL_VERSION` was never read. The client ignored it in `Hello`, so
  version skew was only noticed when some later message failed to decode — in
  whichever code path first asked for one. Attaching to a daemon from an older
  build worked, and then `Ctrl+A s` closed the window, which looks exactly
  like the chooser crashing. It is checked at the handshake now, and both
  frontends report why they stopped instead of vanishing: the TUI after it has
  given the terminal back, the window through its own error.
- The GPU window ran without a key filter at all. It built its own `Client`
  and never installed one, so `Ctrl+A` went straight to the shell and no
  binding in the window did anything. Both attach paths now take the filter as
  a required argument. Reading the key-mapping code would never have found it:
  the mapping was correct.
- Then the prefix still did nothing, because a window delivers key *releases*
  and a terminal does not. The release of `Ctrl+A` matched the prefix table
  and disarmed the prefix a moment after the press armed it; in command mode
  it would have typed every character twice. The encoder already dropped
  releases, so only the mode machine ever saw them. Every test had used
  presses alone, which is why both frontends looked identical in test and
  behaved differently in life.
- Neither was findable without synthetic input, so `xsend` (XTEST) exists:
  with no window manager nothing holds the input focus, so it sets the focus
  itself before typing.
- `ED`/`EL` treated parameter `0` as "absent", shifting every erase mode by one
  and breaking `[1K`/`[2K`. Caught by a tab-stop test, not by the erase tests.
- A deferred wrap changes the cursor's *logical* offset by one; without that,
  widening a reflowed screen walked the cursor backwards.
- Trailing blank rows must not count as content during reflow, or every
  narrowing pushes real text into scrollback.
- The layout solver laid panes out past the screen edge when a one-cell floor
  per pane plus separators exceeded the available space. Found by proptest; a
  pane that cannot fit is now hidden, as tmux does.
- A strict shift comparison never detects a scroll, because applications write
  the new bottom line *before* scrolling. Scoring the overlap fixes it.
- Emitting `24;39;49;59` after an `SGR 0` is pure byte waste.
- `str::replace` with a non-matching pattern is a silent no-op: an edit script
  that does not assert can "succeed" while changing nothing.
- A dropped `Pty` left its child running, so a dead client stayed attached from
  the daemon's point of view. Dropping the master now hangs up the group — and
  reaps it, since `kill(pid, 0)` succeeds for a zombie.
- Render pipelines built for a hardcoded texture format fail validation on a
  window whose surface prefers BGRA. Found by running the window under a
  software rasterizer, which chooses differently than the discrete GPU.
- Growing a pane can legitimately shrink it, two ways: hidden siblings become
  visible when the parent grows, and integer apportionment can withdraw a
  rounding bonus (the Alabama paradox).
- A private parameter marker changes what a CSI final byte means, and the two
  forms share that byte. `CSI > 4 ; 1 m` is XTMODKEYS (modifyOtherKeys), which
  fish sends before *every* prompt; read as `SGR 4;1` it set underline and bold
  on the pen and nothing ever reset them, so every cell erased afterwards was
  drawn with a rule through it — a line across the blank end of each row.
  Reported from a real window, not caught by any test. The same mistake made
  `CSI > 1 u` (kitty keyboard push) restore the cursor and `CSI ? Pm r/s`
  (XTRESTORE/XTSAVE) move the scroll region. Private sequences other than DA2
  are now ignored rather than run as their public namesakes.
- The daemon holds the emulator and outlives every client, so rebuilding after
  a parser fix changes nothing until the daemon restarts — and that is
  indistinguishable from a fix that did not work. It cost a round trip of "it
  still does it". `ServerMsg::Sessions` now carries the daemon's build stamp
  and `haven ls` says so plainly.
- `PROTOCOL_VERSION` could not report its own skew: the version lives *inside*
  a message, so a wire-format change makes the frame fail to decode before
  anything can compare versions. The decode error now names the likely cause
  instead of reporting "Hit the end of buffer". Proper negotiation ahead of the
  first typed message is still owed.
- The fallback font scan set its "already scanned" flag only when it *found*
  a face. A character no font covers — an emoji, on a machine with no colour
  emoji font — therefore rescanned every font on the system every time one
  appeared: 3829 faces, 8.6GB, read in full to look at their character maps.
  A prompt with three emoji took 5.69s to render, of which 5.56s was syscall
  time. The faces are now mapped once and kept; the same render takes 0.249s.
  Reported as "takes forever to render", and nothing to do with the GPU.
- The AwesomeWM freeze was none of the three things it looked like. Measured
  with `crates/winit-probe`: not winit (`--no-gpu` never stalled), not vsync
  (`Immediate` still blocked 2052ms), and not our event loop (moving present
  to its own thread freed the loop and the freeze continued). What it is: on
  this hybrid laptop Vulkan can only run on the discrete NVIDIA GPU, which
  reaches an Intel-driven display over PRIME, and presenting across that while
  the window manager unmaps the window **blocks the X server itself** — a bare
  `GetInputFocus` on an unrelated connection took 2050ms, so every client on
  the display froze, not just ours. `--backend gl` was unaffected because GL
  resolves to the Intel GPU here; there is no Intel Vulkan ICD installed, so
  "GL vs Vulkan" and "integrated vs discrete" could not be separated.
  A terminal has no use for a discrete GPU, so `GpuPreference` now defaults to
  the integrated one, with `--gpu` and `--gpu-power` to override.
- winit offers no signal for this. `Occluded` comes from `VisibilityNotify`,
  which an unmapped window never sends, and `is_visible()` stayed `true`
  through the whole stall. Asking the X server directly (`GetWindowAttributes`
  → `map_state`) does work, and the probe proves it, but it cannot win the
  race: the unmap and the in-flight present land in the same instant.
- Presenting to an unmapped window is not merely wasted work. With
  `PresentMode::Fifo` the acquire waits for a vblank the window will never get,
  and that wait sits inside the event loop — so X requests go unanswered and a
  single-threaded window manager (AwesomeWM) stalls with it. The window now
  stops rendering entirely while occluded. A window manager may also report
  `0x0` while unmapping, which would otherwise resize the session to one cell
  and reflow every pane for a window nobody can see.
