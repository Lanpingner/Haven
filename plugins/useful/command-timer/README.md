# command-timer

How long the last command took, said out loud only when it took long
enough to be worth saying.

```console
cargo build took 2m14s
cargo test took 1h03m
./configure took 48.6s
```

and a small segment on the status bar for afterwards:

```
last 2m14s
```

## What it does

On every `command-ended` event it looks at `millis`. If the command ran
for **ten seconds or more** it returns two commands: a `notice` naming
the command and its duration, and a `remember` putting the duration where
`render` can find it.

Under ten seconds it returns nothing at all.

### Why ten seconds

Ten seconds is about where a person stops waiting and goes to do
something else — it is the usability literature's limit for keeping
attention on a response, and it matches what a terminal feels like. Below
it you watched the command happen and already know it was quick; above
it you looked away, and when you look back the one fact you do not have
is how long it was.

It is also comfortably above the things a shell does all day. `git
status`, a warm `cargo check`, a `grep` across a repository and every
`cd` land under ten seconds, so the plugin is silent during ordinary
work. Haven has nowhere for a user to set a threshold yet (see "No
configuration" in `plugins/README.md`), so the default has to be one that
nobody needs to change, and a quiet one is the only kind that qualifies.

### Why it keeps a segment too

A notice is shown for a moment. "How long did that actually take" is
normally asked *after* that moment — once the build has finished and you
have read the output and then wondered. The segment answers it with no
effort from anyone.

It is updated only for durations over the threshold, for the same reason
the notice exists only for those: a segment that rewrote itself after
every `ls` would be a flicker in the corner of the eye, and a status bar
that moves constantly is one people learn not to look at.

### Durations

`8.2s` under a minute, `2m14s` under an hour, `1h03m` beyond. Precision
drops as the number grows, because that is how people say times: tenths
matter at ten seconds and are ridiculous at an hour.

## What it needs

Nothing. No capabilities, no `exec`, no `read-pane-content`. It needs
the `command-ended` event and the plugin store, which every plugin has.

## Limits

- **It needs shell integration.** `command-ended` comes from `OSC 133`;
  without a shell that emits it there are no events and the plugin is
  silent. That is not a failure mode it can report, because it never
  hears anything to report it about.
- **The threshold is fixed** at ten seconds, because nothing lets a user
  configure a plugin yet.
- **It trusts `millis`.** The duration is the shell's, so a command
  suspended with Ctrl-Z and resumed counts the time it spent stopped.
- **The command is shortened** to its first two words, with `…` for the
  rest, so two different long pipelines beginning `cargo build` read the
  same in a notice.
- **One duration, not a history.** The store holds the last one. A
  plugin that kept a log of timings would be a different and larger
  thing.
