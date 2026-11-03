//! Containers as panes: pick a distro, get a shell in it.
//!
//! The model is nix-shell's rather than a container runtime's. You are not
//! administering anything; you want a pane that is Arch for ten minutes, or
//! an Ubuntu you keep around to check that the build works there too. So the
//! menu lists distros, the pane is a shell, and the question asked is the one
//! that actually matters — does what I install in here survive.
//!
//! Three deliberate decisions:
//!
//! **Everything the person needs to see happens in the pane.** The launch is
//! a short shell script rather than `docker` directly, so a missing CLI, a
//! pull of half a gigabyte and a failed pull are all *visible*, in the pane,
//! as they happen. A pull in particular: a pane that sits blank for two
//! minutes is indistinguishable from a pane that is broken.
//!
//! **A failure leaves a working pane.** Every branch that cannot reach a
//! container ends in `exec $SHELL`, so you get a local shell and a line
//! saying why, never a pane that vanishes before you can read it.
//!
//! **`docker` is a setting, not an assumption.** `podman` takes the same
//! arguments for all of this, so pointing [`Settings::cli`] at it is the
//! whole of podman support.

use std::ffi::OsString;
use std::path::Path;

use haven_pty::PtyConfig;
use haven_shell::Shell;

use crate::{Backend, Context, Lifetime, Settings, Spec, Target};

/// Distros offered whether or not they are pulled yet.
///
/// A fresh machine has no images at all, and a menu that is empty until you
/// have already done the thing by hand is not a feature. Nothing is
/// downloaded until one is chosen.
pub const KNOWN_IMAGES: &[&str] = &[
    "archlinux:latest",
    "ubuntu:latest",
    "debian:latest",
    "fedora:latest",
    "alpine:latest",
    "opensuse/tumbleweed:latest",
    "rockylinux:9",
    "nixos/nix:latest",
];

/// What a named container is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// No such container.
    Missing,
    /// Exists, not running: `start` resumes the shell it was made with.
    Stopped,
    Running,
}

#[derive(Debug)]
pub struct Docker;

impl Backend for Docker {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn targets(&self, settings: &Settings) -> Vec<Target> {
        let ps = capture(&settings.cli, &["ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Status}}"])
            .unwrap_or_default();
        let images =
            capture(&settings.cli, &["images", "--format", "{{.Repository}}:{{.Tag}}\t{{.Size}}"])
                .unwrap_or_default();
        offer(&ps, &images, &settings.images)
    }

    fn launch(&self, spec: &Spec, ctx: &Context) -> PtyConfig {
        // Looked up here rather than in the script because the answer picks
        // between three different commands, and a shell script that branches
        // three ways on `docker inspect` output is a worse thing to read
        // than this is.
        let state = match spec.verb.as_str() {
            "exec" => self.state(&ctx.settings.cli, &spec.target),
            _ if spec.lifetime == Lifetime::Keep => {
                self.state(&ctx.settings.cli, &container_name(&spec.target))
            }
            _ => State::Missing,
        };
        // Opening a container pane is exactly when a container left behind
        // by a killed one is about to become confusing.
        reap(&ctx.settings.cli);
        let integration = install_integration(ctx).ok();
        let script = plan(spec, ctx, state, integration.is_some());

        PtyConfig {
            program: Some("/bin/sh".into()),
            args: vec!["-c".into(), OsString::from(script)],
            // The pane's own environment, which the fallback shell inherits
            // when a container cannot be reached. What the *container* gets
            // is passed with `-e`, further down.
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
                ("TERM_PROGRAM".into(), "haven".into()),
                ("HAVEN_CONTAINER".into(), spec.target.clone().into()),
            ],
            env_remove: vec!["LINES".into(), "COLUMNS".into()],
            cwd: ctx.cwd.clone(),
            ..Default::default()
        }
    }

    fn rejoinable(&self, spec: &Spec, settings: &Settings) -> bool {
        // Only something already up. A restore that starts containers would
        // mean a file on disk decides what runs on the machine after a
        // reboot, which is the one thing the restore design refuses to do.
        let name = match spec.verb.as_str() {
            "exec" => spec.target.clone(),
            _ if spec.lifetime == Lifetime::Keep => container_name(&spec.target),
            // A scratch container was removed when its pane closed. There is
            // nothing to come back to, and making a new one is not a restore.
            _ => return false,
        };
        self.state(&settings.cli, &name) == State::Running
    }
}

