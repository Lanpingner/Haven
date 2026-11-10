//! Shell detection, launch environment and per-shell integration.
//! See `docs/PLAN.md` §9.
//!
//! The per-shell differences (hooks, and above all *how* integration gets
//! loaded without editing the user's dotfiles) are abstracted behind the
//! [`Shell`] trait — Rust's equivalent of an abstract base class. Adding a new
//! shell means adding one impl and one entry in [`all`]; nothing else branches
//! on shell identity.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use haven_pty::PtyConfig;

pub mod history;

/// How integration reached the shell, which the UI needs to report honestly.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Injection {
    /// Loaded with no change to the user's configuration.
    Auto,
    /// We cannot load it without the user's consent; `snippet` is the line to
    /// add to `rc_path`.
    Manual { snippet: String, rc_path: PathBuf },
    /// The shell already emits everything we would have added, so we added
    /// nothing. Reported distinctly from [`Injection::Auto`] because "we did
    /// it" and "it was already there" are different facts to debug.
    Native { note: String },
    /// This shell has no integration support.
    Unsupported,
}

/// What a shell needs to know to materialize its integration.
#[derive(Clone, Debug)]
pub struct IntegrationCtx {
    /// The resolved shell binary. Needed because some shells have to be asked
    /// their version before we know whether to inject anything at all.
    pub program: PathBuf,
    /// A directory we own and may write to, e.g. `$XDG_RUNTIME_DIR/haven`.
    pub dir: PathBuf,
    /// A login shell reads a different startup file, which changes whether
    /// auto-injection is possible at all (notably for bash).
    pub login: bool,
    /// The user's home, for locating real rc files to chain.
    pub home: Option<PathBuf>,
}

/// One supported shell.
///
/// Implementors describe *their* startup contract; the caller never matches on
/// which shell it has.
pub trait Shell: fmt::Debug + Send + Sync {
    /// Stable identifier, also used in `$HAVEN_SHELL`.
    fn name(&self) -> &'static str;

    /// Does this implementation handle the given program path?
    fn matches(&self, program: &Path) -> bool {
        program.file_name().and_then(|n| n.to_str()).is_some_and(|n| n == self.name())
    }

    /// The integration script source for this shell.
    fn script(&self) -> &'static str;

    /// Write the script and set whatever environment loads it, mutating `cfg`.
    ///
    /// Must be side-effect-free on the user's files, and must leave `cfg`
    /// usable when it returns [`Injection::Manual`] or [`Injection::Unsupported`]
    /// — a failed injection is a no-op, never a broken shell.
    fn install(&self, cfg: &mut PtyConfig, ctx: &IntegrationCtx) -> io::Result<Injection>;

    /// Command-line arguments to run a single command and exit, for tests and
    /// for `haven send-keys`-style one-shots.
    fn command_args(&self, command: &str) -> Vec<OsString> {
        vec!["-c".into(), command.into()]
    }
}

/// Shared helper: write `script` to `dir/rel` and return the path.
fn materialize(dir: &Path, rel: &str, script: &str) -> io::Result<PathBuf> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, script)?;
    Ok(path)
}

/// Run a program, wait for it, and give up after `limit`.
///
/// Every external command Haven runs synchronously needs one of these.
/// `Command::output()` waits forever, and the programs being asked are
/// exactly the ones that hang: a container CLI whose daemon is not
/// running can block for a minute or never answer at all, and on the
/// daemon's thread that is the terminal freezing. It was `docker
/// inspect` during a session restore that showed this up — a saved
/// container pane made the restore wait on Docker Desktop.
///
/// `None` means it could not be run, failed, or took too long; the
/// callers here treat all three the same, because a menu of containers
/// that cannot be built is an empty menu either way.
pub fn output_within(
    program: &str,
    args: &[&str],
    limit: std::time::Duration,
) -> Option<std::process::Output> {
    use std::process::{Command, Stdio};

    let mut child = Command::new(program)
        .args(args)
        // No terminal and no input: a program that asks a question gets
        // end-of-file rather than waiting for an answer nobody will give.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(_) => return None,
        }
    }
    child.wait_with_output().ok()
}

