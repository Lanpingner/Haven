//! The mux server. See `docs/PLAN.md` §14.
//!
//! One single-threaded event loop over the listener, the attached clients and
//! every pane's pty. The plan reserved tokio for the daemon; a poll loop turns
//! out to be the simpler fit, because the pane pump is already poll-based and
//! nothing here benefits from task-per-pane concurrency. Revisit if per-pane
//! parsing ever needs its own core.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use haven_core::{Grid, Row};
use haven_mux::layout::{Direction, Edge};
use haven_mux::{MuxRuntime, NamedLayout, RuntimeEvent};
use haven_proto::transport::Framed;
use haven_proto::{
    Action, CellSize, ClientCaps, ClientMsg, PaneId, RowRle, ServerMsg, SessionId, Tables,
    PROTOCOL_VERSION,
};
use haven_pty::Pty;
use rustix::event::{poll, PollFd, PollFlags, Timespec};

pub mod config;
pub mod help;
pub mod snapshot;
pub mod sync;

use sync::SyncState;

/// How a session sizes its panes when clients of different sizes are attached.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ResizePolicy {
    /// Fit the smallest attached client, so every client sees everything.
    #[default]
    Smallest,
    /// Follow whoever attached or resized last.
    Latest,
}

type Spawner = Box<dyn FnMut(haven_mux::PaneRequest) -> io::Result<Pty>>;

/// A session's share of the attaching client's environment.
///
/// Shared rather than copied because a pane opened an hour after the attach
/// must get the *current* values: the session outlives the connection it was
/// made from, and panes spawned later would otherwise inherit a dead agent
/// socket for as long as the session lives.
pub type SessionEnv = std::rc::Rc<std::cell::RefCell<Vec<(String, String)>>>;

/// Where a pane's shell may run other than on this machine, as the config
/// currently says.
///
/// Shared for the same reason the environment is: a pane opened after the
/// config changed must be spawned by the rules that are in force now, not by
/// the ones that were in force when its session was made.
pub type ContainerSettings = std::rc::Rc<std::cell::RefCell<haven_backend::Settings>>;

struct SessionHost {
    /// The environment new panes in this session are given.
    env: SessionEnv,
    /// For the status line's "time open". Wall clock rather than `Instant`,
    /// so a client can render it without the daemon ticking.
    started_unix: u64,
    runtime: MuxRuntime<Spawner>,
    policy: ResizePolicy,
    /// The size last applied, so we only resize when it actually changes.
    applied: Option<CellSize>,
    /// Which user each pane's foreground process belongs to, keyed by the
    /// process group it was read from.
    ///
    /// Cached because the status is rebuilt whenever anything in the
    /// session changes, and reading `/proc` for every pane at that rate to
    /// learn something that changes when you type `sudo` is work for
    /// nothing. The process group is the key rather than a clock: it is
    /// exactly what changes when the answer might have.
    users: RefCell<HashMap<u32, (i32, Option<String>)>>,
    /// What the plugins last drew for this session, so rebuilding the
    /// status bar does not mean calling every plugin again.
    drawn: Vec<haven_proto::PluginSegment>,
    /// What the session looked like the last time plugins were told, so
    /// an event is sent when something actually changed rather than on
    /// every tick.
    was: PluginView,
    /// When each pane's current command was first seen running, so a
    /// plugin can be told how long it took. The shell reports that a
    /// command started and that it ended, never for how long.
    started: HashMap<u32, (String, std::time::Instant)>,
    /// Commands plugins have asked for and the daemon has not applied
    /// yet. Collected rather than applied inside the loop because
    /// applying one needs `&mut self` on the daemon, and the loop is
    /// already holding the session.
    pending: Vec<(String, haven_plugin_host::Command)>,
}

/// The facts plugins are told about, as they were last time.
///
/// Diffed rather than hooked: the daemon already recomputes all of this
/// to build the status bar, and comparing it is cheaper and far less
/// invasive than threading a callback through the mux for every event a
/// plugin might want.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct PluginView {
    focus: Option<u32>,
    /// Working directory per pane.
    cwd: HashMap<u32, String>,
    /// How many commands each pane has finished, so a new one is news.
    finished: HashMap<u32, usize>,
    panes: Vec<u32>,
}

struct Client {
    framed: Framed<UnixStream>,
    /// The theme this client has been sent, so a reload only pushes to
    /// clients that do not already have it.
    theme_sent: Option<haven_proto::theme::Theme>,
    /// The key table this client has, for the same reason.
    keys_sent: Option<haven_proto::KeyTable>,
    caps: ClientCaps,
    session: Option<u32>,
    /// Which tab this client is looking at.
    ///
    /// The current window belongs to the client, not to the session: two
    /// clients on one session browse its tabs independently, the same way
    /// they already read its scrollback at their own pace. `None` until the
    /// client attaches, after which it always names a window.
    window: Option<haven_proto::WindowId>,
    /// The tab this client was on before, for `last-window`. Per client for
    /// the same reason.
    window_before: Option<haven_proto::WindowId>,
    sync: SyncState,
    /// The status last sent. Window names come from pane titles, which arrive
    /// after the first layout and change while the layout does not, so tying
    /// the status to layout changes left the names stuck at their ids.
    last_status: Option<haven_proto::SessionStatus>,
}

/// The server.
pub struct Daemon {
    /// The wasm engine, built on first use.
    ///
    /// Lazily, because a daemon with no plugins installed should not pay
    /// for a JIT it will never call — which is most daemons today.
    plugin_engine: Option<haven_plugin_host::Engine>,
    /// The plugins, loaded **once** for the whole daemon.
    ///
    /// Not once per session, which is what this was first: a plugin is
    /// one program and loading it per session meant instantiating it,
    /// running its `init` and giving it a fresh memory for every window
    /// anyone opened. Worse, `remember` is scoped to the plugin, so two
    /// sessions holding two instances of `pomodoro` wrote the same file
    /// and clobbered each other.
    ///
    /// A session still gets its own *answers*: the snapshot handed to a
    /// plugin before each call is that session's, so `cwd-crumbs` draws
    /// a different directory in each bar from the one instance.
    plugins: Vec<haven_plugin_host::Loaded>,
    /// Whether loading has been attempted. Without it, a daemon with no
    /// plugins installed would read the install record on every tick to
    /// learn the same thing.
    plugins_loaded: bool,
    /// Installs and removals running on their own threads, and what they
    /// have to say when they finish.
    ///
    /// On a thread because installing fetches: a clone, or an HTTPS
    /// request, on the loop that answers every client. The daemon spent
    /// an afternoon having that exact class of freeze taken out of it.
    plugin_jobs: Option<(JobSender, JobReceiver)>,
    /// Where sessions are saved to and restored from.
    pub save_path: PathBuf,
    /// When the last autosave ran.
    last_save: u64,
    listener: Option<UnixListener>,
    socket_path: Option<PathBuf>,
    sessions: BTreeMap<u32, SessionHost>,
    clients: Vec<Client>,
    next_session: u32,
    make_spawner: Box<dyn FnMut(SessionEnv, ContainerSettings) -> Spawner>,
    /// Handed to every spawner, so a config reload reaches panes opened
    /// afterwards without rebuilding anything.
    containers: ContainerSettings,
    /// Help lookups in flight, and what has already been looked up. The
    /// cache is never invalidated: a man page does not change under a
    /// running daemon often enough to be worth the machinery.
    help: help::Helper,
    help_cache: help::Cache,
    policy: ResizePolicy,
    shutting_down: bool,
    /// Configuration, re-read when the file moves. The daemon owns it so a
    /// change reaches every attached client at once.
    config: config::ConfigWatcher,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("socket", &self.socket_path)
            .field("sessions", &self.sessions.keys().collect::<Vec<_>>())
            .field("clients", &self.clients.len())
            .finish_non_exhaustive()
    }
}

impl Daemon {
    /// A daemon with no listener, driven by [`Daemon::accept_stream`]. This is
    /// what the tests use: no socket, no filesystem, no second process.
    pub fn detached(
        make_spawner: impl FnMut(SessionEnv, ContainerSettings) -> Spawner + 'static,
    ) -> Self {
        let config = config::ConfigWatcher::new(default_config_path(), default_theme_dir());
        Self {
            listener: None,
            socket_path: None,
            sessions: BTreeMap::new(),
            clients: Vec::new(),
            next_session: 1,
            make_spawner: Box::new(make_spawner),
            // Seeded from the config rather than from the defaults: the
            // first pane of the first session is spawned before any reload
            // could have happened, and it is not a lesser pane.
            containers: std::rc::Rc::new(std::cell::RefCell::new(config.containers())),
            help: help::Helper::default(),
            help_cache: help::Cache::new(),
            policy: ResizePolicy::default(),
            shutting_down: false,
            config,
            plugin_engine: None,
            plugins: Vec::new(),
            plugins_loaded: false,
            plugin_jobs: None,
            save_path: snapshot::default_path(),
            last_save: unix_now(),
        }
    }

    /// Bind a listening socket. A stale socket from a crashed daemon is
    /// replaced rather than refused: the alternative is a terminal that will
    /// not start until the user deletes a file.
    pub fn bind(
        path: impl AsRef<Path>,
        make_spawner: impl FnMut(SessionEnv, ContainerSettings) -> Spawner + 'static,
    ) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if path.exists() && UnixStream::connect(&path).is_err() {
            std::fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let mut daemon = Self::detached(make_spawner);
        daemon.listener = Some(listener);
        daemon.socket_path = Some(path);
        Ok(daemon)
    }

    /// Everything `haven ls` prints, gathered in the daemon so the CLI stays a
    /// control-mode client rather than a second implementation.
    pub fn session_infos(&self) -> Vec<haven_proto::SessionInfo> {
        self.sessions
            .iter()
            .map(|(id, host)| haven_proto::SessionInfo {
                id: SessionId(*id),
                name: host.name().to_string(),
                windows: host.runtime.session.windows.len() as u16,
                panes: host.runtime.session.panes().len() as u16,
                clients: self.clients.iter().filter(|c| c.session == Some(*id)).count() as u16,
                size: host.runtime.size(),
                // The tabs themselves, for every session: the tree's whole
                // job is to show what is in the one you are not in.
                tabs: host.windows(),
            })
            .collect()
    }