impl Docker {
    fn state(&self, cli: &str, name: &str) -> State {
        match capture(cli, &["inspect", "-f", "{{.State.Running}}", name]) {
            Some(out) if out.trim() == "true" => State::Running,
            Some(_) => State::Stopped,
            None => State::Missing,
        }
    }
}

/// Run the CLI and take its stdout, or `None` if it could not be run or said
/// it failed. A missing `docker` is not an error here: it is an empty menu.
fn capture(cli: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cli)
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The menu, built from what the CLI said.
///
/// Running containers first, because wanting a shell in one is a live
/// problem; then images you have; then the ones that would be fetched. A
/// pullable image is never dropped from the list but never ranks above
/// something already here.
fn offer(ps: &str, images: &str, known: &[String]) -> Vec<Target> {
    let mut out = Vec::new();

    for line in ps.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.split('\t');
        let Some(name) = fields.next().map(str::trim).filter(|n| !n.is_empty()) else { continue };
        let image = fields.next().unwrap_or_default().trim();
        let status = fields.next().unwrap_or_default().trim();
        out.push(Target {
            spec: format!("docker://exec/{name}"),
            label: name.to_string(),
            detail: if status.is_empty() {
                image.to_string()
            } else {
                format!("{image} · {status}")
            },
            present: true,
            choosable: false,
        });
    }

    let mut local: Vec<(String, String)> = Vec::new();
    for line in images.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.split('\t');
        let Some(name) = fields.next().map(str::trim).filter(|n| !n.is_empty()) else { continue };
        // `<none>:<none>` is an intermediate layer, and `<none>` as a tag is
        // an image whose tag moved. Neither is something to offer.
        if name.contains("<none>") {
            continue;
        }
        local.push((name.to_string(), fields.next().unwrap_or_default().trim().to_string()));
    }
    for (name, size) in &local {
        out.push(Target {
            spec: format!("docker://run/{name}"),
            label: name.clone(),
            detail: size.clone(),
            present: true,
            choosable: true,
        });
    }

    for image in known {
        if local.iter().any(|(name, _)| name == image) {
            continue;
        }
        out.push(Target {
            spec: format!("docker://run/{image}"),
            label: image.clone(),
            detail: "not pulled yet".to_string(),
            present: false,
            choosable: true,
        });
    }
    out
}

/// `haven-<image>`, for the container a `keep` pane reuses.
///
/// Prefixed, because it has to be obvious in `docker ps` which containers
/// Haven made; sanitised, because `/` and `:` are legal in image names and
/// not in container names.
pub fn container_name(target: &str) -> String {
    let slug: String = target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '-' })
        .collect();
    format!("haven-{}", slug.trim_matches('-'))
}

/// Write the in-container integration, returning nothing useful but an error
/// if it could not be written.
///
/// Best effort by design: the script is *offered* to the container through a
/// read-only mount, the inner shell loads it if it is there, and a container
/// with no bash simply never looks. Nothing inside is modified, and nothing
/// fails because integration did not happen — a pane with no prompt marks is
/// less capable, not broken.
fn install_integration(ctx: &Context) -> std::io::Result<()> {
    std::fs::create_dir_all(&ctx.dir)?;
    let bash = haven_shell::BashShell;
    std::fs::write(ctx.dir.join("haven-integration.bash"), bash.script())?;
    // The container's own startup files first: --rcfile replaces them, and a
    // distro's bashrc is most of what makes its shell feel like that distro.
    std::fs::write(
        ctx.dir.join("container-init.bash"),
        "# Written by Haven. Loaded inside a container pane; see site/src/containers.md.\n\
         [ -f /etc/bash.bashrc ] && . /etc/bash.bashrc\n\
         [ -f \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"\n\
         [ -f /haven/haven-integration.bash ] && . /haven/haven-integration.bash\n\
         return 0\n",
    )?;
    Ok(())
}

