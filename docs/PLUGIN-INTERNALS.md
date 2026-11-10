# `haven-plugin` internals

For someone about to change the plugin subsystem, review a patch to it, or build
the WASM runtime on top of it. The user-facing half is `site/src/plugins.md`;
the authoring half is `site/src/writing-plugins.md`. This file is about the
code: how it is put together, why, what a change must not break, and where the
sharp edges are.

What exists today is the **distribution** half of §15 of `docs/PLAN.md` — get a
plugin from a repository onto disk, having proved where it came from, and
record what the user agreed to let it do. Nothing loads a plugin yet. That
ordering was deliberate (§15.4): a signature is a feature before anybody has
installed a plugin and a migration afterwards.

Six modules, ~3,600 lines including their tests, 58 unit tests, plus 9
end-to-end tests in `crates/haven/tests/plugin_cli.rs` that drive the shipped
binary. `cargo test -p haven-plugin` runs in well under a second and touches no
network.

| Module | Owns | Does not |
|---|---|---|
| `yaml.rs` | A strict YAML subset, with a line number on every node | Anchors, aliases, tags, flow mappings, multiple documents, block scalars |
| `sign.rs` | OpenPGP key parsing, detached-signature verification, fingerprint normalisation, SHA-256 | Any I/O, any knowledge of where keys live |
| `index.rs` | The `plugins.yaml` schema and all of its validation; `Capabilities` and the grant diff | Decide whether an index was verified |
| `fetch.rs` | The `Fetch` trait, URL classification, and three transports over `git`/`curl`/the filesystem | Decide whether what came back is trustworthy |
| `repo.rs` | `Paths`, `Source`/`Sources`, `Trust`, `Keyring`, `refresh`, `cached` | Prompt, or write anything that did not verify |
| `store.rs` | Install, the two archive audits, `Installed` records and grants | Prompt, or decide what the user consented to |

`crates/haven/src/plugin.rs` is the CLI layer and the only place a question is
put to a person.

---

## 1. The trust chain

In order. A reviewer's first job on any patch here is to check that the order
has not moved, because every link is only worth anything if the links above it
ran first.

| # | Link | Enforced by | Where |
|---|---|---|---|
| 1 | The URL is one of three kinds, is not `http://`, and cannot be read as an argument by `git` or `curl` | `fetch::kind_of` | `fetch.rs` |
| 2 | The keyring file named `<fingerprint>.asc` actually contains that key | `repo::Keyring::get` → `Error::KeyringMismatch` | `repo.rs` |
| 3 | The key vouches for its own subkeys | `sign::Key::parse` → `verify_bindings` | `sign.rs` |
| 4 | A detached signature over the **index bytes**, made by the pinned primary or one of its subkeys | `sign::Key::verify` | `sign.rs` |
| 5 | The index is parsed only after link 4 passed | the statement order of `repo::refresh`, and again in `repo::cached` | `repo.rs` |
| 6 | Every name, version and path in the index is path-safe | `index::name_like`, `version_like`, `relative_path`, `hash_like` | `index.rs` |
| 7 | The artifact's bytes are the ones the signed index named | `sign::check_sha256`, called by `store::install` before anything is created | `store.rs` |
| 8 | No archive **member name** reaches outside the plugin's directory | `store::audit`, before `tar -x` runs | `store.rs` |
| 9 | The **extracted tree** is only plain files and directories, and the `entry` is a regular file | `store::audit_tree`, then the `symlink_metadata` check in `store::unpack` | `store.rs` |
| 10 | Only now is the staging directory renamed into place and a `Record` written | `store::install` | `store.rs` |

Link 0, which the library cannot enforce, is consent: `store::install` takes
`granted: &Capabilities` and believes it. The diff that says what is new is
`index::Capabilities::newly_asked` (via `store::Installed::newly_asked`); the
asking is `haven::plugin::agreed`. A patch that computes a grant inside the
library without a caller having agreed to it has broken the one link that is
held outside the code.

