//! The frontend-agnostic client: what a frame *is*, what input *is*, and the
//! loop that connects them to a [`Transport`].
//!
//! `docs/PLAN.md` §3: the frontend is a trait, not an `if`. The GPU frontend
//! rasterizes a [`FrameModel`]; the TUI frontend serializes the same model to
//! escape sequences. Neither can reach into mux state, and neither can drive
//! the other's code path.

pub mod bindings;
pub mod chooser;
pub mod copy;
pub mod frame;
pub mod frontend;
pub mod input;
pub mod status_bar;

/// Themes live in the protocol crate: the daemon owns the configuration and
/// hands one to every client, so it is wire data, not a client's own idea.
pub use haven_proto::theme;

pub use frame::{FrameModel, PaneFrame, Span, Widget, WidgetKind};
pub use frontend::{Frontend, NullFrontend, PresentError, ScriptedFrontend};
pub use input::{
    InputEvent, Key, KeyEncoder, KeyEvent, KeyState, Modifiers, MouseButton, MouseEvent, NamedKey,
};

use haven_proto::{Action, ClientMsg, PaneId, ServerMsg, Transport, TransportError};

/// How many paste buffers to keep. tmux keeps fifty; the difference only
/// shows up for someone using `choose-buffer`, which does not exist yet.
const BUFFERS: usize = 20;

/// Rows one notch of the wheel moves. Three is what every other terminal
/// does, and matching it is the whole point.
const WHEEL_ROWS: i32 = 3;

/// What a key filter decided about a keystroke.
///
/// This is the seam the key table plugs into (`docs/PLAN.md` §11.2): the
/// client knows how to *send* things, and the table decides what a key means.
/// Keeping them apart is what lets the same loop serve a GPU window, a TUI
/// client and `haven -C`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Intercepted {
    /// Not ours: encode it and send it to the focused pane.
    Pass,
    /// Swallowed, with nothing to send — a prefix key, for instance.
    Consumed,
    /// Ask the daemon to do this.
    Act(Action),
    /// End the session for this client.
    Detach,
    /// Show the session tree. The client owns it, because only the client
    /// holds both the session listing and the attached session's windows.
    OpenChooser,
    /// Make a session and move to it. The client owns this for the same
    /// reason it owns attaching: the daemon has to be told this client's
    /// size, and only the frontend knows it.
    NewSession,
    /// Move the focused pane's view through its history, in rows.
    Scroll(i32),
    /// Enter copy mode on the focused pane.
    EnterCopy,
    /// Number every pane until the next keystroke.
    DisplayPanes,
    /// Go to the tab whose name contains this.
    FindWindow(String),
    /// Send the newest paste buffer to the focused pane.
    Paste,
}

/// Decides what a keystroke means before it reaches a pane.
///
/// The mode is passed in rather than captured, because it is not private to
/// the filter: the status line has to show whether the prefix is armed and
/// what is being typed, and two copies of that state would drift.
pub type KeyFilter =
    Box<dyn FnMut(&KeyEvent, &mut status_bar::Mode, &bindings::Bindings) -> Intercepted>;

/// What the client decided to do with one iteration, so a frontend's main loop
/// (winit's, or a termios read loop) can act without re-deriving it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tick {
    /// Nothing happened; nothing to present. Idle must stay genuinely idle.
    Idle,
    /// State changed: present the frame.
    Redraw,
    /// The user asked to detach, or the server went away.
    Finished,
}