/// How long any of these probes may take.
///
/// Generous for a question like "is this container running", and short
/// enough that a terminal does not appear to hang while one is asked.
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// A path as a shell can safely read it.
///
/// The shims are generated shell code with paths substituted into them,
/// and the paths were substituted bare. A home directory or runtime
/// directory containing a space, a parenthesis or a `$` therefore
/// produced a shim that did not parse, and a shell with no integration
/// and no complaint — found when a test used a directory with
/// parentheses in the name and the shell silently reported nothing.
///
/// Single quotes, because inside them a shell interprets nothing at all.
/// The one character that cannot appear is `'`, so each is closed,
/// escaped and reopened, which is the standard trick and handles every
/// path a filesystem can hold.
fn shell_quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn prepend_path_env(cfg: &mut PtyConfig, key: &str, value: &Path) {
    let existing = std::env::var_os(key);
    let mut combined = OsString::from(value);
    if let Some(existing) = existing {
        if !existing.is_empty() {
            combined.push(":");
            combined.push(existing);
        }
    }
    cfg.env.push((key.into(), combined));
}

/// Ask fish its version. Cached *per path*: once per shell binary per session,
/// not once per process — a single global cache would answer for one fish and
/// then hand that answer to every other shell path.
///
/// A failure to parse means "assume old", which is the safe direction: the
/// script's own version guard then catches it.
fn fish_version(program: &Path) -> Option<(u32, u32)> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// `(major, minor)`, or `None` when the binary could not be interrogated.
    type Version = Option<(u32, u32)>;
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Version>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(program).copied()) {
        return hit;
    }

    let parsed = (|| {
        // Bounded, like every other program Haven asks a question of: a
        // shell that does not answer `--version` must not hold up the
        // pane that wanted one.
        let out = output_within(&program.to_string_lossy(), &["--version"], PROBE_TIMEOUT)?;
        let text = String::from_utf8_lossy(&out.stdout);
        // "fish, version 4.9.3"
        let version = text.split_whitespace().last()?;
        let mut parts = version.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
        Some((major, minor))
    })();

    if let Ok(mut c) = cache.lock() {
        c.insert(program.to_path_buf(), parsed);
    }
    parsed
}

// ---------------------------------------------------------------------------

/// fish: a documented vendor directory, so nothing of the user's is touched.
#[derive(Debug, Default)]
pub struct FishShell;

/// fish emits OSC 133 and OSC 7 natively from 3.7 on. Verified against fish
/// 4.9.3: it emits A, B, C and D (with `click_events` and `cmdline_url`
/// extensions) and OSC 7 with no integration at all.
const FISH_NATIVE_OSC133: (u32, u32) = (3, 7);

impl Shell for FishShell {
    fn name(&self) -> &'static str {
        "fish"
    }

    fn script(&self) -> &'static str {
        include_str!("scripts/haven.fish")
    }

    fn install(&self, cfg: &mut PtyConfig, ctx: &IntegrationCtx) -> io::Result<Injection> {
        // Installing on a fish that already emits marks yields two command-end
        // marks per command, which is worse than no integration: consumers
        // cannot tell which D belongs to which command.
        if let Some((major, minor)) = fish_version(&ctx.program) {
            if (major, minor) >= FISH_NATIVE_OSC133 {
                return Ok(Injection::Native {
                    note: format!("fish {major}.{minor} emits OSC 133 and OSC 7 itself"),
                });
            }
        }
        // fish reads `fish/vendor_conf.d/*.fish` from every XDG_DATA_DIRS entry,
        // which is the cleanest injection point of any shell.
        let root = ctx.dir.join("fish-data");
        materialize(&root, "fish/vendor_conf.d/haven.fish", self.script())?;
        prepend_path_env(cfg, "XDG_DATA_DIRS", &root);
        Ok(Injection::Auto)
    }

    fn command_args(&self, command: &str) -> Vec<OsString> {
        vec!["-c".into(), command.into()]
    }
}

