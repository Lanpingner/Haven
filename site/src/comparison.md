# How it compares

Haven sits in the Alacritty/kitty class — a native, GPU-accelerated terminal,
not an Electron app and not a TUI toolkit — and takes the detachable-server
model from tmux rather than expecting you to run tmux inside it.

The table below is Haven's own reading of the alternatives. Every one of them is
a good program, and several are far more finished than this one. Corrections are
welcome as pull requests against this page.

<div class="compare">

| Capability | Alacritty | kitty | WezTerm | Ghostty | tmux | **Haven** |
|---|---|---|---|---|---|---|
| GPU rendering | yes (GL) | yes (GL) | yes (wgpu) | yes (Metal/GL) | n/a | **yes** — wgpu: Vulkan/Metal/DX12/GL |
| Tabs & splits | no, by design | yes | yes | yes | yes | **yes** — recursive n-ary trees |
| Detachable sessions | no | no | yes (mux server) | no | yes | **yes** — survives GPU crash, logout, SSH drop |
| Runs inside another terminal | no | no | CLI only; the mux client is the GUI | no | yes, that is all it does | **yes** — a full TUI frontend of the same client |
| Multiplexing without a second VT layer | n/a | in-process, GUI-only | yes | in-process, GUI-only | no — it re-parses and re-emits | **yes** — one parser, one grid model |
| Extensions | none | kittens: Python, in-process, unsandboxed | Lua config & event hooks | none yet | shell scripts | **WASM components, capability-sandboxed**, 16 shipped — status segments only so far |
| Where extensions come from | n/a | `kitten` from a URL, or copy a file | `wezterm.plugin.require` on a git URL | n/a | copy a script, or a plugin manager | **signed repositories**, apt-style: one pinned GPG key per repository, hashed artifacts, a grant prompt per capability |
| Config | TOML, reload | custom `conf`, reload | Lua | custom, reload | `.tmux.conf` | **TOML, live reload, server-side** so SSH sees it too |
| Configurable keys | yes | yes | yes | yes | yes | **yes** — server-side, live, same vocabulary as the command line |
| Copy mode & search | selection only | yes | yes | selection only | yes | **yes** — vi keys, search over the whole history |
| Save & restore | n/a | yes (sessions) | yes | n/a | plugin (`resurrect`) | **built in** — layout, names and directories, with an allowlist for programs |
| Hyperlinks (`OSC 8`) | no | yes | yes | yes | passes through | **yes** — Ctrl+click, and re-emitted in the TUI |
| Ligatures / images | no / no | yes / kitty graphics | yes / sixel + kitty | yes / kitty graphics | n/a | **not yet** / not yet (cell model reserves placements) |
| Colour emoji | no | yes | yes | yes | n/a | **yes** — `COLR`, `CBDT` and `sbix`, untinted |
| Maturity | stable, packaged | stable, packaged | stable, packaged | stable, packaged | ancient and bulletproof | **v0.0.1**, from source, Linux/X11 tested |

</div>

## What Haven actually buys you

### Over Alacritty and Ghostty

- **Sessions that outlive the window.** No tmux needed to keep a build alive
  when the compositor restarts.
- **Tabs and splits without a second program** to configure, bind and keep in
  sync.
- **The same session over SSH.** Attach from your laptop's window, reattach from
  a console, get the same panes.

### Over tmux, inside anything

- **No double translation.** One parser owns the bytes; nothing re-encodes a
  screen to be re-parsed.
- **One keymap, one config.** No disagreement between emulator and mux about
  what `Ctrl+Shift` meant.
- **Real key events** are the design target — the kitty keyboard protocol in
  both directions, not the 1978 control-code collapse.
- **GPU text and scrollback** in the same program that owns the panes.
- **Save and restore built in**, not a plugin you have to find — and with the
  same allowlist discipline `tmux-resurrect` arrived at, for the same reason.
- **A pane can be another distro.** The shell chooser lists containers next to
  your shells, so an Arch shell is two keys rather than a `docker run` you
  have to remember the flags for. See
  [a pane that is another distro](./containers.md).

### Over kitty

- **Sessions detach.** `kitty @` controls a live GUI; kill the GUI and the
  shells go with it.
- **Extensions are specified sandboxed** — WASM with declared capabilities
  rather than in-process Python. Once they exist.
- **A TUI frontend** for the machine with no display.

### Over WezTerm, the closest rival

- **A real terminal frontend.** WezTerm's multiplexer is excellent, but
  attaching to it means running the GUI; Haven's TUI client draws the same
  session with escape sequences.
- **A much smaller core** — about 21k lines of Rust, no Lua runtime, no
  scripting VM anywhere near the hot path.
- **A layering gate in CI** that keeps the terminal core unable to link a
  frontend.

Honestly: WezTerm wins today on polish, platforms, images, ligatures and
packaging. Haven's bet is the smaller core and the terminal-side client.

## Where Haven loses, today

- **It is v0.0.1.** No binaries, no packages — you build it with cargo.
- **Linux/X11 is the tested platform.** macOS runs the test suite in CI; the
  window has not been exercised there. Windows/ConPTY is unwritten.
- **Plugins can only draw on the status bar.** The sandbox, the capability
  model and [signed repositories](./plugins.md) are built and sixteen plugins
  run in them — but `render` returns text for the bar, so no plugin can put up
  an overlay or a picker of its own yet, and nothing lets a user configure one.
- **No ligatures** in the GPU frontend yet; they need a shaping engine. Bold,
  italic and colour faces are in.
- **No sixel, no kitty graphics protocol.** Post-1.0 by design, though the cell
  model reserves image placements so retrofitting them does not touch every
  file.
- **No tmux wire compatibility.** You get tmux's mental model, not its control
  protocol.
- **No hooks, `run-shell` or `if-shell`** — a scripting language rather than a
  feature, and `haven run` covers the need from outside.
- **No status-line format strings**, session groups or linked windows.

## What Haven takes from each, deliberately

- **From Alacritty:** a small, fast, boring core. A grid, a parser, a renderer,
  no feature that costs idle CPU. Its *absence* of multiplexing is the gap Haven
  fills.
- **From kitty:** feature breadth without apology — tabs, layouts, the keyboard
  protocol, remote control, graphics.
- **From tmux:** the server/attach model and the key-table mental model, without
  the second VT translation layer or the nested-escape-sequence tax.
