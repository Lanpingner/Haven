//! Getting a repository's files, by whatever a repository turns out to be.
//!
//! Three kinds, chosen by the URL and never by a flag, because a URL
//! already says which it is:
//!
//! | URL | What it is |
//! |---|---|
//! | `https://host/path` | a directory served over HTTPS, fetched file by file |
//! | `https://host/x.git`, `git://…`, `ssh://…`, `git+https://…`, `git+file://…` | a git repository, cloned once and pulled after |
//! | `/path/to/dir`, `file:///path` | a directory on this machine |
//!
//! A git repository is the literal reading of "`plugins.yaml` inside the
//! repo" and is what people will reach for, since they already have one.
//! An HTTPS directory is what a repository becomes when it outgrows that —
//! it needs no git on the client and can be a bucket. A local directory is
//! how you test either without publishing anything, and how a company
//! ships an internal repository over a mount.
//!
//! Fetching shells out to `git` and `curl` rather than linking a git
//! implementation and an HTTP stack. That is the same decision Haven makes
//! for `docker`, `man` and `xdg-open`, and here it also means the transport
//! has no say in what is trusted: whatever comes back is bytes, and the
//! signature over those bytes is checked afterwards, by [`crate::sign`].
//! A hostile mirror, a cache in the middle or a wrong URL all end the same
//! way.

use std::path::{Path, PathBuf};
use std::process::Command;

use std::fmt;

/// How big a file may be before Haven stops reading it.
///
/// An index arrives before anything about it is known — not its signature,
/// not even that it is text — so "it is small" cannot be assumed, only
/// enforced. The artifact limit is the one that matters less: it is hashed
/// against a signed index, so a wrong one is caught, but a download that
/// never ends is still a download that never ends.
pub const MAX_INDEX: u64 = 8 * 1024 * 1024;
pub const MAX_ARTIFACT: u64 = 128 * 1024 * 1024;

/// How long a single fetch may take.
const TIMEOUT_SECS: u32 = 120;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// The URL is not one of the three kinds, or is one Haven refuses.
    Url(String),
    /// The transport ran and failed. `what` is the tool's own complaint,
    /// trimmed, because it is almost always the useful part.
    Failed {
        tool: &'static str,
        what: String,
    },
    /// `tool` is not installed.
    Missing(&'static str),
    TooBig {
        limit: u64,
    },
    NotFound(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(why) => write!(f, "{why}"),
            Self::Failed { tool, what } => write!(f, "{tool}: {what}"),
            Self::Missing(tool) => {
                write!(f, "this repository needs `{tool}`, which is not installed")
            }
            Self::TooBig { limit } => {
                write!(
                    f,
                    "the file is larger than {} MiB, so Haven stopped reading",
                    limit / 1024 / 1024
                )
            }
            Self::NotFound(path) => write!(f, "the repository has no {path}"),
        }
    }
}

impl std::error::Error for Error {}

/// Which of the three a URL names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Git,
    Https,
    Local,
}