/// zsh: a `ZDOTDIR` shim that chains the user's real `.zshrc`.
#[derive(Debug, Default)]
pub struct ZshShell;

impl Shell for ZshShell {
    fn name(&self) -> &'static str {
        "zsh"
    }

    fn script(&self) -> &'static str {
        include_str!("scripts/haven.zsh")
    }

    fn install(&self, cfg: &mut PtyConfig, ctx: &IntegrationCtx) -> io::Result<Injection> {
        let zdotdir = ctx.dir.join("zdotdir");
        let script = materialize(&zdotdir, "haven-integration.zsh", self.script())?;

        // The shim has to do two things that pull against each other: the
        // user's files must see the *real* `ZDOTDIR`, or plugin managers
        // resolve their paths against ours — and zsh must keep reading
        // *our* files, which it finds through `ZDOTDIR` as it stands when
        // it goes looking for each one.
        //
        // The first version restored the real value in `.zshenv`, which
        // is the first file read. So by the time zsh looked for a
        // `.zshrc` it was pointing at the user's directory: it read
        // theirs, never ours, and the integration was never sourced.
        // zsh integration had never worked. Nothing noticed because the
        // Linux CI has no zsh installed, so only the macOS job ran that
        // test, and the macOS job had been failing for other reasons for
        // longer.
        //
        // So the real value is set *around* each `source` and put back
        // afterwards, leaving `ZDOTDIR` ours for the next file zsh wants.
        // `.zshrc` is the exception: it is the last of ours that matters,
        // so it leaves the real value in place for the session — which
        // also means a login shell reads the user's `.zlogin`, as it
        // should.
        // The real `ZDOTDIR` if the user set one, else their home. `~`
        // only as a last resort: zsh expands it, and a shim that points
        // nowhere is better than one that points at ours.
        let user_dir = std::env::var("ZDOTDIR")
            .ok()
            // Set but empty is not a directory. `.ok()` alone accepted
            // one and produced a shim that sourced `''/.zshrc`.
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| ctx.home.clone())
            .unwrap_or_else(|| PathBuf::from("~"));
        let user = shell_quoted(&user_dir);
        for rc in ["zshenv", "zprofile", "zlogin", "zlogout"] {
            let body = format!(
                "__haven_zdotdir=${{ZDOTDIR}}\n\
                 ZDOTDIR={user}\n\
                 [[ -f {user}/.{rc} ]] && source {user}/.{rc}\n\
                 ZDOTDIR=${{__haven_zdotdir}}\n\
                 unset __haven_zdotdir\n"
            );
            materialize(&zdotdir, &format!(".{rc}"), &body)?;
        }
        let body = format!(
            "ZDOTDIR={user}\n\
             [[ -f {user}/.zshrc ]] && source {user}/.zshrc\n\
             source {script}\n",
            script = shell_quoted(&script),
        );
        materialize(&zdotdir, ".zshrc", &body)?;

        cfg.env.push(("ZDOTDIR".into(), zdotdir.into_os_string()));
        Ok(Injection::Auto)
    }
}

/// bash: `--rcfile` chaining for interactive non-login shells, and an honest
/// manual instruction for login shells, which read `.bash_profile` instead and
/// cannot be hooked without rewriting the user's files.
#[derive(Debug, Default)]
pub struct BashShell;

impl Shell for BashShell {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn script(&self) -> &'static str {
        include_str!("scripts/haven.bash")
    }

    fn install(&self, cfg: &mut PtyConfig, ctx: &IntegrationCtx) -> io::Result<Injection> {
        let script = materialize(&ctx.dir, "haven-integration.bash", self.script())?;
        cfg.env.push(("HAVEN_BASH_INTEGRATION".into(), script.clone().into_os_string()));

        if ctx.login {
            return Ok(Injection::Manual {
                snippet: "[ -n \"$HAVEN_BASH_INTEGRATION\" ] && . \"$HAVEN_BASH_INTEGRATION\""
                    .to_string(),
                rc_path: ctx.home.clone().unwrap_or_default().join(".bash_profile"),
            });
        }

        let home = ctx.home.clone().unwrap_or_default();
        let rcfile = materialize(
            &ctx.dir,
            "bash-rcfile",
            &format!(
                "[ -f {home}/.bashrc ] && . {home}/.bashrc\n. {script}\n",
                home = shell_quoted(&home),
                script = shell_quoted(&script),
            ),
        )?;
        cfg.args.insert(0, "--rcfile".into());
        cfg.args.insert(1, rcfile.into_os_string());
        Ok(Injection::Auto)
    }
}

