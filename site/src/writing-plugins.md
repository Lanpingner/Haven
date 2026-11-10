# Writing plugins, and publishing a repository of them

Two halves, and only one of them exists.

**Publishing** is finished and usable today. You can build an artifact,
write an index, sign it, serve it from a directory, a git repository or a
bucket, and have someone install it with `haven plugin install`. Every
command on this page below [Publishing a repository, end to
end](#publishing-a-repository-end-to-end) was run against the real
`haven` binary to write it.

**The plugin itself** does not exist. There is no runtime: nothing loads
a `.wasm` file, nothing calls into it, and `haven-plugin-api` — the guest
SDK that would give you `#[haven::plugin]` and typed events — is not
written. What a plugin's code will look like is [a design, further
down](#what-a-plugins-code-will-look-like-not-implemented), and it is
marked as one, because building against it today means building against
nothing.

So the honest use of this page is: publish the distribution side now, get
the repository and the key and the signing habit in place, and have an
artifact ready. Haven's own ordering is the same one, for the same reason
— a signature added after people are installing plugins is a migration
rather than a feature.

If you are installing plugins rather than writing them, [plugins, and
where they come from](./plugins.md) is the other side of this and covers
the trust model, the consent prompts and where files land. This page
assumes you have read it.

## What you are publishing

A repository is a directory with three files in it, plus the artifacts:

```
plugins.yaml        the index: every plugin, with a hash for each
plugins.yaml.sig    a detached, armoured OpenPGP signature over it
key.asc             your public key (optional)
pool/…              the artifacts the index names
```

`pool/` is a convention and nothing more — `file:` in the index is any
relative path you like. The three filenames are not negotiable; Haven
looks for exactly those.

`key.asc` is optional because `haven plugin repo add --key-file` can take
the key from somewhere else. Publishing it is a kindness: it makes `repo
add` a single command for whoever is adding you. It is not a weakness in
the chain, because the key is checked against the fingerprint the user
pinned, and the fingerprint is the one thing that has to reach them from
somewhere other than you.

### The three transports, and choosing one

Which kind of repository you have is read from the URL, never from a flag:

| What you serve | The URL someone adds |
|---|---|
| a git repository | `https://host/plugins.git`, `git://…`, `ssh://…`, `git+https://…`, `git+ssh://…`, or any URL ending `.git` |
| a directory over HTTPS | `https://host/path` |
| a directory on this machine | `/srv/haven`, `file:///srv/haven` |
| a git repository on this machine | `git+file:///srv/haven.git` |

Start with git. You already have a repository, `plugins.yaml` lives in it
beside the source, and publishing is a push. Haven clones it shallowly
and single-branch, so a repository with history in it is not expensive to
follow.

Move to an HTTPS directory when that outgrows itself: no `git` on the
client, and the whole thing can be a bucket with no server at all. The
cost is that you are serving a directory, so you have to get the paths
right by hand.

A local path is how you test. Everything on this page can be done in a
temporary directory with `--root`, against a repository that is never
published, and that is the right way to find out that your index has a
typo in it.

Plain `http://` is refused, and the reason is worth knowing before you set
up a host: the signature would still be checked, so this is not the hole
it looks like, but over plain HTTP anyone in the path can keep serving an
*older* index that is still correctly signed, and Haven has nothing like
apt's `Valid-Until` to notice. Serve it over TLS.

## Publishing a repository, end to end

A complete worked example. One plugin, one key, a local directory, and
nothing installed but `gpg` and `tar`.

### A key to sign with

```console
$ gpg --quick-generate-key 'Haven Plugins <plugins@example.com>' default default never
$ gpg --list-keys --with-colons | grep ^fpr
fpr:::::::::67F26F42A9BE0656E3E760E0DEBBC75744E9EAF8:
```

That fingerprint is what you publish — on your web page, in your README,
wherever someone looks you up. Verification is [rpgp], not a `gpg` child
process, so nobody needs GnuPG installed to *use* your repository; you
need it to publish one.

[rpgp]: https://docs.rs/pgp

### An artifact

A plugin is a gzipped tar of plain files, with whatever the index names as
`entry` somewhere inside it:

```console
$ mkdir -p build/gitline
$ cp gitline.wasm LICENSE build/gitline/
$ cd build/gitline
$ tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
      -cf - gitline.wasm LICENSE | gzip -n > ~/plugins-demo/pool/gitline-0.2.1.tar.gz
```

`tar -czf` would do. The longer form makes the tarball **reproducible**:
with the member order, the times and the ownership fixed, and `gzip -n` to
keep the filename and timestamp out of the gzip header, building twice
gives byte-identical output and so the same SHA-256. That matters because
the hash is the thing you publish, and a hash that changes when nothing
changed is a hash nobody can check your work against.

Members may be nested in directories if you like. `./`-prefixed names from
`tar -C dir .` are fine too.

### Its hash

```console
$ cd ~/plugins-demo
$ sha256sum pool/gitline-0.2.1.tar.gz
75862154a86f834a482170217599d814bb7ea84ddd2634fbefb1091aa5b6aafd  pool/gitline-0.2.1.tar.gz
```

### The index

```yaml
repo: demo
description: one plugin, published by hand
plugins:
  - name: gitline
    version: 0.2.1
    summary: the branch and its state, in the status line
    file: pool/gitline-0.2.1.tar.gz
    sha256: 75862154a86f834a482170217599d814bb7ea84ddd2634fbefb1091aa5b6aafd
    entry: gitline.wasm
    events: [pane-focus, cwd-change]
    capabilities:
      exec: [git]
      fs: ['$CWD:ro']
```

### The signature, and the key

```console
$ gpg --armor --export 67F26F42A9BE0656E3E760E0DEBBC75744E9EAF8 > key.asc
$ gpg --detach-sign --armor -o plugins.yaml.sig plugins.yaml
```

`--armor` on the signature is required, not stylistic. Haven reads
`plugins.yaml.sig` as armoured text and says so when it is not:

```console
$ haven plugin repo update
haven: demo: plugins.yaml.sig is not an armored signature
```

Sign the index **every time it changes**, and sign it last. An index whose
signature is one edit behind verifies as nothing at all.

### Check it from the other side

Before you publish anything, add your own repository from a throwaway
root and install from it:

```console
$ haven plugin repo add file:///home/you/plugins-demo \
      --key 67F26F42A9BE0656E3E760E0DEBBC75744E9EAF8 \
      --name demo --root /tmp/try
added demo (file:///home/you/plugins-demo)
  key DEBBC75744E9EAF8
      Haven Plugins <plugins@example.com>
  (the name on a key is whatever its owner typed; the fingerprint is the part that matters)
  1 plugin

$ haven plugin install gitline --root /tmp/try
gitline 0.2.1 wants to:
  - run: git
  - files: $CWD:ro

allow this? [y/N] y
installed gitline 0.2.1 from demo
  granted run: git; files: $CWD:ro
  (nothing loads plugins yet: this is the distribution half)
```

`--root /tmp/try` keeps sources, keys and installed plugins out of your
real configuration, so this is safe to repeat and safe to throw away.
`--yes` grants what the plugin asks for without prompting, which is what
you want in CI and not what you want when reading a prompt is the point.

`haven plugin info gitline` is the other thing to look at, because it is
what someone deciding whether to trust you reads:

```console
$ haven plugin info gitline --root /tmp/try
gitline 0.2.1
  the branch and its state, in the status line
  from demo (file:///home/you/plugins-demo)
  artifact pool/gitline-0.2.1.tar.gz  sha256 75862154a86f834a482170217599d814bb7ea84ddd2634fbefb1091aa5b6aafd
  wakes on pane-focus, cwd-change
  asks to run: git; files: $CWD:ro
```

### Publish it

For git, commit and push the four paths. For HTTPS, copy the directory to
wherever it is served, preserving the layout. Then tell people the URL and
the fingerprint, in that order of importance reversed: the fingerprint is
the part that cannot safely come from you over the same channel as
everything else.

### Shipping a new version

Build the artifact, hash it, edit the entry, re-sign:

```console
$ tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
      -cf - gitline.wasm LICENSE | gzip -n > ~/plugins-demo/pool/gitline-0.3.0.tar.gz
$ sha256sum pool/gitline-0.3.0.tar.gz
$ $EDITOR plugins.yaml        # version, file, sha256
$ gpg --detach-sign --armor -o plugins.yaml.sig plugins.yaml
```

One entry per plugin: the index describes what you offer *now*, and a
second entry with the same `name` is refused, pointing at the line the
first one is on. Keeping the old artifact in `pool/` is harmless and
polite, since someone's cached index may still name it.

There is no `haven plugin update` yet. A user upgrades by running
`haven plugin install <name>` again, and there is no lockfile and no
auto-update. That is deliberate for the auto-update and merely unfinished
for the rest.

The one thing to think about when versioning is **capabilities**. What a
user granted is written down, so a version that asks for more has to ask
again — and only about the difference. A `0.3.0` that newly wants
`read-pane-content` stops at a prompt on every machine that had `0.2.1`,
which is correct and which you should expect rather than discover. A
version that gives a capability up asks nothing at all.

## The index, field by field

Top level:

| Key | Required | What it has to be |
|---|---|---|
| `repo` | yes | a name: 1–64 characters of lowercase letters, digits, `-` and `_`, starting with a letter or digit |
| `description` | no | any value |
| `plugins` | yes | a list of entries, possibly `[]` |

`repo` is the repository's name for itself and is shown, not obeyed: the
name a user's machine files you under comes from the URL, or from their
`--name`. It is still restricted like a name, because a name is the sort
of thing that ends up as a directory.

An empty `plugins: []` is not an error. A repository that has published
nothing yet is a normal thing to have added.

Each entry:

| Key | Required | What it has to be |
|---|---|---|
| `name` | yes | the same name rule as `repo`; unique within the index |
| `version` | yes | 1–64 characters of letters, digits and `._+-`, no `..`, not starting with `.` |
| `summary` | no | one line, shown by `search` and `info` |
| `file` | yes | a plain relative path to the artifact, from the repository root |
| `sha256` | yes | exactly 64 hex characters, either case |
| `entry` | yes | a plain relative path *inside* the artifact, to the component the host will load |
| `events` | no | a list of values; empty if absent |
| `capabilities` | no | a block; everything in it defaults to off |

A plain relative path means: no leading `/` or `~`, no `..` or `.` or
empty component, no `\`, no `:`, no NUL, at most 512 characters. The
reason is dull and is the one that matters most: `name`, `version`, `file`
and `entry` all end up as path components on someone else's machine, and
this file is written by whoever runs the repository. `file:
../../../../etc/cron.d/x` has to be refused before anything is fetched
rather than relied on to fail later. The ban on `:` is what also rules out
`C:/x` and `https://elsewhere.example/x.tar.gz` — an index names things
inside itself and nothing else.

`version` is not parsed as semver. Haven compares versions for equality
and prints them, so version however you like; the restriction is only
about what may be a path component.

`events` is not checked against any vocabulary, because the vocabulary
belongs to a runtime that does not exist. `[pane-focus, cwd-change]` and
`[timer:2s]` are both accepted, and so is `[nonsense]`. Treat the plan's
list as the intended set and expect this to tighten once something reads
it.

`capabilities` takes exactly four keys:

| Key | Shape | Means |
|---|---|---|
| `exec` | list of values | programs the plugin may run, as `argv[0]`, never through a shell |
| `net` | list of values | hosts it may reach; empty is denied |
| `fs` | list of values | paths, as `path:ro` or `path:rw` |
| `read-pane-content` | `true` or `false` | whether it may read what panes are showing |

Default deny throughout: a capability you do not write is off, not
inherited and not defaulted to something convenient. Ask for the least you
can, and expect `read-pane-content` to cost you installs — it is named
first in every prompt, and the prompt says plainly that it means passwords
and tokens that scroll through a pane.

An unknown key is an error, at either level:

```console
$ haven plugin repo update
haven: demo: the index could not be read: line 8: `shasum` is not something an index
says here (expected one of: name, version, summary, file, sha256, entry, events,
capabilities)
```

A misspelled key that is quietly ignored is the worst available
behaviour. `shasum` for `sha256` leaves an entry that cannot be verified,
from a file that parsed without complaint. Every error names its line, and
that is most of what makes a hand-written index pleasant to fix.

## The YAML Haven reads, exactly

The index is the one file in Haven that someone else gets to write, and
the parser runs on it before the signature has been checked — on every
refresh, in the daemon. So it is a subset of YAML, written for that, and
you will meet its edges.

What it reads:

| Accepted | Example |
|---|---|
| mappings, one `key: value` per line | `repo: demo` |
| a block under a key with nothing after the colon | `capabilities:` then indented keys |
| sequences of scalars | `- git` |
| sequences of mappings | `- name: gitline` then aligned keys |
| one-line flow sequences | `events: [pane-focus, cwd-change]` |
| plain, `'single'` and `"double"` quoted scalars | `fs: ['$CWD:ro']` |
| `#` comments on their own line, and blank lines | |

What it refuses, each with a line number:

| Refused | Why it is not here |
|---|---|
| anchors `&a`, aliases `*a`, tags `!x` | the rest of YAML is where a parser gets interesting, and interesting is the wrong property for this file |
| flow mappings `{a: b}` | nothing an index says needs one |
| block literals `|` and `>` | likewise |
| several documents, `---`, `...`, `%` directives | an index is one document |
| tabs for indentation | |
| a duplicate key in one block | a silently resolved duplicate is a repository that does not say what it thinks it says |
| a key with nothing after it and nothing indented under it | write `events: []`, not `events:` |
| an empty item in a flow list, `[a, , b]` | |
| nesting more than 32 deep | 2000 nested lines is a cheap stack overflow in a daemon, on bytes nobody has verified yet |
| a value whose first character is `*`, `&`, `!`, `{`, `}`, `]`, `|`, `>`, `` ` `` or `@` | reading it as text would mean Haven and a real YAML parser disagreeing about what your index says — quote it and it is yours |

Four edges that catch people:

**A `#` after a value is not a comment.** It only starts one at the
beginning of a line. Stripping it mid-line would silently change data — a
URL fragment or a colour is exactly the sort of value that has a hash in
it — so the hash stays in the value, and you get this:

```console
$ haven plugin repo update
haven: demo: the index could not be read: line 3: `name: gitline   # the one plugin`
— a name is 1 to 64 characters of lowercase letters, digits, `-` and `_`, starting
with a letter or digit
```

**A capability is `true` or `false` and nothing else.** `yes`, `on`, `y`,
`1` and `True` are all refused. That is YAML's Norway problem, and a
capability is the last place to let a parser guess:

```console
haven: demo: the index could not be read: line 9: `read-pane-content:` is `true` or `false`
```

**A colon inside a value is fine.** The key is split at the first `: `, so
`summary: git: the branch, in the status line` is a summary with a colon
in it, and `fs: [$CWD:ro]` needs no quotes. The fixture quotes `'$CWD:ro'`
out of habit, not necessity.

**Double quotes understand three escapes**: `\\`, `\"` and `\n`. `\u0041`
is an error. Single quotes are literal, as in YAML.

The index must also be UTF-8, and at most 8 MiB — as must the signature
beside it.

## What an artifact may contain

The hash proves *who wrote* the artifact. It never proves that what they
wrote is safe to extract: a correctly signed repository is free to publish
a tarball whose members are `../../.bashrc`. So an artifact is audited
twice — the member list before anything is unpacked, and the extracted
tree afterwards — and both audits are things you can trip over by
accident.

| Refused | Caught by |
|---|---|
| a member name starting with `/` or `~` | the member list |
| a `..` component in a member name | the member list |
| a `\` in a member name | the member list |
| more than 4096 members | the member list |
| an empty archive | the member list |
| a symlink, fifo, socket or device node anywhere in the tree | the extracted tree |
| directories nested more than 32 deep | the extracted tree |
| an `entry` that is not there, or is not a plain file | after extraction, by name |

```console
$ haven plugin install badlink
haven: refused the artifact: `gitline.wasm` — it is a symbolic link, and a plugin
is only ever files

$ haven plugin install noentry
haven: the artifact does not contain `other.wasm`, which its index says to load
```

The second audit exists because the first one cannot do its job alone.
`tar -tzf` prints member *names*: a symlink called `gitline.wasm` pointing
at `/etc/passwd` has a perfectly innocent name, and that is how it was
found — the name audit passed it, `is_file()` followed the link, and the
plugin installed. So the tree is judged after extraction, where a link is
a link whatever this machine's tar prints, and still before anything is
moved into place.

What this means for you as a publisher: **a plugin is files and
directories, and nothing else.** No symlinks for convenience, not even one
pointing inside the artifact. Ship the file twice if you must.

Three more things to know:

- The archive must be **gzipped tar**. It is unpacked with `tar -xzf`, so
  a plain `.tar`, a zip or a zstd tarball fails with tar's own complaint.
  The filename extension is not checked; the format is.
- Ownership, permissions and times in the archive are **not honoured**
  (`--no-same-owner --no-same-permissions --no-overwrite-dir`). None of
  the archive's opinions about those are the installing machine's to
  take. Do not ship something that needs a mode bit.
- An artifact may be at most 128 MiB.

## What a plugin's code will look like (not implemented)

Everything below is from [§15 of the plan](./plan.md). None of it is
built. There is no wasmtime host, no WIT world on disk, no
`haven-plugin-api` crate, and no `xtask plugin-test`. Read it to know
which shape to aim at and which assumptions are safe to make; do not read
it as something you can compile against this afternoon.

The intended world:

```wit
package haven:plugin@0.1.0;

world plugin {
  import terminal;   // read grid cells, cursor, selection, scrollback ranges
  import panes;      // enumerate/split/focus/close/zoom; read titles, cwd, pids
  import actions;    // invoke any registry action; register new ones
  import keys;       // claim bindings & key tables; push/pop modal layers
  import ui;         // status segments, overlays, pickers, popups (widget tree)
  import notify;     // notifications, bell handling
  import store;      // per-plugin scoped KV persistence
  import net;        // opt-in, host-gated, allowlisted
  import proc;       // opt-in, host-gated: spawn with argv, no shell

  export init: func(cfg: config) -> result<_, string>;
  export on-event: func(ev: event) -> list<command>;
  export render: func(ctx: render-ctx) -> widget;   // pure, called on demand
}
```

Three constraints in that shape are the ones that will decide how your
plugin is written, and they are the parts least likely to change:

**`on-event` returns commands; it does not mutate the host.** Effects are
data, handed back and applied by the host. That is what makes a plugin
replayable and testable offline — a recorded event stream in, a list of
commands out, snapshotted — and it is why there is no "do this now"
import in the list above.

**`render` is pure and is called when the host feels like it.** A plugin
cannot drive frames, so no plugin can cause a redraw storm. Build your
widget from the context you are handed, not from state you accumulated
during rendering.

**Events are coalesced and rate-limited per plugin.** `output-chunk`
arrives as a batched row-revision summary, never per byte. Anything that
wants to see every byte a pane emits is not going to get it.

Around that: components run in the daemon, because they need pane state
and have to survive a client detaching. One wasmtime `Store` per
instance, its own linear memory, fuel-metered and epoch-interrupted, so a
runaway plugin is killed rather than tolerated. UI output is forwarded to
clients as widget trees over `haven-proto` — clients never execute plugin
code. The lifecycle is `load → init(caps) → subscribe(events) → …events…
→ shutdown`, with hot reload in dev mode, and a plugin that panics is
unloaded, logged and reported rather than retried in a loop.

The capability names in the index above are the ones the host will
enforce, so `capabilities:` is the one part of this design that is
already real in the file format. Writing an index today that asks for
`exec: [git]` is not speculative; what is speculative is anything that
would use it.

## What is worth doing today

Honestly, this much:

- Set up the repository, the key and the signing step, and publish the
  fingerprint. This is the part that cannot be retrofitted.
- Write the index entry for the plugin you intend to write, including the
  capabilities you expect to need, and run `haven plugin info` on it.
  That is the prompt your users will read, and it is worth seeing early.
- Ship an artifact if you have anything to put in one. It will install,
  verify and unpack, and then sit there.

What is not worth doing is writing the `.wasm`. There is nothing to load
it, the world above is a sketch rather than a file, and the SDK that would
make writing against it bearable does not exist. When the runtime lands,
this page gets the other half.