/// Drives one client session: pulls input from a [`Frontend`], pushes it at a
/// [`Transport`], folds server messages into a [`FrameModel`], presents.
pub struct Client<F, T> {
    pub frontend: F,
    pub transport: T,
    pub frame: FrameModel,
    /// Where keystrokes go. The daemon owns focus; this mirrors it.
    pub focus: Option<PaneId>,
    pub encoder: KeyEncoder,
    /// Last clipboard write a pane asked for, for the frontend to honour.
    pub clipboard: Option<String>,
    /// Consulted for every keystroke before it is sent to a pane.
    pub key_filter: Option<KeyFilter>,
    /// What the client is doing with keystrokes: passing them on, holding the
    /// prefix, or collecting a command.
    pub mode: status_bar::Mode,
    /// The session tree, while it is open.
    pub chooser: Option<chooser::Chooser>,
    /// Copy mode, while it is up. Modal: it owns the keyboard.
    pub copy: Option<copy::CopyMode>,
    /// The session's environment, when it has been asked for.
    pub environment: Vec<(String, String)>,
    /// Whether this client does anything with the mouse at all. Off gives
    /// it back to the terminal Haven is running in.
    pub mouse_enabled: bool,
    /// Pane numbers are on screen until the next keystroke.
    showing_panes: bool,
    /// The key table, as the daemon's config describes it.
    pub bindings: bindings::Bindings,
    /// The rule being dragged, and whether it runs vertically.
    dragging: Option<(PaneId, bool)>,
    /// Copied text, newest first. tmux calls these paste buffers; the name
    /// is worth keeping because the behaviour is the same.
    pub buffers: Vec<String>,
    /// Why the session is ending, when it is ending badly. A frontend reports
    /// this after it has given the terminal back.
    fatal: Option<String>,
    /// Last session listing received, for `haven ls`.
    pub sessions: Vec<haven_proto::SessionInfo>,
    attached: bool,
}

impl<F: std::fmt::Debug, T: std::fmt::Debug> std::fmt::Debug for Client<F, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("frontend", &self.frontend)
            .field("transport", &self.transport)
            .field("focus", &self.focus)
            .field("attached", &self.attached)
            .finish_non_exhaustive()
    }
}

impl<F: Frontend, T: Transport> Client<F, T> {
    pub fn new(frontend: F, transport: T) -> Self {
        let caps = frontend.caps();
        let (size, tier) = (caps.size, caps.key_tier);
        let _ = &size;
        Self {
            frontend,
            transport,
            frame: FrameModel::new(size),
            focus: None,
            encoder: KeyEncoder::new(tier),
            clipboard: None,
            key_filter: None,
            mode: status_bar::Mode::Normal,
            chooser: None,
            copy: None,
            buffers: Vec::new(),
            mouse_enabled: true,
            showing_panes: false,
            environment: Vec::new(),
            bindings: bindings::Bindings::default(),
            dragging: None,
            fatal: None,
            sessions: Vec::new(),
            attached: false,
        }
    }

    /// Send the attach handshake. Separate from `new` so a frontend can be
    /// constructed and inspected before anything is sent.
    pub fn attach(
        &mut self,
        session: Option<haven_proto::SessionId>,
    ) -> Result<(), TransportError> {
        let mut caps = self.frontend.caps();
        self.frame.size = caps.size;
        caps.size = status_bar::reserve(caps.size);
        self.transport.send(ClientMsg::Attach {
            session,
            caps,
            env: haven_proto::client_environment(),
        })?;
        self.attached = true;
        Ok(())
    }

    /// Open the session tree, asking the daemon for a fresh listing: the one
    /// we hold could be minutes old, and a chooser showing a session that has
    /// since died is worse than a moment's delay.
    fn open_chooser(&mut self) -> Result<(), TransportError> {
        self.transport.send(ClientMsg::ListSessions)?;
        self.chooser = Some(chooser::Chooser::new(&self.sessions, self.frame.status.as_ref()));
        Ok(())
    }

