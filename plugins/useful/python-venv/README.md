# python-venv

Which Python virtual environment the focused pane is probably in.

```
~/Projects/haven/.venv/lib/python3.12   →   venv:haven
~/.virtualenvs/haven/bin                →   venv:haven
~/.conda/envs/scratch                   →   venv:scratch
~/Projects/haven                        →   (nothing)
```

The `venv:` is dim and the name plain. Nothing is drawn when the path
gives no sign of an environment: a segment that is always there and
usually empty is clutter.

## What it needs

No capabilities. The pane's `cwd` comes from `host::panes()`, which
needs no grant.

## What it cannot do

**It cannot actually tell whether an environment is active.** The real
answer is `$VIRTUAL_ENV`, and a plugin has no environment to read — the
sandbox has none, and the variable belongs to the shell in the pane
rather than to Haven. So this infers from the shape of the working
directory, and the limits of that are worth knowing before you trust it:

- Activating an environment and then working in a directory that does
  not name it shows **nothing**. That is the normal way to use a virtual
  environment, so a blank segment is not evidence that none is active.
- A path that names an environment shows it whether or not it is
  activated. Reading `~/Projects/haven/.venv/lib/...` looks exactly like
  working in it.
- The markers are `.venv`, `venv`, `virtualenv` and `.conda`, with
  `.virtualenvs`, `virtualenvs` and `envs` treated as directories that
  hold environments by name. A plain `env` is not a marker: too many
  directories are called that for other reasons.
- Nothing is verified on disk, because a plugin has no filesystem. A
  directory called `venv` with nothing in it reads the same as a real
  one.

What it will not do is name the *wrong* environment: every case it
cannot work out, it stays quiet.
