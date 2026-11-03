//! Virtual shells: a pane whose shell does not live on this machine.
//!
//! `haven-shell` answers "which shell, and how does its integration load".
//! This crate answers a question one level out: *where does the shell run at
//! all*. A pane may hold a shell in a container, and later in a VM, a remote
//! host or a namespace, without the mux learning a single thing about any of
//! them.
//!
//! The shape is deliberately the one [`haven_shell::Shell`] already uses — a
//! trait with one impl per thing and a registry — because it has already
//! earned it: adding a shell touches one file, and so should adding a place
//! to run one.
//!
//! Everything crosses the existing seams rather than opening new ones. A
//! virtual shell is named by a [`Spec`] string, which travels as a pane's
//! *program*: the field that already exists, that the daemon already
//! interprets on its own machine, that the save file already records and the
//! command line already accepts. So `haven run 'new-window
//! docker://run/alpine:latest?scratch'` works for free, and nothing in the
//! protocol knows what a container is.
//!
//! A launch never fails. [`Backend::launch`] returns a [`PtyConfig`] that
//! runs a small shell script, and every way the thing can go wrong —
//! the CLI missing, a pull failing — is handled *inside* that script, in the
//! pane, where the person can read it. The alternative is a spawn error on a
//! path that reaches the daemon's event loop, which would rather not die
//! because Docker is not installed.

use std::fmt;
use std::path::PathBuf;

use haven_pty::PtyConfig;

pub mod docker;

pub use docker::Docker;

/// Something a pane could get a shell in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    /// The [`Spec`] string that launches it, minus any per-pane options.
    pub spec: String,
    /// What to call it in a menu: `archlinux:latest`, or a container's name.
    pub label: String,
    /// The second column: a size, a status, or why it is not here yet.
    pub detail: String,
    /// Already on this machine. A target that is not has to be fetched
    /// first, which the person is told about rather than left to guess from
    /// a pane that sits still for two minutes.
    pub present: bool,
    /// Whether this target can be opened as a lasting container or only as
    /// a throwaway. An image can be both; a container that already exists is
    /// already the lasting kind.
    pub choosable: bool,
}

/// Where a virtual shell's files come from, and what to call the CLI.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Settings {
    /// The command that drives the containers. `podman` is argument-for-
    /// argument compatible with everything used here, so pointing this at it
    /// is enough — no second impl, and no pretending Docker is the only one.
    pub cli: String,
    /// Images offered even when they are not pulled yet.
    pub images: Vec<String>,
    /// Whether containers are offered at all.
    pub enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cli: "docker".to_string(),
            images: docker::KNOWN_IMAGES.iter().map(|s| (*s).to_string()).collect(),
            enabled: true,
        }
    }
}

/// What the daemon knows at spawn time that a [`Spec`] cannot carry.
#[derive(Clone, Debug)]
pub struct Context {
    pub settings: Settings,
    /// The directory the pane was opened in, which `mount=cwd` means.
    pub cwd: Option<PathBuf>,
    /// A directory we own and may write to, for integration scripts.
    pub dir: PathBuf,
}

/// How long a container made for a pane lasts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Lifetime {
    /// A container of its own, removed when the pane closes. Truly
    /// nix-shell: nothing you install survives, and nothing is left behind.
    #[default]
    Scratch,
    /// One lasting container per image, reused by every later pane. What you
    /// install stays installed, which is what makes it a sandbox you can
    /// work in rather than one you can only look at.
    Keep,
}

/// A directory handed to the virtual shell.
///
/// `host` is read on the *daemon's* machine, and `cwd` there means the
/// directory the pane was opened in — resolved at spawn, because a spec is
/// written long before the pane it ends up in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Mount {
    pub host: String,
    /// Where it appears inside. Defaults to the same path, which is the
    /// whole point: a build that fails in a path is easier to compare when
    /// the path is the same on both sides.
    pub dest: Option<String>,
}

/// A virtual shell, named as a string.
///
/// `docker://run/archlinux:latest?keep&mount=cwd`
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Spec {
    pub backend: String,
    /// What to do with the target: `run` an image, or `exec` into a
    /// container that is already up.
    pub verb: String,
    pub target: String,
    pub lifetime: Lifetime,
    /// Whether a lifetime was *written*, which is not the same as the
    /// default: an unqualified image spec is the one the menu asks about,
    /// and the question is worth asking rather than guessing.
    pub lifetime_given: bool,
    pub mounts: Vec<Mount>,
}