Two things to notice about the shape:

**Link 4 covers one small file, and link 7 is what extends that to every
download.** One signature over one index that carries a SHA-256 per artifact is
apt's `Release`+`Packages` folded together (§15.4). This means artifacts are
never signed individually and must never be fetched without a verified index in
hand. `store::install` takes an `&Plugin`, which can only have come from
`Index::parse`, which `repo` only calls after verifying — that type is the only
thing carrying the invariant, so a new caller that constructs a `Plugin` by hand
(as the `store` tests do, deliberately) is outside the chain.

**`repo::cached` re-verifies.** The cache is a file in the user's own home and
nothing else should be writing it, but "nothing else should" is not a signature,
and the check costs a millisecond on a page-cached file. Every read path —
`search`, `info`, `install`, `list` — goes through `cached`, so there is no
code path that reads an index without a signature check.

---

## 2. Invariants

Each of these has a test that pins it. If you break one, that is the test that
should go red; if you change one deliberately, that is the test to rewrite with
a reason.

1. **Nothing is parsed before it is verified.** `repo::refresh` fetches, calls
   `key.verify`, and only then converts to `String` and calls `Index::parse`.
   Pinned by `repo::tests::an_index_that_verifies_but_does_not_parse_is_the_repositorys_fault_by_name`,
   which serves unparseable bytes under a signature that does not cover them and
   asserts the error is a `Sign` error, not an `Index` one.
2. **Nothing reaches the index cache unless it verified.**
   `an_index_signed_by_another_key_never_reaches_the_cache`.
3. **A failed refresh leaves the previous good state intact.** The writes happen
   after verification, so a failure returns before them.
   `a_tampered_index_leaves_the_last_good_one_in_place`, and end-to-end in
   `plugin_cli::a_changed_index_is_refused_and_the_last_good_one_survives`.
4. **A failed install leaves the previous good install intact.** Unpacking goes
   into `installed/.<name>.new` and is renamed over the real directory only on
   success; the `Record` is written only after the rename.
   `store::tests::a_failed_reinstall_does_not_destroy_the_working_one` and
   `an_archive_without_the_entry_the_index_promised_is_not_an_install`.
5. **One `sync` per refresh, not one per file.** `Fetch::sync` brings a git
   clone up to date; `Fetch::file` must not. Pinned by a counter in the stub:
   `repo::tests::a_refresh_verifies_before_it_caches_anything` asserts
   `syncs == 1` after a refresh that reads two files. This is not only about
   speed: a pull between two file reads would mean the index and the signature
   could come from different commits.
6. **`store::install` does not `sync`.** It reads the artifact out of the same
   clone the verified index came from. Syncing there would let the clone move
   past the index whose hash we are about to check. (The hash check would catch
   it, as a confusing error; not syncing means it cannot arise.)
7. **A pin is to one key, and only that key signs that repository.** Not "a key
   in the keyring" — apt's long-undone mistake.
   `sign::tests::a_signature_from_another_key_is_not_this_repositorys` uses a
   second *real* key whose signature really does verify, which is the only
   version of that test worth having.
8. **A short key id is never accepted as a fingerprint.** 40 or 64 hex digits
   only; 64-bit key ids are forgeable.
   `sign::tests::a_short_key_id_is_refused_rather_than_padded`.
9. **Default deny on capabilities.** Absent means off, everywhere: `index.rs`
   defaults, `Installed::parse` reads a missing `read-pane-content` as false.
   `index::tests::what_is_not_asked_for_is_not_granted`.
10. **An update asks only about the difference, and asking for less asks
    nothing.** `index::tests::only_the_newly_asked_capabilities_need_asking_about`.
11. **The library never prompts and never reads stdin.** See §3.
12. **An unknown key in an index is an error.** `shasum:` for `sha256:` must not
    parse into an entry with no hash. `known_keys` in `index.rs`;
    `a_misspelled_key_is_an_error_and_not_a_silence`.