    /// Drive the chooser. Returns `Redraw` for anything it handles, which is
    /// everything: a modal overlay that let keys through to the shell behind
    /// it would be a nasty surprise.
    fn chooser_key(&mut self, event: &input::KeyEvent) -> Result<Tick, TransportError> {
        use input::{Key, KeyState, NamedKey};
        if event.state == KeyState::Release {
            return Ok(Tick::Idle);
        }
        let Some(chooser) = self.chooser.as_mut() else { return Ok(Tick::Idle) };

        match &event.key {
            Key::Named(NamedKey::Escape) => self.chooser = None,
            Key::Named(NamedKey::Up) => chooser.move_by(-1),
            Key::Named(NamedKey::Down) => chooser.move_by(1),
            Key::Char('k') if chooser.filter.is_empty() => chooser.move_by(-1),
            Key::Char('j') if chooser.filter.is_empty() => chooser.move_by(1),
            Key::Named(NamedKey::Backspace) => {
                chooser.filter.pop();
                chooser.resettle();
            }
            Key::Named(NamedKey::Enter) => {
                let choice = chooser.choice();
                self.chooser = None;
                if let Some(choice) = choice {
                    self.pick(choice)?;
                }
            }
            Key::Char(c) if !event.mods.contains(input::Modifiers::CTRL) => {
                chooser.filter.push(*c);
                chooser.resettle();
            }
            _ => {}
        }
        Ok(Tick::Redraw)
    }

    /// Enter copy mode on the focused pane.
    fn enter_copy(&mut self) {
        let Some(pane) = self.focus else { return };
        let Some(frame) = self.frame.pane(pane) else { return };
        let (rows, cols) = (frame.rect.rows as usize, frame.rect.cols as usize);
        let (offset, total) = frame.scroll;
        self.copy = Some(copy::CopyMode::new(pane, rows, cols, offset, total));
        self.sync_copy();
    }

    /// One keystroke while copy mode is up.
    fn copy_key(&mut self, event: &input::KeyEvent) -> Result<Tick, TransportError> {
        if event.state == input::KeyState::Release {
            return Ok(Tick::Idle);
        }
        let Some(mut copy) = self.copy.take() else { return Ok(Tick::Idle) };
        // Word motion needs the text of the row the cursor is on, which the
        // mode does not keep: the frame already has it.
        let line = self.copy_line(&copy);
        let action = copy.key(event, &line);
        let pane = copy.pane;
        self.copy = Some(copy);

        match action {
            copy::CopyAction::None => {}
            copy::CopyAction::Leave => {
                self.copy = None;
                // Reading is over, so go back to the live screen.
                self.transport.send(ClientMsg::Scroll { pane, delta: i32::MIN })?;
            }
            copy::CopyAction::Scroll(delta) => {
                self.transport.send(ClientMsg::Scroll { pane, delta })?;
            }
            copy::CopyAction::Copy(start, end) => {
                self.copy = None;
                self.transport.send(ClientMsg::Copy { pane, start, end })?;
                self.transport.send(ClientMsg::Scroll { pane, delta: i32::MIN })?;
            }
            copy::CopyAction::Search { query, from, backwards } => {
                self.frame.notice = None;
                self.transport.send(ClientMsg::Search { pane, query, from, backwards })?;
            }
        }
        self.sync_copy();
        Ok(Tick::Redraw)
    }

    /// Mirror copy mode's cursor and selection into the frame, which is all
    /// a frontend is given.
    fn sync_copy(&mut self) {
        match self.copy.as_ref() {
            Some(copy) => {
                self.frame.copy_cursor = Some((copy.pane, copy.row, copy.col));
                self.frame.selection = copy.selection().map(|(a, b)| (copy.pane, a, b));
            }
            None => {
                self.frame.copy_cursor = None;
                self.frame.selection = None;
            }
        }
    }

    /// The text of the row the copy cursor is on.
    fn copy_line(&self, copy: &copy::CopyMode) -> String {
        let Some(frame) = self.frame.pane(copy.pane) else { return String::new() };
        let Some(row) = frame.rows.get(copy.row) else { return String::new() };
        let mut line = String::new();
        for span in &row.spans {
            while line.chars().count() < span.col as usize {
                line.push(' ');
            }
            line.push_str(&span.text);
        }
        line
    }

    /// Keep copied text: in a buffer, and on the system clipboard if the
    /// frontend has one.
    fn take_copy(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.frontend.set_clipboard(&text);
        self.clipboard = Some(text.clone());
        self.buffers.insert(0, text);
        self.buffers.truncate(BUFFERS);
    }

