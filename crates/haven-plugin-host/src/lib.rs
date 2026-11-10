//! Running a plugin.
//!
//! One [`wasmtime`] `Store` per plugin, its own linear memory, its own
//! fuel. A plugin cannot see another plugin, cannot see the host's
//! memory, and cannot run longer than it is allowed to — not by
//! convention but because there is nowhere for it to reach and the fuel
//! runs out.
//!
//! What a plugin may do beyond arithmetic is exactly what its manifest
//! asked for and the user granted. The capability check is here, at the
//! host function, rather than at load time: a plugin with no `exec`
//! grant still has `run` in its world, and calling it returns an error
//! it can report. A missing capability is a refusal, never a trap —
//! a plugin that wanted `git` and did not get it should be able to say
//! so on the status bar instead of disappearing.
//!
//! # WASI
//!
//! Components built for `wasm32-wasip2` import `wasi:io`, `wasi:cli` and
//! friends whether the plugin uses them or not, because the target links
//! the WASI standard library. So WASI is linked — and granted nothing.
//! No preopened directory, no network, no environment, no arguments, no
//! standard input or output. A plugin that calls `std::fs::read` gets an
//! error from an empty filesystem rather than a file.
//!
//! That is the sandbox: not a list of things Haven remembers to forbid,
//! but a context with nothing in it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use haven_plugin::index::Capabilities;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine as WasmEngine, Store};
use wasmtime_wasi::p2::{IoView, WasiCtx, WasiCtxBuilder, WasiView};

wasmtime::component::bindgen!({
    world: "plugin",
    path: "../haven-plugin-api/wit",
    // The host functions are plain and fast; nothing here is worth the
    // complexity of async in a daemon that is otherwise a poll loop.
    async: false,
    // A command is data the host compares, stores and asserts on, so the
    // generated types need to behave like data.
    additional_derives: [PartialEq, Eq, Clone],
});

/// Helpers for running a real plugin in a test.
///
/// Not behind a feature: the daemon does not link this module's cost
/// (it is a few hundred lines and no new dependencies), and a `testing`
/// feature that the plugin tests have to remember to turn on is a
/// feature somebody will forget.
pub mod testing;

pub use haven::plugin::types::{
    Colour, Command, CommandEnd, CommandStart, CwdChange, Event, Pane, Remembered, Rgb, Run, Style,
};

/// How much a plugin may compute in one call.
///
/// Fuel is counted per wasm instruction, so this is "a few million
/// instructions" rather than a wall-clock figure — which is the point.
/// A plugin that loops forever stops here, deterministically, whatever
/// the machine is doing. Formatting a status segment costs a few
/// thousand; a plugin that needs more than this per event is doing
/// something it should be doing in a command.
pub const FUEL_PER_CALL: u64 = 50_000_000;

/// How long a program a plugin runs may take before it is given up on.
const EXEC_TIMEOUT: Duration = Duration::from_secs(5);

/// How much of a program's output is kept. A plugin asking `git log` for
/// everything should not be able to make the daemon hold a gigabyte.
const MAX_OUTPUT: usize = 64 * 1024;

/// What the host can answer while a plugin is running: the session, as
/// it was when the plugin was last called.
///
/// A snapshot rather than a borrow of the session, and that is the whole
/// reason it exists. The daemon owns the sessions and a session owns its
/// plugins, so a plugin holding a reference back to the session would be
/// a cycle the borrow checker rightly refuses. Instead the daemon builds
/// one of these — cheap, a handful of panes — and hands it over with
/// [`Loaded::refresh`] before it asks the plugin anything.
///
/// Pane *text* is only filled in for a plugin that was granted
/// `read-pane-content`. Copying every pane's screen for every plugin on
/// every event would be the most expensive thing the daemon does, and
/// for most plugins it would be copying something they are not allowed
/// to look at.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub panes: Vec<Pane>,
    pub text: BTreeMap<u32, String>,
}

