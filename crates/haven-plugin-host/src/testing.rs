//! Running a real plugin in a test, with no terminal and no daemon.
//!
//! This is the half of the design that makes plugins testable: a plugin
//! returns commands rather than performing them, and `render` is pure, so
//! "what does this plugin do" is answered by calling it with a made-up
//! session and comparing what comes back.
//!
//! The components are built from `plugins/` on demand rather than
//! committed. The WIT world and the SDK both live in this repository, so a
//! committed `.wasm` would go stale the first time either changed, and
//! nothing would notice.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use haven_plugin::index::Capabilities;

use crate::{Engine, Load, Loaded, Pane, Snapshot};

/// A session a plugin can be shown, built by hand.
#[derive(Clone, Debug, Default)]
pub struct Session {
    panes: Vec<Pane>,
    text: BTreeMap<u32, String>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// One pane. The first added is the focused one unless another is
    /// marked, which is what a plugin asking for "where am I" expects.
    pub fn pane(mut self, id: u32, title: &str, cwd: &str) -> Self {
        let focused = self.panes.is_empty();
        self.panes.push(Pane {
            id,
            title: title.into(),
            command: String::new(),
            cwd: cwd.into(),
            running: false,
            focused,
        });
        self
    }

    /// A pane that is running something, as shell integration reports.
    pub fn running(self, id: u32, title: &str, cwd: &str, command: &str) -> Self {
        self.with_command(id, title, cwd, command, true)
    }

    /// A pane whose last command has *finished*.
    ///
    /// Separate from [`Session::running`] because `command` and `running`
    /// are different facts and a plugin may key off either: shell
    /// integration keeps reporting the last command after it ends, so
    /// "ran `cargo build`, not running now" is an ordinary state that
    /// could not be built before this existed.
    pub fn ran(self, id: u32, title: &str, cwd: &str, command: &str) -> Self {
        self.with_command(id, title, cwd, command, false)
    }

    fn with_command(
        mut self,
        id: u32,
        title: &str,
        cwd: &str,
        command: &str,
        running: bool,
    ) -> Self {
        let focused = self.panes.is_empty();
        self.panes.push(Pane {
            id,
            title: title.into(),
            command: command.into(),
            cwd: cwd.into(),
            running,
            focused,
        });
        self
    }

    /// What a pane is showing, for a plugin granted `read-pane-content`.
    pub fn showing(mut self, id: u32, text: &str) -> Self {
        self.text.insert(id, text.to_string());
        self
    }

    /// Move the focus, for a plugin that cares which pane it is.
    pub fn focus(mut self, id: u32) -> Self {
        for pane in &mut self.panes {
            pane.focused = pane.id == id;
        }
        self
    }
}

impl From<Session> for Snapshot {
    fn from(session: Session) -> Self {
        Snapshot { panes: session.panes, text: session.text }
    }
}

/// Build every plugin once per test run. `None` when the toolchain for
/// it is not installed, so a contributor without `wasm32-wasip2` can
/// still run the suite — CI has it, which is where it has to hold.
pub fn built_plugins() -> Option<&'static PathBuf> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
            let installed = Command::new("rustup")
                .args(["target", "list", "--installed"])
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).contains("wasm32-wasip2"))
                .unwrap_or(false);
            if !installed {
                eprintln!(
                    "skipping plugin tests: the wasm32-wasip2 target is not installed \
                     (`rustup target add wasm32-wasip2`)"
                );
                return None;
            }
            let out = Command::new("cargo")
                .current_dir(&root)
                .args(["build", "--release", "--target", "wasm32-wasip2"])
                .output()
                .expect("run cargo");
            assert!(
                out.status.success(),
                "the plugins did not build:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            Some(root.join("target/wasm32-wasip2/release"))
        })
        .as_ref()
}

/// One built component by name, or `None` when the toolchain is absent.
///
/// A plugin called `git-wip` builds as `git_wip.wasm`, because a cargo
/// library target cannot have a hyphen in its name. So both spellings
/// are tried and a test can use the name a person would.
pub fn component(name: &str) -> Option<Vec<u8>> {
    let dir = built_plugins()?;
    let direct = dir.join(format!("{name}.wasm"));
    let underscored = dir.join(format!("{}.wasm", name.replace('-', "_")));
    for path in [&direct, &underscored] {
        if let Ok(wasm) = std::fs::read(path) {
            return Some(wasm);
        }
    }
    panic!(
        "neither {} nor {} was built — is the plugin a directory under plugins/useful?",
        direct.display(),
        underscored.display()
    )
}

/// Load a plugin with the capabilities named, against a made-up session.
///
/// `None` means the toolchain is missing and the test should return. The
/// engine is leaked deliberately: it is immutable, shared, and lives as
/// long as the test process, and threading its lifetime through every
/// test signature buys nothing.
pub fn load(name: &str, granted: Capabilities, session: Session) -> Option<Loaded> {
    let wasm = component(name)?;
    let engine: &'static Engine = Box::leak(Box::new(Engine::new().expect("engine")));
    Some(
        Loaded::new(
            engine,
            Load { name, wasm: &wasm, granted: &granted, store_path: None, world: session.into() },
        )
        .unwrap_or_else(|e| panic!("{name} should load: {e}")),
    )
}

/// Load a plugin that was granted nothing, which is most of them.
pub fn plain_load(name: &str, session: Session) -> Option<Loaded> {
    load(name, Capabilities::default(), session)
}

/// Load a plugin allowed to run exactly these programs.
pub fn with_exec(name: &str, programs: &[&str], session: Session) -> Option<Loaded> {
    let granted = Capabilities {
        exec: programs.iter().map(|p| p.to_string()).collect(),
        ..Default::default()
    };
    load(name, granted, session)
}

/// Load a plugin allowed to read what panes are showing.
pub fn with_pane_text(name: &str, session: Session) -> Option<Loaded> {
    load(name, Capabilities { read_pane_content: true, ..Default::default() }, session)
}