/// Shell-quote for the script we are about to write.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `-v` arguments, with `cwd` resolved against the pane's directory.
fn mounts(spec: &Spec, ctx: &Context) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for mount in &spec.mounts {
        let host = if mount.host == "cwd" {
            match ctx.cwd.as_deref().and_then(Path::to_str) {
                Some(cwd) => cwd.to_string(),
                // A pane with no directory has nothing to mount; silently
                // nothing is better than a container that will not start.
                None => continue,
            }
        } else {
            mount.host.clone()
        };
        let dest = mount.dest.clone().unwrap_or_else(|| host.clone());
        out.push((host, dest));
    }
    out
}

/// The command inside the container.
///
/// A distro image's shell is not knowable from outside — `alpine` has ash,
/// `archlinux` has bash, a `nixos/nix` image has bash in an odd place — so
/// the choosing happens in there, where `command -v` can answer. bash is
/// first and gets the integration; everything else gets a plain shell, which
/// is the promise: degrade quietly.
fn inner(integrated: bool, cd_to: Option<&str>) -> String {
    let mut script = String::new();
    if let Some(dir) = cd_to {
        // A `cd` that may fail, rather than `docker exec -w`, which refuses
        // to start when the directory is not there. A container that already
        // exists was created with whatever mounts it was created with, so a
        // pane asking for a different one must still open.
        script.push_str(&format!("cd {} 2>/dev/null; ", quote(dir)));
    }
    if integrated {
        script.push_str(
            "if [ -r /haven/container-init.bash ] && command -v bash >/dev/null 2>&1; then \
             exec bash --rcfile /haven/container-init.bash; fi; ",
        );
    }
    script.push_str(
        "for s in bash zsh ash sh; do \
         if command -v \"$s\" >/dev/null 2>&1; then exec \"$s\"; fi; done; exec /bin/sh",
    );
    script
}

