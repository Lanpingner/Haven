//! Wire types, framing and transports. See `docs/PLAN.md` §14.
//!
//! The daemon is frontend-blind: it ships cells, layout and widget trees, and
//! learns about a client only through [`ClientCaps`]. Standalone mode is not a
//! second architecture, it is a different [`Transport`] (§4.1).

use haven_core::{Cell, Cursor, Style};
use serde::{Deserialize, Serialize};

pub mod theme;
pub mod transport;

pub use transport::{LocalTransport, Transport, TransportError};

/// Bumped on any incompatible change. A mismatch is refused with a readable
/// message rather than surfacing as a deserialization panic.
pub const PROTOCOL_VERSION: u32 = 19;

/// When this executable was built, as seconds since the epoch, or `0` if it
/// cannot be told.
///
/// The daemon outlives every client and holds the terminal emulator, so
/// rebuilding after a parser fix changes nothing until the daemon is
/// restarted — the old one keeps running the old code and the bug looks
/// unfixed. A client that is newer than the daemon it attached to can at
/// least say so.
pub fn build_stamp() -> u64 {
    std::fs::metadata("/proc/self/exe")
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct PaneId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WindowId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionId(pub u32);

/// Cell *and* pixel geometry. Pixels matter: image protocols read them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CellSize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl Default for CellSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24, pixel_width: 0, pixel_height: 0 }
    }
}

/// How much of a key event survived the journey to us (`docs/PLAN.md` §11.1).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum KeyTier {
    /// Control codes and xterm function keys only.
    Legacy = 1,
    /// `modifyOtherKeys=2` or partial CSI-u.
    Extended = 2,
    /// Full kitty keyboard protocol, or a native GPU window.
    Full = 3,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ColorDepth {
    Ansi16,
    Indexed256,
    TrueColor,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FrontendKind {
    Gpu,
    Tui,
    /// `haven -C`: a script, not a human.
    Control,
    /// Tests.
    Null,
}

/// Everything the daemon is allowed to know about a client. It tailors
/// *content* to these (how many colors to resolve), never behaviour.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ClientCaps {
    pub frontend: FrontendKind,
    pub size: CellSize,
    pub color_depth: ColorDepth,
    pub key_tier: KeyTier,
    pub synchronized_output: bool,
    pub clipboard_osc52: bool,
    pub hyperlinks: bool,
}

impl ClientCaps {
    pub fn null(size: CellSize) -> Self {
        Self {
            frontend: FrontendKind::Null,
            size,
            color_depth: ColorDepth::TrueColor,
            key_tier: KeyTier::Full,
            synchronized_output: false,
            clipboard_osc52: false,
            hyperlinks: false,
        }
    }
}

/// A run of cells sharing one style, as sent over the wire. Run-length
/// encoding is not an optimization here, it is what keeps a full-screen
/// redraw from costing a frame's worth of allocation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RowRle {
    pub runs: Vec<(u16, Cell)>,
}

impl RowRle {
    pub fn encode(cells: &[Cell]) -> Self {
        let mut runs: Vec<(u16, Cell)> = Vec::new();
        for &cell in cells {
            match runs.last_mut() {
                Some((count, prev)) if *prev == cell && *count < u16::MAX => *count += 1,
                _ => runs.push((1, cell)),
            }
        }
        Self { runs }
    }

    pub fn decode(&self, out: &mut Vec<Cell>) {
        out.clear();
        for &(count, cell) in &self.runs {
            out.extend(std::iter::repeat_n(cell, count as usize));
        }
    }

