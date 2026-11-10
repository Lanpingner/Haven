# load-average

The machine's one-minute load average, in four characters.

```text
1.41      the machine is fine
9.87      bold, because this machine has four cores
```

## What it shows

One figure: the one-minute load average, exactly as `uptime` rounded it.
The five- and fifteen-minute figures are not shown. They are the ones you
want when you are investigating something, and at that point you are
running `uptime` yourself; the status bar's job is to tell you *now*, so
one number is the right number.

The figure is **bold** when it is greater than the number of cores,
because a load of 8 means nothing by itself. On an eight-core machine it
is fully busy; on a sixteen-core machine it is half idle; on a two-core
laptop everything you type is going to be slow. Bold rather than red: the
threshold is a rule of thumb, not an alarm, and a hard-coded colour is
unreadable on half the themes people have.

Before the first reading, and for the moment between loading and the
first event, it draws a dim `load …`.

## What it runs

| Program | When | Why |
|---|---|---|
| `uptime` | on `started`, then on every `tick` | the figures |
| `nproc` | once, on `started` | the core count to judge them against |

Two capabilities for one segment is worth justifying. `nproc` earns its
place because without it the plugin can only show a number, and a number
with no threshold does not answer the question the segment exists to
answer. It is also about as small as a capability gets: it prints an
integer, and there is no argument you could pass it that would make it
reveal anything about the machine that `uptime` has not already said.

The cost of granting it is not nothing, though, and this is the thing
worth understanding about Haven's `exec` capability: **the allowlist
matches `argv[0]` and nothing else.** A plugin granted a program may run
it with any arguments at all, as often as it likes. So the security of
the grant is entirely the security of the program. `uptime` and `nproc`
are safe to grant because there is no interesting thing they can be asked
to do. `cat` would have been catastrophic to grant — "it only reads
`/proc/loadavg`" is not something the host can check or the user can
verify, and `cat /home/you/.ssh/id_ed25519` is the same capability. The
same goes for `sh`, `head`, `awk`, `python` and every other tool that
takes a program or a path and does what it is told. When you write a
plugin, ask for the program that does the one thing.

## Limits

- **Linux and macOS.** Both end their `uptime` line with `load average:
  1.41, 3.62, 3.13` (Linux) or `load averages: 1.41 3.62 3.13` (macOS),
  and the parser finds the words and takes the first number after the
  colon rather than counting fields, so the differing prefixes — `up
  22:30, 2 users` against `up 3 days, 2 users` — do not matter. A locale
  that writes `1,41` is handled by treating a comma as a decimal point.
- **No configuration.** Haven has nowhere for a user to set a value yet,
  so the threshold is the core count and the refresh interval is whatever
  `tick` is. Both are the defaults you would have chosen anyway.
- **Refusals are shown, not hidden.** Without the `uptime` grant the
  segment reads `` `load-average` was not granted permission to run
  `uptime` ``, which is long for a status bar and deliberate: a plugin
  that silently draws nothing because of a missing permission is a plugin
  nobody can diagnose. Without the `nproc` grant it simply never goes
  bold.
- **`uptime` not installed** comes back as an error from the host and is
  shown the same way, with the system's own message in it.
