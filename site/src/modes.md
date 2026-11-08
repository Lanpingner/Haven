# Run modes

One binary, one mux core, two frontends, two transports. The matrix is the whole
product surface.

| | **GPU frontend** | **TUI frontend** |
|---|---|---|
| **Attached** — the daemon owns the panes | `haven gpu` — the primary desktop mode. Detachable, survives client death. | `haven attach` — inside any terminal, over SSH, in a tty. The tmux-replacement mode. |
| **Standalone** — the mux runs in-process | *(planned)* | `haven tui` — a pure multiplexer with no GPU and no daemon. |

## Server plus a GPU window

```sh
haven gpu
```

Attaches a desktop window to the daemon, starting it if there is not one.
Closing the window leaves the session running; opening two windows gives two
viewers of the same panes, each with its own size policy and its own scroll
position.

## Server plus a TUI client

```sh
haven attach
haven ls
haven server stop
```

Draws the same session into whatever terminal you are already in. This is the
mode that replaces tmux, and it is a full client rather than a fallback — the
same frame model, the same input encoding, the same key table, rendered with
escape sequences by [`haven-vtwrite`](./architecture.md) instead of with
triangles.

## Standalone, no daemon

```sh
haven tui
```

Runs the whole multiplexer in-process: no socket, no background process,
nothing left behind when it exits.

It is the *mux*, not the whole interface. Splits, focus, resize, zoom, tabs,
layouts, swap, rotate, break and join, respawn, `send-keys`, synchronize and
rename all work from the same key table the attached modes use. What needs a
client — the status bar, the command line, the palette, menus (so the shell
and [container](./containers.md) chooser), copy mode and the session tree —
is not there, and neither is anything that needs a server: detach, save and
restore. Those keys do nothing rather than something surprising.

Which half you get is not a judgement about what matters; it is where the
code lives. The interface belongs to `haven-client`, and standalone has its
own small loop instead. Running `haven attach` against an autostarted daemon
costs nothing and has all of it, which is why this has not been worth
moving.

## Which session you get

`haven attach -t <id>` names one. Without it, the daemon joins the first
session that exists and creates one when there are none — which is right
when there is one session and a guess when there are several.

So when there are several, attaching **opens the session tree over the top**
and the guess becomes a default rather than a decision: pick a session, a tab
inside one, or `+ new session` at the bottom of the list to start somewhere
fresh. `Esc` keeps whatever you were given.

```sh
haven attach            # asks, when there is more than one session
haven attach --choose   # asks even when there is one: "+ new session" is on
                        # that list too
haven attach -t 2       # no question; you already said
```

It is the same tree that `Ctrl+A` `s` opens, for the same reason the command
line and the keys resolve to the same actions: a second picker that did
almost this would be a second place to fix.

## Headless

```sh
haven dump --cols 80 --rows 20 -- mc -u
haven screenshot --out mc.ppm -- mc -u
haven shell-info
```

`dump` prints a program's final screen as text; `screenshot` renders it through
the real GPU pipeline to an image, which is what keeps the renderer reviewable
on a machine with no display.

## The three levels, named carefully

The word *window* means two things in a GPU terminal, which tmux never had to
deal with. Haven's interface says **tab** for the thing that owns a pane tree,
and keeps tmux's `window` in the protocol and the command names, with
`new-tab` accepted everywhere `new-window` is.

| | |
|---|---|
| **desktop window** | A `haven gpu` process. A *viewer*, not part of a session — several can watch one, and closing one leaves it running. |
| **session** | Lives in the daemon and outlives every viewer. |
| **tab** | Owns a tree of panes, and is named after the command running in its active pane. |
| **pane** | One shell, one pty. |

So the session tree has three levels, not four: the desktop window is the thing
looking at it.

## What belongs to a client, and what to the session

A session owns the tabs, the panes and everything running in them. A *client*
owns where it is looking.

| | |
|---|---|
| The session | tabs, pane trees, what each pane is running, names, layouts |
| Each client | **which tab it is on**, which pane is focused in it, how far back it is reading, its own size |

So two clients on one session browse its tabs independently: select tab 2 in
the GPU window and the client on the other end of an SSH connection stays on
tab 1, with its own status bar marking its own tab. The same already went for
the scrollback — [the offset is per client](./keys.md#scrollback) — and the
current tab follows the same rule for the same reason.

tmux does not work this way: clients attached to one session share its current
window, and getting two views means a second session in the same group. Haven
makes it a property of the reader instead, because that is what people
attaching twice turn out to want.

A client whose tab is closed by somebody else falls back to a live one rather
than being left pointing at nothing. What a *pane* shows is still shared, of
course: that is one program with one screen, and two clients watching it see
the same thing.
