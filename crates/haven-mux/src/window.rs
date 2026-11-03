//! Windows (the things a tab bar shows) and their pane trees.

use haven_proto::{CellSize, Layout, PaneId, PaneRect, WindowId};

use crate::layout::{Direction, Edge, PaneNode};

#[derive(Clone, PartialEq, Debug)]
pub struct Window {
    pub id: WindowId,
    pub title: Option<String>,
    pub root: PaneNode,
    pub active: PaneId,
    /// Zoom is an *override*, never a mutation of the tree, so unzooming is
    /// lossless (`docs/PLAN.md` §10).
    pub zoomed: Option<PaneId>,
    /// The pane focus was on before this one, for `last-pane`.
    last: Option<PaneId>,
    /// Everything typed here goes to every pane in this window.
    pub synchronized: bool,
    /// A pane here rang while the window was not the active one. Cleared on
    /// arrival, which is what makes it mean "since you last looked".
    pub bell: bool,
    /// A pane here produced output while the window was not the active one.
    pub activity: bool,
}

impl Window {
    pub fn new(id: WindowId, pane: PaneId) -> Self {
        Self {
            id,
            title: None,
            root: PaneNode::Leaf(pane),
            active: pane,
            zoomed: None,
            last: None,
            synchronized: false,
            bell: false,
            activity: false,
        }
    }

    pub fn panes(&self) -> Vec<PaneId> {
        self.root.panes()
    }

    pub fn pane_count(&self) -> usize {
        self.panes().len()
    }

    /// Solve the tree for a client of this size.
    pub fn rects(&self, size: CellSize) -> Vec<PaneRect> {
        let area = PaneRect { pane: self.active, col: 0, row: 0, cols: size.cols, rows: size.rows };
        match self.zoomed {
            // A zoomed pane is the only thing on screen; the rest of the tree
            // is untouched and comes back exactly as it was.
            Some(pane) if self.root.contains(pane) => vec![PaneRect { pane, ..area }],
            _ => self.root.solve(area),
        }
    }

    pub fn layout(&self, size: CellSize) -> Layout {
        Layout { window: self.id.0, panes: self.rects(size), active: Some(self.active) }
    }

    /// Split the active pane; the new pane becomes active, as users expect.
    pub fn split(&mut self, dir: Direction, new: PaneId, ratio: f32) -> bool {
        // Splitting while zoomed would be invisible, so unzoom first.
        self.zoomed = None;
        if self.root.split(self.active, dir, new, ratio) {
            self.focus_on(new);
            true
        } else {
            false
        }
    }

