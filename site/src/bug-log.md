# Notes from the bug log

The [design plan](./plan.md) keeps a record of what each piece of work actually
broke, because the interesting failures were never where they looked. A
selection, kept here because they say more about the program than a feature list
does.

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