impl Spec {
    /// Parse a spec string, or `None` if it is not one.
    ///
    /// `None` is the ordinary answer: every pane program goes through here,
    /// and almost all of them are `/bin/bash`.
    pub fn parse(text: &str) -> Option<Self> {
        let (backend, rest) = text.split_once("://")?;
        if backend.is_empty() || !backend.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        let (path, query) = match rest.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (rest, None),
        };
        let (verb, target) = path.split_once('/')?;
        if verb.is_empty() || target.is_empty() {
            return None;
        }

        let mut spec = Self {
            backend: backend.to_string(),
            verb: verb.to_string(),
            target: target.to_string(),
            lifetime: Lifetime::default(),
            lifetime_given: false,
            mounts: Vec::new(),
        };
        for part in query.unwrap_or_default().split('&').filter(|p| !p.is_empty()) {
            match part.split_once('=') {
                Some(("mount", value)) => {
                    // `host:dest`, and a bare path means the same path
                    // inside. Windows drive letters are not a worry here;
                    // the daemon is a Unix process by construction.
                    let (host, dest) = match value.split_once(':') {
                        Some((host, dest)) => (host, Some(dest.to_string())),
                        None => (value, None),
                    };
                    spec.mounts.push(Mount { host: host.to_string(), dest });
                }
                Some(_) => {}
                None => match part {
                    "keep" => {
                        spec.lifetime = Lifetime::Keep;
                        spec.lifetime_given = true;
                    }
                    "scratch" => {
                        spec.lifetime = Lifetime::Scratch;
                        spec.lifetime_given = true;
                    }
                    _ => {}
                },
            }
        }
        Some(spec)
    }

    /// The same spec with a lifetime written into it, as the menu's second
    /// question answers.
    pub fn with_lifetime(mut self, lifetime: Lifetime) -> Self {
        self.lifetime = lifetime;
        self.lifetime_given = true;
        self
    }

    /// Whether this spec still has a question to answer before it can be
    /// launched sensibly.
    pub fn asks_lifetime(&self) -> bool {
        self.verb == "run" && !self.lifetime_given
    }
}

impl fmt::Display for Spec {
    /// Canonical form, and the form written to a save file: parsing this
    /// back must give the same spec, or a restored pane is a different pane.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}/{}", self.backend, self.verb, self.target)?;
        let mut sep = '?';
        if self.lifetime_given {
            let word = match self.lifetime {
                Lifetime::Keep => "keep",
                Lifetime::Scratch => "scratch",
            };
            write!(f, "{sep}{word}")?;
            sep = '&';
        }
        for mount in &self.mounts {
            write!(f, "{sep}mount={}", mount.host)?;
            if let Some(dest) = &mount.dest {
                write!(f, ":{dest}")?;
            }
            sep = '&';
        }
        Ok(())
    }
}

/// One place a pane's shell can run.
pub trait Backend: fmt::Debug + Send + Sync {
    /// The spec scheme this backend answers to.
    fn name(&self) -> &'static str;

    /// Everything a pane could be opened in, most useful first.
    fn targets(&self, settings: &Settings) -> Vec<Target>;

    /// How to run it. Infallible on purpose — see the module docs.
    fn launch(&self, spec: &Spec, ctx: &Context) -> PtyConfig;

    /// Whether a restore may take this spec at its word.
    ///
    /// A save file decides what a machine runs when it comes back, which is
    /// the same reason `restore_programs` is an allowlist. The line drawn
    /// here is that a restore may *rejoin* something already running and may
    /// never start anything: so a container that is up comes back, and one
    /// that is not leaves an ordinary shell in the pane.
    fn rejoinable(&self, spec: &Spec, settings: &Settings) -> bool;
}

static DOCKER: Docker = Docker;
/// Every backend there is. One for now; the list is the registry, and adding
/// the next one is an impl and a line here.
static ALL: &[&dyn Backend] = &[&DOCKER];

pub fn all() -> &'static [&'static dyn Backend] {
    ALL
}

/// The backend a spec names, if we have it.
pub fn for_spec(spec: &Spec) -> Option<&'static dyn Backend> {
    all().iter().copied().find(|b| b.name() == spec.backend)
}

/// Everything every backend offers.
pub fn targets(settings: &Settings) -> Vec<Target> {
    if !settings.enabled {
        return Vec::new();
    }
    all().iter().flat_map(|b| b.targets(settings)).collect()
}

