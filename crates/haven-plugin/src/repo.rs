//! Repositories: the list of them, the keys they are pinned to, and the
//! refresh that turns one into a verified index.
//!
//! apt's shape, with apt's hard-won rule: a repository is pinned to **one**
//! key, written down when it is added, and only that key may sign that
//! repository's index. A keyring that any repository can sign for is how a
//! repository you added for one plugin gets to replace every other.
//!
//! ```text
//! ~/.config/haven/plugins/
//!   sources.yaml                 the repositories, as added
//!   keys/<fingerprint>.asc       Haven's own keyring, never GnuPG's
//! ~/.local/share/haven/plugins/
//!   repos/<name>/plugins.yaml        the last index that verified
//!   repos/<name>/plugins.yaml.sig    and the signature it verified against
//!   repos/<name>/git/                a clone, for a git repository
//! ```
//!
//! The split is deliberate: what you *decided* is configuration and worth
//! backing up, what was *fetched* is a cache and can be deleted at any
//! time. `haven plugin repo update` rebuilds the second from the first.
//!
//! A refresh writes the index to the cache only after it verifies. A
//! repository that serves a bad signature is left with the index it had,
//! which is the behaviour that matters on a flaky network: the last good
//! answer beats no answer, and an attacker who can break the signature
//! cannot make Haven forget the good one either.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::fetch::{self, Fetch};
use crate::index::Index;
use crate::sign::{self, Key};

/// The file in a repository that holds its index, and the signature over
/// it. Named once, here, because three tools have to agree on them: Haven,
/// whatever publishes a repository, and the documentation.
pub const INDEX_FILE: &str = "plugins.yaml";
pub const SIGNATURE_FILE: &str = "plugins.yaml.sig";
/// Where a repository may publish its own key, for `repo add` to fetch.
/// Fetching it is safe because it is checked against the fingerprint the
/// user pinned; what must come from somewhere else is the fingerprint.
pub const KEY_FILE: &str = "key.asc";

/// Where everything lives. A struct rather than functions so a test can
/// put the whole lot in a temporary directory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Paths {
    /// What the user decided: sources and keys.
    pub config: PathBuf,
    /// What was fetched: indices and clones.
    pub data: PathBuf,
}

impl Paths {
    /// `$XDG_CONFIG_HOME/haven/plugins` and
    /// `$XDG_DATA_HOME/haven/plugins`, with the usual fallbacks.
    pub fn standard() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.clone().map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("haven/plugins");
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("haven/plugins");
        Self { config, data }
    }

    /// Everything under one directory, for tests and for `--root`.
    pub fn under(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self { config: root.join("config"), data: root.join("data") }
    }

    pub fn sources_file(&self) -> PathBuf {
        self.config.join("sources.yaml")
    }

    pub fn keys_dir(&self) -> PathBuf {
        self.config.join("keys")
    }

    pub fn key_file(&self, fingerprint: &str) -> PathBuf {
        self.keys_dir().join(format!("{fingerprint}.asc"))
    }

    /// The cache for one repository. `name` has been through
    /// [`check_name`], so it is a single path component.
    pub fn repo_dir(&self, name: &str) -> PathBuf {
        self.data.join("repos").join(name)
    }

    pub fn index_file(&self, name: &str) -> PathBuf {
        self.repo_dir(name).join(INDEX_FILE)
    }

    pub fn signature_file(&self, name: &str) -> PathBuf {
        self.repo_dir(name).join(SIGNATURE_FILE)
    }

    /// Where a git repository is cloned. Beside the index rather than in
    /// it, so a clone can be deleted without losing the verified index.
    pub fn clone_dir(&self, name: &str) -> PathBuf {
        self.repo_dir(name).join("git")
    }

    /// What each refresh of this repository found.
    pub fn changes_file(&self, name: &str) -> PathBuf {
        self.repo_dir(name).join("changes.yaml")
    }
}

/// A repository as the user added it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Source {
    /// What the user calls it, and the directory its cache lives in.
    pub name: String,
    pub url: String,
    /// The one key that may sign this repository's index, lowercase hex.
    pub fingerprint: String,
    /// Where that fingerprint came from. Recorded because it is the
    /// difference between a key that was checked and a key that was
    /// merely accepted, and only the user can tell them apart later.
    pub trust: Trust,
}

/// How a repository's key came to be trusted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Trust {
    /// The fingerprint was given on the command line and the key was
    /// checked against it. The key could have come from anywhere; the
    /// fingerprint came from outside the repository, which is what makes
    /// the signature mean something.
    #[default]
    Fingerprint,
    /// The key came from the repository and was accepted as it stood.
    ///
    /// Pinned from then on, so only the first contact was ever exposed —
    /// this is `ssh`'s bargain with host keys, and it is worth a great
    /// deal more than nothing. But whoever answered at that moment is
    /// who Haven now trusts, and nothing has checked that it was the
    /// repository. Recorded, and said out loud in `repo list`, because a
    /// user who meant to compare a fingerprint and did not should be able
    /// to find out.
    FirstContact,
}

