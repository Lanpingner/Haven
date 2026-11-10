//! `plugins.yaml`: what a repository says it has.
//!
//! This is apt's `Release` and `Packages` folded into one file, because a
//! plugin repository is small enough that splitting them would buy nothing
//! but a second signature to check. One file, one detached signature, and
//! a SHA-256 for every artifact it lists — so the signature covers the
//! hashes and the hashes cover the downloads.
//!
//! ```yaml
//! repo: haven-community
//! description: plugins people have written for Haven
//! plugins:
//!   - name: gitline
//!     version: 0.2.1
//!     summary: the branch and its state, in the status line
//!     file: pool/gitline-0.2.1.tar.gz
//!     sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
//!     entry: gitline.wasm
//!     events: [pane-focus, cwd-change]
//!     capabilities:
//!       exec: [git]
//!       fs: ["$CWD:ro"]
//! ```
//!
//! Everything is validated here, with the line it went wrong on, and
//! unknown keys are an error rather than something to ignore. An index is
//! written by hand far more often than it is generated, and a key that is
//! quietly dropped — `shasum` for `sha256`, `files` for `file` — is a
//! repository that looks fine and installs nothing.
//!
//! The validation that matters most is the dull kind. A `name`, a `version`
//! and a `file` all end up as path components, and this file is written by
//! whoever runs the repository: `file: ../../../../etc/cron.d/x` has to be
//! refused *here*, before anything is fetched, rather than relied on to
//! fail later.

use std::fmt;

use crate::yaml::{self, Node};

/// A verified index, parsed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Index {
    /// The repository's own name for itself. Shown, and used as the
    /// directory the index is cached in, so it is restricted like a name.
    pub repo: String,
    pub description: Option<String>,
    pub plugins: Vec<Plugin>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plugin {
    pub name: String,
    pub version: String,
    pub summary: Option<String>,
    /// Where the artifact is, relative to the repository root.
    pub file: String,
    /// SHA-256 of the artifact, lowercase hex.
    pub sha256: String,
    /// The component inside the artifact that the host loads.
    pub entry: String,
    pub events: Vec<String>,
    pub capabilities: Capabilities,
    /// The line in the index this entry starts on, for error messages
    /// about it later.
    pub line: usize,
}

/// What a plugin asks to be allowed to do.
///
/// Default deny: everything here is empty or false unless the index says
/// otherwise, and what the index says is what the user is asked to grant.
/// The host enforces it; this type only carries it and compares it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Capabilities {
    /// Programs the plugin may run, as argv[0]. Never through a shell.
    pub exec: Vec<String>,
    /// Hosts it may reach. Empty is denied, which is the default.
    pub net: Vec<String>,
    /// Paths it may read or write, `path:ro` or `path:rw`.
    pub fs: Vec<String>,
    /// Whether it may read what is on the screen. The dangerous one:
    /// passwords and tokens scroll through a terminal, so it is off unless
    /// asked for and granted, and it is named first in any prompt.
    pub read_pane_content: bool,
}

impl Capabilities {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The lines to show a user who is being asked to grant these.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        // First, because it is the one that matters most and a prompt
        // people skim should lead with it.
        if self.read_pane_content {
            out.push("read what your panes are showing".to_string());
        }
        if !self.exec.is_empty() {
            out.push(format!("run: {}", self.exec.join(", ")));
        }
        if !self.net.is_empty() {
            out.push(format!("reach: {}", self.net.join(", ")));
        }
        if !self.fs.is_empty() {
            out.push(format!("files: {}", self.fs.join(", ")));
        }
        out
    }

    /// What `new` asks for that `self` did not.
    ///
    /// An update that newly wants to read your screen needs asking again,
    /// and an update that wants *less* needs no prompt at all. Only the
    /// additions are a question.
    pub fn newly_asked(&self, new: &Self) -> Self {
        let added = |was: &[String], now: &[String]| -> Vec<String> {
            now.iter().filter(|x| !was.contains(x)).cloned().collect()
        };
        Self {
            exec: added(&self.exec, &new.exec),
            net: added(&self.net, &new.net),
            fs: added(&self.fs, &new.fs),
            read_pane_content: new.read_pane_content && !self.read_pane_content,
        }
    }
}