    pub fn len(&self) -> usize {
        self.runs.iter().map(|(n, _)| *n as usize).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

/// Variables a client refreshes when it attaches.
///
/// The same list tmux updates by default, and for the same reason: these are
/// the ones that name a resource belonging to the *connection*, not to the
/// session, so a reattach from somewhere else leaves them pointing at
/// something that is gone.
pub const UPDATE_ENVIRONMENT: &[&str] = &[
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "KRB5CCNAME",
    "SSH_ASKPASS",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "SSH_CONNECTION",
    "WINDOWID",
    "XAUTHORITY",
    "TERM_PROGRAM",
];

/// Read those of [`UPDATE_ENVIRONMENT`] this process actually has.
pub fn client_environment() -> Vec<(String, String)> {
    UPDATE_ENVIRONMENT
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| ((*name).to_string(), value)))
        .collect()
}

/// A key table, as the daemon's config describes it.
///
/// Specs and commands stay strings on the wire. The protocol has no business
/// knowing what a key or a command *is*: the client parses the spec, the
/// composition root parses the command, and the vocabulary stays the one a
/// person types on the command line.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct KeyTable {
    /// Whether the client handles the mouse at all. It rides with the key
    /// table because it is the same kind of thing — what the client does
    /// with input — and sending it alone would be a message for one bool.
    pub mouse: bool,
    pub prefix: String,
    /// Named tables. `prefix` is consulted while the prefix is armed, `root`
    /// with nothing armed, and any other name is a table of its own that
    /// `switch-table` can arm.
    pub tables: Vec<(String, Vec<(String, String)>)>,
}

/// What a pane has asked to hear about the mouse: DEC modes 1000/1002/1003
/// for how much, and 1006 for how it is written.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct MouseMode {
    pub tracking: MouseTracking,
    pub sgr: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum MouseTracking {
    #[default]
    Off,
    /// 1000: presses and releases.
    Click,
    /// 1002: and motion while a button is held.
    ButtonEvent,
    /// 1003: and all motion.
    AnyEvent,
}

