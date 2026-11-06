//! Cells to escape sequences: the inverse of the parser. See `docs/PLAN.md` §13.1.
//!
//! Three users, one implementation: the TUI frontend, `capture-pane`/session
//! recording, and the round-trip test that validates the parser, this
//! serializer and the cell model against each other.
//!
//! The emission rules that matter are about bytes and flicker, not
//! correctness: a row that changed in twenty scattered cells is usually
//! cheaper to rewrite wholesale than to patch, and that decision is made per
//! row by [`Serializer`].

use haven_core::{CellFlags, Grid, Links, Palette, Style, StyleTable};

mod style;

pub use style::{sgr_transition, ColorDepth};

/// What the destination terminal can actually do. Everything degrades.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Caps {
    pub depth: ColorDepth,
    /// `DECSET 2026`. Removes nearly all tearing in TUI mode.
    pub synchronized_output: bool,
    /// `SGR 4:n` and `SGR 58`.
    pub underline_styles: bool,
    /// When false, box drawing is re-encoded through DEC Special Graphics.
    pub utf8: bool,
    /// `OSC 8`. When the destination understands hyperlinks, ours are passed
    /// through to it and it is the one that makes them clickable — which is
    /// the right answer in a terminal we are a guest in: the host already
    /// knows how this desktop opens a URL, and we would be guessing.
    pub hyperlinks: bool,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            depth: ColorDepth::TrueColor,
            synchronized_output: false,
            underline_styles: true,
            utf8: true,
            hyperlinks: false,
        }
    }
}

// The serializer works on the shared composited-screen type, so what the TUI
// frontend draws and what the GPU frontend draws are the same data.
pub use haven_core::{Screen as Snapshot, ScreenCell as RenderCell};

/// Flatten a grid into content-only cells.
pub fn snapshot(grid: &Grid, styles: &StyleTable) -> Snapshot {
    snapshot_linked(grid, styles, None)
}

/// As [`snapshot`], resolving `OSC 8` targets through a link table.
///
/// Separate because the table lives on the terminal rather than on the grid,
/// and the callers that have only a grid — the round-trip test, a capture of
/// a scrollback region — should not have to invent one.
pub fn snapshot_linked(grid: &Grid, styles: &StyleTable, links: Option<&Links>) -> Snapshot {
    grid.rows()
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| RenderCell {
                    text: grid.cell_text(cell).into_owned(),
                    style: styles.get(cell.style),
                    wide: cell.flags.contains(CellFlags::WIDE),
                    spacer: cell.flags.contains(CellFlags::WIDE_SPACER),
                    link: links.and_then(|table| table.get(cell.link)).map(std::sync::Arc::from),
                })
                .collect()
        })
        .collect()
}

#[derive(Debug)]
pub struct Serializer {
    pub caps: Caps,
    pub palette: Palette,
    prev: Option<Snapshot>,
    /// The pen and cursor position we believe the destination is in.
    pen: Style,
    /// The hyperlink we believe the destination has open, so one is opened
    /// and closed per run rather than per cell.
    link: Option<std::sync::Arc<str>>,
    pos: Option<(usize, usize)>,
    /// Whether the destination currently shows a cursor, so DECTCEM is only
    /// written when it changes rather than on every frame.
    cursor_shown: bool,
}

impl Serializer {
    pub fn new(caps: Caps) -> Self {
        Self {
            caps,
            palette: Palette::default(),
            prev: None,
            pen: Style::default(),
            link: None,
            pos: None,
            cursor_shown: true,
        }
    }

    /// Forget what we think the terminal shows. The next frame is a full
    /// repaint — used on attach, on resize and after a resync.
    pub fn invalidate(&mut self) {
        self.prev = None;
        self.pos = None;
        self.pen = Style::default();
        self.link = None;
    }

    /// Serialize the difference between the last frame and this grid.
    pub fn frame(&mut self, grid: &Grid, styles: &StyleTable) -> Vec<u8> {
        let next = snapshot(grid, styles);
        self.frame_snapshot(next, (grid.cursor.row, grid.cursor.col))
    }

    /// Serialize an already-composited screen. This is the entry point the TUI
    /// frontend uses, because what it draws is several panes plus widgets, not
    /// one grid.
    pub fn frame_snapshot(&mut self, next: Snapshot, cursor: (usize, usize)) -> Vec<u8> {
        self.frame_snapshot_with_cursor(next, cursor, true)
    }

