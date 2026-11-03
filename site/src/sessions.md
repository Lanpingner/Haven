# Sessions that outlive the machine

A session already outlives every client — that is what the daemon is for. What
it did not outlive was a reboot.

## Save and restore

`Ctrl+A` `Ctrl+S` saves every session; `Ctrl+A` `Ctrl+R` builds back what is in
the file. From a script, `haven save` and `haven restore`, where restore starts
the daemon if there is not one — which is the state a machine is in after a
reboot, and exactly when a restore is wanted.

What comes back is the **shape of the work**:

- sessions, with their names
- tabs, with the names you typed
- the pane tree, with its proportions
- each pane's working directory

What does not come back is the processes. They are gone; a restored pane is a
fresh shell in the right place.

The directory is read from `/proc/<pid>/cwd`, not from `OSC 7`. The shell
integration would also answer, but only once it is installed and only after a
prompt has been drawn. The kernel answers whatever is running and whether or
not anything is installed, which is the case a save has to cover.

### Bringing a program back

A program is started again only if the config names it, and it is typed at the
restored shell rather than `exec`'d — so when you quit it you are left with a
shell, which is what a pane should be.

```toml
restore_programs = ["vim", "nvim", "less", "man", "tail", "htop"]
autosave_seconds = 900     # 0 turns it off
```

The allowlist is not caution for its own sake. A save file is a file on disk
that decides what a machine runs when it comes back; replaying whatever
happened to be in the foreground is how a restore becomes an exploit. The
default list is the one tmux-resurrect ships, for the same reason: editors and
pagers, which re-open a file and do nothing else.

The autosave matters more than it looks. Without it a save file is only as good
as the last time someone remembered, and the times you want one are the times
nobody did. A daemon holding no sessions writes nothing, so an empty daemon
cannot quietly destroy the file from before the reboot.

### Running it twice is safe

A restore matches sessions by name and skips the ones already there. It is
therefore safe from a login script that may fire more than once, and safe to
run while you are working — it will not throw away what is live.

### The file

`$XDG_DATA_HOME/haven/sessions.toml`, written atomically, and TOML you can read
and edit:

```toml
version = 1
saved_unix = 1793729337

[[session]]
name = "haven-1"
active_window = 1

[[session.window]]
active_pane = 2

[session.window.layout]
kind = "split"
columns = true
# …children, each with a weight…

[[session.window.pane]]
cwd = "/Projects/HavenTerminal"

[[session.window.pane]]
cwd = "/etc"
```

The save types are their own set, not the mux's. A file written by one version
is read by another and must not change shape every time the pane tree does.
Leaves refer to panes **by position**, so nothing in the file names a pane id
that will not exist again.

## Choosing a shell

`Alt+C` — no prefix — opens a new pane and asks what to run in it. Arrows or
`j`/`k` to move, a digit to take one outright, `Enter` for the highlighted one,
`Esc` to drop it.

The list comes from the **daemon's** machine, because that is where the pane is
spawned; a client's idea of where `fish` lives means nothing once a session is
on another host. It is ordered rather than filtered: your current shell, then
the ones Haven has integration for, then the rest of `/etc/shells` — which is
also where `git-shell` and the systemd fallback live. Dropping those would be
guessing at a list the system owns, so they are last instead of absent.

The menu itself knows nothing about shells. It holds labels paired with command
strings and asks the key filter to resolve them, exactly as a typed line is
resolved. So a menu cannot offer anything the command line could not do, and
the next menu is data rather than code.

## The environment

A session outlives the connection it was made from, so `SSH_AUTH_SOCK`,
`DISPLAY` and the rest are refreshed from each attaching client and handed to
panes opened after that. The values are held shared, so a pane spawned an hour
after a reattach reads the current ones rather than the ones it was born with.

A shell that is already running cannot be told anything — its environment was
set at `exec`. For that there is:

```sh
eval "$(haven show-env)"
```
