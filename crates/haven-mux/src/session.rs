//! Sessions: a list of windows, an active one, and the id allocator.

use haven_proto::{CellSize, Layout, PaneId, SessionId, WindowId};

use crate::layout::{Direction, Edge};
use crate::window::{NamedLayout, Window};

/// What a mutation implies for the caller, which owns the ptys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionEvent {
    /// Start a child for this pane.
    PaneOpened(PaneId),
    /// Tear down this pane's child.
    PaneClosed(PaneId),
    WindowClosed(WindowId),
    /// The last window closed: the session is over.
    SessionEnded,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub windows: Vec<Window>,
    pub active: usize,
    next_pane: u32,
    next_window: u32,
}

impl Session {
    /// A session always starts with one window holding one pane: there is no
    /// valid empty state, so the type never has to represent one.
    pub fn new(id: SessionId, name: impl Into<String>) -> (Self, PaneId) {
        let pane = PaneId(1);
        let window = Window::new(WindowId(1), pane);
        let session = Self {
            id,
            name: name.into(),
            windows: vec![window],
            active: 0,
            next_pane: 2,
            next_window: 2,
        };
        (session, pane)
    }

    fn alloc_pane(&mut self) -> PaneId {
        let id = PaneId(self.next_pane);
        self.next_pane += 1;
        id
    }

    pub fn active_window(&self) -> &Window {
        &self.windows[self.active]
    }

    pub fn active_window_mut(&mut self) -> &mut Window {
        &mut self.windows[self.active]
    }

    pub fn active_pane(&self) -> PaneId {
        self.active_window().active
    }

    pub fn panes(&self) -> Vec<PaneId> {
        self.windows.iter().flat_map(|w| w.panes()).collect()
    }

    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn layout(&self, size: CellSize) -> Layout {
        self.active_window().layout(size)
    }

    pub fn new_window(&mut self) -> (WindowId, PaneId, SessionEvent) {
        let pane = self.alloc_pane();
        let id = WindowId(self.next_window);
        self.next_window += 1;
        self.windows.push(Window::new(id, pane));
        self.active = self.windows.len() - 1;
        (id, pane, SessionEvent::PaneOpened(pane))
    }

    pub fn split_active(&mut self, dir: Direction, ratio: f32) -> Option<(PaneId, SessionEvent)> {
        let pane = self.alloc_pane();
        if self.active_window_mut().split(dir, pane, ratio) {
            Some((pane, SessionEvent::PaneOpened(pane)))
        } else {
            // Hand the id back rather than leaking it.
            self.next_pane -= 1;
            None
        }
    }

    /// Close a pane, collapsing the window and then the session if that was
    /// the last of each.
    pub fn close_pane(&mut self, pane: PaneId) -> Vec<SessionEvent> {
        let Some(index) = self.windows.iter().position(|w| w.panes().contains(&pane)) else {
            return Vec::new();
        };
        let mut events = vec![SessionEvent::PaneClosed(pane)];

        if self.windows[index].close(pane) {
            return events;
        }

        // That was the window's last pane.
        let window_id = self.windows[index].id;
        self.windows.remove(index);
        events.push(SessionEvent::WindowClosed(window_id));
        if self.windows.is_empty() {
            events.push(SessionEvent::SessionEnded);
            self.active = 0;
            return events;
        }
        self.active = self.active.min(self.windows.len() - 1);
        events
    }