    /// As [`Self::frame_snapshot`], but a pane may ask for no cursor at all —
    /// DECTCEM off, or a view scrolled away from where typing would land.
    pub fn frame_snapshot_with_cursor(
        &mut self,
        next: Snapshot,
        cursor: (usize, usize),
        show_cursor: bool,
    ) -> Vec<u8> {
        let mut out = Vec::new();

        let dims_changed = self.prev.as_ref().is_none_or(|p| {
            p.len() != next.len() || p.first().map(Vec::len) != next.first().map(Vec::len)
        });

        if self.caps.synchronized_output {
            out.extend_from_slice(b"\x1b[?2026h");
        }

        if dims_changed {
            self.full_repaint(&next, &mut out);
        } else {
            let prev = self.prev.take().expect("dims_changed covers None");
            let prev = self.emit_scroll(&prev, &next, &mut out);
            for (index, (old, new)) in prev.iter().zip(next.iter()).enumerate() {
                if old != new {
                    self.emit_row(index, old, new, &mut out);
                }
            }
        }

        // Never end a frame inside a link. The next thing written at that
        // position is the cursor, and anything the host terminal draws after
        // an unclosed `OSC 8` belongs to the link as far as it is concerned.
        if self.link.is_some() {
            out.extend_from_slice(open_link(None).as_bytes());
            self.link = None;
        }

        // Leave the cursor where the application put it.
        self.move_to(cursor.0, cursor.1, &mut out);
        if show_cursor != self.cursor_shown {
            self.cursor_shown = show_cursor;
            out.extend_from_slice(if show_cursor { b"\x1b[?25h" } else { b"\x1b[?25l" });
        }
        if self.caps.synchronized_output {
            out.extend_from_slice(b"\x1b[?2026l");
        }
        self.prev = Some(next);
        out
    }

