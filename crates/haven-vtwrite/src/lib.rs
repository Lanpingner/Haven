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

use haven_core::{CellFlags, Grid, Palette, Style, StyleTable};

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
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            depth: ColorDepth::TrueColor,
            synchronized_output: false,
            underline_styles: true,
            utf8: true,
        }
    }
}

/// One cell as the serializer sees it: resolved text and style, independent of
/// the grid's interning tables so diffing compares *content*.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RenderCell {
    pub text: String,
    pub style: Style,
    pub wide: bool,
    pub spacer: bool,
}

impl RenderCell {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self { text: text.into(), style, wide: false, spacer: false }
    }

    pub fn blank() -> Self {
        Self { text: " ".into(), style: Style::default(), wide: false, spacer: false }
    }

    pub fn is_blank(&self) -> bool {
        self.text == " " && self.style == Style::default()
    }
}

/// A frame's worth of rendered cells.
pub type Snapshot = Vec<Vec<RenderCell>>;

/// Flatten a grid into content-only cells.
pub fn snapshot(grid: &Grid, styles: &StyleTable) -> Snapshot {
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
    pos: Option<(usize, usize)>,
}

impl Serializer {
    pub fn new(caps: Caps) -> Self {
        Self { caps, palette: Palette::default(), prev: None, pen: Style::default(), pos: None }
    }

    /// Forget what we think the terminal shows. The next frame is a full
    /// repaint — used on attach, on resize and after a resync.
    pub fn invalidate(&mut self) {
        self.prev = None;
        self.pos = None;
        self.pen = Style::default();
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

        // Leave the cursor where the application put it.
        self.move_to(cursor.0, cursor.1, &mut out);
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

        // How many rows a given alignment explains, counting only non-blank
        // rows so a mostly-empty screen cannot match trivially.
        let score = |shift: isize| -> usize {
            (0..rows as isize)
                .filter(|&i| {
                    let source = i + shift;
                    (0..rows as isize).contains(&source)
                        && prev[source as usize] == next[i as usize]
                        && prev[source as usize] != blank_row
                })
                .count()
        };

        let baseline = score(0);
        let max_shift = (rows as isize) - 2;
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
        let mut shifted: Snapshot = Vec::with_capacity(rows);
        if shift > 0 {
            // Content moved up: SU.
            out.extend_from_slice(format!("\x1b[{shift}S").as_bytes());
            shifted.extend_from_slice(&prev[shift as usize..]);
            shifted.resize(rows, blank_row);
        } else {
            let down = (-shift) as usize;
            out.extend_from_slice(format!("\x1b[{down}T").as_bytes());
            shifted.extend(std::iter::repeat_n(blank_row, down));
            shifted.extend_from_slice(&prev[..rows - down]);
        }
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