    /// Send the newest buffer to the focused pane, bracketed if it asked for
    /// that — without the brackets an editor treats a paste as typing and
    /// auto-indents every line of it.
    fn paste(&mut self) -> Result<(), TransportError> {
        let Some(pane) = self.focus else { return Ok(()) };
        let Some(text) = self.buffers.first().cloned() else { return Ok(()) };
        let bracketed = self.frame.pane(pane).is_some_and(|p| p.bracketed_paste);
        let mut bytes = Vec::new();
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(text.as_bytes());
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        self.transport.send(ClientMsg::Input { pane, bytes })
    }

    /// Rebuild the chooser from newer data, keeping the selection where the
    /// user left it rather than snapping back to the active pane.
    fn refresh_chooser(&mut self) {
        let Some(old) = self.chooser.as_ref() else { return };
        let mut next = chooser::Chooser::new(&self.sessions, self.frame.status.as_ref());
        next.filter = old.filter.clone();
        // The chooser is opened before the session listing has arrived, so
        // the first rebuild is the one that finally has rows in it. Keeping
        // the old selection then would pin it to row zero — the session —
        // instead of the pane the user is actually in.
        if !old.rows.is_empty() {
            next.selected = old.selected.min(next.rows.len().saturating_sub(1));
        }
        next.resettle();
        self.chooser = Some(next);
    }

    /// One mouse event.
    ///
    /// A pane that asked for mouse reporting gets it, and everything else is
    /// the multiplexer's: the wheel reads history, a click moves focus, and a
    /// drag on the rule between two panes resizes them. The pane under the
    /// *pointer* decides, not the focused one — reading or clicking in one
    /// pane while typing in another is the ordinary case.
    fn mouse(&mut self, event: input::MouseEvent) -> Result<Tick, TransportError> {
        if !self.mouse_enabled || self.copy.is_some() || self.chooser.is_some() {
            return Ok(Tick::Idle);
        }
        let under = self.pane_at(event.col, event.row);

        if let Some(pane) = under {
            let mode = self.frame.pane(pane).map(|p| p.mouse).unwrap_or_default();
            if mode.tracking != haven_proto::MouseTracking::Off {
                let Some(rect) = self.frame.pane(pane).map(|p| p.rect) else {
                    return Ok(Tick::Idle);
                };
                let local = input::MouseEvent {
                    col: event.col - rect.col,
                    row: event.row - rect.row,
                    ..event
                };
                let bytes = local.encode(mode);
                if bytes.is_empty() {
                    return Ok(Tick::Idle);
                }
                // A click still moves focus: a program that wants the mouse
                // has no way to tell the mux it should be the one typed at.
                if event.pressed && !event.motion && self.focus != Some(pane) {
                    self.transport.send(ClientMsg::Action(Action::SelectPane(pane.0)))?;
                }
                self.transport.send(ClientMsg::Input { pane, bytes })?;
                return Ok(Tick::Idle);
            }
        }

        match event.button {
            MouseButton::WheelUp | MouseButton::WheelDown => {
                let delta =
                    if event.button == MouseButton::WheelUp { WHEEL_ROWS } else { -WHEEL_ROWS };
                let Some(pane) = under.or(self.focus) else { return Ok(Tick::Idle) };
                self.transport.send(ClientMsg::Scroll { pane, delta })?;
                Ok(Tick::Idle)
            }
            MouseButton::Left if event.pressed && !event.motion => {
                match under {
                    Some(pane) if self.focus != Some(pane) => {
                        self.transport.send(ClientMsg::Action(Action::SelectPane(pane.0)))?;
                        Ok(Tick::Idle)
                    }
                    // Not on a pane means on a rule between panes: remember
                    // which one, so the drag that follows knows what to move.
                    None => {
                        self.dragging = self.edge_at(event.col, event.row);
                        Ok(Tick::Idle)
                    }
                    _ => Ok(Tick::Idle),
                }
            }
            MouseButton::Left if !event.pressed => {
                self.dragging = None;
                Ok(Tick::Idle)
            }
            _ if event.motion => self.drag(event),
            _ => Ok(Tick::Idle),
        }
    }