/// Anything else: run it, integrate nothing, claim nothing.
#[derive(Debug)]
pub struct GenericShell {
    name: String,
}

impl Shell for GenericShell {
    fn name(&self) -> &'static str {
        "sh"
    }

    fn matches(&self, _program: &Path) -> bool {
        true
    }

    fn script(&self) -> &'static str {
        ""
    }

    fn install(&self, _cfg: &mut PtyConfig, _ctx: &IntegrationCtx) -> io::Result<Injection> {
        Ok(Injection::Unsupported)
    }
}

impl GenericShell {
    pub fn program_name(&self) -> &str {
        &self.name
    }
}

/// Every shell with real integration support, in match order.
pub fn all() -> Vec<Box<dyn Shell>> {
    vec![Box::new(FishShell), Box::new(ZshShell), Box::new(BashShell)]
}

/// Whether a path is a shell rather than some other program.
///
/// `/etc/shells` is the list the system keeps for exactly this question, and
/// the names Haven has integration for are checked too, since a shell built
/// from source is often in neither.
pub fn is_shell(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if ["fish", "zsh", "bash", "sh", "dash", "nu", "elvish", "ksh", "tcsh", "csh"].contains(&name) {
        return true;
    }
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let target = real(path);
    std::fs::read_to_string("/etc/shells").is_ok_and(|text| {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .any(|l| real(Path::new(l)) == target)
    })
}

/// Pick the implementation for a program path, falling back to [`GenericShell`].
pub fn for_program(program: &Path) -> Box<dyn Shell> {
    all().into_iter().find(|s| s.matches(program)).unwrap_or_else(|| {
        let name = program.file_name().and_then(|n| n.to_str()).unwrap_or("sh").to_string();
        Box::new(GenericShell { name })
    })
}

/// Every shell this machine will admit to having, most useful first.
///
/// Three sources, in order: the shell you are already using, the ones Haven
/// has integration for, and whatever else `/etc/shells` lists. The order
/// matters more than the contents — `/etc/shells` is also where `git-shell`
/// and the systemd fallback live, which are real shells nobody means to
/// pick, and dropping them would be guessing at a list the system owns.
pub fn available() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let add = |found: &mut Vec<PathBuf>, path: PathBuf| {
        if !path.is_file() {
            return;
        }
        // Resolved before the duplicate check: /bin is a symlink to /usr/bin
        // on most systems now, so the same shell appears twice under two
        // names unless they are compared by what they point at.
        let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let target = real(&path);
        if !found.iter().any(|p| real(p) == target) {
            found.push(path);
        }
    };

    add(&mut found, resolve(None));
    for name in ["fish", "zsh", "bash", "sh", "nu", "elvish", "dash", "ksh", "tcsh"] {
        for dir in ["/usr/bin", "/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
            add(&mut found, Path::new(dir).join(name));
        }
    }
    if let Ok(text) = std::fs::read_to_string("/etc/shells") {
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                add(&mut found, PathBuf::from(line));
            }
        }
    }
    found
}

/// Resolution order from `docs/PLAN.md` §9.2: config, then `$SHELL`, then the
/// passwd entry, then `/bin/sh`. The passwd lookup matters because `$SHELL` is
/// frequently stale or absent under display managers.
pub fn resolve(configured: Option<&Path>) -> PathBuf {
    if let Some(p) = configured {
        return p.to_path_buf();
    }
    if let Some(shell) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
        return PathBuf::from(shell);
    }
    passwd_shell().unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn passwd_shell() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getpwuid returns a pointer into a static buffer owned by libc;
    // we copy out of it immediately and never retain it.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return None;
        }
        let bytes = std::ffi::CStr::from_ptr((*pw).pw_shell).to_bytes();
        if bytes.is_empty() {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes).to_os_string()))
    }
}

