# build-errors

A wall of compiler output, as a number.

```
12 errors, 3 warnings
1 error
4 warnings
```

Nothing at all when the build was clean.

## What it reads, and why

**It reads the text of the focused pane — the last 400 rows — once,
when a command finishes.** That is everything that scrolled past in
those rows: the compiler's diagnostics, the source lines it quoted back,
the commands you typed, and anything else that happened to be printed
there, including a secret if something printed one.

It cannot do its job with less. A diagnostic is part of a pane's
*contents*: the compiler wrote it to the terminal and nowhere else a
plugin can reach. The permission that does not need granting, `panes()`,
gives the pane's title, its working directory and the command line, none
of which says how many errors there were. There is no narrower
capability for "only the lines beginning with `error:`", because
deciding which lines those are means reading all of them.

In exchange, what it does with what it read is as small as it can be:

- **Nothing is sent anywhere.** There is no `net` capability, the host
  refuses one, and this plugin does not ask.
- **Nothing is written to disk.** Two integers are kept in memory for as
  long as the plugin is loaded. They are deliberately *not* kept with
  `Command::Remember`, which persists: an error message quotes the
  source line it is about, so a remembered diagnostic is a piece of your
  code and your screen copied into a file that outlives the session. A
  number on a status bar is not worth that.
- **Nothing is logged**, because Haven's log is a file.
- **Nothing read from the pane is ever shown.** The segment is two
  counts. It never quotes a diagnostic, which means it never re-prints
  whatever else was on the line.

## What it does

On `command-ended` it reads the pane once, counts, and keeps the two
numbers. `render` formats them.

**It never counts in `render`.** `render` runs whenever Haven draws;
counting four hundred rows per frame for an answer that changes once a
minute would be indefensible, and the host would be copying the pane's
text across the component boundary every time. The count is cleared on
`command-began`, so a stale number does not sit next to a build that is
still running, and on a focus change away from the pane it came from,
because a count from a pane you are not looking at is misinformation.

## What counts

| Shape | Example |
|---|---|
| rustc / cargo | `error[E0308]: mismatched types`, `error: cannot find value`, `warning: unused variable` |
| gcc / clang | `main.c:12:5: error: expected ';'`, `main.c:12: warning: …`, `fatal error: no such file` |
| a test runner | `FAILED: build/x.o` (ninja), `FAILED tests/t.py::case` (pytest), `--- FAIL: TestThing` (go), `test thing ... FAILED` (Rust) |

A gcc-style line is recognised by the `path:line[:col]` in front of it.
The digits are the whole test — without them, any sentence containing
the characters `: error:` would be counted, and build output contains
plenty of those.

## The summary line, which is the trap

Every one of these begins exactly like a real diagnostic and none of
them is one:

```
error: could not compile `haven` (lib) due to 2 previous errors
error: aborting due to 2 previous errors
warning: `haven` (lib) generated 3 warnings
warning: 2 warnings emitted
error: build failed
test result: FAILED. 2 passed; 1 failed
FAIL
```

Counting them is the difference between `12 errors` and `14 errors`,
which is the difference between a number people trust and one they stop
reading. They are recognised by what follows the familiar start — "due
to … previous error", "generated …", "… emitted", a `test result:`
prefix, cargo's own closing lines — and skipped before anything is
counted. A bare `FAIL` on a line of its own is go's verdict on
everything above it, so it is skipped too, while `FAIL<tab>pkg` is a
package that failed and counts.

## Why red and yellow

The README for plugins says to use the theme's own colours and that
`coloured` is for when a plugin genuinely needs to mean red. This one
does. The number means *failure*, and failure has had one colour in
every terminal anybody has used; in the theme's foreground it reads as
decoration beside the clock and the branch name.

The two colours used are indexed 1 and 3, not RGB. Indices 0–15 come
from the user's own palette, so this is the red and the yellow their
theme chose, at their contrast — not a hard-coded shade that disappears
on half the themes people have.

## What it needs

`read-pane-content`, and the `command-began`, `command-ended` and
`pane-focus` events. No `exec`, no `net`, no filesystem, no store.

## Limits

- **It cannot tell where the last command's output began.** `pane-text`
  gives rows, not "everything since this event", so a short build whose
  predecessor's errors are still on screen counts both. 400 rows is a
  judgement about how often that is wrong against how often a long build
  would be cut short.
- **Linker errors are not counted.** `ld: error: undefined symbol` has
  no line number and every linker words it differently, so a link
  failure shows as nothing rather than as a number that happens to be
  wrong.
- **It is a count, not an analysis.** A single mistake that produces six
  diagnostics is six. The compiler's own total would say the same.
- **It needs shell integration.** `command-ended` comes from `OSC 133`;
  without a shell that emits it, nothing ever triggers a count.