/// Read a URL as one of the three kinds, or say why not.
pub fn kind_of(url: &str) -> Result<Kind, Error> {
    let url = url.trim();
    if url.is_empty() {
        return Err(Error::Url("an empty URL".into()));
    }
    // Control characters in a URL end up in an argv or a config file.
    if url.chars().any(|c| c.is_control()) {
        return Err(Error::Url("a URL with a control character in it".into()));
    }
    // `-` first would be read as an option by git and by curl both. This
    // is the whole argument-injection surface and it closes here.
    if url.starts_with('-') {
        return Err(Error::Url("a URL cannot start with `-`".into()));
    }
    let lower = url.to_lowercase();
    if lower.starts_with("git://")
        || lower.starts_with("ssh://")
        || lower.starts_with("git+https://")
        || lower.starts_with("git+ssh://")
        // A git repository on this machine, which is both how you try a
        // repository out before publishing it and how an internal one
        // arrives over a mount.
        || lower.starts_with("git+file://")
        || lower.ends_with(".git")
        || lower.ends_with(".git/")
    {
        return Ok(Kind::Git);
    }
    if lower.starts_with("https://") {
        return Ok(Kind::Https);
    }
    if lower.starts_with("http://") {
        // The signature would still be checked, so this is not unsafe in
        // the way it looks — but plain HTTP lets anyone in the path serve
        // an *older* index that is still correctly signed, and Haven has
        // nothing like apt's `Valid-Until` to notice. Refusing is cheap:
        // every host that can serve a repository can serve it over TLS.
        return Err(Error::Url(
            "plain `http://` is refused: a repository must be served over HTTPS, \
             so nobody in the middle can hold you on an old index"
                .into(),
        ));
    }
    if let Some(path) = lower.strip_prefix("file://") {
        if !path.starts_with('/') {
            return Err(Error::Url("a `file://` URL needs an absolute path".into()));
        }
        return Ok(Kind::Local);
    }
    if url.starts_with('/') {
        return Ok(Kind::Local);
    }
    Err(Error::Url(format!(
        "`{url}` is not a repository Haven knows how to reach — expected `https://…`, \
         a git URL, or an absolute path"
    )))
}

/// Where a repository's files come from.
///
/// A trait because the tests must not touch the network, and because the
/// three kinds answer "give me this file" in genuinely different ways: one
/// clones, one makes a request, one reads a directory.
pub trait Fetch: fmt::Debug {
    /// Bring the repository within reach of [`Fetch::file`]. For git that
    /// is a clone or a pull into `work`; for the others it does nothing.
    /// Called once per refresh, so a repository is not pulled per file.
    fn sync(&self, url: &str, work: &Path) -> Result<(), Error>;

    /// One file from the repository, by its path relative to the root.
    /// `limit` is the point at which to give up rather than keep reading.
    fn file(&self, url: &str, path: &str, work: &Path, limit: u64) -> Result<Vec<u8>, Error>;
}

/// The real thing: `git`, `curl`, or the filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tools;

impl Fetch for Tools {
    fn sync(&self, url: &str, work: &Path) -> Result<(), Error> {
        match kind_of(url)? {
            Kind::Git => git_sync(url, work),
            Kind::Https | Kind::Local => Ok(()),
        }
    }

    fn file(&self, url: &str, path: &str, work: &Path, limit: u64) -> Result<Vec<u8>, Error> {
        match kind_of(url)? {
            Kind::Git => read_capped(&work.join(path), path, limit),
            Kind::Local => read_capped(&local_root(url).join(path), path, limit),
            Kind::Https => curl(&join_url(url, path), limit),
        }
    }
}

/// The directory a `file://` URL or a bare path names.
fn local_root(url: &str) -> PathBuf {
    PathBuf::from(url.strip_prefix("file://").unwrap_or(url))
}

/// `base` and `path` joined with exactly one slash.
///
/// `path` comes from a validated index, which has already refused `..`,
/// absolute paths and anything with a `:` in it, so this is a join and not
/// a URL resolution.
fn join_url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

fn read_capped(full: &Path, shown: &str, limit: u64) -> Result<Vec<u8>, Error> {
    let meta = std::fs::metadata(full).map_err(|_| Error::NotFound(shown.to_string()))?;
    if meta.len() > limit {
        return Err(Error::TooBig { limit });
    }
    std::fs::read(full).map_err(|e| Error::Failed { tool: "read", what: e.to_string() })
}

