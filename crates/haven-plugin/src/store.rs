//! What is installed, and how it got there.
//!
//! The last two links of the chain. A verified index gives a SHA-256 for
//! every artifact, so an install is: fetch the artifact, hash it, compare,
//! and only then unpack it. Nothing is unpacked that the repository did
//! not sign for, and nothing is recorded as installed that was not
//! unpacked.
//!
//! ```text
//! ~/.local/share/haven/plugins/
//!   installed.yaml          name, version, repository, hash, what was granted
//!   installed/<name>/       the unpacked artifact
//! ```
//!
//! The record keeps the granted capabilities because an *update* has to
//! know them. A new version that asks for something the old one did not
//! has to ask again — that is the whole reason a grant is written down
//! rather than inferred from the index, which is the thing that changed.
//!
//! Unpacking shells out to `tar`, and audits the archive first. An
//! artifact is hash-verified before it is opened, which means it is
//! exactly what the repository published — and a repository is free to
//! publish an archive whose members are `../../.bashrc`. The hash proves
//! who wrote it, never that what they wrote is safe to extract.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::fetch::{self, Fetch};
use crate::index::{Capabilities, Plugin};
use crate::repo::{Paths, Source};
use crate::sign;

/// One installed plugin, as `installed.yaml` records it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Record {
    pub name: String,
    pub version: String,
    /// The repository it came from, by source name.
    pub repo: String,
    /// The artifact's hash, from the signed index.
    ///
    /// Which artifact this directory was unpacked from, not a checksum of
    /// the directory: the archive is deleted after unpacking, so nothing
    /// can re-derive this from what is on disk. It is here to tell one
    /// published build from another — `install` uses it to notice that a
    /// re-publish of the same version is a different artifact — and the
    /// loader will want it for the same reason.
    pub sha256: String,
    pub entry: String,
    /// What the user agreed to. Not what the index asks for now — that is
    /// the thing an update changes.
    pub granted: Capabilities,
}

/// Everything installed.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Installed {
    pub list: Vec<Record>,
}

impl Installed {
    pub fn load(paths: &Paths) -> Result<Self, Error> {
        let path = paths.data.join("installed.yaml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(Error::Io { path, what: e.to_string() }),
        };
        Self::parse(&text).map_err(|what| Error::Record { path, what })
    }