    /// Which pane a rule belongs to, and which way it runs: the pane above or
    /// to the left of it, since that is the one a resize acts on.
    fn edge_at(&self, col: u16, row: u16) -> Option<(PaneId, bool)> {
        for pane in self.frame.panes.values() {
            let r = &pane.rect;
            let right = r.col + r.cols;
            let bottom = r.row + r.rows;
            if col == right && row >= r.row && row < bottom {
                return Some((pane.id, true));
            }
            if row == bottom && col >= r.col && col < r.col + r.cols {
                return Some((pane.id, false));
            }
        }
        None
    }

    /// Continue a drag on a rule. Resizing is a nudge per cell crossed rather
    /// than an absolute position, because the daemon's layout is weighted and
    /// has no notion of "put the edge here".
    fn drag(&mut self, event: input::MouseEvent) -> Result<Tick, TransportError> {
        let Some((pane, vertical)) = self.dragging else { return Ok(Tick::Idle) };
        let Some(rect) = self.frame.pane(pane).map(|p| p.rect) else { return Ok(Tick::Idle) };
        let (edge, at) = if vertical {
            (rect.col + rect.cols, event.col)
        } else {
            (rect.row + rect.rows, event.row)
        };
        if at == edge {
            return Ok(Tick::Idle);
        }
        let grow = at > edge;
        let action = match (vertical, grow) {
            (true, true) => Action::ResizeRight,
            (true, false) => Action::ResizeLeft,
            (false, true) => Action::ResizeDown,
            (false, false) => Action::ResizeUp,
        };
        // The resize applies to the focused pane, so the dragged one has to
        // be focused first or the wrong edge moves.
        if self.focus != Some(pane) {
            self.transport.send(ClientMsg::Action(Action::SelectPane(pane.0)))?;
        }
        self.transport.send(ClientMsg::Action(action))?;
        Ok(Tick::Idle)
    }

    /// Go to the first tab whose name contains `text`, case-insensitively.
    ///
    /// The client searches rather than the daemon because the client already
    /// holds the listing the bar is drawn from; asking would be a round trip
    /// for an answer already in hand.
    #[doc(hidden)]
    pub fn find_window_for_test(&mut self, text: &str) -> Result<(), TransportError> {
        self.find_window(text)
    }

    fn find_window(&mut self, text: &str) -> Result<(), TransportError> {
        let text = text.trim().to_lowercase();
        if text.is_empty() {
            return Ok(());
        }
        let Some(status) = self.frame.status.as_ref() else { return Ok(()) };
        let found = status.windows.iter().position(|w| w.name.to_lowercase().contains(&text));
        match found {
            Some(index) => {
                self.transport.send(ClientMsg::Action(Action::SelectWindow(index as u16)))
            }
            None => {
                self.frame.notice = Some(format!("no tab matching {text:?}"));
                Ok(())
            }
        }
    }

    /// Which pane covers a screen cell, if any.
    fn pane_at(&self, col: u16, row: u16) -> Option<PaneId> {
        self.frame
            .panes
            .values()
            .find(|p| {
                let r = &p.rect;
                col >= r.col && col < r.col + r.cols && row >= r.row && row < r.row + r.rows
            })
            .map(|p| p.id)
    }

    /// Ask the daemon for a new session and move this client to it.
    ///
    /// The panes we hold belong to the session we are leaving, so they go
    /// now rather than lingering under the new one's layout.
    pub fn new_session(&mut self, name: Option<String>) -> Result<(), TransportError> {
        let mut caps = self.frontend.caps();
        caps.size = status_bar::reserve(caps.size);
        self.frame.panes.clear();
        self.frame.status = None;
        self.chooser = None;
        self.transport.send(ClientMsg::NewSession {
            name,
            caps,
            env: haven_proto::client_environment(),
        })
    }