/// Clone the repository, or bring an existing clone up to date.
///
/// Shallow and single-branch: Haven wants the current state of one file,
/// never the history, and a repository of plugins can be large.
fn git_sync(url: &str, work: &Path) -> Result<(), Error> {
    let url = url.strip_prefix("git+").unwrap_or(url);
    let hardening = [
        // A cloned repository must not get to run anything. Hooks are not
        // transferred by a clone, so this is belt and braces — but the
        // belt is one argument long.
        "-c",
        "core.hooksPath=/dev/null",
        // Never stop to ask for a password: this runs without a terminal
        // as often as with one, and a prompt there is a hang.
        "-c",
        "core.askPass=",
        "-c",
        "credential.helper=",
    ];
    if work.join(".git").is_dir() {
        let dir = work.to_string_lossy().to_string();
        run(
            "git",
            hardening
                .iter()
                .map(|s| (*s).to_string())
                .chain(
                    ["-C", &dir, "fetch", "--depth", "1", "--no-tags", "origin", "HEAD"]
                        .iter()
                        .map(|s| (*s).to_string()),
                )
                .collect(),
        )?;
        return run(
            "git",
            hardening
                .iter()
                .map(|s| (*s).to_string())
                .chain(
                    ["-C", &dir, "reset", "--hard", "FETCH_HEAD"].iter().map(|s| (*s).to_string()),
                )
                .collect(),
        );
    }
    if let Some(parent) = work.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Failed { tool: "git", what: e.to_string() })?;
    }
    // `--` so a URL that survived `kind_of` still cannot be read as a flag.
    run(
        "git",
        hardening
            .iter()
            .map(|s| (*s).to_string())
            .chain(
                ["clone", "--depth", "1", "--single-branch", "--no-tags", "--"]
                    .iter()
                    .map(|s| (*s).to_string()),
            )
            .chain([url.to_string(), work.to_string_lossy().to_string()])
            .collect(),
    )
}

