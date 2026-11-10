# secret-guard

Tells you when something shaped like a credential has just been printed
to your screen.

```
an AWS access key id in pane 1 — now in the scrollback
a GitHub token in pane 2 — now in the scrollback
a private key and a bearer token in pane 1 — now in the scrollback
```

That is the whole output. **The notice never contains the secret.**

Nothing on the status bar at all, unless the plugin was not granted the
permission it needs, in which case it says so there rather than quietly
not guarding.

## Why it exists

A token that appears in a terminal is in the scrollback, and the
scrollback is in the session, and the session is in whatever screenshot
ends up in a bug report an hour later, or in whatever `tmux`-style
capture or screen share happens next. The moment it is printed is the
only moment anyone can act on it — rotate the key, clear the buffer,
stop sharing the screen — and it is precisely the moment nobody
notices, because it went past in the middle of two hundred rows of some
tool's debug output.

## What it reads, and why

**It reads the text of a pane — the last 200 rows — once, when a command
in it finishes.** That is everything in those rows, which here is the
entire point: the plugin's job is to look at what just scrolled past, in
order to tell you that something in it should not have.

There is no way to do it with less. The question is literally "what
characters are now on that screen", and `pane-text` is the only function
that answers it. It needs `read-pane-content`. `panes()` would give the
title, the working directory and the command — never the output.

So this is the plugin that justifies the capability Haven names first in
every prompt, and granting it is a judgement that this plugin's handling
of what it sees is trustworthy. That handling is deliberately tiny:

- **The secret never leaves the plugin.** The notice names the *kind* and
  the pane. A notice that quoted the token would print it a second time,
  onto the status bar, where it can be logged, screenshotted or copied —
  which would make this plugin the leak it is warning about.
- **Nothing is written to disk.** No `Command::Remember`, deliberately:
  that persists, and a credential — or a fingerprint of one — in a file
  that outlives the session is the opposite of the point. The only thing
  kept between events is the text of the last notice, in memory, so the
  same find does not warn twice. That text is a kind and a pane number.
- **Nothing is sent anywhere.** There is no `net` capability, the host
  refuses one, and this would be the worst imaginable plugin to want
  one.
- **Nothing is logged.** `host::log` writes to a file, so this plugin
  does not call it.

## What it looks for

Only shapes that cannot plausibly be anything else.

| Shape | Rule |
|---|---|
| AWS access key id | `AKIA` then 16 upper-case alphanumerics |
| GitHub token | `ghp_` or `github_pat_` then 20 or more token characters |
| API key, OpenAI style | `sk-` then 32 or more token characters including at least one digit |
| private key | a line with `-----BEGIN` and `PRIVATE KEY` in it |
| bearer token | `Bearer ` (capitalised, as in a header) then 16 or more token characters |
| exported secret | `export NAME=value` where `NAME` is `SCREAMING_SNAKE_CASE` ending in `_TOKEN`, `_SECRET`, `_KEY`, `_PASSWORD`, `_PASSWD` or `_CREDENTIALS`, and the value is 12 characters or more, is not `$ANOTHER_VARIABLE`, and contains no `/` |

Each prefix must begin a word, so `ghp_` inside a longer identifier is
not a token. The length rules are what keep placeholders out: `sk-test`
is something everyone writes, and `sk-some-long-hyphenated-filename`
is a filename, which is why that rule also wants a digit. The last two
conditions on `export` are what keep `export SSH_KEY=~/.ssh/id_ed25519`
and `export GITHUB_TOKEN=$GH_PAT` quiet — a path to a key is not a key,
and a reference to a variable is not its value.

## What it deliberately does not try to detect

A false positive a day gets a plugin uninstalled, and an uninstalled
guard catches nothing. So the omissions are as deliberate as the rules.

- **Entropy heuristics.** "This string looks random" is true of every
  git object id, content hash, UUID, docker image digest and line of
  minified JavaScript, and build output is made of those. A rule that
  fires once during `cargo build` is a rule nobody keeps on, so there is
  no entropy rule here at all.
- **Generic `password=`, `token=`, `secret=`.** These appear in `--help`
  text, in documented connection strings, in error messages containing
  URLs, and in this plugin's own source. Without a shape for the value
  there is nothing to tell `password=<your password>` in a usage message
  from the real thing — and a real credential in that form is usually
  caught by one of the rules above anyway.
- **JWTs** (`eyJ…`), even though the prefix is distinctive, because
  example tokens are printed in documentation, test fixtures and
  tutorials constantly, and an expired example JWT is not a credential.
- **Anything about whether the secret is live.** The plugin cannot know
  if a key is real, revoked or invented for a test. It reports the shape
  it saw and leaves the judgement to you.

The cost of being conservative is that it misses things: an in-house API
key with no prefix, a password typed at a prompt that echoed, a `.env`
file printed with `cat` whose variable names do not match. That is the
intended direction to be wrong in. **This is a smoke alarm, not an
auditor** — treat a silent status bar as "nothing obvious", never as
"nothing".

## What it does

On `command-ended` it reads the pane once and scans each line for the
shapes above. If it finds anything it returns a single
`Command::Notice`. If it finds the same thing again — the same kinds in
the same pane, which is what happens when the next command prints
nothing new — it stays quiet.

**It never scans in `render`.** `render` is called whenever Haven draws;
searching two hundred rows for credential shapes per frame would be the
most expensive thing on the status bar, for an answer that can only
change when new output arrives. `command-ended` is both the cheapest
moment and the most useful one, because that is when whatever printed
the token has finished printing it.

## What it needs

`read-pane-content`, and the `command-ended` event. No `exec`, no `net`,
no filesystem, no store.

## Limits

- **200 rows.** A credential printed further back than that, by an
  earlier command, is not seen.
- **It needs shell integration.** `command-ended` comes from `OSC 133`.
  Without a shell that emits it, nothing triggers a scan and the plugin
  is silent — and it cannot tell you that, because it never hears
  anything.
- **It cannot clear the scrollback for you.** `Command::Act` can run any
  of Haven's own actions, so a future version could offer to; the first
  version only tells you, because a plugin that wipes your scrollback on
  a false positive is a plugin that loses your output.
- **One notice per find.** It does not say how many times the token
  appeared, or where in the pane, because both would be a step towards
  quoting it.