    pub fn session_names(&self) -> Vec<(SessionId, String, usize)> {
        self.sessions
            .iter()
            .map(|(id, host)| {
                (SessionId(*id), host.name().to_string(), host.runtime.session.windows.len())
            })
            .collect()
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub fn set_policy(&mut self, policy: ResizePolicy) {
        self.policy = policy;
        for host in self.sessions.values_mut() {
            host.policy = policy;
        }
    }

    /// Adopt an already-connected stream, as the tests and a socket-activated
    /// launch both need.
    pub fn accept_stream(&mut self, stream: UnixStream) -> io::Result<()> {
        stream.set_nonblocking(true)?;
        self.clients.push(Client {
            framed: Framed::new(stream),
            caps: ClientCaps::null(CellSize::default()),
            session: None,
            window: None,
            window_before: None,
            sync: SyncState::default(),
            theme_sent: None,
            keys_sent: None,
            last_status: None,
        });
        Ok(())
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    /// Run until asked to stop. A daemon with no sessions keeps listening —
    /// that is the point of a server you attach to.
    pub fn run(&mut self) -> io::Result<()> {
        while !self.shutting_down {
            self.tick(Duration::from_millis(16))?;
            // Without a listener there is no way for work to arrive, so an
            // empty detached daemon is finished rather than idle forever.
            if self.listener.is_none() && self.sessions.is_empty() && self.clients.is_empty() {
                break;
            }
        }
        // Give refusals a chance to reach the clients before the socket dies.
        self.flush_clients();
        Ok(())
    }

    /// One iteration: accept, read, pump, sync, flush.
    /// Where the configuration is read from, and where named themes live.
    pub fn watch_config(&mut self, path: PathBuf, theme_dir: PathBuf) {
        self.config = config::ConfigWatcher::new(path, theme_dir);
    }

    pub fn tick(&mut self, timeout: Duration) -> io::Result<()> {
        self.accept_pending()?;
        self.wait(timeout)?;
        self.read_clients()?;
        self.pump_sessions()?;
        self.apply_resize_policy()?;
        // A changed theme invalidates what every client was last sent, which
        // is what turns an edit into a redraw rather than into nothing.
        if self.config.poll() {
            *self.containers.borrow_mut() = self.config.containers();
            for client in &mut self.clients {
                client.theme_sent = None;
                client.keys_sent = None;
            }
            if let Some(problem) = &self.config.problem {
                eprintln!("haven: config: {problem}");
            }
        }
        // Help that a thread finished since the last tick. Sent to every
        // client in the session rather than remembered against the one that
        // asked: the answer is about a command, not about a client.
        for (word, text) in self.help.drain() {
            self.help_cache.insert(word.clone(), text.clone());
            for client in &mut self.clients {
                let _ = client
                    .framed
                    .queue(&ServerMsg::Help { command: word.clone(), text: text.clone() });
            }
        }
        self.drain_plugin_jobs();
        self.drive_plugins();
        self.apply_plugin_commands();
        self.autosave();
        self.sync_clients()?;
        self.flush_clients();
        Ok(())
    }

    fn accept_pending(&mut self) -> io::Result<()> {
        // Collect first, adopt second: `accept_stream` needs `&mut self`.
        let mut accepted = Vec::new();
        {
            let Some(listener) = &self.listener else { return Ok(()) };
            loop {
                match listener.accept() {
                    Ok((stream, _)) => accepted.push(stream),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
        }
        for stream in accepted {
            self.accept_stream(stream)?;
        }
        Ok(())
    }

    /// Block until something needs attention: a new client, client input, or
    /// pane output. Without this the daemon would spin.
    fn wait(&mut self, timeout: Duration) -> io::Result<()> {
        let mut owned: Vec<std::os::fd::BorrowedFd<'_>> = Vec::new();
        if let Some(listener) = &self.listener {
            owned.push(listener.as_fd());
        }
        for client in &self.clients {
            owned.push(client.framed.stream.as_fd());
        }
        for host in self.sessions.values() {
            for (_, pane) in host.runtime.visible() {
                owned.push(pane.pty.master());
            }
        }
        if owned.is_empty() {
            std::thread::sleep(timeout);
            return Ok(());
        }
        let mut fds: Vec<PollFd> = owned.iter().map(|fd| PollFd::new(fd, PollFlags::IN)).collect();
        let ts =
            Timespec { tv_sec: timeout.as_secs() as i64, tv_nsec: timeout.subsec_nanos() as i64 };
        match poll(&mut fds, Some(&ts)) {
            Ok(_) => Ok(()),
            Err(rustix::io::Errno::INTR) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn read_clients(&mut self) -> io::Result<()> {
        let mut requests: Vec<(usize, ClientMsg)> = Vec::new();
        for (index, client) in self.clients.iter_mut().enumerate() {
            if client.framed.fill().is_err() {
                continue;
            }
            while let Ok(Some(msg)) = client.framed.next_message::<ClientMsg>() {
                requests.push((index, msg));
            }
        }
        for (index, msg) in requests {
            self.handle(index, msg)?;
        }
        // Drop clients whose peer went away; their sessions keep running.
        self.clients.retain(|c| !c.framed.is_closed() || c.framed.has_pending_output());
        Ok(())
    }

    fn handle(&mut self, index: usize, msg: ClientMsg) -> io::Result<()> {
        match msg {
            ClientMsg::Attach { session, caps, env } => {
                let id = match session {
                    Some(SessionId(id)) if self.sessions.contains_key(&id) => id,
                    Some(SessionId(id)) => {
                        // Asking for a session that does not exist is an error
                        // worth saying out loud, not a silent new session.
                        let client = &mut self.clients[index];
                        let _ = client.framed.queue(&ServerMsg::Refused {
                            reason: format!("no such session: {id}"),
                        });
                        return Ok(());
                    }
                    None => self.first_or_create(caps, &env)?,
                };
                self.refresh_env(id, env);
                self.join(index, id, caps);
            }
            ClientMsg::NewSession { name, caps, env } => {
                let id = self.create_session(name, caps, &env)?;
                self.join(index, id, caps);
            }
            ClientMsg::SaveSessions => {
                let (count, problem) = self.save_sessions();
                let path = self.save_path.display().to_string();
                let _ = self.clients[index].framed.queue(&ServerMsg::Saved {
                    sessions: count,
                    path,
                    problem,
                });
            }
            ClientMsg::RestoreSessions => {
                let (count, problem) = self.restore_sessions();
                let path = self.save_path.display().to_string();
                let _ = self.clients[index].framed.queue(&ServerMsg::Saved {
                    sessions: count,
                    path,
                    problem,
                });
            }
            ClientMsg::InstallPlugin { name } => {
                let session = self.clients.get(index).and_then(|c| c.session);
                self.start_plugin_job(session, PluginJob::Install(name));
            }
            ClientMsg::RemovePlugin { name } => {
                let session = self.clients.get(index).and_then(|c| c.session);
                self.start_plugin_job(session, PluginJob::Remove(name));
            }
            ClientMsg::ListPlugins => {
                let repos = plugin_repos();
                let _ = self.clients[index].framed.queue(&ServerMsg::Plugins { repos });
            }
            ClientMsg::ListShells => {
                let shells: Vec<String> =
                    haven_shell::available().iter().map(|p| p.display().to_string()).collect();
                // The same question, one message: which shell and where it
                // runs are one decision, and two listings arriving
                // separately would mean a menu that grows under the cursor.
                let containers = haven_backend::targets(&self.config.containers())
                    .into_iter()
                    .map(|t| haven_proto::ContainerTarget {
                        spec: t.spec,
                        label: t.label,
                        detail: t.detail,
                        present: t.present,
                        choosable: t.choosable,
                    })
                    .collect();
                let _ = self.clients[index].framed.queue(&ServerMsg::Shells { shells, containers });
            }
            ClientMsg::ShowEnvironment => {
                let env = self
                    .clients
                    .get(index)
                    .and_then(|c| c.session)
                    .and_then(|id| self.sessions.get(&id))
                    .map(|h| h.env.borrow().clone())
                    .unwrap_or_default();
                let _ = self.clients[index].framed.queue(&ServerMsg::Environment(env));
            }
            ClientMsg::Input { pane, bytes } => {
                // Typing returns to the live screen. Reading history and then
                // pressing a key means you are done reading, and watching
                // your own keystrokes land somewhere off-screen is worse than
                // losing your place.
                let state = self.clients[index].sync.pane(pane);
                if state.offset != 0 {
                    state.offset = 0;
                    state.snapshotted = false;
                }
                let view = self.clients.get(index).and_then(|c| c.window);
                if let Some(host) = self.host_of(index) {
                    // Synchronized windows send everything typed in one pane
                    // to all of them, which is the whole point of the mode.
                    // Which window that is depends on the client doing the
                    // typing: the flag and the active pane are both the
                    // typist's tab, not the session's.
                    let broadcast = host.runtime.session.viewing(view, |session| {
                        session.active_window().synchronized && session.active_pane() == pane
                    });
                    let targets = if broadcast {
                        host.runtime.session.view = view;
                        let all = host.runtime.input_targets();
                        host.runtime.session.view = None;
                        all
                    } else {
                        vec![pane]
                    };
                    for target in targets {
                        host.runtime.send_to(target, &bytes)?;
                    }
                }
            }
            ClientMsg::Resize { caps } => {
                self.clients[index].caps = caps;
                // A client that changed size must be re-synced: its rows are
                // a different width now.
                self.clients[index].sync.invalidate();
            }
            // These three need more than one session, so they are settled
            // here rather than in the per-session action table.
            ClientMsg::Action(Action::SaveSessions) => {
                let (sessions, problem) = self.save_sessions();
                let path = self.save_path.display().to_string();
                let _ =
                    self.clients[index].framed.queue(&ServerMsg::Saved { sessions, path, problem });
            }
            ClientMsg::Action(Action::RestoreSessions) => {
                let (sessions, problem) = self.restore_sessions();
                let path = self.save_path.display().to_string();
                let _ =
                    self.clients[index].framed.queue(&ServerMsg::Saved { sessions, path, problem });
            }
            // Detaching others needs every client, not one session, so it is
            // settled here rather than in the per-session action table.
            ClientMsg::Action(Action::DetachOthers) => {
                let Some(id) = self.clients.get(index).and_then(|c| c.session) else {
                    return Ok(());
                };
                for (i, client) in self.clients.iter_mut().enumerate() {
                    if i != index && client.session == Some(id) {
                        let _ = client.framed.queue(&ServerMsg::Refused {
                            reason: "detached by another client".into(),
                        });
                        client.session = None;
                    }
                }
            }
            ClientMsg::Action(action) => self.act(index, action)?,
            ClientMsg::Scroll { pane, delta } => self.scroll(index, pane, delta),
            ClientMsg::ScrollTo { pane, row } => self.scroll_to(index, pane, row),
            ClientMsg::Copy { pane, start, end } => {
                let text = self
                    .pane_grid(index, pane)
                    .map(|g| {
                        g.text_between(
                            (start.row as usize, start.col as usize),
                            (end.row as usize, end.col as usize),
                        )
                    })
                    .unwrap_or_default();
                let _ = self.clients[index].framed.queue(&ServerMsg::Copied { pane, text });
            }
            ClientMsg::Search { pane, query, from, backwards } => {
                let row = self
                    .pane_grid(index, pane)
                    .and_then(|g| g.find(&query, from as usize, backwards))
                    .map(|r| r as u32);
                let _ = self.clients[index].framed.queue(&ServerMsg::Found { pane, row });
            }
            ClientMsg::History { pane } => {
                let entries = self.history_for(index, pane);
                let _ = self.clients[index].framed.queue(&ServerMsg::History { pane, entries });
            }
            ClientMsg::Help { command } => {
                // Looked up on a thread. `man` reads a compressed file and
                // runs a formatter; doing that in the event loop would stop
                // every pane in every session while somebody moved a
                // selection down one line.
                let word = help::subject(&command);
                match self.help_cache.get(&word) {
                    Some(text) => {
                        let _ = self.clients[index]
                            .framed
                            .queue(&ServerMsg::Help { command: word, text: text.clone() });
                    }
                    None => self.help.ask(word),
                }
            }
            ClientMsg::Preview { pane, cols, rows } => {
                let preview = self.preview(pane, cols as usize, rows as usize);
                let _ =
                    self.clients[index].framed.queue(&ServerMsg::Preview { pane, rows: preview });
            }
            ClientMsg::Grep { query } => {
                let hits = self.grep(index, &query);
                let _ = self.clients[index].framed.queue(&ServerMsg::Matches { query, hits });
            }
            ClientMsg::ListSessions => {
                let sessions = self.session_infos();
                let _ = self.clients[index].framed.queue(&ServerMsg::Sessions {
                    sessions,
                    daemon_build: haven_proto::build_stamp(),
                });
            }
            ClientMsg::KillSession(SessionId(id)) => {
                self.sessions.remove(&id);
                for client in self.clients.iter_mut().filter(|c| c.session == Some(id)) {
                    client.session = None;
                    let _ = client
                        .framed
                        .queue(&ServerMsg::Refused { reason: "session killed".into() });
                }
                let sessions = self.session_infos();
                let _ = self.clients[index].framed.queue(&ServerMsg::Sessions {
                    sessions,
                    daemon_build: haven_proto::build_stamp(),
                });
            }
            ClientMsg::Shutdown => {
                self.sessions.clear();
                for client in self.clients.iter_mut() {
                    client.session = None;
                    let _ = client
                        .framed
                        .queue(&ServerMsg::Refused { reason: "server shutting down".into() });
                }
                self.shutting_down = true;
            }
            ClientMsg::Detach => {
                self.clients[index].session = None;
                self.clients[index]
                    .framed
                    .queue(&ServerMsg::Refused { reason: "detached".into() })
                    .ok();
            }
        }
        Ok(())
    }

    fn first_or_create(&mut self, caps: ClientCaps, env: &[(String, String)]) -> io::Result<u32> {
        if let Some(id) = self.sessions.keys().next().copied() {
            return Ok(id);
        }
        self.create_session(None, caps, env)
    }

    /// Make a session, whether or not one already exists.
    fn create_session(
        &mut self,
        name: Option<String>,
        caps: ClientCaps,
        env: &[(String, String)],
    ) -> io::Result<u32> {
        let id = self.next_session;
        self.next_session += 1;
        // An unnamed session is numbered, which is what makes `haven-2`
        // follow `haven-1` without the client having to invent a name.
        let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("haven-{id}"));
        // Filled before the runtime is built, because building it spawns the
        // session's first pane — which would otherwise be the one pane that
        // never saw the attaching client's environment.
        let cell: SessionEnv = std::rc::Rc::new(std::cell::RefCell::new(env.to_vec()));
        let spawner = (self.make_spawner)(cell.clone(), self.containers.clone());
        let runtime = MuxRuntime::new(SessionId(id), &name, caps.size, spawner)?;
        self.sessions.insert(
            id,
            SessionHost {
                users: RefCell::default(),
                env: cell,
                started_unix: unix_now(),
                runtime,
                policy: self.policy,
                applied: Some(caps.size),
                drawn: Vec::new(),
                was: PluginView::default(),
                started: HashMap::new(),
                pending: Vec::new(),
            },
        );
        Ok(id)
    }

    /// Take the attaching client's environment for this session.
    ///
    /// Replaces rather than merges: a stale `SSH_AUTH_SOCK` kept beside a
    /// fresh one is the bug, not a feature, and the list is exactly the set
    /// that names something belonging to the connection.
    fn refresh_env(&mut self, id: u32, env: Vec<(String, String)>) {
        if env.is_empty() {
            return;
        }
        if let Some(host) = self.sessions.get(&id) {
            *host.env.borrow_mut() = env;
        }
    }

    /// Point a client at a session and tell it so. Attaching and creating
    /// end the same way, and a second copy of this was how the two could
    /// have drifted.
    fn join(&mut self, index: usize, id: u32, caps: ClientCaps) {
        // A client arrives on the session's own current tab and from then on
        // moves independently. Giving it an explicit window here, rather than
        // leaving it to follow `session.active`, is what keeps two fresh
        // clients from moving together until one of them picks a tab.
        let start = self.sessions.get(&id).map(|h| h.runtime.session.current_window());
        let client = &mut self.clients[index];
        client.caps = caps;
        client.session = Some(id);
        client.window = start;
        client.window_before = None;
        client.sync.invalidate();
        // The theme is sent once per session; a client that moved to another
        // one has to be told again.
        client.theme_sent = None;
        client.keys_sent = None;
        client.last_status = None;
        let _ = client.framed.queue(&ServerMsg::Hello {
            protocol: PROTOCOL_VERSION,
            session: SessionId(id),
            build: haven_proto::build_stamp(),
        });
    }

    /// Move one client's view of one pane. Clamped here rather than in the
    /// client, because only the daemon knows how much history there is.
    fn scroll(&mut self, index: usize, pane: PaneId, delta: i32) {
        let Some(id) = self.clients.get(index).and_then(|c| c.session) else { return };
        let Some(host) = self.sessions.get(&id) else { return };
        let Some(max) = host.runtime.pane(pane).map(|p| p.grid().max_scroll()) else { return };
        let client = &mut self.clients[index];
        let state = client.sync.pane(pane);
        let wanted = state.offset as i64 + delta as i64;
        let next = wanted.clamp(0, max as i64) as usize;
        if next == state.offset {
            return;
        }
        state.offset = next;
        // The view is about to be a different set of rows at the same
        // indices, which row revisions cannot express.
        state.snapshotted = false;
    }

    /// Save on a timer, so a save file is not only as good as the last time
    /// someone remembered. A daemon holding nothing writes nothing: an empty
    /// save file would quietly destroy the one from before a reboot.
    fn autosave(&mut self) {
        let every = self.config.autosave_seconds();
        if every == 0 || self.sessions.is_empty() {
            return;
        }
        if unix_now().saturating_sub(self.last_save) < every {
            return;
        }
        if let (_, Some(problem)) = self.save_sessions() {
            eprintln!("haven: autosave: {problem}");
            // Not retried every tick: a broken path would print forever.
            self.last_save = unix_now();
        }
    }

    /// Write every session's shape to the save file.
    ///
    /// Returns how many sessions were written and what went wrong, because a
    /// save that silently did nothing is the failure people discover after
    /// the reboot.
    pub fn save_sessions(&mut self) -> (u16, Option<String>) {
        let sessions: Vec<snapshot::SavedSession> = self
            .sessions
            .values()
            .map(|host| {
                let runtime = &host.runtime;
                snapshot::describe(&runtime.session, |pane| {
                    let Some(runtime) = runtime.pane(pane) else {
                        return snapshot::SavedPane::default();
                    };
                    snapshot::SavedPane {
                        cwd: runtime.cwd().map(|p| p.display().to_string()),
                        command: runtime.foreground_command().unwrap_or_default(),
                        // Only ever a virtual shell, never an arbitrary
                        // program: what a save file may cause to run is the
                        // same question `restore_programs` exists to answer,
                        // and this half of it is answered at restore time by
                        // refusing to start anything that is not already up.
                        program: runtime
                            .program()
                            .and_then(|p| p.to_str())
                            .filter(|p| haven_backend::Spec::parse(p).is_some())
                            .map(str::to_string),
                    }
                })
            })
            .collect();
        let count = sessions.len().min(u16::MAX as usize) as u16;
        let snapshot = snapshot::Snapshot::new(sessions);
        match snapshot.write(&self.save_path) {
            Ok(()) => {
                self.last_save = unix_now();
                (count, None)
            }
            Err(e) => (0, Some(e)),
        }
    }

    /// Rebuild the saved sessions that are not already here.
    ///
    /// Sessions are matched by name and never replaced: a restore run twice,
    /// or run while you are working, must not throw away what is live.
    pub fn restore_sessions(&mut self) -> (u16, Option<String>) {
        let snapshot = match snapshot::Snapshot::read(&self.save_path) {
            Ok(s) => s,
            Err(e) => return (0, Some(e)),
        };
        let size = self
            .clients
            .iter()
            .filter(|c| c.session.is_some())
            .map(|c| c.caps.size)
            .next()
            .unwrap_or(CellSize { cols: 80, rows: 24, ..Default::default() });

        let mut restored = 0u16;
        for saved in &snapshot.sessions {
            if self.sessions.values().any(|h| h.name() == saved.name) {
                continue;
            }
            match self.restore_one(saved, size) {
                Ok(()) => restored += 1,
                Err(e) => return (restored, Some(e.to_string())),
            }
        }
        (restored, None)
    }

    fn restore_one(&mut self, saved: &snapshot::SavedSession, size: CellSize) -> io::Result<()> {
        let Some(first) = saved.windows.first() else { return Ok(()) };
        let allowed: Vec<String> = self.config.restore_programs().to_vec();
        let containers = self.config.containers();
        let request = |pane: &snapshot::SavedPane| haven_mux::PaneRequest {
            size: haven_pty::PtySize { cols: 1, rows: 1, ..Default::default() },
            // A virtual shell comes back only if there is something to
            // rejoin. A restore never starts a container: otherwise a file
            // on disk would decide what runs on the machine after a reboot.
            // A pane whose container is gone is an ordinary shell, which is
            // the same thing every other restored pane is.
            program: pane
                .program
                .as_deref()
                .filter(|spec| haven_backend::rejoinable(spec, &containers))
                .map(std::path::PathBuf::from),
            cwd: pane.cwd.as_ref().map(std::path::PathBuf::from),
            run: if snapshot::restorable(&pane.command, &allowed) {
                pane.command.clone()
            } else {
                Vec::new()
            },
        };

        let id = self.next_session;
        self.next_session += 1;
        let cell: SessionEnv = Default::default();
        let spawner = (self.make_spawner)(cell.clone(), self.containers.clone());
        // The session is created with its first window's first pane, and the
        // rest of the shape is installed over it: `MuxRuntime::new` has to
        // make a pane, and a session with no panes is not representable.
        let mut runtime = MuxRuntime::new(SessionId(id), &saved.name, size, spawner)?;
        runtime.restore_window(
            first.name.clone(),
            &first.layout.to_tree(),
            first.active_pane,
            &first.panes.iter().map(request).collect::<Vec<_>>(),
        )?;
        for window in saved.windows.iter().skip(1) {
            runtime.restore_window(
                window.name.clone(),
                &window.layout.to_tree(),
                window.active_pane,
                &window.panes.iter().map(request).collect::<Vec<_>>(),
            )?;
        }
        // The placeholder window `MuxRuntime::new` made is not part of the
        // saved shape, so it goes once there is something else to be on.
        let placeholder = runtime.session.windows.first().map(|w| w.id);
        if let Some(id) = placeholder.filter(|_| runtime.session.windows.len() > 1) {
            let (panes, _) = runtime.session.close_window(id);
            for pane in panes {
                runtime.close_pane_quietly(pane);
            }
        }
        // The placeholder took window 1, so without this a restored session
        // starts at tab 2 and every number is off by one.
        runtime.session.renumber_windows();
        runtime.select_window(saved.active_window)?;

        self.sessions.insert(
            id,
            SessionHost {
                users: RefCell::default(),
                env: cell,
                started_unix: unix_now(),
                runtime,
                policy: self.policy,
                applied: Some(size),
                drawn: Vec::new(),
                was: PluginView::default(),
                started: HashMap::new(),
                pending: Vec::new(),
            },
        );
        Ok(())
    }

    /// A pane's grid, for the read-only queries copy mode makes.
    /// What a pane has run, and what its shell remembers from before.
    ///
    /// The pane's own commands come last, because the list is read from the
    /// bottom and they are the ones with any claim on your attention: they
    /// happened here, and they carry how they ended. Everything the file
    /// holds that is not already in that half goes above, newest nearest.
    fn history_for(&self, index: usize, pane: PaneId) -> Vec<haven_proto::HistoryEntry> {
        let Some(id) = self.clients.get(index).and_then(|c| c.session) else { return Vec::new() };
        let Some(host) = self.sessions.get(&id) else { return Vec::new() };
        let Some(runtime) = host.runtime.pane(pane) else { return Vec::new() };

        // Each command once, at its most recent run — so the status shown
        // beside it is the one it ended with last, and a list you are
        // reading to find a line does not spend thirty rows on `cargo
        // test`. The same rule the shell half goes through.
        let mut seen_mine = std::collections::HashSet::new();
        let mut mine: Vec<haven_proto::HistoryEntry> = runtime
            .history()
            .iter()
            .rev()
            .filter(|run| seen_mine.insert(run.command.as_str()))
            .map(|run| haven_proto::HistoryEntry {
                command: run.command.clone(),
                exit: run.exit,
                from_pane: true,
            })
            .collect();
        mine.reverse();

        let seen: std::collections::HashSet<&str> =
            mine.iter().map(|e| e.command.as_str()).collect();
        let shell = haven_shell::history::shell_name(runtime.program());
        let mut out: Vec<haven_proto::HistoryEntry> = haven_shell::history::recent(&shell, 500)
            .into_iter()
            .filter(|c| !seen.contains(c.as_str()))
            .map(|command| haven_proto::HistoryEntry { command, exit: None, from_pane: false })
            .collect();
        out.extend(mine);
        out
    }

    /// What a pane is showing, small enough to put in a panel.
    ///
    /// Looked for in every session, not just this client's: the point of a
    /// preview is to see where you are *not*. Styles are resolved here
    /// rather than interned, because the client has never been sent the
    /// style table of a session it is not attached to.
    fn preview(&self, pane: PaneId, cols: usize, rows: usize) -> Vec<haven_proto::PreviewRow> {
        let Some(runtime) = self.sessions.values().find_map(|h| h.runtime.pane(pane)) else {
            return Vec::new();
        };
        let grid = runtime.grid();
        let styles = runtime.styles();
        let all = grid.rows();

        all[preview_window(grid, rows)]
            .iter()
            .map(|row| {
                let mut runs: Vec<(u16, String, haven_core::Style)> = Vec::new();
                for cell in row.cells.iter().take(cols) {
                    let text = grid.cell_text(cell).into_owned();
                    let style = styles.get(cell.style);
                    match runs.last_mut() {
                        Some((count, prev, prev_style))
                            if *prev == text && *prev_style == style && *count < u16::MAX =>
                        {
                            *count += 1
                        }
                        _ => runs.push((1, text, style)),
                    }
                }
                // A row of trailing blanks is most of a preview; dropping it
                // here costs nothing and the client pads anyway.
                while runs.last().is_some_and(|(_, text, style)| {
                    text.trim().is_empty() && *style == haven_core::Style::default()
                }) {
                    runs.pop();
                }
                haven_proto::PreviewRow { runs }
            })
            .collect()
    }

    /// Every matching line in every pane of this client's session.
    fn grep(&self, index: usize, query: &str) -> Vec<haven_proto::GrepHit> {
        const PER_PANE: usize = 200;
        const TOTAL: usize = 1000;
        let Some(id) = self.clients.get(index).and_then(|c| c.session) else { return Vec::new() };
        let Some(host) = self.sessions.get(&id) else { return Vec::new() };
        let mut hits = Vec::new();
        for window in &host.runtime.session.windows {
            for pane in window.panes() {
                let Some(runtime) = host.runtime.pane(pane) else { continue };
                for (row, text) in runtime.grid().find_all(query, PER_PANE) {
                    hits.push(haven_proto::GrepHit {
                        pane,
                        window: window.id.0,
                        row: row as u32,
                        text,
                    });
                    if hits.len() >= TOTAL {
                        return hits;
                    }
                }
            }
        }
        hits
    }

    fn pane_grid(&self, index: usize, pane: PaneId) -> Option<&haven_core::Grid> {
        let id = self.clients.get(index)?.session?;
        self.sessions.get(&id)?.runtime.pane(pane).map(|p| p.grid())
    }

    /// Put a view at an absolute row of the history: where a search landed.
    fn scroll_to(&mut self, index: usize, pane: PaneId, row: u32) {
        let Some(grid) = self.pane_grid(index, pane) else { return };
        let (history, rows) = (grid.max_scroll(), grid.num_rows());
        // Land the row a third of the way down, so there is context above it
        // as well as below — a match pinned to the top row reads badly.
        let want = (history + rows).saturating_sub(row as usize + rows / 3 + 1);
        let state = self.clients[index].sync.pane(pane);
        let next = want.min(history);
        if next != state.offset {
            state.offset = next;
            state.snapshotted = false;
        }
    }

    fn host_of(&mut self, index: usize) -> Option<&mut SessionHost> {
        let id = self.clients.get(index)?.session?;
        self.sessions.get_mut(&id)
    }

    fn act(&mut self, index: usize, action: Action) -> io::Result<()> {
        let view = self.clients.get(index).and_then(|c| c.window);
        let before = self.clients.get(index).and_then(|c| c.window_before);
        let Some(host) = self.host_of(index) else { return Ok(()) };
        // The acting client's tab is the current one for the whole of this
        // action: splitting, closing and focusing all mean "in the window I
        // am looking at", and another client may be looking at a different
        // one.
        host.runtime.session.view = view;
        host.runtime.session.set_window_before(before);
        let runtime = &mut host.runtime;
        let ended = match action {
            Action::SplitColumns { program } => {
                runtime.split(Direction::Columns, program.map(Into::into))?;
                false
            }
            Action::SplitRows { program } => {
                runtime.split(Direction::Rows, program.map(Into::into))?;
                false
            }
            Action::FocusLeft => !runtime.focus(Edge::Left)?,
            Action::FocusRight => !runtime.focus(Edge::Right)?,
            Action::FocusUp => !runtime.focus(Edge::Up)?,
            Action::FocusDown => !runtime.focus(Edge::Down)?,
            Action::ResizeLeft => !runtime.resize_active(Edge::Left, 0.05)?,
            Action::ResizeRight => !runtime.resize_active(Edge::Right, 0.05)?,
            Action::ResizeUp => !runtime.resize_active(Edge::Up, 0.05)?,
            Action::ResizeDown => !runtime.resize_active(Edge::Down, 0.05)?,
            Action::ClosePane => runtime.close_active()?,
            Action::ToggleZoom => {
                runtime.toggle_zoom()?;
                false
            }
            Action::NewWindow { program } => {
                runtime.new_window(program.map(Into::into))?;
                false
            }
            Action::NextWindow => {
                runtime.next_window()?;
                false
            }
            Action::PreviousWindow => {
                runtime.previous_window()?;
                false
            }
            Action::SelectWindow(n) => {
                runtime.select_window(n as usize)?;
                false
            }
            Action::SelectPane(id) => {
                runtime.session.focus_pane(haven_proto::PaneId(id));
                false
            }
            Action::EvenHorizontal => {
                runtime.apply_named_layout(NamedLayout::EvenHorizontal)?;
                false
            }
            Action::EvenVertical => {
                runtime.apply_named_layout(NamedLayout::EvenVertical)?;
                false
            }
            Action::Tiled => {
                runtime.apply_named_layout(NamedLayout::Tiled)?;
                false
            }
            Action::LastWindow => {
                runtime.last_window()?;
                false
            }
            Action::LastPane => {
                runtime.session.active_window_mut().last_pane();
                false
            }
            Action::SwapPane(edge) => {
                runtime.swap_pane(edge.map(edge_of))?;
                false
            }
            Action::RotateWindow { backwards } => {
                runtime.rotate_window(backwards)?;
                false
            }
            Action::BreakPane => {
                runtime.break_pane()?;
                false
            }
            Action::JoinPane { window, columns } => {
                let dir = if columns { Direction::Columns } else { Direction::Rows };
                runtime.join_pane(window, dir)?;
                false
            }
            Action::CloseWindow => runtime.close_window()?,
            Action::SendKeys { pane, bytes } => {
                let target = pane.map(PaneId).unwrap_or_else(|| runtime.session.active_pane());
                runtime.send_to(target, &bytes)?;
                false
            }
            // Handled before the dispatch, where every session is reachable.
            Action::DetachOthers | Action::SaveSessions | Action::RestoreSessions => false,
            Action::SynchronizePanes { on } => {
                let window = runtime.session.active_window_mut();
                window.synchronized = on.unwrap_or(!window.synchronized);
                false
            }
            Action::RespawnPane { program } => {
                runtime.respawn_active(program.map(Into::into))?;
                false
            }
            Action::NextLayout => {
                runtime.next_layout()?;
                false
            }
            Action::Rename { window, name } => {
                let id = window.then(|| runtime.session.active_window().id);
                runtime.session.rename(id, &name);
                // The session's own name is on the status bar, which is only
                // resent when it changes; renaming is exactly such a change.
                false
            }
        };
        // Whatever the action left current is this client's tab now — that
        // covers selecting one outright, stepping to the next, and the ones
        // that move you implicitly, like breaking a pane into its own tab.
        // The view is cleared because it belongs to a client and the session
        // outlives every one of them.
        if let Some(host) = self.host_of(index) {
            let current = host.runtime.session.current_window();
            let before = host.runtime.session.window_before();
            host.runtime.session.view = None;
            if let Some(client) = self.clients.get_mut(index) {
                client.window = Some(current);
                client.window_before = before;
            }
        }
        if ended {
            let id = self.clients[index].session.take();
            if let Some(id) = id {
                self.sessions.remove(&id);
            }
        }
        Ok(())
    }

    fn pump_sessions(&mut self) -> io::Result<()> {
        let mut ended: Vec<u32> = Vec::new();
        let mut bells: Vec<(u32, PaneId)> = Vec::new();
        for (id, host) in self.sessions.iter_mut() {
            for event in host.runtime.pump(Duration::from_millis(0))? {
                match event {
                    RuntimeEvent::SessionEnded => ended.push(*id),
                    RuntimeEvent::Bell(pane) => bells.push((*id, pane)),
                    RuntimeEvent::Dirty => {}
                }
            }
        }
        for (session, pane) in bells {
            // Mark the window so the bar can show it after the sound has
            // gone: a bell you were not looking at is the one worth seeing.
            if let Some(host) = self.sessions.get_mut(&session) {
                host.runtime.session.note_bell(pane);
            }
            for client in self.clients.iter_mut().filter(|c| c.session == Some(session)) {
                let _ = client.framed.queue(&ServerMsg::Bell { pane });
            }
        }
        for id in ended {
            self.sessions.remove(&id);
            for client in self.clients.iter_mut().filter(|c| c.session == Some(id)) {
                let _ = client.framed.queue(&ServerMsg::Refused { reason: "session ended".into() });
                client.session = None;
            }
        }
        Ok(())
    }

    /// Size each session according to its policy over the attached clients.
    fn apply_resize_policy(&mut self) -> io::Result<()> {
        for (id, host) in self.sessions.iter_mut() {
            let sizes: Vec<CellSize> = self
                .clients
                .iter()
                .filter(|c| c.session == Some(*id))
                .map(|c| c.caps.size)
                .collect();
            if sizes.is_empty() {
                continue;
            }
            let target = match host.policy {
                // The smallest client, so nothing is hidden from anyone.
                ResizePolicy::Smallest => CellSize {
                    cols: sizes.iter().map(|s| s.cols).min().unwrap_or(80),
                    rows: sizes.iter().map(|s| s.rows).min().unwrap_or(24),
                    pixel_width: sizes.iter().map(|s| s.pixel_width).min().unwrap_or(0),
                    pixel_height: sizes.iter().map(|s| s.pixel_height).min().unwrap_or(0),
                },
                ResizePolicy::Latest => *sizes.last().expect("non-empty"),
            };
            if host.applied != Some(target) {
                host.runtime.resize(target)?;
                host.applied = Some(target);
                for client in self.clients.iter_mut().filter(|c| c.session == Some(*id)) {
                    client.sync.invalidate();
                }
            }
        }
        Ok(())
    }

    fn sync_clients(&mut self) -> io::Result<()> {
        // Counted once: the status line reports how many clients share the
        // session, and that is a property of the session, not of the reader.
        let mut attached: std::collections::HashMap<u32, u16> = std::collections::HashMap::new();
        for client in &self.clients {
            if let Some(id) = client.session {
                *attached.entry(id).or_default() += 1;
            }
        }
        let theme = self.config.theme();
        let keys = self.config.keys();
        for client in &mut self.clients {
            if client.theme_sent.as_ref() != Some(theme) {
                let _ = client.framed.queue(&ServerMsg::Theme(Box::new(theme.clone())));
                client.theme_sent = Some(theme.clone());
            }
            if client.keys_sent.as_ref() != Some(keys) {
                let _ = client.framed.queue(&ServerMsg::Keys(Box::new(keys.clone())));
                client.keys_sent = Some(keys.clone());
            }
            let Some(id) = client.session else { continue };
            let Some(host) = self.sessions.get(&id) else { continue };
            sync_one(client, host, attached.get(&id).copied().unwrap_or(0));
        }
        Ok(())
    }

    fn flush_clients(&mut self) {
        for client in &mut self.clients {
            let _ = client.framed.flush();
        }
        self.clients.retain(|c| !c.framed.is_closed() || c.framed.has_pending_output());
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Leave no socket behind for the next start to trip over.
        if let Some(path) = &self.socket_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A command line, capped. Unlike a title it must *not* be split on `/`:
/// doing that turns `ls -la /tmp` into `tmp`, which names the wrong thing
/// entirely.
fn short_command(command: &str) -> String {
    command.trim().chars().take(24).collect()
}

/// A window name has to share one row with everything else, so take the last
/// path component and cap it.
fn short_title(title: &str) -> String {
    let tail = title.rsplit('/').next().unwrap_or(title).trim();
    let tail = if tail.is_empty() { title.trim() } else { tail };
    tail.chars().take(16).collect()
}

/// `$XDG_CONFIG_HOME/haven/config.toml`, falling back to `~/.config`.
pub fn default_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Where a named theme is looked up.
pub fn default_theme_dir() -> PathBuf {
    config_dir().join("themes")
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("haven")
}

/// Seconds since the epoch, or `0` if the clock is before it.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl SessionHost {
    /// What this session is called.
    ///
    /// Read from the mux rather than kept beside it. It *was* kept beside
    /// it, and `rename-session` wrote the mux's copy while the status bar,
    /// `haven ls` and the session tree all read this one — so renaming a
    /// session worked perfectly and showed up nowhere.
    fn name(&self) -> &str {
        &self.runtime.session.name
    }

    /// What the status line needs. Window names fall back to the active
    /// pane's title, which is what a shell sets and what a user recognises.
    /// `on` is the window the reading client is looking at, which is what
    /// its status bar must mark as active — not the session's own, which
    /// belongs to whichever client last chose one.
    fn status(
        &self,
        clients: u16,
        on: Option<haven_proto::WindowId>,
    ) -> haven_proto::SessionStatus {
        let session = &self.runtime.session;
        let windows = self.windows();
        let active = on
            .filter(|id| session.window(*id).is_some())
            .or_else(|| session.windows.get(session.active).map(|w| w.id));
        haven_proto::SessionStatus {
            session: session.id,
            name: self.name().to_string(),
            windows,
            active_window: active.map(|id| id.0).unwrap_or(0),
            started_unix: self.started_unix,
            clients,
            // Whatever this session's plugins last drew. Taken from a
            // cache rather than by calling them here: `status` is `&self`
            // and running a plugin needs `&mut`, and more to the point a
            // plugin should be called when something happened, not every
            // time a bar is rebuilt.
            plugins: self.drawn.clone(),
        }
    }

    /// What is in this session: its tabs, their panes, and what each is
    /// doing.
    ///
    /// Shared with the session listing, because the tree has to show the
    /// tabs of a session nobody is attached to and a count cannot be
    /// navigated.
    /// Which user a pane is running as, when it is not the one Haven is.
    ///
    /// Read from the foreground process group, so `sudo vim` marks the pane
    /// while vim is up and unmarks it when you quit, and `sudo -i` marks it
    /// until the root shell exits.
    fn user_of(&self, pane: haven_proto::PaneId) -> Option<String> {
        let runtime = self.runtime.pane(pane)?;
        let group = runtime.foreground_pid()?;
        if let Some((cached, user)) = self.users.borrow().get(&pane.0) {
            if *cached == group {
                return user.clone();
            }
        }
        let uid = runtime.foreground_uid();
        let user = uid
            .filter(|uid| *uid != haven_pty::our_uid())
            .map(|uid| user_name(uid).unwrap_or_else(|| format!("uid {uid}")));
        self.users.borrow_mut().insert(pane.0, (group, user.clone()));
        user
    }

    fn windows(&self) -> Vec<haven_proto::WindowInfo> {
        let session = &self.runtime.session;
        // Solved once rather than per pane: a client labelling panes needs
        // their geometry, and working it out again would be a second answer
        // to a question the layout has already settled.
        let rects = self.runtime.session.layout(self.runtime.size()).panes;
        session
            .windows
            .iter()
            .map(|w| haven_proto::WindowInfo {
                id: w.id.0,
                synchronized: w.synchronized,
                bell: w.bell,
                activity: w.activity,
                // What the tab is *doing* names it best. An explicit name
                // wins, then the command the shell reported through OSC 133,
                // then the title — which is usually a path — and only then
                // the number, which tells nobody anything.
                name: w.title.clone().unwrap_or_else(|| {
                    let pane = self.runtime.pane(w.active);
                    pane.and_then(|p| p.command())
                        .map(|(c, _)| short_command(c))
                        .filter(|c| !c.is_empty())
                        .or_else(|| pane.and_then(|p| p.title()).map(short_title))
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| format!("{}", w.id.0))
                }),
                panes: w
                    .root
                    .panes()
                    .into_iter()
                    .map(|p| {
                        let pane = self.runtime.pane(p);
                        let rect = rects.iter().find(|r| r.pane == p).copied().unwrap_or_default();
                        let (command, running) = pane
                            .and_then(|r| r.command())
                            .map(|(c, running)| (short_command(c), running))
                            .unwrap_or_default();
                        haven_proto::PaneInfo {
                            id: p.0,
                            rect,
                            title: pane
                                .and_then(|r| r.title())
                                .map(short_title)
                                .unwrap_or_default(),
                            command,
                            running,
                            user: self.user_of(p),
                        }
                    })
                    .collect(),
                active_pane: w.active.0,
                zoomed: w.zoomed.is_some(),
            })
            .collect()
    }
}

/// Bring one client up to date with one session.
fn sync_one(client: &mut Client, host: &SessionHost, clients: u16) {
    // Everything below is drawn from the window *this* client is on. The
    // session has no single current window once two clients are attached.
    let on = client
        .window
        .filter(|id| host.runtime.session.window(*id).is_some())
        .unwrap_or_else(|| host.runtime.session.current_window());
    client.window = Some(on);
    let layout = host.runtime.session.layout_of(on, host.runtime.size());
    if client.sync.layout_changed(&layout) {
        let _ = client.framed.queue(&ServerMsg::Layout(layout.clone()));
        // Panes that vanished must not keep stale row revisions.
        let live: Vec<u32> = layout.panes.iter().map(|r| r.pane.0).collect();
        for pane in client.sync.known_panes() {
            if !live.contains(&pane.0) {
                client.sync.forget(pane);
                let _ = client.framed.queue(&ServerMsg::PaneExited { pane, status: 0 });
            }
        }
    }

    let status = host.status(clients, Some(on));
    if client.last_status.as_ref() != Some(&status) {
        let _ = client.framed.queue(&ServerMsg::Status(status.clone()));
        client.last_status = Some(status);
    }

    for (rect, pane) in host.runtime.visible_of(on) {
        let grid = pane.grid();
        let styles = pane.styles();
        let state = client.sync.pane(rect.pane);

        // Tables first: a snapshot that references an unknown style id would
        // render with the wrong colours.
        let styles_len = styles.len().min(u16::MAX as usize) as u16;
        let clusters_len = grid.clusters().len().min(u16::MAX as usize) as u16;
        let links = pane.links();
        let links_len = links.len().min(u16::MAX as usize) as u16;
        if styles_len > state.styles_sent
            || clusters_len > state.clusters_sent
            || links_len > state.links_sent
        {
            let tables = Tables {
                pane: rect.pane,
                styles_from: state.styles_sent,
                styles: (state.styles_sent..styles_len)
                    .map(|i| styles.get(haven_core::StyleId(i)))
                    .collect(),
                clusters_from: state.clusters_sent,
                clusters: (state.clusters_sent..clusters_len)
                    .map(|i| grid.clusters().get(i + 1).unwrap_or_default().to_string())
                    .collect(),
                links_from: state.links_sent,
                links: (state.links_sent..links_len)
                    .map(|i| links.get(i + 1).unwrap_or_default().to_string())
                    .collect(),
            };
            state.styles_sent = styles_len;
            state.clusters_sent = clusters_len;
            state.links_sent = links_len;
            let _ = client.framed.queue(&ServerMsg::Tables(tables));
        }

        // The view this client is reading. At rest it is the live screen, so
        // nothing below has to ask whether anyone is scrolled.
        let history = grid.max_scroll();
        state.offset = state.offset.min(history);
        let offset = state.offset;
        let view = grid.view(offset);

        // A cursor outside the view is not drawn: a block parked at the edge
        // of a page of history says the wrong thing about where typing goes.
        let cursor = scrolled_cursor(grid.cursor, offset, grid.num_rows());
        let modes = PaneModes {
            app_cursor_keys: pane.emulator.term.modes.app_cursor_keys,
            bracketed_paste: pane.emulator.term.modes.bracketed_paste,
            cursor_visible: pane.emulator.term.modes.cursor_visible && cursor.is_some(),
            mouse: mouse_mode(&pane.emulator.term.modes),
        };
        if state.modes != Some(modes) {
            state.modes = Some(modes);
            let _ = client.framed.queue(&ServerMsg::Modes {
                pane: rect.pane,
                app_cursor_keys: modes.app_cursor_keys,
                bracketed_paste: modes.bracketed_paste,
                mouse: modes.mouse,
                cursor_visible: modes.cursor_visible,
            });
        }

        let position = (offset as u32, history as u32);
        if state.reported != Some(position) {
            state.reported = Some(position);
            let _ = client.framed.queue(&ServerMsg::Scrolled {
                pane: rect.pane,
                offset: position.0,
                total: position.1,
            });
        }

        let cursor = cursor.unwrap_or(grid.cursor);
        // History growing under a scrolled view moves every row, and row
        // revisions cannot detect that: they are per-row counters, so two
        // unrelated rows may share one. The view is resent whole instead.
        let shifted = offset > 0 && state.anchor != history;
        if !state.snapshotted || state.rows.len() != view.len() || shifted {
            state.rows = view.iter().map(|r| r.rev).collect();
            state.snapshotted = true;
            state.anchor = history;
            state.cursor = Some(cursor);
            let _ = client.framed.queue(&ServerMsg::Snapshot {
                pane: rect.pane,
                rows: view.iter().map(|r| encode_row(r)).collect(),
                cursor,
                rev: view.first().map(|r| r.rev).unwrap_or(0),
            });
        } else {
            let changed: Vec<(u16, RowRle)> = view
                .iter()
                .enumerate()
                .filter(|(i, row)| state.rows.get(*i) != Some(&row.rev))
                .map(|(i, row)| (i as u16, encode_row(row)))
                .collect();
            let cursor_moved = state.cursor != Some(cursor);
            if !changed.is_empty() || cursor_moved {
                for (i, _) in &changed {
                    if let Some(slot) = state.rows.get_mut(*i as usize) {
                        *slot = view[*i as usize].rev;
                    }
                }
                state.cursor = Some(cursor);
                let _ = client.framed.queue(&ServerMsg::Delta {
                    pane: rect.pane,
                    rows: changed,
                    cursor,
                    rev: 0,
                });
            }
        }

        let title = pane.title().map(str::to_string);
        if title.is_some() && state.title != title {
            state.title = title.clone();
            let _ = client
                .framed
                .queue(&ServerMsg::Title { pane: rect.pane, title: title.unwrap_or_default() });
        }
    }
}

/// The protocol's edge as the mux spells it.
fn edge_of(edge: haven_proto::Edge) -> Edge {
    match edge {
        haven_proto::Edge::Left => Edge::Left,
        haven_proto::Edge::Right => Edge::Right,
        haven_proto::Edge::Up => Edge::Up,
        haven_proto::Edge::Down => Edge::Down,
    }
}

/// What a pane's modes amount to for one client, which is what gets diffed
/// against what that client was last told.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PaneModes {
    pub app_cursor_keys: bool,
    pub bracketed_paste: bool,
    pub cursor_visible: bool,
    pub mouse: haven_proto::MouseMode,
}

fn mouse_mode(modes: &haven_core::term::Modes) -> haven_proto::MouseMode {
    use haven_core::term::MouseReport;
    use haven_proto::MouseTracking;
    haven_proto::MouseMode {
        tracking: match modes.mouse_report {
            MouseReport::Off => MouseTracking::Off,
            MouseReport::Click => MouseTracking::Click,
            MouseReport::ButtonEvent => MouseTracking::ButtonEvent,
            MouseReport::AnyEvent => MouseTracking::AnyEvent,
        },
        sgr: modes.mouse_sgr,
    }
}

/// Where the cursor sits in a scrolled view, or `None` when it is below it.
///
/// The view is `offset` rows above the live bottom, so the cursor moves down
/// by that much and falls off the end once the view has gone back further
/// than the cursor's own row.
fn scrolled_cursor(
    cursor: haven_core::Cursor,
    offset: usize,
    rows: usize,
) -> Option<haven_core::Cursor> {
    let row = cursor.row.checked_add(offset)?;
    (row < rows).then_some(haven_core::Cursor { row, ..cursor })
}

fn encode_row(row: &Row) -> RowRle {
    RowRle::encode(&row.cells)
}

/// The default socket path for a named session set.
pub fn socket_path(name: &str) -> PathBuf {
    let base =
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    // SAFETY: getuid cannot fail and touches no memory we own.
    let uid = unsafe { libc::getuid() };
    base.join("haven").join(format!("{uid}-{name}.sock"))
}

/// A user's name, from `/etc/passwd`.
///
/// Parsed rather than asked of `getpwuid`, which is not reentrant and whose
/// answer this does not need: the one name that matters is root's, every
/// passwd file has it, and a uid this cannot name is shown as a number
/// rather than as nothing.
fn user_name(uid: u32) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let found: u32 = fields.nth(1)?.parse().ok()?;
        (found == uid).then(|| name.to_string())
    })
}

/// Which rows of a pane to preview, when it is taller than the panel.
///
/// A shell's news is at the **bottom**: the prompt you are at, and whatever
/// just finished. Taking the top gave every pane a preview of its login
/// banner — the oldest thing on the screen and the one thing they all have
/// in common — while the output that would have said which pane this was
/// sat below the fold.
///
/// A full-screen program is the other way round. It composes the whole
/// screen from the top, and its first rows are its title and its menu, so
/// that is where it is recognisable.
fn preview_window(grid: &Grid, rows: usize) -> std::ops::Range<usize> {
    let all = grid.rows();
    if grid.is_alt {
        return 0..rows.min(all.len());
    }
    let used = all
        .iter()
        .rposition(|row| row.cells.iter().any(|c| !grid.cell_text(c).trim().is_empty()))
        .map(|r| r + 1)
        .unwrap_or(0);
    // The cursor counts as used even on a blank row: a pane sitting at an
    // empty prompt should preview as an empty prompt, not as the paragraph
    // above it.
    let end = used.max(grid.cursor.row + 1).min(all.len());
    end.saturating_sub(rows)..end
}

/// Silence the unused-import warning for a type used only in a signature.
#[doc(hidden)]
pub fn _grid_anchor(_: &Grid) {}

#[cfg(test)]
mod user_tests {
    #[test]
    fn root_is_named_rather_than_numbered() {
        // Every passwd file has it, which is the point: the one name that
        // matters resolves without asking a name service.
        assert_eq!(super::user_name(0).as_deref(), Some("root"));
    }

