//! The mux server. See `docs/PLAN.md` §14.
//!
//! One single-threaded event loop over the listener, the attached clients and
//! every pane's pty. The plan reserved tokio for the daemon; a poll loop turns
//! out to be the simpler fit, because the pane pump is already poll-based and
//! nothing here benefits from task-per-pane concurrency. Revisit if per-pane
//! parsing ever needs its own core.

use std::collections::BTreeMap;
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
    sync: SyncState,
    /// The status last sent. Window names come from pane titles, which arrive
    /// after the first layout and change while the layout does not, so tying
    /// the status to layout changes left the names stuck at their ids.
    last_status: Option<haven_proto::SessionStatus>,
}

/// The server.
pub struct Daemon {
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
                if let Some(host) = self.host_of(index) {
                    // Synchronized windows send everything typed in one pane
                    // to all of them, which is the whole point of the mode.
                    let targets = if host.runtime.session.active_window().synchronized
                        && host.runtime.session.active_pane() == pane
                    {
                        host.runtime.input_targets()
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
                env: cell,
                started_unix: unix_now(),
                runtime,
                policy: self.policy,
                applied: Some(caps.size),
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
        let client = &mut self.clients[index];
        client.caps = caps;
        client.session = Some(id);
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
                env: cell,
                started_unix: unix_now(),
                runtime,
                policy: self.policy,
                applied: Some(size),
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

        let mine: Vec<haven_proto::HistoryEntry> = runtime
            .history()
            .iter()
            .map(|run| haven_proto::HistoryEntry {
                command: run.command.clone(),
                exit: run.exit,
                from_pane: true,
            })
            .collect();

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
        grid.rows()
            .iter()
            .take(rows)
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
        let Some(host) = self.host_of(index) else { return Ok(()) };
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
    fn status(&self, clients: u16) -> haven_proto::SessionStatus {
        let session = &self.runtime.session;
        let windows = self.windows();
        haven_proto::SessionStatus {
            session: session.id,
            name: self.name().to_string(),
            windows,
            active_window: session.windows.get(session.active).map(|w| w.id.0).unwrap_or(0),
            started_unix: self.started_unix,
            clients,
        }
    }

    /// What is in this session: its tabs, their panes, and what each is
    /// doing.
    ///
    /// Shared with the session listing, because the tree has to show the
    /// tabs of a session nobody is attached to and a count cannot be
    /// navigated.
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
    let layout = host.runtime.session.layout(host.runtime.size());
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

    let status = host.status(clients);
    if client.last_status.as_ref() != Some(&status) {
        let _ = client.framed.queue(&ServerMsg::Status(status.clone()));
        client.last_status = Some(status);
    }

    for (rect, pane) in host.runtime.visible() {
        let grid = pane.grid();
        let styles = pane.styles();
        let state = client.sync.pane(rect.pane);

        // Tables first: a snapshot that references an unknown style id would
        // render with the wrong colours.
        let styles_len = styles.len().min(u16::MAX as usize) as u16;
        let clusters_len = grid.clusters().len().min(u16::MAX as usize) as u16;
        if styles_len > state.styles_sent || clusters_len > state.clusters_sent {
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
            };
            state.styles_sent = styles_len;
            state.clusters_sent = clusters_len;
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

/// Silence the unused-import warning for a type used only in a signature.
#[doc(hidden)]
pub fn _grid_anchor(_: &Grid) {}
