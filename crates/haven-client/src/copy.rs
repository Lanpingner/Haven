//! Copy mode: a cursor over a pane's history, a selection, and a search.
//!
//! It lives in the client rather than behind the key filter for the same
//! reason the chooser does: it is the only place holding both halves of what
//! it needs — the view the frontend is showing and the pane it belongs to.
//!
//! Coordinates are the awkward part and are worth stating once. The *cursor*
//! is in view coordinates, because that is where it is drawn. The *anchor* of
//! a selection is in history coordinates ([`haven_proto::HistoryPoint`]),
//! because selecting more than a screenful means scrolling, and a view-
//! relative anchor would slide out from under the selection as soon as it
//! did. `absolute()` and `view_row()` convert between the two, using the
//! offset the daemon reports.

use haven_proto::{HistoryPoint, PaneId};

/// What a keystroke in copy mode asks the client to do. Returned rather than
/// acted on so the mode stays testable without a transport.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CopyAction {
    /// Nothing the client has to do; just redraw.
    None,
    /// Leave copy mode.
    Leave,
    /// Move the view by this many rows, positive being back into history.
    Scroll(i32),
    /// Copy this range of the history and leave.
    Copy(HistoryPoint, HistoryPoint),
    /// Run a search from this row.
    Search { query: String, from: u32, backwards: bool },
}