impl Snapshot {
    /// The last `rows` rows of a pane, oldest first, as the host
    /// contract promises.
    fn pane_text(&self, pane: u32, rows: u32) -> Option<String> {
        let text = self.text.get(&pane)?;
        let lines: Vec<&str> = text.lines().collect();
        let from = lines.len().saturating_sub(rows as usize);
        Some(lines[from..].join("\n"))
    }
}

/// The engine, shared by every plugin, and the compiled components it
/// has already produced.
///
/// Compiling is the expensive part by a wide margin — about a second per
/// component — and it was being paid once per plugin *per session*.
/// Sixteen plugins meant seventeen seconds before the first frame
/// appeared, and a second session meant seventeen more. Measured by
/// running them, not by reasoning about them.
///
/// So a component is compiled once and kept: `Component` is per-engine
/// and can be instantiated any number of times, which is exactly the
/// shape of "one plugin, many sessions". And the compiled form is
/// written beside the plugin, so a daemon restart does not pay for it
/// again either.
pub struct Engine {
    inner: WasmEngine,
    /// Keyed by the hash of the component, not its name: a plugin that
    /// has been updated is a different component under the same name,
    /// and serving the old one from a cache would be worse than slow.
    compiled: std::cell::RefCell<BTreeMap<String, Component>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // By hand: a compiled `Component` has no `Debug`, and the only
        // interesting fact about this is how many are cached.
        f.debug_struct("Engine")
            .field("compiled", &self.compiled.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Engine {
    pub fn new() -> Result<Self, Error> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        // Fuel, so a runaway plugin is stopped rather than tolerated.
        config.consume_fuel(true);
        // The cheapest compile, not the fastest code. A plugin formats a
        // few dozen characters when something happens; what matters is
        // that it is ready before the terminal is, and `Speed` cost
        // about a second per component to optimise work that takes
        // microseconds to run.
        config.cranelift_opt_level(wasmtime::OptLevel::None);
        let inner = WasmEngine::new(&config).map_err(|e| Error::Engine(e.to_string()))?;
        Ok(Self { inner, compiled: std::cell::RefCell::new(BTreeMap::new()) })
    }

    /// Compile a plugin now and leave the result in its directory, so
    /// that nothing else ever has to.
    ///
    /// Called by `haven plugin install`. Compiling sixteen plugins takes
    /// about ten seconds, and the choice is only about *when* somebody
    /// waits for it: at the end of an install they asked for, or in
    /// front of a terminal that will not open. An install already takes
    /// a moment and nobody is staring at a blank prompt.
    pub fn precompile(&self, name: &str, wasm: &[u8], cache: &Path) -> Result<(), Error> {
        self.component(name, wasm, Some(cache)).map(|_| ())
    }

    /// Compile a component, or hand back one already compiled.
    ///
    /// `cache` is a directory to keep the compiled form in — the
    /// plugin's own, normally. `None` compiles in memory only, which is
    /// what a test wants.
    fn component(&self, name: &str, wasm: &[u8], cache: Option<&Path>) -> Result<Component, Error> {
        let key = haven_plugin::sign::sha256(wasm);
        if let Some(ready) = self.compiled.borrow().get(&key) {
            return Ok(ready.clone());
        }

        // The wasmtime version is part of the filename because a
        // compiled artifact is only valid for the version that made it,
        // and `deserialize` on a mismatch is an error rather than a
        // crash — but a stale file that errors every startup is still a
        // stale file.
        let cached = cache.map(|dir| {
            dir.join(format!("compiled-{}-{}.cwasm", env!("WASMTIME_VERSION_TAG"), &key[..16]))
        });

        if let Some(path) = &cached {
            if let Ok(bytes) = std::fs::read(path) {
                // SAFETY: wasmtime cannot check a precompiled artifact,
                // so this trusts the bytes. They are ours: written by
                // the block below into a directory inside the user's own
                // data directory, named after the hash of the component
                // they were compiled from and the version that compiled
                // it. Anyone able to rewrite this file could equally
                // replace the `.wasm` beside it, which is checked
                // against a signed index at install time and not after
                // — so this widens nothing. A corrupt or stale file
                // comes back as an error and is recompiled.
                let loaded = unsafe { Component::deserialize(&self.inner, &bytes) };
                match loaded {
                    Ok(component) => {
                        self.compiled.borrow_mut().insert(key, component.clone());
                        return Ok(component);
                    }
                    // Not fatal, and not worth a word to the user: the
                    // only cost is compiling it again.
                    Err(_) => {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }

        let component = Component::new(&self.inner, wasm)
            .map_err(|e| Error::Compile { name: name.into(), why: e.to_string() })?;
        if let Some(path) = &cached {
            if let Ok(bytes) = component.serialize() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                // Written by rename, so a daemon starting while another
                // is writing cannot read half a file.
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, &bytes).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
        }
        self.compiled.borrow_mut().insert(key, component.clone());
        Ok(component)
    }
}

/// Everything one plugin's instance can see.
struct State {
    name: String,
    granted: Capabilities,
    world: Snapshot,
    /// What the plugin remembered, and whether it has changed.
    store: BTreeMap<String, String>,
    dirty: bool,
    /// Where `store` is kept between runs.
    store_path: Option<PathBuf>,
    /// What it logged, newest last. Kept rather than printed so a test
    /// can read it and the daemon can decide where it goes.
    pub log: Vec<String>,
    wasi: WasiCtx,
    table: ResourceTable,
}

impl IoView for State {
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

impl WasiView for State {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
}

// `types` carries only records and variants, but it is an imported
// interface all the same, so the host has to say it implements it.
impl haven::plugin::types::Host for State {}

impl haven::plugin::host::Host for State {
    fn log(&mut self, text: String) {
        // Bounded: a plugin logging per event forever is a leak, and the
        // interesting lines are the recent ones.
        if self.log.len() >= 200 {
            self.log.remove(0);
        }
        self.log.push(text);
    }

    fn panes(&mut self) -> Vec<Pane> {
        self.world.panes.clone()
    }

    fn pane_text(&mut self, pane: u32, rows: u32) -> Result<String, String> {
        if !self.granted.read_pane_content {
            // Named in full, because this is the message that explains to
            // somebody why their plugin is not working.
            return Err(format!(
                "`{}` was not granted read-pane-content, so it cannot read what panes show",
                self.name
            ));
        }
        // A plugin asking for a million rows should not be able to make
        // the daemon build a million-row string.
        let rows = rows.min(1000);
        self.world.pane_text(pane, rows).ok_or_else(|| format!("no pane {pane}"))
    }

    fn run(&mut self, argv: Vec<String>) -> Result<haven::plugin::host::Output, String> {
        let Some(program) = argv.first() else { return Err("nothing to run".into()) };
        if !self.granted.exec.iter().any(|allowed| allowed == program) {
            return Err(format!("`{}` was not granted permission to run `{program}`", self.name));
        }
        run_program(&argv)
    }

    fn recall(&mut self, key: String) -> Option<String> {
        self.store.get(&key).cloned()
    }
}

/// Run a program for a plugin: argv only, no shell, bounded output, and
/// a clock on it.
///
/// There is no string for a shell to re-split, so there is nothing to
/// quote and nothing to get wrong — which is why the capability is an
/// argv allowlist rather than a command line.
fn run_program(argv: &[String]) -> Result<haven::plugin::host::Output, String> {
    use std::process::{Command, Stdio};

    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        // No terminal and no input: a program that asks a question gets
        // end-of-file rather than hanging the daemon.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", argv[0]))?;

    // Waited for with a deadline. `wait_with_output` would block the
    // daemon for as long as the program felt like running.
    let deadline = Instant::now() + EXEC_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} took longer than {}s", argv[0], EXEC_TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(format!("{}: {e}", argv[0])),
        }
    }
    let out = child.wait_with_output().map_err(|e| format!("{}: {e}", argv[0]))?;
    let cut = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes);
        text.chars().take(MAX_OUTPUT).collect::<String>()
    };
    Ok(haven::plugin::host::Output {
        status: out.status.code().unwrap_or(-1),
        stdout: cut(&out.stdout),
        stderr: cut(&out.stderr),
    })
}

