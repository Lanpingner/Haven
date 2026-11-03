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