/// How far `Ctrl+U` and `Ctrl+D` move, as a fraction of the pane.
fn half(rows: usize) -> i32 {
    (rows / 2).max(1) as i32
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CopyMode {
    pub pane: PaneId,
    /// Cursor, in view coordinates.
    pub row: usize,
    pub col: usize,
    /// Where a selection began, in history coordinates. `None` means no
    /// selection is being made.
    pub anchor: Option<HistoryPoint>,
    /// The search being typed, if one is.
    pub query: Option<String>,
    /// Which way the search being typed will run.
    pub backwards: bool,
    /// The last completed search, for `n` and `N`.
    pub last: Option<(String, bool)>,
    /// Size of the pane's view, so movement can be clamped without asking
    /// the frame for it on every keystroke.
    pub rows: usize,
    pub cols: usize,
    /// The view's position in the history, mirrored from the daemon.
    pub offset: u32,
    pub total: u32,
}

impl CopyMode {
    pub fn new(pane: PaneId, rows: usize, cols: usize, offset: u32, total: u32) -> Self {
        Self {
            pane,
            // Start at the bottom left, where the newest output is.
            row: rows.saturating_sub(1),
            col: 0,
            anchor: None,
            query: None,
            backwards: true,
            last: None,
            rows,
            cols,
            offset,
            total,
        }
    }

    /// The history row a view row refers to.
    pub fn absolute(&self, view_row: usize) -> u32 {
        (self.total - self.offset) + view_row as u32
    }

    /// Where a history row sits in the view, if it is on screen at all.
    pub fn view_row(&self, row: u32) -> Option<usize> {
        let top = self.total - self.offset;
        (row >= top && (row - top) < self.rows as u32).then(|| (row - top) as usize)
    }

    pub fn cursor(&self) -> HistoryPoint {
        HistoryPoint { row: self.absolute(self.row), col: self.col as u16 }
    }

    /// The selection, in view coordinates, as (row, col) start and end
    /// inclusive. `None` when nothing is selected or it is entirely off
    /// screen; clipped to the view when it runs past either edge.
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.anchor?;
        let cursor = self.cursor();
        let (start, end) = if anchor <= cursor { (anchor, cursor) } else { (cursor, anchor) };
        let top = self.total - self.offset;
        let bottom = top + self.rows as u32;
        // Defensive: one end of the selection is always the cursor, which is
        // always in the view, so this cannot normally hold. `offset` and
        // `total` are mirrored from the daemon, and a guard beats an
        // arithmetic underflow if they ever arrive out of step.
        if end.row < top || start.row >= bottom {
            return None;
        }
        let start_view = if start.row < top {
            (0usize, 0usize)
        } else {
            ((start.row - top) as usize, start.col as usize)
        };
        let end_view = if end.row >= bottom {
            (self.rows - 1, self.cols.saturating_sub(1))
        } else {
            ((end.row - top) as usize, end.col as usize)
        };
        Some((start_view, end_view))
    }

    /// What the bar should say while this is up.
    pub fn label(&self) -> String {
        match &self.query {
            Some(q) => format!("{}{q}", if self.backwards { "?" } else { "/" }),
            None if self.anchor.is_some() => "COPY · select".into(),
            None => "COPY".into(),
        }
    }

    /// Handle one keystroke. `line` is the text of the cursor's row, which
    /// word motion needs and the mode does not store.
    pub fn key(&mut self, event: &crate::input::KeyEvent, line: &str) -> CopyAction {
        use crate::input::{Key, Modifiers, NamedKey};
        let ctrl = event.mods.contains(Modifiers::CTRL);

        // A search being typed owns the keyboard, exactly as the command line
        // does: every printable character belongs to it.
        if let Some(query) = &mut self.query {
            match &event.key {
                Key::Named(NamedKey::Escape) => {
                    self.query = None;
                }
                Key::Named(NamedKey::Backspace) => {
                    if query.pop().is_none() {
                        self.query = None;
                    }
                }
                Key::Named(NamedKey::Enter) => {
                    let query = self.query.take().unwrap_or_default();
                    if query.is_empty() {
                        return CopyAction::None;
                    }
                    let backwards = self.backwards;
                    self.last = Some((query.clone(), backwards));
                    return CopyAction::Search { query, from: self.absolute(self.row), backwards };
                }
                Key::Char(c) if !ctrl => query.push(*c),
                _ => {}
            }
            return CopyAction::None;
        }

        match &event.key {
            Key::Named(NamedKey::Escape) | Key::Char('q') => return CopyAction::Leave,
            Key::Char('v') | Key::Char(' ') => {
                // Toggle: a second press drops the selection rather than
                // leaving one that cannot be got rid of.
                self.anchor = if self.anchor.is_some() { None } else { Some(self.cursor()) };
            }
            Key::Named(NamedKey::Enter) | Key::Char('y') => {
                let start = self.anchor.unwrap_or_else(|| self.cursor());
                let end = self.cursor();
                let (start, end) = if start <= end { (start, end) } else { (end, start) };
                return CopyAction::Copy(start, end);
            }
            Key::Char('/') => {
                self.query = Some(String::new());
                self.backwards = false;
            }
            Key::Char('?') => {
                self.query = Some(String::new());
                self.backwards = true;
            }
            Key::Char('n') | Key::Char('N') => {
                let Some((query, backwards)) = self.last.clone() else {
                    return CopyAction::None;
                };
                let flip = matches!(event.key, Key::Char('N'));
                return CopyAction::Search {
                    query,
                    from: self.absolute(self.row),
                    backwards: backwards != flip,
                };
            }
            Key::Char('h') | Key::Named(NamedKey::Left) => self.col = self.col.saturating_sub(1),
            Key::Char('l') | Key::Named(NamedKey::Right) => {
                self.col = (self.col + 1).min(self.cols.saturating_sub(1));
            }
            Key::Char('0') => self.col = 0,
            Key::Char('$') => self.col = line.trim_end().len().saturating_sub(1),
            Key::Char('w') => self.col = next_word(line, self.col),
            Key::Char('b') => self.col = previous_word(line, self.col),
            Key::Char('k') | Key::Named(NamedKey::Up) => return self.step(-1),
            Key::Char('j') | Key::Named(NamedKey::Down) => return self.step(1),
            Key::Char('u') if ctrl => return self.step(-half(self.rows)),
            Key::Char('d') if ctrl => return self.step(half(self.rows)),
            Key::Named(NamedKey::PageUp) => return self.step(-(self.rows as i32)),
            Key::Named(NamedKey::PageDown) => return self.step(self.rows as i32),
            // The oldest row there is, and the newest.
            Key::Char('g') => return CopyAction::Scroll(i32::MAX),
            Key::Char('G') => {
                self.row = self.rows.saturating_sub(1);
                return CopyAction::Scroll(i32::MIN);
            }
            _ => {}
        }
        CopyAction::None
    }

    /// Move the cursor by rows, scrolling the view when it would leave it.
    fn step(&mut self, delta: i32) -> CopyAction {
        let wanted = self.row as i32 + delta;
        let last = self.rows.saturating_sub(1) as i32;
        if wanted < 0 {
            self.row = 0;
            return CopyAction::Scroll(-wanted);
        }
        if wanted > last {
            self.row = last.max(0) as usize;
            return CopyAction::Scroll(-(wanted - last));
        }
        self.row = wanted as usize;
        CopyAction::None
    }

    /// Mirror the daemon's report of where the view sits.
    ///
    /// The cursor stays on its *view* row rather than following its history
    /// row. That is what makes pushing against the top edge work: the cursor
    /// asked for the row above, the view moved up by one to show it, and
    /// re-pinning to the old history row would put the cursor straight back
    /// where it started — a key that appears to do nothing.
    pub fn scrolled(&mut self, offset: u32, total: u32) {
        self.offset = offset;
        self.total = total;
    }
}

