# git-wip

How much uncommitted work is sitting in the working tree, as a line
count: `+127-14`.

## What it shows

| Shown | Means |
|---|---|
| `+127-14` | 127 lines added and 14 removed since the last commit, staged and unstaged together |
| **`+312-88`** | bold, because more than 200 lines have changed |
| *nothing* | a clean tree, a directory that is not a repository, or no pane directory at all |

The point is not curiosity about the diff. It is to notice that a day's
work is uncommitted *before* the laptop sleeps, the branch is switched,
or `git checkout .` is typed in the wrong pane.

## Staged changes count

The command is `git diff --shortstat HEAD`, which compares the working
tree with the last commit and so counts staged and unstaged changes
together. That is a deliberate choice: "staged" is a step towards a
commit, not a commit, and work in the index is exactly as lost as work
in the file if the tree is thrown away. Counting only unstaged changes
would make the number *fall* when you `git add` — an alarm that
switches itself off.

In a repository with no commits yet there is no `HEAD`, so the fallback
diffs the index against git's empty tree object,
`4b825dc642cb6eb9a060e54bf8d69288fbee4904`. A fresh `git init` holding
a morning's work is precisely the case this plugin exists for.

## The threshold

Bold past 200 changed lines, added and removed together. The number is
a guess, but a considered one: a couple of hundred lines is about where
a diff stops being one change you are holding in your head and starts
being a morning you would be sorry to lose. Nothing lets a user
configure it yet — Haven has no plugin configuration (see
[`../../README.md`](../../README.md)) — so it is a default, not a
setting.

## What it runs

```console
$ git -C <cwd> diff --shortstat HEAD
$ git -C <cwd> diff --shortstat --cached 4b825dc642cb6eb9a060e54bf8d69288fbee4904   # no commits yet
```

`--shortstat` makes git count lines without formatting a single hunk,
which matters because the host kills a program after five seconds.

Neither runs when Haven draws. `render` formats a number that is
already known; the diff runs from `on_event` — the directory changed, a
command finished (which is where `git commit`, `git add` and `git
stash` land), the focus moved, or the tick came round. Haven redraws
many times a second and a `git diff` per frame would be indefensible.

## `git -C`, and why it matters

`host::run` takes an argv and has no working-directory argument, so the
program runs in the **daemon's** cwd — wherever Haven was started. A
plugin that ran a bare `git diff` would report Haven's own repository in
every pane. The directory here comes from the focused pane's `cwd`, as
reported by `OSC 7`, and is passed explicitly with `git -C`.

## Limits

- **Untracked files do not count.** `git diff` does not see them, and
  counting their lines would mean a second command that walks and reads
  every new file. A new file you have `git add`ed does count.
  [`gitline`](../gitline/README.md) shows a `*` for untracked files, so
  between the two segments nothing is silent.
- Binary files contribute no lines, only a "files changed" that this
  does not show.
- The cwd comes from `OSC 7`, so a shell without Haven's shell
  integration reports no directory and this shows nothing.
- It describes the focused pane only, and is stale between refreshes by
  design.
- It needs `exec: [git]`. Without the grant it says so on the bar
  rather than drawing nothing.