impl fmt::Display for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lines = self.lines();
        if lines.is_empty() {
            return write!(f, "nothing");
        }
        write!(f, "{}", lines.join("; "))
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Error {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for Error {}

impl From<yaml::Error> for Error {
    fn from(e: yaml::Error) -> Self {
        Self { line: e.line, message: e.message }
    }
}

fn err<T>(line: usize, message: impl Into<String>) -> Result<T, Error> {
    Err(Error { line, message: message.into() })
}

impl Index {
    /// Parse and validate an index. The bytes must already have been
    /// checked against the repository's signature — this function has no
    /// way to know whether they were, which is why the only thing that
    /// calls it is [`crate::repo`], after verifying.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let doc = yaml::parse(text)?;
        known_keys(&doc, &["repo", "description", "plugins"])?;
        let repo = name_like(field(&doc, "repo", doc.line)?, "repo", line_of(&doc, "repo"))?;
        let description = optional(&doc, "description")?.map(str::to_string);

        let plugins_node = doc
            .get("plugins")
            .ok_or_else(|| Error { line: doc.line, message: "no `plugins:` list".into() })?;
        let items = plugins_node.as_list().ok_or_else(|| Error {
            line: plugins_node.line,
            message: format!("`plugins:` has to be a list of entries, not {}", plugins_node.kind()),
        })?;

        let mut plugins = Vec::with_capacity(items.len());
        for item in items {
            let plugin = Plugin::parse(item)?;
            if let Some(first) = plugins.iter().find(|p: &&Plugin| p.name == plugin.name) {
                return err(
                    plugin.line,
                    format!("`{}` is already listed on line {}", plugin.name, first.line),
                );
            }
            plugins.push(plugin);
        }
        Ok(Self { repo, description, plugins })
    }

    pub fn plugin(&self, name: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.name == name)
    }
}

impl Plugin {
    fn parse(node: &Node) -> Result<Self, Error> {
        if node.as_map().is_none() {
            return err(
                node.line,
                format!("a plugin entry has to be `name: …` keys, not {}", node.kind()),
            );
        }
        known_keys(
            node,
            &["name", "version", "summary", "file", "sha256", "entry", "events", "capabilities"],
        )?;
        let name = name_like(field(node, "name", node.line)?, "name", line_of(node, "name"))?;
        let version = version_like(field(node, "version", node.line)?, line_of(node, "version"))?;
        let file = relative_path(node, "file")?;
        let entry = relative_path(node, "entry")?;
        let sha256 = hash_like(node)?;
        let summary = optional(node, "summary")?.map(str::to_string);
        let events = match node.get("events") {
            None => Vec::new(),
            Some(n) => strings(n, "events")?,
        };
        let capabilities = match node.get("capabilities") {
            None => Capabilities::default(),
            Some(n) => parse_capabilities(n)?,
        };
        Ok(Self {
            name,
            version,
            summary,
            file,
            sha256,
            entry,
            events,
            capabilities,
            line: node.line,
        })
    }

    /// `name 1.2.3`, which is how one is named everywhere a user sees it.
    pub fn titled(&self) -> String {
        format!("{} {}", self.name, self.version)
    }
}

fn parse_capabilities(node: &Node) -> Result<Capabilities, Error> {
    if node.as_map().is_none() {
        return err(node.line, format!("`capabilities:` has to be a block, not {}", node.kind()));
    }
    known_keys(node, &["exec", "net", "fs", "read-pane-content"])?;
    let list = |key: &str| -> Result<Vec<String>, Error> {
        match node.get(key) {
            None => Ok(Vec::new()),
            Some(n) => strings(n, key),
        }
    };
    let read_pane_content = match node.get("read-pane-content") {
        None => false,
        Some(n) => boolean(n, "read-pane-content")?,
    };
    Ok(Capabilities { exec: list("exec")?, net: list("net")?, fs: list("fs")?, read_pane_content })
}

