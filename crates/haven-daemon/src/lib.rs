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
    pub fn tick(&mut self, timeout: Duration) -> io::Result<()> {
        self.accept_pending()?;
        self.wait(timeout)?;
        self.read_clients()?;
        self.pump_sessions()?;
        self.apply_resize_policy()?;
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
                let client = &mut self.clients[index];
                client.caps = caps;
                client.session = Some(id);
                client.sync.invalidate();
                let _ = client.framed.queue(&ServerMsg::Hello {
                    protocol: PROTOCOL_VERSION,
                    session: SessionId(id),
                    build: haven_proto::build_stamp(),
                });
            }
            ClientMsg::Input { pane, bytes } => {
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
        let id = self.next_session;
        self.next_session += 1;
        let spawner = (self.make_spawner)();
        let runtime = MuxRuntime::new(SessionId(id), &format!("haven-{id}"), caps.size, spawner)?;
        self.sessions.insert(
            id,
            SessionHost {
                name: format!("haven-{id}"),
                started_unix: unix_now(),
                runtime,
                policy: self.policy,
                applied: Some(caps.size),
            },
        );
        Ok(id)
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
        for client in &mut self.clients {
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

/// A window name has to share one row with everything else, so take the last
/// path component and cap it.
fn short_title(title: &str) -> String {
    let tail = title.rsplit('/').next().unwrap_or(title).trim();
    let tail = if tail.is_empty() { title.trim() } else { tail };
    tail.chars().take(16).collect()
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
                // An explicit name wins; otherwise the active pane's title,
                // which is what the shell sets and what the user recognises.
                // The id alone tells nobody which window is which.
                name: w.title.clone().unwrap_or_else(|| {
                    self.runtime
                        .pane(w.active)
                        .and_then(|p| p.title())
                        .map(short_title)
                        .unwrap_or_else(|| format!("{}", w.id.0))
                }),
                panes: w
                    .root
                    .panes()
                    .into_iter()
                    .map(|p| haven_proto::PaneInfo {
                        id: p.0,
                        title: self
                            .runtime
                            .pane(p)
                            .and_then(|r| r.title())
                            .map(short_title)
                            .unwrap_or_default(),
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

        let modes =
            (pane.emulator.term.modes.app_cursor_keys, pane.emulator.term.modes.bracketed_paste);
        if state.modes != Some(modes) {
            state.modes = Some(modes);
            let _ = client.framed.queue(&ServerMsg::Modes {
                pane: rect.pane,
                app_cursor_keys: modes.0,
                bracketed_paste: modes.1,
            });
        }

        if !state.snapshotted || state.rows.len() != grid.num_rows() {
            state.rows = grid.rows().iter().map(|r| r.rev).collect();
            state.snapshotted = true;
            state.cursor = Some(grid.cursor);
            let _ = client.framed.queue(&ServerMsg::Snapshot {
                pane: rect.pane,
                rows: grid.rows().iter().map(encode_row).collect(),
                cursor: grid.cursor,
                rev: grid.rows().first().map(|r| r.rev).unwrap_or(0),
            });
        } else {
            let changed: Vec<(u16, RowRle)> = grid
                .rows()
                .iter()
                .enumerate()
                .filter(|(i, row)| state.rows.get(*i) != Some(&row.rev))
                .map(|(i, row)| (i as u16, encode_row(row)))
                .collect();
            let cursor_moved = state.cursor != Some(grid.cursor);
            if !changed.is_empty() || cursor_moved {
                for (i, _) in &changed {
                    if let Some(slot) = state.rows.get_mut(*i as usize) {
                        *slot = grid.rows()[*i as usize].rev;
                    }
                }
                state.cursor = Some(grid.cursor);
                let _ = client.framed.queue(&ServerMsg::Delta {
                    pane: rect.pane,
                    rows: changed,
                    cursor: grid.cursor,
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
