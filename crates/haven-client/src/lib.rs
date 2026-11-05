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
pub mod dialog;
pub mod frame;
pub mod frontend;
pub mod input;
pub mod menu;
pub mod status_bar;

/// Themes live in the protocol crate: the daemon owns the configuration and
/// hands one to every client, so it is wire data, not a client's own idea.
pub use std::time::Instant;

use haven_proto::theme;

pub use frame::{FrameModel, PaneFrame, Span, Widget, WidgetKind};
pub use frontend::{Frontend, NullFrontend, PresentError, ScriptedFrontend, Zoom};
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
    /// Ask the daemon what shells it has, and run the one picked with this
    /// command — `new-window` for a tab, `split-v` for a pane.
    ChooseShell(String),
    /// A container was picked from the shell menu and has a question left to
    /// answer: does what you install in it survive the pane. Asked here
    /// rather than guessed, and asked per pane because the answer differs
    /// per pane — a scratch Arch to try one command, a kept Ubuntu to build
    /// in all week.
    ChooseLifetime { verb: String, spec: String },
    /// Change which host directories a container pane is given. `add` and
    /// `remove` are both `None` for a request to simply say what they are.
    ContainerMounts { add: Option<String>, remove: Option<String> },
    /// Make the text bigger or smaller. Only a frontend that owns its font
    /// can; the rest say so.
    Zoom(frontend::Zoom),
    /// Ask the daemon for the focused pane's command history.
    OpenHistory,
    /// Ask for every line matching this, in every pane of the session.
    Grep(String),
    /// Go to a grep hit: its window, its pane, its row.
    GoTo { window: u16, pane: u32, row: u32 },
    /// Show a menu the composition root built, since only it knows what the
    /// commands are. Title, then label-and-command pairs.
    OpenMenu {
        title: String,
        entries: Vec<(String, String)>,
        /// A symbol per entry, or empty. Parallel to `entries`.
        icons: Vec<String>,
        filterable: bool,
    },
    /// Swallowed, and worth saying why. A key that does nothing and reports
    /// nothing is indistinguishable from a broken binding.
    Notice(String),
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

/// A font size as a person would write it: `16`, not `16.0`, and `16.5`
/// when it really is.
fn trim_px(px: f32) -> String {
    if (px - px.round()).abs() < 0.05 {
        format!("{}", px.round() as i32)
    } else {
        format!("{px:.1}")
    }
}

/// A command as one line, for a list that has one row per entry.
///
/// A shell history is full of multi-line commands — a `for` loop someone
/// typed over four lines — and a list that let one of them be four rows
/// would misalign everything under it.
fn one_line(text: &str) -> String {
    let flat: String =
        text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ; ");
    flat.chars().take(400).collect()
}

/// The word a help lookup is about. The same rule the daemon applies, so
/// an answer can be matched to the line that asked for it.
pub fn help_subject(command: &str) -> String {
    const TRANSPARENT: &[&str] = &["sudo", "doas", "env", "command", "nohup", "time", "nice"];
    for word in command.split_whitespace() {
        if word.contains('=') || word.starts_with('-') || TRANSPARENT.contains(&word) {
            continue;
        }
        return word.rsplit('/').next().unwrap_or(word).to_string();
    }
    command.trim().to_string()
}

/// A command line as a `send-keys` argument.
///
/// `send-keys` decodes `\n`, `\t`, `\e` and `\xNN`, so a command that
/// contains a backslash has to say so, or `grep '\d'` would be typed as a
/// carriage return and a `d`.
fn escape_for_send(command: &str) -> String {
    command.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t")
}

