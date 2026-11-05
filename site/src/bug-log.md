# Notes from the bug log

The [design plan](./plan.md) keeps a record of what each piece of work actually
broke, because the interesting failures were never where they looked. A
selection, kept here because they say more about the program than a feature list
does.

## Four commands that worked where nobody could see

Reported in one sitting, and all four the same shape: something happened
correctly, somewhere nothing reads.

**A session rename reached nothing.** The name was kept twice — once in the
mux, once beside it in the daemon — and `rename-session` wrote the mux's copy
while the status bar, `haven ls` and the session tree all read the other one.
The command was never broken. The second copy is gone; the daemon asks the
mux.

**The clock did not move.** The bar is rebuilt on every redraw, and the
comment above it says the clock moves on its own — but a redraw only happens
when something *else* has changed, and the time is the one thing on the bar
that no message announces. So it sat still until a keystroke or some output
happened to repaint the row. The client now redraws when the second it last
drew for is no longer the current one, and tells a frontend how long it may
sleep: the GPU window was written to wait with no timer at all, which is worth
keeping for a window with no bar.

**Picking from a menu only worked for half the verdicts.** The key path
handled thirteen kinds of outcome; the menu path handled one — the kind that
sends a message to the daemon — and dropped the rest in silence. So a menu
entry whose job is to ask a second question could not ask it, and a notice
from a menu never reached the bar. Two places interpreting one thing; now
one.

**Most of the command palette was inert.** Its entries for commands that take
an argument are `command-prompt <verb> ` strings, and a typed line could not
run the two commands that change the client's own mode — the prompt and
`switch-table`. Picking `rename-window` from the palette asked for a prompt
and got nothing. A typed line now goes through the same path a bound key
does, because that is what it is. The trailing space in the prefill turned
out to matter too: trimmed, you typed the name onto the end of the verb.

A fifth, found while reading rather than reported: a new pane started wherever
the *daemon* was, which for an autostarted daemon is `/`. Splitting a pane to
run one command against the files in front of you meant `cd`-ing back to them
first. Panes now open where the pane you were in is — including a respawn,
whose directory has to be read before the old child is hung up, because
`/proc` has nothing to say about a process that has gone.

## Two keys, one of them invisible

`k` at the top of copy mode's view scrolls one row to reveal the row above —
and the cursor must stay where it is, in *view* coordinates. Re-pinning it to
the history row it had been on looks more correct and undoes the move the
scroll was for, so the key appears dead.

The opposite applies to a selection's anchor, which is absolute in the history.
Selecting more than a screenful means scrolling, and a view-relative anchor
slides out from under the selection as soon as it does. One cursor, two
coordinate systems, and swapping them is a bug either way round.

## A line through every blank cell

`CSI > 4 ; 1 m` is XTMODKEYS — modifyOtherKeys — which fish sends before *every*
prompt. Read as `SGR 4;1` it set underline and bold on the pen and nothing ever
reset them, so every cell erased afterwards was drawn with a rule through it: a
line across the blank end of each row.

A private parameter marker changes what a CSI final byte means, and the two
forms share that byte. The same mistake made `CSI > 1 u` (kitty keyboard push)
restore the cursor and `CSI ? Pm r/s` move the scroll region. Private sequences
other than DA2 are now ignored rather than run as their public namesakes.

Reported from a real window. No test caught it.

## "Takes forever to render" had nothing to do with the GPU

The fallback font scan set its "already scanned" flag only when it *found* a
face. A character no font covers — an emoji, on a machine with no colour emoji
font — therefore rescanned every font on the system every time it appeared:
3829 faces, 8.6 GB, read in full to look at their character maps.

A prompt with three emoji took 5.69 s to render, of which 5.56 s was syscall
time. The faces are mapped once and kept now; the same render takes 0.249 s.

## The window-manager freeze was none of the three things it looked like

Measured with `crates/winit-probe`, which exists for this: not winit (`--no-gpu`
never stalled), not vsync (`Immediate` still blocked 2052 ms), and not the event
loop (moving present to its own thread freed the loop and the freeze continued).

What it is: on a hybrid laptop Vulkan can only run on the discrete NVIDIA GPU,
which reaches an Intel-driven display over PRIME, and presenting across that
while the window manager unmaps the window **blocks the X server itself** — a
bare `GetInputFocus` on an unrelated connection took 2050 ms, so every client on
the display froze, not just ours.

winit offers no signal for it. `Occluded` comes from `VisibilityNotify`, which an
unmapped window never sends, and `is_visible()` stayed `true` through the whole
stall. A terminal has no use for a discrete GPU, so the integrated one is now the
default, and the window stops rendering entirely while occluded.

A footnote for anyone copying the fix: asking for `PowerPreference::LowPower`
is not the same as leaving it alone. In wgpu 23 the default is `None` — "power
usage is not considered" — so an adapter request that looks neutral can still
be handed the discrete GPU. Found by another project on this machine, which
hit exactly this configuration before changing one line.

## The texture limit that was not the hardware's