13. **The parser terminates on hostile input.** `yaml::MAX_DEPTH`. See §4.

---

## 3. Why the library never talks to a user

`haven-plugin` contains no `print!`, no `eprintln!` and no `is_terminal`, and
nothing outside one test's `gzip` pipe touches a standard stream. Every question
lives in `crates/haven/src/plugin.rs`:
`accepted()` for a repository key taken on first contact, `agreed()` for a
capability grant.

This is not tidiness. It is what makes the subsystem testable and scriptable:

- The capability diff is computed by the library and *presented* by the caller,
  so the same decision can be made by a CLI prompt today, by the command
  palette later, and by `--yes` in CI, with no second implementation of "what
  is new here".
- Every test in the crate can drive an install to completion without a tty.
  The CLI tests run with `stdin` on `/dev/null` precisely so that a prompt
  would *fail* rather than hang
  (`plugin_cli::a_plugin_that_wants_your_screen_is_not_installed_without_an_answer`).
- The daemon will eventually install plugins with no terminal attached at all.
  A library that writes to stdout in that context is writing into a pane.

A patch that adds a `print!` to `haven-plugin` — even a `--verbose` one, even a
progress bar — is a design regression. The shape to use instead is what
`Refreshed { index, changed }` already does: return the facts, let the caller
decide whether anybody wants to hear them.

The corollary is the one genuine limit of the library: it cannot tell whether
the `granted` it was handed was agreed to by anyone. Keep that boundary narrow
and obvious.

---

## 4. Why the YAML is hand-rolled and strict

`plugins.yaml` is the one file in Haven that an attacker gets to write. It is
signed — but the parser runs on whatever arrives, *before* anything has been
verified, and it runs on every refresh. So the parser is the subset in
`yaml.rs`: mappings, nested blocks, sequences of scalars or mappings, one-line
flow sequences, three kinds of scalar, comments. Everything else is an error
that names its line.

Three consequences worth defending against "simplification":

**`MAX_DEPTH = 32`.** The parser recurses once per level, and nothing in the
file bounds the number of levels: 2000 nested levels is 2000 short lines, each
one space further in, which costs an attacker nothing to generate. Without a
cap that is a stack overflow — in the daemon, on unverified bytes, which is to
say a one-line denial of service by anyone who can answer an HTTP request. The
test `yaml::tests::nothing_here_recurses_without_bound` was written in the
belief that indentation bounded the recursion; its first run overflowed the
stack. An index uses four levels. If you find `MAX_DEPTH` inconvenient, the
answer is almost certainly that your schema is wrong.