    #[test]
    fn a_uid_with_no_name_is_shown_as_a_number_rather_than_as_nothing() {
        // A container pane running as uid 65533 is still not yours, and a
        // mark that disappears because the name lookup failed is the
        // failure you would not notice.
        assert_eq!(super::user_name(4_000_000_001), None);
    }
}

#[cfg(test)]
mod preview_tests {
    use super::preview_window;
    use haven_core::Grid;

    fn filled(rows: usize, used: usize) -> Grid {
        let mut grid = Grid::new(20, rows);
        for row in 0..used {
            grid.cursor.row = row;
            grid.cursor.col = 0;
            for c in format!("line {row}").chars() {
                grid.write(c, 1);
            }
        }
        grid.cursor.row = used.saturating_sub(1);
        grid
    }

    fn text(grid: &Grid, window: std::ops::Range<usize>) -> Vec<String> {
        grid.rows()[window]
            .iter()
            .map(|row| row.cells.iter().map(|c| grid.cell_text(c).into_owned()).collect::<String>())
            .map(|line| line.trim_end().to_string())
            .collect()
    }

    #[test]
    fn a_shell_previews_its_last_rows_not_its_first() {
        // It previewed the first: every pane showed its login banner, and
        // the one thing a preview is for — telling panes apart — was the
        // thing it could not do.
        let grid = filled(40, 40);
        let shown = text(&grid, preview_window(&grid, 5));
        assert_eq!(shown, vec!["line 35", "line 36", "line 37", "line 38", "line 39"]);
    }

