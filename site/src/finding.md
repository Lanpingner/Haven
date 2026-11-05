# The history, and finding things

Two modals, both built on the same idea: the daemon knows things about your
session that you would otherwise go looking for by hand.

## What this pane has run

`Alt+H` opens the history of the focused pane, with what each command *is*
beside it:

```
┌─ history · type to search ─────────────────────────────────────┐
│ ! false                      │ grep (1) - print lines that     │
│ + cargo test --workspace     │ match patterns                  │
│ + git status --short         │                                 │
│   grep -rn needle crates/    │ grep searches for PATTERNS in   │
│   vim notes.md               │ each FILE.  PATTERNS is one or  │
└────────────────────────────────────────────────────────────────┘
```

Type to narrow it, arrows to move, `Enter` to take one, `Esc` to drop it.
The box is a fixed frame and does not resize as you narrow the list — see
[one modal, everywhere](./modals.md), which is also what the palette and the
session tree are.

### Each command once

A history is mostly repetition — `cargo test` forty times, `git status`
twice that — and a list you are reading to find one line should not spend
forty rows saying the same thing. Each command appears once, at its **most
recent** run, so the order still reads as what you did last and the status
beside it is how it ended last.

Both halves go through the rule, and the shell file is deduplicated before
it is capped rather than after: 500 entries are meant to be 500 things
worth choosing from, and cutting first would spend most of them on repeats
of the last few commands.

### Two sources, and the difference is visible

The list is merged from two places that know different things:

- **What this pane ran**, learned from [shell integration](./architecture.md)
  as it happened. In order, this pane's own, and with how each command
  ended — `+` for a success, `!` for a failure, `*` for something that never
  reported an ending.
- **The shell's history file**, which goes back months and knows nothing
  else: no pane, no order within a pane, no statuses. Those lines are
  unmarked.

fish, zsh and bash are read, in each one's own format and from each one's own
location. A file that is missing, unreadable or in a format Haven does not
know leaves the list with the pane's own commands rather than an error — the
half that is usually what you were reaching for anyway.

### Enter types it; it does not run it

The line arrives at your prompt with the cursor after it, for you to look at
and press Enter on yourself. What a command did an hour ago is not what it
would do now — a different directory, a different branch, a different
`build` to delete — and the one time that matters is the one you would not
have chosen.

### The panel

What the selected command is, from `whatis` and the opening of its `man`
page. Nothing is run to describe it: `<command> --help` would cover more,
including everything with no man page, but it means running a program that
may never have heard of `--help` and may do its job instead. A terminal that
deletes something because you moved a selection down one line is not a
terminal anybody should use.

The lookup happens on a thread in the daemon and the answer is cached, so
holding an arrow key down does not start a `man` per keypress — and an
answer that arrives after the selection has moved on is dropped rather than
shown against the wrong line. A command with no man page simply has no
panel, which is most shell functions and all of your own scripts.

## Finding a line anywhere in the session

`Ctrl+A` `/` asks for a pattern and searches **every pane in the session**,
not just the one in front of you:

```
grep error — 7 lines
  2:413    error[E0308]: mismatched types
  2:467    error: could not compile `haven-client`
  1:88     error: linking with `cc` failed
```

Each hit names its window and its row. `Enter` goes there: the window, then
the pane, then the row — none of the three implies the others, which is why
a hit carries all three. The pane you were in is where you left it.

This is the other half of the search in [copy mode](./keys.md), which answers
*where is the next one* in the pane you are reading. This one answers *where
are they all*, across panes you are not looking at, which is the question
you have when something scrolled past in a window you have since left.

Matching folds case unless the pattern has a capital in it, as every editor
since vi. Results are capped — 200 a pane, 1000 in all — because a one-word
search over a day's output would otherwise send a client more lines than it
could ever show.