/// The whole pane program: what runs, what it tidies up, and what it says
/// when it cannot.
fn plan(spec: &Spec, ctx: &Context, state: State, integrated: bool) -> String {
    let name = &ctx.settings.cli;
    let mut script = format!("cli={}\n", quote(&ctx.settings.cli));

    // Nothing below works without the CLI, and "command not found" flashing
    // past as the pane closes is not an explanation.
    script.push_str(&format!(
        "if ! command -v \"$cli\" >/dev/null 2>&1; then \
         printf 'haven: {name} is not installed, so this pane is a local shell instead\\n'; \
         exec \"${{SHELL:-/bin/sh}}\"; fi\n"
    ));

    let asked = mounts(spec, ctx);
    let mut argv: Vec<String> = Vec::new();

    // Entering a container that already exists cannot change its
    // directories, so the mounts asked for are not silently dropped: the
    // pane says they were ignored, and still tries to start where you meant.
    let note_fixed_mounts = || {
        if asked.is_empty() {
            return String::new();
        }
        "printf 'haven: this container is already running, so its directories are \
         the ones it started with\\n'\n"
            .to_string()
    };

    match (spec.verb.as_str(), state) {
        ("exec", State::Running) => {
            script.push_str(&note_fixed_mounts());
            argv.push("exec".into());
            argv.push("-it".into());
            for (key, value) in env_args(spec) {
                argv.push("-e".into());
                argv.push(quote(&format!("{key}={value}")));
            }
            argv.push(quote(&spec.target));
            argv.push(format!(
                "sh -c {}",
                quote(&inner(integrated, asked.first().map(|(_, dest)| dest.as_str())))
            ));
        }
        ("exec", _) => {
            // Asked for a container that is no longer up. Starting it would
            // be a different thing from what was asked for.
            script.push_str(&format!(
                "printf 'haven: container %s is not running, so this pane is a local shell\\n' {}\n\
                 exec \"${{SHELL:-/bin/sh}}\"\n",
                quote(&spec.target)
            ));
            return script;
        }
        (_, _) if spec.lifetime == Lifetime::Keep && state == State::Running => {
            // The container is up from an earlier pane: another shell in it.
            script.push_str(&note_fixed_mounts());
            argv.push("exec".into());
            argv.push("-it".into());
            for (key, value) in env_args(spec) {
                argv.push("-e".into());
                argv.push(quote(&format!("{key}={value}")));
            }
            argv.push(quote(&container_name(&spec.target)));
            argv.push(format!(
                "sh -c {}",
                quote(&inner(integrated, asked.first().map(|(_, dest)| dest.as_str())))
            ));
        }
        (_, _) if spec.lifetime == Lifetime::Keep && state == State::Stopped => {
            // Resumes the shell the container was created with, so what was
            // installed in it is still installed.
            script.push_str(&note_fixed_mounts());
            argv.push("start".into());
            argv.push("-ai".into());
            argv.push(quote(&container_name(&spec.target)));
        }
        _ => {
            script.push_str(&pull(spec));
            argv.push("run".into());
            argv.push("-it".into());
            match spec.lifetime {
                Lifetime::Scratch => {
                    argv.push("--rm".into());
                    // Labelled with the pid of the process that is about to
                    // *become* the CLI — `exec` keeps the pid — so a pane
                    // that was killed rather than closed can be cleaned up
                    // afterwards by looking for a pid that is gone.
                    //
                    // This is deliberately not a `trap` in the wrapper, which
                    // was tried and does not work: `SIGHUP` reaches the
                    // container's PID 1, which the kernel does not deliver
                    // default-action signals to, so nothing downstream ever
                    // notices and the shell's trap never runs. Verified by
                    // leaking three containers.
                    argv.push("--name".into());
                    argv.push(format!("{}-$$", quote(&scratch_name(&spec.target))));
                    argv.push("--label".into());
                    argv.push(format!("{SCRATCH_LABEL}=1"));
                    argv.push("--label".into());
                    argv.push(format!("{PANE_LABEL}=$$"));
                }
                Lifetime::Keep => {
                    argv.push("--name".into());
                    argv.push(quote(&container_name(&spec.target)));
                }
            }
            if integrated {
                argv.push("-v".into());
                argv.push(quote(&format!("{}:/haven:ro", ctx.dir.display())));
            }
            for (host, dest) in &asked {
                argv.push("-v".into());
                argv.push(quote(&format!("{host}:{dest}")));
                // Safe here, unlike on a container that already exists: this
                // one is being created with that directory in it.
                argv.push("-w".into());
                argv.push(quote(dest));
            }
            for (key, value) in env_args(spec) {
                argv.push("-e".into());
                argv.push(quote(&format!("{key}={value}")));
            }
            argv.push(quote(&spec.target));
            argv.push(format!("sh -c {}", quote(&inner(integrated, None))));
        }
    }

    script.push_str(&format!("exec \"$cli\" {}\n", argv.join(" ")));
    script
}

/// The name a throwaway container is given so that it can be removed again.
/// The pane's pid is appended by the script, not here.
fn scratch_name(target: &str) -> String {
    format!("{}-scratch", container_name(target))
}

/// Marks a container Haven made for one pane and nothing else.
const SCRATCH_LABEL: &str = "haven.scratch";
/// The pid of the CLI process that pane runs.
const PANE_LABEL: &str = "haven.pane";