    pub fn select_window(&mut self, index: usize) -> bool {
        if index < self.windows.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    pub fn next_window(&mut self) {
        if !self.windows.is_empty() {
            self.active = (self.active + 1) % self.windows.len();
        }
    }

    pub fn previous_window(&mut self) {
        if !self.windows.is_empty() {
            self.active = (self.active + self.windows.len() - 1) % self.windows.len();
        }
    }

    pub fn focus(&mut self, edge: Edge, size: CellSize) -> bool {
        self.active_window_mut().focus(edge, size)
    }

    pub fn focus_pane(&mut self, pane: PaneId) -> bool {
        // Focusing a pane in another window switches to that window, which is
        // what `haven select-pane -t` has to do.
        if let Some(index) = self.windows.iter().position(|w| w.panes().contains(&pane)) {
            self.active = index;
            return self.windows[index].focus_pane(pane);
        }
        false
    }

    pub fn toggle_zoom(&mut self) {
        self.active_window_mut().toggle_zoom();
    }

    pub fn resize_active(&mut self, edge: Edge, fraction: f32) -> bool {
        self.active_window_mut().resize_active(edge, fraction)
    }

    pub fn apply_named_layout(&mut self, layout: NamedLayout) {
        self.active_window_mut().apply_named_layout(layout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(cols: u16, rows: u16) -> CellSize {
        CellSize { cols, rows, pixel_width: 0, pixel_height: 0 }
    }

    fn session() -> Session {
        Session::new(SessionId(1), "work").0
    }

    #[test]
    fn a_new_session_has_exactly_one_window_and_pane() {
        let s = session();
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.panes(), vec![PaneId(1)]);
        assert_eq!(s.active_pane(), PaneId(1));
    }

    #[test]
    fn pane_ids_are_never_reused_and_never_leaked() {
        let mut s = session();
        let (a, _) = s.split_active(Direction::Columns, 0.5).unwrap();
        let (b, _) = s.split_active(Direction::Rows, 0.5).unwrap();
        assert_ne!(a, b);
        s.close_pane(b);
        let (c, _) = s.split_active(Direction::Columns, 0.5).unwrap();
        assert!(c.0 > b.0, "ids move forward so stale references cannot alias");
    }

    #[test]
    fn splitting_reports_the_pane_to_start_a_child_for() {
        let mut s = session();
        let (pane, event) = s.split_active(Direction::Columns, 0.5).unwrap();
        assert_eq!(event, SessionEvent::PaneOpened(pane));
        assert_eq!(s.active_pane(), pane);
    }

    #[test]
    fn closing_the_last_pane_of_the_last_window_ends_the_session() {
        let mut s = session();
        let events = s.close_pane(PaneId(1));
        assert_eq!(
            events,
            vec![
                SessionEvent::PaneClosed(PaneId(1)),
                SessionEvent::WindowClosed(WindowId(1)),
                SessionEvent::SessionEnded,
            ]
        );
    }

    #[test]
    fn closing_a_windows_last_pane_closes_the_window_but_not_the_session() {
        let mut s = session();
        let (_, pane, _) = s.new_window();
        let events = s.close_pane(pane);
        assert!(events.contains(&SessionEvent::WindowClosed(WindowId(2))));
        assert!(!events.contains(&SessionEvent::SessionEnded));
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.active, 0, "focus must land on a window that still exists");
    }

    #[test]
    fn window_selection_wraps_in_both_directions() {
        let mut s = session();
        s.new_window();
        s.new_window();
        assert_eq!(s.active, 2);
        s.next_window();
        assert_eq!(s.active, 0);
        s.previous_window();
        assert_eq!(s.active, 2);
        assert!(s.select_window(1));
        assert!(!s.select_window(9), "out-of-range selection is refused, not clamped");
        assert_eq!(s.active, 1);
    }

    #[test]
    fn focusing_a_pane_in_another_window_switches_window() {
        let mut s = session();
        let (_, other, _) = s.new_window();
        s.select_window(0);
        assert!(s.focus_pane(other));
        assert_eq!(s.active, 1);
        assert_eq!(s.active_pane(), other);
    }

    #[test]
    fn closing_an_unknown_pane_is_a_no_op() {
        let mut s = session();
        assert!(s.close_pane(PaneId(99)).is_empty());
    }

    #[test]
    fn the_layout_sent_to_a_client_covers_every_pane_once() {
        let mut s = session();
        s.split_active(Direction::Columns, 0.5);
        s.split_active(Direction::Rows, 0.5);
        let layout = s.layout(size(80, 24));
        let mut panes: Vec<u32> = layout.panes.iter().map(|r| r.pane.0).collect();
        panes.sort_unstable();
        assert_eq!(panes, vec![1, 2, 3]);
        assert_eq!(layout.active, Some(s.active_pane()));
    }
}
