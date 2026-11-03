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
use haven_pty::{Pty, PtySize};
use rustix::event::{poll, PollFd, PollFlags, Timespec};

pub mod config;
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

type Spawner = Box<dyn FnMut(PtySize) -> io::Result<Pty>>;

struct SessionHost {
    name: String,
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
    listener: Option<UnixListener>,
    socket_path: Option<PathBuf>,
    sessions: BTreeMap<u32, SessionHost>,
    clients: Vec<Client>,
    next_session: u32,
    make_spawner: Box<dyn FnMut() -> Spawner>,
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
    pub fn detached(make_spawner: impl FnMut() -> Spawner + 'static) -> Self {
        Self {
            listener: None,
            socket_path: None,
            sessions: BTreeMap::new(),
            clients: Vec::new(),
            next_session: 1,
            make_spawner: Box::new(make_spawner),
            policy: ResizePolicy::default(),
            shutting_down: false,
            config: config::ConfigWatcher::new(default_config_path(), default_theme_dir()),
        }
    }

    /// Bind a listening socket. A stale socket from a crashed daemon is
    /// replaced rather than refused: the alternative is a terminal that will
    /// not start until the user deletes a file.
    pub fn bind(
        path: impl AsRef<Path>,
        make_spawner: impl FnMut() -> Spawner + 'static,
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
                name: host.name.clone(),
                windows: host.runtime.session.windows.len() as u16,
                panes: host.runtime.session.panes().len() as u16,
                clients: self.clients.iter().filter(|c| c.session == Some(*id)).count() as u16,
                size: host.runtime.size(),
            })
            .collect()
    }

    pub fn session_names(&self) -> Vec<(SessionId, String, usize)> {
        self.sessions
            .iter()
            .map(|(id, host)| {
                (SessionId(*id), host.name.clone(), host.runtime.session.windows.len())
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
            for client in &mut self.clients {
                client.theme_sent = None;
            }
            if let Some(problem) = &self.config.problem {
                eprintln!("haven: config: {problem}");
            }
        }
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
            ClientMsg::Attach { session, caps } => {
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
                    None => self.first_or_create(caps)?,
                };
                self.join(index, id, caps);
            }
            ClientMsg::NewSession { name, caps } => {
                let id = self.create_session(name, caps)?;
                self.join(index, id, caps);
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
                    host.runtime.send_to(pane, &bytes)?;
                }
            }
            ClientMsg::Resize { caps } => {
                self.clients[index].caps = caps;
                // A client that changed size must be re-synced: its rows are
                // a different width now.
                self.clients[index].sync.invalidate();
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

    fn first_or_create(&mut self, caps: ClientCaps) -> io::Result<u32> {
        if let Some(id) = self.sessions.keys().next().copied() {
            return Ok(id);
        }
        self.create_session(None, caps)
    }

    /// Make a session, whether or not one already exists.
    fn create_session(&mut self, name: Option<String>, caps: ClientCaps) -> io::Result<u32> {
        let id = self.next_session;
        self.next_session += 1;
        // An unnamed session is numbered, which is what makes `haven-2`
        // follow `haven-1` without the client having to invent a name.
        let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("haven-{id}"));
        let spawner = (self.make_spawner)();
        let runtime = MuxRuntime::new(SessionId(id), &name, caps.size, spawner)?;
        self.sessions.insert(
            id,
            SessionHost {
                name,
                started_unix: unix_now(),
                runtime,
                policy: self.policy,
                applied: Some(caps.size),
            },
        );
        Ok(id)
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

    /// A pane's grid, for the read-only queries copy mode makes.
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
            Action::SplitColumns => {
                runtime.split(Direction::Columns)?;
                false
            }
            Action::SplitRows => {
                runtime.split(Direction::Rows)?;
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
            Action::NewWindow => {
                runtime.new_window()?;
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
        for client in &mut self.clients {
            if client.theme_sent.as_ref() != Some(theme) {
                let _ = client.framed.queue(&ServerMsg::Theme(Box::new(theme.clone())));
                client.theme_sent = Some(theme.clone());
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
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl SessionHost {
    /// What the status line needs. Window names fall back to the active
    /// pane's title, which is what a shell sets and what a user recognises.
    fn status(&self, clients: u16) -> haven_proto::SessionStatus {
        let session = &self.runtime.session;
        let windows = session
            .windows
            .iter()
            .map(|w| haven_proto::WindowInfo {
                id: w.id.0,
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
                        let (command, running) = pane
                            .and_then(|r| r.command())
                            .map(|(c, running)| (short_command(c), running))
                            .unwrap_or_default();
                        haven_proto::PaneInfo {
                            id: p.0,
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
            .collect();
        haven_proto::SessionStatus {
            session: session.id,
            name: self.name.clone(),
            windows,
            active_window: session.windows.get(session.active).map(|w| w.id.0).unwrap_or(0),
            started_unix: self.started_unix,
            clients,
        }
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