/// Throw away scratch containers whose pane is gone.
///
/// `--rm` removes a container when it *exits*, which covers the pane you
/// closed by typing `exit`. It does not cover the pane that was killed: the
/// CLI dies, the container keeps running, and nothing ever tells it
/// otherwise. So each time a container pane opens, the ones whose pane
/// process no longer exists are removed — the leak heals itself, and in the
/// place where a stale container is about to be noticed anyway.
fn reap(cli: &str) {
    let Some(listing) = capture(
        cli,
        &[
            "ps",
            "-a",
            "--filter",
            &format!("label={SCRATCH_LABEL}"),
            "--format",
            &format!("{{{{.ID}}}} {{{{.Label \"{PANE_LABEL}\"}}}}"),
        ],
    ) else {
        return;
    };
    for id in stale(&listing, |pid| Path::new(&format!("/proc/{pid}")).exists()) {
        let _ = capture(cli, &["rm", "-f", &id]);
    }
}

/// Which of `<id> <pid>` lines belong to a pane that is no longer running.
///
/// Split out from [`reap`] because the interesting half is the decision, and
/// the decision is testable without Docker.
fn stale(listing: &str, alive: impl Fn(&str) -> bool) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let (id, pid) = line.trim().split_once(' ')?;
            let pid = pid.trim();
            // A container with no pid label is not ours to judge. Leaving it
            // is the conservative half of the only choice here.
            (!id.is_empty() && !pid.is_empty() && !alive(pid)).then(|| id.to_string())
        })
        .collect()
}

/// The pull, announced before it starts and explained if it fails.
///
/// `docker run` would pull by itself, but only after printing one terse line
/// and then several hundred megabytes of progress with no indication of why.
/// Doing it explicitly means the pane says what is happening, says that it
/// happens once, and ends somewhere usable if it does not work.
fn pull(spec: &Spec) -> String {
    let image = quote(&spec.target);
    format!(
        "if ! \"$cli\" image inspect {image} >/dev/null 2>&1; then \
         printf 'haven: %s is not here yet — pulling it once, then opening a shell\\n' {image}; \
         if ! \"$cli\" pull {image}; then \
         printf '\\nhaven: could not pull %s, so this pane is a local shell instead\\n' {image}; \
         exec \"${{SHELL:-/bin/sh}}\"; fi; fi\n"
    )
}