    fn parse(text: &str) -> Result<Self, String> {
        let doc = crate::yaml::parse(text).map_err(|e| e.to_string())?;
        let Some(items) = doc.get("plugins").and_then(|n| n.as_list()) else {
            return Err("no `plugins:` list".into());
        };
        let mut list = Vec::new();
        for item in items {
            let get = |key: &str| -> Result<String, String> {
                item.get(key)
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
                    .ok_or_else(|| format!("line {}: an entry needs `{key}:`", item.line))
            };
            let strings = |key: &str| -> Vec<String> {
                item.get("granted")
                    .and_then(|g| g.get(key))
                    .and_then(|n| n.as_list())
                    .map(|l| l.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
                    .unwrap_or_default()
            };
            let granted = Capabilities {
                exec: strings("exec"),
                net: strings("net"),
                fs: strings("fs"),
                read_pane_content: item
                    .get("granted")
                    .and_then(|g| g.get("read-pane-content"))
                    .and_then(|n| n.as_str())
                    == Some("true"),
            };
            list.push(Record {
                name: get("name")?,
                version: get("version")?,
                repo: get("repo")?,
                sha256: get("sha256")?,
                entry: get("entry")?,
                granted,
            });
        }
        Ok(Self { list })
    }

    fn save(&self, paths: &Paths) -> Result<(), Error> {
        let mut out = String::from(
            "# Installed plugins. Written by `haven plugin install`.\n\
             #\n\
             # `granted` is what you agreed to when you installed it. A new\n\
             # version asking for more than this has to ask you again.\n",
        );
        // `plugins:` with nothing under it is not something Haven's own
        // YAML reads — by design, since a key with no value is a mistake
        // in an index. Removing the last plugin used to write exactly
        // that, and the next command could not read the file back.
        if self.list.is_empty() {
            out.push_str("plugins: []\n");
        } else {
            out.push_str("plugins:\n");
        }
        for r in &self.list {
            out.push_str(&format!(
                "  - name: {}\n    version: {}\n    repo: {}\n    sha256: {}\n    entry: {}\n",
                r.name, r.version, r.repo, r.sha256, r.entry
            ));
            out.push_str("    granted:\n");
            out.push_str(&format!(
                "      read-pane-content: {}\n",
                if r.granted.read_pane_content { "true" } else { "false" }
            ));
            for (key, values) in
                [("exec", &r.granted.exec), ("net", &r.granted.net), ("fs", &r.granted.fs)]
            {
                let items: Vec<String> = values.iter().map(|v| quoted(v)).collect();
                out.push_str(&format!("      {key}: [{}]\n", items.join(", ")));
            }
        }
        std::fs::create_dir_all(&paths.data)
            .map_err(|e| Error::Io { path: paths.data.clone(), what: e.to_string() })?;
        let path = paths.data.join("installed.yaml");
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, out.as_bytes())
            .map_err(|e| Error::Io { path: tmp.clone(), what: e.to_string() })?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::Io { path, what: e.to_string() })
    }

    pub fn get(&self, name: &str) -> Option<&Record> {
        self.list.iter().find(|r| r.name == name)
    }

    /// What a plugin would newly be allowed to do, compared with what was
    /// granted when it was installed. Empty means nothing to ask about.
    pub fn newly_asked(&self, plugin: &Plugin) -> Capabilities {
        match self.get(&plugin.name) {
            Some(record) => record.granted.newly_asked(&plugin.capabilities),
            // Nothing granted yet, so everything it asks for is new.
            None => Capabilities::default().newly_asked(&plugin.capabilities),
        }
    }
}

/// A granted capability as the record writes it.
///
/// Escaped, not stripped. This used to delete any `'` in the string,
/// which silently recorded a grant other than the one agreed to — and
/// then `newly_asked` saw the difference and asked again on every
/// install. A record of what a user permitted is the last place to
/// quietly alter the text.
fn quoted(text: &str) -> String {
    let plain = !text.is_empty()
        && text.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:+~@$".contains(c))
        && !text.starts_with('-');
    if plain {
        text.to_string()
    } else {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Where one plugin's files live.
pub fn plugin_dir(paths: &Paths, name: &str) -> PathBuf {
    paths.data.join("installed").join(name)
}

/// Fetch, verify and unpack one plugin.
///
/// `granted` is what the caller has agreed with the user. This function
/// does not prompt — a library that asks questions cannot be driven from a
/// test, a script or the command palette, and the whole point of the
/// capability diff is that the *caller* knows how to ask.
pub fn install(
    source: &Source,
    plugin: &Plugin,
    granted: &Capabilities,
    fetcher: &dyn Fetch,
    paths: &Paths,
) -> Result<Record, Error> {
    let work = paths.clone_dir(&source.name);
    let artifact = fetcher
        .file(&source.url, &plugin.file, &work, fetch::MAX_ARTIFACT)
        .map_err(Error::Fetch)?;
    // The link between the signature and the bytes. Everything after this
    // line is operating on what the repository signed for.
    sign::check_sha256(&artifact, &plugin.sha256).map_err(Error::Sign)?;

    let staging = paths.data.join("installed").join(format!(".{}.new", plugin.name));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| Error::Io { path: staging.clone(), what: e.to_string() })?;

    let outcome = unpack(&artifact, &staging, plugin);
    if outcome.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    outcome?;

    let final_dir = plugin_dir(paths, &plugin.name);
    let _ = std::fs::remove_dir_all(&final_dir);
    std::fs::rename(&staging, &final_dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staging);
        Error::Io { path: final_dir.clone(), what: e.to_string() }
    })?;

    let record = Record {
        name: plugin.name.clone(),
        version: plugin.version.clone(),
        repo: source.name.clone(),
        sha256: plugin.sha256.clone(),
        entry: plugin.entry.clone(),
        granted: granted.clone(),
    };
    let mut installed = Installed::load(paths)?;
    installed.list.retain(|r| r.name != record.name);
    installed.list.push(record.clone());
    installed.list.sort_by(|a, b| a.name.cmp(&b.name));
    installed.save(paths)?;
    Ok(record)
}