**No sigils in plain scalars.** A plain scalar beginning `*`, `&`, `!`, `{`,
`|`, `>`, `` ` `` or `@` is refused rather than read as text (`plain_sigil`).
The reason is disagreement: if Haven reads `*other` as the literal string and a
real YAML parser reads it as an alias, then Haven and the repository's own
tooling disagree about what the signed file says. Refusing means a repository
has to quote it, which is always available.

**A duplicate key is an error, and `Value::Map` is a `Vec`, not a map.** Silent
last-wins resolution of a duplicated `sha256:` is exactly the bug class this
file exists to avoid. Lookup cost does not come into it; an index has a handful
of keys.

Refusing a document is always safe here: the file is fetched again on the next
refresh, and a repository that cannot write this much YAML has made a mistake
worth being told about. That is why every node carries its line, and why
`index.rs` can complain about a hash on line 8 rather than about the file.

Note the asymmetry, and keep it: **`index.rs` is strict because a repository
wrote the file; `Sources::parse` and `Installed::parse` are lenient because
Haven wrote it.** The latter two reuse the same parser but tolerate missing
fields with defaults.

---

## 5. Why the archive is audited twice

A hash proves *who* wrote an artifact. It never proves that what they wrote is
safe to extract — a repository is perfectly free to sign an archive whose
members are `../../.bashrc`, and a compromised repository will.

So `store::unpack` does three things in order:

1. `audit(&archive)` reads `tar -tzf` and judges the **member names**: no
   leading `/` or `~`, no `..` component, no backslash, no ` -> `, at most 4096
   members, not empty. This happens *before* extraction, because a name that
   escapes must never be written at all.
2. `tar -xzf … --no-same-owner --no-same-permissions --no-overwrite-dir` into
   the staging directory. None of the archive's opinions about ownership, mode
   or time are ours to honour.
3. `audit_tree(into)` walks what was actually created and refuses anything that
   is not a plain file or a directory.

Step 3 exists because step 1 cannot see it. `tar -tzf` prints member *names*,
so a member that is a symlink is indistinguishable from a file of the same name
— and a symlink called `gitline.wasm` pointing at `/etc/passwd` has a perfectly
innocent name. That case installed cleanly: the name audit passed it, the
`is_file()` check followed the link and said yes, and the plugin was recorded as
installed. The fix is in two parts: judge the tree after extraction, where a
link is a link whatever `tar`'s listing format happens to be on this machine,
and use `symlink_metadata` rather than `is_file` for the `entry` check. Both
still happen before anything is renamed into place.

Both audits are load-bearing and neither subsumes the other:

- Only step 1 can stop a traversal, because by step 3 an escaped file is already
  outside the directory being walked.
- Only step 3 can stop a link, a fifo, a socket or a device node.

`store::tests::an_archive_that_climbs_out_of_its_directory_is_refused` covers
four cases — `../escaped.wasm`, an absolute member, `a/../../escaped.wasm`, and
a symlink member — and asserts both that the install failed and that nothing
appeared outside the plugin directory. It needs its own tar writer
(`hostile_tar`) because GNU `tar` will not *create* an archive with a `../`
member: it strips the prefix and warns. A hostile repository is under no such
constraint — a tar header is 512 bytes of mostly padding — so a test that used
`tar -c` would prove nothing.

`an_artifact_with_directories_in_it_installs_normally` is the other half:
`audit_tree` refuses a whole category, so it is worth proving it does not refuse
the ordinary case.

---

## 6. `Fetch` as the testing seam

```rust
pub trait Fetch: fmt::Debug {
    fn sync(&self, url: &str, work: &Path) -> Result<(), Error>;
    fn file(&self, url: &str, path: &str, work: &Path, limit: u64) -> Result<Vec<u8>, Error>;
}
```

Two methods, because that is all the three transports have in common: one
clones, one makes a request, one reads a directory. `Tools` is the real
implementation and dispatches on `kind_of(url)`. Every function that reaches the
outside takes `&dyn Fetch`.

This is why no test in the crate touches the network:

- `repo.rs` and `store.rs` both define a `StubRepo` that is a `HashMap<String,
  Vec<u8>>`. `repo`'s also counts `sync` calls, because "one pull per refresh"
  is a property, not an implementation detail.
- `fetch.rs` tests the real `Tools` against **a real git repository on disk** —
  a local path is a perfectly good git remote — and asserts that a second
  `sync` is a pull that picks up a new commit. It skips itself if `git` is
  absent.
- `crates/haven/tests/plugin_cli.rs` runs the shipped binary against
  `crates/haven-plugin/tests/fixtures`, which *is* a repository: a real
  `plugins.yaml`, a detached signature GnuPG actually made, `key.asc`,
  `other-key.asc`, a second signature by that other key, and real tarballs in
  `pool/`. An absolute path is one of the three kinds of URL, so the fixture
  directory needs no server. Tests that need to tamper copy the tree to a
  mirror and repoint `sources.yaml` — which is what a hostile mirror or a bad
  DNS answer amounts to — rather than writing to the fixtures.

The fixtures are made by GnuPG itself (`--quick-generate-key`, `--armor
--export`, `--detach-sign --armor`) because the thing being tested is
interoperation with what people have on their machines, not with rpgp's own
output. They live in `haven-plugin` and are used from both crates: one
repository with two readers beats two that drift.

`kind_of` deserves its own mention as the whole argument-injection surface for
two child processes. It refuses control characters, a leading `-`, a relative
`file://`, and `http://` with a reason (the signature would still hold, but
there is no `Valid-Until` here, so anyone in the path could hold you on an old
index). `git_sync` and `curl` both also pass `--`, and `git` is invoked with
`core.hooksPath=/dev/null`, an empty `core.askPass` and an empty
`credential.helper` — a clone must not run anything and must never block on a
password prompt in a daemon.