/// What the shell inside is told about itself.
fn env_args(spec: &Spec) -> Vec<(String, String)> {
    let mut out = vec![
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
        ("TERM_PROGRAM".to_string(), "haven".to_string()),
        ("TERM_PROGRAM_VERSION".to_string(), env!("CARGO_PKG_VERSION").to_string()),
        ("HAVEN_CONTAINER".to_string(), spec.target.clone()),
    ];
    // Without this a distro pane mangles every box-drawing character, which
    // is most of what you look at in a terminal.
    for key in ["LANG", "LC_ALL"] {
        if let Some(value) = std::env::var_os(key).and_then(|v| v.into_string().ok()) {
            out.push((key.to_string(), value));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> Context {
        Context {
            settings: Settings::default(),
            cwd: Some(PathBuf::from("/Projects/HavenTerminal")),
            dir: PathBuf::from("/run/user/1000/haven"),
        }
    }

    fn plan_for(spec: &str, state: State) -> String {
        plan(&Spec::parse(spec).unwrap(), &ctx(), state, true)
    }

    #[test]
    fn a_scratch_container_is_labelled_with_the_pane_that_owns_it() {
        // `--rm` covers the pane you closed by typing `exit`. The pane that
        // was *killed* leaves the CLI dead and the container running — so it
        // carries the pid of the process that becomes that CLI, and the next
        // container pane throws away the ones whose pid is gone.
        let scratch = plan_for("docker://run/alpine:latest?scratch", State::Missing);
        assert!(scratch.contains("--rm"), "{scratch}");
        assert!(scratch.contains("--label haven.scratch=1"), "{scratch}");
        assert!(scratch.contains("--label haven.pane=$$"), "{scratch}");
        // `exec`, so that `$$` really is the CLI's pid rather than a wrapper
        // that is about to exit and take the label's meaning with it.
        assert!(scratch.contains("exec \"$cli\" run"), "{scratch}");
        // And two scratch panes of one image must not fight over a name.
        assert!(scratch.contains("-scratch'-$$"), "{scratch}");
    }

    #[test]
    fn only_a_container_whose_pane_is_gone_is_reaped() {
        let listing = "aaa 1\nbbb 999999\nccc \n\nddd 2\n";
        // Pretend 1 and 2 are alive, 999999 is not.
        let gone = stale(listing, |pid| pid == "1" || pid == "2");
        assert_eq!(gone, vec!["bbb"], "a live pane's container must survive");
        // A container with no pid label is nobody's to remove.
        assert!(!gone.contains(&"ccc".to_string()));
    }

    #[test]
    fn a_kept_container_is_never_reaped_because_it_is_never_labelled() {
        let keep = plan_for("docker://run/alpine:latest?keep", State::Missing);
        assert!(!keep.contains("haven.scratch"), "{keep}");
        assert!(!keep.contains("haven.pane"), "{keep}");
    }

    #[test]
    fn a_kept_container_is_named_and_left_alone() {
        let keep = plan_for("docker://run/alpine:latest?keep", State::Missing);
        assert!(keep.contains("--name 'haven-alpine-latest'"), "{keep}");
        assert!(!keep.contains("--rm"), "a container you keep must not be removed: {keep}");
        assert!(
            !keep.contains("trap"),
            "stopping it would pull the floor from under another pane sharing it: {keep}"
        );
    }

    #[test]
    fn a_kept_container_is_reused_rather_than_made_twice() {
        // The whole point of `keep`: the second pane is another shell in the
        // same container, so what you installed in the first one is there.
        let running = plan_for("docker://run/archlinux:latest?keep", State::Running);
        assert!(running.contains("exec -it"), "{running}");
        assert!(running.contains("'haven-archlinux-latest'"));
        assert!(!running.contains("docker' run"), "it must not start a second container");

        let stopped = plan_for("docker://run/archlinux:latest?keep", State::Stopped);
        assert!(stopped.contains("start -ai"), "{stopped}");
    }

    #[test]
    fn an_image_that_is_not_here_is_pulled_in_the_pane_with_a_word_about_why() {
        // The request this exists for: a pane that sits blank while half a
        // gigabyte arrives looks broken.
        let script = plan_for("docker://run/archlinux:latest?scratch", State::Missing);
        assert!(script.contains("image inspect"), "presence is checked: {script}");
        assert!(script.contains("pull"), "{script}");
        assert!(script.contains("pulling it once"), "it has to say what it is doing: {script}");
        // And a pull that fails leaves something you can type in.
        assert!(script.contains("could not pull"));
        assert!(script.contains("exec \"${SHELL:-/bin/sh}\""));
    }

    #[test]
    fn nothing_is_pulled_for_a_container_that_already_exists() {
        for script in [
            plan_for("docker://exec/web", State::Running),
            plan_for("docker://run/alpine:latest?keep", State::Running),
            plan_for("docker://run/alpine:latest?keep", State::Stopped),
        ] {
            assert!(!script.contains("pull"), "{script}");
        }
    }

    #[test]
    fn a_missing_cli_leaves_a_shell_and_says_so() {
        // Every pane program here begins with this, because the alternative
        // is a pane that opens and closes before the error can be read.
        let script = plan_for("docker://run/alpine:latest?scratch", State::Missing);
        assert!(script.starts_with("cli='docker'\nif ! command -v \"$cli\""), "{script}");
        assert!(script.contains("is not installed"));
    }

    #[test]
    fn mounts_asked_of_a_running_container_are_said_to_be_ignored() {
        // A running container's directories are fixed. Dropping the request
        // in silence would look like the mount command not working.
        let script = plan_for("docker://exec/web?mount=/srv", State::Running);
        assert!(script.contains("already running"), "{script}");
        // It still tries to start where you meant, with a `cd` that may fail
        // rather than a `-w` that refuses to start the shell at all.
        assert!(inner(false, Some("/srv")).starts_with("cd '/srv' 2>/dev/null; "), "{script}");
        assert!(script.contains("/srv"), "{script}");
        assert!(!script.contains("-w"), "a directory that is not there must not cost the pane");
    }

    #[test]
    fn nothing_is_said_about_mounts_when_none_were_asked_for() {
        let script = plan_for("docker://exec/web", State::Running);
        assert!(!script.contains("already running"), "{script}");
        assert!(!script.contains("cd "), "{script}");
    }

    #[test]
    fn a_container_that_stopped_is_reported_not_started() {
        let script = plan_for("docker://exec/web", State::Stopped);
        assert!(script.contains("is not running"), "{script}");
        assert!(!script.contains("start"), "exec must not start it behind your back: {script}");
    }

    #[test]
    fn a_mount_lands_at_the_same_path_and_becomes_the_working_directory() {
        let script = plan_for("docker://run/alpine:latest?scratch&mount=cwd", State::Missing);
        assert!(
            script.contains("-v '/Projects/HavenTerminal:/Projects/HavenTerminal'"),
            "the same path on both sides, so a path in an error means one thing: {script}"
        );
        assert!(script.contains("-w '/Projects/HavenTerminal'"), "{script}");
    }

    #[test]
    fn a_mount_can_be_given_a_different_path_inside() {
        let script =
            plan_for("docker://run/alpine:latest?scratch&mount=/Projects:/work", State::Missing);
        assert!(script.contains("-v '/Projects:/work'"), "{script}");
        assert!(script.contains("-w '/work'"), "{script}");
    }

    #[test]
    fn a_cwd_mount_with_no_directory_is_dropped_rather_than_fatal() {
        let spec = Spec::parse("docker://run/alpine:latest?scratch&mount=cwd").unwrap();
        let ctx = Context { cwd: None, ..ctx() };
        let script = plan(&spec, &ctx, State::Missing, true);
        assert!(!script.contains("-v 'cwd"), "{script}");
        assert!(script.contains("run -it --rm"), "the pane still opens: {script}");
    }

    #[test]
    fn the_integration_is_offered_read_only_and_never_required() {
        let with = plan_for("docker://run/alpine:latest?scratch", State::Missing);
        assert!(with.contains(":/haven:ro'"), "read-only, always: {with}");
        assert!(with.contains("--rcfile /haven/container-init.bash"));
        // And when it could not be written, the inner shell still runs.
        let spec = Spec::parse("docker://run/alpine:latest?scratch").unwrap();
        let without = plan(&spec, &ctx(), State::Missing, false);
        assert!(!without.contains("/haven"), "{without}");
        assert!(without.contains("for s in bash zsh ash sh"), "{without}");
    }

    #[test]
    fn the_shell_inside_is_chosen_inside_because_only_there_can_it_be_known() {
        // alpine has ash and no bash; archlinux has bash. Guessing from the
        // image name would be wrong for every image not on the known list.
        let script = plan_for("docker://run/alpine:latest?scratch", State::Missing);
        assert!(script.contains("command -v \"$s\""), "{script}");
    }

    #[test]
    fn the_container_is_told_what_terminal_it_is_in() {
        let script = plan_for("docker://run/alpine:latest?scratch", State::Missing);
        assert!(script.contains("-e 'TERM=xterm-256color'"), "{script}");
        assert!(script.contains("-e 'COLORTERM=truecolor'"), "{script}");
        assert!(script.contains("-e 'HAVEN_CONTAINER=alpine:latest'"), "{script}");
    }

    #[test]
    fn a_name_with_a_quote_in_it_cannot_break_out_of_the_script() {
        // Image names come from `docker images`, which is as much an outside
        // input as anything else.
        let spec = Spec {
            backend: "docker".into(),
            verb: "run".into(),
            target: "evil';touch /tmp/pwned;'".into(),
            lifetime: Lifetime::Scratch,
            lifetime_given: true,
            mounts: Vec::new(),
        };
        let script = plan(&spec, &ctx(), State::Missing, false);
        // Quoted, every quote of it: the payload is in the script as inert
        // text and nowhere as a second command.
        assert!(script.contains("'evil'\\'';touch /tmp/pwned;'\\'''"), "{script}");
        assert_eq!(
            script.matches('\'').count() % 2,
            0,
            "an odd number of quotes means one of them closed something it should not: {script}"
        );
    }

    #[test]
    fn a_cli_that_is_not_docker_is_used_everywhere_or_nowhere() {
        // podman takes these arguments unchanged, so this is the whole of
        // podman support — provided nothing hardcodes the name.
        let spec = Spec::parse("docker://run/alpine:latest?scratch").unwrap();
        let ctx =
            Context { settings: Settings { cli: "podman".into(), ..Settings::default() }, ..ctx() };
        let script = plan(&spec, &ctx, State::Missing, false);
        assert!(!script.contains("docker"), "{script}");
        // Named once and then referred to, so there is one place for it to
        // be wrong rather than six.
        assert_eq!(script.matches("'podman'").count(), 1, "{script}");
        assert!(script.contains("\"$cli\" image inspect"), "{script}");
    }

    #[test]
    fn a_kept_container_is_named_after_the_image_it_came_from() {
        assert_eq!(container_name("archlinux:latest"), "haven-archlinux-latest");
        assert_eq!(
            container_name("opensuse/tumbleweed:latest"),
            "haven-opensuse-tumbleweed-latest"
        );
        assert_eq!(container_name("ubuntu:24.04"), "haven-ubuntu-24.04");
        // Container names may not start or end with punctuation.
        assert_eq!(container_name(":weird:"), "haven-weird");
    }

    #[test]
    fn running_containers_come_first_then_what_is_pulled_then_what_is_not() {
        let ps = "web\tnginx:latest\tUp 2 hours\n";
        let images = "alpine:latest\t7.8MB\nmy/app:dev\t120MB\n<none>:<none>\t4MB\n";
        let known: Vec<String> =
            ["alpine:latest", "archlinux:latest"].iter().map(|s| s.to_string()).collect();
        let targets = offer(ps, images, &known);

        let labels: Vec<&str> = targets.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, vec!["web", "alpine:latest", "my/app:dev", "archlinux:latest"]);
        assert_eq!(targets[0].spec, "docker://exec/web");
        assert_eq!(targets[0].detail, "nginx:latest · Up 2 hours");
        assert!(!targets[0].choosable, "a container that exists has no lifetime to choose");
        assert_eq!(targets[1].spec, "docker://run/alpine:latest");
        assert!(targets[1].present);
        assert!(!targets[3].present, "archlinux is on the known list but not pulled");
        assert_eq!(targets[3].detail, "not pulled yet");
    }

    #[test]
    fn an_image_already_pulled_is_not_also_offered_as_a_download() {
        let known: Vec<String> = vec!["alpine:latest".to_string()];
        let targets = offer("", "alpine:latest\t7.8MB\n", &known);
        assert_eq!(targets.len(), 1, "{targets:?}");
        assert!(targets[0].present);
    }

    #[test]
    fn intermediate_layers_are_not_offered_as_distros() {
        let targets = offer("", "<none>:<none>\t4MB\nubuntu:<none>\t70MB\n", &[]);
        assert!(targets.is_empty(), "{targets:?}");
    }

    #[test]
    fn nothing_installed_still_gives_a_menu_worth_opening() {
        // The fresh-machine case: no containers, no images, and the list is
        // still the thing that makes the feature discoverable.
        let known: Vec<String> = KNOWN_IMAGES.iter().map(|s| s.to_string()).collect();
        let targets = offer("", "", &known);
        assert_eq!(targets.len(), KNOWN_IMAGES.len());
        assert!(targets.iter().all(|t| !t.present));
    }
}
