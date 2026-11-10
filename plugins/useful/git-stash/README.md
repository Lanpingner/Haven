# git-stash

How many stashes are hiding: `⚑2`, and nothing at all when there are
none.

## What it shows

| Shown | Means |
|---|---|
| `⚑2` | two entries on the stash |
| *nothing* | an empty stash, a directory that is not a repository, or no pane directory |

A stash you remember is a tool; a stash you have forgotten is an
afternoon you will lose in three weeks, and the only reason it is
forgotten is that nothing ever mentions it. So this mentions it, and is
silent the rest of the time — which is most of the time, and is the
state that needs no characters on a shared status bar.

`⚑` is a flag: something put aside and marked to come back to. It is
drawn in the theme's own colours, not a hard-coded yellow.

## Count, not age

Showing the age of the oldest stash was the tempting alternative —
`⚑3w` says more about whether you have forgotten it than `⚑1` does — and
it is not what this shows, for two reasons.

The count is the number that changes behaviour. One stash is something
you are in the middle of; four is a drawer nobody has opened. An age
tells you that something is old without telling you how much is in
there, and the thing you do about it (`git stash list`) is the same
either way.

The age is also dearer. It needs `git log -g` with a format, a clock,
and the plugin doing date arithmetic by hand — against one
`rev-list --count` that git answers out of the reflog. Cheap and
actionable beat dear and evocative on a status bar.

## What it runs

One command:

```console
$ git -C <cwd> rev-list --count --walk-reflogs refs/stash
```

`git stash list` is the obvious command and the wrong one: it is
porcelain, so it walks every entry and formats a line for each, only
for a plugin to throw the lines away and count them. The stash *is* a
reflog, and this reads its length.

When nothing has ever been stashed, `refs/stash` does not exist and the
command fails — which is the answer "none", not an error. The same
failure covers "this is not a repository", so one command answers all
three cases.

It does not run when Haven draws. `render` formats a number that is
already known; the count is taken in `on_event` — the directory
changed, a command finished (which is where `git stash` itself lands),
the focus moved, or the tick came round.

## `git -C`, and why it matters

`host::run` takes an argv and has no working-directory argument, so the
program runs in the **daemon's** cwd — wherever Haven was started. A
plugin that ran a bare `git rev-list` would be reporting Haven's own
stash in every pane, which is worse than useless. The directory here
comes from the focused pane's `cwd`, as reported by `OSC 7`, and is
passed explicitly with `git -C`.

## Limits

- The count is of the stash reflog, so a stash dropped outside Haven is
  noticed on the next refresh, not at once.
- `git stash` run in a *different* program shows up on the tick rather
  than immediately; there is no event for it.
- The cwd comes from `OSC 7`, so a shell without Haven's shell
  integration reports no directory and this shows nothing.
- It describes the focused pane only. A stash in another repository in
  another pane is that pane's business.
- It needs `exec: [git]`. Without the grant it says so on the bar
  rather than drawing nothing.
