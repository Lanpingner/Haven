//! The frontend-agnostic client: what a frame *is*, what input *is*, and the
//! loop that connects them to a [`Transport`].
//!
//! `docs/PLAN.md` §3: the frontend is a trait, not an `if`. The GPU frontend
//! rasterizes a [`FrameModel`]; the TUI frontend serializes the same model to
//! escape sequences. Neither can reach into mux state, and neither can drive
//! the other's code path.

pub mod frame;
pub mod frontend;
pub mod input;

pub use frame::{FrameModel, PaneFrame, Span, Widget, WidgetKind};
pub use frontend::{Frontend, NullFrontend, PresentError, ScriptedFrontend};
pub use input::{
    InputEvent, Key, KeyEncoder, KeyEvent, KeyState, Modifiers, MouseButton, MouseEvent, NamedKey,
};

use haven_proto::{Action, ClientMsg, PaneId, ServerMsg, Transport, TransportError};

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
}

/// Decides what a keystroke means before it reaches a pane.
pub type KeyFilter = Box<dyn FnMut(&KeyEvent) -> Intercepted>;

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
        Self {
            frontend,
            transport,
            frame: FrameModel::new(size),
            focus: None,
            encoder: KeyEncoder::new(tier),
            clipboard: None,
            key_filter: None,
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
        let caps = self.frontend.caps();
        self.transport.send(ClientMsg::Attach { session, caps })?;
        self.attached = true;
        Ok(())
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

        if outcome == Tick::Redraw {
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
                caps.size = size;
                self.frame.size = size;
                self.transport.send(ClientMsg::Resize { caps })?;
                Ok(Tick::Redraw)
            }
            InputEvent::Key(key) => {
                match self.key_filter.as_mut().map(|f| f(&key)) {
                    Some(Intercepted::Consumed) => return Ok(Tick::Redraw),
                    Some(Intercepted::Act(action)) => {
                        self.transport.send(ClientMsg::Action(action))?;
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
            InputEvent::Mouse(_) | InputEvent::Focus(_) => Ok(Tick::Idle),
        }
    }

    /// Fold one server message into the frame. Returns whether to redraw.
    fn apply(&mut self, msg: ServerMsg) -> bool {
        match msg {
            ServerMsg::Hello { .. } => false,
            ServerMsg::Tables(tables) => {
                self.frame.apply_tables(&tables);
                false
            }
            ServerMsg::Modes { pane, app_cursor_keys, bracketed_paste } => {
                self.frame.set_modes(pane, app_cursor_keys, bracketed_paste);
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
            ServerMsg::Sessions(sessions) => {
                self.sessions = sessions;
                false
            }
            ServerMsg::Refused { reason } => {
                self.frame.notice = Some(reason);
                true
            }
        }
    }
}