/// Build the launch config for a pane: shell, login semantics, environment,
/// and integration. This is the single place that composes §9.2 and §9.3.
pub fn launch_config(
    program: Option<&Path>,
    login: bool,
    integrate: bool,
    ctx: &IntegrationCtx,
) -> (PtyConfig, Box<dyn Shell>, Injection) {
    let path = resolve(program);
    let shell = for_program(&path);
    // `$SHELL` is inherited from whatever started the daemon, so a pane
    // opened as bash from a session started under fish would say fish — and
    // everything that spawns a subshell through `$SHELL`, from `vim :sh` to
    // `git`, would get the wrong one. It is only corrected for a program
    // that *is* a shell: `haven -- mc` must not claim `SHELL=/usr/bin/mc`.
    let shell_var = is_shell(&path).then(|| path.clone());

    let ctx = IntegrationCtx { program: path.clone(), ..ctx.clone() };
    let mut cfg = PtyConfig {
        program: Some(path),
        login,
        env: vec![
            // TERM stays xterm-256color until our terminfo entry is everywhere
            // (docs/PLAN.md §8); claiming TERM=haven breaks mc over ssh.
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("TERM_PROGRAM".into(), "haven".into()),
            ("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into()),
            ("HAVEN_SHELL".into(), shell.name().into()),
        ],
        // Stale LINES/COLUMNS break resize for curses apps.
        env_remove: vec!["LINES".into(), "COLUMNS".into()],
        ..Default::default()
    };

    if let Some(path) = shell_var {
        cfg.env.push(("SHELL".into(), path.into_os_string()));
    }

    let injection = if integrate {
        // An integration failure must be harmless, never fatal: a shell with no
        // marks is merely less capable, a shell that will not start is a bug.
        shell.install(&mut cfg, &ctx).unwrap_or(Injection::Unsupported)
    } else {
        Injection::Unsupported
    };
    (cfg, shell, injection)
}

#[cfg(test)]
mod shell_var_tests {
    use super::*;

    fn env_of(cfg: &PtyConfig, key: &str) -> Option<String> {
        cfg.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.to_string_lossy().into_owned())
    }

    fn ctx() -> IntegrationCtx {
        IntegrationCtx {
            program: PathBuf::from("/bin/sh"),
            dir: std::env::temp_dir(),
            login: false,
            home: None,
        }
    }

    #[test]
    fn a_pane_says_which_shell_it_is_actually_running() {
        // Inherited from whatever started the daemon otherwise, so a bash
        // pane in a session started under fish reported fish — and anything
        // spawning a subshell through `$SHELL` got the wrong one.
        let Some(bash) = ["/bin/bash", "/usr/bin/bash"].iter().map(Path::new).find(|p| p.is_file())
        else {
            return;
        };
        let (cfg, _, _) = launch_config(Some(bash), false, false, &ctx());
        assert_eq!(env_of(&cfg, "SHELL").as_deref(), bash.to_str());
    }

    #[test]
    fn a_program_that_is_not_a_shell_does_not_claim_to_be_one() {
        // `haven -- mc` must not leave `SHELL=/usr/bin/mc` behind for every
        // program in the pane to trip over.
        let Some(mc) = ["/usr/bin/mc", "/bin/mc"].iter().map(Path::new).find(|p| p.is_file())
        else {
            return;
        };
        let (cfg, _, _) = launch_config(Some(mc), false, false, &ctx());
        assert_eq!(env_of(&cfg, "SHELL"), None);
    }

    #[test]
    fn shells_are_recognised_by_name_and_by_etc_shells() {
        assert!(is_shell(Path::new("/usr/bin/fish")));
        assert!(is_shell(Path::new("/some/where/zsh")));
        assert!(!is_shell(Path::new("/usr/bin/mc")));
        assert!(!is_shell(Path::new("/usr/bin/vim")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path, login: bool) -> IntegrationCtx {
        IntegrationCtx {
            program: PathBuf::from("/nonexistent/shell"),
            dir: dir.to_path_buf(),
            login,
            home: Some(PathBuf::from("/home/test")),
        }
    }

    #[test]
    fn detection_dispatches_on_program_name() {
        assert_eq!(for_program(Path::new("/usr/bin/fish")).name(), "fish");
        assert_eq!(for_program(Path::new("/bin/zsh")).name(), "zsh");
        assert_eq!(for_program(Path::new("/usr/local/bin/bash")).name(), "bash");
        // Unknown shells still run; they just get no integration.
        let other = for_program(Path::new("/usr/bin/nu"));
        assert_eq!(other.name(), "sh");
        assert_eq!(other.script(), "");
    }

    #[test]
    fn every_shell_script_emits_the_marks_it_claims() {
        for shell in all() {
            let s = shell.script();
            // B is the one mark fish < 3.7 cannot emit (no post-prompt hook),
            // and it is native from 3.7 on, so it is not required there.
            let required: &[&str] = if shell.name() == "fish" {
                &["133;A", "133;C", "133;D"]
            } else {
                &["133;A", "133;B", "133;C", "133;D"]
            };
            for mark in required {
                assert!(s.contains(mark), "{} is missing {mark}", shell.name());
            }
            assert!(s.contains("]7;file://"), "{} is missing OSC 7", shell.name());
        }
    }

    #[test]
    fn fish_with_native_marks_installs_nothing() {
        // The real fish on this machine; skipped where fish is absent.
        let Some(program) = ["/usr/bin/fish", "/usr/local/bin/fish", "/bin/fish"]
            .into_iter()
            .map(Path::new)
            .find(|p| p.exists())
        else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = PtyConfig::default();
        let mut c = ctx(tmp.path(), false);
        c.program = program.to_path_buf();
        match FishShell.install(&mut cfg, &c).unwrap() {
            Injection::Native { .. } => {
                // Nothing written, nothing in the environment: double marks are
                // worse than none at all.
                assert!(!tmp.path().join("fish-data").exists());
                assert!(cfg.env.is_empty());
            }
            Injection::Auto => {
                // Only acceptable on fish < 3.7.
                assert!(tmp.path().join("fish-data/fish/vendor_conf.d/haven.fish").is_file());
            }
            other => panic!("unexpected injection for fish: {other:?}"),
        }
    }

    #[test]
    fn fish_installs_a_vendor_dir_and_touches_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = PtyConfig::default();
        // ctx() points at a nonexistent binary, so version detection fails and
        // we take the conservative "assume old fish" path.
        let injection = FishShell.install(&mut cfg, &ctx(tmp.path(), false)).unwrap();
        assert_eq!(injection, Injection::Auto);
        assert!(tmp.path().join("fish-data/fish/vendor_conf.d/haven.fish").is_file());
        let (key, value) = &cfg.env[0];
        assert_eq!(key, "XDG_DATA_DIRS");
        assert!(value.to_string_lossy().contains("fish-data"));
        assert!(cfg.args.is_empty(), "fish needs no argv changes");
    }

    #[test]
    fn zsh_shim_chains_the_users_rc_and_restores_zdotdir() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = PtyConfig::default();
        assert_eq!(ZshShell.install(&mut cfg, &ctx(tmp.path(), false)).unwrap(), Injection::Auto);
        let zshrc = fs::read_to_string(tmp.path().join("zdotdir/.zshrc")).unwrap();
        // Quoted, because the path is substituted into shell code and a
        // path may contain anything.
        assert!(zshrc.contains("ZDOTDIR='/home/test'"), "{zshrc}");
        assert!(zshrc.contains("source '/home/test'/.zshrc"), "{zshrc}");
        assert!(zshrc.contains("haven-integration.zsh"), "{zshrc}");
        assert_eq!(cfg.env[0].0, "ZDOTDIR");

        // And the invariant the first version got wrong: every file
        // *before* `.zshrc` must leave `ZDOTDIR` pointing back at ours,
        // or zsh goes looking for the next one in the user's directory
        // and never reads ours at all. That is not a style point — it is
        // the difference between the integration loading and not, and it
        // cannot be caught by a test that only looks at `.zshrc`.
        for rc in ["zshenv", "zprofile"] {
            let body = fs::read_to_string(tmp.path().join(format!("zdotdir/.{rc}"))).unwrap();
            assert!(
                body.contains("ZDOTDIR=${__haven_zdotdir}"),
                ".{rc} must hand ZDOTDIR back, or zsh stops reading our files:\n{body}"
            );
            let restore = body.find("ZDOTDIR=${__haven_zdotdir}").unwrap();
            let chain = body.find("source '/home/test'/").unwrap();
            assert!(chain < restore, ".{rc} restores before chaining:\n{body}");
        }
    }

    #[test]
    fn an_empty_zdotdir_is_not_a_directory() {
        // Set but empty. `std::env::var(..).ok()` accepts it, and the
        // shim then sourced `''/.zshrc`.
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = PtyConfig::default();
        let before = std::env::var_os("ZDOTDIR");
        // SAFETY: setting an environment variable is unsound only in the
        // presence of another thread reading one. This test does its own
        // work inline and restores the value before returning, and no
        // other test here touches `ZDOTDIR`.
        unsafe { std::env::set_var("ZDOTDIR", "") };
        ZshShell.install(&mut cfg, &ctx(tmp.path(), false)).unwrap();
        match before {
            // SAFETY: as above, putting back what was there.
            Some(value) => unsafe { std::env::set_var("ZDOTDIR", value) },
            // SAFETY: as above.
            None => unsafe { std::env::remove_var("ZDOTDIR") },
        }
        let zshrc = fs::read_to_string(tmp.path().join("zdotdir/.zshrc")).unwrap();
        assert!(!zshrc.contains("ZDOTDIR=''"), "an empty ZDOTDIR became a path:\n{zshrc}");
        assert!(zshrc.contains("/home/test"), "it should fall back to the home:\n{zshrc}");
    }

    #[test]
    fn bash_chains_via_rcfile_when_not_a_login_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = PtyConfig::default();
        assert_eq!(BashShell.install(&mut cfg, &ctx(tmp.path(), false)).unwrap(), Injection::Auto);
        assert_eq!(cfg.args[0], "--rcfile");
        let rcfile = fs::read_to_string(&cfg.args[1]).unwrap();
        assert!(rcfile.contains("'/home/test'/.bashrc"), "{rcfile}");
        assert!(rcfile.contains("haven-integration.bash"), "{rcfile}");
    }

    #[test]
    fn bash_login_shell_is_manual_rather_than_pretending() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = PtyConfig::default();
        let injection = BashShell.install(&mut cfg, &ctx(tmp.path(), true)).unwrap();
        match injection {
            Injection::Manual { rc_path, snippet } => {
                assert!(rc_path.ends_with(".bash_profile"));
                assert!(snippet.contains("HAVEN_BASH_INTEGRATION"));
            }
            other => panic!("login bash must not claim auto-injection: {other:?}"),
        }
        // Even when manual, the shell must still launch cleanly.
        assert!(cfg.args.is_empty());
    }

    #[test]
    fn launch_config_sets_env_and_scrubs_stale_geometry() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, shell, _) =
            launch_config(Some(Path::new("/nonexistent/fish")), true, true, &ctx(tmp.path(), true));
        assert_eq!(shell.name(), "fish");
        assert!(cfg.login);
        let env: Vec<_> = cfg
            .env
            .iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
            .collect();
        assert!(env.contains(&("TERM".into(), "xterm-256color".into())));
        assert!(env.contains(&("COLORTERM".into(), "truecolor".into())));
        assert!(env.iter().any(|(k, _)| k == "XDG_DATA_DIRS"));
        assert!(cfg.env_remove.iter().any(|k| k == "LINES"));
        assert!(cfg.env_remove.iter().any(|k| k == "COLUMNS"));
    }

    #[test]
    fn integration_can_be_declined_entirely() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, injection) = launch_config(
            Some(Path::new("/nonexistent/fish")),
            false,
            false,
            &ctx(tmp.path(), false),
        );
        assert_eq!(injection, Injection::Unsupported);
        assert!(!cfg.env.iter().any(|(k, _)| k == "XDG_DATA_DIRS"));
    }
}