/// A loaded plugin, ready to be asked things.
pub struct Loaded {
    name: String,
    store: Store<State>,
    bindings: Plugin,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded").field("name", &self.name).finish_non_exhaustive()
    }
}

/// How a plugin is loaded.
pub struct Load<'a> {
    pub name: &'a str,
    /// The component itself.
    pub wasm: &'a [u8],
    /// What the user granted. Nothing else is permitted, whatever the
    /// component's world says it can import.
    pub granted: &'a Capabilities,
    /// Where to keep what the plugin remembers. `None` means it
    /// remembers nothing past this instance, which is what a test wants.
    pub store_path: Option<PathBuf>,
    /// The session as it is now. Replaced before each call with
    /// [`Loaded::refresh`].
    pub world: Snapshot,
    /// Where to keep the compiled form of this component, so the next
    /// session and the next daemon do not compile it again. `None`
    /// compiles in memory only.
    pub cache_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Load<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // By hand, because `world` is a trait object and requiring
        // `Debug` on `World` would push it onto the daemon's session.
        f.debug_struct("Load")
            .field("name", &self.name)
            .field("bytes", &self.wasm.len())
            .field("granted", &self.granted)
            .field("store_path", &self.store_path)
            .finish_non_exhaustive()
    }
}

impl Loaded {
    /// Compile, instantiate and `init` a plugin.
    ///
    /// `init` is called here rather than lazily so that a plugin which
    /// cannot start is a load failure with a reason, rather than
    /// something that fails later on an event nobody connected to it.
    pub fn new(engine: &Engine, load: Load<'_>) -> Result<Self, Error> {
        let component = engine.component(load.name, load.wasm, load.cache_dir.as_deref())?;

        let mut linker = Linker::new(&engine.inner);
        // WASI first, and granted nothing: see the note at the top.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| Error::Engine(e.to_string()))?;
        Plugin::add_to_linker::<State, wasmtime::component::HasSelf<State>>(&mut linker, |s| s)
            .map_err(|e| Error::Engine(e.to_string()))?;

        let remembered = load.store_path.as_deref().map(read_store).unwrap_or_default();
        let state = State {
            name: load.name.to_string(),
            granted: load.granted.clone(),
            world: load.world,
            store: remembered,
            dirty: false,
            store_path: load.store_path,
            log: Vec::new(),
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        };
        let mut store = Store::new(&engine.inner, state);
        store.set_fuel(FUEL_PER_CALL).map_err(|e| Error::Engine(e.to_string()))?;

        let bindings = Plugin::instantiate(&mut store, &component, &linker)
            .map_err(|e| Error::Compile { name: load.name.into(), why: e.to_string() })?;

        let mut loaded = Self { name: load.name.to_string(), store, bindings };
        loaded.refuel();
        match loaded.bindings.haven_plugin_guest().call_init(&mut loaded.store) {
            Ok(Ok(())) => Ok(loaded),
            Ok(Err(why)) => Err(Error::Init { name: loaded.name, why }),
            Err(e) => Err(Error::Trap { name: loaded.name, why: trap_of(&e) }),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Tell the plugin what the session looks like now.
    ///
    /// Called before delivering an event or drawing. Nothing stops a
    /// caller from skipping it; the plugin then sees the session as it
    /// was, which is wrong but not unsafe — so the daemon refreshes on
    /// every call and a test refreshes when it wants to change the
    /// answer.
    pub fn refresh(&mut self, world: Snapshot) {
        self.store.data_mut().world = world;
    }

    /// Whether this plugin may read what panes are showing, so a caller
    /// knows whether filling [`Snapshot::text`] is worth the copy.
    pub fn reads_panes(&self) -> bool {
        self.store.data().granted.read_pane_content
    }

    /// What the plugin has logged, newest last.
    pub fn log(&self) -> &[String] {
        &self.store.data().log
    }

    /// Deliver an event and collect what the plugin asks for.
    ///
    /// The commands are returned rather than applied: this crate does
    /// not know what a session is, and a plugin's effects being data is
    /// what makes them replayable.
    pub fn on_event(&mut self, event: &Event) -> Result<Vec<Command>, Error> {
        self.refuel();
        let out = self
            .bindings
            .haven_plugin_guest()
            .call_on_event(&mut self.store, event)
            .map_err(|e| Error::Trap { name: self.name.clone(), why: trap_of(&e) })?;
        self.flush();
        Ok(out)
    }

    /// What to draw. Pure, so it is safe to call whenever Haven draws.
    pub fn render(&mut self) -> Result<Vec<Run>, Error> {
        self.refuel();
        let out = self
            .bindings
            .haven_plugin_guest()
            .call_render(&mut self.store)
            .map_err(|e| Error::Trap { name: self.name.clone(), why: trap_of(&e) })?;
        self.flush();
        Ok(out)
    }

    /// Apply a `remember` or `forget` the plugin asked for.
    ///
    /// Here rather than inside the guest call because it arrives as a
    /// command, and a command is applied by the host — the same path a
    /// `notice` or an `act` takes.
    pub fn remember(&mut self, key: &str, value: Option<&str>) {
        let state = self.store.data_mut();
        match value {
            Some(value) => {
                state.store.insert(key.to_string(), value.to_string());
            }
            None => {
                state.store.remove(key);
            }
        }
        state.dirty = true;
        self.flush();
    }

    /// Top the fuel back up before each call, so the limit is per call
    /// rather than per lifetime — a plugin that has been loaded all week
    /// is not slowly running out.
    fn refuel(&mut self) {
        let _ = self.store.set_fuel(FUEL_PER_CALL);
    }

    /// Write what the plugin remembered, if it changed.
    fn flush(&mut self) {
        let state = self.store.data_mut();
        if !state.dirty {
            return;
        }
        state.dirty = false;
        let Some(path) = state.store_path.clone() else { return };
        let mut text = String::from("# What this plugin remembered.\n");
        for (key, value) in &state.store {
            text.push_str(&format!("{}\t{}\n", escape(key), escape(value)));
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Best effort: a plugin's scratch notes are not worth failing an
        // event over, and the next write will try again.
        let _ = std::fs::write(&path, text);
    }
}

/// Tab-separated, so the one character that would break the format is
/// the one character that is escaped.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn read_store(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeMap::new() };
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
        .map(|(k, v)| (unescape(k), unescape(v)))
        .collect()
}

/// A trap's reason, with the cause chain flattened.
///
/// wasmtime's top-level message is "error while executing at wasm
/// backtrace", and the useful part — "all fuel consumed", "unreachable"
/// — is further down. Reporting only the top line makes every failure
/// look identical.
fn trap_of(e: &wasmtime::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(trap) = e.downcast_ref::<wasmtime::Trap>() {
        parts.push(trap.to_string());
    }
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e.as_ref());
    while let Some(err) = source {
        let line = err.to_string();
        // The backtrace is pages long and is not what a status bar wants.
        if let Some(first) = line.lines().next() {
            if !first.trim().is_empty() && !parts.iter().any(|p| p == first) {
                parts.push(first.to_string());
            }
        }
        source = err.source();
    }
    if parts.is_empty() {
        return "trapped".into();
    }
    // Most specific first: the trap itself, then the context.
    parts.reverse();
    parts.join(": ")
}

#[derive(Debug)]
pub enum Error {
    Engine(String),
    Compile { name: String, why: String },
    Init { name: String, why: String },
    Trap { name: String, why: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(why) => write!(f, "the plugin engine could not start: {why}"),
            Self::Compile { name, why } => {
                write!(f, "`{name}` is not a plugin Haven can load: {why}")
            }
            Self::Init { name, why } => write!(f, "`{name}` refused to start: {why}"),
            Self::Trap { name, why } => write!(f, "`{name}` stopped: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// The text of some runs, which is what most assertions about a plugin
/// are really about.
pub fn text_of(runs: &[Run]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}