impl Trust {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fingerprint => "fingerprint",
            Self::FirstContact => "first-contact",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "fingerprint" => Some(Self::Fingerprint),
            "first-contact" => Some(Self::FirstContact),
            _ => None,
        }
    }

    /// Whether this key was ever checked against anything.
    pub fn is_verified(self) -> bool {
        self == Self::Fingerprint
    }
}

/// The sources file: every repository, in the order they were added.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Sources {
    pub list: Vec<Source>,
}

impl Sources {
    /// Read the sources file. A file that is not there is no sources, not
    /// an error — that is the state every new installation is in.
    pub fn load(paths: &Paths) -> Result<Self, Error> {
        let path = paths.sources_file();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(Error::Io { path, what: e.to_string() }),
        };
        Self::parse(&text).map_err(|e| Error::Sources { path: paths.sources_file(), what: e })
    }

    fn parse(text: &str) -> Result<Self, String> {
        let doc = crate::yaml::parse(text).map_err(|e| e.to_string())?;
        let Some(items) = doc.get("repos").and_then(|n| n.as_list()) else {
            return Err("no `repos:` list".into());
        };
        let mut list = Vec::new();
        for item in items {
            let get = |key: &str| -> Result<String, String> {
                item.get(key)
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
                    .ok_or_else(|| format!("line {}: a repository needs `{key}:`", item.line))
            };
            // `trust:` is optional and defaults to the checked case: a
            // sources file written by hand, or by an older build, says a
            // fingerprint was given, because that was the only way to add
            // a repository at all.
            let trust = match item.get("trust").and_then(|n| n.as_str()) {
                None => Trust::default(),
                Some(text) => Trust::parse(text).ok_or_else(|| {
                    format!(
                        "line {}: `trust: {text}` is not one of fingerprint, first-contact",
                        item.line
                    )
                })?,
            };
            // Validated on the way in as well as when it is added: the
            // sources file is editable, and a name is a path component.
            let name = check_name(&get("name")?).map_err(|e| format!("line {}: {e}", item.line))?;
            list.push(Source { name, url: get("url")?, fingerprint: get("key")?, trust });
        }
        Ok(Self { list })
    }

    /// Write the sources file, creating its directory.
    pub fn save(&self, paths: &Paths) -> Result<(), Error> {
        let path = paths.sources_file();
        let mut out = String::from(
            "# Plugin repositories, added with `haven plugin repo add`.\n\
             #\n\
             # `key` is the fingerprint of the one key allowed to sign this\n\
             # repository's index. Changing it by hand changes who Haven trusts.\n\
             #\n\
             # `trust` is how that fingerprint was arrived at: `fingerprint` if you\n\
             # gave it and the key was checked against it, `first-contact` if the key\n\
             # came from the repository and was accepted as it stood.\n",
        );
        // `[]` rather than a key with nothing under it: Haven's own YAML
        // refuses the latter, so removing the last repository would leave
        // a sources file that could not be read back.
        if self.list.is_empty() {
            out.push_str("repos: []\n");
        } else {
            out.push_str("repos:\n");
        }
        for source in &self.list {
            out.push_str(&format!(
                "  - name: {}\n    url: {}\n    key: {}\n    trust: {}\n",
                quoted(&source.name),
                quoted(&source.url),
                quoted(&source.fingerprint),
                source.trust.as_str(),
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io { path: parent.to_path_buf(), what: e.to_string() })?;
        }
        write_atomically(&path, out.as_bytes())
    }

    pub fn get(&self, name: &str) -> Option<&Source> {
        self.list.iter().find(|s| s.name == name)
    }

    /// Add a repository. Refuses a name that is taken, because the name is
    /// the cache directory and how the user names it on the command line.
    pub fn add(&mut self, source: Source) -> Result<(), Error> {
        if let Some(existing) = self.get(&source.name) {
            return Err(Error::Taken { name: source.name.clone(), url: existing.url.clone() });
        }
        self.list.push(source);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Option<Source> {
        let at = self.list.iter().position(|s| s.name == name)?;
        Some(self.list.remove(at))
    }
}

/// A scalar as the sources file writes it: quoted unless it is plainly
/// safe. Writing YAML is easy in a way reading it is not — there is one
/// shape and we chose it — but a URL with a `#` or a leading `-` in it
/// would still come back as something else.
fn quoted(text: &str) -> String {
    let plain = !text.is_empty()
        && text.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:+~@".contains(c))
        && !text.starts_with('-');
    if plain {
        text.to_string()
    } else {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Write a file by renaming one, so an interrupted write cannot leave a
/// half-written sources file or a half-written index.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)
        .map_err(|e| Error::Io { path: tmp.clone(), what: e.to_string() })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::Io { path: path.to_path_buf(), what: e.to_string() }
    })
}