#[cfg(test)]
mod quoting_tests {
    use super::*;

    /// The shims are generated shell code with paths substituted in, and
    /// a path is whatever the filesystem allows. Checked by handing the
    /// result to `bash -n`, which is the only authority on whether a
    /// bash file parses.
    #[test]
    fn a_directory_a_shell_would_choke_on_still_produces_a_shim_that_parses() {
        let awkward = std::env::temp_dir()
            .join(format!("haven shell (test) 'quoted' {}", std::process::id()));
        let home = awkward.join("home dir");
        std::fs::create_dir_all(&home).expect("home");

        let ctx = IntegrationCtx {
            program: PathBuf::from("/bin/bash"),
            dir: awkward.clone(),
            login: false,
            home: Some(home.clone()),
        };
        let mut cfg = PtyConfig { size: Default::default(), ..Default::default() };
        let injection = BashShell.install(&mut cfg, &ctx).expect("install");
        assert_eq!(injection, Injection::Auto);

        let rcfile = awkward.join("bash-rcfile");
        let body = std::fs::read_to_string(&rcfile).expect("rcfile");
        assert!(body.contains('\''), "the paths should be quoted:\n{body}");

        // The real test: bash's own parser.
        if let Some(bash) =
            ["/bin/bash", "/usr/bin/bash"].into_iter().find(|p| Path::new(p).exists())
        {
            let out =
                std::process::Command::new(bash).arg("-n").arg(&rcfile).output().expect("bash -n");
            assert!(
                out.status.success(),
                "bash cannot parse the shim it would be given:\n{body}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let _ = std::fs::remove_dir_all(&awkward);
    }

    #[test]
    fn quoting_survives_a_quote_in_the_path() {
        // The one character single quotes cannot contain. Closed,
        // escaped and reopened, which is the standard trick.
        let quoted = shell_quoted(Path::new("/tmp/it's here"));
        assert_eq!(quoted, "'/tmp/it'\\''s here'");
        if let Some(bash) =
            ["/bin/bash", "/usr/bin/bash"].into_iter().find(|p| Path::new(p).exists())
        {
            let out = std::process::Command::new(bash)
                .args(["-c", &format!("printf '%s' {quoted}")])
                .output()
                .expect("bash");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "/tmp/it's here");
        }
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The property the daemon depends on: a program that does not finish
    /// is given up on, rather than waited for. `docker` with no daemon
    /// behind it is this, and it used to be asked during a session
    /// restore on the thread that answers clients.
    #[test]
    fn a_program_that_will_not_finish_is_given_up_on() {
        let started = Instant::now();
        let out = output_within("/bin/sh", &["-c", "sleep 60"], Duration::from_millis(300));
        let waited = started.elapsed();
        assert!(out.is_none(), "a program that outlived its deadline should give nothing");
        assert!(waited < Duration::from_secs(5), "waited {waited:?} for a 300ms deadline");
    }

    #[test]
    fn a_program_that_finishes_is_not_interfered_with() {
        let out = output_within("/bin/sh", &["-c", "printf hello"], Duration::from_secs(5))
            .expect("it finishes well inside the deadline");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello");
    }

    #[test]
    fn a_program_that_is_not_there_is_nothing_rather_than_a_panic() {
        assert!(output_within("haven-no-such-program", &[], Duration::from_secs(1)).is_none());
    }

    #[test]
    fn a_program_that_waits_for_input_is_not_waited_for() {
        // stdin is closed, so `cat` reads end-of-file and exits rather
        // than sitting there until the deadline.
        let started = Instant::now();
        let out = output_within("/bin/cat", &[], Duration::from_secs(5));
        assert!(out.is_some(), "it should exit on end-of-file, not time out");
        assert!(started.elapsed() < Duration::from_secs(4), "it waited for input");
    }
}