/// Refuse a key the schema does not have.
///
/// A misspelled key that is ignored is the worst of the available
/// behaviours: `shasum:` for `sha256:` leaves a plugin that cannot be
/// verified, and `enrty:` leaves one that cannot be loaded, both from a
/// file that parsed without complaint.
fn known_keys(node: &Node, allowed: &[&str]) -> Result<(), Error> {
    let Some(entries) = node.as_map() else { return Ok(()) };
    for (key, value) in entries {
        if !allowed.contains(&key.as_str()) {
            return err(
                value.line,
                format!(
                    "`{key}` is not something an index says here (expected one of: {})",
                    allowed.join(", ")
                ),
            );
        }
    }
    Ok(())
}

/// Where a key was written, falling back to the block it is in — which is
/// only reached when the key is absent, and then the block is the right
/// thing to point at anyway.
fn line_of(node: &Node, key: &str) -> usize {
    node.get(key).map(|n| n.line).unwrap_or(node.line)
}

fn field<'a>(node: &'a Node, key: &str, at: usize) -> Result<&'a str, Error> {
    let found = node.get(key).ok_or_else(|| Error { line: at, message: format!("no `{key}:`") })?;
    found.as_str().ok_or_else(|| Error {
        line: found.line,
        message: format!("`{key}:` has to be a value, not {}", found.kind()),
    })
}

fn optional<'a>(node: &'a Node, key: &str) -> Result<Option<&'a str>, Error> {
    match node.get(key) {
        None => Ok(None),
        Some(found) => found.as_str().map(Some).ok_or_else(|| Error {
            line: found.line,
            message: format!("`{key}:` has to be a value, not {}", found.kind()),
        }),
    }
}

fn strings(node: &Node, key: &str) -> Result<Vec<String>, Error> {
    let items = node.as_list().ok_or_else(|| Error {
        line: node.line,
        message: format!("`{key}:` has to be a list, not {}", node.kind()),
    })?;
    items
        .iter()
        .map(|item| {
            item.as_str().map(str::to_string).ok_or_else(|| Error {
                line: item.line,
                message: format!(
                    "`{key}:` holds a list of values, and this one is {}",
                    item.kind()
                ),
            })
        })
        .collect()
}

fn boolean(node: &Node, key: &str) -> Result<bool, Error> {
    match node.as_str() {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        // Not YAML's wider set of truths. `yes`, `on` and `y` are the
        // Norway problem, and a capability is the last place to guess.
        _ => err(node.line, format!("`{key}:` is `true` or `false`")),
    }
}

/// A name that is going to be a directory: lowercase, and nothing a path
/// or a shell would read as punctuation.
fn name_like(text: &str, key: &str, line: usize) -> Result<String, Error> {
    let ok = !text.is_empty()
        && text.len() <= 64
        && text.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        return Ok(text.to_string());
    }
    err(
        line,
        format!(
            "`{key}: {text}` — a name is 1 to 64 characters of lowercase letters, digits, \
             `-` and `_`, starting with a letter or digit"
        ),
    )
}

fn version_like(text: &str, line: usize) -> Result<String, Error> {
    // Not a semver parse: a repository may version however it likes, and
    // Haven only ever compares versions for equality and prints them. What
    // it must not do is let one be a path component with a `/` or a `..`
    // in it, because that is exactly where it ends up.
    let ok = !text.is_empty()
        && text.len() <= 64
        && text.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
        && !text.contains("..")
        && !text.starts_with('.');
    if ok {
        return Ok(text.to_string());
    }
    err(line, format!("`version: {text}` is not a version Haven will put in a path"))
}

/// A path inside the repository, or inside an artifact. The whole of the
/// traversal question lives here.
fn relative_path(node: &Node, key: &str) -> Result<String, Error> {
    let text = field(node, key, node.line)?;
    let at = line_of(node, key);
    let bad = text.is_empty()
        || text.len() > 512
        || text.starts_with('/')
        || text.starts_with('~')
        || text.contains('\\')
        || text.contains('\0')
        // A Windows drive letter or a URL scheme; either way not relative.
        || text.contains(':')
        || text.split('/').any(|part| part == ".." || part == "." || part.is_empty());
    if bad {
        return err(
            at,
            format!(
                "`{key}: {text}` has to be a plain relative path inside the repository — \
                 no leading `/`, no `..`, no empty components"
            ),
        );
    }
    Ok(text.to_string())
}

