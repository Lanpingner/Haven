# Keys and commands

`Ctrl+A` is the prefix, unless the config says otherwise. Every key resolves to
the same declarative action the [command line](./commands.md) does, so nothing
is reachable one way and not the other — and the table itself is
[data from the config](./themes.md#keys), so a binding is a line in a file
rather than a code change.

## Prefix keys

`Ctrl+A` then:

| Key | Action |
|---|---|
| `%` / `"` | split into columns / rows, in the directory you are in |
| `h` `j` `k` `l` or arrows | move focus |
| `H` `J` `K` `L` | resize |
| `z` | zoom the active *pane* — the layout, not the text |
| `x` | close the pane |
| `c` / `n` / `p` / `1`-`9` | new tab / next / previous / select |
| `C` | new session, and move to it |
| `w` / `;` | the tab / pane you were on before |
| `,` / `$` | rename this tab / this session |
| `PageUp` / `PageDown` | read back through the pane's history |
| `Home` / `End` | the oldest row / back to the live screen |
| `e` / `E` / `t` / `Space` | even-horizontal / even-vertical / tiled / next layout |
| `o` | swap this pane with the next |
| `{` / `}` | rotate the panes round their places |
| `!` / `&` | break this pane into its own tab / kill the whole tab |
| `r` | respawn the pane's program |
| `S` | synchronize: type into every pane of the tab at once |
| `q` | number the panes until the next key |
| `f` | find a tab by name |
| `/` | [grep every pane](./finding.md) in the session |
| `D` | detach every other client |
| `Ctrl+S` / `Ctrl+R` | [save](./sessions.md) every session / build back the saved ones |
| `P` | the command palette |
| `g` / `G` | new tab / new pane, [choosing what runs in it](./sessions.md#choosing-a-shell) — a shell, or [another distro](./containers.md) |
| `[` / `]` | enter copy mode / paste the newest buffer |
| `d` | detach |
| `:` | open the command line |
| `s` | the session tree |
| `Ctrl+A` | send a literal `Ctrl+A` |

`Shift` plus an arrow resizes, the same as `H` `J` `K` `L`. `PageUp` moves
`PAGE_ROWS` — nearly a screenful — rather than the pane's own height, because
the key table has no way to know how tall the pane is; the daemon clamps it to
the history that exists.

## Without the prefix

| Key | Action |
|---|---|
| `Alt+C` | open a tab and [choose what runs in it](./sessions.md#choosing-a-shell): a local shell or [a container](./containers.md) |
| `Alt+P` | the command palette |
| `Alt+H` | [this pane's history](./finding.md), with help beside it |
| `Ctrl+=` / `Ctrl+-` | bigger / smaller text, in the GPU window |
| `Ctrl+0` | back to the size the config asks for |

Those two are the defaults, and the *root table* they live in is the same
mechanism as everything else: `root` in the config binds keys that act with
no prefix at all.

## The command palette

`Alt+P`, or the prefix and `P`, lists every command and narrows as you type.
Entries that *start* with what you typed come first — a plain substring match
puts `display-panes` above `split-v` for "spl", which is correct and useless.

A command that needs an argument opens the [command line](./commands.md) with
the verb already on it rather than running a bare verb that would do nothing
or, for `rename-window`, the wrong thing. Which commands those are is decided
by the parser, not by a list someone has to remember to update.

## Key tables

`root` is consulted with nothing armed and `prefix` after the prefix key.
Those two always exist. Any other name is a table of your own, and the
difference that matters is that **a table of your own stays armed**:

```toml
[keys]
bind = { "R" = "switch-table resize" }

[keys.table.resize]
h = "resize-left"
l = "resize-right"
Escape = "switch-table root"
```

One `Ctrl+A` `R`, then as many `h`s as you like. That is the modal part —
`prefix` is one-shot, as it has always been, and everything else is a table
that binds `switch-table` to get into and out of it. The bar shows the armed
table's name, so a mode of your own says which one it is.

`Escape` always leaves a sticky table even if it forgot to bind a way out, and
`switch-table` refuses a name no table answers to: arming one would swallow
every key and answer none of them.

**A root binding fires whatever is armed.** Pressing the prefix and then
changing your mind must not disable the keys that work without one, so an
armed table falls through to `root` when it has no binding of its own. A key
that matches nothing at all says so on the bar rather than vanishing — a
keystroke that does nothing and reports nothing looks exactly like a broken
binding, which is how this went unnoticed.

## Bindings are data

The built-in table is the default *value* of the config's table, not a `match`
in the source. A configured binding and a built-in one therefore take one path;
the alternative is a table config can only half override, which is how bindings
end up with two behaviours depending on which half you hit.

That forced the command vocabulary to cover every bound key. There had been no
names for moving focus, for resizing, or for sending a literal prefix — so the
claim that anything a binding can do a typed command can do was not true until
the table became data. A test now walks the default table and parses every
command in it, because a typo in data is a key that silently does nothing
rather than a compile error.

The modal key stack and a command palette are still part of the unfinished half
of M4.

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

## The mouse

A pane that asked for mouse reporting — 1000, 1002 or 1003, in the SGR encoding
or the original one — gets it, in its own coordinates, so `vim`, `mc` and
`less` respond to clicks and drags. Everything else belongs to the
multiplexer: the wheel reads history, a click moves focus, and dragging the
rule between two panes resizes them. **The pane under the pointer decides**,
not the focused one — reading or clicking in one pane while typing in another
is the ordinary case.

A click on a pane that wants the mouse still moves focus first, because a
program holding the mouse has no way to tell the mux it should be the one typed
at. `mouse = false` in the config hands the mouse back to whatever terminal
`haven attach` is running in, and really hands it back: the host is told to
stop reporting, not merely ignored.

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

## Text

Bold, italic and bold-italic use the family's real cuts where it has them and
a 12-degree shear where it does not. A styled face is consulted only for
characters the primary family covers — falling through to it for CJK would be
asking a Latin italic for a glyph it has never had — so a fallback glyph keeps
its own face and is slanted instead.

Colour glyphs keep their own colours. The atlas is RGBA and a coverage mask is
stored as white with the coverage as alpha, so one texture serves both and the
shader's only question is whether to tint. Only a colour glyph's alpha follows
the cell, which is what makes `DIM` dim an emoji without draining the colour
out of it.

## Status bar

The bottom row shows the session name, its tabs with the active one painted,
the pane count, how many clients are attached when more than one, and how long
the session has been open. A tab carries marks for what you would otherwise
have to go and look for:

| Mark | Means |
|---|---|
| `▣` | the tab has a zoomed pane |
| `≡` | the tab is synchronized — what you type goes to every pane in it |
| `○` | a pane produced output while you were elsewhere |
| `●` | a pane rang while you were elsewhere; the tab is painted too |

Both `○` and `●` clear when you arrive, which is what makes them mean "since
you last looked". Activity gets a mark but not a colour on purpose: it is
information, not a summons, and painting every busy tab is noise.

The right-hand end shows what the client is doing when it is doing something —
`PREFIX` while the prefix is armed, `SCROLL 20/38` while reading history,
`COPY` in copy mode.

The client keeps that row for itself and never offers it to the daemon, so no
pane is ever laid out underneath it.