    fn full_repaint(&mut self, next: &Snapshot, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[H\x1b[2J");
        self.pos = Some((0, 0));
        self.pen = Style::default();
        for (index, row) in next.iter().enumerate() {
            let blank = vec![RenderCell::blank(); row.len()];
            if row != &blank {
                self.emit_row(index, &blank, row, out);
            }
        }
    }

    /// If the new frame is mostly the old frame shifted, say so instead of
    /// repainting. The difference between smooth and horrible when tailing a
    /// log over a slow link.
    ///
    /// A *strict* shift comparison almost never matches in practice: the
    /// application writes the new bottom line and only then scrolls, so one
    /// row of the overlap legitimately differs. So the overlap is scored, and
    /// a shift is accepted when it explains far more rows than no shift does;
    /// whatever it fails to explain is picked up by the ordinary row diff.
    fn emit_scroll(&mut self, prev: &Snapshot, next: &Snapshot, out: &mut Vec<u8>) -> Snapshot {
        let rows = prev.len();
        if rows < 4 {
            return prev.clone();
        }
        let width = prev[0].len();
        let blank_row = vec![RenderCell::blank(); width];

        // The band of rows that changed at all. Whatever scrolled did so
        // inside it, and confining the scroll to that band is the whole
        // point: this screen is usually several panes, and `SU` without a
        // region moves every one of them — along with the status bar, and
        // along with the top line into the *host terminal's* scrollback.
        // That looked exactly like what it was: pressing Enter in the lower
        // pane of a split scrolled the entire window.
        let Some(top) = (0..rows).find(|&i| prev[i] != next[i]) else {
            return prev.clone();
        };
        let bottom = (0..rows).rev().find(|&i| prev[i] != next[i]).unwrap_or(top);
        let band = bottom - top + 1;
        if band < 4 {
            return prev.clone();
        }

        // How many rows of the band a given alignment explains, counting
        // only non-blank rows so a mostly-empty pane cannot match trivially.
        let score = |shift: isize| -> usize {
            (top..=bottom)
                .filter(|&i| {
                    let source = i as isize + shift;
                    (top as isize..=bottom as isize).contains(&source)
                        && prev[source as usize] == next[i]
                        && prev[source as usize] != blank_row
                })
                .count()
        };

        let baseline = score(0);
        let max_shift = (band as isize) - 2;
        let mut best: Option<(isize, usize)> = None;
        for shift in (1..=max_shift).chain(-max_shift..=-1) {
            let s = score(shift);
            // Needs real evidence: at least two rows, and clearly better than
            // staying put.
            if s >= 2 && s > baseline + 1 && best.is_none_or(|(_, bs)| s > bs) {
                best = Some((shift, s));
            }
        }

        let Some((shift, _)) = best else { return prev.clone() };

        // A band that is the whole screen needs no region, and saying so
        // keeps the common unsplit case to the three bytes it was.
        let whole = top == 0 && bottom == rows - 1;
        if !whole {
            out.extend_from_slice(format!("\x1b[{};{}r", top + 1, bottom + 1).as_bytes());
        }
        let mut shifted = prev.clone();
        if shift > 0 {
            out.extend_from_slice(format!("\x1b[{shift}S").as_bytes());
            let up = shift as usize;
            for i in top..=bottom {
                shifted[i] =
                    if i + up <= bottom { prev[i + up].clone() } else { blank_row.clone() };
            }
        } else {
            let down = (-shift) as usize;
            out.extend_from_slice(format!("\x1b[{down}T").as_bytes());
            for i in (top..=bottom).rev() {
                shifted[i] =
                    if i >= top + down { prev[i - down].clone() } else { blank_row.clone() };
            }
        }
        if !whole {
            // Back to the whole screen, or everything written afterwards is
            // confined to one pane's rows.
            out.extend_from_slice(b"\x1b[r");
        }
        // DECSTBM homes the cursor, and so does the scroll on some hosts.
        self.pos = None;
        shifted
    }

    /// Emit one row, choosing the cheaper of patching and rewriting.
    fn emit_row(&mut self, row: usize, old: &[RenderCell], new: &[RenderCell], out: &mut Vec<u8>) {
        let Some(first) = (0..new.len()).find(|&i| old.get(i) != new.get(i)) else { return };
        let last = (0..new.len()).rev().find(|&i| old.get(i) != new.get(i)).unwrap_or(first);

        // Rewriting from the first change to the row's last non-blank, with an
        // EL to clear the tail, beats patching when the changes are scattered.
        let content_end = new.iter().rposition(|c| !c.is_blank()).map_or(0, |i| i + 1);
        let patch_cost = self.estimate(new, first, last + 1);
        let rewrite_cost = self.estimate(new, first, content_end.max(first)) + 4;

        if rewrite_cost < patch_cost {
            self.move_to(row, first, out);
            out.extend_from_slice(b"\x1b[K");
            self.write_cells(new, first, content_end.max(first), out);
        } else {
            self.move_to(row, first, out);
            self.write_cells(new, first, last + 1, out);
        }
    }

    /// Rough byte cost of writing a span, used only to compare two options.
    fn estimate(&self, cells: &[RenderCell], from: usize, to: usize) -> usize {
        let mut cost = 8; // the CUP
        let mut pen = self.pen;
        for cell in &cells[from..to.min(cells.len())] {
            if cell.spacer {
                continue;
            }
            if cell.style != pen {
                cost += 12;
                pen = cell.style;
            }
            cost += cell.text.len();
        }
        cost
    }

    fn write_cells(&mut self, cells: &[RenderCell], from: usize, to: usize, out: &mut Vec<u8>) {
        for cell in &cells[from..to.min(cells.len())] {
            // A wide glyph's spacer column is not written: the glyph already
            // advanced the cursor across it.
            if cell.spacer {
                continue;
            }
            if self.caps.hyperlinks && cell.link != self.link {
                out.extend_from_slice(open_link(cell.link.as_deref()).as_bytes());
                self.link = cell.link.clone();
            }
            if cell.style != self.pen {
                let sgr = sgr_transition(
                    &self.pen,
                    &cell.style,
                    self.caps.depth,
                    &self.palette,
                    self.caps.underline_styles,
                );
                out.extend_from_slice(sgr.as_bytes());
                self.pen = cell.style;
            }
            out.extend_from_slice(cell.text.as_bytes());
        }
        // Track where the cursor ended up, counting wide glyphs as two columns.
        if let Some((row, _)) = self.pos {
            let advance: usize = cells[from..to.min(cells.len())]
                .iter()
                .filter(|c| !c.spacer)
                .map(|c| if c.wide { 2 } else { 1 })
                .sum();
            self.pos = Some((row, from + advance));
        }
    }

    fn move_to(&mut self, row: usize, col: usize, out: &mut Vec<u8>) {
        if self.pos == Some((row, col)) {
            return;
        }
        match self.pos {
            // Same row, a short hop forward: CUF is shorter than CUP.
            Some((r, c)) if r == row && col > c && col - c <= 3 => {
                out.extend_from_slice(format!("\x1b[{}C", col - c).as_bytes());
            }
            Some((r, _)) if r == row && col == 0 => out.push(b'\r'),
            _ => {
                out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
            }
        }
        self.pos = Some((row, col));
    }
}

/// One-shot full render of a grid, for `capture-pane` and tests.
pub fn render_full(grid: &Grid, styles: &StyleTable, caps: Caps) -> Vec<u8> {
    let mut s = Serializer::new(caps);
    s.frame(grid, styles)
}

/// Re-encode a cell for a terminal that cannot do UTF-8, mapping box drawing
/// back through DEC Special Graphics (`docs/PLAN.md` §13.3).
pub fn dec_graphics_byte(c: char) -> Option<u8> {
    Some(match c {
        '◆' => b'`',
        '▒' => b'a',
        '°' => b'f',
        '±' => b'g',
        '┘' => b'j',
        '┐' => b'k',
        '┌' => b'l',
        '└' => b'm',
        '┼' => b'n',
        '─' => b'q',
        '├' => b't',
        '┤' => b'u',
        '┴' => b'v',
        '┬' => b'w',
        '│' => b'x',
        '≤' => b'y',
        '≥' => b'z',
        'π' => b'{',
        '≠' => b'|',
        '£' => b'}',
        '·' => b'~',
        _ => return None,
    })
}

/// `OSC 8` open, or close when there is no target.
///
/// The `id=` parameter is left empty: it exists to tell a terminal that two
/// separate runs are one link, which matters for a URL broken across lines
/// and which the pane that wrote it has already decided. Passing through
/// what we were given would be better still — see the note in the parser
/// about why we do not keep it yet.
///
/// Terminated with `ST` rather than `BEL`: both are allowed, and a stray
/// `BEL` in a terminal that does not understand the sequence rings a bell.
fn open_link(target: Option<&str>) -> String {
    format!("\x1b]8;;{}\x1b\\", target.unwrap_or(""))
}