---

## 7. What the dependencies cost

Two: `pgp` (rpgp) 0.21 and `sha2` 0.10. Together they pull **147 crates** into
`cargo tree -p haven-plugin` and add about **+1.9 MB** to the release binary
(11.8 → 13.7 MB).

That was judged worth it exactly once, for one reason: a terminal should not
need GnuPG installed to update a plugin, and shelling out to `gpg` would mean
parsing its output and living with whatever is in the user's personal keyring.
Haven keeps its own keyring file and never touches the one the user signs mail
with.

The bar for the next dependency in this crate is that argument or better. Note
what is *not* here: no HTTP client, no git implementation, no tar crate, no
serde, no YAML crate. Those are `curl`, `git` and `tar` as child processes —
the same decision Haven makes for `docker`, `man` and `xdg-open` — and a
hand-rolled parser, and the reasons are in §4 and §6. "It would be fewer lines
with `serde_yaml`" is answered by §4: the lines are not the point, the subset
is.

---

## 8. State on disk

```text
~/.config/haven/plugins/             what you decided
  sources.yaml                       repositories, as added (name, url, key, trust)
  keys/<fingerprint>.asc             Haven's own keyring
~/.local/share/haven/plugins/        what was fetched
  repos/<name>/plugins.yaml          the last index that verified
  repos/<name>/plugins.yaml.sig      and the signature it verified against
  repos/<name>/git/                  a clone, for a git repository
  installed.yaml                     name, version, repo, hash, what was granted
  installed/<name>/                  the unpacked artifact
```

The split is the point: config is worth backing up, data can be deleted at any
time and `haven plugin repo update` rebuilds it. `Paths::under(root)` puts the
whole lot in one directory, which is what `--root` and every test use.

`Source::trust` records *how* a fingerprint was arrived at —
`Trust::Fingerprint` when the user supplied it (or a key file) and the key was
checked against it, `Trust::FirstContact` when the key came from the repository
and was accepted as it stood. The second is ssh's bargain with host keys: pinned
from then on, so only the first contact was ever exposed, and worth a great deal
more than nothing — but nothing checked that whoever answered was the
repository. `repo list` prints `(never checked)` every time, not just at add
time, because "I meant to compare that fingerprint and didn't" is a thing that
happens and a month later the listing is the only place it can still be noticed.
`trust:` is optional when reading, and defaults to `Fingerprint`, because that
was the only way to add a repository before the field existed.

Writing: `repo::write_atomically` is write-to-`.tmp`-then-`rename`, so an
interrupted write cannot leave a half-written sources file or index.
`Installed::save` does the same by hand. Both writers emit `repos: []` /
`plugins: []` rather than a bare `key:` when the list is empty, because Haven's
own YAML refuses a key with nothing under it — removing the last entry used to
write a file the next command could not read back.

---

## 9. Sharp edges

Known, in the code as it stands. The first three are open and worth fixing; the
rest are decisions or closed traps recorded so nobody reopens them. None are
hidden.

- **Nothing bounds the *extracted* size of an artifact.** `MAX_ARTIFACT` is
  128 MiB of compressed bytes and `audit` caps the member count at 4096, but
  neither bounds what gzip expands to; a one-megabyte artifact can fill a disk.
  A hostile repository needs a valid signature over it to get that far, which is
  the mitigation, not an argument against a cap.
