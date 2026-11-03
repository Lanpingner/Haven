# The command line

`Ctrl+A` `:` opens a command line on the status bar. Commands resolve to the
same actions the [keys](./keys.md) do, so nothing is reachable one way and not
the other.

| Command | Does |
|---|---|
| `split-h` / `split-v` | split into rows / columns |
| `new-window` (`new-tab`), `next`, `prev` | tabs |
| `new-session` (`new`) | a new session on this daemon, named `haven-<n>` |
| `scroll-up`, `scroll-down`, `scroll-end` | as the paging keys do |
| `select-window <n>` (`select-tab`) | go to tab *n*, numbered from 1 |
| `copy-mode` (`copy`), `paste-buffer` | enter copy mode / paste the newest buffer |
| `choose` | the session tree |
| `kill-pane`, `zoom`, `detach` | as the keys do |
| `even-horizontal`, `even-vertical`, `tiled` | layouts |

Aliases are offered by completion deliberately: someone who types `vsplit`
should see it in the list rather than be told after the fact that it does not
exist.

## Completion

`Tab` completes as far as the candidates agree. A box above the bar lists what
matches as you type, with one entry highlighted.

`Up` and `Down` walk the list and put the highlighted entry on the command line,
so **what `Enter` runs is always what is written there** — typing again starts
the list over. `Esc` cancels, and `Backspace` on an empty line leaves the mode.

## The CLI

The same vocabulary from outside a session:

```sh
haven gpu              # attach a GPU window, starting the daemon if needed
haven attach           # attach from inside another terminal
haven tui              # standalone: the whole mux, no daemon
haven ls               # sessions, pane counts, and the daemon's build stamp
haven kill-session     # end one
haven server start|stop|run
haven dump             # print a program's final screen
haven screenshot       # render it to an image
haven shell-info       # what Haven detected about your shell
haven help
```

`haven ls` reports the daemon's build stamp on purpose. The daemon holds the
emulator and outlives every client, so rebuilding after a parser fix changes
nothing until the daemon restarts — and that is indistinguishable from a fix
that did not work.