/// Unpack an artifact into a directory that is ours.
fn unpack(artifact: &[u8], into: &Path, plugin: &Plugin) -> Result<(), Error> {
    let archive = into.join(".artifact");
    std::fs::write(&archive, artifact)
        .map_err(|e| Error::Io { path: archive.clone(), what: e.to_string() })?;

    audit(&archive)?;

    let out = Command::new("tar")
        .args(["-xzf"])
        .arg(&archive)
        .arg("-C")
        .arg(into)
        .args([
            // None of the archive's opinions about ownership, permissions
            // or times are ours to honour.
            "--no-same-owner",
            "--no-same-permissions",
            "--no-overwrite-dir",
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::Missing("tar")
            } else {
                Error::Io { path: archive.clone(), what: e.to_string() }
            }
        })?;
    let _ = std::fs::remove_file(&archive);
    if !out.status.success() {
        return Err(Error::Unpack(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }

    // What `tar -tzf` cannot show. It prints member *names*, so a member
    // that is a symlink is indistinguishable from a file of the same name
    // — and a symlink called `gitline.wasm` pointing at `/etc/passwd` has
    // a perfectly innocent name. Found exactly that way: the name audit
    // passed it, `is_file()` followed it, and the plugin installed.
    //
    // So the tree is judged after extraction, where a link is a link
    // whatever tar's listing format happens to be on this machine. It is
    // still before anything is moved into place.
    audit_tree(into, 0)?;

    // The index said which component the host loads. If the archive does
    // not contain it, the install has failed even though tar succeeded —
    // better caught now than when something tries to load it.
    //
    // `symlink_metadata`, not `is_file`: the latter follows the link that
    // the walk above has just refused to allow.
    let entry = into.join(&plugin.entry);
    let is_regular =
        std::fs::symlink_metadata(&entry).map(|m| m.file_type().is_file()).unwrap_or(false);
    if !is_regular {
        return Err(Error::NoEntry { entry: plugin.entry.clone() });
    }
    Ok(())
}

/// Refuse anything in the extracted tree that is not a plain file or a
/// directory.
///
/// A plugin is some files. A symlink, a hardlink, a fifo, a socket or a
/// device node in an artifact is either a way out of the directory or a
/// way to make something else read or write where it should not, and no
/// plugin needs one. Refusing the whole category is both safer and far
/// easier to be sure of than deciding which links point somewhere
/// acceptable.
fn audit_tree(dir: &Path, depth: usize) -> Result<(), Error> {
    if depth > 32 {
        return Err(Error::Unsafe {
            member: dir.display().to_string(),
            why: "the archive nests directories deeper than Haven will walk",
        });
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Io { path: dir.to_path_buf(), what: e.to_string() })?;
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Io { path: dir.to_path_buf(), what: e.to_string() })?;
        let path = entry.path();
        let kind = std::fs::symlink_metadata(&path)
            .map_err(|e| Error::Io { path: path.clone(), what: e.to_string() })?
            .file_type();
        if kind.is_dir() {
            audit_tree(&path, depth + 1)?;
            continue;
        }
        // A hard link extracts as an ordinary regular file — `file_type`
        // cannot tell you it is one — so it is caught by its link
        // count instead. Nothing a plugin ships has a reason to be
        // linked twice, and the alternative is trusting that this
        // machine's `tar` refuses a link target outside the archive.
        // GNU tar does; bsdtar and busybox are their own programs,
        // and not relying on that is the whole point of auditing.
        if kind.is_file() {
            use std::os::unix::fs::MetadataExt;
            let links = std::fs::symlink_metadata(&path)
                .map(|m| m.nlink())
                .map_err(|e| Error::Io { path: path.clone(), what: e.to_string() })?;
            if links > 1 {
                return Err(Error::Unsafe {
                    member: shown_under(dir, &path),
                    why: "it is a hard link, and a plugin is only ever files",
                });
            }
            continue;
        }
        {
            let why = if kind.is_symlink() {
                "it is a symbolic link, and a plugin is only ever files"
            } else {
                "it is not a plain file, and a plugin is only ever files"
            };
            return Err(Error::Unsafe { member: shown_under(dir, &path), why });
        }
    }
    Ok(())
}

