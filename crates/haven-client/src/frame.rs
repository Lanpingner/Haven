//! The renderer-agnostic frame: cell spans, pane rects, cursor, widgets.
//!
//! Spans carry a resolved [`Style`] rather than a `StyleId`. The interner is a
//! storage optimization inside the grid; a frame is a presentation snapshot,
//! and making every frontend carry a style table just to draw text would leak
//! the grid's representation into both of them.

use std::collections::BTreeMap;

use haven_core::{blank_screen, Cell, CellFlags, Cursor, Palette, Screen, ScreenCell, Style};
use haven_proto::{CellSize, Layout, PaneId, PaneRect, RowRle, Tables};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Span {
    pub col: u16,
    pub text: String,
    pub style: Style,
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
    /// Modes that change how this client must encode input.
    pub app_cursor_keys: bool,
    pub bracketed_paste: bool,
}

impl PaneFrame {
    fn new(id: PaneId, rect: PaneRect) -> Self {
        Self { id, rect, styles: vec![Style::default()], ..Default::default() }
    }

    fn style(&self, cell: &Cell) -> Style {
        self.styles.get(cell.style.0 as usize).copied().unwrap_or_default()
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
            match spans.last_mut() {
                Some(span) if span.style == style => span.text.push_str(&text),
                _ => spans.push(Span { col: i as u16, text, style }),
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
        self.generation += 1;
    }

    pub fn set_modes(&mut self, pane: PaneId, app_cursor_keys: bool, bracketed_paste: bool) {
        if let Some(entry) = self.panes.get_mut(&pane.0) {
            entry.app_cursor_keys = app_cursor_keys;
            entry.bracketed_paste = bracketed_paste;
        }
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
        separator(&mut screen, &pane.rect, cols, rows);
        if Some(pane.id) == frame.focus {
            cursor = (
                (pane.rect.row as usize + pane.cursor.row).min(rows.saturating_sub(1)),
                (pane.rect.col as usize + pane.cursor.col).min(cols.saturating_sub(1)),
            );
        }
    }
    for widget in &frame.widgets {
        place(&mut screen, &widget.rect, &widget.rows, cols);
    }
    (screen, cursor)
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
                screen[screen_row][col] = ScreenCell::new(ch.to_string(), span.style);
            }
        }
    }
}

/// A vertical rule to the right of every pane that does not reach the edge.
fn separator(screen: &mut Screen, rect: &PaneRect, cols: usize, rows: usize) {
    let right = rect.col as usize + rect.cols as usize;
    if right >= cols {
        return;
    }
    for r in 0..rect.rows as usize {
        let row = rect.row as usize + r;
        if row < rows {
            screen[row][right] = ScreenCell::new("│", Style::default());
        }
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
        });
        assert_eq!(frame.pane(PaneId(1)).unwrap().styles.len(), 1, "the gap was accepted");
    }
}