    fn pick(&mut self, choice: chooser::Choice) -> Result<(), TransportError> {
        match choice {
            chooser::Choice::Session(id) => {
                // Attaching elsewhere invalidates every pane we hold, so the
                // frame is cleared rather than left showing the old session.
                let mut caps = self.frontend.caps();
                caps.size = status_bar::reserve(caps.size);
                self.frame.panes.clear();
                self.frame.status = None;
                self.transport.send(ClientMsg::Attach {
                    session: Some(id),
                    caps,
                    env: haven_proto::client_environment(),
                })
            }
            chooser::Choice::Window(index) => {
                self.transport.send(ClientMsg::Action(Action::SelectWindow(index)))
            }
            chooser::Choice::Pane(id) => {
                self.transport.send(ClientMsg::Action(Action::SelectPane(id)))
            }
        }
    }

    /// Rebuild the status line into the frame's widgets.
    ///
    /// Done every redraw rather than on change, because the clock moves with
    /// no message to prompt it, and the bar is one row of text.
    fn rebuild_status(&mut self) {
        self.frame.widgets.retain(|w| {
            !matches!(w.kind, frame::WidgetKind::StatusLine | frame::WidgetKind::Overlay)
        });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(bar) = status_bar::build(
            self.frame.size,
            self.frame.status.as_ref(),
            &self.mode,
            now,
            &self.frame.theme,
            &status_bar::BarState {
                scroll: self.frame.scroll(),
                copy: self.copy.as_ref().map(|c| c.label()),
            },
        ) {
            self.frame.widgets.push(bar);
        }
        if let Some(box_) = status_bar::suggestions(self.frame.size, &self.mode, &self.frame.theme)
        {
            self.frame.widgets.push(box_);
        }
        if self.showing_panes {
            self.frame.widgets.extend(status_bar::pane_numbers(
                self.frame.status.as_ref(),
                self.focus,
                &self.frame.theme,
            ));
        }
        // After the bar, so it draws on top of it.
        if let Some(widget) =
            self.chooser.as_ref().and_then(|c| c.widget(self.frame.size, &self.frame.theme))
        {
            self.frame.widgets.push(widget);
        }
    }

    /// Why the session ended, if it ended badly.
    pub fn fatal(&self) -> Option<&str> {
        self.fatal.as_deref()
    }

    /// Record a transport failure so a frontend can report it rather than
    /// vanishing. A disconnect is ordinary; anything else is not.
    pub fn note_error(&mut self, e: &TransportError) {
        if !matches!(e, TransportError::Disconnected) && self.fatal.is_none() {
            self.fatal = Some(e.to_string());
        }
    }

    pub fn is_attached(&self) -> bool {
        self.attached
    }

    /// One iteration: drain input, drain server messages, present if needed.
    pub fn tick(&mut self) -> Result<Tick, TransportError> {
        let mut outcome = Tick::Idle;

        while let Some(event) = self.frontend.next_input() {
            match self.handle_input(event)? {
                Tick::Finished => return Ok(Tick::Finished),
                Tick::Redraw => outcome = Tick::Redraw,
                Tick::Idle => {}
            }
        }

        loop {
            match self.transport.try_recv() {
                Ok(Some(msg)) => {
                    if self.apply(msg) {
                        outcome = Tick::Redraw;
                    }
                }
                Ok(None) => break,
                // A server that vanished is a normal end of session, not a crash.
                Err(TransportError::Disconnected) => return Ok(Tick::Finished),
                Err(e) => return Err(e),
            }
        }

        if self.fatal.is_some() {
            return Ok(Tick::Finished);
        }
        if outcome == Tick::Redraw {
            self.rebuild_status();
            // A present failure is the frontend's problem to report; it must
            // not take down the session.
            let _ = self.frontend.present(&self.frame);
        }
        Ok(outcome)
    }