    #[test]
    fn a_pane_with_less_on_it_than_the_panel_holds_starts_at_the_top() {
        // No blank band above three lines of output.
        let grid = filled(40, 3);
        assert_eq!(text(&grid, preview_window(&grid, 20)), vec!["line 0", "line 1", "line 2"]);
    }

    #[test]
    fn an_empty_prompt_below_the_output_is_kept_in_view() {
        // The cursor is on a blank row under the text. Anchoring on the
        // last *written* row would scroll the prompt you are sitting at out
        // of its own preview.
        let mut grid = filled(40, 10);
        grid.cursor.row = 10;
        let shown = text(&grid, preview_window(&grid, 3));
        assert_eq!(shown, vec!["line 8", "line 9", ""]);
    }

    #[test]
    fn a_full_screen_program_previews_from_the_top() {
        let mut grid = Grid::new_alt(20, 40);
        for row in 0..40 {
            grid.cursor.row = row;
            grid.cursor.col = 0;
            for c in format!("line {row}").chars() {
                grid.write(c, 1);
            }
        }
        // Its title and its menu are at the top, which is where it is
        // recognisable; its last rows are a status line every one of them
        // has.
        assert_eq!(text(&grid, preview_window(&grid, 2)), vec!["line 0", "line 1"]);
    }
}

/// The plugin repositories, as the browser shows them.
///
/// All the verifying happens here, on the daemon's own disk, so that a
/// client is handed conclusions rather than evidence. A repository whose
/// cached index does not verify — or which has never been refreshed —
/// comes back with `problem` set rather than silently empty: "nothing to
/// show" and "something is wrong" are different answers and a browser
/// that conflates them is a browser that hides the interesting case.
fn plugin_repos() -> Vec<haven_proto::RepoInfo> {
    use haven_plugin::repo::{self, Paths, Sources};
    use haven_plugin::store::Installed;

    let paths = Paths::standard();
    let Ok(sources) = Sources::load(&paths) else { return Vec::new() };
    // One read of what is installed, for every repository: the record is
    // not per-repository and re-reading it per row would be a file read
    // per keystroke once a client refreshes this.
    let installed = Installed::load(&paths).unwrap_or_default();

    sources
        .list
        .iter()
        .map(|source| {
            let changes = repo::changes(&source.name, &paths)
                .into_iter()
                .map(|change| haven_proto::PluginChange {
                    summary: change.kind.summary(),
                    name: change.name,
                    when: change.when,
                })
                .collect();
            let mut info = haven_proto::RepoInfo {
                name: source.name.clone(),
                url: source.url.clone(),
                key: haven_plugin::sign::short(&source.fingerprint),
                checked: source.trust.is_verified(),
                problem: None,
                plugins: Vec::new(),
                changes,
            };
            match repo::cached(source, &paths) {
                Ok(index) => {
                    info.plugins = index
                        .plugins
                        .iter()
                        .map(|plugin| haven_proto::PluginInfo {
                            name: plugin.name.clone(),
                            version: plugin.version.clone(),
                            summary: plugin.summary.clone().unwrap_or_default(),
                            installed: installed
                                .get(&plugin.name)
                                .map(|record| record.version.clone()),
                            asks: plugin.capabilities.lines(),
                            reads_panes: plugin.capabilities.read_pane_content,
                            granted: installed
                                .get(&plugin.name)
                                .map(|record| record.granted.lines())
                                .unwrap_or_default(),
                        })
                        .collect();
                }
                Err(e) => info.problem = Some(e.to_string()),
            }
            info
        })
        .collect()
}

/// Everything about running this session's plugins.
///
/// Plugins run here, in the daemon, because that is where the panes are
/// and because they have to survive a client detaching — a status
/// segment that vanishes when you close the window and comes back
/// different when you reattach is not a status segment. A client is sent
/// what they drew, never the code: over SSH it could not run it, and if
/// it could, it would be running with capabilities granted for another
/// machine.
impl Daemon {
    /// Load every installed plugin, with the capabilities its record
    /// says were granted.
    ///
    /// A plugin that will not load is logged and skipped rather than
    /// taking the session with it. That is the whole lifecycle rule: one
    /// broken plugin is a missing segment, not a terminal that will not
    /// start.
    fn load_plugins(&mut self) -> Vec<haven_plugin_host::Loaded> {
        use haven_plugin::repo::Paths;
        use haven_plugin::store::{self, Installed};
        use haven_plugin_host::{Load, Loaded};

        let paths = Paths::standard();
        let Ok(installed) = Installed::load(&paths) else { return Vec::new() };
        if installed.list.is_empty() {
            return Vec::new();
        }
        // The engine is built on first use rather than at startup: a
        // daemon with no plugins should not pay for a JIT it will never
        // call.
        let engine = match &self.plugin_engine {
            Some(engine) => engine,
            None => match haven_plugin_host::Engine::new() {
                Ok(engine) => self.plugin_engine.insert(engine),
                Err(e) => {
                    eprintln!("haven: plugins are unavailable: {e}");
                    return Vec::new();
                }
            },
        };

        let mut out = Vec::new();
        for record in &installed.list {
            let dir = store::plugin_dir(&paths, &record.name);
            let component = dir.join(&record.entry);
            let wasm = match std::fs::read(&component) {
                Ok(wasm) => wasm,
                Err(e) => {
                    eprintln!("haven: plugin `{}`: {}: {e}", record.name, component.display());
                    continue;
                }
            };
            let load = Load {
                name: &record.name,
                wasm: &wasm,
                granted: &record.granted,
                // Beside the plugin's own files, so uninstalling it takes
                // what it remembered with it.
                store_path: Some(dir.join("remembered")),
                world: haven_plugin_host::Snapshot::default(),
                // Beside the plugin, so uninstalling it takes the
                // compiled form with it and a reinstall recompiles.
                cache_dir: Some(dir.clone()),
            };
            match Loaded::new(engine, load) {
                Ok(loaded) => out.push(loaded),
                Err(e) => eprintln!("haven: {e}"),
            }
        }
        out
    }

