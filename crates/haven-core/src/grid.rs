//! The cell grid: cursor motion, erasing, scrolling. See `docs/PLAN.md` §6.
//!
//! No scrollback and no reflow yet — those are M2 (`docs/PLAN.md` §16).

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};

use crate::style::StyleId;

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
    pub struct CellFlags: u8 {
        /// Occupies two columns (CJK, most emoji).
        const WIDE        = 1 << 0;
        /// The placeholder column owned by the WIDE cell to its left.
        const WIDE_SPACER = 1 << 1;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Cell {
    /// The base character. Combining marks do not live here; they extend it
    /// via [`Cell::cluster`], which keeps `Cell` at 12 bytes instead of
    /// hanging a `String` off every cell on the screen.
    pub c: char,
    pub style: StyleId,
    pub flags: CellFlags,
    /// Index into the grid's [`Clusters`] table; 0 means "just `c`".
    pub cluster: ClusterId,
}

impl Default for Cell {
    fn default() -> Self {
        Self { c: ' ', style: StyleId::DEFAULT, flags: CellFlags::empty(), cluster: 0 }
    }
}

/// Index into a grid's cluster table. 0 is reserved for "no cluster".
pub type ClusterId = u16;

/// Interned grapheme clusters: emoji with ZWJ, base characters with combining
/// marks, anything that is one *cell* but several `char`s.
#[derive(Clone, Debug)]
pub struct Clusters {
    strings: Vec<String>,
    index: HashMap<String, ClusterId>,
}

impl Default for Clusters {
    fn default() -> Self {
        // Slot 0 is the "none" sentinel and is never read.
        Self { strings: vec![String::new()], index: HashMap::new() }
    }
}

impl Clusters {
    pub fn intern(&mut self, text: &str) -> ClusterId {
        if let Some(&id) = self.index.get(text) {
            return id;
        }
        let Ok(id) = ClusterId::try_from(self.strings.len()) else {
            // 65k distinct clusters on one screen: drop back to the base
            // character rather than panicking in a parser.
            return 0;
        };
        self.strings.push(text.to_string());
        self.index.insert(text.to_string(), id);
        id
    }

    pub fn get(&self, id: ClusterId) -> Option<&str> {
        if id == 0 {
            return None;
        }
        self.strings.get(id as usize).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.strings.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Cell {
    pub fn new(c: char) -> Self {
        Self { c, ..Default::default() }
    }

    pub fn with_style(c: char, style: StyleId) -> Self {
        Self { c, style, ..Default::default() }
    }

    pub fn is_blank(&self) -> bool {
        self.c == ' ' && self.style == StyleId::DEFAULT
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Set when the line continues onto the next row (needed for reflow and
    /// for selection to copy a logical line back as one line).
    pub wrapped: bool,
    /// Bumped on every mutation. Clients track "last rev seen" instead of a
    /// dirty bit, which is what makes multi-client sync and dropped frames
    /// correct by construction (`docs/PLAN.md` §6).
    pub rev: u32,
}

impl Row {
    /// Cells with trailing blanks removed. Reflow must not re-wrap padding, or
    /// a shrink-then-grow cycle loses the shape of the text.
    pub fn trimmed(&self) -> &[Cell] {
        let end = self.cells.iter().rposition(|c| !c.is_blank()).map_or(0, |i| i + 1);
        &self.cells[..end]
    }

    fn new(cols: usize) -> Self {
        Self { cells: vec![Cell::default(); cols], wrapped: false, rev: 1 }
    }

    fn clear(&mut self, style: StyleId) {
        for cell in &mut self.cells {
            *cell = Cell { style, ..Default::default() };
        }
        self.wrapped = false;
        self.rev = self.rev.wrapping_add(1);
    }

    fn touch(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    pub style: StyleId,
    /// Deferred wrap: the cursor sits *on* the last column with a pending
    /// wrap, rather than off the edge. Getting this wrong is why some
    /// terminals drop a character at column 80.
    pub pending_wrap: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EraseMode {
    ToEnd,
    ToStart,
    All,
}

/// Default scrollback budget. Counted in *bytes*, not lines, so a pane full of
/// 4000-column lines cannot quietly eat a gigabyte (`docs/PLAN.md` §6).
pub const DEFAULT_SCROLLBACK_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Grid {
    cols: usize,
    rows: Vec<Row>,
    /// Rows that scrolled off the top, oldest first.
    scrollback: VecDeque<Row>,
    scrollback_bytes: usize,
    max_scrollback_bytes: usize,
    pub cursor: Cursor,
    saved_cursor: Option<Cursor>,
    /// Inclusive 0-based `DECSTBM` region.
    pub scroll_top: usize,
    pub scroll_bot: usize,
    /// DECAWM. Lives here because it changes what a write does.
    pub autowrap: bool,
    clusters: Clusters,
    /// The alternate screen accumulates no scrollback and is never reflowed:
    /// full-screen apps redraw themselves, and reflowing their output corrupts
    /// it (`docs/PLAN.md` §8).
    pub is_alt: bool,
}

fn row_bytes(row: &Row) -> usize {
    row.cells.len() * std::mem::size_of::<Cell>()
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows_n = rows.max(1);
        Self {
            cols,
            rows: (0..rows_n).map(|_| Row::new(cols)).collect(),
            scrollback: VecDeque::new(),
            scrollback_bytes: 0,
            max_scrollback_bytes: DEFAULT_SCROLLBACK_BYTES,
            cursor: Cursor::default(),
            saved_cursor: None,
            scroll_top: 0,
            scroll_bot: rows_n - 1,
            autowrap: true,
            clusters: Clusters::default(),
            is_alt: false,
        }
    }

    /// An alternate screen: same geometry, no scrollback, no reflow.
    pub fn new_alt(cols: usize, rows: usize) -> Self {
        Self { is_alt: true, max_scrollback_bytes: 0, ..Self::new(cols, rows) }
    }

    pub fn with_scrollback_bytes(mut self, max: usize) -> Self {
        self.max_scrollback_bytes = max;
        self
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn scrollback_bytes(&self) -> usize {
        self.scrollback_bytes
    }

    /// Scrollback rows, oldest first.
    pub fn scrollback(&self) -> impl Iterator<Item = &Row> {
        self.scrollback.iter()
    }

    /// Every row, scrollback first: the coordinate space copy mode works in.
    pub fn history(&self) -> impl Iterator<Item = &Row> {
        self.scrollback.iter().chain(self.rows.iter())
    }

    /// One row of the whole history, scrollback included.
    pub fn history_row(&self, index: usize) -> Option<&Row> {
        match self.scrollback.get(index) {
            Some(row) => Some(row),
            None => self.rows.get(index - self.scrollback.len()),
        }
    }

    pub fn history_len(&self) -> usize {
        self.scrollback.len() + self.rows.len()
    }

    /// The text between two points of the history, inclusive of both.
    ///
    /// A row that wrapped is joined to the next without a newline: it was one
    /// logical line that the width broke, and pasting it back with a break in
    /// it is how a copied command turns into two broken ones.
    pub fn text_between(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let (start, end) = if start <= end { (start, end) } else { (end, start) };
        let mut out = String::new();
        for index in start.0..=end.0.min(self.history_len().saturating_sub(1)) {
            let Some(row) = self.history_row(index) else { break };
            let first = if index == start.0 { start.1 } else { 0 };
            let last = if index == end.0 { end.1 } else { row.cells.len().saturating_sub(1) };
            for cell in row.cells.iter().take(last + 1).skip(first) {
                if cell.flags.contains(crate::CellFlags::WIDE_SPACER) {
                    continue;
                }
                out.push_str(&self.cell_text(cell));
            }
            if index < end.0 && !row.wrapped {
                // Trailing blanks on a hard-ended line are padding, not text.
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('\n');
            }
        }
        while out.ends_with(' ') {
            out.pop();
        }
        out
    }

    /// The first row past `from` whose text contains `query`, searching in
    /// the given direction. `from` itself is skipped, so repeating a search
    /// advances instead of finding the same row again. Case-insensitive unless the query has a
    /// capital in it, which is the convention every editor since vi uses.
    pub fn find(&self, query: &str, from: usize, backwards: bool) -> Option<usize> {
        if query.is_empty() {
            return None;
        }
        let fold = !query.chars().any(char::is_uppercase);
        let needle = if fold { query.to_lowercase() } else { query.to_string() };
        let len = self.history_len();
        let matches = |index: usize| -> bool {
            let Some(row) = self.history_row(index) else { return false };
            let text: String = row.cells.iter().map(|c| self.cell_text(c)).collect();
            let text = if fold { text.to_lowercase() } else { text };
            text.contains(&needle)
        };
        if backwards {
            (0..from.min(len)).rev().find(|i| matches(*i))
        } else {
            ((from + 1).min(len)..len).find(|i| matches(*i))
        }
    }

    /// How far back a view may be scrolled: one row per row of history.
    pub fn max_scroll(&self) -> usize {
        self.scrollback.len()
    }

    /// A screenful ending `offset` rows above the live bottom.
    ///
    /// The history is `scrollback` followed by the live rows, and a view is a
    /// window of `num_rows()` onto it. At offset 0 that is exactly the live
    /// rows, which is why nothing else in the daemon needs to care whether a
    /// client is scrolled.
    pub fn view(&self, offset: usize) -> Vec<&Row> {
        let offset = offset.min(self.max_scroll());
        if offset == 0 {
            return self.rows.iter().collect();
        }
        let n = self.num_rows();
        let start = self.scrollback.len() - offset;
        (start..start + n)
            .map(|i| match self.scrollback.get(i) {
                Some(row) => row,
                None => &self.rows[i - self.scrollback.len()],
            })
            .collect()
    }

    fn push_scrollback(&mut self, row: Row) {
        if self.is_alt || self.max_scrollback_bytes == 0 {
            return;
        }
        self.scrollback_bytes += row_bytes(&row);
        self.scrollback.push_back(row);
        while self.scrollback_bytes > self.max_scrollback_bytes {
            match self.scrollback.pop_front() {
                Some(old) => self.scrollback_bytes -= row_bytes(&old),
                None => break,
            }
        }
    }

    pub fn clusters(&self) -> &Clusters {
        &self.clusters
    }

    /// The full text of a cell: its base character plus any combining marks.
    pub fn cell_text(&self, cell: &Cell) -> Cow<'_, str> {
        match self.clusters.get(cell.cluster) {
            Some(text) => Cow::Borrowed(text),
            None => Cow::Owned(cell.c.to_string()),
        }
    }

    /// Attach a zero-width character to the cell the cursor just left.
    ///
    /// Dropping these is what makes accented text and ZWJ emoji render as
    /// their base glyph; attaching them to the *following* cell is what makes
    /// them render in the wrong place.
    pub fn combine(&mut self, c: char) {
        let (mut row, mut col) = (self.cursor.row, self.cursor.col);
        if !self.cursor.pending_wrap {
            if col > 0 {
                col -= 1;
            } else if row > 0 {
                row -= 1;
                col = self.cols - 1;
            } else {
                // Nothing printed yet: a leading combining mark has no base.
                return;
            }
        }
        // Step off a wide glyph's spacer onto the glyph itself.
        if self.rows[row].cells[col].flags.contains(CellFlags::WIDE_SPACER) && col > 0 {
            col -= 1;
        }
        let mut text = self.cell_text(&self.rows[row].cells[col]).into_owned();
        text.push(c);
        let id = self.clusters.intern(&text);
        self.rows[row].cells[col].cluster = id;
        self.rows[row].touch();
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn num_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn row(&self, row: usize) -> &Row {
        &self.rows[row]
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Resize, reflowing wrapped lines on the primary screen.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows_n = rows.max(1);
        if cols != self.cols && !self.is_alt {
            self.reflow(cols, rows_n);
            return;
        }
        self.resize_simple(cols, rows_n);
    }

    fn resize_simple(&mut self, cols: usize, rows_n: usize) {
        if cols != self.cols {
            for row in &mut self.rows {
                row.cells.resize(cols, Cell::default());
                row.touch();
            }
            self.cols = cols;
        }
        while self.rows.len() > rows_n {
            // Rows leaving the top are history, not garbage.
            let row = self.rows.remove(0);
            self.push_scrollback(row);
        }
        while self.rows.len() < rows_n {
            self.rows.push(Row::new(cols));
        }
        self.scroll_top = 0;
        self.scroll_bot = rows_n - 1;
        self.cursor.row = self.cursor.row.min(rows_n - 1);
        self.cursor.col = self.cursor.col.min(cols - 1);
        self.cursor.pending_wrap = false;
    }

    /// Re-wrap every logical line to a new width.
    ///
    /// This is the operation the plan calls a bug swamp, so it is written as
    /// one pure-ish pass with the cursor tracked as a *logical* position:
    /// gather logical lines, re-wrap them, then decide what is viewport and
    /// what is history. Tracking the cursor by cell index rather than by row
    /// is what makes shrink-then-grow return it to the same character.
    fn reflow(&mut self, new_cols: usize, new_rows: usize) {
        let cursor_abs_row = self.scrollback.len() + self.cursor.row;
        // With a deferred wrap pending, the logical cursor is one cell *past*
        // its column — otherwise widening walks it backwards by one.
        let cursor_logical_col = self.cursor.col + usize::from(self.cursor.pending_wrap);

        // 1. Flatten scrollback + viewport into logical lines.
        let mut source: Vec<Row> = Vec::with_capacity(self.scrollback.len() + self.rows.len());
        source.extend(self.scrollback.drain(..));
        source.append(&mut self.rows);
        self.scrollback_bytes = 0;

        // Trailing blank rows below the cursor are padding, not content. Left
        // in, they inflate the row count and push real text into history on
        // every narrowing.
        let last_meaningful = source
            .iter()
            .enumerate()
            .rposition(|(i, row)| i == cursor_abs_row || !row.trimmed().is_empty());
        source.truncate(last_meaningful.map_or(0, |i| i + 1));

        struct Logical {
            cells: Vec<Cell>,
            /// Where the cursor sits inside this line, if it does.
            cursor_at: Option<usize>,
        }
        let mut logical: Vec<Logical> = Vec::new();
        let mut continuing = false;
        for (index, row) in source.iter().enumerate() {
            let is_cursor_row = index == cursor_abs_row;
            // A wrapped row contributes all its cells; a hard-ended row
            // contributes only up to its last non-blank.
            let cells: &[Cell] = if row.wrapped { &row.cells } else { row.trimmed() };

            if continuing {
                let line = logical.last_mut().expect("continuation without a line");
                if is_cursor_row {
                    line.cursor_at = Some(line.cells.len() + cursor_logical_col.min(cells.len()));
                }
                line.cells.extend_from_slice(cells);
            } else {
                logical.push(Logical {
                    cells: cells.to_vec(),
                    cursor_at: is_cursor_row.then(|| cursor_logical_col.min(cells.len())),
                });
            }
            continuing = row.wrapped;
        }

        // 2. Re-wrap into rows of the new width.
        let mut out: Vec<Row> = Vec::new();
        // (row, col, pending_wrap)
        let mut cursor_target: Option<(usize, usize, bool)> = None;
        for line in &logical {
            let start = out.len();
            let chunk_count = line.cells.chunks(new_cols).len().max(1);
            for (i, chunk) in line.cells.chunks(new_cols).enumerate() {
                let mut row = Row::new(new_cols);
                row.cells[..chunk.len()].copy_from_slice(chunk);
                row.wrapped = i + 1 < chunk_count;
                out.push(row);
            }
            if line.cells.is_empty() {
                out.push(Row::new(new_cols));
            }
            if let Some(at) = line.cursor_at {
                // A cursor exactly past the end of a line that fills its last
                // row is a *deferred wrap*, not a position on a row that does
                // not exist yet. Materializing a padding row instead would
                // break the next reflow, because the padding becomes an empty
                // logical line and the cursor loses its offset.
                let past_end_on_boundary = at == line.cells.len() && at > 0 && at % new_cols == 0;
                cursor_target = Some(if past_end_on_boundary {
                    (start + chunk_count - 1, new_cols - 1, true)
                } else {
                    (start + (at / new_cols).min(chunk_count - 1), at % new_cols, false)
                });
            }
        }

        // 3. Split into history and viewport, keeping the cursor visible.
        let keep_from = out.len().saturating_sub(new_rows);
        let keep_from = match cursor_target {
            // Never scroll the cursor off the top of its own screen.
            Some((row, _, _)) if row < keep_from => row.saturating_sub(new_rows - 1),
            _ => keep_from,
        };
        let viewport: Vec<Row> = out.split_off(keep_from);
        for row in out {
            self.push_scrollback(row);
        }
        self.rows = viewport;
        while self.rows.len() < new_rows {
            self.rows.push(Row::new(new_cols));
        }
        self.rows.truncate(new_rows);

        self.cols = new_cols;
        self.scroll_top = 0;
        self.scroll_bot = new_rows - 1;
        self.cursor.row = cursor_target
            .map(|(row, _, _)| row.saturating_sub(keep_from))
            .unwrap_or(self.cursor.row)
            .min(new_rows - 1);
        self.cursor.col =
            cursor_target.map(|(_, col, _)| col).unwrap_or(self.cursor.col).min(new_cols - 1);
        self.cursor.pending_wrap = cursor_target.is_some_and(|(_, _, pending)| pending);
        for row in &mut self.rows {
            row.touch();
        }
    }

    pub fn move_to(&mut self, row: usize, col: usize) {
        self.cursor.row = row.min(self.rows.len() - 1);
        self.cursor.col = col.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    pub fn move_by(&mut self, d_row: isize, d_col: isize) {
        let row = (self.cursor.row as isize + d_row).clamp(0, self.rows.len() as isize - 1);
        let col = (self.cursor.col as isize + d_col).clamp(0, self.cols as isize - 1);
        self.move_to(row as usize, col as usize);
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = Some(self.cursor);
    }

    pub fn restore_cursor(&mut self) {
        if let Some(c) = self.saved_cursor {
            self.cursor = c;
            self.cursor.row = self.cursor.row.min(self.rows.len() - 1);
            self.cursor.col = self.cursor.col.min(self.cols - 1);
        }
    }

    pub fn write(&mut self, c: char, width: usize) {
        // A grid one column wide cannot hold a wide glyph at all. Wrapping
        // it would wrap forever and writing it would write a spacer past the
        // end of the row — which panicked, in the daemon, taking every
        // session with it. One column is not a hypothetical: a 1x1 pty is
        // what a client with no window size gets.
        let width = if width == 2 && self.cols < 2 { 1 } else { width };
        if self.cursor.pending_wrap && self.autowrap {
            self.rows[self.cursor.row].wrapped = true;
            self.cursor.col = 0;
            self.line_feed();
        }
        self.cursor.pending_wrap = false;

        // A wide glyph that does not fit wraps as a unit rather than being split.
        if width == 2 && self.cursor.col + 1 >= self.cols {
            if self.autowrap {
                self.rows[self.cursor.row].wrapped = true;
                self.cursor.col = 0;
                self.line_feed();
            } else {
                return;
            }
        }

        let (row, col, style) = (self.cursor.row, self.cursor.col, self.cursor.style);
        let flags = if width == 2 { CellFlags::WIDE } else { CellFlags::empty() };
        self.rows[row].cells[col] = Cell { c, style, flags, cluster: 0 };
        if width == 2 {
            self.rows[row].cells[col + 1] =
                Cell { c: ' ', style, flags: CellFlags::WIDE_SPACER, cluster: 0 };
        }
        self.rows[row].touch();

        let advance = width.max(1);
        if col + advance >= self.cols {
            self.cursor.col = self.cols - 1;
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col = col + advance;
        }
    }

    pub fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    pub fn backspace(&mut self) {
        if self.cursor.pending_wrap {
            self.cursor.pending_wrap = false;
        } else if self.cursor.col > 0 {
            self.cursor.col -= 1;
        }
    }

    pub fn tab(&mut self, count: usize) {
        // Fixed 8-column stops until HTS/TBC land in M2.
        for _ in 0..count.max(1) {
            let next = ((self.cursor.col / 8) + 1) * 8;
            if next >= self.cols {
                self.cursor.col = self.cols - 1;
                break;
            }
            self.cursor.col = next;
        }
        self.cursor.pending_wrap = false;
    }

    pub fn line_feed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_bot {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows.len() {
            self.cursor.row += 1;
        }
    }

    /// Reverse index.
    pub fn reverse_line_feed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        let style = self.cursor.style;
        // Only content leaving the *top of the screen* becomes scrollback. A
        // DECSTBM region scrolling internally discards its rows, which is what
        // keeps a status-line app from polluting history.
        let feeds_scrollback = self.scroll_top == 0;
        for _ in 0..n {
            let mut row = self.rows.remove(self.scroll_top);
            if feeds_scrollback {
                self.push_scrollback(row.clone());
            }
            row.clear(style);
            self.rows.insert(self.scroll_bot, row);
        }
        self.touch_region();
    }

    pub fn scroll_down(&mut self, n: usize) {
        let style = self.cursor.style;
        for _ in 0..n {
            let mut row = self.rows.remove(self.scroll_bot);
            row.clear(style);
            self.rows.insert(self.scroll_top, row);
        }
        self.touch_region();
    }

    fn touch_region(&mut self) {
        for row in &mut self.rows[self.scroll_top..=self.scroll_bot] {
            row.touch();
        }
    }

    pub fn erase_in_line(&mut self, mode: EraseMode) {
        let (row, col, style) = (self.cursor.row, self.cursor.col, self.cursor.style);
        let range = match mode {
            EraseMode::ToEnd => col..self.cols,
            EraseMode::ToStart => 0..(col + 1).min(self.cols),
            EraseMode::All => 0..self.cols,
        };
        for cell in &mut self.rows[row].cells[range] {
            *cell = Cell { style, ..Default::default() };
        }
        self.rows[row].touch();
    }

    pub fn erase_in_display(&mut self, mode: EraseMode) {
        let style = self.cursor.style;
        let (row, last) = (self.cursor.row, self.rows.len() - 1);
        match mode {
            EraseMode::ToEnd => {
                self.erase_in_line(EraseMode::ToEnd);
                for r in (row + 1)..=last {
                    self.rows[r].clear(style);
                }
            }
            EraseMode::ToStart => {
                self.erase_in_line(EraseMode::ToStart);
                for r in 0..row {
                    self.rows[r].clear(style);
                }
            }
            EraseMode::All => {
                for r in 0..=last {
                    self.rows[r].clear(style);
                }
            }
        }
    }

    /// `ECH`: erase n characters at the cursor without moving it.
    pub fn erase_chars(&mut self, n: usize) {
        let (row, col, style) = (self.cursor.row, self.cursor.col, self.cursor.style);
        let end = (col + n.max(1)).min(self.cols);
        for cell in &mut self.rows[row].cells[col..end] {
            *cell = Cell { style, ..Default::default() };
        }
        self.rows[row].touch();
    }

    /// `ICH`.
    pub fn insert_chars(&mut self, n: usize) {
        let (row, col, style) = (self.cursor.row, self.cursor.col, self.cursor.style);
        let n = n.max(1).min(self.cols - col);
        let cells = &mut self.rows[row].cells;
        for _ in 0..n {
            cells.pop();
            cells.insert(col, Cell { style, ..Default::default() });
        }
        self.rows[row].touch();
    }

    /// `DCH`.
    pub fn delete_chars(&mut self, n: usize) {
        let (row, col, style) = (self.cursor.row, self.cursor.col, self.cursor.style);
        let n = n.max(1).min(self.cols - col);
        let cells = &mut self.rows[row].cells;
        for _ in 0..n {
            cells.remove(col);
            cells.push(Cell { style, ..Default::default() });
        }
        self.rows[row].touch();
    }

    /// `IL` / `DL`: scroll the region below the cursor, not the whole screen.
    pub fn insert_lines(&mut self, n: usize) {
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bot {
            return;
        }
        let saved_top = self.scroll_top;
        self.scroll_top = self.cursor.row;
        self.scroll_down(n.max(1).min(self.scroll_bot - self.cursor.row + 1));
        self.scroll_top = saved_top;
    }

    pub fn delete_lines(&mut self, n: usize) {
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bot {
            return;
        }
        let saved_top = self.scroll_top;
        self.scroll_top = self.cursor.row;
        self.scroll_up(n.max(1).min(self.scroll_bot - self.cursor.row + 1));
        self.scroll_top = saved_top;
    }

    /// Scrollback followed by the viewport, as text. Tests use this to prove
    /// reflow conserves content.
    pub fn dump_all(&self) -> String {
        let mut out = String::new();
        for row in self.scrollback.iter().chain(self.rows.iter()) {
            out.push_str(row_text(row, &self.clusters).trim_end());
            out.push('\n');
        }
        out
    }

    /// Plain-text dump, trailing blanks trimmed. The spine of the golden-screen
    /// tests in `docs/PLAN.md` §17.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for row in &self.rows {
            out.push_str(row_text(row, &self.clusters).trim_end());
            out.push('\n');
        }
        out
    }
}

fn row_text(row: &Row, clusters: &Clusters) -> String {
    let mut out = String::with_capacity(row.cells.len());
    for cell in &row.cells {
        if cell.flags.contains(CellFlags::WIDE_SPACER) {
            continue;
        }
        match clusters.get(cell.cluster) {
            Some(text) => out.push_str(text),
            None => out.push(cell.c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_with(s: &str, cols: usize, rows: usize) -> Grid {
        let mut g = Grid::new(cols, rows);
        for c in s.chars() {
            g.write(c, 1);
        }
        g
    }

    #[test]
    fn a_wide_glyph_in_a_one_column_grid_does_not_take_the_daemon_with_it() {
        // A client that reports no window size gets a 1x1 pty, and the first
        // CJK character or emoji written into it indexed past the end of the
        // row. In the daemon that is every session on the machine, gone.
        let mut g = Grid::new(1, 2);
        g.write('あ', 2);
        g.write('x', 1);
        assert_eq!(g.rows[0].cells[0].c, 'あ', "the glyph is kept, narrow");
        assert!(!g.rows[0].cells[0].flags.contains(CellFlags::WIDE));
    }

    #[test]
    fn deferred_wrap_does_not_drop_the_last_column() {
        let g = grid_with("abcd", 4, 2);
        assert_eq!(g.dump(), "abcd\n\n");
        assert_eq!((g.cursor.row, g.cursor.col, g.cursor.pending_wrap), (0, 3, true));
    }

    #[test]
    fn wrap_marks_the_row_and_advances() {
        let g = grid_with("abcde", 4, 2);
        assert_eq!(g.dump(), "abcd\ne\n");
        assert!(g.row(0).wrapped);
        assert_eq!((g.cursor.row, g.cursor.col), (1, 1));
    }

    #[test]
    fn wide_glyph_claims_a_spacer() {
        let mut g = Grid::new(3, 2);
        g.write('a', 1);
        g.write('漢', 2);
        assert_eq!(g.dump(), "a漢\n\n");
        assert!(g.row(0).cells[1].flags.contains(CellFlags::WIDE));
        assert!(g.row(0).cells[2].flags.contains(CellFlags::WIDE_SPACER));
    }

    #[test]
    fn wide_glyph_wraps_as_a_unit_rather_than_splitting() {
        let mut g = Grid::new(2, 2);
        g.write('a', 1);
        g.write('漢', 2);
        // One column left: the glyph moves to the next row intact instead of
        // being sliced, and the row it left is marked wrapped.
        assert_eq!(g.dump(), "a\n漢\n");
        assert!(g.row(0).wrapped);
        assert!(g.row(1).cells[1].flags.contains(CellFlags::WIDE_SPACER));
    }

    #[test]
    fn scroll_up_recycles_the_top_row() {
        let mut g = Grid::new(4, 2);
        for c in "ab".chars() {
            g.write(c, 1);
        }
        g.line_feed();
        g.carriage_return();
        for c in "cd".chars() {
            g.write(c, 1);
        }
        g.line_feed();
        assert_eq!(g.dump(), "cd\n\n");
    }

    #[test]
    fn erase_modes() {
        let mut g = grid_with("abcdef", 6, 1);
        g.move_to(0, 3);
        g.erase_in_line(EraseMode::ToEnd);
        assert_eq!(g.dump(), "abc\n");

        let mut g = grid_with("abcdef", 6, 1);
        g.move_to(0, 2);
        g.erase_in_line(EraseMode::ToStart);
        assert_eq!(g.dump(), "   def\n");
    }

    #[test]
    fn insert_and_delete_chars_shift_within_the_row() {
        let mut g = grid_with("abcdef", 6, 1);
        g.move_to(0, 2);
        g.delete_chars(2);
        assert_eq!(g.dump(), "abef\n");
        g.move_to(0, 2);
        g.insert_chars(1);
        assert_eq!(g.dump(), "ab ef\n");
    }

    fn write_str(g: &mut Grid, s: &str) {
        for c in s.chars() {
            if c == '\n' {
                g.line_feed();
                g.carriage_return();
            } else {
                g.write(c, 1);
            }
        }
    }

    #[test]
    fn rows_leaving_the_top_become_scrollback() {
        let mut g = Grid::new(6, 2);
        write_str(&mut g, "one\ntwo\nthree");
        assert_eq!(g.dump(), "two\nthree\n");
        assert_eq!(g.scrollback_len(), 1);
        assert_eq!(g.dump_all(), "one\ntwo\nthree\n");
    }

    #[test]
    fn scrollback_is_budgeted_in_bytes_and_evicts_the_oldest() {
        // Room for roughly two rows, so a third must evict the first.
        let budget = 2 * 4 * std::mem::size_of::<Cell>();
        let mut g = Grid::new(4, 1).with_scrollback_bytes(budget);
        write_str(&mut g, "aa\nbb\ncc\ndd");
        assert!(g.scrollback_bytes() <= budget, "budget exceeded");
        let history = g.dump_all();
        assert!(!history.contains("aa"), "oldest row should have been evicted:\n{history}");
        assert!(history.contains("dd"));
    }

    #[test]
    fn the_alternate_screen_accumulates_no_history() {
        let mut g = Grid::new_alt(4, 2);
        write_str(&mut g, "one\ntwo\nthree");
        assert_eq!(g.scrollback_len(), 0, "alt screen must not pollute scrollback");
    }

    #[test]
    fn a_scroll_region_does_not_feed_scrollback() {
        // A status-line app scrolling rows 1-2 of 4 must not write history.
        let mut g = Grid::new(4, 4);
        g.scroll_top = 1;
        g.scroll_bot = 2;
        g.move_to(2, 0);
        write_str(&mut g, "a\nb\nc");
        assert_eq!(g.scrollback_len(), 0);
    }

    // --- reflow ------------------------------------------------------------

    #[test]
    fn narrowing_rewraps_a_long_line_and_widening_restores_it() {
        let mut g = Grid::new(10, 4);
        write_str(&mut g, "abcdefghijklmno");
        assert_eq!(g.dump_all(), "abcdefghij\nklmno\n\n\n");
        assert!(g.row(0).wrapped);

        g.resize(5, 4);
        assert_eq!(g.dump_all().replace('\n', "|"), "abcde|fghij|klmno||");

        g.resize(10, 4);
        // The logical line is intact again, which is the whole point of
        // tracking `wrapped` per row.
        assert!(g.dump_all().starts_with("abcdefghij\nklmno"));
    }

    #[test]
    fn reflow_conserves_content_across_a_width_round_trip() {
        let mut g = Grid::new(20, 6);
        write_str(&mut g, "the quick brown fox jumps over the lazy dog\nsecond line\nthird");
        let before: String = g.dump_all().split_whitespace().collect();

        for width in [7, 13, 4, 31, 20] {
            g.resize(width, 6);
        }
        let after: String = g.dump_all().split_whitespace().collect();
        assert_eq!(before, after, "reflow lost or invented characters");
    }

    #[test]
    fn hard_ended_lines_stay_separate_through_reflow() {
        let mut g = Grid::new(10, 4);
        write_str(&mut g, "ab\ncd");
        g.resize(4, 4);
        let text = g.dump_all();
        assert!(text.starts_with("ab\ncd"), "lines were joined:\n{text}");
    }

    #[test]
    fn the_cursor_follows_its_own_character_through_reflow() {
        let mut g = Grid::new(10, 4);
        write_str(&mut g, "abcdefgh");
        // Cursor sits just after 'h', at column 8 of row 0.
        assert_eq!((g.cursor.row, g.cursor.col), (0, 8));

        g.resize(4, 4);
        // 'abcdefgh' is now two full rows of 4, and the cursor is past the end
        // of the second: that is a deferred wrap, the same state you would be
        // in having just typed 8 characters into a 4-column terminal.
        assert_eq!((g.cursor.row, g.cursor.col, g.cursor.pending_wrap), (1, 3, true));

        g.resize(10, 4);
        // And it round-trips: offset 8 is an ordinary position at width 10.
        assert_eq!((g.cursor.row, g.cursor.col, g.cursor.pending_wrap), (0, 8, false));
    }

    #[test]
    fn reflow_keeps_the_cursor_on_screen_when_history_grows() {
        let mut g = Grid::new(40, 3);
        write_str(&mut g, "0123456789012345678901234567890123456789abc");
        g.resize(5, 3);
        assert!(g.cursor.row < 3, "cursor fell off the viewport: row {}", g.cursor.row);
    }

    #[test]
    fn the_alternate_screen_is_never_reflowed() {
        let mut g = Grid::new_alt(10, 2);
        write_str(&mut g, "abcdefghijkl");
        g.resize(5, 2);
        // Truncated, not re-wrapped: a full-screen app will repaint itself.
        assert_eq!(g.cols(), 5);
        assert_eq!(g.scrollback_len(), 0);
    }

    #[test]
    fn combining_marks_attach_to_the_preceding_cell() {
        let mut g = Grid::new(6, 1);
        g.write('e', 1);
        g.combine('\u{0301}'); // combining acute
        assert_eq!(g.dump(), "e\u{0301}\n");
        // Still one cell wide: the mark did not consume a column.
        assert_eq!(g.cursor.col, 1);
    }

    #[test]
    fn clusters_are_interned_so_repeats_cost_nothing() {
        let mut g = Grid::new(6, 1);
        for _ in 0..3 {
            g.write('e', 1);
            g.combine('\u{0301}');
        }
        assert_eq!(g.clusters().len(), 1, "identical clusters must share one entry");
    }

    #[test]
    fn a_combining_mark_at_the_very_start_has_no_base_and_is_dropped() {
        let mut g = Grid::new(4, 1);
        g.combine('\u{0301}');
        assert_eq!(g.dump(), "\n");
    }

    #[test]
    fn combining_marks_attach_past_a_wide_glyphs_spacer() {
        let mut g = Grid::new(6, 1);
        g.write('漢', 2);
        g.combine('\u{0301}');
        assert_eq!(g.dump(), "漢\u{0301}\n");
    }

    #[test]
    fn clusters_survive_reflow() {
        let mut g = Grid::new(4, 2);
        for c in "abc".chars() {
            g.write(c, 1);
        }
        g.combine('\u{0301}');
        g.resize(2, 2);
        assert!(g.dump_all().contains('\u{0301}'), "cluster lost in reflow: {:?}", g.dump_all());
    }

    #[test]
    fn a_view_at_rest_is_the_live_screen() {
        let mut g = Grid::new(4, 2);
        write_str(&mut g, "a\nb");
        assert_eq!(g.view(0).len(), 2);
        assert_eq!(g.view(0)[0].cells[0].c, 'a');
    }

    #[test]
    fn a_view_reaches_back_into_the_scrollback() {
        let mut g = Grid::new(4, 2);
        // Four lines through a two-row screen: two fall into history.
        write_str(&mut g, "1\n2\n3\n4");
        assert_eq!(g.max_scroll(), 2, "history: {}", g.max_scroll());

        let view = g.view(1);
        assert_eq!(view.len(), 2, "a view is always a screenful");
        assert_eq!(view[0].cells[0].c, '2');
        assert_eq!(view[1].cells[0].c, '3');

        let view = g.view(2);
        assert_eq!(view[0].cells[0].c, '1');
        assert_eq!(view[1].cells[0].c, '2');
    }

    #[test]
    fn a_view_cannot_be_scrolled_past_the_oldest_row() {
        let mut g = Grid::new(4, 2);
        write_str(&mut g, "1\n2\n3\n4");
        assert_eq!(
            g.view(99).iter().map(|r| r.cells[0].c).collect::<String>(),
            g.view(g.max_scroll()).iter().map(|r| r.cells[0].c).collect::<String>()
        );
    }

    #[test]
    fn text_between_two_points_spans_rows() {
        let mut g = Grid::new(10, 3);
        write_str(&mut g, "alpha\nbeta\ngamma");
        assert_eq!(g.text_between((0, 0), (2, 4)), "alpha\nbeta\ngamma");
        // Both ends are included, so columns 1 to 3 of "beta" are "eta".
        assert_eq!(g.text_between((1, 1), (1, 3)), "eta");
        // Given backwards, it still reads forwards.
        assert_eq!(g.text_between((1, 3), (1, 1)), "eta");
    }

    #[test]
    fn a_wrapped_line_is_copied_as_one_line() {
        // The property that makes a copied command paste back as one command
        // instead of two broken ones.
        let mut g = Grid::new(5, 3);
        write_str(&mut g, "abcdefgh");
        assert!(g.row(0).wrapped, "the test needs a wrap to be meaningful");
        assert_eq!(g.text_between((0, 0), (1, 4)), "abcdefgh");
    }

    #[test]
    fn trailing_blanks_are_not_copied() {
        let mut g = Grid::new(10, 2);
        write_str(&mut g, "hi\nthere");
        assert_eq!(g.text_between((0, 0), (1, 9)), "hi\nthere");
    }

    #[test]
    fn search_walks_the_history_both_ways() {
        let mut g = Grid::new(10, 2);
        write_str(&mut g, "needle\nhay\nhay\nneedle\nhay");
        // Two rows on screen, three in history.
        assert_eq!(g.find("needle", 0, false), Some(3));
        assert_eq!(g.find("needle", 4, true), Some(3));
        assert_eq!(g.find("needle", 3, true), Some(0));
        assert_eq!(g.find("nothing", 0, false), None);
    }

    #[test]
    fn search_folds_case_until_the_query_does_not() {
        let mut g = Grid::new(10, 2);
        write_str(&mut g, "hay\nNeedle\nhay");
        assert_eq!(g.find("needle", 2, true), Some(1));
        // A capital in the query means the search is exact.
        assert_eq!(g.find("NEEDLE", 2, true), None);
    }

    #[test]
    fn revision_counters_advance_on_mutation() {
        let mut g = Grid::new(4, 2);
        let before = g.row(0).rev;
        g.write('x', 1);
        assert_ne!(g.row(0).rev, before);
    }
}
