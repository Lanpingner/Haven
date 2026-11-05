# Haven Terminal

A GPU-accelerated, keyboard-first terminal emulator in Rust with multiplexing —
sessions, window tabs and split panes — built into the core instead of layered
on via an external tmux process.

It runs three ways from one codebase: as a **server** you attach to and detach
from, as a **standalone** terminal with no daemon, and as a **TUI client** that
draws into any existing terminal over plain escape sequences.

**Status: usable, with plenty still ahead.** Sessions, splits, window tabs,
zoom, attach/detach, copy mode, configuration, save and restore, container
panes, shell integration and a GPU window all work; `mc`, `vim` and `htop`
render correctly. Plugins, live daemon upgrade and ligatures are not written
yet. See
[docs/PLAN.md](docs/PLAN.md) for the design and
[its status section](docs/PLAN.md#20-implementation-status) for exactly what
exists.

There is a project page at <https://lanpingner.github.io/Haven/> — an
mdBook whose chapters are the markdown files in [site/src/](site/src/). Build it
with `mdbook serve site --open`; the last chapter includes this repository's
[docs/PLAN.md](docs/PLAN.md) verbatim rather than copying it.

## Install

```sh
# Arch, from a signed pacman repository — no AUR account needed
curl -fsSL https://github.com/Lanpingner/Haven/raw/master/packaging/repo/add-repo.sh | sudo sh
sudo pacman -Syu && sudo pacman -S haven-terminal-bin

# the key that repository is signed with:
#   2F10059CF6636B150B9584867D04983E5D12A3E4

# Arch, from the AUR
yay -S haven-terminal-bin        # the binary from the release
yay -S haven-terminal            # or build it yourself

# Anywhere else
curl -fsSL https://github.com/Lanpingner/Haven/releases/latest/download/haven-install.sh | sh
```

The installer checks the download's checksum before it believes it, installs
into `~/.local` without ever calling `sudo`, and takes `--prefix`,
`--version`, `--build` and `--uninstall`. It is short and worth
[reading first](install.sh) — piping a script from the internet into a shell
is a thing to be uneasy about.

`haven attach` and `haven tui` need nothing but glibc 2.35 or newer. The
window loads Vulkan, libxkbcommon and your display server's client library at
runtime, which is why a terminal-only install can do without all of them.
How the release, the installer and the two AUR packages fit together is in
[packaging/README.md](packaging/README.md).

## Try it

```sh
cargo test --workspace     # 645 tests, incl. mc/vim/less driven on real ptys

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

`Alt+P` opens the command palette: every command, narrowed as you type.
`Alt+C` opens a new tab and asks which shell to run in it — the same thing
`c` does, with a choice. `Ctrl+A` `G` does it as a pane instead — arrows or `j`/`k`
to move, a digit to pick one outright, `Enter` to take the highlighted one,
`Esc` to drop it. The list is the daemon's machine's, since that is where the
pane is spawned: your shell first, then the ones Haven has integration for,
then whatever else `/etc/shells` holds.

Resizing is a conversation with the shell: a burst of zooms reaches the
session as one resize, and where shell integration has reported a prompt
(`OSC 133;A`), the prompt's rows are cleared before the rewrap because the
shell is about to redraw exactly those. Without that, a hard zoom leaves a
line of merged prompt fragments above the live one — as it does in every
terminal that reflows.

`Ctrl+=` and `Ctrl+-` make the text bigger and smaller in the window, and
`Ctrl+0` goes back to the configured size — Alacritty's keys, and every
browser's. The window keeps its pixels, so a bigger font means fewer rows and
columns and the session is resized to match. The size is remembered in
`[font] size` in your config, written as one line so the rest of the file
survives. In `haven attach` the surrounding terminal owns the font, and the
key says so rather than doing nothing.

`Ctrl+A` then:

| Key | Action |
|---|---|
| `%` / `"` | split into columns / rows |
| `h` `j` `k` `l` or arrows | move focus |
| `H` `J` `K` `L` | resize |
| `z` | zoom the active *pane* — the layout, not the text |
| `x` | close the pane |
| `c` / `n` / `p` / `1`-`9` | new tab / next / previous / select |
| `C` | new session, and move to it |
| `w` / `;` | the tab / pane you were on before |
| `,` / `$` | rename this tab / the session |
| `[` | copy mode |
| `]` | paste the newest buffer |
| `PageUp` / `PageDown` | read back through the pane's history |
| `Home` / `End` | the oldest row / back to the live screen |
| `e` / `E` / `t` / `Space` | even-horizontal / even-vertical / tiled / next layout |
| `o` | swap this pane with the next |
| `{` / `}` | rotate the panes round |
| `!` | break this pane out into its own tab |
| `&` | kill the whole tab |
| `r` | respawn the pane's program |
| `S` | synchronize: type into every pane of the tab at once |
| `q` | number the panes until the next key |
| `f` | find a tab by name |
| `D` | detach every other client |
| `Ctrl+S` / `Ctrl+R` | save every session / build back the saved ones |
| `P` | the command palette |
| `g` / `G` | new tab / new pane, choosing what runs in it |
| `d` | detach |
| `:` | open the command line |
| `s` | the session tree: sessions, windows, panes |
| `Ctrl+A` | send a literal `Ctrl+A` |

Scrolling also works with the mouse wheel in `haven gpu`, over whichever pane
the pointer is on — not necessarily the focused one. While a pane is scrolled
the bar says so, its cursor is not drawn, and typing returns to the live
screen. The offset is per client: two people on one session read at their own
pace. The alternate screen keeps no history, so a full-screen program scrolls
nothing.

### Configuration

`$XDG_CONFIG_HOME/haven/config.toml` on the daemon. It is re-read when its
timestamp moves, so an edit reaches every attached client without a reattach.

```toml
theme = "gruvbox"          # a built-in, a file in themes/, or an inline table
mouse = true               # false hands the mouse back to the host terminal
autosave_seconds = 900     # save every session this often; 0 is off
restore_programs = ["vim", "less"]   # what a restore may start again

[keys]
prefix = "C-b"             # default C-a
bind = { "|" = "split-v", "-" = "split-h", "x" = "" }   # empty unbinds
root = { "C-t" = "new-window", "A-c" = "choose-shell" }   # no prefix needed

[keys.table.resize]        # a table of your own; it stays armed
h = "resize-left"
Escape = "switch-table root"
```

Key specs are `C-`, `S-`, `A-`/`M-`, `D-` in any order, then a character or a
name (`Up`, `PageUp`, `Enter`, `Space`, `F5`, …). The commands are the ones
the command line takes, so nothing is bindable that cannot be typed.

### Save and restore

`Ctrl+A` `Ctrl+S` saves every session; `Ctrl+A` `Ctrl+R` builds back what is
in the file. From a script, `haven save` and `haven restore` — restore starts
the daemon if there is not one, which is the state a machine is in after a
reboot.

What comes back is the *shape* of the work: sessions and their names, tabs and
their names, the pane tree with its proportions, and each pane's working
directory. The processes are gone; a restored pane is a fresh shell in the
right place. A program is started again only if it is named in
`restore_programs`, and it is typed at the shell rather than exec'd, so
quitting it leaves you with a shell.

```toml
restore_programs = ["vim", "nvim", "less", "man", "tail", "htop"]
autosave_seconds = 900     # 0 turns it off
```

A restore never touches a session that is already there — it matches by name
and skips — so it is safe from a login script that may run twice, and safe to
run while you are working. The file is
`$XDG_DATA_HOME/haven/sessions.toml`, written atomically, and it is TOML you
can read and edit.

### History, and finding things

`Alt+H` opens the focused pane's command history with a panel beside it
saying what each command *is*:

```
┌─ history ──────────────────────────────────────────────────────┐
│ ! false                      │ grep (1) - print lines that     │
│ + cargo test --workspace     │ match patterns                  │
│   grep -rn needle crates/    │                                 │
└────────────────────────────────────────────────────────────────┘
```

The list merges two sources that know different things: what *this pane*
ran, learned from shell integration, with how each command ended (`+`
succeeded, `!` failed, `*` never said); and the shell's own history file —
fish, zsh or bash, each in its own format — which goes back months and knows
nothing else. Type to narrow, `Enter` to take one.

`Enter` **types** the command at your prompt rather than running it. What a
command did an hour ago is not what it would do now, and the once that
matters is the once you would not have chosen.

The panel comes from `whatis` and the opening of the man page. Nothing is run
to describe it — `--help` would cover more and means running a program that
may do its job instead — and the lookup happens on a thread in the daemon, so
holding an arrow down does not start a `man` per keypress.

`Ctrl+A` `/` greps **every pane in the session**, not just the one in front
of you, and lists what it found with the window and row. `Enter` goes there.
It is the other half of copy mode's search: that one answers *where is the
next one*, this one answers *where are they all* — including in the window
you have since left.

### Containers as panes

`Alt+C` lists the shells on this machine and, in the same menu, containers:
the ones already running, the images you have, and a handful of distros that
are not here yet. Pick `archlinux:latest` and the pane is an Arch shell. The
model is nix-shell's — a pane that is Arch for ten minutes to see whether the
build breaks there.

One menu rather than two keys, because it is one decision: "give me a shell"
does not split into *which shell* and *where it runs* until after you have
seen the list.

A machine that builds things has a lot of images, so the list is one line per
image rather than per tag, one line per repository for commit-shaped build
tags, and distros before the things you built — sixty-two rows became
forty-one on the machine this was written on. Past a dozen entries you narrow
it by typing instead of by number. Nothing is hidden that a name cannot still
reach.

An image is asked one question first, per pane, because the answer differs per
pane: **keep** reuses one lasting container per image, named
`haven-<image>`, so what you install stays installed; **scratch** is `--rm`,
gone with the pane. A container that is already running is never asked — it
outlives the pane either way — and picking one is a `docker exec` alongside
whatever it is doing.

An image that is not here is pulled *in the pane*, with a line saying what is
happening and that it happens once: a pane that sits blank while half a
gigabyte arrives is indistinguishable from a broken one. Every failure ends in
a working pane and a reason — no Docker, a failed pull, a container that has
since stopped. Asking for a stopped container gets a local shell and a line
about it, never a container Haven started on your behalf.

Nothing of your filesystem is in there until you say so:

```
:container-mount cwd         # the directory the pane is opened in
:container-mount /srv:/work  # or a path, somewhere else inside
:container-mounts            # say what they are
```

Shell integration is *offered* rather than installed: the script is mounted
read-only at `/haven`, bash loads it through `--rcfile` after the container's
own rc files, anything else gets the environment and no marks, and nothing in
the container is modified. Which shell runs is decided inside, by `command -v`
— alpine has ash and no bash, and guessing from an image name would be wrong
for every image nobody listed.

```toml
[containers]
enabled = true
cli = "docker"      # podman takes the same arguments; this is the whole of podman support
images = ["archlinux:latest", "ubuntu:latest", "alpine:latest"]
```

It is one implementation of a `Backend` trait, shaped like the `Shell` trait
next to it, and it crosses no new seams: a virtual shell is a string,
`docker://run/archlinux:latest?keep`, carried in the field a pane's program
already used. So the save file records it, a restore rejoins a container that
is still running (and never starts one), and this works without a line of
container code anywhere near it:

```sh
haven run -- new-window 'docker://run/alpine:latest?scratch'
```

### Scripting

```sh
haven run -- split-v
haven run -- send-keys 'cargo test\n'
haven run -t 2 -- select-window 3
```

Anything a key can do, `haven run` can do: both resolve to the same action,
so there is no second vocabulary for scripts. A synchronized tab is marked
`≡` on the bar, a tab that produced output while you were elsewhere is marked
`○`, and one that rang is marked `●` and painted — an invisible mode that multiplies every keystroke is a trap,
and a bell you did not see is the one worth seeing.

### Environment

A session outlives the connection it was made from, so `SSH_AUTH_SOCK`,
`DISPLAY` and the rest are refreshed from each attaching client and given to
panes opened after that. An existing shell cannot be told — its environment
was set at exec — so `eval "$(haven show-env)"` in that shell does it.

### Text

Bold, italic and bold-italic use the family's real cuts where it has them,
and a 12-degree shear where it does not. Colour glyphs — emoji, or any
`COLR`, `CBDT` or `sbix` face — render in their own colours rather than being
tinted with the foreground; only their alpha follows the cell, so `DIM` dims
an emoji without draining the colour out of it.

Ligatures are the one thing still missing here, and need a shaping engine.

### Mouse

A pane that asked for mouse reporting (1000/1002/1003, SGR or the original
encoding) gets it, in its own coordinates — so vim, mc and less respond to
clicks and drags. Everything else belongs to the multiplexer: the wheel reads
history, a click moves focus, and dragging the rule between two panes resizes
them. The pane under the *pointer* decides, not the focused one.

### Copy mode

`Ctrl+A` `[` puts a cursor on the pane's history. `Ctrl+A` `]` pastes the
newest buffer back, bracketed if the pane asked for that.

| Key | Does |
| --- | --- |
| `h` `j` `k` `l`, arrows | move; off the edge scrolls |
| `w` / `b` / `0` / `$` | word and line motion |
| `Ctrl+U` / `Ctrl+D`, `PageUp` / `PageDown` | half a screen, a screen |
| `g` / `G` | oldest row / back to the live screen |
| `v` or `Space` | start a selection; again to drop it |
| `Enter` or `y` | copy it and leave |
| `/` `?` then `Enter` | search forwards / backwards |
| `n` / `N` | repeat the search, `N` the other way |
| `q` or `Esc` | leave |

The selection is anchored in the pane's *history*, not in what is on screen,
so it survives the scrolling that making it needs. Copied text goes to a paste
buffer and to the system clipboard — `OSC 52` from `haven attach`, so it works
over ssh, and the desktop clipboard from `haven gpu`. Search folds case unless
the query has a capital in it. A line that wrapped is copied as one line.

### Command line

`Ctrl+A` `:` opens a command line on the status bar. Commands resolve to the
same actions the keys do, so nothing is reachable one way and not the other.

| Command | Does |
| --- | --- |
| `split-h [prog]` / `split-v [prog]` | split into rows / columns in the current pane's directory, optionally running `prog` |
| `choose-shell [verb]` | the shell menu, containers and all; `verb` is what to do with the pick, `new-window` by default |
| `container-mount <path>` (`mount`) | hand a host directory to container panes; `cwd` means the pane's own |
| `container-unmount <path>`, `container-mounts` | stop handing one over / say what they are |
| `command-palette` (`palette`) | every command, narrowed by typing |
| `switch-table <name>` | arm a key table |
| `new-window [prog]` (`new-tab`), `next`, `prev` | tabs |
| `new-session` (`new`) | a new session on this daemon, named `haven-<n>` |
| `scroll-up`, `scroll-down`, `scroll-end` | as the paging keys do |
| `copy-mode`, `paste-buffer` | as `[` and `]` do |
| `font-bigger` (`zoom-in`), `font-smaller`, `font-reset` | the size of the text in the window |
| `history` | this pane's commands, merged with the shell's history file |
| `grep <pattern>` | every matching line in every pane of the session |
| `last`, `last-pane` | back to the previous tab / pane |
| `rename-window <name>`, `rename-session <name>` | an empty name gives a tab back to its command |
| `command-prompt <text>` | open the command line with `text` on it |
| `select-window <n>` (`select-tab`) | go to tab *n* (numbered from 1) |
| `kill-pane`, `kill-window`, `zoom`, `detach` | as the keys do |
| `swap-pane [-L\|-R\|-U\|-D]` | exchange panes; no direction means the next one |
| `rotate [-D]` | move the panes round their places |
| `break-pane`, `join-pane <tab> [-h]` | move a pane out of / into this tab |
| `respawn-pane`, `next-layout` | restart the program / cycle the layout |
| `send-keys <text>` | type into the pane; `\n` `\t` `\e` `\xNN` are decoded |
| `synchronize-panes [on\|off]` | type into every pane at once |
| `display-panes`, `find-window <text>`, `detach-others` | as the keys do |
| `save-sessions`, `restore-sessions` | as `Ctrl+S` and `Ctrl+R` do |
| `even-horizontal`, `even-vertical`, `tiled` | layouts |

`Tab` completes as far as the candidates agree. A box above the bar lists what
matches as you type, with one entry highlighted. `Up` and `Down` walk the list
and put the highlighted entry on the command line, so what `Enter` runs is
always what is written there; typing again starts the list over. `Esc`
cancels, `Backspace` on an empty line leaves the mode.

### Tab names

A tab is named after the command its active pane is running, which needs the
shell to say what that command is — the `OSC 133;C` mark. fish reports it
natively; bash and zsh do once Haven's integration is installed, which is
automatic unless `--no-integrate` is passed. Without integration there is
nothing to report and the tab falls back to the window title, then to its
number.

The chooser shows the same thing per pane, with `…` while the command is
still running.

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
| **tab** | owns a tree of panes, and is named after the command running in its active pane. tmux calls this a *window*, and so do the protocol and the commands (`new-window`); the interface says *tab*, because the other meaning is taken. |
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