    /// Do what the plugins asked for.
    ///
    /// Separate from [`Daemon::drive_plugins`] because applying a command
    /// needs the daemon — the client list, the session table — and the
    /// loop that collects them is already holding one session. So the
    /// commands are queued there and applied here, which is also exactly
    /// what the design intends: a plugin's effects are data, and this is
    /// the one place that turns them into effects.
    fn apply_plugin_commands(&mut self) {
        use haven_plugin_host::Command;

        let mut taken: Vec<(u32, String, Command)> = Vec::new();
        for (id, host) in self.sessions.iter_mut() {
            for (name, command) in host.pending.drain(..) {
                taken.push((*id, name, command));
            }
        }
        for (session, name, command) in taken {
            match command {
                Command::Notice(text) => {
                    // To the clients on that session only: a plugin in one
                    // session has nothing to say to another.
                    for client in &mut self.clients {
                        if client.session == Some(session) {
                            let _ = client
                                .framed
                                .queue(&ServerMsg::Notice { text: format!("{name}: {text}") });
                        }
                    }
                }
                // The store belongs to the plugin, not to the session
                // that asked — there is one instance and one file. Two
                // sessions each holding an instance of `pomodoro` used
                // to write the same file and clobber each other, which
                // is the clearest argument against loading per session.
                Command::Remember(what) => {
                    if let Some(plugin) = self.plugins.iter_mut().find(|p| p.name() == name) {
                        plugin.remember(&what.key, Some(&what.value));
                    }
                }
                Command::Forget(key) => {
                    if let Some(plugin) = self.plugins.iter_mut().find(|p| p.name() == name) {
                        plugin.remember(&key, None);
                    }
                }
                Command::Act(action) => {
                    // Deliberately not wired yet, and said out loud rather
                    // than dropped. Turning a name like `split-v` into an
                    // `Action` lives in the `haven` crate's key parser,
                    // which the daemon cannot reach without a dependency
                    // the wrong way round; moving that vocabulary into
                    // `haven-proto` is the fix and is its own change.
                    eprintln!(
                        "haven: plugin `{name}` asked to run `{action}`, \
                         which plugins cannot do yet"
                    );
                }
            }
        }
    }

