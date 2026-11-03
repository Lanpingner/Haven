# The command line

`Ctrl+A` `:` opens a command line on the status bar. Commands resolve to the
same actions the [keys](./keys.md) do, so nothing is reachable one way and not
the other.

| Command | Does |
|---|---|
| `split-h [prog]` / `split-v [prog]` | split into rows / columns, optionally running `prog` |
| `new-window [prog]` (`new-tab`), `next`, `prev`, `last` | tabs |
| `new-session` (`new`) | a new session on this daemon, named `haven-<n>` |
| `select-window <n>` (`select-tab`) | go to tab *n*, numbered from 1 |
| `rename-window <name>`, `rename-session <name>` | an empty name gives a tab back to its command |
| `focus-left\|right\|up\|down`, `resize-left\|…` | move focus / resize |
| `last-pane`, `swap-pane [-L\|-R\|-U\|-D]` | the previous pane / exchange two |
| `rotate [-D]` | move the panes round their places |
| `break-pane`, `join-pane <tab> [-h]` | move a pane out of / into this tab |
| `kill-pane`, `kill-window`, `zoom`, `detach` | as the keys do |
| `respawn-pane [prog]`, `next-layout` | restart the program / cycle the layout |
| `even-horizontal`, `even-vertical`, `tiled` | layouts |
| `scroll-up`, `scroll-down`, `scroll-top`, `scroll-end` | as the paging keys do |
| `copy-mode` (`copy`), `paste-buffer` | enter copy mode / paste the newest buffer |
| `choose` | the session tree |
| `choose-shell` | the shell menu `Alt+C` opens |
| `command-palette` (`palette`) | every command, narrowed by typing |
| `switch-table <name>` | arm a [key table](./keys.md#key-tables) |
| `display-panes`, `find-window <text>` | number the panes / go to a tab by name |
| `send-keys <text>` | type into the pane; `\n` `\t` `\e` `\xNN` are decoded |
| `synchronize-panes [on\|off]` | type into every pane of the tab at once |
| `detach-others` | detach every other client from this session |
| `save-sessions`, `restore-sessions` | [save and restore](./sessions.md) |
| `command-prompt [text]` | open this command line with `text` already on it |
| `send-prefix` | send a literal prefix key to the pane |

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
haven run -- <command> # run one command against a session, without attaching
haven save | restore   # write sessions to disk, or build them back
haven show-env         # the session's environment, as `export` lines
haven server start|stop|run
haven dump             # print a program's final screen
haven screenshot       # render it to an image
haven shell-info       # what Haven detected about your shell
haven help
```

`haven run` is the whole of control mode for now, and the thing that makes
Haven scriptable at all:

```sh
haven run -- split-v
haven run -- send-keys 'cargo test\n'
haven run -t 2 -- select-window 3
```

It takes the same vocabulary a key binding resolves to, so there is no second
language for scripts. Neither `save` nor `restore` attaches first: attaching
with no session named creates one, which on a fresh daemon would make an empty
session and then skip restoring the saved session of that name.

`haven ls` reports the daemon's build stamp on purpose. The daemon holds the
emulator and outlives every client, so rebuilding after a parser fix changes
nothing until the daemon restarts — and that is indistinguishable from a fix
that did not work.