/// A cell in a pane's whole history: scrollback first, then the live rows.
///
/// Absolute rather than relative to a view, so a selection survives the
/// scrolling that making it requires. Rows trimmed from the front of the
/// scrollback do shift these, which is a budget away and a copy-mode session
/// long — worth knowing, not worth a generation counter.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
pub struct HistoryPoint {
    pub row: u32,
    pub col: u16,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ClientMsg {
    Attach {
        session: Option<SessionId>,
        caps: ClientCaps,
        /// Variables worth refreshing from the attaching client: the agent
        /// socket, the display, the ssh connection. A session outlives the
        /// connection it was made from, so panes opened later would
        /// otherwise inherit a dead `SSH_AUTH_SOCK` for as long as the
        /// session lives.
        env: Vec<(String, String)>,
    },
    /// Make a session and attach to it. Distinct from `Attach { session:
    /// None }`, which joins the first session that exists and only creates
    /// one when there are none — so without this there was no way to have a
    /// second session on one daemon at all.
    NewSession {
        /// `None` lets the daemon name it, as it does the first one.
        name: Option<String>,
        caps: ClientCaps,
        env: Vec<(String, String)>,
    },
    /// Already-resolved input. Key *encoding* happens client-side, because only
    /// the client knows what its host terminal told it.
    Input {
        pane: PaneId,
        bytes: Vec<u8>,
    },
    Resize {
        caps: ClientCaps,
    },
    /// A resolved mux action, from the client's key table.
    Action(Action),
    /// Move this client's view of a pane through its scrollback. Positive is
    /// backwards, into history. Per client, not per session: two people on
    /// one session read at their own pace.
    Scroll {
        pane: PaneId,
        delta: i32,
    },
    /// Put this client's view at an absolute row of the pane's history,
    /// which is how a search result is reached.
    ScrollTo {
        pane: PaneId,
        row: u32,
    },
    /// Text between two points of a pane's history, for copy mode.
    ///
    /// Done on the daemon because only it holds the whole history, and
    /// because joining wrapped rows correctly needs the `wrapped` flag that
    /// never reaches a client.
    Copy {
        pane: PaneId,
        start: HistoryPoint,
        end: HistoryPoint,
    },
    /// Find `query` in a pane's history, starting from `from` and moving in
    /// the given direction. Case-insensitive unless the query has a capital
    /// in it, as every editor written since vi has done.
    Search {
        pane: PaneId,
        query: String,
        from: u32,
        backwards: bool,
    },
    /// What this pane has run, and what the shell's own history file holds.
    /// Answered by the daemon because both live where the shell does.
    History {
        pane: PaneId,
    },
    /// What a command is, for the panel beside the history. Looked up with
    /// `whatis` and `man` — read-only, on the machine the command is on.
    Help {
        command: String,
    },
    /// A look at what a pane is showing, for the session tree's preview.
    /// Any pane in any session, since the point is to see where you are
    /// not.
    Preview {
        pane: PaneId,
        cols: u16,
        rows: u16,
    },
    /// Every line matching `query`, in every pane of this session. The
    /// search message next door finds the *next* match in one pane; this
    /// one is the list you pick from.
    Grep {
        query: String,
    },
    /// Control-mode queries, which the CLI is built on: anything `haven ls`
    /// can do, a script can do (`docs/PLAN.md` §14.2).
    ListSessions,
    /// What shells the *daemon's* machine has, and what containers it could
    /// put one in. Asked for rather than guessed at by the client, because
    /// the pane is spawned where the daemon is: a client's idea of where
    /// `fish` lives, or of which images are pulled, means nothing once a
    /// session is on another host.
    ListShells,
    /// Write every session's shape to the save file, or read it back.
    ///
    /// Both are the daemon's: it is the only thing that outlives a client,
    /// and the only thing that knows where each pane's shell actually is.
    SaveSessions,
    RestoreSessions,
    /// The session's current environment, for a shell that was started
    /// before the latest attach and cannot be told any other way.
    ShowEnvironment,
    KillSession(SessionId),
    Shutdown,
    Detach,
}

/// Mux actions a client can ask for. The same enum the key table resolves to,
/// so anything a binding can do, `haven -C` and a plugin can do too.
/// Which way a pane sits from another.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Edge {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Action {
    /// `program` runs instead of the session's usual shell. The path is the
    /// daemon's to interpret: a session may be on another machine, where the
    /// client's idea of where `fish` lives means nothing.
    SplitColumns {
        program: Option<String>,
    },
    SplitRows {
        program: Option<String>,
    },
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    ClosePane,
    ToggleZoom,
    NewWindow {
        program: Option<String>,
    },
    NextWindow,
    PreviousWindow,
    SelectWindow(u16),
    /// Focus a pane by id, switching windows if it lives in another one.
    SelectPane(u32),
    EvenHorizontal,
    EvenVertical,
    Tiled,
    /// The window, or the pane, focus was on before this one.
    LastWindow,
    LastPane,
    /// Exchange the active pane with its neighbour, or with the next pane in
    /// the tree when no direction is given.
    SwapPane(Option<Edge>),
    RotateWindow {
        backwards: bool,
    },
    /// Move the active pane into a window of its own.
    BreakPane,
    /// Move another window's only pane into this one.
    JoinPane {
        window: u32,
        columns: bool,
    },
    /// Close the active window and everything in it.
    CloseWindow,
    /// Type into a pane, or into the active one. Separate from `Input`
    /// because it comes from a script rather than from a keyboard: the bytes
    /// are already decided and must not be re-encoded.
    SendKeys {
        pane: Option<u32>,
        bytes: Vec<u8>,
    },
    /// Send everything typed in one pane to every pane of its window.
    /// `None` toggles: only the daemon knows the current state.
    SynchronizePanes {
        on: Option<bool>,
    },
    /// Detach every *other* client from this session, for when a phone or a
    /// dead ssh session is still holding it to a size nothing wants.
    DetachOthers,
    /// Start a fresh program in the active pane.
    RespawnPane {
        program: Option<String>,
    },
    /// The next named layout, cycling.
    NextLayout,
    /// Write every session to the save file, or build back what is in it.
    SaveSessions,
    RestoreSessions,
    /// Rename the active window, or the session when `window` is false. An
    /// empty name gives a window back to the command it is running.
    Rename {
        window: bool,
        name: String,
    },
}

