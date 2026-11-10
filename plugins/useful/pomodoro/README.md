# pomodoro

A work and break timer on the status bar: twenty-five minutes of work,
five of break, and fifteen every fourth time.

```
work 23:40
break 4:02
long 14:30
```

and when a phase ends:

```console
time's up — take 5
four done — take 15
break over — back to it
```

## What it does

It counts `tick` events. Each tick advances the current phase by one;
when the count reaches the length of the phase it returns a `notice`
saying so and a `remember` starting the next one. `render` formats what
is left as `m:ss` — bold while working, dim on a break, because a break
is a phase you should be able to ignore until it ends.

Work phases are counted, and every fourth one is followed by the long
break rather than the short one.

## The tick interval

The manifest asks for **`timer:2s`**, and every length in the plugin is
in units of two seconds: 750 ticks of work, 150 of short break, 450 of
long.

Two seconds is a compromise in both directions. A one-second tick is
eighty-six thousand calls into a sandbox per day to move a counter a
person glances at a few times an hour. A ten-second tick would make the
displayed seconds lurch in tens and would overshoot the end of a phase
by up to ten seconds. At two seconds the display is honest to the second
it shows, and a whole work phase costs 750 calls — which is nothing, but
is 750 fewer than the obvious choice.

## When ticks are late or lost — and they will be

Events are coalesced and rate-limited per plugin, machines suspend, and
a plugin is reloaded when it is updated. This plugin counts ticks rather
than reading a clock, so the consequence is simple and worth stating: **a
lost tick makes the phase two seconds longer, and the error
accumulates.** Suspend the laptop for an hour and the work phase did not
advance for that hour; it resumes where it was.

That is the behaviour wanted here rather than a shortcoming tolerated. A
pomodoro is a nudge to stop working, not a stopwatch. A timer that
caught up by firing three notices at once would be noise, and one that
announced "break over" for a break you slept through would be nonsense.
The price is that the timer drifts slow, which for a tool whose job is
to interrupt you at roughly the right time is the cheap direction to
drift in.

## Surviving a reload

Everything the plugin knows goes through `Command::Remember` and comes
back through `host::recall`, because a reload is invisible to the person
watching the status bar and the timer has to be invisible too. A Rust
`static` with interior mutability would survive calls within one loaded
instance and nothing beyond it, which is precisely the wrong lifetime
for a twenty-five minute phase.

The state is one key, `state`, holding `phase:ticks:completed`, rather
than three keys. `Command::Remember` carries a single key and value, so
three keys would be three commands applied one at a time, and an unload
between them would leave a phase that disagrees with its own count. One
key is one command and cannot half-apply.

A value that does not parse is treated as absent and a fresh work phase
begins. Starting a new pomodoro is a better answer to a value written by
some other version of the plugin than a blank status bar.

## What it needs

Nothing but the tick. No capabilities, no `exec`, no
`read-pane-content`.

## Limits

- **It cannot be configured.** 25/5/15 and every-fourth are fixed,
  because nothing lets a user set a plugin's values yet.
- **It cannot be paused or reset** from a keystroke. A plugin only
  receives the events its manifest subscribes to, and there is no event
  for "the user asked this plugin to do something"; `Command::Act` goes
  the other way. The nearest thing available is to `forget` the key,
  which only the plugin itself can ask for.
- **It starts the moment it loads.** `started` writes a fresh work
  phase, so installing the plugin begins a pomodoro whether or not you
  were ready. Without a way for the user to say "begin", starting is a
  better default than a timer that sits at `25:00` forever.
- **The clock moves in twos.** The seconds shown are always even. That
  is the tick interval being honest rather than a rounded display.
- **One timer per session.** The store is per-plugin, not per-pane, which
  is correct for this — a pomodoro is about the person, not the pane.