/// Haven's keyring: one armored key per file, named by its fingerprint.
///
/// Its own, not GnuPG's. Adding a plugin repository must not put a key in
/// the keyring the user signs mail with, and Haven must not be at the
/// mercy of what is in there already.
#[derive(Debug)]
pub struct Keyring {
    paths: Paths,
}

impl Keyring {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    /// Store a key, under its own fingerprint.
    pub fn add(&self, key: &Key) -> Result<(), Error> {
        let dir = self.paths.keys_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::Io { path: dir.clone(), what: e.to_string() })?;
        write_atomically(&self.paths.key_file(&key.fingerprint), key.armored().as_bytes())
    }

    /// The key a repository is pinned to, or a readable complaint.
    pub fn get(&self, fingerprint: &str) -> Result<Key, Error> {
        let path = self.paths.key_file(fingerprint);
        let armored = std::fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::NoKey { fingerprint: fingerprint.to_string() }
            } else {
                Error::Io { path: path.clone(), what: e.to_string() }
            }
        })?;
        let key = Key::parse(&armored).map_err(Error::Sign)?;
        // The file is named after a fingerprint; if its contents are a
        // different key, something has rewritten the keyring and the
        // honest thing is to say so rather than to trust the contents.
        if key.fingerprint != fingerprint {
            return Err(Error::KeyringMismatch {
                path,
                expected: fingerprint.to_string(),
                found: key.fingerprint,
            });
        }
        Ok(key)
    }

    pub fn forget(&self, fingerprint: &str) -> Result<(), Error> {
        let path = self.paths.key_file(fingerprint);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io { path, what: e.to_string() }),
        }
    }
}

/// Fetch a repository's key, for `repo add` to show and pin.
///
/// `expect` is the fingerprint the caller already has, if it has one.
/// Given one, fetching the key over the same channel as everything else
/// is safe: a substituted key is caught by the fingerprint, which is the
/// entire point of having one.
///
/// Without one, this returns whatever the repository served. That is not
/// verification and this function cannot make it so — the caller has to
/// put it in front of a person, which is why it is the caller's job and
/// not this function's.
pub fn fetch_key(
    url: &str,
    expect: Option<&str>,
    fetcher: &dyn Fetch,
    paths: &Paths,
    name: &str,
) -> Result<Key, Error> {
    let work = paths.clone_dir(name);
    fetcher.sync(url, &work).map_err(Error::Fetch)?;
    let bytes = fetcher.file(url, KEY_FILE, &work, fetch::MAX_INDEX).map_err(Error::Fetch)?;
    let armored = String::from_utf8(bytes)
        .map_err(|_| Error::Sign(sign::Error::Key(format!("{KEY_FILE} is not text"))))?;
    let key = Key::parse(&armored).map_err(Error::Sign)?;
    if let Some(expect) = expect {
        if key.fingerprint != expect {
            return Err(Error::Sign(sign::Error::WrongKey {
                pinned: expect.to_string(),
                signed_by: key.fingerprint,
            }));
        }
    }
    Ok(key)
}

/// What a refresh found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Refreshed {
    pub index: Index,
    /// Whether the index changed from the one in the cache. `update` says
    /// so, because "nothing new" is the answer most of the time and is
    /// worth being told plainly.
    pub changed: bool,
    /// What moved, if anything did.
    pub changes: Vec<Change>,
}

/// One difference between the index a repository served and the one it
/// served last time.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    pub name: String,
    pub kind: ChangeKind,
    /// Seconds since the epoch, when the refresh that saw it ran.
    pub when: u64,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChangeKind {
    Added {
        version: String,
    },
    /// A version moved. Both ends, because "0.2.1 → 0.3.0" is the useful
    /// form and the old one is gone from the index by the time anyone
    /// reads this.
    Updated {
        from: String,
        to: String,
    },
    /// Still the same version, different bytes. A repository re-publishing
    /// a version is either a mistake or someone going back and changing
    /// what a released version is, and both are worth seeing.
    Republished {
        version: String,
    },
    Removed {
        version: String,
    },
}

impl ChangeKind {
    /// A short line for a list, without the plugin's name — whatever
    /// shows this has the name already.
    pub fn summary(&self) -> String {
        match self {
            Self::Added { version } => format!("new, {version}"),
            Self::Updated { from, to } => format!("{from} → {to}"),
            Self::Republished { version } => format!("{version} republished"),
            Self::Removed { version } => format!("gone, was {version}"),
        }
    }
}

/// What changed between two indices, newest-interesting first.
fn differences(before: Option<&Index>, after: &Index, when: u64) -> Vec<Change> {
    // No "before" is a repository being added, and everything in it being
    // "new" is noise rather than news: there is nothing to have missed.
    let Some(before) = before else { return Vec::new() };
    let mut out = Vec::new();
    for plugin in &after.plugins {
        match before.plugin(&plugin.name) {
            None => out.push(Change {
                name: plugin.name.clone(),
                kind: ChangeKind::Added { version: plugin.version.clone() },
                when,
            }),
            Some(was) if was.version != plugin.version => out.push(Change {
                name: plugin.name.clone(),
                kind: ChangeKind::Updated { from: was.version.clone(), to: plugin.version.clone() },
                when,
            }),
            Some(was) if was.sha256 != plugin.sha256 => out.push(Change {
                name: plugin.name.clone(),
                kind: ChangeKind::Republished { version: plugin.version.clone() },
                when,
            }),
            Some(_) => {}
        }
    }
    for was in &before.plugins {
        if after.plugin(&was.name).is_none() {
            out.push(Change {
                name: was.name.clone(),
                kind: ChangeKind::Removed { version: was.version.clone() },
                when,
            });
        }
    }
    out
}

