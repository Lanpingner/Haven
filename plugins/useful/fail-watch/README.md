# fail-watch

Says when a command failed, and what the failure was, in words.

```console
cargo: failed
mke2fs: not found
deploy.sh: not executable
make: killed (SIGKILL)
ffmpeg: crashed (SIGSEGV)
curl: exited 28
```

## What it does

On every `command-ended` event it reads `status`, translates it, and
returns a single `notice` naming the program and what happened to it.

| Status | Notice |
|---|---|
| `0` | nothing |
| `-1` (shell did not report) | nothing |
| `1` | `failed` |
| `126` | `not executable` |
| `127` | `not found` |
| `128+6` | `aborted (SIGABRT)` |
| `128+9` | `killed (SIGKILL)` |
| `128+11` | `crashed (SIGSEGV)` |
| `128+13` | `broken pipe (SIGPIPE)` |
| `128+14` | `timed out (SIGALRM)` |
| `128+15` | `terminated (SIGTERM)` |
| other `128+n` | `killed by signal n` |
| anything else | `exited n` |

The translation is the whole value of the plugin. `127` means the shell
could not find what you typed, which is a typo or a package you have not
installed; `126` means the file is there and is not executable, which is
a forgotten `chmod`; `137` is a `SIGKILL`, and on a build that is very
often the out-of-memory killer. A notice reading `exited 137` sends you
to a search engine. One reading `killed (SIGKILL)` sends you to `dmesg`,
which is where the answer is.

## What it stays quiet about

A plugin that cries wolf gets uninstalled by the end of the first day,
so two whole classes of non-zero exit are deliberately not failures.

**You interrupted it.** `130` is `SIGINT` — Ctrl-C — and `131` is
`SIGQUIT` — Ctrl-\. Both mean the person at the keyboard stopped the
command on purpose, several times an hour, and being told that the thing
you just killed has died is worse than being told nothing.

**Exit 1 that means "no".** For `grep`, `egrep`, `fgrep`, `rg`, `ag`,
`ack`, `diff`, `cmp` and `test`, a status of 1 is an answer, not a fault:
no matches, no difference, not true. `grep TODO src/` finding nothing is
a search that worked. Those programs are excused for status 1 **only** —
`grep` exiting 2 is a real error, a bad pattern or an unreadable file,
and is still reported.

The program's name is taken from the command the shell reported, with
leading `VAR=value` assignments skipped and any directory stripped, so
`LC_ALL=C /usr/bin/grep x` is recognised as `grep`. That matching is
what makes the silence work, so it is the part most worth reading if the
plugin ever says something it should not.

## What it needs

Nothing. No capabilities, no store, no `render`. The event carries
everything it decides on, which is why it has no state to lose on a
reload — `on_event` is a pure function of the event and the plugin would
behave identically if it were reloaded between every command.

## Limits

- **It needs shell integration.** No `OSC 133`, no `command-ended`, no
  notices.
- **The allowlist is a list of names.** A wrapper script called
  `my-grep`, or `grep` run as `rg --vimgrep` inside a function, is not
  recognised, so a routine "no matches" from one of those will be
  reported as a failure. Extending the list is a one-line change; making
  it configurable needs plugin configuration, which Haven does not have
  yet.
- **A pipeline reports one status.** The shell reports the status of the
  pipeline, so `cat x | grep y` attributes a failure to `cat`.
- **Signals are inferred from `128+n`**, which is the shell's convention
  and not a fact. A program that genuinely chose to exit with 139 will be
  described as having crashed.
- **It does not suppress repeats.** Ten failing commands in a row produce
  ten notices, because each one is a separate thing you did.
