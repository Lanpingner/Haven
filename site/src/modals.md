# One modal, everywhere

Haven has a handful of things that open over your work and ask you to pick
something: the shell menu, the command palette, the history, the session
tree. They are one widget. Not for tidiness in the source — for the thing
you notice, which is that all four behave the same way.

```
┌─ history · type to search ───────────────────────────────────────┐
│ cargo test --workspace            │ cargo-test - execute unit    │
│ git commit -m 'the dialog holds'  │ and integration tests        │
│ grep -rn needle crates/           │                              │
│ vim notes.md                      │ Compile and execute unit,    │
│                                   │ integration and documen-     │
│                                   │ tation tests.                │
│ enter types · esc · +36 more                                     │
└──────────────────────────────────────────────────────────────────┘
```

Three rules, which is the whole of it.

## The box holds still

This is the rule the others exist to serve, and it was a real bug for
longer than it should have been. Both of the old modals sized themselves to
their contents — as many rows as the filter admitted, as wide as the widest
line in it. So the window grew and shrank on every keystroke, and the line
you were reading moved out from under the cursor while you read it.

A searchable modal now takes a **fixed frame off the screen**, the way
telescope does in neovim: two thirds of the rows, four fifths of the
columns, and nothing inside it can move that. Narrow the list to one line
and the box is the same box with nineteen blank rows. Nothing matching at
all says `nothing matches` in a box of the same size, because a window that
vanishes and a key that does nothing look alike.

Sizing it from the *unfiltered* list — the obvious first fix — only moved
the problem: the help panel beside the history is as long as the selected
command's man page, so the height still jumped on every arrow press. The
frame has to come from the screen or it comes from something that changes.

Room for the panel is reserved from the first frame, too. A man page lookup
and a pane preview are both a round trip away, and a box that opens narrow
and widens a moment later is the same jump arriving late.

A **menu** is the exception: four shells picked by digit is sized to fit,
because nothing about it changes while it is open and a short list in a
screen-sized frame reads as a mistake.

## The title says where you are and how to search

One line, two questions:

- `sessions · type to search` — what this is, and that typing narrows it.
- `sessions / ops` — and once you are narrowing, what by.

The hint goes when it stops being true. A query longer than the box is cut
with an ellipsis rather than allowed to widen it; what you typed is on the
screen in front of you anyway, and the box holding still matters more.

## The panel is optional and on the right

Same furniture, different content: `whatis` and the top of a man page beside
a history line, a live preview beside a pane in the session tree. It gets
slightly under half the frame, so the list — the thing you are actually
choosing from — is the wider column. Below 72 columns there is no panel at
all; the list is what you came for.

## Everything in them agrees

| Key | Does |
|---|---|
| arrows, or `j` / `k` | move, wrapping at the ends |
| type | narrow the list, where the list is searchable |
| `Backspace` | take a character back |
| `Enter` | take the highlighted one |
| `Esc` | close, changing nothing |
| `1`–`9` | pick outright, on a short menu that is not searchable |