fn hash_like(node: &Node) -> Result<String, Error> {
    let text = field(node, "sha256", node.line)?;
    let at = line_of(node, "sha256");
    if text.len() != 64 || !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return err(at, format!("`sha256: {text}` is not 64 hex characters"));
    }
    Ok(text.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "\
repo: haven-community
description: plugins people have written
plugins:
  - name: gitline
    version: 0.2.1
    summary: the branch, in the status line
    file: pool/gitline-0.2.1.tar.gz
    sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
    entry: gitline.wasm
    events: [pane-focus, cwd-change]
    capabilities:
      exec: [git]
      fs: ['$CWD:ro']
  - name: urls
    version: 1.0.0
    file: pool/urls-1.0.0.tar.gz
    sha256: 0000000000000000000000000000000000000000000000000000000000000001
    entry: urls.wasm
    capabilities:
      read-pane-content: true
";

    fn good() -> Index {
        Index::parse(GOOD).unwrap_or_else(|e| panic!("the example should parse: {e}"))
    }

    /// Replace one line of the good index, so each failure test differs
    /// from a working file in exactly one way.
    fn with_line(find: &str, replace: &str) -> Error {
        assert!(GOOD.contains(find), "fixture does not contain {find:?}");
        Index::parse(&GOOD.replace(find, replace)).expect_err("should be refused")
    }

    #[test]
    fn an_index_reads_as_what_it_says() {
        let index = good();
        assert_eq!(index.repo, "haven-community");
        assert_eq!(index.description.as_deref(), Some("plugins people have written"));
        assert_eq!(index.plugins.len(), 2);

        let gitline = index.plugin("gitline").expect("gitline");
        assert_eq!(gitline.titled(), "gitline 0.2.1");
        assert_eq!(gitline.file, "pool/gitline-0.2.1.tar.gz");
        assert_eq!(gitline.entry, "gitline.wasm");
        assert_eq!(gitline.events, ["pane-focus", "cwd-change"]);
        assert_eq!(gitline.capabilities.exec, ["git"]);
        assert_eq!(gitline.capabilities.fs, ["$CWD:ro"]);
        assert!(!gitline.capabilities.read_pane_content);
        assert!(index.plugin("nothing").is_none());
    }

    #[test]
    fn what_is_not_asked_for_is_not_granted() {
        // Every capability absent from an entry is off, not inherited and
        // not defaulted to something convenient.
        let index = good();
        let urls = index.plugin("urls").expect("urls");
        assert!(urls.capabilities.read_pane_content);
        assert!(urls.capabilities.exec.is_empty());
        assert!(urls.capabilities.net.is_empty());
        assert!(urls.capabilities.fs.is_empty());
        assert_eq!(urls.events, [] as [String; 0]);
        assert_eq!(urls.summary, None);
    }

    #[test]
    fn a_path_that_climbs_out_of_the_repository_is_refused() {
        // The index is written by whoever runs the repository, and both of
        // these end up joined to a local directory. This is the test that
        // has to keep passing.
        for bad in [
            "../../../../etc/cron.d/x",
            "/etc/passwd",
            "~/.bashrc",
            "pool/../../x.tar.gz",
            "pool//x.tar.gz",
            "C:/x.tar.gz",
            "https://elsewhere.example/x.tar.gz",
            "pool/x\\y.tar.gz",
            "./x.tar.gz",
        ] {
            let e = with_line("file: pool/gitline-0.2.1.tar.gz", &format!("file: {bad}"));
            assert_eq!(e.line, 7, "{bad}: {e}");
            assert!(e.message.contains("relative path"), "{bad}: {e}");
        }
        // And the same rule for the entry inside the artifact.
        let e = with_line("entry: gitline.wasm", "entry: ../../gitline.wasm");
        assert!(e.message.contains("relative path"), "{e}");
    }

    #[test]
    fn a_name_or_a_version_that_is_not_path_safe_is_refused() {
        for bad in ["../evil", "Git Line", "git/line", "-leading", "GITLINE"] {
            let e = with_line("name: gitline", &format!("name: {bad}"));
            assert!(e.message.contains("a name is"), "{bad}: {e}");
            assert_eq!(e.line, 4, "{bad}: {e}");
        }
        for bad in ["../1.0", "1.0/2", ".hidden", "1..0"] {
            let e = with_line("version: 0.2.1", &format!("version: {bad}"));
            assert!(e.message.contains("not a version"), "{bad}: {e}");
            assert_eq!(e.line, 5, "{bad}: {e}");
        }
        // A key written with nothing after it is refused one layer down,
        // by the YAML, which is why neither check above has an empty case.
        let e = with_line("name: gitline", "name:");
        assert!(e.message.contains("nothing under it"), "{e}");
    }

    #[test]
    fn a_hash_that_is_not_a_hash_is_refused_with_its_line() {
        let short = "sha256: 9f86d081";
        let e = with_line(
            "sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            short,
        );
        assert_eq!(e.line, 8);
        assert!(e.message.contains("64 hex"), "{e}");
    }

    #[test]
    fn a_misspelled_key_is_an_error_and_not_a_silence() {
        // The failure mode this prevents: `shasum` parses, is ignored,
        // and leaves an entry with no hash — so either nothing installs or,
        // worse, something installs unverified.
        let e = with_line("sha256: 9f86", "shasum: 9f86");
        assert!(e.message.contains("`shasum` is not something"), "{e}");
        let e = Index::parse("repo: x\nplugins: []\nextra: y\n").expect_err("unknown top key");
        assert!(e.message.contains("`extra`"), "{e}");
    }

    #[test]
    fn a_missing_field_says_which_one() {
        for (find, missing) in [
            ("    version: 0.2.1\n", "version"),
            ("    file: pool/gitline-0.2.1.tar.gz\n", "file"),
            ("    entry: gitline.wasm\n", "entry"),
        ] {
            let e = Index::parse(&GOOD.replace(find, "")).expect_err("should be refused");
            assert!(e.message.contains(missing), "expected to miss {missing}: {e}");
        }
        let e = Index::parse("repo: x\n").expect_err("no plugins");
        assert!(e.message.contains("no `plugins:`"), "{e}");
    }

    #[test]
    fn the_same_plugin_twice_is_refused_and_points_at_the_first() {
        let doubled = GOOD.replace("name: urls", "name: gitline");
        let e = Index::parse(&doubled).expect_err("two entries, one name");
        assert!(e.message.contains("already listed on line 4"), "{e}");
    }

    #[test]
    fn a_capability_is_true_or_false_and_not_yaml_s_wider_set() {
        for bad in ["yes", "on", "1", "True", "y"] {
            let e = with_line("read-pane-content: true", &format!("read-pane-content: {bad}"));
            assert!(e.message.contains("`true` or `false`"), "{bad}: {e}");
        }
    }

    #[test]
    fn an_empty_plugin_list_is_a_repository_with_nothing_in_it() {
        // Not an error: a repository that has published nothing yet is a
        // normal thing to have added.
        let index = Index::parse("repo: empty\nplugins: []\n").expect("should parse");
        assert!(index.plugins.is_empty());
    }

    #[test]
    fn only_the_newly_asked_capabilities_need_asking_about() {
        let was = Capabilities {
            exec: vec!["git".into()],
            fs: vec!["$CWD:ro".into()],
            ..Default::default()
        };
        let now = Capabilities {
            exec: vec!["git".into(), "jj".into()],
            fs: vec!["$CWD:ro".into()],
            read_pane_content: true,
            ..Default::default()
        };
        let added = was.newly_asked(&now);
        assert_eq!(added.exec, ["jj"], "git was already granted");
        assert!(added.fs.is_empty(), "unchanged paths are not a new question");
        assert!(added.read_pane_content, "the dangerous one has to be asked again");
        assert_eq!(added.lines()[0], "read what your panes are showing", "and asked first");

        // An update that gives something up asks nothing.
        assert!(now.newly_asked(&was).is_empty());
    }

    #[test]
    fn capabilities_read_as_a_sentence_a_person_can_judge() {
        let caps = Capabilities {
            exec: vec!["git".into()],
            net: vec!["crates.io".into()],
            read_pane_content: true,
            ..Default::default()
        };
        let shown = caps.to_string();
        assert!(shown.starts_with("read what your panes are showing"), "{shown}");
        assert!(shown.contains("run: git"), "{shown}");
        assert_eq!(Capabilities::default().to_string(), "nothing");
    }
}