/// The launch for a pane program, or `None` when it is an ordinary program.
pub fn launch(program: &str, ctx: &Context) -> Option<PtyConfig> {
    let spec = Spec::parse(program)?;
    let backend = for_spec(&spec)?;
    Some(backend.launch(&spec, ctx))
}

/// Whether a restore may honour this pane program.
pub fn rejoinable(program: &str, settings: &Settings) -> bool {
    let Some(spec) = Spec::parse(program) else { return false };
    let Some(backend) = for_spec(&spec) else { return false };
    backend.rejoinable(&spec, settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_program_is_not_a_spec() {
        // The common case by a mile: every pane program is offered here.
        assert_eq!(Spec::parse("/bin/bash"), None);
        assert_eq!(Spec::parse("vim"), None);
        assert_eq!(Spec::parse(""), None);
        assert_eq!(Spec::parse("docker://"), None);
        assert_eq!(Spec::parse("docker://run/"), None);
        assert_eq!(Spec::parse("docker://run"), None, "a verb with no target is not a spec");
    }

    #[test]
    fn a_spec_survives_the_round_trip_it_is_saved_through() {
        // A save file holds the string, so parsing what was written has to
        // give back the same pane. Anything else restores something else.
        for text in [
            "docker://run/archlinux:latest?keep",
            "docker://run/alpine:latest?scratch",
            "docker://exec/my-container",
            "docker://run/ubuntu:24.04?keep&mount=cwd",
            "docker://run/debian:latest?scratch&mount=/src:/work",
        ] {
            let spec = Spec::parse(text).expect(text);
            assert_eq!(spec.to_string(), text);
            assert_eq!(Spec::parse(&spec.to_string()), Some(spec));
        }
    }

    #[test]
    fn an_image_with_no_lifetime_is_a_question_not_a_default() {
        let spec = Spec::parse("docker://run/alpine:latest").unwrap();
        assert!(spec.asks_lifetime());
        assert!(!spec.lifetime_given);
        // Answering it is what the second menu does.
        let answered = spec.with_lifetime(Lifetime::Keep);
        assert!(!answered.asks_lifetime());
        assert_eq!(answered.to_string(), "docker://run/alpine:latest?keep");
    }

    #[test]
    fn a_container_that_already_exists_asks_nothing() {
        // `exec` has no lifetime to choose: the container outlives the pane
        // either way, because it was not the pane that made it.
        assert!(!Spec::parse("docker://exec/web").unwrap().asks_lifetime());
    }

    #[test]
    fn a_mount_defaults_to_the_same_path_inside() {
        let spec = Spec::parse("docker://run/alpine:latest?mount=/srv").unwrap();
        assert_eq!(spec.mounts, vec![Mount { host: "/srv".into(), dest: None }]);
        let spec = Spec::parse("docker://run/alpine:latest?mount=/srv:/work").unwrap();
        assert_eq!(spec.mounts[0].dest.as_deref(), Some("/work"));
    }

    #[test]
    fn several_mounts_are_kept_in_the_order_they_were_added() {
        let spec = Spec::parse("docker://run/alpine:latest?mount=/a&mount=/b&keep").unwrap();
        assert_eq!(spec.mounts.len(), 2);
        assert_eq!(spec.mounts[0].host, "/a");
        assert_eq!(spec.mounts[1].host, "/b");
        assert_eq!(spec.lifetime, Lifetime::Keep);
    }

    #[test]
    fn an_unknown_option_is_ignored_rather_than_fatal() {
        // A spec is written by hand and by older versions of Haven. A
        // misspelled option must not cost the pane.
        let spec = Spec::parse("docker://run/alpine:latest?keep&nonsense&x=1").unwrap();
        assert_eq!(spec.lifetime, Lifetime::Keep);
    }

    #[test]
    fn a_backend_nobody_implements_launches_nothing() {
        let ctx = Context {
            settings: Settings::default(),
            cwd: None,
            dir: PathBuf::from("/tmp/haven-test"),
        };
        assert!(launch("qemu://run/debian", &ctx).is_none());
        assert!(launch("/bin/sh", &ctx).is_none());
        assert!(!rejoinable("qemu://run/debian", &Settings::default()));
    }

    #[test]
    fn containers_can_be_switched_off_entirely() {
        let settings = Settings { enabled: false, ..Default::default() };
        assert!(targets(&settings).is_empty());
    }
}
