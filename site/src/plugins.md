# Plugins, and where they come from

Haven takes plugins from **signed repositories**, the way a distribution
takes packages. You add a repository once, pinned to one GPG key. From
then on, everything Haven fetches from it has to be signed by that key —
and everything it installs has to hash to what that signed index says.

What exists today is the whole of that: repositories, signatures, the
index, hashes, consent and installation. **Nothing loads a plugin yet.**
The runtime — the WASM host described in [§15 of the
plan](./plan.md) — is the next piece of work. `haven plugin install`
tells you so when it finishes.

That order is deliberate. Distribution is the part you cannot retrofit:
once people are installing plugins by pasting a `curl | sh`, a signature
is a migration rather than a feature.

## The repository

```console
$ haven plugin repo add https://github.com/haven-terminal/plugins.git \
      --key '2F10 059C F663 6B15 0B95 8486 7D04 983E 5D12 A3E4'
$ haven plugin repo update
```

That is a git repository, so Haven clones it shallowly and pulls on each
update — one of [the three kinds](#what-a-repository-is).

The fingerprint is the same key the Haven releases and the pacman
repository are signed with, and it is published in more than one place
on purpose: in `haven-repo.pub` on every release, in
[`packaging/README.md`](https://github.com/haven-terminal/Haven/blob/master/packaging/README.md),
and in that repository's own README. A fingerprint that only ever
appears beside the thing it signs is a fingerprint nobody has verified,
so compare it against whichever of those did not come to you the same
way the repository did.

## What there is to install

Sixteen plugins live in
[haven-terminal/plugins](https://github.com/haven-terminal/plugins),
which is also the signed repository they are published from. Each one
really runs — the tests there load the real component into this runtime
with the real capability checks:

| Plugin | Shows | Needs |
|---|---|---|
| `clock` | `17:20` | nothing |
| `cwd-crumbs` | `~/P/H/c/haven-client` | nothing |
| `python-venv` | `venv:haven` | nothing |
| `ssh-marker` | `⇥ box`, when the pane is elsewhere | nothing |
| `command-timer` | `cargo build took 2m14s`, past ten seconds | nothing |
| `fail-watch` | `mke2fs: not found` | nothing |
| `pomodoro` | `work 23:41` | nothing |
| `gitline` | `master* ↑5` | `git` |
| `git-wip` | `+736-46` | `git` |
| `git-stash` | `⚑2` | `git` |
| `load-average` | `7.22`, bold past the core count | `uptime`, `nproc` |
| `disk-free` | `205G 54%` | `df` |
| `docker-status` | `docker 7` | `docker` |
| `url-hints` | `3 links` | your screen |
| `build-errors` | `12 errors, 3 warnings` | your screen |
| `secret-guard` | warns when a credential was just printed | your screen |

Four of them, installed, look like this — between the tabs and the
clock:

```
 haven-1   1:1      17:20 /P/HavenTerminal +795-55 master* ↑5   1 panes · 00:10
```

The last three are the ones that ask to read what your panes are
showing, and `secret-guard` is the one that justifies the capability: it
warns you when something shaped like an AWS key or a GitHub token has
just been printed into your scrollback. It never repeats the secret, and
it stores nothing — a `remember` would write your screen into a file.

## How a plugin runs

In the daemon, sandboxed, with nothing but what it was granted.

A plugin is a WebAssembly component. It gets its own memory, its own
fuel, and a WASI context with **nothing in it** — no filesystem, no
network, no environment. `std::fs::read("/etc/passwd")` inside a plugin
fails like an empty disk. That is not a list of things Haven forbids; it
is the absence of anything to reach.

Fuel rather than a timeout: 50 million instructions per call, so a
plugin that loops forever stops deterministically rather than when the
machine happens to notice. One that traps is unloaded and said to be
broken, never retried in a loop.

They run in the **daemon**, not the client, because they need the panes
and have to survive you closing the window: a segment that vanished on
detach and came back different would not be a status segment. A client
is sent what a plugin *drew*, never the code — over SSH it could not run
it, and if it could it would be running with permissions granted for a
different machine.

Loaded **once for the whole daemon**, not once per session. A plugin is
one program; instantiating it for every window anyone opens is waste,
and because `remember` is scoped to the plugin, two sessions holding two
instances of `pomodoro` would write the same file and clobber each
other. A session still gets its own answers — each plugin is handed that
session's panes immediately before it is asked anything, so `cwd-crumbs`
draws a different directory in each bar from the one instance.

### Nothing is compiled when you open a terminal

A component is compiled when you **install** it, and the result is kept
beside it. Opening a terminal loads that precompiled artifact, which is
the same bargain a shared library makes:

| | 16 plugins |
|---|---|
| compiling them | 792 ms |
| loading the precompiled artifacts | **7 ms** |

So installing all sixteen takes about a second, once, and every terminal
after that pays seven milliseconds. The artifact is named after the
hash of the component and the version of wasmtime that produced it, so
an upgrade recompiles instead of loading something it cannot use, and
uninstalling takes it with the plugin.

(If you ever see those figures ten times larger, you are measuring a
debug build of Haven — the compiler doing the work is wasmtime's, and an
unoptimised build of it is five to ten times slower. That mistake cost
an afternoon's worth of wrong conclusions here.)

What a plugin asks for is refused rather than fatal. A plugin granted no
`exec` still has `run` in its vocabulary; calling it returns a sentence
it can put on the status bar. So a plugin that wanted `git` and did not
get it tells you so instead of disappearing.

## Writing one

See [writing and publishing them](./writing-plugins.md). The short
version is that a plugin is a `cdylib` implementing one trait, and

```console
$ cargo build --release --target wasm32-wasip2
```

produces the component — no `cargo-component`, no `wasm-tools`.

## The shape of it

```
haven plugin repo add <url> --key <fingerprint>
haven plugin repo update
haven plugin search <text>
haven plugin install <name>
```

If you know apt, you know this. The vocabulary is borrowed on purpose.

## Adding a repository

```console
$ haven plugin repo add https://plugins.example.com/haven \
      --key 'BA20 5FB3 8B5F BA0F  FFB5 9CC1 E1E1 F35F 2B2E D307'
added haven (https://plugins.example.com/haven)
  key E1E1F35F2B2ED307
      Haven Community <plugins@example.com>
  (the name on a key is whatever its owner typed; the fingerprint is the part that matters)
  7 plugins
```

The fingerprint is the one thing worth getting from somewhere other than
the repository — a web page, a mailing list, a colleague. Everything else
can be fetched, because everything else is checked against it.

Paste it however you have it: with spaces, without, upper or lower case,
with a `0x` in front. A **short key id** is refused:

```console
$ haven plugin repo add https://plugins.example.com/haven --key E1E1F35F2B2ED307
haven: `E1E1F35F2B2ED307` is 16 characters; a fingerprint is 40 (or 64), and a
short key id is not enough to pin a key to
```

A 64-bit key id can be collided on a laptop. Accepting one would make the
pin decorative.

The key itself comes from `key.asc` in the repository, or from a file you
pass with `--key-file`. Fetching a key from the thing it authenticates
sounds backwards and is not: a substituted key is caught by the
fingerprint, which is the entire point of having one.

### If you have no fingerprint

You can leave `--key` out. Then the key is imported from the repository
and put in front of you to accept:

```console
$ haven plugin repo add https://plugins.example.com/haven
https://plugins.example.com/haven offers this key:

  BA20 5FB3 8B5F BA0F FFB5  9CC1 E1E1 F35F 2B2E D307
  Haven Community <plugins@example.com>

Nothing has checked this. The name on a key is whatever its owner typed,
and the key was served by the repository it would vouch for — so accepting
it trusts whoever answered just now. Compare the fingerprint against one
from somewhere else (the project's page, a mail, a colleague) if you can.

Haven will pin it: from now on only this key can sign this repository.

accept this key? [y/N]
```

The fingerprint is printed in fours, the way `gpg` prints one, because
the only useful thing to do with it is hold it beside a fingerprint from
somewhere else.

This is `ssh`'s bargain with host keys, and it is worth a great deal more
than nothing: Haven **pins** the key you accept, so only the first
contact was ever exposed. A repository that later changes its mind — new
key, new signature, both perfectly self-consistent — is refused:

```console
$ haven plugin repo update
haven: community: the signature was not made by E1E1F35F2B2ED307 or any of its subkeys
```

But nothing checked that it was the repository talking, so Haven says so,
and keeps saying so:

```console
$ haven plugin repo list
community  https://plugins.example.com/haven
  key E1E1F35F2B2ED307  (never checked)  7 plugins
```

That marker is there for later. "I meant to compare that fingerprint and
didn't" is a thing that happens, and a month afterwards this listing is
the only place it can still be noticed.

A key nobody looked at is not accepted just because it was the only one
on offer, so with nothing attached to the terminal the add stops:

```console
$ haven plugin repo add https://plugins.example.com/haven < /dev/null
haven: there is nobody here to look at that key — pass `--key <fingerprint>` to
check it against one you already have, or `--trust-repo-key` to accept whatever
the repository serves
```

`--trust-repo-key` is for scripts, and means exactly what it says.

### One repository, one key

A key is pinned to the repository it was added with. It is not "a key in
the keyring", and no repository can sign for another:

```console
$ haven plugin repo update
haven: haven: the signature was not made by E1E1F35F2B2ED307 or any of its subkeys
```

That signature can be perfectly valid. `gpg --verify` may well call it a
good signature, by a real key, with a real name on it. It is still not
*this* repository's signature, and that is the distinction. apt spent
years undoing the version of this where any trusted key could sign for
anything.

## What a repository is

Three kinds, chosen by the URL and never by a flag:

| URL | What it is |
|---|---|
| `https://host/path` | a directory served over HTTPS |
| `https://host/x.git`, `git://…`, `ssh://…`, `git+https://…` | a git repository, cloned shallowly and pulled after |
| `/srv/haven`, `file:///srv/haven`, `git+file:///srv/haven` | a directory, or a git repository, on this machine |

A git repository is the obvious first thing — you already have one, and
`plugins.yaml` sits in it beside everything else. An HTTPS directory is
what that becomes when it outgrows git: no git on the client, and it can
be a bucket. A local path is how you try either one out before
publishing, and how an internal repository arrives over a mount.

Plain `http://` is refused. The signature would still be checked, so this
is not the hole it looks like — but over plain HTTP anyone in the path
can keep serving you an *older* index that is still correctly signed, and
Haven has nothing like apt's `Valid-Until` to notice. Every host that can
serve a repository can serve it over TLS.

## Publishing one

Three files, one of them optional:

```
plugins.yaml        the index
plugins.yaml.sig    a detached signature over it
key.asc             your public key (optional, for `repo add` to fetch)
pool/…              the artifacts the index names
```

To publish:

```console
$ gpg --armor --export YOUR_FINGERPRINT > key.asc
$ gpg --detach-sign --armor -o plugins.yaml.sig plugins.yaml
```

That is all. There is no tooling to install and nothing Haven-specific
about the signature — it is an ordinary detached OpenPGP signature, and
anyone can check it with `gpg --verify`.

### `plugins.yaml`

```yaml
repo: haven-community
description: plugins people have written for Haven
plugins:
  - name: gitline
    version: 0.2.1
    summary: the branch and its state, in the status line
    file: pool/gitline-0.2.1.tar.gz
    sha256: ba201fa547336620002fa4c78e684a729c71315ca68d5181d46d5fbdb4e6fceb
    entry: gitline.wasm
    events: [pane-focus, cwd-change]
    capabilities:
      exec: [git]
      fs: ['$CWD:ro']
```

One file rather than apt's `Release` plus `Packages`, because a plugin
repository is small enough that splitting them would buy nothing but a
second signature to check. The signature covers the hashes, and the
hashes cover the artifacts — so one signature over one small file covers
everything you install.

Haven reads a **deliberately small YAML**: mappings, nested blocks, lists
of scalars or of mappings, `[a, b]` on one line, `'single'` and `"double"`
quotes, `#` comments. Anchors, aliases, tags, several documents in one
file, block literals and flow mappings are all errors — and so is an
unknown key:

```console
$ haven plugin repo update
haven: community: the index could not be read: line 8: `shasum` is not something
an index says here (expected one of: name, version, summary, file, sha256, entry,
events, capabilities)
```

A misspelled key that is quietly ignored is the worst available
behaviour: `shasum` for `sha256` leaves an entry that cannot be verified,
from a file that parsed without complaint. Every error names its line.

## What a plugin is allowed to do

Default deny. A plugin gets nothing it did not ask for in the index, and
nothing you did not agree to:

```console
$ haven plugin install urls
urls 1.0.0 wants to:
  - read what your panes are showing

Reading your panes means everything that scrolls through them: command lines,
output, and any password or token that appears there.

allow this? [y/N]
```

`read-pane-content` is the dangerous one and is named first wherever it
appears. `exec` is an argv allowlist and never goes through a shell. `net`
and `fs` are empty unless asked for.

What you granted is written down, so an **update that asks for more has
to ask again** — and only about the difference:

```console
$ haven plugin install gitline      # 0.3.0 now wants the screen too
gitline 0.3.0 wants to:
  - read what your panes are showing
```

`git` was already granted, so it is not asked about again. A version that
gives something up asks nothing at all.

## What is checked, in order

This is the whole trust chain, and each link has a test:

1. **The fingerprint** you pinned, which came from outside the repository.
2. **The signature** over `plugins.yaml`, by that key or one of its
   subkeys — GnuPG signs with a subkey by default, so a verifier that
   only tries the primary rejects almost every real key.
3. **The index**, parsed only after it verifies.
4. **The SHA-256** in the index, against the artifact's bytes.
5. **The archive's members**, before anything is unpacked, and the
   extracted tree afterwards.

Step 5 is there because the hash proves *who wrote* the artifact, never
that what they wrote is safe to extract. A correctly signed repository is
free to publish a tarball whose members are `../../.bashrc`, so member
names are audited first — and then the extracted files are audited again,
because a `tar -tzf` listing shows member *names* and cannot tell you
that `gitline.wasm` is a symlink to `/etc/passwd`. A plugin is files; a
symlink, a fifo or a device node in an artifact is refused.

An index or artifact that fails any of these leaves the last good one in
place. A repository that is down, or being attacked, cannot make Haven
forget what it already had.

## Where things are kept

```
~/.config/haven/plugins/
  sources.yaml                  the repositories, how you added them, and
                                whether their key was ever checked
  keys/<fingerprint>.asc        Haven's own keyring
~/.local/share/haven/plugins/
  installed.yaml                what is installed, and what you granted
  installed/<name>/             the unpacked artifact
  repos/<name>/plugins.yaml     the last index that verified
  repos/<name>/changes.yaml     what each refresh found
```

What you *decided* is configuration and worth backing up. What was
*fetched* is a cache and can be deleted at any time — `haven plugin repo
update` rebuilds it.

The keyring is Haven's own. Adding a plugin repository does not put a key
in the keyring you sign mail with, and Haven is not at the mercy of
what is in there already. Verification is [rpgp], so none of this needs
GnuPG installed; you only need it to *publish*.

`--root <dir>` puts the whole lot somewhere else, which is how you try a
repository out without touching your real configuration.

[rpgp]: https://docs.rs/pgp

## The browser

`Ctrl+A` `u` opens the repositories, what is in them, and what moved —
without leaving the terminal:

```
┌─ plugins · type to search ──────────────────────────────────────────────────────┐
│  community (2 plugins)                    │  gitline 0.3.0                      │
│  ├─gitline  0.2.1 → 0.3.0                 │  the branch and its state, in the   │
│  ├─urls 1.0.0                             │  status line                        │
│  work (2 plugins, key never checked)      │                                     │
│  ├─gitline 0.2.1  installed               │  from community                     │
│  ├─urls 1.0.0                             │  installed: 0.2.1 — 0.3.0 available │
│                                           │                                     │
│                                           │  asks to                            │
│                                           │  - run: git                         │
│                                           │  - files: $CWD:ro                   │
│ haven plugin install gitline · esc                                              │
└─────────────────────────────────────────────────────────────────────────────────┘
```

It opens on the first plugin with an update waiting, because that is the
question people open it to answer. Select a repository instead and the
panel shows its key, whether that key was ever checked, and what the last
few refreshes found:

```
│  community (2 plugins)                    │  community                          │
│  ├─gitline 0.3.0  installed               │  /srv/haven                         │
│  ├─urls 1.0.0                             │                                     │
│                                           │  key E1E1F35F2B2ED307               │
│                                           │                                     │
│                                           │  recent changes                     │
│                                           │  gitline  0.2.1 → 0.3.0             │
```

| Key | Does |
|---|---|
| `j` / `k` or arrows | move (wraps) |
| type anything | filter — a repository's name keeps its plugins |
| `Backspace` | undo a filter character |
| `Esc` or `Enter` | close |

**It only shows.** Installing from here would put a permission prompt
inside a modal drawn over a pane, which is the one place a question about
permissions should not be: it has to be read, and a modal is somewhere
people press keys to make go away. So the bottom line is the command for
whatever is selected, and you run it yourself.

The repositories come from the **daemon**, not from the machine you are
typing on. Over SSH the plugins are on the far machine — that is where
they would run — and a browser showing your local ones would be showing
the wrong thing with no way to tell. Everything in it has already been
verified by the daemon, so the client is handed conclusions: a repository
whose index does not verify appears with `!` against it and the reason in
the panel, rather than as an empty repository.

The change log is Haven's note to itself about indices it had already
verified: the last fifty entries per repository, written by `repo update`,
and nothing is trusted from it.

## Commands

| Command | Does |
|---|---|
| `haven plugin repo add <url> --key <fpr>` | add a repository, pinned to a key you checked |
| `haven plugin repo add <url>` | the same, importing the key from the repository for you to accept |
| `haven plugin repo list` | the repositories, their keys and their state |
| `haven plugin repo remove <name>` | forget it (what you installed from it stays) |
| `haven plugin repo update [<name>]` | fetch and verify indices |
| `haven plugin search [<text>]` | what the repositories offer |
| `haven plugin info <name>` | where it comes from, and what it asks for |
| `haven plugin install <name>...` | fetch, verify, unpack — several at once if you like |
| `haven plugin install --all` | everything the repositories offer |
| `haven plugin remove <name>` | uninstall |
| `haven plugin list` | what is installed, and what has a newer version |
| `Ctrl+A` `u` | all of the above in one box — see [the browser](#the-browser) |

To take the lot:

```console
$ haven plugin install --all
```

It still asks about each plugin that wants something, because "install
everything" is not the same sentence as "let everything read my screen".
Three of the sixteen want exactly that. `--yes` answers for you, and is
what a script should pass; without a terminal to ask in, the command
tells you how many it could not ask about and exits non-zero rather than
reporting success for having installed nothing.

Plugins are loaded when a session is made, so a terminal that is already
open does not have what you just installed. Open a new one, or restart
the daemon.

Removing a repository leaves what you installed from it alone, and says
so — it still works, it just has nowhere to update from.

A plugin offered by two repositories is an error rather than a race:

```console
$ haven plugin install gitline
haven: `gitline` is in more than one repository (community, work) — remove one,
or install from a repository you trust for it
```

Precedence is how you end up with the wrong `gitline`, from the
repository you added for one unrelated thing.
