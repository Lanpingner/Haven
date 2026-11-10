# disk-free

Free space on the filesystem holding the focused pane's working
directory.

```text
207G 54%      room for anything
9.4G 96%      bold, because the next build is not going to fit
df ?          nobody has said where the pane is
df —          a filesystem with no size worth printing
```

## What it shows

The free space, and after it the percentage in use, dim.

The absolute figure leads because it is the one you can act on. "92%"
on a four-terabyte disk still leaves three hundred gigabytes, which is
room for anything you were about to do; "92%" on a 16G root partition
means the next `cargo build` fails. The percentage cannot be acted on
without knowing the size, so on its own it is not a number, it is a
mood. The free figure answers the only question a status bar is being
asked here — *will this fit* — and the percentage is kept because it is
the better one for noticing that something has been growing.

It goes **bold** at 90% used. That is a rule of thumb rather than a
threshold anybody chose, and bold rather than red for the same reason:
a hard-coded colour is unreadable on half the themes people have.

Three cases that are not a number:

- `df ?` — there is no focused pane, or Haven does not know its working
  directory. `cwd` arrives from `OSC 7`, which means the shell has to be
  emitting it: `haven shell install` sets that up. Rather than guess, or
  silently report on `/` and be wrong in a way nobody would notice, the
  segment says it does not know. Four characters is the right length for
  that admission.
- `df —` — `df` answered, and the filesystem it answered about has no
  meaningful size: an `autofs` placeholder (`map auto_home` on macOS),
  some overlay and union mounts, anything it prints `0` or `-` for. There
  *is* a filesystem under that directory, so saying nothing would look
  like a bug; there is no figure, so printing `0G 100%` would be a lie.
- the host's own refusal, in full, when the `df` capability was not
  granted.

## What it runs

`df -P -h <cwd>`, on `started`, on `tick`, and on the two events that
change which filesystem is in question: `pane-focus` and `cwd-changed`.

The working directory comes from `host::panes()`, which needs no
capability — a pane's title and directory are not secrets, its *contents*
are, which is why `pane-text` is the one behind `read-pane-content`.

Two notes on the shape of this:

**`host::run` has no working-directory argument.** The directory has to
be passed to `df` as an argument rather than by running `df` in it. For
`df` that is no loss at all, since taking a path is what it does. It
would be a real one for a program that only ever reports on where it was
started, and there is no way for a plugin to work around it.

**The `exec` allowlist matches `argv[0]` only.** Granting `df` grants
running `df` with any arguments whatsoever, so the whole safety of the
grant is that `df` is a program that reports on filesystems and cannot be
talked into reading a file. That is the property to look for. A plugin
that asked for `sh` in order to run `df` would have been granted the
entire machine, and neither the host nor the user would have any way to
tell the difference.

## Limits

- **`-P -h` is a compromise between two `df`s.** GNU `df` honours both
  and prints `207G`. BSD's gives `-P` precedence and prints 1K blocks
  instead, so a field that is all digits is converted here, in integer
  arithmetic, with the same "one decimal below ten" rule `df -h` uses.
  Where `df` has already formatted the figure its own rounding is kept,
  because that is the one the user sees everywhere else.
- **The output line is found by the `%` field**, not by counting columns
  from the left, because a device name can contain a space — `map
  auto_home` again — and so can a mount point.
- **One filesystem, the focused pane's.** Not the busiest, not `/`, not
  every mount. A status segment reporting on somewhere you are not is
  worse than no segment.
- **No configuration**, so 90% is 90%. Haven has nowhere for a user to
  set a threshold yet.
- **`df` can block.** On a dead NFS mount it hangs until the host kills
  it after five seconds. That is exactly why the reading is taken on an
  event and cached, and why `render` never shells out: the worst that
  happens is a stale figure for one tick, rather than a terminal that
  stops drawing.