/// A member's path as the archive wrote it, for an error message: the part
/// below the staging directory, which is meaningless to the reader.
fn shown_under(dir: &Path, path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.strip_prefix(dir).unwrap_or(path).display().to_string())
}

/// Refuse an archive that reaches outside the directory it is unpacked in.
///
/// The hash check proves the archive is the one the repository signed for.
/// It says nothing about whether the repository is friendly, and `tar`'s
/// own protections differ between GNU tar, bsdtar and whatever busybox
/// provides. So the member list is read first and judged here, where the
/// rule is written down once and tested.
fn audit(archive: &Path) -> Result<(), Error> {
    let out = Command::new("tar").arg("-tzf").arg(archive).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Missing("tar")
        } else {
            Error::Io { path: archive.to_path_buf(), what: e.to_string() }
        }
    })?;
    if !out.status.success() {
        return Err(Error::Unpack(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let mut members = 0usize;
    for member in listing.lines().map(str::trim).filter(|m| !m.is_empty()) {
        members += 1;
        if members > 4096 {
            return Err(Error::Unsafe {
                member: member.to_string(),
                why: "the archive has more than 4096 files in it",
            });
        }
        let bad = member.starts_with('/')
            || member.starts_with("~")
            || member.contains('\\')
            || member.split('/').any(|part| part == "..")
            // Only if this tar prints verbose listings; most do not, which
            // is why `audit_tree` exists below. Cheap to keep.
            || member.contains(" -> ");
        if bad {
            return Err(Error::Unsafe {
                member: member.to_string(),
                why: "it names a path outside the plugin's own directory",
            });
        }
    }
    if members == 0 {
        return Err(Error::Unpack("the archive is empty".into()));
    }
    Ok(())
}

/// Forget a plugin and delete its files.
pub fn remove(name: &str, paths: &Paths) -> Result<Record, Error> {
    let mut installed = Installed::load(paths)?;
    let at = installed
        .list
        .iter()
        .position(|r| r.name == name)
        .ok_or_else(|| Error::NotInstalled { name: name.to_string() })?;
    let record = installed.list.remove(at);
    // The record goes first: a directory left behind is untidy, a record
    // of a plugin whose files are gone is a load failure at startup.
    installed.save(paths)?;
    let dir = plugin_dir(paths, name);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::Io { path: dir, what: e.to_string() }),
    }
    Ok(record)
}