- **Hard links are refused by their link count, not their file type.** This is
  worth not "simplifying": a hard link extracts as an ordinary regular file, so
  `file_type().is_file()` is true for one, and `audit` never sees a link
  *target* because `tar -tzf` prints names only. So `audit_tree` additionally
  rejects any regular file whose `nlink > 1`. GNU tar 1.35 happens to defend
  this itself — it strips `/` and `../` from hard link targets — but "tar's own
  protections differ between GNU tar, bsdtar and busybox" is the stated reason
  `audit_tree` exists at all, so leaning on that would contradict the module's
  own argument. Pinned by
  `a_hard_link_in_an_artifact_is_refused_though_it_looks_like_a_file`, which
  fails if the `nlink` check is removed. The doc comment claimed this for a
  while before the code did; two reviewers found the gap by reading the comment
  against the code.
- **`Record::sha256` is the artifact's hash, and cannot be re-derived.** It
  identifies which published build a directory came from — `install` uses it to
  notice that a re-publish of the same version is different bytes — but it is
  the hash of the *archive*, which `unpack` deletes, so nothing on disk can be
  checked against it. `list` does not try. If the loader ever wants to verify
  what is installed, it needs a hash of the extracted tree, which is not
  recorded today.
- **The index and its signature are two separate atomic writes.** A crash
  between them leaves a new index beside an old signature; `cached` then fails
  with a signature error rather than `NeverRefreshed`, which reads as an attack
  rather than as a torn write. A single directory rename would fix it.
- **No locking.** Two `haven plugin` processes refreshing the same repository
  share a `.tmp` path, and two concurrent installs do a read-modify-write of
  `installed.yaml` in which one record is lost. Fine for a CLI, not fine once
  the daemon installs plugins.
- **A granted capability is escaped when written, never stripped.** This used
  to be `v.replace('\'', "")`, which recorded a grant other than the one
  agreed to — and `newly_asked` then saw the difference and re-prompted on
  every install of the same version. Silent mutation of a security-relevant
  record. `store::quoted` now double-quotes anything unusual, and the grant
  round-trip test carries a path with a quote in it.
- **`curl --max-filesize` is not a memory bound on its own.** It does not stop a
  chunked response whose length is unknown up front on curl before 8.4, which is
  why `fetch::curl` re-checks `out.len()` afterwards — by which point the bytes
  are in memory. The local and git transports stat before reading
  (`read_capped`), so they have no equivalent window.
- **A repository's name is validated like a plugin's**, by `repo::check_name`,
  both when `--name` supplies it and when `Sources::parse` reads it back. It
  becomes a directory under `repos/`, so it has to be a single path component;
  for a while `Paths::repo_dir`'s comment asserted this and nothing enforced
  it, and `--name ../../x` put the cache elsewhere.

---

## 10. Where the runtime attaches

§15.1–15.3 of `docs/PLAN.md` specify a wasmtime host with the Component Model,
one `Store` per plugin, fuel-metered and epoch-interrupted, running **in the
daemon** because plugins need pane state and must survive client detach. None of
it is built.

What `haven-plugin` already hands it, per installed plugin:

| The runtime needs | It has | From |
|---|---|---|
| A plugin that is what its repository published | `store::Record { name, version, repo, sha256 }` | `installed.yaml` |
| A directory of its files, containing only plain files and directories | `store::plugin_dir(paths, name)` | `installed/<name>/` |
| The component to instantiate | `Record::entry`, already checked to exist as a regular file at install time | the index |
| What it is allowed to do | `Record::granted: Capabilities` — what the *user agreed to*, not what the index asks for now | `installed.yaml` |
| Which events to subscribe it to | `index::Plugin::events` | the index |

Two gaps in that table the runtime will feel immediately:

- **`events` is not in the `Record`.** It lives in the index, so the runtime
  either reads the (verified) cached index for a repository that may since have
  been removed, or `Record` grows the field. The latter is a two-line change to
  `Installed::parse`/`save` plus the writer in `install`; do it before the
  loader, not after.
- **`Capabilities` is carried and compared, never interpreted.** `exec` is argv
  allowlist strings, `net` is hostnames, `fs` is `path:ro`/`path:rw` with
  `$CWD` unexpanded. Nothing parses or enforces any of it. The host owns that,
  and the moment it does, `index.rs` should validate the *syntax* of each so a
  malformed `fs` entry is a line number rather than a runtime surprise.

Also missing, and specified: `haven plugin update` for in-place upgrading (today
`install` of a newer version upgrades it, correctly re-prompting only about new
capabilities, but nothing walks every installed plugin), and a lockfile. No
auto-update, by design. There is no loader, no `haven-plugin-api` guest SDK and
no `xtask plugin-test`.

The thing the runtime must not do: trust `installed/<name>/` because it is on
disk. It is on disk because the chain in §1 ran once. A plugin directory whose
contents were replaced afterwards is indistinguishable to the loader, and the
honest place to fix that is a hash of the `entry` in the `Record` (see §9), not
in the loader.

---

## 11. How to extend it

**Adding a transport.** Add a variant to `fetch::Kind`, teach `kind_of` to
recognise it — and nothing else, because a URL already says which kind it is;
a flag that overrides the URL would mean two sources of truth about where a
repository lives. Then implement the two `Fetch` methods for it in `Tools`.
Obligations: a `kind_of` case in
`a_url_says_which_kind_of_repository_it_is`; a case in
`a_url_that_could_be_an_argument_is_not_a_url` for whatever argument injection
the new tool is vulnerable to; a size ceiling that is enforced *before* the
bytes are in memory if the transport allows it; and a one-line row in the table
at the top of `fetch.rs` and in the `USAGE` text.

**Adding an index field.** Add it to the `allowed` list in the relevant
`known_keys` call — an unlisted key is an error, so forgetting this is a
loud failure rather than a silent one — then parse it with `field`/`optional`/
`strings`/`boolean`, and validate it. If it will ever become a path component,
it goes through `relative_path` or `name_like`; if it is a hash, `hash_like`.
Obligations: a parse assertion in `an_index_reads_as_what_it_says`, a refusal
test with the line number it reports, and the field in the example at the top of
`index.rs` and in the user-facing docs. If the runtime needs the field after
install, it also goes in `store::Record` and both ends of `Installed`.

**Adding a capability.** This is the expensive one, and all four steps are
required:

1. `index::Capabilities`: the field, defaulting to denied; `parse_capabilities`
   and its `known_keys`; `is_empty` keeps working via `PartialEq` on the
   default.
2. `Capabilities::lines()` — the sentence a person is shown. If it is more
   dangerous than `exec`, it goes *above* `read-pane-content`; the prompt leads
   with the worst thing.
3. `Capabilities::newly_asked` — the grant diff. A new field that is not added
   here is a capability an update can acquire without asking. This is the single
   most review-worthy line in such a patch.
4. `Installed::parse` and `Installed::save` — the grant record. A field written
   but not read back reads as "not granted" on the next command, which is the
   safe direction but will re-prompt forever; a field read but not written is
   worse.

Plus: the words in `haven::plugin::agreed` if it needs explaining rather than
listing, and tests at both ends — `only_the_newly_asked_capabilities_need_asking_about`
and `what_was_granted_survives_being_written_and_read_back`.

**A general note on patches here.** The error types are deliberately specific
(`sign::Error::WrongKey` versus `Signature`, `store::Error::Unsafe` versus
`Unpack`) because the tests match on them: `matches!(e, Error::Sign(_))` is how
`an_index_that_verifies_but_does_not_parse…` proves the *order* of two
operations. Collapsing an error type into a `String` deletes that proof.