/// Appended entries for a pane's interning tables.
///
/// Both tables are append-only, so a client that knows the first `*_from`
/// entries only needs what came after. That is what lets a cell stay a
/// `StyleId` on the wire without the client having to guess what the id means.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Tables {
    pub pane: PaneId,
    /// Index of the first style in `styles`.
    pub styles_from: u16,
    pub styles: Vec<Style>,
    /// Index of the first cluster in `clusters`.
    pub clusters_from: u16,
    pub clusters: Vec<String>,
}

impl Tables {
    pub fn is_empty(&self) -> bool {
        self.styles.is_empty() && self.clusters.is_empty()
    }
}

/// One command somebody ran.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub command: String,
    /// How it ended, where that is known — which is only for commands this
    /// pane watched run. A history file records no statuses.
    pub exit: Option<i32>,
    /// Whether this pane ran it, as against the shell's history file. The
    /// two are merged into one list and the difference is worth showing:
    /// one of them happened here.
    pub from_pane: bool,
}

/// A line matching a grep, and where it is.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GrepHit {
    pub pane: PaneId,
    /// The window the pane is in, so a client can go there.
    pub window: u32,
    /// The row in that pane's history, as `ScrollTo` counts them.
    pub row: u32,
    pub text: String,
}

/// Somewhere a pane's shell could run that is not this machine's filesystem.
///
/// Described rather than named, because a client must be able to draw the
/// menu without knowing what a container is: it has a label, a second
/// column, and a `spec` string it hands back untouched.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ContainerTarget {
    /// The pane program that opens it, e.g. `docker://run/alpine:latest`.
    pub spec: String,
    pub label: String,
    /// The second column: a size, a status, or that it is not here yet.
    pub detail: String,
    /// Already on the daemon's machine. One that is not has to be fetched
    /// first, which the menu says out loud rather than leaving to be
    /// discovered by a pane that sits still for two minutes.
    pub present: bool,
    /// Whether opening it poses the lasting-or-throwaway question.
    pub choosable: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ServerMsg {
    Hello {
        protocol: u32,
        session: SessionId,
        /// The daemon's [`build_stamp`], so a client can notice it is talking
        /// to a binary older than its own.
        build: u64,
    },
    /// Sent before any snapshot or delta that references new entries.
    Tables(Tables),
    /// Terminal modes a client needs in order to encode input correctly —
    /// `DECCKM` above all, without which arrow keys type letters.
    Modes {
        pane: PaneId,
        app_cursor_keys: bool,
        bracketed_paste: bool,
        mouse: MouseMode,
        /// DECTCEM, and false besides while this client's view is scrolled
        /// away from the cursor: a block parked at the bottom of a page of
        /// history is not where anything is about to be typed.
        cursor_visible: bool,
    },
    /// The text a [`ClientMsg::Copy`] asked for.
    Copied {
        pane: PaneId,
        text: String,
    },
    /// Where a [`ClientMsg::Search`] found the query, if anywhere.
    Found {
        pane: PaneId,
        row: Option<u32>,
    },
    /// What a save or a restore did, for the client to show.
    Saved {
        sessions: u16,
        path: String,
        problem: Option<String>,
    },
    /// Answer to [`ClientMsg::ListShells`], most useful first.
    ///
    /// One message with both halves, because they are one menu: picking
    /// where a shell runs is the same decision as picking which shell, and
    /// two listings arriving separately would mean a menu that grows under
    /// the cursor after it has been opened.
    Shells {
        shells: Vec<String>,
        containers: Vec<ContainerTarget>,
    },
    /// Answer to [`ClientMsg::ShowEnvironment`].
    Environment(Vec<(String, String)>),
    /// Answer to [`ClientMsg::History`], oldest last.
    History {
        pane: PaneId,
        entries: Vec<HistoryEntry>,
    },
    /// Answer to [`ClientMsg::Help`]. Empty text means nothing was found,
    /// which is common and not an error: a shell function has no man page.
    Help {
        command: String,
        text: String,
    },
    /// Answer to [`ClientMsg::Preview`]. Empty when the pane has gone.
    Preview {
        pane: PaneId,
        rows: Vec<PreviewRow>,
    },
    /// Answer to [`ClientMsg::Grep`], in session order.
    Matches {
        query: String,
        hits: Vec<GrepHit>,
    },
    /// The key table. Sent on attach and whenever the config changes, for
    /// the same reason the theme is.
    Keys(Box<KeyTable>),
    /// Where this client's view of a pane sits in its history. Sent whenever
    /// it moves, including back to the bottom, so the bar can say so.
    Scrolled {
        pane: PaneId,
        /// Rows above the live bottom. Zero is the live screen.
        offset: u32,
        /// Rows of history there are to scroll through.
        total: u32,
    },
    /// A pane asked for the clipboard via `OSC 52`; the client decides.
    Clipboard {
        pane: PaneId,
        data: String,
    },
    /// Full state for a pane, sent on attach and on resync.
    Snapshot {
        pane: PaneId,
        rows: Vec<RowRle>,
        cursor: Cursor,
        rev: u32,
    },
    /// Only the rows whose revision moved.
    Delta {
        pane: PaneId,
        rows: Vec<(u16, RowRle)>,
        cursor: Cursor,
        rev: u32,
    },
    Layout(Layout),
    Title {
        pane: PaneId,
        title: String,
    },
    Bell {
        pane: PaneId,
    },
    PaneExited {
        pane: PaneId,
        status: i32,
    },
    /// What the interface is painted with. Sent on attach and again whenever
    /// the daemon's configuration changes, so editing a theme reaches every
    /// attached client without reattaching.
    Theme(Box<theme::Theme>),
    /// Session, window and pane facts for the status line.
    Status(SessionStatus),
    /// Reply to [`ClientMsg::ListSessions`].
    Sessions {
        sessions: Vec<SessionInfo>,
        /// The daemon's [`build_stamp`]: `haven ls` is where someone looks
        /// when a rebuild appears to have changed nothing.
        daemon_build: u64,
    },
    /// Version skew, in words rather than as a decode failure.
    Refused {
        reason: String,
    },
}

