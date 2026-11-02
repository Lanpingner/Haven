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
pub const PROTOCOL_VERSION: u32 = 5;

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

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ClientMsg {
    Attach {
        session: Option<SessionId>,
        caps: ClientCaps,
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
    /// Control-mode queries, which the CLI is built on: anything `haven ls`
    /// can do, a script can do (`docs/PLAN.md` §14.2).
    ListSessions,
    KillSession(SessionId),
    Shutdown,
    Detach,
}

/// Mux actions a client can ask for. The same enum the key table resolves to,
/// so anything a binding can do, `haven -C` and a plugin can do too.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Action {
    SplitColumns,
    SplitRows,
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
    NewWindow,
    NextWindow,
    PreviousWindow,
    SelectWindow(u16),
    /// Focus a pane by id, switching windows if it lives in another one.
    SelectPane(u32),
    EvenHorizontal,
    EvenVertical,
    Tiled,
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
    /// Whatever the program running in it last called itself.
    pub title: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub windows: u16,
    pub panes: u16,
    pub clients: u16,
    pub size: CellSize,
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