/// Seconds since the epoch, for the bar's clock.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Above this many entries a menu is narrowed by typing rather than picked
/// from by number. Nine is where digits stop working; the margin is for the
/// ones you can still count down to faster than you can type.
const DIGIT_LIMIT: usize = 12;

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
    /// A list of things to pick, while one is up. Modal like the chooser.
    pub menu: Option<menu::Menu>,
    /// The session's environment, when it has been asked for.
    pub environment: Vec<(String, String)>,
    /// Whether this client does anything with the mouse at all. Off gives
    /// it back to the terminal Haven is running in.
    pub mouse_enabled: bool,
    /// What to do with the shell picked from the menu, held between asking
    /// the daemon for the list and the list arriving.
    pub shell_verb: String,
    /// Where a new font size goes to be remembered.
    ///
    /// A callback rather than a path, for the same reason the key filter is
    /// one: the config file belongs to the composition root above, and a
    /// client that wrote to it would be a second thing that knows where it
    /// lives and what is in it.
    pub font_saver: Option<Box<dyn FnMut(f32)>>,
    /// Host directories handed to container panes opened from here.
    ///
    /// Client-side and deliberately so: a mount can only be decided before
    /// the container starts, so it belongs to the act of opening a pane
    /// rather than to the session. Empty by default — a distro pane sees
    /// nothing of the machine until you say otherwise.
    pub container_mounts: Vec<String>,
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
    /// The second the bar was last drawn for. Redrawing when it moves is
    /// what makes the clock a clock.
    last_clock: u64,
    /// When a resize that has happened on screen should be told to the
    /// daemon, if one is waiting.
    resize_due: Option<Instant>,
    /// How long a size has to hold still first. Short enough not to be felt
    /// while dragging a window edge, long enough to swallow a burst.
    pub resize_debounce: std::time::Duration,
    /// The size the daemon was last told about, so a zoom that ends where
    /// it started costs the session nothing at all.
    last_sent_size: Option<haven_proto::CellSize>,
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
            menu: None,
            buffers: Vec::new(),
            shell_verb: "new-window".into(),
            container_mounts: Vec::new(),
            font_saver: None,
            mouse_enabled: true,
            showing_panes: false,
            environment: Vec::new(),
            bindings: bindings::Bindings::default(),
            dragging: None,
            fatal: None,
            sessions: Vec::new(),
            last_clock: 0,
            resize_due: None,
            resize_debounce: std::time::Duration::from_millis(90),
            last_sent_size: None,
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

    /// Ask what the selected entry is about, if the menu wants a panel.
    ///
    /// Nothing is asked for a subject already shown: moving down and back
    /// up is one keypress each way, and a `man` lookup per keypress is how
    /// a panel becomes a reason not to use the arrows.
    fn ask_help(&mut self) {
        let Some(menu) = &self.menu else { return };
        let Some(subject) = menu.subject().map(help_subject).filter(|s| !s.is_empty()) else {
            return;
        };
        if menu.detail.as_ref().is_some_and(|d| d.subject == subject) {
            return;
        }
        let _ = self.transport.send(ClientMsg::Help { command: subject });
    }

    /// A command with this client's container mounts appended.
    ///
    /// Only a spec can take them: appending to `new-window /bin/bash` would
    /// be nonsense, and the test suite says so.
    fn with_mounts(&self, command: &str) -> String {
        if self.container_mounts.is_empty() || !command.contains("://") {
            return command.to_string();
        }
        let mut out = command.to_string();
        for mount in &self.container_mounts {
            out.push(if out.contains('?') { '&' } else { '?' });
            out.push_str("mount=");
            out.push_str(mount);
        }
        out
    }

    /// The second half of picking a container: does it last.
    ///
    /// A menu rather than two entries per distro in the first one. The list
    /// of places to run a shell is long and worth reading; doubling it to
    /// carry an answer that only applies to half of its entries makes the
    /// common case worse to serve the second question.
    fn ask_lifetime(&mut self, verb: &str, spec: &str) {
        let name = spec.rsplit('/').next().unwrap_or(spec);
        self.menu = Some(menu::Menu::new(
            format!("{name} — keep what you install in it?"),
            vec![
                (
                    "keep        packages and files survive the pane".to_string(),
                    self.with_mounts(&format!("{verb} {spec}?keep")),
                ),
                (
                    "scratch     gone when the pane closes".to_string(),
                    self.with_mounts(&format!("{verb} {spec}?scratch")),
                ),
            ],
        ));
    }

    /// Add or drop a mount, or just say what they are. Returns the line to
    /// show, because a command that changes something invisible and says
    /// nothing is indistinguishable from one that failed.
    fn change_mounts(&mut self, add: Option<String>, remove: Option<String>) -> String {
        if let Some(path) = add {
            if !self.container_mounts.contains(&path) {
                self.container_mounts.push(path);
            }
        }
        if let Some(path) = remove {
            // Matched on either half, so `container-unmount /src` drops
            // `/src:/work` as well: you asked about the directory, and
            // having to retype the destination to undo a mount is a trap.
            self.container_mounts
                .retain(|m| m != &path && m.split(':').next() != Some(path.as_str()));
        }
        if self.container_mounts.is_empty() {
            "container panes see none of this machine".to_string()
        } else {
            format!("container panes get {}", self.container_mounts.join(", "))
        }
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
        // Whatever is selected now is what the preview is about.
        self.ask_preview();
        Ok(Tick::Redraw)
    }

    /// Ask what the selected pane is showing, for the tree's preview.
    ///
    /// Nothing is asked for a pane already on screen beside the list: the
    /// arrows are held down, and a snapshot per keypress of a pane you are
    /// moving past is traffic for a panel nobody read.
    fn ask_preview(&mut self) {
        let Some(chooser) = &self.chooser else { return };
        let Some(pane) = chooser.previewed(&self.sessions) else { return };
        if chooser.preview.as_ref().is_some_and(|(shown, _)| *shown == pane) {
            return;
        }
        let size = crate::dialog::panel_width(self.frame.size);
        if size == 0 {
            return;
        }
        let rows = (self.frame.size.rows as usize).saturating_sub(6).clamp(4, 40);
        let _ = self.transport.send(ClientMsg::Preview {
            pane: PaneId(pane),
            cols: size as u16,
            rows: rows as u16,
        });
    }

    /// One keystroke while a menu is up.
    fn menu_key(&mut self, event: &input::KeyEvent) -> Result<Tick, TransportError> {
        use input::{Key, KeyState, NamedKey};
        if event.state == KeyState::Release {
            return Ok(Tick::Idle);
        }
        let Some(menu) = self.menu.as_mut() else { return Ok(Tick::Idle) };
        let chosen = match &event.key {
            Key::Named(NamedKey::Escape) => {
                self.menu = None;
                return Ok(Tick::Redraw);
            }
            Key::Named(NamedKey::Up) => {
                menu.move_by(-1);
                None
            }
            Key::Named(NamedKey::Down) => {
                menu.move_by(1);
                None
            }
            // vi keys only where typing is not how you narrow the list.
            Key::Char('k') if !menu.filterable => {
                menu.move_by(-1);
                None
            }
            Key::Char('j') if !menu.filterable => {
                menu.move_by(1);
                None
            }
            Key::Char('q') if !menu.filterable => {
                self.menu = None;
                return Ok(Tick::Redraw);
            }
            Key::Named(NamedKey::Enter) => menu.command().map(str::to_string),
            Key::Named(NamedKey::Backspace) if menu.filterable => {
                if menu.filter.pop().is_none() {
                    self.menu = None;
                    return Ok(Tick::Redraw);
                }
                menu.resettle();
                None
            }
            Key::Char(c) if menu.filterable && !event.mods.contains(input::Modifiers::CTRL) => {
                menu.filter.push(*c);
                menu.resettle();
                None
            }
            // A digit picks straight away: the numbers are printed beside the
            // entries, and making them only *move* the selection would be a
            // promise half kept.
            Key::Char(c) if c.is_ascii_digit() => {
                menu.pick_digit(*c).then(|| menu.command().unwrap_or_default().to_string())
            }
            _ => None,
        };
        if let Some(command) = chosen.filter(|c| !c.is_empty()) {
            self.menu = None;
            return self.run_menu_command(&command);
        }
        // The selection may have moved, and the panel beside it is about
        // whatever is selected now.
        self.ask_help();
        Ok(Tick::Redraw)
    }

    /// What a verdict from the key filter means.
    ///
    /// `None` is the key belonging to the pane rather than to us. One method
    /// rather than one per caller, because it was two: picking from a menu
    /// honoured only [`Intercepted::Act`] and dropped every other verdict in
    /// silence, so a menu entry that opens a second menu — a container being
    /// asked whether it lasts — did nothing, and said nothing either.
    fn act_on(&mut self, verdict: Intercepted) -> Result<Option<Tick>, TransportError> {
        Ok(Some(match verdict {
            Intercepted::Consumed => Tick::Redraw,
            Intercepted::Notice(text) => {
                self.frame.notice = Some(text);
                Tick::Redraw
            }
            Intercepted::Act(action) => {
                self.transport.send(ClientMsg::Action(action))?;
                Tick::Idle
            }
            Intercepted::OpenChooser => {
                self.open_chooser()?;
                Tick::Redraw
            }
            Intercepted::NewSession => {
                self.new_session(None)?;
                Tick::Redraw
            }
            Intercepted::EnterCopy => {
                self.enter_copy();
                Tick::Redraw
            }
            Intercepted::ChooseShell(verb) => {
                // Remembered until the listing arrives: the daemon answers
                // with shells, not with what to do with one.
                self.shell_verb = verb;
                self.transport.send(ClientMsg::ListShells)?;
                Tick::Idle
            }
            Intercepted::ChooseLifetime { verb, spec } => {
                self.ask_lifetime(&verb, &spec);
                Tick::Redraw
            }
            Intercepted::ContainerMounts { add, remove } => {
                self.frame.notice = Some(self.change_mounts(add, remove));
                Tick::Redraw
            }
            Intercepted::OpenHistory => {
                match self.focus {
                    Some(pane) => self.transport.send(ClientMsg::History { pane })?,
                    None => self.frame.notice = Some("no pane to take a history from".into()),
                }
                Tick::Idle
            }
            Intercepted::Grep(query) => {
                self.transport.send(ClientMsg::Grep { query })?;
                Tick::Idle
            }
            Intercepted::GoTo { window, pane, row } => {
                // Three steps because a match is in a pane, in a window,
                // at a row, and none of the three implies the others.
                self.transport.send(ClientMsg::Action(Action::SelectWindow(window)))?;
                self.transport.send(ClientMsg::Action(Action::SelectPane(pane)))?;
                self.transport.send(ClientMsg::ScrollTo { pane: PaneId(pane), row })?;
                Tick::Idle
            }
            Intercepted::Zoom(step) => {
                match self.frontend.zoom(step) {
                    Some(px) => {
                        // Remembered, so the next window opens where this
                        // one was left. The client does not write the file
                        // itself — it is not the client's file to know
                        // about — it hands the number to whoever gave it
                        // the saver.
                        if let Some(save) = &mut self.font_saver {
                            save(px);
                        }
                        self.frame.notice = Some(format!("font {}px", trim_px(px)));
                    }
                    // A key that does nothing and says nothing is
                    // indistinguishable from a broken binding.
                    None => {
                        self.frame.notice =
                            Some("the terminal Haven is running in owns the font size".into())
                    }
                }
                Tick::Redraw
            }
            Intercepted::OpenMenu { title, entries, icons, filterable } => {
                let mut menu = menu::Menu::new(title, entries).with_icons(icons);
                if filterable {
                    menu = menu.filterable();
                }
                self.menu = Some(menu);
                Tick::Redraw
            }
            Intercepted::DisplayPanes => {
                self.showing_panes = true;
                Tick::Redraw
            }
            Intercepted::FindWindow(text) => {
                self.find_window(&text)?;
                Tick::Redraw
            }
            Intercepted::Paste => {
                self.paste()?;
                Tick::Idle
            }
            Intercepted::Scroll(delta) => {
                if let Some(pane) = self.focus {
                    self.transport.send(ClientMsg::Scroll { pane, delta })?;
                }
                Tick::Idle
            }
            Intercepted::Detach => {
                self.transport.send(ClientMsg::Detach)?;
                Tick::Finished
            }
            Intercepted::Pass => return Ok(None),
        }))
    }

    /// What a menu entry resolves to. The client cannot parse a command — the
    /// vocabulary lives above it — so the filter is asked, exactly as it is
    /// for a typed line.
    fn run_menu_command(&mut self, command: &str) -> Result<Tick, TransportError> {
        let Some(mut filter) = self.key_filter.take() else { return Ok(Tick::Redraw) };
        // The client's own mode, not a scratch copy of one: a command that
        // opens a prompt has to leave the prompt open, and a mode dropped on
        // the floor took the prompt with it.
        self.mode = status_bar::Mode::command(command.to_string());
        let verdict =
            filter(&input::KeyEvent::named(input::NamedKey::Enter), &mut self.mode, &self.bindings);
        self.key_filter = Some(filter);
        Ok(self.act_on(verdict)?.unwrap_or(Tick::Redraw))
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

    /// How long a frontend may sleep before the bar is out of date.
    ///
    /// `None` when there is no bar to keep up to date, which is what lets an
    /// idle window cost nothing at all.
    pub fn next_wake(&self) -> Option<std::time::Duration> {
        // A resize waiting to be sent is the more urgent of the two: a
        // window that stopped being dragged and was never mentioned to the
        // daemon would keep the old grid until something else happened.
        let resize = self.resize_due.map(|due| due.saturating_duration_since(Instant::now()));
        let clock = self.frame.status.as_ref().map(|_| {
            let subsec = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            // To the next whole second, so the displayed time changes when
            // the clock does rather than a fraction late.
            std::time::Duration::from_nanos(1_000_000_000 - u64::from(subsec))
        });
        match (resize, clock) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Rebuild the status line into the frame's widgets.
    ///
    /// Done every redraw rather than on change, because the clock moves with
    /// no message to prompt it, and the bar is one row of text.
    fn rebuild_status(&mut self, now: u64) {
        self.frame.widgets.retain(|w| {
            !matches!(w.kind, frame::WidgetKind::StatusLine | frame::WidgetKind::Overlay)
        });
        if let Some(bar) = status_bar::build(
            self.frame.size,
            self.frame.status.as_ref(),
            &self.mode,
            now,
            &self.frame.theme,
            &status_bar::BarState {
                scroll: self.frame.scroll(),
                copy: self.copy.as_ref().map(|c| c.label()),
                notice: self.frame.notice.clone(),
            },
        ) {
            self.frame.widgets.push(bar);
        }
        if let Some(box_) = status_bar::suggestions(self.frame.size, &self.mode, &self.frame.theme)
        {
            self.frame.widgets.push(box_);
        }
        if let Some(widget) =
            self.menu.as_ref().and_then(|m| m.widget(self.frame.size, &self.frame.theme))
        {
            self.frame.widgets.push(widget);
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
        self.tick_at(unix_now())
    }

    /// [`Client::tick`], told what time it is.
    ///
    /// The clock is a parameter so that the bar's own clock can be tested
    /// without sleeping, and because the one thing the bar shows that no
    /// message announces is the time.
    pub fn tick_at(&mut self, now: u64) -> Result<Tick, TransportError> {
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

        if self.resize_due.is_some_and(|due| Instant::now() >= due) {
            self.resize_due = None;
            let size = status_bar::reserve(self.frame.size);
            // A burst that ends where it began — zoom in, zoom out — is a
            // resize the session never needs to hear about.
            if self.last_sent_size != Some(size) {
                self.last_sent_size = Some(size);
                let mut caps = self.frontend.caps();
                // The daemon lays panes out in the space it is told about,
                // so the row the status line occupies is never offered.
                caps.size = size;
                self.transport.send(ClientMsg::Resize { caps })?;
            }
        }

        if self.fatal.is_some() {
            return Ok(Tick::Finished);
        }
        // The clock is the one thing on the bar that changes with nothing to
        // announce it, so nothing was redrawing it: the time sat still until
        // a keystroke or some output happened to repaint the row.
        if outcome == Tick::Idle && self.frame.status.is_some() && now != self.last_clock {
            outcome = Tick::Redraw;
        }
        if outcome == Tick::Redraw {
            self.last_clock = now;
            self.rebuild_status(now);
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
                self.frame.size = size;
                // Drawn now, told to the daemon in a moment. A resize costs
                // the session a `SIGWINCH`, and a shell answers one by
                // redrawing its prompt — so a drag across the screen, or a
                // dozen taps of `Ctrl+-`, would leave a dozen prompts and a
                // scrollback reflowed at a dozen different widths. Held
                // until the size stops changing, a burst is one resize.
                self.resize_due = Some(Instant::now() + self.resize_debounce);
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
                // A notice describes the last thing that happened, so the
                // next keystroke is where it stops being true.
                if key.state == input::KeyState::Press {
                    self.frame.notice = None;
                }
                // The chooser and copy mode are modal and own the keyboard
                // while they are up, so they are consulted before the key
                // table rather than after.
                if self.chooser.is_some() {
                    return self.chooser_key(&key);
                }
                if self.menu.is_some() {
                    return self.menu_key(&key);
                }
                if self.copy.is_some() {
                    return self.copy_key(&key);
                }
                // Taken out and put back so the filter can borrow the mode
                // mutably without borrowing the whole client.
                let mut filter = self.key_filter.take();
                let verdict = filter.as_mut().map(|f| f(&key, &mut self.mode, &self.bindings));
                self.key_filter = filter;
                if let Some(verdict) = verdict {
                    if let Some(tick) = self.act_on(verdict)? {
                        return Ok(tick);
                    }
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
            ServerMsg::Saved { sessions, path, problem } => {
                // Said out loud either way: a save that quietly did nothing
                // is the failure people find after the reboot.
                self.frame.notice = Some(match problem {
                    Some(reason) => format!("could not save: {reason}"),
                    None if sessions == 1 => format!("1 session · {path}"),
                    None => format!("{sessions} sessions · {path}"),
                });
                true
            }
            ServerMsg::Shells { shells, containers } => {
                // The listing is what the menu was opened for; until it
                // arrives there is nothing to show.
                let verb = self.shell_verb.clone();
                let where_ = if verb.starts_with("split") { "pane" } else { "tab" };
                let mut entries: Vec<(String, String)> = shells
                    .iter()
                    .map(|path| {
                        let name = path.rsplit('/').next().unwrap_or(path).to_string();
                        (format!("{name:<10} {path}"), self.with_mounts(&format!("{verb} {path}")))
                    })
                    .collect();
                // One menu, not two. Which shell and where it runs are the
                // same decision — "give me a shell" — and splitting them
                // across two keys would mean knowing which kind of shell you
                // wanted before you had seen the list.
                entries.extend(containers.iter().map(|target| {
                    let label = format!("{:<10} {}  {}", "container", target.label, target.detail);
                    let command = if target.choosable {
                        // One question still to ask, so this entry opens a
                        // menu rather than a pane.
                        format!("container-shell {verb} {}", target.spec)
                    } else {
                        self.with_mounts(&format!("{verb} {}", target.spec))
                    };
                    (label, command)
                }));
                let mut menu = menu::Menu::new(format!("run in a new {where_}"), entries);
                // Digits beat typing for four shells, and are useless for
                // forty images — there is no key `23`. So the long list is
                // the one you narrow by typing, and the short one keeps the
                // numbers printed beside it.
                if menu.entries.len() > DIGIT_LIMIT {
                    menu = menu.filterable();
                }
                self.menu = Some(menu);
                true
            }
            ServerMsg::History { entries, .. } => {
                // Newest last is how the daemon sends it and how a shell
                // thinks of it; a menu is read from the top, so the list is
                // turned over here rather than there.
                let mut labels: Vec<(String, String)> = Vec::new();
                let mut subjects: Vec<String> = Vec::new();
                for entry in entries.iter().rev() {
                    let mark = match (entry.from_pane, entry.exit) {
                        (true, Some(0)) => "+",
                        (true, Some(_)) => "!",
                        (true, None) => "*",
                        // From the shell's file: this pane never saw it, so
                        // there is no status to claim.
                        (false, _) => " ",
                    };
                    labels.push((
                        format!("{mark} {}", one_line(&entry.command)),
                        // Typed at the prompt, not run. What a past command
                        // did is not what it would do now — a different
                        // directory, a different branch — and the one that
                        // matters is the one you would not have chosen.
                        format!("send-keys {}", escape_for_send(&entry.command)),
                    ));
                    subjects.push(entry.command.clone());
                }
                let mut menu = menu::Menu::new("history", labels).filterable();
                menu = menu.with_subjects(subjects);
                self.menu = Some(menu);
                self.ask_help();
                true
            }
            ServerMsg::Help { command, text } => {
                if let Some(menu) = &mut self.menu {
                    // Dropped unless it is about the line now selected: a
                    // lookup can come back after the selection moved on.
                    if menu.subject().map(crate::help_subject) == Some(command.clone()) {
                        menu.detail = Some(menu::MenuDetail {
                            subject: command,
                            lines: text.lines().map(str::to_string).collect(),
                        });
                        return true;
                    }
                }
                false
            }
            ServerMsg::Preview { pane, rows } => {
                if let Some(chooser) = &mut self.chooser {
                    chooser.preview = Some((pane.0, rows));
                    return true;
                }
                false
            }
            ServerMsg::Matches { query, hits } => {
                let entries: Vec<(String, String)> = hits
                    .iter()
                    .map(|hit| {
                        (
                            format!("{:>3}:{:<6} {}", hit.window, hit.row, one_line(&hit.text)),
                            format!("goto {} {} {}", hit.window, hit.pane.0, hit.row),
                        )
                    })
                    .collect();
                let title = if entries.is_empty() {
                    format!("grep {query} — nothing")
                } else {
                    format!("grep {query} — {} lines", entries.len())
                };
                self.menu = Some(menu::Menu::new(title, entries).filterable());
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
                let mut folded =
                    bindings::Bindings::default().with(Some(&table.prefix), bindings::ROOT, &[]);
                for (name, entries) in &table.tables {
                    folded = folded.with(None, name, entries);
                }
                self.bindings = folded;
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
                // And the preview of whatever it opened on.
                self.ask_preview();
                open
            }
            ServerMsg::Refused { reason } => {
                self.frame.notice = Some(reason);
                true
            }
        }
    }
}