/// What a status line needs, which is more than a pane's contents.
///
/// Sent whenever the layout changes rather than on a timer: the one part that
/// moves on its own is the clock, and a client can read that itself from
/// `started_unix` without the daemon waking up to tell it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionStatus {
    pub session: SessionId,
    pub name: String,
    pub windows: Vec<WindowInfo>,
    pub active_window: u32,
    /// When the session was created, in seconds since the epoch.
    pub started_unix: u64,
    pub clients: u16,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u32,
    /// Everything typed goes to every pane in it. On the status bar because
    /// an invisible mode that multiplies every keystroke is a trap.
    pub synchronized: bool,
    /// A pane in this window rang while you were somewhere else.
    pub bell: bool,
    /// A pane in this window produced output while you were elsewhere.
    pub activity: bool,
    pub name: String,
    /// The panes themselves, not a count: a chooser has to list them, and a
    /// count cannot be navigated.
    pub panes: Vec<PaneInfo>,
    pub active_pane: u32,
    pub zoomed: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: u32,
    /// Where this pane is on screen, so a client can label it without
    /// solving the layout itself.
    pub rect: PaneRect,
    /// Whatever the program running in it last called itself.
    pub title: String,
    /// What the shell last reported running, from `OSC 133`. Empty when the
    /// shell has no integration to report it with.
    pub command: String,
    /// Whether that command is still going.
    pub running: bool,
    /// The user this pane is running as, when it is not the one Haven is.
    ///
    /// `Some("root")` is the case worth having: a pane you ran `sudo -i`
    /// in an hour ago looks exactly like one you did not, and the shell's
    /// own prompt is the only thing saying otherwise — in whichever window
    /// you are not looking at.
    pub user: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub windows: u16,
    pub panes: u16,
    pub clients: u16,
    pub size: CellSize,
    /// What is actually in the session, not just how much of it.
    ///
    /// A chooser has to show the tabs of a session you are *not* attached
    /// to — that is the question it exists to answer — and counts cannot be
    /// navigated. Sent with the listing, which is asked for when the tree
    /// opens rather than on every tick.
    pub tabs: Vec<WindowInfo>,
}

