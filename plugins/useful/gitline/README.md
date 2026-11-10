# gitline

The branch and its state, in the status bar: `main`, `main*`,
`feat/x* ↑3↓1`.

## What it shows

| Shown | Means |
|---|---|
| `main` | the current branch |
| `9f86d08` | `HEAD` is detached, so the short sha, which is the thing you can act on |
| `*` | the tree is dirty — something tracked is modified or staged, or something is untracked. Bold, because it is the part people look for |
| `↑3` | three commits ahead of the upstream |
| `↓1` | one commit behind it |
| *nothing* | the directory is not in a repository, or no pane has reported one |

No arrows means level with the upstream *or* no upstream configured.
Both are ordinary; neither is an error worth a character on a shared
status bar. Line counts are not here on purpose — that is
[`git-wip`](../git-wip/README.md), and two segments both claiming
`+2-1` would be unreadable.

## What it runs

Four commands, all plumbing rather than porcelain, because the host
kills a program after five seconds and `git` on a large working tree is
not fast:

```console
$ git -C <cwd> rev-parse --git-dir                                  # a repository at all?
$ git -C <cwd> symbolic-ref --quiet --short HEAD                    # the branch
$ git -C <cwd> rev-parse --short HEAD                               # only when detached
$ git -C <cwd> status --porcelain                                   # dirty?
$ git -C <cwd> rev-list --count --left-right @{upstream}...HEAD     # ahead and behind
```

`symbolic-ref` rather than `rev-parse --abbrev-ref HEAD`, because it
still answers in a repository with no commits yet, where `rev-parse
HEAD` fails outright. A fresh `git init` reads `main`, not an error.

They do **not** run when Haven draws. `render` formats a value that is
already known; the commands run from `on_event`, on the directory
changing, a command finishing, the focus moving, or the tick. Haven
redraws far more often than a branch changes, and a plugin that shelled
out per frame would be several processes a second.

## `git -C`, and why it matters

`host::run` takes an argv and has no working-directory argument, so the
program runs in the **daemon's** cwd — wherever the user happened to
start Haven. A plugin that ran a bare `git status` would therefore
report Haven's own repository in every pane, confidently and wrongly.
The directory here comes from the focused pane's `cwd`, as reported by
`OSC 7`, and is passed explicitly with `git -C`.

## Limits

- The cwd comes from `OSC 7`, so a shell without Haven's shell
  integration reports no directory and this shows nothing.
- It describes the **focused** pane, not each pane. There is one status
  bar.
- Between refreshes it is stale by design; a commit made in another
  program shows up on the next tick.
- `status --porcelain` still stats every tracked file. In a very large
  working tree with a cold cache that is the slow part, and the five
  second limit applies.
- It needs `exec: [git]`. Without the grant it says so on the bar
  rather than drawing nothing, because a segment that is blank because
  of a missing permission is one nobody can diagnose.