/// How many changes are kept per repository.
///
/// A log, not a history: it exists so that "what moved?" has an answer
/// after an update, and a list nobody can read to the end answers
/// nothing. Oldest are dropped.
const KEEP_CHANGES: usize = 50;

/// Read the recorded changes for a repository, newest first.
///
/// Best effort: this is a log beside a cache, and a missing or unreadable
/// one is no changes rather than an error. Nothing is trusted from it —
/// it is Haven's own note to itself about indices it had already
/// verified.
pub fn changes(name: &str, paths: &Paths) -> Vec<Change> {
    let Ok(text) = std::fs::read_to_string(paths.changes_file(name)) else { return Vec::new() };
    let Ok(doc) = crate::yaml::parse(&text) else { return Vec::new() };
    let Some(items) = doc.get("changes").and_then(|n| n.as_list()) else { return Vec::new() };
    let mut out = Vec::new();
    for item in items {
        let get = |key: &str| item.get(key).and_then(|n| n.as_str()).unwrap_or("").to_string();
        let when = get("when").parse().unwrap_or(0);
        let version = get("version");
        let kind = match get("kind").as_str() {
            "added" => ChangeKind::Added { version },
            "updated" => ChangeKind::Updated { from: get("from"), to: get("to") },
            "republished" => ChangeKind::Republished { version },
            "removed" => ChangeKind::Removed { version },
            _ => continue,
        };
        let name = get("name");
        if name.is_empty() {
            continue;
        }
        out.push(Change { name, kind, when });
    }
    out.reverse();
    out.truncate(KEEP_CHANGES);
    out
}

/// Append to a repository's change log. Failing to write it does not fail
/// a refresh: the index verified and is cached, which is the part that
/// matters, and a log is a convenience.
fn record_changes(name: &str, new: &[Change], paths: &Paths) {
    if new.is_empty() {
        return;
    }
    // Oldest first on disk, so appending is appending.
    let mut all: Vec<Change> = changes(name, paths);
    all.reverse();
    all.extend_from_slice(new);
    if all.len() > KEEP_CHANGES {
        all.drain(..all.len() - KEEP_CHANGES);
    }
    let mut out = String::from(
        "# What each `haven plugin repo update` found.
changes:
",
    );
    for change in &all {
        out.push_str(&format!(
            "  - name: {}
    when: {}
",
            quoted(&change.name),
            change.when
        ));
        match &change.kind {
            ChangeKind::Added { version } => out.push_str(&format!(
                "    kind: added
    version: {}
",
                quoted(version)
            )),
            ChangeKind::Updated { from, to } => out.push_str(&format!(
                "    kind: updated
    from: {}
    to: {}
",
                quoted(from),
                quoted(to)
            )),
            ChangeKind::Republished { version } => out.push_str(&format!(
                "    kind: republished
    version: {}
",
                quoted(version)
            )),
            ChangeKind::Removed { version } => out.push_str(&format!(
                "    kind: removed
    version: {}
",
                quoted(version)
            )),
        }
    }
    let _ = write_atomically(&paths.changes_file(name), out.as_bytes());
}

/// Fetch a repository's index, verify it, and cache it.
///
/// The order is the whole function: fetch, *then* verify, *then* write.
/// Nothing reaches the cache that has not been signed by the key this
/// repository is pinned to, so everything downstream — `list`, `install` —
/// can read the cache without checking anything again.
pub fn refresh(source: &Source, fetcher: &dyn Fetch, paths: &Paths) -> Result<Refreshed, Error> {
    let keyring = Keyring::new(paths.clone());
    let key = keyring.get(&source.fingerprint)?;

    let work = paths.clone_dir(&source.name);
    fetcher.sync(&source.url, &work).map_err(Error::Fetch)?;
    let index_bytes =
        fetcher.file(&source.url, INDEX_FILE, &work, fetch::MAX_INDEX).map_err(Error::Fetch)?;
    let signature =
        fetcher.file(&source.url, SIGNATURE_FILE, &work, fetch::MAX_INDEX).map_err(Error::Fetch)?;
    let signature = String::from_utf8(signature).map_err(|_| {
        Error::Sign(sign::Error::Signature(format!("{SIGNATURE_FILE} is not an armored signature")))
    })?;

    key.verify(&signature, &index_bytes).map_err(Error::Sign)?;

    // Only now is it text worth parsing.
    let text = String::from_utf8(index_bytes.clone()).map_err(|_| Error::Index {
        name: source.name.clone(),
        what: "the index is not UTF-8".into(),
    })?;
    let index = Index::parse(&text)
        .map_err(|e| Error::Index { name: source.name.clone(), what: e.to_string() })?;

    let dir = paths.repo_dir(&source.name);
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Io { path: dir.clone(), what: e.to_string() })?;
    // The index we are replacing, for the change log. Read through
    // `cached` so a cache that does not verify counts as no cache rather
    // than as a previous state worth diffing against.
    let before = cached(source, paths).ok();
    let changed =
        std::fs::read(paths.index_file(&source.name)).map(|old| old != index_bytes).unwrap_or(true);
    let differences = differences(before.as_ref(), &index, unix_now());

    // The signature first. Either order leaves a window, and this is the
    // one whose failure mode is honest: a new signature over an old index
    // fails to verify, so the repository is simply refreshed again —
    // where a new index under an old signature fails the same way but
    // looks like tampering.
    write_atomically(&paths.signature_file(&source.name), signature.as_bytes())?;
    write_atomically(&paths.index_file(&source.name), &index_bytes)?;
    record_changes(&source.name, &differences, paths);
    Ok(Refreshed { index, changed, changes: differences })
}