    fn handle_input(&mut self, event: input::InputEvent) -> Result<Tick, TransportError> {
        use input::InputEvent;
        match event {
            InputEvent::Quit => {
                self.transport.send(ClientMsg::Detach)?;
                Ok(Tick::Finished)
            }
            InputEvent::Resize(size) => {
                let mut caps = self.frontend.caps();
                self.frame.size = size;
                // The daemon lays panes out in the space it is told about, so
                // the row the status line occupies is simply never offered.
                caps.size = status_bar::reserve(size);
                self.transport.send(ClientMsg::Resize { caps })?;
                Ok(Tick::Redraw)
            }
            // Any key dismisses the numbers, and then does nothing else: an
            // overlay that swallowed the key *and* acted on it would be two
            // things happening for one press. Releases are not keystrokes —
            // the release of the key that *opened* the overlay arrives a
            // millisecond later and would close it again.
            InputEvent::Key(key) if self.showing_panes && key.state == input::KeyState::Press => {
                self.showing_panes = false;
                Ok(Tick::Redraw)
            }
            InputEvent::Key(key) => {
                // The chooser and copy mode are modal and own the keyboard
                // while they are up, so they are consulted before the key
                // table rather than after.
                if self.chooser.is_some() {
                    return self.chooser_key(&key);
                }
                if self.copy.is_some() {
                    return self.copy_key(&key);
                }
                // Taken out and put back so the filter can borrow the mode
                // mutably without borrowing the whole client.
                let mut filter = self.key_filter.take();
                let verdict = filter.as_mut().map(|f| f(&key, &mut self.mode, &self.bindings));
                self.key_filter = filter;
                match verdict {
                    Some(Intercepted::Consumed) => return Ok(Tick::Redraw),
                    Some(Intercepted::Act(action)) => {
                        self.transport.send(ClientMsg::Action(action))?;
                        return Ok(Tick::Idle);
                    }
                    Some(Intercepted::OpenChooser) => {
                        self.open_chooser()?;
                        return Ok(Tick::Redraw);
                    }
                    Some(Intercepted::NewSession) => {
                        self.new_session(None)?;
                        return Ok(Tick::Redraw);
                    }
                    Some(Intercepted::EnterCopy) => {
                        self.enter_copy();
                        return Ok(Tick::Redraw);
                    }
                    Some(Intercepted::DisplayPanes) => {
                        self.showing_panes = true;
                        return Ok(Tick::Redraw);
                    }
                    Some(Intercepted::FindWindow(text)) => {
                        self.find_window(&text)?;
                        return Ok(Tick::Redraw);
                    }
                    Some(Intercepted::Paste) => {
                        self.paste()?;
                        return Ok(Tick::Idle);
                    }
                    Some(Intercepted::Scroll(delta)) => {
                        if let Some(pane) = self.focus {
                            self.transport.send(ClientMsg::Scroll { pane, delta })?;
                        }
                        return Ok(Tick::Idle);
                    }
                    Some(Intercepted::Detach) => {
                        self.transport.send(ClientMsg::Detach)?;
                        return Ok(Tick::Finished);
                    }
                    Some(Intercepted::Pass) | None => {}
                }
                let Some(pane) = self.focus else { return Ok(Tick::Idle) };
                // Encoding happens client-side: only the client knows what its
                // host terminal is willing to send (`docs/PLAN.md` §11.1).
                let bytes = self.encoder.encode(&key);
                if bytes.is_empty() {
                    return Ok(Tick::Idle);
                }
                self.transport.send(ClientMsg::Input { pane, bytes })?;
                Ok(Tick::Idle)
            }
            InputEvent::Paste(text) => {
                let Some(pane) = self.focus else { return Ok(Tick::Idle) };
                self.transport.send(ClientMsg::Input { pane, bytes: text.into_bytes() })?;
                Ok(Tick::Idle)
            }
            InputEvent::Mouse(mouse) => self.mouse(mouse),
            InputEvent::Focus(_) => Ok(Tick::Idle),
        }
    }

