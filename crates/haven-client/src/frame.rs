//! The renderer-agnostic frame: cell spans, pane rects, cursor, widgets.
//!
//! Spans carry a resolved [`Style`] rather than a `StyleId`. The interner is a
//! storage optimization inside the grid; a frame is a presentation snapshot,
//! and making every frontend carry a style table just to draw text would leak
//! the grid's representation into both of them.

use std::collections::BTreeMap;
use std::sync::Arc;

use haven_core::{blank_screen, Cell, CellFlags, Cursor, Palette, Screen, ScreenCell, Style};
use haven_proto::{CellSize, Layout, PaneId, PaneRect, RowRle, Tables};

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Span {
    pub col: u16,
    pub text: String,
    pub style: Style,
    /// What this run points at, from `OSC 8`.
    ///
    /// Shared rather than copied: a URL is long, a run of a hundred cells
    /// is one link, and every frame rebuilds these.
    pub link: Option<Arc<str>>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FrameRow {
    pub spans: Vec<Span>,
    /// Revision this row was built from, so a frontend can skip untouched rows.
    pub rev: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PaneFrame {
    pub id: PaneId,
    pub rect: PaneRect,
    pub rows: Vec<FrameRow>,
    pub cursor: Cursor,
    pub title: Option<String>,
    pub rev: u32,
    /// The pane's interning tables, mirrored from the daemon. Append-only, so
    /// they only ever grow and an index never changes meaning.
    pub styles: Vec<Style>,
    pub clusters: Vec<String>,
    /// `OSC 8` targets, in the same append-only way.
    pub links: Vec<Arc<str>>,
    /// Modes that change how this client must encode input.
    pub app_cursor_keys: bool,
    pub bracketed_paste: bool,
    /// What this pane has asked to hear about the mouse.
    pub mouse: haven_proto::MouseMode,
    /// Drawn only when the pane says so: DECTCEM, and a view scrolled away
    /// from the cursor.
    pub cursor_visible: bool,
    /// Where this client is reading: rows above the live bottom, and how
    /// many there are to read.
    pub scroll: (u32, u32),
}

impl PaneFrame {
    fn new(id: PaneId, rect: PaneRect) -> Self {
        Self {
            id,
            rect,
            styles: vec![Style::default()],
            cursor_visible: true,
            ..Default::default()
        }
    }

    fn style(&self, cell: &Cell) -> Style {
        self.styles.get(cell.style.0 as usize).copied().unwrap_or_default()
    }

    fn link(&self, cell: &Cell) -> Option<Arc<str>> {
        match cell.link {
            0 => None,
            id => self.links.get(id as usize - 1).cloned(),
        }
    }

    fn text(&self, cell: &Cell) -> String {
        match cell.cluster {
            0 => cell.c.to_string(),
            id => self.clusters.get(id as usize - 1).cloned().unwrap_or_else(|| cell.c.to_string()),
        }
    }

    /// Rebuild rows from the cells last received, which is needed when the
    /// tables arrive after the content that referenced them.
    fn rebuild(&mut self, cells: &[Vec<Cell>]) {
        self.rows = cells.iter().map(|row| self.spans_for(row)).collect();
    }

    fn spans_for(&self, cells: &[Cell]) -> FrameRow {
        let mut spans: Vec<Span> = Vec::new();
        for (i, cell) in cells.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            let style = self.style(cell);
            let text = self.text(cell);
            let link = self.link(cell);
            match spans.last_mut() {
                // A run breaks where the link changes as well as where the
                // style does: two adjacent words pointing at different URLs
                // are two runs however alike they look.
                Some(span) if span.style == style && span.link == link => span.text.push_str(&text),
                _ => spans.push(Span { col: i as u16, text, style, link }),
            }
        }
        FrameRow { spans, rev: self.rev }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WidgetKind {
    StatusLine,
    TabBar,
    Overlay,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Widget {
    pub kind: WidgetKind,
    pub rect: PaneRect,
    pub rows: Vec<FrameRow>,
}

/// A selected run of cells: the pane it is in, and the first and last cell
/// in that pane's view coordinates.
pub type Selection = (PaneId, (usize, usize), (usize, usize));

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameModel {
    pub size: CellSize,
    /// Ordered so a frontend's iteration order is stable frame to frame.
    pub panes: BTreeMap<u32, PaneFrame>,
    pub widgets: Vec<Widget>,
    pub palette: Palette,
    /// Which pane has focus, mirrored from the layout. A frontend needs it to
    /// know whose cursor to draw.
    pub focus: Option<PaneId>,
    /// Session facts for the status line, as the daemon last reported them.
    pub status: Option<haven_proto::SessionStatus>,
    /// Out-of-band message to show the user (version skew, server gone).
    pub notice: Option<String>,
    /// Where copy mode's own cursor is, as (pane, row, col) in that pane's
    /// view. It replaces the pane's cursor while copy mode is up: the pane's
    /// is where typing would land, which is not where you are reading.
    pub copy_cursor: Option<(PaneId, usize, usize)>,
    /// The copy-mode selection: which pane, and the start and end cells in
    /// that pane's view, both inclusive. Painted by `composite`, so it looks
    /// the same in a window and in a terminal.
    pub selection: Option<Selection>,
    /// What the interface is painted with, as the daemon last sent it. It
    /// lives on the frame because compositing draws with it and a frontend
    /// is handed nothing else.
    pub theme: haven_proto::theme::Theme,
    /// Bumped whenever anything changed, for frontends that cache aggressively.
    pub generation: u64,
}

impl FrameModel {
    pub fn new(size: CellSize) -> Self {
        Self {
            size,
            panes: BTreeMap::new(),
            widgets: Vec::new(),
            palette: Palette::default(),
            status: None,
            focus: None,
            notice: None,
            copy_cursor: None,
            selection: None,
            theme: haven_proto::theme::Theme::default(),
            generation: 0,
        }
    }

    pub fn pane(&self, id: PaneId) -> Option<&PaneFrame> {
        self.panes.get(&id.0)
    }

    /// Append new interning entries for a pane.
    pub fn apply_tables(&mut self, tables: &Tables) {
        let entry = self.panes.entry(tables.pane.0).or_insert_with(|| {
            PaneFrame::new(
                tables.pane,
                PaneRect { pane: tables.pane, col: 0, row: 0, cols: 0, rows: 0 },
            )
        });
        if entry.styles.len() < tables.styles_from as usize {
            // A gap means we missed a message; a resync will follow.
            return;
        }
        entry.styles.truncate(tables.styles_from as usize);
        entry.styles.extend(tables.styles.iter().copied());
        if entry.clusters.len() >= tables.clusters_from as usize {
            entry.clusters.truncate(tables.clusters_from as usize);
            entry.clusters.extend(tables.clusters.iter().cloned());
        }
        if entry.links.len() >= tables.links_from as usize {
            entry.links.truncate(tables.links_from as usize);
            entry.links.extend(tables.links.iter().map(|l| Arc::from(l.as_str())));
        }
        self.generation += 1;
    }

    pub fn set_modes(
        &mut self,
        pane: PaneId,
        app_cursor_keys: bool,
        bracketed_paste: bool,
        cursor_visible: bool,
        mouse: haven_proto::MouseMode,
    ) {
        if let Some(entry) = self.panes.get_mut(&pane.0) {
            entry.app_cursor_keys = app_cursor_keys;
            entry.bracketed_paste = bracketed_paste;
            entry.cursor_visible = cursor_visible;
            entry.mouse = mouse;
        }
    }

    pub fn set_scroll(&mut self, pane: PaneId, offset: u32, total: u32) {
        if let Some(entry) = self.panes.get_mut(&pane.0) {
            entry.scroll = (offset, total);
        }
    }

    /// Whether a cursor should be drawn at all. Copy mode always has one —
    /// it is the thing you are moving.
    pub fn cursor_visible(&self) -> bool {
        self.copy_cursor.is_some()
            || self.focus.and_then(|id| self.pane(id)).is_none_or(|p| p.cursor_visible)
    }

    /// How far the focused pane is scrolled back, when it is.
    pub fn scroll(&self) -> Option<(u32, u32)> {
        let (offset, total) = self.focus.and_then(|id| self.pane(id))?.scroll;
        (offset > 0).then_some((offset, total))
    }

    pub fn apply_layout(&mut self, layout: &Layout) {
        for rect in &layout.panes {
            let entry =
                self.panes.entry(rect.pane.0).or_insert_with(|| PaneFrame::new(rect.pane, *rect));
            entry.rect = *rect;
            entry.rows.resize(rect.rows as usize, FrameRow::default());
        }
        // Panes the layout no longer mentions are gone.
        let live: Vec<u32> = layout.panes.iter().map(|r| r.pane.0).collect();
        self.panes.retain(|id, _| live.contains(id));
        self.focus = layout.active.or(self.focus).filter(|p| live.contains(&p.0));
        self.generation += 1;
    }

    pub fn apply_snapshot(&mut self, pane: PaneId, rows: &[RowRle], cursor: Cursor, rev: u32) {
        let rect = PaneRect { pane, col: 0, row: 0, cols: 0, rows: 0 };
        let frame = self.panes.entry(pane.0).or_insert_with(|| PaneFrame::new(pane, rect));
        frame.cursor = cursor;
        frame.rev = rev;
        let cells: Vec<Vec<Cell>> = rows
            .iter()
            .map(|r| {
                let mut out = Vec::new();
                r.decode(&mut out);
                out
            })
            .collect();
        frame.rebuild(&cells);
        self.generation += 1;
    }

    /// Returns whether anything actually changed: a delta whose rows we already
    /// have at this revision must not cost a frame.
    pub fn apply_delta(
        &mut self,
        pane: PaneId,
        rows: &[(u16, RowRle)],
        cursor: Cursor,
        rev: u32,
    ) -> bool {
        let Some(frame) = self.panes.get_mut(&pane.0) else { return false };
        let mut changed = frame.cursor.row != cursor.row
            || frame.cursor.col != cursor.col
            || frame.cursor.style != cursor.style;
        frame.cursor = cursor;
        for (index, rle) in rows {
            let i = *index as usize;
            if i >= frame.rows.len() {
                frame.rows.resize(i + 1, FrameRow::default());
            }
            let mut cells = Vec::new();
            rle.decode(&mut cells);
            let built = frame.spans_for(&cells);
            // Compare *content*, not the revision: a new rev carrying identical
            // cells must not cost a frame, or every idle redraw becomes a
            // repaint and the TUI frontend spends bytes on nothing.
            if frame.rows[i].spans != built.spans {
                changed = true;
            }
            frame.rows[i] = built;
        }
        frame.rev = rev;
        if changed {
            self.generation += 1;
        }
        changed
    }

    pub fn set_title(&mut self, pane: PaneId, title: String) {
        if let Some(frame) = self.panes.get_mut(&pane.0) {
            frame.title = Some(title);
            self.generation += 1;
        }
    }

    pub fn remove_pane(&mut self, pane: PaneId) {
        if self.panes.remove(&pane.0).is_some() {
            self.generation += 1;
        }
    }

    /// Flatten a pane to text, trailing blanks trimmed. Used by tests and by
    /// `capture-pane`.
    pub fn pane_text(&self, pane: PaneId) -> String {
        let Some(frame) = self.pane(pane) else { return String::new() };
        let mut out = String::new();
        for row in &frame.rows {
            let mut line = String::new();
            for span in &row.spans {
                let target = span.col as usize;
                while line.chars().count() < target {
                    line.push(' ');
                }
                line.push_str(&span.text);
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }
}

/// Flatten panes, separators and widgets onto one screen-sized grid of cells.
///
/// Compositing is presentation-neutral — it turns pane rectangles and rows
/// into a screen — so it lives here rather than in either frontend. The TUI
/// frontend serializes the result to escape sequences; the GPU frontend
/// rasterizes it. Neither has its own copy of this logic.
pub fn composite(frame: &FrameModel) -> (Screen, (usize, usize)) {
    let cols = frame.size.cols as usize;
    let rows = frame.size.rows as usize;
    let mut screen = blank_screen(cols, rows);
    let mut cursor = (0usize, 0usize);

    for pane in frame.panes.values() {
        place(&mut screen, &pane.rect, &pane.rows, cols);
        if Some(pane.id) == frame.focus {
            cursor = (
                (pane.rect.row as usize + pane.cursor.row).min(rows.saturating_sub(1)),
                (pane.rect.col as usize + pane.cursor.col).min(cols.saturating_sub(1)),
            );
        }
    }
    paint_selection(&mut screen, frame, cols, rows);
    // After every pane, because a rule is decided by what its neighbours are.
    separators(&mut screen, frame, cols, rows);
    for widget in &frame.widgets {
        place(&mut screen, &widget.rect, &widget.rows, cols);
    }
    // Copy mode's cursor wins: it is the one being moved.
    if let Some((pane, row, col)) = frame.copy_cursor {
        if let Some(rect) = frame.pane(pane).map(|p| p.rect) {
            cursor = (
                (rect.row as usize + row).min(rows.saturating_sub(1)),
                (rect.col as usize + col).min(cols.saturating_sub(1)),
            );
        }
    }
    (screen, cursor)
}

/// Repaint the selected cells in the theme's selection colours.
///
/// Done as a pass over the composited screen rather than inside `place`,
/// because a selection is a property of the *view* — which rows are on screen
/// right now — and not of any pane's content.
fn paint_selection(screen: &mut Screen, frame: &FrameModel, cols: usize, rows: usize) {
    let Some((pane, start, end)) = frame.selection else { return };
    let Some(rect) = frame.pane(pane).map(|p| p.rect) else { return };
    let style = frame.theme.selection.style();
    for row in start.0..=end.0 {
        // A multi-row selection takes whole lines between its ends, which is
        // what a terminal selection means and what a per-row box does not.
        let first = if row == start.0 { start.1 } else { 0 };
        let last = if row == end.0 { end.1 } else { rect.cols as usize - 1 };
        let screen_row = rect.row as usize + row;
        if screen_row >= rows {
            break;
        }
        for col in first..=last {
            let screen_col = rect.col as usize + col;
            if screen_col >= cols || col >= rect.cols as usize {
                break;
            }
            screen[screen_row][screen_col].style = style;
        }
    }
}

fn place(screen: &mut Screen, rect: &PaneRect, rows: &[FrameRow], cols: usize) {
    let right_edge = (rect.col as usize + rect.cols as usize).min(cols);
    for (r, row) in rows.iter().enumerate() {
        let screen_row = rect.row as usize + r;
        if screen_row >= screen.len() {
            break;
        }
        for span in &row.spans {
            let start = rect.col as usize + span.col as usize;
            for (col, ch) in (start..right_edge).zip(span.text.chars()) {
                screen[screen_row][col] =
                    ScreenCell::new(ch.to_string(), span.style).linked(span.link.clone());
            }
        }
    }
}

/// Rules in the gaps the layout leaves between panes.
///
/// `haven_mux::SEPARATOR` reserves one cell between neighbours in *both*
/// directions, but only the vertical one was ever drawn: stacked panes ran
/// into each other across a blank row. A gap is a rule only where there is a
/// pane on each side of it, which is also what keeps the row the status line
/// sits on from growing one.
///
/// Junctions come from the neighbouring gap cells rather than from the pane
/// tree, so a four-way meeting is a cross however the splits were nested.
fn separators(screen: &mut Screen, frame: &FrameModel, cols: usize, rows: usize) {
    if cols == 0 || rows == 0 {
        return;
    }
    let mut covered = vec![vec![false; cols]; rows];
    for pane in frame.panes.values() {
        let rect = &pane.rect;
        let last_row = (rect.row as usize + rect.rows as usize).min(rows);
        let last_col = (rect.col as usize + rect.cols as usize).min(cols);
        for row in covered.iter_mut().take(last_row).skip(rect.row as usize) {
            for cell in row.iter_mut().take(last_col).skip(rect.col as usize) {
                *cell = true;
            }
        }
    }

    let between = |a: Option<bool>, b: Option<bool>| a == Some(true) && b == Some(true);
    let at = |grid: &[Vec<bool>], row: usize, col: usize| grid[row][col];
    let mut vertical = vec![vec![false; cols]; rows];
    let mut horizontal = vec![vec![false; cols]; rows];
    for row in 0..rows {
        for col in 0..cols {
            if covered[row][col] {
                continue;
            }
            vertical[row][col] = between(
                col.checked_sub(1).map(|c| at(&covered, row, c)),
                (col + 1 < cols).then(|| at(&covered, row, col + 1)),
            );
            horizontal[row][col] = between(
                row.checked_sub(1).map(|r| at(&covered, r, col)),
                (row + 1 < rows).then(|| at(&covered, row + 1, col)),
            );
        }
    }

    let g = &frame.theme.glyphs;
    let style = frame.theme.pane_border.style();
    for row in 0..rows {
        for col in 0..cols {
            if covered[row][col] {
                continue;
            }
            // A rule always spans its own cell end to end; it gains the other
            // axis from the rules that run into it. The cell where four panes
            // meet is in neither run — nothing is beside it and nothing is
            // above it — so it is drawn from its neighbours alone.
            let up = vertical[row][col] || (row > 0 && vertical[row - 1][col]);
            let down = vertical[row][col] || (row + 1 < rows && vertical[row + 1][col]);
            let left = horizontal[row][col] || (col > 0 && horizontal[row][col - 1]);
            let right = horizontal[row][col] || (col + 1 < cols && horizontal[row][col + 1]);
            let arms = u8::from(up) + u8::from(down) + u8::from(left) + u8::from(right);
            if !vertical[row][col] && !horizontal[row][col] && arms < 2 {
                continue;
            }
            screen[row][col] = ScreenCell::new(rule(g, up, down, left, right), style);
        }
    }
}

/// The glyph for a rule with these arms. Every one of them comes from the
/// theme: a terminal whose font has no box drawing gets `+`, not a hole.
fn rule(g: &haven_proto::theme::Glyphs, up: bool, down: bool, left: bool, right: bool) -> &str {
    match (up, down, left, right) {
        (true, true, true, true) => &g.cross,
        (true, true, true, false) => &g.tee_left,
        (true, true, false, true) => &g.tee_right,
        (true, false, true, true) => &g.tee_up,
        (false, true, true, true) => &g.tee_down,
        (_, _, true, true) => &g.horizontal,
        _ => &g.vertical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cells(s: &str) -> Vec<Cell> {
        s.chars().map(Cell::new).collect()
    }

    fn layout(panes: &[(u32, u16, u16, u16, u16)]) -> Layout {
        Layout {
            window: 0,
            panes: panes
                .iter()
                .map(|&(id, col, row, cols, rows)| PaneRect {
                    pane: PaneId(id),
                    col,
                    row,
                    cols,
                    rows,
                })
                .collect(),
            active: panes.first().map(|p| PaneId(p.0)),
        }
    }

    #[test]
    fn snapshot_becomes_text() {
        let mut frame = FrameModel::new(CellSize::default());
        frame.apply_layout(&layout(&[(1, 0, 0, 10, 2)]));
        frame.apply_snapshot(
            PaneId(1),
            &[RowRle::encode(&cells("hello")), RowRle::encode(&cells("world"))],
            Cursor::default(),
            1,
        );
        assert_eq!(frame.pane_text(PaneId(1)), "hello\nworld\n");
    }

    #[test]
    fn a_delta_that_changes_nothing_does_not_cost_a_frame() {
        let mut frame = FrameModel::new(CellSize::default());
        frame.apply_layout(&layout(&[(1, 0, 0, 10, 1)]));
        frame.apply_snapshot(PaneId(1), &[RowRle::encode(&cells("same"))], Cursor::default(), 1);
        let generation = frame.generation;

        let changed = frame.apply_delta(
            PaneId(1),
            &[(0, RowRle::encode(&cells("same")))],
            Cursor::default(),
            2,
        );
        assert!(!changed, "identical content must not report a change");
        assert_eq!(frame.generation, generation, "generation must not advance");
    }

    #[test]
    fn a_delta_that_changes_a_row_redraws() {
        let mut frame = FrameModel::new(CellSize::default());
        frame.apply_layout(&layout(&[(1, 0, 0, 10, 1)]));
        frame.apply_snapshot(PaneId(1), &[RowRle::encode(&cells("old"))], Cursor::default(), 1);
        assert!(frame.apply_delta(
            PaneId(1),
            &[(0, RowRle::encode(&cells("new")))],
            Cursor::default(),
            2
        ));
        assert_eq!(frame.pane_text(PaneId(1)), "new\n");
    }

    #[test]
    fn cursor_movement_alone_still_redraws() {
        let mut frame = FrameModel::new(CellSize::default());
        frame.apply_layout(&layout(&[(1, 0, 0, 10, 2)]));
        frame.apply_snapshot(PaneId(1), &[RowRle::encode(&cells("x"))], Cursor::default(), 1);
        let moved = Cursor { row: 1, col: 3, ..Default::default() };
        assert!(frame.apply_delta(PaneId(1), &[], moved, 2));
    }

    #[test]
    fn layout_removes_panes_it_no_longer_mentions() {
        let mut frame = FrameModel::new(CellSize::default());
        frame.apply_layout(&layout(&[(1, 0, 0, 10, 2), (2, 10, 0, 10, 2)]));
        assert_eq!(frame.panes.len(), 2);
        frame.apply_layout(&layout(&[(1, 0, 0, 20, 2)]));
        assert_eq!(frame.panes.len(), 1);
        assert!(frame.pane(PaneId(2)).is_none());
    }

    #[test]
    fn wide_spacers_do_not_produce_a_cell_of_their_own() {
        let mut row = cells("a");
        row.push(Cell { c: '漢', flags: CellFlags::WIDE, ..Cell::default() });
        row.push(Cell { c: ' ', flags: CellFlags::WIDE_SPACER, ..Cell::default() });

        let mut frame = FrameModel::new(CellSize::default());
        frame.apply_layout(&layout(&[(1, 0, 0, 10, 1)]));
        frame.apply_snapshot(PaneId(1), &[RowRle::encode(&row)], Cursor::default(), 1);
        assert_eq!(frame.pane_text(PaneId(1)), "a漢\n");
    }

    /// The screen as characters, so a rule is readable in the assertion.
    fn picture(frame: &FrameModel) -> Vec<String> {
        composite(frame)
            .0
            .iter()
            .map(|row| row.iter().map(|c| c.text.clone()).collect::<String>())
            .collect()
    }

    fn filled(frame: &mut FrameModel, panes: &[(u32, u16, u16, u16, u16)]) {
        frame.apply_layout(&layout(panes));
        for &(id, _, _, cols, rows) in panes {
            let row = cells(&"x".repeat(cols as usize));
            let rows = (0..rows).map(|_| RowRle::encode(&row)).collect::<Vec<_>>();
            frame.apply_snapshot(PaneId(id), &rows, Cursor::default(), 1);
        }
    }

    #[test]
    fn stacked_panes_are_divided_by_a_rule_not_a_blank_row() {
        // The layout reserves the row; nothing used to draw in it, so two
        // panes above one another just ran together.
        let mut frame = FrameModel::new(CellSize { cols: 5, rows: 5, ..Default::default() });
        filled(&mut frame, &[(1, 0, 0, 5, 2), (2, 0, 3, 5, 2)]);
        assert_eq!(picture(&frame)[2], "─────", "the gap row was left blank");
    }

    #[test]
    fn side_by_side_panes_are_divided_by_a_rule() {
        let mut frame = FrameModel::new(CellSize { cols: 5, rows: 2, ..Default::default() });
        filled(&mut frame, &[(1, 0, 0, 2, 2), (2, 3, 0, 2, 2)]);
        assert_eq!(picture(&frame), vec!["xx│xx".to_string(), "xx│xx".to_string()]);
    }

    #[test]
    fn four_panes_meet_at_a_cross() {
        // The cell where four panes meet is in neither run — no pane beside
        // it, none above it — so it is only right if junctions are resolved
        // from the neighbouring rules.
        let mut frame = FrameModel::new(CellSize { cols: 5, rows: 5, ..Default::default() });
        filled(&mut frame, &[(1, 0, 0, 2, 2), (2, 3, 0, 2, 2), (3, 0, 3, 2, 2), (4, 3, 3, 2, 2)]);
        assert_eq!(picture(&frame)[2], "──┼──");
    }

    #[test]
    fn a_rule_that_stops_at_another_becomes_a_tee() {
        // One tall pane on the left, two stacked on the right.
        let mut frame = FrameModel::new(CellSize { cols: 5, rows: 5, ..Default::default() });
        filled(&mut frame, &[(1, 0, 0, 2, 5), (2, 3, 0, 2, 2), (3, 3, 3, 2, 2)]);
        assert_eq!(picture(&frame)[2], "xx├──");
    }

    #[test]
    fn the_row_the_status_line_sits_on_does_not_become_a_rule() {
        // A pane one row short of the screen leaves a row free. It is not a
        // gap between panes, and a rule there would be drawn under the bar.
        let mut frame = FrameModel::new(CellSize { cols: 4, rows: 3, ..Default::default() });
        filled(&mut frame, &[(1, 0, 0, 4, 2)]);
        assert_eq!(picture(&frame)[2], "    ", "a rule was drawn where no pane follows");
    }

    #[test]
    fn rules_are_painted_with_the_theme_not_a_hardcoded_colour() {
        use haven_proto::theme::{Pair, Theme};
        let mut frame = FrameModel::new(CellSize { cols: 5, rows: 2, ..Default::default() });
        frame.theme = Theme {
            pane_border: Pair::new(haven_core::Color::Indexed(3), haven_core::Color::Indexed(1)),
            ..Theme::default()
        };
        filled(&mut frame, &[(1, 0, 0, 2, 2), (2, 3, 0, 2, 2)]);
        let cell = composite(&frame).0[0][2].clone();
        assert_eq!(cell.style.fg, haven_core::Color::Indexed(3));
        assert_eq!(cell.style.bg, haven_core::Color::Indexed(1));
    }

    #[test]
    fn an_ascii_theme_draws_rules_it_can_actually_show() {
        use haven_proto::theme::Theme;
        let mut frame = FrameModel::new(CellSize { cols: 5, rows: 5, ..Default::default() });
        frame.theme = Theme::plain();
        filled(&mut frame, &[(1, 0, 0, 2, 2), (2, 3, 0, 2, 2), (3, 0, 3, 2, 2), (4, 3, 3, 2, 2)]);
        assert_eq!(picture(&frame)[2], "--+--");
        assert_eq!(picture(&frame)[0], "xx|xx");
    }

    #[test]
    fn styles_arrive_as_a_table_and_are_resolved_into_spans() {
        use haven_core::{Color, Rgb, StyleId};

        let red = Style { fg: Color::Rgb(Rgb::new(255, 0, 0)), ..Default::default() };
        let mut frame = FrameModel::new(CellSize::default());
        frame.apply_layout(&layout(&[(1, 0, 0, 10, 1)]));
        frame.apply_tables(&Tables {
            pane: PaneId(1),
            styles_from: 1,
            styles: vec![red],
            clusters_from: 0,
            clusters: vec![],
            links_from: 0,
            links: vec![],
        });

        let row = vec![Cell::new('a'), Cell { c: 'b', style: StyleId(1), ..Cell::default() }];
        frame.apply_snapshot(PaneId(1), &[RowRle::encode(&row)], Cursor::default(), 1);

        let spans = &frame.pane(PaneId(1)).unwrap().rows[0].spans;
        assert_eq!(spans.len(), 2, "a style change must break the span");
        assert_eq!(spans[0].style, Style::default());
        assert_eq!(spans[1].style, red, "the style table was not applied");
    }

    #[test]
    fn clusters_arrive_as_a_table_too() {
        let mut frame = FrameModel::new(CellSize::default());
        frame.apply_layout(&layout(&[(1, 0, 0, 10, 1)]));
        frame.apply_tables(&Tables {
            pane: PaneId(1),
            styles_from: 1,
            styles: vec![],
            clusters_from: 0,
            clusters: vec!["e\u{0301}".into()],
            links_from: 0,
            links: vec![],
        });
        let row = vec![Cell { c: 'e', cluster: 1, ..Cell::default() }];
        frame.apply_snapshot(PaneId(1), &[RowRle::encode(&row)], Cursor::default(), 1);
        assert_eq!(frame.pane_text(PaneId(1)), "e\u{0301}\n");
    }

    #[test]
    fn a_table_with_a_gap_is_ignored_rather_than_misapplied() {
        // A missed message must not silently shift every style id.
        let mut frame = FrameModel::new(CellSize::default());
        frame.apply_layout(&layout(&[(1, 0, 0, 10, 1)]));
        frame.apply_tables(&Tables {
            pane: PaneId(1),
            styles_from: 9,
            styles: vec![Style::default()],
            clusters_from: 0,
            clusters: vec![],
            links_from: 0,
            links: vec![],
        });
        assert_eq!(frame.pane(PaneId(1)).unwrap().styles.len(), 1, "the gap was accepted");
    }
}