/// The cached index for a repository, as the last refresh left it.
///
/// Re-verified on the way in. The cache is a file in the user's own home
/// and nothing else should be writing it — but "nothing else should" is
/// not a signature, and checking costs a millisecond on a file that is
/// already in the page cache.
pub fn cached(source: &Source, paths: &Paths) -> Result<Index, Error> {
    let index_path = paths.index_file(&source.name);
    let bytes = std::fs::read(&index_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NeverRefreshed { name: source.name.clone() }
        } else {
            Error::Io { path: index_path.clone(), what: e.to_string() }
        }
    })?;
    let signature = std::fs::read_to_string(paths.signature_file(&source.name))
        .map_err(|_| Error::NeverRefreshed { name: source.name.clone() })?;
    let keyring = Keyring::new(paths.clone());
    keyring.get(&source.fingerprint)?.verify(&signature, &bytes).map_err(Error::Sign)?;
    let text = String::from_utf8(bytes).map_err(|_| Error::Index {
        name: source.name.clone(),
        what: "the index is not UTF-8".into(),
    })?;
    Index::parse(&text).map_err(|e| Error::Index { name: source.name.clone(), what: e.to_string() })
}

/// Whether a repository may be called this.
///
/// It becomes a directory under `repos/` and the word the user types to
/// name it, so it is held to the same rule as a plugin's name.
pub fn check_name(name: &str) -> Result<String, String> {
    crate::index::check_name(name, "name").map_err(|e| e.message)
}

/// Seconds since the epoch, or zero if the clock is before it.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A name for a repository taken from its URL, for when the user does not
/// give one. `https://github.com/x/haven-plugins.git` becomes
/// `haven-plugins`.
pub fn name_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit('/').find(|part| !part.is_empty())?;
    let stem = last.strip_suffix(".git").unwrap_or(last);
    let cleaned: String = stem
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() || !cleaned.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(cleaned.chars().take(64).collect())
}