/// A row of a pane preview, as runs of identical cells.
///
/// Styles are *resolved* rather than interned, unlike the rows of an
/// attached pane: a preview may be of a session whose style table this
/// client has never been sent, and shipping a second table to colour a
/// thumbnail would be a great deal of machinery for one panel.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct PreviewRow {
    pub runs: Vec<(u16, String, Style)>,
}

/// Pane rectangles in cells, as computed by the daemon's layout solver.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Layout {
    pub window: u32,
    pub panes: Vec<PaneRect>,
    pub active: Option<PaneId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct PaneRect {
    pub pane: PaneId,
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
}

/// Length-prefixed postcard framing, for the socket transport.
pub mod frame {
    use super::*;

    pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, postcard::Error> {
        let body = postcard::to_allocvec(msg)?;
        let mut out = Vec::with_capacity(body.len() + 4);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decode one frame, returning it and how many bytes were consumed.
    /// `None` means "not enough bytes yet", never an error.
    pub fn decode<T: for<'a> Deserialize<'a>>(
        buf: &[u8],
    ) -> Result<Option<(T, usize)>, postcard::Error> {
        if buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if buf.len() < 4 + len {
            return Ok(None);
        }
        let msg = postcard::from_bytes(&buf[4..4 + len])?;
        Ok(Some((msg, 4 + len)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_core::Cell;

    fn cell(c: char) -> Cell {
        Cell::new(c)
    }

    #[test]
    fn rle_round_trips_and_actually_compresses() {
        let cells: Vec<Cell> = "aaabbc".chars().map(cell).collect();
        let rle = RowRle::encode(&cells);
        assert_eq!(rle.runs.len(), 3, "three runs for aaabbc");
        assert_eq!(rle.len(), 6);
        let mut out = Vec::new();
        rle.decode(&mut out);
        assert_eq!(out, cells);
    }

    #[test]
    fn a_blank_row_costs_one_run() {
        let blank = vec![Cell::default(); 200];
        assert_eq!(RowRle::encode(&blank).runs.len(), 1);
    }

    #[test]
    fn framing_round_trips_and_waits_for_partial_frames() {
        let msg = ServerMsg::Hello { protocol: PROTOCOL_VERSION, session: SessionId(7), build: 42 };
        let bytes = frame::encode(&msg).unwrap();

        // A truncated frame is "not yet", not an error: that distinction is
        // what keeps a socket reader from dropping a connection on a split read.
        for cut in 0..bytes.len() {
            let partial: Option<(ServerMsg, usize)> = frame::decode(&bytes[..cut]).unwrap();
            assert!(partial.is_none(), "decoded a truncated frame at {cut}");
        }
        let (decoded, used): (ServerMsg, usize) = frame::decode(&bytes).unwrap().unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(used, bytes.len());
    }

    #[test]
    fn two_frames_in_one_buffer_decode_in_order() {
        let a = ClientMsg::Detach;
        let b = ClientMsg::Input { pane: PaneId(1), bytes: b"hi".to_vec() };
        let mut buf = frame::encode(&a).unwrap();
        buf.extend(frame::encode(&b).unwrap());

        let (first, used): (ClientMsg, usize) = frame::decode(&buf).unwrap().unwrap();
        let (second, _): (ClientMsg, usize) = frame::decode(&buf[used..]).unwrap().unwrap();
        assert_eq!(first, a);
        assert_eq!(second, b);
    }
}