#[derive(Debug)]
pub enum Error {
    Io { path: PathBuf, what: String },
    Record { path: PathBuf, what: String },
    Fetch(fetch::Error),
    Sign(sign::Error),
    Unpack(String),
    Unsafe { member: String, why: &'static str },
    NoEntry { entry: String },
    Missing(&'static str),
    NotInstalled { name: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, what } => write!(f, "{}: {what}", path.display()),
            Self::Record { path, what } => write!(f, "{}: {what}", path.display()),
            Self::Fetch(e) => write!(f, "{e}"),
            Self::Sign(e) => write!(f, "{e}"),
            Self::Unpack(what) => write!(f, "the artifact could not be unpacked: {what}"),
            Self::Unsafe { member, why } => {
                write!(f, "refused the artifact: `{member}` — {why}")
            }
            Self::NoEntry { entry } => {
                write!(f, "the artifact does not contain `{entry}`, which its index says to load")
            }
            Self::Missing(tool) => write!(f, "unpacking needs `{tool}`, which is not installed"),
            Self::NotInstalled { name } => write!(f, "`{name}` is not installed"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Index;
    use std::collections::HashMap;

    const REPO_FPR: &str = "ba205fb38b5fba0fffb59cc1e1e1f35f2b2ed307";

    #[derive(Debug, Default)]
    struct StubRepo {
        files: HashMap<String, Vec<u8>>,
    }

    impl Fetch for StubRepo {
        fn sync(&self, _url: &str, _work: &Path) -> Result<(), fetch::Error> {
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

    /// A temporary home that cleans itself up even when a test panics.
    struct TempHome {
        root: PathBuf,
        paths: Paths,
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn temp_paths(tag: &str) -> TempHome {
        let root = std::env::temp_dir().join(format!(
            "haven-store-{tag}-{}-{:?}",
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
            trust: crate::repo::Trust::Fingerprint,
        }
    }

    /// Build a real `.tar.gz` holding the named files, and an index entry
    /// whose `sha256` is that archive's. Everything an install needs,
    /// without a network.
    fn artifact(tag: &str, files: &[(&str, &[u8])]) -> (Vec<u8>, String) {
        let dir = std::env::temp_dir().join(format!(
            "haven-artifact-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            let full = dir.join(name);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, body).unwrap();
        }
        let tarball = dir.with_extension("tar.gz");
        let names: Vec<&str> = files.iter().map(|(n, _)| *n).collect();
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&dir)
            .args(&names)
            .output()
            .expect("tar");
        assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
        let bytes = std::fs::read(&tarball).unwrap();
        let hash = sign::sha256(&bytes);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&tarball);
        (bytes, hash)
    }

    /// A gzipped tar holding one member, with whatever name is asked for.
    ///
    /// Written by hand because GNU `tar` will not *create* the archive
    /// this test needs: asked for `../escaped.wasm` it strips the `../`
    /// and warns. A hostile repository is under no such constraint — a tar
    /// header is 512 bytes of mostly padding and anyone can emit one — so
    /// the test has to emit one too, or it proves nothing.
    fn hostile_tar(name: &str, link_to: Option<&str>, body: &[u8]) -> Vec<u8> {
        hostile_tar_typed(name, link_to.map(|t| (b'2', t)), body)
    }

    /// The same, with the tar type flag chosen: `2` is a symlink, `1` a
    /// hard link, `0` a plain file.
    fn hostile_tar_typed(name: &str, link: Option<(u8, &str)>, body: &[u8]) -> Vec<u8> {
        let link_to = link.map(|(_, t)| t);
        let mut header = [0u8; 512];
        fn put(header: &mut [u8; 512], at: usize, text: &[u8]) {
            header[at..at + text.len()].copy_from_slice(text);
        }
        let h = &mut header;
        put(h, 0, name.as_bytes());
        put(h, 100, b"0000644\0");
        put(h, 108, b"0000000\0");
        put(h, 116, b"0000000\0");
        put(
            h,
            124,
            format!("{:011o}\0", if link_to.is_some() { 0 } else { body.len() }).as_bytes(),
        );
        put(h, 136, b"00000000000\0");
        // The checksum is computed with its own field full of spaces.
        put(h, 148, b"        ");
        h[156] = link.map(|(flag, _)| flag).unwrap_or(b'0');
        if let Some(target) = link_to {
            put(h, 157, target.as_bytes());
        }
        put(h, 257, b"ustar\0");
        put(h, 263, b"00");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        put(h, 148, format!("{sum:06o}\0 ").as_bytes());

        let mut tar = header.to_vec();
        if link_to.is_none() {
            tar.extend_from_slice(body);
            tar.resize(tar.len().next_multiple_of(512), 0);
        }
        // Two empty blocks end an archive.
        tar.extend_from_slice(&[0u8; 1024]);

        let mut child = Command::new("gzip")
            .args(["-n", "-c"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("gzip");
        {
            use std::io::Write;
            child.stdin.take().expect("stdin").write_all(&tar).expect("write");
        }
        let out = child.wait_with_output().expect("gzip");
        assert!(out.status.success());
        out.stdout
    }

    fn plugin_with(hash: &str, entry: &str, caps: Capabilities) -> Plugin {
        let text = format!(
            "repo: haven-test\nplugins:\n  - name: gitline\n    version: 0.2.1\n    \
             file: pool/gitline.tar.gz\n    sha256: {hash}\n    entry: {entry}\n"
        );
        let mut plugin = Index::parse(&text).expect("fixture index").plugins.remove(0);
        plugin.capabilities = caps;
        plugin
    }

    fn repo_with(artifact: &[u8]) -> StubRepo {
        StubRepo {
            files: [("pool/gitline.tar.gz".to_string(), artifact.to_vec())].into_iter().collect(),
        }
    }

    #[test]
    fn installing_unpacks_what_the_index_signed_for_and_records_it() {
        let home = temp_paths("install");
        let paths = &home.paths;
        let (bytes, hash) = artifact("ok", &[("gitline.wasm", b"\0asm fake")]);
        let plugin = plugin_with(&hash, "gitline.wasm", Capabilities::default());
        let record = install(&source(), &plugin, &plugin.capabilities, &repo_with(&bytes), paths)
            .expect("should install");

        assert_eq!(record.version, "0.2.1");
        assert_eq!(record.repo, "haven-test");
        assert!(plugin_dir(paths, "gitline").join("gitline.wasm").is_file(), "not unpacked");
        // The archive itself is not left lying in the plugin's directory.
        assert!(!plugin_dir(paths, "gitline").join(".artifact").exists());

        let installed = Installed::load(paths).expect("load");
        assert_eq!(installed.get("gitline"), Some(&record));
    }

    #[test]
    fn an_artifact_that_is_not_the_one_the_index_named_is_never_unpacked() {
        let home = temp_paths("wrong-hash");
        let paths = &home.paths;
        let (bytes, _) = artifact("swap", &[("gitline.wasm", b"the real one")]);
        let (_, other_hash) = artifact("swap2", &[("gitline.wasm", b"something else")]);
        // The index says one hash; the repository serves other bytes. This
        // is a repository that has been compromised *after* signing, or a
        // mirror substituting a file, and it is the case the hashes exist
        // for.
        let plugin = plugin_with(&other_hash, "gitline.wasm", Capabilities::default());
        let e = install(&source(), &plugin, &plugin.capabilities, &repo_with(&bytes), paths)
            .expect_err("must not install");
        assert!(matches!(e, Error::Sign(sign::Error::Hash { .. })), "{e}");
        assert!(!plugin_dir(paths, "gitline").exists(), "something was unpacked anyway");
        assert!(Installed::load(paths).unwrap().list.is_empty(), "it was recorded anyway");
    }

    #[test]
    fn an_archive_that_climbs_out_of_its_directory_is_refused() {
        // A hash-verified artifact is proof of who wrote it, not that what
        // they wrote is safe to extract. This archive is correctly signed
        // for and still must not be unpacked.
        let home = temp_paths("traversal");
        let paths = &home.paths;
        // The absolute member points inside this test's own directory, so
        // a failure is visible without writing anywhere shared.
        let absolute = paths.data.join("escaped.wasm").to_string_lossy().to_string();
        for (name, link, what) in [
            ("../escaped.wasm".to_string(), None, "a member that climbs out"),
            (absolute.clone(), None, "an absolute member"),
            ("a/../../escaped.wasm".to_string(), None, "a member that climbs out in the middle"),
            ("gitline.wasm".to_string(), Some("/etc/passwd"), "a symlink pointing out"),
        ] {
            let bytes = hostile_tar(&name, link, b"nope");
            let hash = sign::sha256(&bytes);
            let plugin = plugin_with(&hash, "gitline.wasm", Capabilities::default());
            let e = install(&source(), &plugin, &plugin.capabilities, &repo_with(&bytes), paths)
                .expect_err(what);
            assert!(matches!(e, Error::Unsafe { .. }), "{what}: {e}");
            assert!(
                !paths.data.join("installed/escaped.wasm").exists()
                    && !paths.data.join("escaped.wasm").exists(),
                "{what}: a file escaped the plugin directory"
            );
        }
    }

    /// A hard link extracts as an ordinary regular file, so `file_type`
    /// cannot see it and the audit has to count links instead. The doc
    /// comment claimed hard links were refused for a while before they
    /// actually were — found by someone reading the two together.
    #[test]
    fn a_hard_link_in_an_artifact_is_refused_though_it_looks_like_a_file() {
        let home = temp_paths("hardlink");
        let paths = &home.paths;
        // A plain file, then a hard link to it: both members are inside
        // the archive, so this is the harmless shape, and it is still
        // refused — the category goes, not the provably-safe instances of
        // it, because deciding which are safe is what goes wrong.
        let mut tar = hostile_tar_typed("real.wasm", None, b"component");
        // Two archives concatenated is a valid tar stream for GNU tar,
        // but building one member at a time is clearer than hand-rolling
        // a two-member archive: `tar` makes the pair for us instead.
        tar.clear();
        let dir = std::env::temp_dir().join(format!(
            "haven-hardlink-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gitline.wasm"), b"component").unwrap();
        std::fs::hard_link(dir.join("gitline.wasm"), dir.join("also.wasm")).unwrap();
        let tarball = dir.join("a.tar.gz");
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&dir)
            .args(["gitline.wasm", "also.wasm"])
            .output()
            .expect("tar");
        assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
        tar.extend_from_slice(&std::fs::read(&tarball).unwrap());
        let _ = std::fs::remove_dir_all(&dir);

        let hash = sign::sha256(&tar);
        let plugin = plugin_with(&hash, "gitline.wasm", Capabilities::default());
        let e = install(&source(), &plugin, &plugin.capabilities, &repo_with(&tar), paths)
            .expect_err("a hard link must be refused");
        assert!(matches!(e, Error::Unsafe { .. }), "{e}");
        assert!(e.to_string().contains("hard link"), "{e}");
        assert!(!plugin_dir(paths, "gitline").exists(), "it was installed anyway");
    }

    /// The tree audit refuses a whole category, so it is worth proving it
    /// does not refuse the ordinary case it has to allow.
    #[test]
    fn an_artifact_with_directories_in_it_installs_normally() {
        let home = temp_paths("nested");
        let paths = &home.paths;
        let (bytes, hash) = artifact(
            "nested",
            &[("gitline.wasm", b"component"), ("assets/icon.svg", b"<svg/>"), ("README", b"hi")],
        );
        let plugin = plugin_with(&hash, "gitline.wasm", Capabilities::default());
        install(&source(), &plugin, &plugin.capabilities, &repo_with(&bytes), paths)
            .expect("a plugin with subdirectories is an ordinary plugin");
        let dir = plugin_dir(paths, "gitline");
        assert!(dir.join("assets/icon.svg").is_file(), "a subdirectory was lost");
        assert!(dir.join("README").is_file());
    }

    #[test]
    fn an_archive_without_the_entry_the_index_promised_is_not_an_install() {
        let home = temp_paths("no-entry");
        let paths = &home.paths;
        let (bytes, hash) = artifact("bare", &[("readme.txt", b"hello")]);
        let plugin = plugin_with(&hash, "gitline.wasm", Capabilities::default());
        let e = install(&source(), &plugin, &plugin.capabilities, &repo_with(&bytes), paths)
            .expect_err("tar succeeds, the install does not");
        assert!(matches!(e, Error::NoEntry { .. }), "{e}");
        assert!(!plugin_dir(paths, "gitline").exists(), "a half-install was left behind");
    }

    #[test]
    fn a_failed_reinstall_does_not_destroy_the_working_one() {
        // Upgrades are where a store earns its keep: the one on disk has
        // to survive a bad new version.
        let home = temp_paths("upgrade");
        let paths = &home.paths;
        let (good, good_hash) = artifact("v1", &[("gitline.wasm", b"version one")]);
        let first = plugin_with(&good_hash, "gitline.wasm", Capabilities::default());
        install(&source(), &first, &first.capabilities, &repo_with(&good), paths).unwrap();

        let (broken, broken_hash) = artifact("v2", &[("readme.txt", b"no component here")]);
        let second = plugin_with(&broken_hash, "gitline.wasm", Capabilities::default());
        assert!(
            install(&source(), &second, &second.capabilities, &repo_with(&broken), paths).is_err()
        );

        assert_eq!(
            std::fs::read(plugin_dir(paths, "gitline").join("gitline.wasm")).unwrap(),
            b"version one",
            "the installed plugin was replaced by a failed install"
        );
        assert_eq!(Installed::load(paths).unwrap().get("gitline").unwrap().version, "0.2.1");
    }

    #[test]
    fn what_was_granted_survives_being_written_and_read_back() {
        let home = temp_paths("grants");
        let paths = &home.paths;
        let caps = Capabilities {
            exec: vec!["git".into()],
            net: vec!["crates.io".into()],
            // A quote in a granted path used to be deleted rather than
            // escaped, so the record said something other than what was
            // agreed to — and the difference then re-prompted on every
            // install of the same version.
            fs: vec!["$CWD:ro".into(), "/tmp/it's there:rw".into()],
            read_pane_content: true,
        };
        let (bytes, hash) = artifact("grant", &[("gitline.wasm", b"x")]);
        let plugin = plugin_with(&hash, "gitline.wasm", caps.clone());
        install(&source(), &plugin, &caps, &repo_with(&bytes), paths).expect("install");

        let installed = Installed::load(paths).expect("load");
        assert_eq!(installed.get("gitline").unwrap().granted, caps, "a grant was lost on disk");
        // And so nothing is newly asked for by the same version.
        assert!(installed.newly_asked(&plugin).is_empty());
    }

    #[test]
    fn an_update_that_asks_for_more_is_a_question_and_asking_for_less_is_not() {
        let home = temp_paths("reconsent");
        let paths = &home.paths;
        let (bytes, hash) = artifact("re", &[("gitline.wasm", b"x")]);
        let modest = Capabilities { exec: vec!["git".into()], ..Default::default() };
        let plugin = plugin_with(&hash, "gitline.wasm", modest.clone());
        install(&source(), &plugin, &modest, &repo_with(&bytes), paths).expect("install");

        let installed = Installed::load(paths).unwrap();
        let greedy = plugin_with(
            &hash,
            "gitline.wasm",
            Capabilities {
                exec: vec!["git".into(), "curl".into()],
                read_pane_content: true,
                ..Default::default()
            },
        );
        let asked = installed.newly_asked(&greedy);
        assert!(asked.read_pane_content, "a new version wanting the screen must be asked about");
        assert_eq!(asked.exec, ["curl"], "git was already granted");

        let smaller = plugin_with(&hash, "gitline.wasm", Capabilities::default());
        assert!(
            installed.newly_asked(&smaller).is_empty(),
            "giving something up is not a question"
        );
    }

    #[test]
    fn a_plugin_nobody_installed_is_not_installed_and_removing_it_says_so() {
        let home = temp_paths("remove");
        let paths = &home.paths;
        let e = remove("nothing", paths).expect_err("not installed");
        assert!(matches!(e, Error::NotInstalled { .. }), "{e}");

        let (bytes, hash) = artifact("rm", &[("gitline.wasm", b"x")]);
        let plugin = plugin_with(&hash, "gitline.wasm", Capabilities::default());
        install(&source(), &plugin, &plugin.capabilities, &repo_with(&bytes), paths).unwrap();
        let gone = remove("gitline", paths).expect("remove");
        assert_eq!(gone.name, "gitline");
        assert!(!plugin_dir(paths, "gitline").exists(), "the files are still there");
        assert!(Installed::load(paths).unwrap().get("gitline").is_none());
    }

    #[test]
    fn nothing_installed_is_not_an_error() {
        let home = temp_paths("fresh");
        let paths = &home.paths;
        assert!(Installed::load(paths).expect("a fresh install").list.is_empty());
    }
}