Requesting `Limits::downlevel_defaults()` caps textures at 2048 *whatever the
hardware can do*, so a window 2088 pixels tall could not be given a surface and
wgpu aborted the process — a validation error cannot be caught.

It never showed while the GPU default was the discrete card, because that window
happened to be smaller; switching to the integrated one and maximising found it.
The limit goes from 2048 to 16384 on this machine. Measured, not assumed.

## Panes stacked one above another had nothing between them

The layout reserves a cell between neighbours in both directions, but the
client's compositor only ever drew the vertical rule, so a row split showed as a
blank line and read as two panes running together. Standalone mode had drawn
both all along, which is why it never came up.

Rules are now decided from a map of which cells a pane covers: a gap is a rule
only where there is a pane on each side of it — which is also what keeps the row
the status line sits on from growing one — and the arms come from the
neighbouring gap cells, so four panes meet at a `┼` however the splits were
nested.

## The prefix key, twice

The GPU window ran without a key filter at all. It built its own `Client` and
never installed one, so `Ctrl+A` went straight to the shell and no binding in
the window did anything. Reading the key-mapping code would never have found it:
the mapping was correct.

Then the prefix still did nothing, because a window delivers key *releases* and
a terminal does not. The release of `Ctrl+A` matched the prefix table and
disarmed the prefix a moment after the press armed it; in command mode it would
have typed every character twice. Every test had used presses alone, which is why
both frontends looked identical in test and behaved differently in life.

Neither was findable without synthetic input, so `xsend` (XTEST) exists — and
with no window manager nothing holds the input focus, so it sets the focus itself
before typing.

## Smaller ones, kept because they cost time

- `haven server run -- prog -x` dropped `-x`. Every other subcommand does
  `cfg.args.extend(args.rest)`; the daemon's spawner never did, so the program
  ran with no arguments and looked like it had ignored them.
- `ED`/`EL` treated parameter `0` as "absent", shifting every erase mode by one
  and breaking `[1K` and `[2K`. Caught by a tab-stop test, not by the erase
  tests.
- A deferred wrap changes the cursor's *logical* offset by one; without that,
  widening a reflowed screen walked the cursor backwards.
- Trailing blank rows must not count as content during reflow, or every
  narrowing pushes real text into scrollback.
- The layout solver laid panes out past the screen edge when a one-cell floor
  per pane plus separators exceeded the space available. Found by proptest; a
  pane that cannot fit is now hidden, as tmux does.
- Growing a pane can legitimately shrink it, two ways: hidden siblings become
  visible when the parent grows, and integer apportionment can withdraw a
  rounding bonus — the Alabama paradox.
- A strict shift comparison never detects a scroll, because applications write
  the new bottom line *before* scrolling. Scoring the overlap fixes it.
- A dropped `Pty` left its child running, so a dead client stayed attached from
  the daemon's point of view. Dropping the master now hangs up the group — and
  reaps it, since `kill(pid, 0)` succeeds for a zombie.
- Render pipelines built for a hardcoded texture format fail validation on a
  window whose surface prefers BGRA. Found by running under a software
  rasterizer, which chooses differently than the discrete GPU.
- Box drawing vanished wherever a panel gave it a background: procedural box
  drawing and background rectangles shared one quad buffer, and a background run
  is flushed when it *ends*, after the cells it covers are drawn.
- `PROTOCOL_VERSION` could not report its own skew, because the version lives
  *inside* a message — so a wire-format change makes the frame fail to decode
  before anything can compare versions. The decode error now names the likely
  cause instead of reporting "Hit the end of buffer".
- `OSC 52` payloads were kept base64-encoded and handed to the client as if
  they were text, so honouring one would have put `aGVsbG8=` on the clipboard.
- `str::replace` with a non-matching pattern is a silent no-op: an edit script
  that does not assert can "succeed" while changing nothing.
- `haven server run -- prog -x` dropped `-x`. Every other subcommand extends
  the child's arguments with the trailing ones; the daemon's spawner never did,
  so the program ran with nothing and looked as though it had ignored them.
- `raw.rs` had been asking host terminals for mouse reports since M2.5, and the
  decoder had no case for them — so the replies fell through to the key path
  and typed rubbish into the pane.
- `haven restore` restored nothing on a fresh daemon. The CLI attached before
  asking, and attaching with no session named calls `first_or_create` — so it
  made an empty `haven-1`, and the restore then skipped the saved session of
  that name because one was already there.
- A pane-numbering overlay that any key dismisses is dismissed by the *release*
  of the key that opened it, a millisecond later. Only presses are keystrokes.
- A double-width character in a one-column grid indexed past the end of the
  row: the wide glyph wrapped to the next line, where it did not fit either,
  and then wrote its spacer into a cell that was not there. One column is not
  hypothetical — a client that reports no window size gets a 1x1 pty — and the
  panic was in the *daemon*, so one misbehaving client took every session on
  the machine with it. Found by attaching from a pty that had never been
  sized, while testing something else.