    /// Fold one server message into the frame. Returns whether to redraw.
    fn apply(&mut self, msg: ServerMsg) -> bool {
        match msg {
            ServerMsg::Hello { protocol, .. } => {
                // The version is checked here or nowhere. Leaving it unread
                // meant a daemon left running from an older build was only
                // noticed when some later message failed to decode — in
                // whatever code path happened to ask for one first, which
                // looked like that feature crashing rather than like skew.
                if protocol != haven_proto::PROTOCOL_VERSION {
                    self.fatal = Some(format!(
                        "the daemon speaks protocol v{protocol}, this client speaks v{}. \
                         It is running an older build: `haven server stop` will replace it.",
                        haven_proto::PROTOCOL_VERSION
                    ));
                }
                self.fatal.is_some()
            }
            ServerMsg::Tables(tables) => {
                self.frame.apply_tables(&tables);
                false
            }
            ServerMsg::Modes { pane, app_cursor_keys, bracketed_paste, cursor_visible, mouse } => {
                self.frame.set_modes(pane, app_cursor_keys, bracketed_paste, cursor_visible, mouse);
                // Mirror DECCKM into the encoder: only the pane knows it, and
                // without it arrow keys type letters.
                if Some(pane) == self.focus {
                    self.encoder.app_cursor = app_cursor_keys;
                }
                false
            }
            ServerMsg::Clipboard { data, .. } => {
                self.clipboard = Some(data);
                true
            }
            ServerMsg::Snapshot { pane, rows, cursor, rev } => {
                self.frame.apply_snapshot(pane, &rows, cursor, rev);
                true
            }
            ServerMsg::Delta { pane, rows, cursor, rev } => {
                self.frame.apply_delta(pane, &rows, cursor, rev)
            }
            ServerMsg::Layout(layout) => {
                self.focus = layout.active.or(self.focus);
                self.frame.apply_layout(&layout);
                true
            }
            ServerMsg::Title { pane, title } => {
                self.frame.set_title(pane, title);
                true
            }
            ServerMsg::Bell { .. } => true,
            ServerMsg::PaneExited { pane, .. } => {
                self.frame.remove_pane(pane);
                true
            }
            ServerMsg::Scrolled { pane, offset, total } => {
                self.frame.set_scroll(pane, offset, total);
                if let Some(copy) = self.copy.as_mut().filter(|c| c.pane == pane) {
                    copy.scrolled(offset, total);
                    self.sync_copy();
                }
                Some(pane) == self.focus
            }
            ServerMsg::Copied { text, .. } => {
                self.take_copy(text);
                true
            }
            ServerMsg::Found { pane, row } => {
                match row {
                    Some(row) => {
                        let _ = self.transport.send(ClientMsg::ScrollTo { pane, row });
                    }
                    // Saying nothing would look like a key that did nothing.
                    None => self.frame.notice = Some("not found".into()),
                }
                true
            }
            ServerMsg::Environment(env) => {
                self.environment = env;
                false
            }
            ServerMsg::Keys(table) => {
                if self.mouse_enabled != table.mouse {
                    self.mouse_enabled = table.mouse;
                    self.frontend.set_mouse(table.mouse);
                }
                self.bindings = bindings::Bindings::default()
                    .with(Some(&table.prefix), &table.prefix_table, false)
                    .with(None, &table.root, true);
                false
            }
            ServerMsg::Theme(theme) => {
                self.frame.theme = *theme;
                true
            }
            ServerMsg::Status(status) => {
                self.frame.status = Some(status);
                self.refresh_chooser();
                true
            }
            ServerMsg::Sessions { sessions, .. } => {
                self.sessions = sessions;
                // The listing is what the chooser was waiting for.
                let open = self.chooser.is_some();
                self.refresh_chooser();
                open
            }
            ServerMsg::Refused { reason } => {
                self.frame.notice = Some(reason);
                true
            }
        }
    }
}