    /// Tell every session's plugins what has happened, and collect what
    /// they drew.
    ///
    /// Called once per tick. Events are found by diffing what the daemon
    /// already knows against what it knew last time, rather than by
    /// threading a callback through the mux for every event a plugin
    /// might one day want: the status bar is rebuilt from this same
    /// information anyway, so comparing it is nearly free and touches
    /// nothing.
    fn drive_plugins(&mut self) {
        // Loaded on the first tick rather than at startup, so a daemon
        // that is only ever asked to list sessions never builds a JIT,
        // and once rather than per session.
        if !self.plugins_loaded {
            self.plugins_loaded = true;
            self.plugins = self.load_plugins();
        }
        if self.plugins.is_empty() || self.sessions.is_empty() {
            return;
        }
        // One pass per session, over the one set of plugins. What makes
        // that correct is `refresh`: a plugin is handed the session's
        // own snapshot immediately before it is asked anything, so the
        // answers are per session even though the instance is not.
        let ids: Vec<u32> = self.sessions.keys().copied().collect();
        for id in ids {
            let Some(host) = self.sessions.get_mut(&id) else { continue };
            let events = host.plugin_events();
            let reading = self.plugins.iter().any(|p| p.reads_panes());
            let plain = host.plugin_snapshot(false);
            let with_text = reading.then(|| host.plugin_snapshot(true));

            let mut drawn = Vec::new();
            let mut asked: Vec<(String, haven_plugin_host::Command)> = Vec::new();
            let mut broken = Vec::new();
            for (at, plugin) in self.plugins.iter_mut().enumerate() {
                let world = if plugin.reads_panes() {
                    with_text.clone().unwrap_or_else(|| plain.clone())
                } else {
                    plain.clone()
                };
                plugin.refresh(world);

                let name = plugin.name().to_string();
                let mut failed = false;
                for event in &events {
                    match plugin.on_event(event) {
                        Ok(commands) => {
                            asked.extend(commands.into_iter().map(|c| (name.clone(), c)))
                        }
                        Err(e) => {
                            eprintln!("haven: {e}");
                            failed = true;
                            break;
                        }
                    }
                }
                if failed {
                    broken.push(at);
                    continue;
                }
                match plugin.render() {
                    Ok(runs) if !runs.is_empty() => drawn.push(haven_proto::PluginSegment {
                        name,
                        runs: runs.iter().map(styled).collect(),
                    }),
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("haven: {e}");
                        broken.push(at);
                    }
                }
            }

            if let Some(host) = self.sessions.get_mut(&id) {
                host.drawn = drawn;
                host.pending.extend(asked);
            }
            // A plugin that trapped is unloaded, not retried in a loop.
            // The alternative is a daemon that spends every tick running
            // the same failing plugin and logging the same line.
            for at in broken.into_iter().rev() {
                let gone = self.plugins.remove(at);
                eprintln!("haven: plugin `{}` was unloaded after it failed", gone.name());
            }
        }
    }
}

