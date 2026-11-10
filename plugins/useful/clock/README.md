# clock

`12:34` on the status bar, and nothing else.

The smallest plugin that is still worth having, and the one that proves
the shape: no capabilities, no events, one function.

## What it needs

Nothing. No `exec`, no `read-pane-content`, no filesystem — WASI's clock
is granted to every plugin because `std::time` needs it to start up and
because the time of day is not sensitive.

## Limits

UTC, because that is the only thing a sandboxed component can know. There
is no `$TZ` to read and no timezone database to read it against, and
nothing yet lets a user configure a plugin, so a local-time clock would
have to guess an offset. The bar's own clock, on the right-hand side, is
the daemon's and does know your timezone — this plugin exists as an
example more than as a feature.
