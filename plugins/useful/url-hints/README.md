# url-hints

How many links are on the screen, and which one was most recent.

```
3 links  docs.rs/…
1 link  github.com/…
```

Nothing at all when there are no links in view.

## What it reads, and why

**It reads the text of the pane you are looking at: the last 200 rows,
every time a command finishes and every time you move the focus to
another pane.** That is the whole of what scrolled past — the commands
you typed, the output they produced, and anything else that appeared
there, including a token or a password if one was printed.

It cannot do its job with less. A URL is part of a pane's *contents*, so
the only function that can find one is `pane-text`, and that function
needs `read-pane-content`. The permission Haven can offer without it,
`panes()`, gives titles, working directories and the running command —
facts *about* a pane, not a single character of what is in it. There is
no "let it see only the links" permission, because finding the links is
what requires reading the rest.

So the honest version is: this plugin sees everything in the focused
pane, in order to count a particular shape in it. In exchange:

- **Nothing is sent anywhere.** There is no `net` capability in Haven,
  the host refuses one, and this plugin does not ask for one.
- **Nothing is written to disk.** The count and the latest link are kept
  in memory for as long as the plugin is loaded, and go away with it. In
  particular they are *not* kept with `Command::Remember`, which
  persists: a remembered URL is a line of your screen copied into a file
  that outlives the session, and a URL can carry a session token in its
  query string.
- **Nothing is logged.** Haven's log is a file.

If that trade is not worth `3 links` to you, do not grant it. That is a
reasonable conclusion and this README is written so that it can be
reached.

## What it does

On `command-ended` and on `pane-focus` it reads the focused pane once,
scans it for URLs, and keeps two things: how many distinct ones there
were and the last of them. `render` formats those two things.

**It never scans in `render`.** `render` is called every time Haven
draws, which is every time anything on the screen moves; searching two
hundred rows per frame for an answer that only changes when new output
arrives would be a waste of the machine, and the host would be copying
the pane's text across the component boundary each time. Scanning on an
event is not an optimisation here, it is the design.

## The rules for finding a URL

There is no regex crate in a plugin, and a regex would be the wrong tool
anyway: matching a URL is easy and deciding where one *stops* is the
hard part. The scanner is written by hand and these are its rules.

| Case | What happens |
|---|---|
| `https://example.com` | a link |
| `See https://example.com.` | the full stop is the sentence's, not the URL's |
| `(https://example.com)` | the closing bracket is dropped; brackets *inside* a URL are kept, so `https://en.wikipedia.org/wiki/Foo_(bar)` survives whole |
| `<https://example.com>` | the angle brackets are not URL characters, so they end it |
| `www.example.com` | a link, as printed, with no scheme invented |
| `www.foo` | not a link: a bare `www.` needs two dots to stand apart from prose |
| `http://` | not a link: what is left has to look like a host |
| `http://localhost:8080/x` | a link, and so is `http://127.0.0.1:8080` |
| the same URL ten times | one link |
| `https://example.com/a/https://b` | one link: a scheme in the middle of a path is part of that path |
| `verylong.example/path` broken over two rows | counted up to the break, and **not** reassembled |

### Why a wrapped URL is not reassembled

A terminal hard-wraps a long line at the pane width and leaves no mark
to say that it did. `pane-text` hands back the wrapped rows with nothing
to distinguish them from two separate lines of output, so a row ending
in URL-ish characters followed by a row beginning with URL-ish
characters is exactly as likely to be two things as one. Joining them
would manufacture URLs that were never printed — and the only thing
anybody does with one of these is paste it into a browser, where a wrong
URL is worse than a missing one. So the plugin counts what it can see
and may show a wrapped link short.

If the WIT world ever reports which rows continue a logical line, this
is the first plugin that would use it.

## What it needs

`read-pane-content`, and the `command-ended` and `pane-focus` events.
No `exec`, no `net`, no filesystem, and no store.

## Limits

- **200 rows.** A link further back than that is not counted. The window
  is a screenful and a little scrollback, chosen so the scan is never
  something you can feel.
- **It cannot tell where the last command's output began.** `pane-text`
  gives rows, not "everything since this event", so the count is "links
  visible in the last 200 rows", not "links this command printed".
- **It cannot open one.** `render` returns text; there is no click, no
  overlay and no picker yet, so the segment tells you a link is up there
  and you still scroll to it. The shortened host is there to make that
  scroll shorter.
- **It needs shell integration** for `command-ended`. Without `OSC 133`
  the count only updates when the focus changes.
- **ASCII only.** An internationalised domain ends the span at its first
  non-ASCII byte, because guessing where one of those stops in terminal
  output is a worse failure than cutting it short.