impl SessionHost {
    /// What this session looks like, for a plugin to be told about.
    fn plugin_snapshot(&self, with_text: bool) -> haven_plugin_host::Snapshot {
        let focus = self.runtime.session.active_pane();
        let mut panes = Vec::new();
        let mut text = std::collections::BTreeMap::new();
        for window in &self.runtime.session.windows {
            for id in window.panes() {
                let Some(pane) = self.runtime.pane(id) else { continue };
                let (command, running) =
                    pane.command().map(|(c, r)| (c.to_string(), r)).unwrap_or_default();
                panes.push(haven_plugin_host::Pane {
                    id: id.0,
                    title: pane.title().unwrap_or_default().to_string(),
                    command,
                    cwd: pane.cwd().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
                    running,
                    focused: id == focus,
                });
                // Only for a plugin that may look. Copying every pane's
                // screen for every plugin on every tick would be the most
                // expensive thing the daemon does, and for most plugins it
                // would be copying something they are not allowed to read.
                if with_text {
                    text.insert(id.0, pane_as_text(pane.grid()));
                }
            }
        }
        haven_plugin_host::Snapshot { panes, text }
    }

    /// The facts plugins are told about, as they are now.
    fn plugin_view(&self) -> PluginView {
        let mut cwd = HashMap::new();
        let mut finished = HashMap::new();
        let mut panes = Vec::new();
        for window in &self.runtime.session.windows {
            for id in window.panes() {
                panes.push(id.0);
                if let Some(pane) = self.runtime.pane(id) {
                    if let Some(dir) = pane.cwd() {
                        cwd.insert(id.0, dir.to_string_lossy().to_string());
                    }
                    finished.insert(id.0, pane.history().len());
                }
            }
        }
        PluginView { focus: Some(self.runtime.session.active_pane().0), cwd, finished, panes }
    }