/// The start of the next word at or after `col`, vi's `w`.
fn next_word(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut i = col;
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i.min(chars.len().saturating_sub(1))
}

/// The start of the word before `col`, vi's `b`.
fn previous_word(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut i = col.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{KeyEvent, Modifiers, NamedKey};

    fn mode() -> CopyMode {
        // A ten-row view onto a history of thirty scrollback rows, sitting at
        // the bottom: history rows 30..40 are on screen.
        CopyMode::new(PaneId(1), 10, 20, 0, 30)
    }

    fn press(c: char) -> KeyEvent {
        KeyEvent::char(c)
    }

    fn named(k: NamedKey) -> KeyEvent {
        KeyEvent::named(k)
    }

    #[test]
    fn the_cursor_starts_on_the_newest_row() {
        let m = mode();
        assert_eq!((m.row, m.col), (9, 0));
        assert_eq!(m.absolute(9), 39);
    }

    #[test]
    fn moving_off_the_top_scrolls_instead_of_sticking() {
        let mut m = mode();
        for _ in 0..9 {
            assert_eq!(m.key(&press('k'), ""), CopyAction::None);
        }
        assert_eq!(m.row, 0);
        // One more has nowhere to go in the view, so the view moves.
        assert_eq!(m.key(&press('k'), ""), CopyAction::Scroll(1));
        assert_eq!(m.row, 0, "the cursor should stay at the edge it pushed against");
    }

    #[test]
    fn a_selection_is_anchored_in_history_not_in_the_view() {
        let mut m = mode();
        m.key(&press('v'), "");
        assert_eq!(m.anchor, Some(HistoryPoint { row: 39, col: 0 }));

        // Scroll five rows back. The anchor is a history row and must not
        // have moved with the view; a view-relative one would have.
        m.scrolled(5, 30);
        assert_eq!(m.anchor, Some(HistoryPoint { row: 39, col: 0 }));
        assert_eq!(m.absolute(m.row), 34, "the cursor keeps its place on screen");
    }

    #[test]
    fn pushing_against_the_top_edge_really_moves_the_cursor() {
        // The cursor keeps its view row across a scroll. Re-pinning it to the
        // history row it was on would undo the move the scroll was for, and
        // `k` at the top of the view would do nothing at all.
        let mut m = mode();
        for _ in 0..9 {
            m.key(&press('k'), "");
        }
        assert_eq!(m.absolute(m.row), 30);
        assert_eq!(m.key(&press('k'), ""), CopyAction::Scroll(1));
        m.scrolled(1, 30);
        assert_eq!(m.absolute(m.row), 29, "the cursor did not reach the newly shown row");
    }

    #[test]
    fn a_selection_running_off_the_top_is_clipped_not_dropped() {
        // Anchor above the view, cursor inside it: only the part on screen is
        // painted, and it starts at the top-left rather than nowhere.
        let mut m = CopyMode::new(PaneId(1), 10, 20, 10, 30);
        m.anchor = Some(HistoryPoint { row: 15, col: 3 });
        m.row = 4;
        let (start, end) = m.selection().expect("partly visible");
        assert_eq!(start, (0, 0), "the start should be clipped to the top of the view");
        assert_eq!(end, (4, 0));
    }

    #[test]
    fn space_toggles_the_selection_off_again() {
        let mut m = mode();
        m.key(&press(' '), "");
        assert!(m.anchor.is_some());
        m.key(&press(' '), "");
        assert!(m.anchor.is_none(), "a selection you cannot cancel is a trap");
    }

    #[test]
    fn enter_copies_the_whole_selection_in_order() {
        let mut m = mode();
        m.key(&press('v'), "");
        m.key(&press('k'), "");
        m.key(&press('k'), "");
        // The cursor is now above the anchor; the copy must still run
        // forwards through the history.
        match m.key(&named(NamedKey::Enter), "") {
            CopyAction::Copy(start, end) => {
                assert_eq!(start.row, 37);
                assert_eq!(end.row, 39);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn enter_with_no_selection_copies_the_cell_under_the_cursor() {
        let mut m = mode();
        match m.key(&named(NamedKey::Enter), "") {
            CopyAction::Copy(start, end) => assert_eq!(start, end),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_search_is_typed_then_run() {
        let mut m = mode();
        m.key(&press('?'), "");
        for c in "hello".chars() {
            m.key(&press(c), "");
        }
        assert_eq!(m.label(), "?hello", "the bar must show what is being typed");
        match m.key(&named(NamedKey::Enter), "") {
            CopyAction::Search { query, backwards, from } => {
                assert_eq!(query, "hello");
                assert!(backwards, "? searches backwards");
                assert_eq!(from, 39);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(m.last, Some(("hello".into(), true)));
    }

    #[test]
    fn letters_typed_into_a_search_are_not_motions() {
        // `j` and `k` are movement outside a search and text inside one; a
        // mode that got this wrong would be unusable for any query with a
        // `j` in it.
        let mut m = mode();
        let before = m.row;
        m.key(&press('/'), "");
        m.key(&press('j'), "");
        m.key(&press('k'), "");
        assert_eq!(m.row, before, "the search text moved the cursor");
        assert_eq!(m.label(), "/jk");
    }

    #[test]
    fn repeating_a_search_can_be_reversed() {
        let mut m = mode();
        m.key(&press('/'), "");
        m.key(&press('x'), "");
        m.key(&named(NamedKey::Enter), "");
        match m.key(&press('n'), "") {
            CopyAction::Search { backwards, .. } => assert!(!backwards),
            other => panic!("{other:?}"),
        }
        match m.key(&press('N'), "") {
            CopyAction::Search { backwards, .. } => assert!(backwards, "N reverses the direction"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn word_motion_walks_a_line() {
        let mut m = mode();
        let line = "one two three";
        m.key(&press('w'), line);
        assert_eq!(m.col, 4);
        m.key(&press('w'), line);
        assert_eq!(m.col, 8);
        m.key(&press('b'), line);
        assert_eq!(m.col, 4);
        m.key(&press('$'), line);
        assert_eq!(m.col, 12);
        m.key(&press('0'), line);
        assert_eq!(m.col, 0);
    }

    #[test]
    fn ctrl_u_and_d_move_by_half_a_screen() {
        let mut m = mode();
        assert_eq!(m.key(&KeyEvent::char('u').with(Modifiers::CTRL), ""), CopyAction::None);
        assert_eq!(m.row, 4, "half of ten rows up from the bottom");
        assert_eq!(m.key(&KeyEvent::char('d').with(Modifiers::CTRL), ""), CopyAction::None);
        assert_eq!(m.row, 9);
    }

    #[test]
    fn q_and_escape_both_leave() {
        assert_eq!(mode().key(&press('q'), ""), CopyAction::Leave);
        assert_eq!(mode().key(&named(NamedKey::Escape), ""), CopyAction::Leave);
    }
}