    /// Returns false when this was the window's last pane: the caller decides
    /// whether that closes the window.
    pub fn close(&mut self, pane: PaneId) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        let rects = self.rects(CellSize::default());
        if !self.root.close(pane) {
            return false;
        }
        if self.zoomed == Some(pane) {
            self.zoomed = None;
        }
        if self.active == pane {
            // Focus the pane that was nearest, falling back to the first.
            let next = crate::layout::neighbour(&rects, pane, Edge::Left)
                .or_else(|| crate::layout::neighbour(&rects, pane, Edge::Right))
                .filter(|p| self.root.contains(*p))
                .or_else(|| self.panes().first().copied())
                .unwrap_or(pane);
            self.focus_on(next);
        }
        // A pane that is gone is not somewhere to go back to.
        if self.last == Some(pane) {
            self.last = None;
        }
        true
    }

    pub fn focus(&mut self, edge: Edge, size: CellSize) -> bool {
        // Movement inside a zoomed pane is meaningless; unzoom and move.
        if self.zoomed.is_some() {
            self.zoomed = None;
        }
        match crate::layout::neighbour(&self.rects(size), self.active, edge) {
            Some(pane) => {
                self.focus_on(pane);
                true
            }
            None => false,
        }
    }

    pub fn focus_pane(&mut self, pane: PaneId) -> bool {
        if self.root.contains(pane) {
            self.focus_on(pane);
            true
        } else {
            false
        }
    }

    /// Back to the pane focus was on before. Does nothing if it has closed.
    pub fn last_pane(&mut self) -> bool {
        match self.last.filter(|p| self.root.contains(*p)) {
            Some(pane) => {
                self.focus_on(pane);
                true
            }
            None => false,
        }
    }

    /// The one place focus moves, so `last` can never miss a move.
    fn focus_on(&mut self, pane: PaneId) {
        if pane == self.active {
            return;
        }
        self.last = Some(self.active);
        self.active = pane;
    }

    pub fn toggle_zoom(&mut self) {
        self.zoomed = match self.zoomed {
            Some(_) => None,
            // Zooming a lone pane is a no-op, not an error.
            None if self.pane_count() > 1 => Some(self.active),
            None => None,
        };
    }

    pub fn resize_active(&mut self, edge: Edge, fraction: f32) -> bool {
        self.root.resize(self.active, edge, fraction)
    }

    /// Re-lay the window out with one of the named layouts.
    pub fn apply_named_layout(&mut self, layout: NamedLayout) {
        let panes = self.panes();
        let root = match layout {
            NamedLayout::EvenHorizontal => PaneNode::even(&panes, Direction::Columns),
            NamedLayout::EvenVertical => PaneNode::even(&panes, Direction::Rows),
            NamedLayout::Tiled => PaneNode::tiled(&panes),
        };
        if let Some(root) = root {
            self.root = root;
            self.zoomed = None;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NamedLayout {
    EvenHorizontal,
    EvenVertical,
    Tiled,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(cols: u16, rows: u16) -> CellSize {
        CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
    }

    fn window() -> Window {
        Window::new(WindowId(0), PaneId(1))
    }

    #[test]
    fn a_new_window_has_one_full_size_pane() {
        let w = window();
        let rects = w.rects(size(80, 24));
        assert_eq!(rects.len(), 1);
        assert_eq!((rects[0].cols, rects[0].rows), (80, 24));
        assert_eq!(w.active, PaneId(1));
    }

    #[test]
    fn splitting_focuses_the_new_pane() {
        let mut w = window();
        assert!(w.split(Direction::Columns, PaneId(2), 0.5));
        assert_eq!(w.active, PaneId(2));
        assert_eq!(w.pane_count(), 2);
    }

    #[test]
    fn zoom_is_lossless() {
        let mut w = window();
        w.split(Direction::Columns, PaneId(2), 0.5);
        w.split(Direction::Rows, PaneId(3), 0.5);
        let before = w.rects(size(80, 24));

        w.toggle_zoom();
        let zoomed = w.rects(size(80, 24));
        assert_eq!(zoomed.len(), 1, "a zoomed window shows one pane");
        assert_eq!((zoomed[0].cols, zoomed[0].rows), (80, 24));

        w.toggle_zoom();
        assert_eq!(w.rects(size(80, 24)), before, "unzoom must restore exactly");
    }

    #[test]
    fn zooming_a_single_pane_does_nothing() {
        let mut w = window();
        w.toggle_zoom();
        assert_eq!(w.zoomed, None);
    }

    #[test]
    fn splitting_while_zoomed_unzooms_first() {
        let mut w = window();
        w.split(Direction::Columns, PaneId(2), 0.5);
        w.toggle_zoom();
        assert!(w.zoomed.is_some());
        w.split(Direction::Rows, PaneId(3), 0.5);
        assert_eq!(w.zoomed, None, "the new pane would otherwise be invisible");
        assert_eq!(w.rects(size(80, 24)).len(), 3);
    }

    #[test]
    fn closing_the_active_pane_moves_focus_to_a_neighbour() {
        let mut w = window();
        w.split(Direction::Columns, PaneId(2), 0.5);
        assert_eq!(w.active, PaneId(2));
        assert!(w.close(PaneId(2)));
        assert_eq!(w.active, PaneId(1));
        assert_eq!(w.pane_count(), 1);
    }

    #[test]
    fn the_last_pane_cannot_be_closed_by_the_window_itself() {
        let mut w = window();
        assert!(!w.close(PaneId(1)), "closing the last pane is the session's decision");
    }

    #[test]
    fn closing_a_zoomed_pane_clears_the_zoom() {
        let mut w = window();
        w.split(Direction::Columns, PaneId(2), 0.5);
        w.toggle_zoom();
        w.close(PaneId(2));
        assert_eq!(w.zoomed, None);
    }

    #[test]
    fn named_layouts_rearrange_without_losing_panes() {
        let mut w = window();
        w.split(Direction::Columns, PaneId(2), 0.5);
        w.split(Direction::Rows, PaneId(3), 0.5);
        w.split(Direction::Columns, PaneId(4), 0.5);

        w.apply_named_layout(NamedLayout::EvenVertical);
        let rects = w.rects(size(80, 24));
        assert_eq!(rects.len(), 4);
        assert!(rects.iter().all(|r| r.cols == 80), "even-vertical means full-width rows");

        w.apply_named_layout(NamedLayout::Tiled);
        assert_eq!(w.rects(size(80, 24)).len(), 4);
        let mut panes = w.panes();
        panes.sort_by_key(|p| p.0);
        assert_eq!(panes, vec![PaneId(1), PaneId(2), PaneId(3), PaneId(4)]);
    }

    #[test]
    fn focus_movement_unzooms_rather_than_moving_invisibly() {
        let mut w = window();
        w.split(Direction::Columns, PaneId(2), 0.5);
        w.toggle_zoom();
        assert!(w.focus(Edge::Left, size(80, 24)));
        assert_eq!(w.zoomed, None);
        assert_eq!(w.active, PaneId(1));
    }
}

#[cfg(test)]
mod last_pane_tests {
    use super::*;

    #[test]
    fn last_pane_is_a_toggle_between_two_panes() {
        let mut w = Window::new(WindowId(1), PaneId(1));
        w.split(Direction::Columns, PaneId(2), 0.5);
        assert_eq!(w.active, PaneId(2), "a split focuses the new pane");
        assert!(w.last_pane());
        assert_eq!(w.active, PaneId(1));
        assert!(w.last_pane());
        assert_eq!(w.active, PaneId(2), "it must toggle, not stick");
    }

    #[test]
    fn a_closed_pane_is_not_somewhere_to_go_back_to() {
        let mut w = Window::new(WindowId(1), PaneId(1));
        w.split(Direction::Columns, PaneId(2), 0.5);
        // Focus 1, so 2 is the pane we came from, then close 2.
        w.focus_pane(PaneId(1));
        w.close(PaneId(2));
        assert!(!w.last_pane(), "focus moved to a pane that is gone");
        assert_eq!(w.active, PaneId(1));
    }

    #[test]
    fn focusing_the_pane_already_focused_does_not_lose_the_last_one() {
        let mut w = Window::new(WindowId(1), PaneId(1));
        w.split(Direction::Columns, PaneId(2), 0.5);
        // A redundant focus must not make "last" point at the current pane,
        // or the toggle would stop working.
        w.focus_pane(PaneId(2));
        assert!(w.last_pane());
        assert_eq!(w.active, PaneId(1));
    }
}