#[derive(Debug)]
pub enum Error {
    Io { path: PathBuf, what: String },
    Sources { path: PathBuf, what: String },
    Fetch(fetch::Error),
    Sign(sign::Error),
    Index { name: String, what: String },
    NoKey { fingerprint: String },
    KeyringMismatch { path: PathBuf, expected: String, found: String },
    Taken { name: String, url: String },
    NeverRefreshed { name: String },
    Unknown { name: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, what } => write!(f, "{}: {what}", path.display()),
            Self::Sources { path, what } => write!(f, "{}: {what}", path.display()),
            Self::Fetch(e) => write!(f, "{e}"),
            Self::Sign(e) => write!(f, "{e}"),
            // Not "{name}'s index": every caller that prints this
            // already says which repository it is talking about, and the
            // two together read `demo: demo's index is not usable`.
            Self::Index { name: _, what } => write!(f, "the index could not be read: {what}"),
            Self::NoKey { fingerprint } => write!(
                f,
                "no key {} in Haven's keyring — add the repository again with its key",
                sign::short(fingerprint)
            ),
            Self::KeyringMismatch { path, expected, found } => write!(
                f,
                "{} holds key {}, not {} — something has rewritten the keyring",
                path.display(),
                sign::short(found),
                sign::short(expected)
            ),
            Self::Taken { name, url } => {
                write!(f, "there is already a repository called `{name}` ({url})")
            }
            Self::NeverRefreshed { name } => {
                write!(f, "`{name}` has no index yet — run `haven plugin repo update`")
            }
            Self::Unknown { name } => write!(f, "no repository called `{name}`"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
    const REPO_FPR: &str = "ba205fb38b5fba0fffb59cc1e1e1f35f2b2ed307";

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{FIXTURES}{name}")).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    /// A repository that is a map, so no test needs a network or a disk
    /// layout it did not make itself. `syncs` counts the calls, because
    /// "one pull per refresh, not one per file" is a property worth
    /// pinning.
    #[derive(Debug, Default)]
    struct StubRepo {
        files: HashMap<String, Vec<u8>>,
        syncs: Mutex<usize>,
    }

    impl StubRepo {
        fn with(files: &[(&str, &[u8])]) -> Self {
            Self {
                files: files.iter().map(|(k, v)| (k.to_string(), v.to_vec())).collect(),
                syncs: Mutex::new(0),
            }
        }
    }

    impl Fetch for StubRepo {
        fn sync(&self, _url: &str, _work: &Path) -> Result<(), fetch::Error> {
            *self.syncs.lock().unwrap() += 1;
            Ok(())
        }

        fn file(
            &self,
            _url: &str,
            path: &str,
            _work: &Path,
            limit: u64,
        ) -> Result<Vec<u8>, fetch::Error> {
            let found =
                self.files.get(path).ok_or_else(|| fetch::Error::NotFound(path.to_string()))?;
            if found.len() as u64 > limit {
                return Err(fetch::Error::TooBig { limit });
            }
            Ok(found.clone())
        }
    }

    /// The signed index fixture, as a repository serves it.
    fn signed_repo() -> StubRepo {
        StubRepo::with(&[
            (INDEX_FILE, &fixture("plugins.yaml")),
            (SIGNATURE_FILE, &fixture("plugins.yaml.sig")),
            (KEY_FILE, &fixture("repo-key.asc")),
        ])
    }

    /// A temporary home that cleans itself up, so no test has to remember
    /// to — and so a panicking one does not leave a directory behind.
    pub(super) struct TempHome {
        root: PathBuf,
        pub(super) paths: Paths,
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    pub(super) fn temp_paths(tag: &str) -> TempHome {
        let root = std::env::temp_dir().join(format!(
            "haven-repo-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        TempHome { paths: Paths::under(&root), root }
    }

    fn source() -> Source {
        Source {
            name: "haven-test".into(),
            url: "https://plugins.example.invalid/haven".into(),
            fingerprint: REPO_FPR.into(),
            trust: Trust::Fingerprint,
        }
    }

    fn with_key(paths: &Paths) {
        let key = Key::parse(&String::from_utf8(fixture("repo-key.asc")).unwrap()).unwrap();
        Keyring::new(paths.clone()).add(&key).expect("keyring");
    }

    #[test]
    fn a_refresh_verifies_before_it_caches_anything() {
        let home = temp_paths("refresh");
        let paths = &home.paths;
        with_key(paths);
        let repo = signed_repo();
        let got = refresh(&source(), &repo, paths).expect("should verify and parse");
        assert_eq!(got.index.repo, "haven-test");
        assert!(got.changed, "the first refresh is a change");
        assert_eq!(*repo.syncs.lock().unwrap(), 1, "one pull per refresh, not one per file");

        // Cached, and re-readable without the network.
        let again = cached(&source(), paths).expect("the cache should verify");
        assert_eq!(again, got.index);

        // A second refresh of the same bytes is not a change.
        let second = refresh(&source(), &repo, paths).expect("second refresh");
        assert!(!second.changed, "the same index twice is not news");
    }

    #[test]
    fn an_index_signed_by_another_key_never_reaches_the_cache() {
        // The attack: a repository, or anyone who can answer for it,
        // serves an index signed by a key they do hold.
        let home = temp_paths("wrong-key");
        let paths = &home.paths;
        with_key(paths);
        let repo = StubRepo::with(&[
            (INDEX_FILE, &fixture("plugins.yaml")),
            (SIGNATURE_FILE, &fixture("plugins.yaml.other.sig")),
        ]);
        let e = refresh(&source(), &repo, paths).expect_err("must not verify");
        assert!(matches!(e, Error::Sign(sign::Error::Signature(_))), "{e}");
        assert!(
            !paths.index_file("haven-test").exists(),
            "an index that did not verify was written to the cache anyway"
        );
    }

    #[test]
    fn a_tampered_index_leaves_the_last_good_one_in_place() {
        // What matters on a hostile network is not only that the bad index
        // is refused, but that the good one survives being attacked.
        let home = temp_paths("tampered");
        let paths = &home.paths;
        with_key(paths);
        refresh(&source(), &signed_repo(), paths).expect("first refresh");
        let before = std::fs::read(paths.index_file("haven-test")).unwrap();

        let mut evil = fixture("plugins.yaml");
        evil.extend_from_slice(b"  - name: evil\n    version: 1\n");
        let hostile =
            StubRepo::with(&[(INDEX_FILE, &evil), (SIGNATURE_FILE, &fixture("plugins.yaml.sig"))]);
        assert!(refresh(&source(), &hostile, paths).is_err(), "tampering must be refused");
        assert_eq!(
            std::fs::read(paths.index_file("haven-test")).unwrap(),
            before,
            "the cached index was replaced by one that did not verify"
        );
    }

    #[test]
    fn a_repository_with_no_key_in_the_keyring_says_which_key_it_wants() {
        let home = temp_paths("no-key");
        let paths = &home.paths;
        let e = refresh(&source(), &signed_repo(), paths).expect_err("no key yet");
        let shown = e.to_string();
        assert!(shown.contains("no key"), "{shown}");
        assert!(shown.contains("E1E1F35F2B2ED307"), "it should name the key: {shown}");
    }

    #[test]
    fn a_keyring_file_holding_the_wrong_key_is_not_quietly_used() {
        let home = temp_paths("swapped");
        let paths = &home.paths;
        with_key(paths);
        // Someone replaces the contents of ba20….asc with another key,
        // leaving the filename alone. The filename is not the trust.
        std::fs::write(paths.key_file(REPO_FPR), fixture("other-key.asc")).unwrap();
        let e = refresh(&source(), &signed_repo(), paths).expect_err("swapped key");
        assert!(matches!(e, Error::KeyringMismatch { .. }), "{e}");
    }

    #[test]
    fn fetching_a_repositorys_own_key_is_pinned_by_the_fingerprint() {
        let home = temp_paths("fetch-key");
        let paths = &home.paths;
        let key = fetch_key(
            "https://plugins.example.invalid/haven",
            Some(REPO_FPR),
            &signed_repo(),
            paths,
            "haven-test",
        )
        .expect("the fixture key is the pinned one");
        assert_eq!(key.fingerprint, REPO_FPR);

        // And the case that makes fetching it safe at all.
        let e = fetch_key(
            "https://plugins.example.invalid/haven",
            Some("0000000000000000000000000000000000000000"),
            &signed_repo(),
            paths,
            "haven-test",
        )
        .expect_err("a key that is not the pinned one");
        assert!(matches!(e, Error::Sign(sign::Error::WrongKey { .. })), "{e}");

        // And with no fingerprint to check against, whatever the
        // repository served comes back, for the caller to show to
        // somebody. Nothing here has verified it.
        let unchecked = fetch_key(
            "https://plugins.example.invalid/haven",
            None,
            &signed_repo(),
            paths,
            "haven-test",
        )
        .expect("the repository served a key");
        assert_eq!(unchecked.fingerprint, REPO_FPR);
    }

    #[test]
    fn sources_survive_being_written_and_read_back() {
        let home = temp_paths("sources");
        let paths = &home.paths;
        let mut sources = Sources::default();
        sources.add(source()).expect("add");
        sources
            .add(Source {
                name: "weird".into(),
                // A URL with the characters that would break plain YAML.
                url: "https://x.example/a#b".into(),
                fingerprint: REPO_FPR.into(),
                // And the other way a key gets trusted, so the round trip
                // covers both.
                trust: Trust::FirstContact,
            })
            .expect("add");
        sources.save(paths).expect("save");

        let read = Sources::load(paths).expect("load");
        assert_eq!(read, sources, "a source did not survive the round trip");

        // Adding the same name twice is refused, and says what is there.
        let e = read_back(paths).add(source()).expect_err("duplicate name");
        assert!(e.to_string().contains("already a repository"), "{e}");

        let mut read = read_back(paths);
        assert_eq!(read.remove("haven-test").map(|s| s.name).as_deref(), Some("haven-test"));
        assert!(read.remove("haven-test").is_none(), "removing twice removes nothing");
    }

    fn read_back(paths: &Paths) -> Sources {
        Sources::load(paths).expect("load")
    }

    #[test]
    fn no_sources_file_is_no_repositories_and_not_an_error() {
        let home = temp_paths("empty");
        let paths = &home.paths;
        assert!(Sources::load(paths).expect("a fresh install").list.is_empty());
    }

    #[test]
    fn a_repository_is_named_after_its_url_when_nobody_names_it() {
        for (url, want) in [
            ("https://github.com/x/haven-plugins.git", Some("haven-plugins")),
            ("https://github.com/x/haven-plugins", Some("haven-plugins")),
            ("https://plugins.example.com/haven/", Some("haven")),
            ("git://example.com/Some.Repo.git", Some("some-repo")),
            ("/srv/local-repo", Some("local-repo")),
            ("https://example.com/", Some("example-com")),
            ("https://example.com/---", None),
        ] {
            assert_eq!(name_from_url(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn an_index_that_verifies_but_does_not_parse_is_the_repositorys_fault_by_name() {
        // A signature over nonsense is still a valid signature. The error
        // has to say which repository, or a user with six of them has to
        // guess.
        let home = temp_paths("bad-index");
        let paths = &home.paths;
        with_key(paths);
        let repo = StubRepo::with(&[
            (INDEX_FILE, b"repo: haven-test\nplugins: not-a-list\n"),
            (SIGNATURE_FILE, &fixture("plugins.yaml.sig")),
        ]);
        // The fixture signature does not cover these bytes, so this also
        // shows the order: verification comes first, parsing second.
        let e = refresh(&source(), &repo, paths).expect_err("should fail");
        assert!(matches!(e, Error::Sign(_)), "verification must come before parsing: {e}");
    }
}

#[cfg(test)]
mod change_tests {
    use super::tests::temp_paths;
    use super::*;
    use crate::index::Index;

    fn index_of(entries: &[(&str, &str, &str)]) -> Index {
        let mut text = String::from("repo: haven-test\nplugins:\n");
        for (name, version, hash) in entries {
            text.push_str(&format!(
                "  - name: {name}\n    version: {version}\n    \
                 file: pool/{name}.tar.gz\n    sha256: {}\n    entry: {name}.wasm\n",
                hash.repeat(64 / hash.len())
            ));
        }
        Index::parse(&text).unwrap_or_else(|e| panic!("fixture: {e}"))
    }

    #[test]
    fn adding_a_repository_is_not_fifty_pieces_of_news() {
        // Everything in a repository you have just added is "new", which
        // is noise: there was nothing to have missed.
        let after = index_of(&[("gitline", "0.2.1", "a"), ("urls", "1.0.0", "b")]);
        assert!(differences(None, &after, 7).is_empty());
    }

    #[test]
    fn a_refresh_reports_what_actually_moved() {
        let before =
            index_of(&[("gitline", "0.2.1", "a"), ("urls", "1.0.0", "b"), ("old", "1", "c")]);
        let after = index_of(&[
            // unchanged
            ("urls", "1.0.0", "b"),
            // a version moved
            ("gitline", "0.3.0", "a"),
            // same version, different bytes
            ("new", "2.0.0", "d"),
        ]);
        let changes = differences(Some(&before), &after, 99);
        let find = |name: &str| {
            changes
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("no {name}: {changes:?}"))
        };
        assert_eq!(
            find("gitline").kind,
            ChangeKind::Updated { from: "0.2.1".into(), to: "0.3.0".into() }
        );
        assert_eq!(find("new").kind, ChangeKind::Added { version: "2.0.0".into() });
        assert_eq!(find("old").kind, ChangeKind::Removed { version: "1".into() });
        assert!(changes.iter().all(|c| c.name != "urls"), "unchanged is not a change");
        assert!(changes.iter().all(|c| c.when == 99), "every change is stamped");
    }

    #[test]
    fn a_version_republished_under_the_same_number_is_news_too() {
        // The same version with different bytes is either a mistake or
        // somebody changing what a released version is. Both are worth
        // seeing, and a version comparison alone would miss it.
        let before = index_of(&[("gitline", "0.2.1", "a")]);
        let after = index_of(&[("gitline", "0.2.1", "f")]);
        let changes = differences(Some(&before), &after, 1);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Republished { version: "0.2.1".into() });
    }

    #[test]
    fn the_change_log_survives_a_round_trip_and_stays_bounded() {
        let home = temp_paths("changelog");
        let paths = &home.paths;
        std::fs::create_dir_all(paths.repo_dir("haven-test")).unwrap();

        let one = vec![
            Change {
                name: "gitline".into(),
                kind: ChangeKind::Added { version: "1".into() },
                when: 10,
            },
            Change {
                name: "urls".into(),
                kind: ChangeKind::Updated { from: "1".into(), to: "2".into() },
                when: 10,
            },
        ];
        record_changes("haven-test", &one, paths);
        let read = changes("haven-test", paths);
        assert_eq!(read.len(), 2);
        // Newest first, which is the order anything showing these wants.
        assert_eq!(read[0].name, "urls");
        assert_eq!(read[0].kind.summary(), "1 → 2");
        assert_eq!(read[1].kind.summary(), "new, 1");

        // Appending keeps the old ones...
        record_changes(
            "haven-test",
            &[Change {
                name: "third".into(),
                kind: ChangeKind::Removed { version: "9".into() },
                when: 20,
            }],
            paths,
        );
        let read = changes("haven-test", paths);
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].name, "third");

        // ...up to a point. A log nobody can read to the end answers
        // nothing, so the oldest go.
        let many: Vec<Change> = (0..KEEP_CHANGES + 20)
            .map(|i| Change {
                name: format!("p{i}"),
                kind: ChangeKind::Added { version: "1".into() },
                when: 30,
            })
            .collect();
        record_changes("haven-test", &many, paths);
        let read = changes("haven-test", paths);
        assert_eq!(read.len(), KEEP_CHANGES);
        assert_eq!(read[0].name, format!("p{}", KEEP_CHANGES + 19), "newest kept");
    }

    #[test]
    fn a_missing_or_broken_change_log_is_no_changes_rather_than_an_error() {
        let home = temp_paths("nolog");
        let paths = &home.paths;
        assert!(changes("never-refreshed", paths).is_empty());
        std::fs::create_dir_all(paths.repo_dir("broken")).unwrap();
        std::fs::write(paths.changes_file("broken"), b"this is not yaml: [[[\n").unwrap();
        assert!(changes("broken", paths).is_empty(), "a broken log must not be fatal");
    }
}
