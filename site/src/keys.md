# Keys and commands

`Ctrl+A` is the prefix. Every key resolves to the same declarative action the
[command line](./commands.md) does, so nothing is reachable one way and not the
other.

## Prefix keys

`Ctrl+A` then:

| Key | Action |
|---|---|
| `%` / `"` | split into columns / rows |
| `h` `j` `k` `l` or arrows | move focus |
| `H` `J` `K` `L` | resize |
| `z` | zoom the active pane |
| `x` | close the pane |
| `c` / `n` / `p` / `1`-`9` | new tab / next / previous / select |
| `C` | new session, and move to it |
| `PageUp` / `PageDown` | read back through the pane's history |
| `Home` / `End` | the oldest row / back to the live screen |
| `e` / `E` / `t` | even-horizontal / even-vertical / tiled layout |
| `d` | detach |
| `:` | open the command line |
| `s` | the session tree |
| `Ctrl+A` | send a literal `Ctrl+A` |

The prefix table is fixed for now; configurable bindings and a modal key stack
are part of the unfinished half of M4.

## Scrollback

Scrolling works with the mouse wheel in `haven gpu`, over whichever pane the
pointer is on — not necessarily the focused one. While a pane is scrolled the
bar says so, its cursor is not drawn, and typing returns to the live screen.

**The offset is per client**, so two people on one session read at their own
pace. The alternate screen keeps no history, so a full-screen program scrolls
nothing.

## The session tree

`Ctrl+A` `s` opens a chooser over the panes, listing every session and, for the
one you are attached to, its tabs and the panes inside them. It opens on the
pane you are in.

| Key | Does |
|---|---|
| `j` / `k` or arrows | move (wraps) |
| type anything | filter the list |
| `Backspace` | undo a filter character |
| `Enter` | go there — attaching to another session if that is what is selected |
| `Esc` | close |

Only the attached session can show its tabs and panes; for the others the daemon
reports counts alone, so that is what is listed.

## Tab names

A tab is named after the command its active pane is running, which needs the
shell to say what that command is — the `OSC 133;C` mark. fish reports it
natively; bash and zsh do once Haven's integration is installed, which is
automatic unless `--no-integrate` is passed. Without integration there is
nothing to report and the tab falls back to the window title, then to its
number.

Getting the command out of each shell differs, and bash is the awkward one: it
exposes the command only in a `DEBUG` trap or in history. The trap is the wrong
place, because it also fires for every `PROMPT_COMMAND` function and names the
tab after the prompt — so `PS0` plus `history 1` is used instead, which expands
exactly once per interactive command line.

## Status bar

The bottom row shows the session name, its tabs with the active one marked (`*`,
plus `Z` when zoomed), the pane count, how many clients are attached when more
than one, and how long the session has been open.

The client keeps that row for itself and never offers it to the daemon, so no
pane is ever laid out underneath it.