    /// What has happened in this session since the plugins were last
    /// told, and the timekeeping that goes with it.
    fn plugin_events(&mut self) -> Vec<haven_plugin_host::Event> {
        use haven_plugin_host::Event;

        // Note what is running, so a finish can be timed.
        for window in &self.runtime.session.windows {
            for id in window.panes() {
                let Some(pane) = self.runtime.pane(id) else { continue };
                if let Some((command, true)) = pane.command() {
                    let fresh =
                        self.started.get(&id.0).map(|(seen, _)| seen != command).unwrap_or(true);
                    if fresh {
                        self.started.insert(id.0, (command.to_string(), std::time::Instant::now()));
                    }
                }
            }
        }

        let now = self.plugin_view();
        let mut events: Vec<Event> = Vec::new();

        // First time through: nothing to compare against, so the only
        // honest event is that the plugin has started.
        if self.was == PluginView::default() {
            events.push(Event::Started);
        } else {
            if now.focus != self.was.focus {
                if let Some(focus) = now.focus {
                    events.push(Event::PaneFocus(focus));
                }
            }
            for id in &now.panes {
                if !self.was.panes.contains(id) {
                    events.push(Event::PaneOpened(*id));
                }
            }
            for id in &self.was.panes {
                if !now.panes.contains(id) {
                    events.push(Event::PaneClosed(*id));
                }
            }
            for (id, dir) in &now.cwd {
                if self.was.cwd.get(id) != Some(dir) {
                    events.push(Event::CwdChanged(haven_plugin_host::CwdChange {
                        pane: *id,
                        cwd: dir.clone(),
                    }));
                }
            }
            for (id, count) in &now.finished {
                let before = self.was.finished.get(id).copied().unwrap_or(0);
                if *count > before {
                    if let Some(end) = self.last_command(*id) {
                        events.push(Event::CommandEnded(end));
                    }
                }
            }
        }
        // Always, so a plugin that only wants a clock gets one.
        events.push(Event::Tick);
        self.was = now;
        events
    }

    /// The command a pane last finished, as an event.
    fn last_command(&self, pane: u32) -> Option<haven_plugin_host::CommandEnd> {
        let runs = self.runtime.pane(haven_proto::PaneId(pane))?.history();
        let run = runs.iter().next_back()?;
        Some(haven_plugin_host::CommandEnd {
            pane,
            command: run.command.clone(),
            status: run.exit.unwrap_or(-1),
            // Timed here, not by the shell: `OSC 133;D` carries an exit
            // status and nothing about duration, so the alternative to
            // the daemon noticing when a command appeared was an API
            // that promises a number it cannot produce. Zero means "we
            // did not see it start" — a plugin loaded while something
            // was already running — and the plugins are told so.
            millis: self
                .started
                .get(&pane)
                .filter(|(command, _)| *command == run.command)
                .map(|(_, at)| at.elapsed().as_millis() as u64)
                .unwrap_or(0),
        })
    }
}

/// A plugin's styled run, as the wire carries it.
fn styled(run: &haven_plugin_host::Run) -> haven_proto::StyledRun {
    use haven_plugin_host::Colour;
    haven_proto::StyledRun {
        text: run.text.clone(),
        // RGB is dropped to the nearest thing the bar can express rather
        // than honoured: a plugin that picks its own true colour is a
        // plugin that is unreadable on somebody's theme, and the indexed
        // palette is the theme's to define.
        colour: match run.style.fg {
            Colour::Default => None,
            Colour::Indexed(n) => Some(n),
            Colour::Rgb(_) => None,
        },
        bold: run.style.bold,
        dim: run.style.dim,
        italic: run.style.italic,
    }
}

/// A pane's screen as text, for a plugin granted `read-pane-content`.
fn pane_as_text(grid: &haven_core::Grid) -> String {
    grid.rows()
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| grid.cell_text(cell))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a finished plugin job has to say, and to which session.
///
/// `None` for the session means the client that asked has gone, and
/// everyone hears it rather than nobody.
type JobResult = (Option<u32>, String);
type JobSender = std::sync::mpsc::Sender<JobResult>;
type JobReceiver = std::sync::mpsc::Receiver<JobResult>;

/// What a plugin job was asked to do.
#[derive(Clone, Debug)]
enum PluginJob {
    Install(String),
    Remove(String),
}

impl Daemon {
    /// Start an install or a removal, and do not wait for it.
    ///
    /// Fetching an artifact is a clone or an HTTPS request, and a
    /// removal is a directory tree: neither belongs on the loop that
    /// answers clients. The thread sends back one line, which is shown
    /// on the bar of whoever asked.
    fn start_plugin_job(&mut self, session: Option<u32>, job: PluginJob) {
        let (tx, _) = self.plugin_jobs.get_or_insert_with(std::sync::mpsc::channel);
        let tx = tx.clone();
        std::thread::spawn(move || {
            let said = match job {
                PluginJob::Install(name) => match install_plugin(&name) {
                    Ok(record) => format!("installed {} {}", record.name, record.version),
                    Err(why) => why,
                },
                PluginJob::Remove(name) => {
                    match haven_plugin::store::remove(&name, &haven_plugin::repo::Paths::standard())
                    {
                        Ok(record) => format!("removed {}", record.name),
                        Err(e) => e.to_string(),
                    }
                }
            };
            let _ = tx.send((session, said));
        });
    }

    /// Tell whoever asked how it went, and reload if anything changed.
    fn drain_plugin_jobs(&mut self) {
        let Some((_, rx)) = &self.plugin_jobs else { return };
        let done: Vec<(Option<u32>, String)> = rx.try_iter().collect();
        if done.is_empty() {
            return;
        }
        for (session, said) in done {
            for client in &mut self.clients {
                // To the session that asked, or to everyone when the
                // asking client has since gone.
                if session.is_none() || client.session == session {
                    let _ = client.framed.queue(&ServerMsg::Notice { text: said.clone() });
                }
            }
        }
        // The set changed, so load it again: a plugin installed from the
        // browser should appear on the bar without opening a new
        // terminal, which was the one caveat the command line has.
        self.plugins.clear();
        self.plugins = self.load_plugins();
        for host in self.sessions.values_mut() {
            host.was = PluginView::default();
        }
    }
}

/// Find a plugin by name in the verified indices, and install it.
///
/// The capabilities come from the index, not from the client: a client
/// decides whether to ask and may say no, but it does not get to say
/// what a plugin is allowed to do.
fn install_plugin(name: &str) -> Result<haven_plugin::store::Record, String> {
    use haven_plugin::repo::{self, Paths, Sources};

    let paths = Paths::standard();
    let sources = Sources::load(&paths).map_err(|e| e.to_string())?;
    let mut found = None;
    for source in &sources.list {
        let Ok(index) = repo::cached(source, &paths) else { continue };
        if let Some(plugin) = index.plugin(name) {
            if found.is_some() {
                // The same refusal the command line gives, for the same
                // reason: precedence is how you install the wrong one.
                return Err(format!("`{name}` is in more than one repository"));
            }
            found = Some((source.clone(), plugin.clone()));
        }
    }
    let Some((source, plugin)) = found else { return Err(format!("no plugin called `{name}`")) };

    let record = haven_plugin::store::install(
        &source,
        &plugin,
        &plugin.capabilities,
        &haven_plugin::fetch::Tools,
        &paths,
    )
    .map_err(|e| e.to_string())?;

    // Compiled here rather than when it is first loaded, which is a
    // moment away: the reload below would otherwise pay for it.
    let dir = haven_plugin::store::plugin_dir(&paths, &record.name);
    if let Ok(wasm) = std::fs::read(dir.join(&record.entry)) {
        if let Ok(engine) = haven_plugin_host::Engine::new() {
            let _ = engine.precompile(&record.name, &wasm, &dir);
        }
    }
    Ok(record)
}
