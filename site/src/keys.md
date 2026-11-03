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
| `[` / `]` | enter copy mode / paste the newest buffer |
| `d` | detach |
| `:` | open the command line |
| `s` | the session tree |
| `Ctrl+A` | send a literal `Ctrl+A` |

`Shift` plus an arrow resizes, the same as `H` `J` `K` `L`. `PageUp` moves
`PAGE_ROWS` — nearly a screenful — rather than the pane's own height, because
the key table has no way to know how tall the pane is; the daemon clamps it to
the history that exists.

The prefix table is fixed for now; configurable bindings and a modal key stack
are part of the unfinished half of M4.

## Copy mode

`Ctrl+A` `[` goes up into the history and `]` brings text back, which is how
tmux spells it. The keys inside are vi's:

| Key | Does |
|---|---|
| `h` `j` `k` `l` or arrows | move the cursor |
| `0` / `$` | start / end of the line |
| `w` / `b` | next / previous word |
| `Ctrl+U` / `Ctrl+D` | half a page up / down |
| `PageUp` / `PageDown` | a page |
| `g` / `G` | the oldest row / back to the live screen |
| `/` / `?` | search forward / backward |
| `n` / `N` | the next match / the previous one |
| `v` or `Space` | start a selection |
| `y` or `Enter` | copy it and leave |
| `q` or `Esc` | leave |

The split of work is the interesting part. **The mode is the client's**, like
the session tree, because it needs the view the frontend is drawing.
**Extraction and search are the daemon's**, because only the daemon has the
history to extract from or search.

The coordinates differ for the same reason: the cursor is in view coordinates,
because that is where it is drawn, while a selection's anchor is in history
coordinates — selecting more than a screenful means scrolling, and a
view-relative anchor would slide out from under the selection as soon as it
did.

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
