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
        let out = std::process::Command::new(program).arg("--version").output().ok()?;
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

        // The shim must restore the real ZDOTDIR before sourcing the user's
        // files, or plugin managers resolve paths against ours.
        let user_zdotdir =
            std::env::var("ZDOTDIR").ok().map(PathBuf::from).or_else(|| ctx.home.clone());
        let user_dir = user_zdotdir.unwrap_or_else(|| PathBuf::from("~"));
        for rc in ["zshrc", "zshenv", "zprofile", "zlogin", "zlogout"] {
            let body = if rc == "zshrc" {
                format!(
                    "ZDOTDIR={user}\n\
                     [[ -f {user}/.{rc} ]] && source {user}/.{rc}\n\
                     source {script}\n",
                    user = user_dir.display(),
                    script = script.display(),
                )
            } else {
                format!(
                    "ZDOTDIR={user}\n[[ -f {user}/.{rc} ]] && source {user}/.{rc}\n",
                    user = user_dir.display(),
                )
            };
            materialize(&zdotdir, &format!(".{rc}"), &body)?;
        }
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
                home = home.display(),
                script = script.display(),
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

/// Pick the implementation for a program path, falling back to [`GenericShell`].
pub fn for_program(program: &Path) -> Box<dyn Shell> {
    all().into_iter().find(|s| s.matches(program)).unwrap_or_else(|| {
        let name = program.file_name().and_then(|n| n.to_str()).unwrap_or("sh").to_string();
        Box::new(GenericShell { name })
    })
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
        assert!(zshrc.contains("ZDOTDIR=/home/test"), "{zshrc}");
        assert!(zshrc.contains("source /home/test/.zshrc"), "{zshrc}");
        assert!(zshrc.contains("haven-integration.zsh"), "{zshrc}");
        assert_eq!(cfg.env[0].0, "ZDOTDIR");
    }

    #[test]
    fn bash_chains_via_rcfile_when_not_a_login_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = PtyConfig::default();
        assert_eq!(BashShell.install(&mut cfg, &ctx(tmp.path(), false)).unwrap(), Injection::Auto);
        assert_eq!(cfg.args[0], "--rcfile");
        let rcfile = fs::read_to_string(&cfg.args[1]).unwrap();
        assert!(rcfile.contains("/home/test/.bashrc"), "{rcfile}");
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