/// One HTTPS GET, to memory, with a ceiling and a clock.
fn curl(url: &str, limit: u64) -> Result<Vec<u8>, Error> {
    let args: Vec<String> = [
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        // Redirects may not leave HTTPS, or the ceiling above on `http://`
        // is one redirect wide.
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--tlsv1.2",
        "--max-time",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .chain([
        TIMEOUT_SECS.to_string(),
        "--max-filesize".to_string(),
        limit.to_string(),
        "--output".to_string(),
        "-".to_string(),
        "--".to_string(),
        url.to_string(),
    ])
    .collect();

    let out = output("curl", args)?;
    if out.len() as u64 > limit {
        return Err(Error::TooBig { limit });
    }
    Ok(out)
}

/// Run a tool for its exit status, keeping its complaint.
fn run(tool: &'static str, args: Vec<String>) -> Result<(), Error> {
    output(tool, args).map(|_| ())
}

/// Run a tool and keep its standard output.
fn output(tool: &'static str, args: Vec<String>) -> Result<Vec<u8>, Error> {
    let out = Command::new(tool).args(&args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Missing(tool)
        } else {
            Error::Failed { tool, what: e.to_string() }
        }
    })?;
    if !out.status.success() {
        let what = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let what = if what.is_empty() {
            match out.status.code() {
                Some(code) => format!("exited {code}"),
                None => "killed".to_string(),
            }
        } else {
            // One line: curl and git both say the useful thing first, and
            // a wall of text in a status bar is a wall of text.
            what.lines().next().unwrap_or(&what).to_string()
        };
        return Err(Error::Failed { tool, what });
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_says_which_kind_of_repository_it_is() {
        for (url, kind) in [
            ("https://plugins.example.com/haven", Kind::Https),
            ("https://example.com/haven-plugins.git", Kind::Git),
            ("https://example.com/haven-plugins.git/", Kind::Git),
            ("git://example.com/x", Kind::Git),
            ("ssh://git@example.com/x", Kind::Git),
            ("git+https://example.com/x", Kind::Git),
            ("git+file:///srv/haven.repo", Kind::Git),
            ("file:///srv/haven", Kind::Local),
            ("/srv/haven", Kind::Local),
            ("HTTPS://EXAMPLE.COM/x", Kind::Https),
        ] {
            assert_eq!(kind_of(url).unwrap_or_else(|e| panic!("{url}: {e}")), kind, "{url}");
        }
    }

    #[test]
    fn plain_http_is_refused_and_says_why() {
        let e = kind_of("http://plugins.example.com/haven").expect_err("http must be refused");
        let shown = e.to_string();
        assert!(shown.contains("HTTPS"), "{shown}");
        // The reason matters more than the refusal: the signature *would*
        // still be checked, so the risk is being held on an old index.
        assert!(shown.contains("old index"), "{shown}");
    }

    #[test]
    fn a_url_that_could_be_an_argument_is_not_a_url() {
        // `kind_of` is the only thing between a repository URL and an
        // argv. `--upload-file` to curl, `--upload-pack` to git.
        for bad in [
            "--upload-file=/etc/passwd",
            "-X",
            "",
            "   ",
            "ftp://example.com/x",
            "relative/path",
            "file://relative",
            "https://example.com/\nX",
        ] {
            assert!(kind_of(bad).is_err(), "{bad:?} should not be a repository URL");
        }
    }

    #[test]
    fn a_file_url_and_a_bare_path_name_the_same_directory() {
        assert_eq!(local_root("file:///srv/haven"), PathBuf::from("/srv/haven"));
        assert_eq!(local_root("/srv/haven"), PathBuf::from("/srv/haven"));
    }

    #[test]
    fn a_url_is_joined_with_exactly_one_slash() {
        assert_eq!(
            join_url("https://x.example/h", "plugins.yaml"),
            "https://x.example/h/plugins.yaml"
        );
        assert_eq!(
            join_url("https://x.example/h/", "plugins.yaml"),
            "https://x.example/h/plugins.yaml"
        );
        assert_eq!(
            join_url("https://x.example/h/", "/plugins.yaml"),
            "https://x.example/h/plugins.yaml"
        );
    }

    #[test]
    fn a_local_repository_reads_its_files_and_refuses_an_oversized_one() {
        let dir = tempdir("fetch-local");
        std::fs::write(dir.join("plugins.yaml"), b"repo: x\n").unwrap();
        let got = Tools.file(&dir.to_string_lossy(), "plugins.yaml", &dir, MAX_INDEX).unwrap();
        assert_eq!(got, b"repo: x\n");

        let e = Tools
            .file(&dir.to_string_lossy(), "plugins.yaml", &dir, 2)
            .expect_err("a 2-byte ceiling");
        assert!(matches!(e, Error::TooBig { .. }), "{e}");

        let e = Tools
            .file(&dir.to_string_lossy(), "nothing.yaml", &dir, MAX_INDEX)
            .expect_err("missing file");
        assert!(matches!(e, Error::NotFound(_)), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real git repository, cloned and then pulled, with no network: a
    /// local path is a perfectly good git remote, which is what makes this
    /// testable at all.
    #[test]
    fn a_git_repository_is_cloned_once_and_updated_after() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = tempdir("fetch-git");
        let origin = root.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&origin)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output()
                .expect("git");
            assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("plugins.yaml"), b"repo: first\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);

        let work = root.join("clone");
        let url = origin.to_string_lossy().to_string();
        // A path ending in `.git` is the git signal, but a plain directory
        // that happens to be a repository is reached by naming it as one.
        let git_url = format!("git+file://{url}");
        Tools.sync(&git_url, &work).expect("clone");
        let got = Tools.file(&git_url, "plugins.yaml", &work, MAX_INDEX).unwrap();
        assert_eq!(got, b"repo: first\n");

        // A second sync is a pull, and it has to pick up the change.
        std::fs::write(origin.join("plugins.yaml"), b"repo: second\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "two"]);
        Tools.sync(&git_url, &work).expect("pull");
        let got = Tools.file(&git_url, "plugins.yaml", &work, MAX_INDEX).unwrap();
        assert_eq!(got, b"repo: second\n", "the clone was not brought up to date");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tool_that_is_not_installed_says_so_rather_than_failing_vaguely() {
        let e = output("haven-no-such-tool", vec![]).expect_err("should not exist");
        assert!(matches!(e, Error::Missing(_)), "{e}");
        assert!(e.to_string().contains("not installed"), "{e}");
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "haven-plugin-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }
}
