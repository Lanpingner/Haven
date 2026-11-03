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
    /// The window before this one, by id rather than index: indices shift
    /// when a window closes and would point at a different window.
    last_window: Option<WindowId>,
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
            last_window: None,
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
        // Through `go_to`, so a new tab is somewhere `last-window` can come
        // back from — which is what makes it useful right after making one.
        self.go_to(self.windows.len() - 1);
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
            self.go_to(index);
            true
        } else {
            false
        }
    }

    pub fn next_window(&mut self) {
        if !self.windows.is_empty() {
            self.go_to((self.active + 1) % self.windows.len());
        }
    }

    pub fn previous_window(&mut self) {
        if !self.windows.is_empty() {
            self.go_to((self.active + self.windows.len() - 1) % self.windows.len());
        }
    }

    /// Back to the window you came from. Does nothing if that window has
    /// since closed, rather than guessing at a replacement.
    pub fn last_window(&mut self) -> bool {
        let Some(id) = self.last_window else { return false };
        let Some(index) = self.windows.iter().position(|w| w.id == id) else { return false };
        self.go_to(index);
        true
    }

    /// Move to a window, remembering where we were.
    fn go_to(&mut self, index: usize) {
        if index == self.active {
            return;
        }
        self.last_window = self.windows.get(self.active).map(|w| w.id);
        self.active = index;
        // Arriving is what clears the bell: it means "rang since you last
        // looked", and you are looking.
        if let Some(window) = self.windows.get_mut(index) {
            window.bell = false;
        }
    }

    /// Mark the window holding a pane as having rung.
    pub fn note_bell(&mut self, pane: PaneId) {
        let active = self.active;
        for (i, window) in self.windows.iter_mut().enumerate() {
            if window.root.contains(pane) {
                window.bell = i != active;
            }
        }
    }

    /// Rename a window, or the session itself when `window` is `None`.
    pub fn rename(&mut self, window: Option<WindowId>, name: &str) {
        let name = name.trim();
        match window {
            None if !name.is_empty() => self.name = name.to_string(),
            None => {}
            Some(id) => {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                    // An empty name gives the window back to the command it
                    // is running, which is where the name came from.
                    w.title = (!name.is_empty()).then(|| name.to_string());
                }
            }
        }
    }

    /// Exchange the active pane with the one in a direction, or with the
    /// next one in the tree when no direction is given.
    pub fn swap_pane(&mut self, edge: Option<Edge>, size: CellSize) -> bool {
        let window = self.active_window();
        let active = window.active;
        let other = match edge {
            Some(edge) => crate::layout::neighbour(&window.rects(size), active, edge),
            None => {
                let panes = window.panes();
                let at = panes.iter().position(|p| *p == active).unwrap_or(0);
                panes.get((at + 1) % panes.len().max(1)).copied()
            }
        };
        let Some(other) = other.filter(|p| *p != active) else { return false };
        let window = self.active_window_mut();
        if !window.root.swap(active, other) {
            return false;
        }
        // Focus follows the pane, not the position: you asked to move *this*
        // pane, so it is still the one you are typing into.
        window.active = active;
        true
    }

    pub fn rotate_window(&mut self, backwards: bool) -> bool {
        self.active_window_mut().root.rotate(backwards)
    }

    /// Move the active pane into a window of its own. Returns the new window
    /// and the pane, or `None` when the window has only this pane — breaking
    /// the only pane out of a window would leave an empty one behind.
    pub fn break_pane(&mut self) -> Option<(WindowId, PaneId)> {
        let pane = self.active_pane();
        if self.active_window().pane_count() <= 1 {
            return None;
        }
        self.active_window_mut().close(pane);
        let id = WindowId(self.next_window);
        self.next_window += 1;
        self.windows.push(Window::new(id, pane));
        self.go_to(self.windows.len() - 1);
        Some((id, pane))
    }

    /// Move the only pane of another window into this one, splitting the
    /// active pane. Returns the window that emptied, for the caller to drop.
    pub fn join_pane(&mut self, from: WindowId, dir: Direction) -> Option<WindowId> {
        let index = self.windows.iter().position(|w| w.id == from)?;
        if index == self.active {
            return None;
        }
        let panes = self.windows[index].panes();
        let [pane] = panes[..] else {
            // Only a single-pane window can be joined whole; anything else
            // would need to decide what happens to the rest of its tree.
            return None;
        };
        if !self.active_window_mut().split(dir, pane, 0.5) {
            return None;
        }
        self.windows.remove(index);
        if self.active > index {
            self.active -= 1;
        }
        if self.last_window == Some(from) {
            self.last_window = None;
        }
        Some(from)
    }

    /// Close a whole window and everything in it. Returns its panes, so the
    /// caller can hang them up, and whether the session is now empty.
    pub fn close_window(&mut self, id: WindowId) -> (Vec<PaneId>, bool) {
        let Some(index) = self.windows.iter().position(|w| w.id == id) else {
            return (Vec::new(), false);
        };
        let panes = self.windows[index].panes();
        self.windows.remove(index);
        if self.last_window == Some(id) {
            self.last_window = None;
        }
        if self.windows.is_empty() {
            return (panes, true);
        }
        self.active = self.active.min(self.windows.len() - 1);
        (panes, false)
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

#[cfg(test)]
mod rearrange_tests {
    use super::*;

    fn session() -> Session {
        Session::new(SessionId(1), "work").0
    }

    #[test]
    fn breaking_a_pane_out_makes_a_window_and_moves_to_it() {
        let mut s = session();
        s.split_active(Direction::Columns, 0.5);
        let moved = s.active_pane();
        let (id, pane) = s.break_pane().expect("two panes, so one can leave");
        assert_eq!(pane, moved);
        assert_eq!(s.windows.len(), 2);
        assert_eq!(s.active_window().id, id, "should follow the pane out");
        assert_eq!(s.windows[0].pane_count(), 1, "the pane did not leave its old window");
    }

    #[test]
    fn the_only_pane_of_a_window_cannot_be_broken_out() {
        // It would leave an empty window behind, which the type cannot hold.
        let mut s = session();
        assert!(s.break_pane().is_none());
        assert_eq!(s.windows.len(), 1);
    }

    #[test]
    fn joining_brings_a_window_in_and_removes_it() {
        let mut s = session();
        let (id, _, _) = s.new_window();
        s.select_window(0);
        assert_eq!(s.join_pane(id, Direction::Columns), Some(id));
        assert_eq!(s.windows.len(), 1, "the emptied window should be gone");
        assert_eq!(s.active_window().pane_count(), 2);
    }

    #[test]
    fn a_window_cannot_be_joined_into_itself() {
        let mut s = session();
        let id = s.active_window().id;
        assert_eq!(s.join_pane(id, Direction::Columns), None);
    }

    #[test]
    fn a_window_with_several_panes_cannot_be_joined_whole() {
        // What would happen to the rest of its tree is not obvious, so it is
        // refused rather than guessed at.
        let mut s = session();
        let (id, _, _) = s.new_window();
        s.split_active(Direction::Columns, 0.5);
        s.select_window(0);
        assert_eq!(s.join_pane(id, Direction::Columns), None);
    }

    #[test]
    fn closing_a_window_returns_its_panes_for_hangup() {
        let mut s = session();
        s.new_window();
        s.split_active(Direction::Rows, 0.5);
        let id = s.active_window().id;
        let (panes, ended) = s.close_window(id);
        assert_eq!(panes.len(), 2, "both panes must be handed back to be hung up");
        assert!(!ended);
        assert_eq!(s.windows.len(), 1);
    }

    #[test]
    fn closing_the_last_window_ends_the_session() {
        let mut s = session();
        let id = s.active_window().id;
        let (_, ended) = s.close_window(id);
        assert!(ended);
    }

    #[test]
    fn swapping_keeps_focus_on_the_pane_that_moved() {
        // You asked to move *this* pane; it is still the one you are typing
        // into, which is the opposite of what focusing a position would do.
        let mut s = session();
        s.split_active(Direction::Columns, 0.5);
        let moved = s.active_pane();
        assert!(
            s.swap_pane(Some(Edge::Left), CellSize { cols: 80, rows: 24, ..Default::default() })
        );
        assert_eq!(s.active_pane(), moved);
        assert_eq!(s.active_window().panes()[0], moved, "it did not change places");
    }
}

#[cfg(test)]
mod last_and_rename_tests {
    use super::*;

    fn session() -> Session {
        Session::new(SessionId(1), "work").0
    }

    #[test]
    fn last_window_goes_back_where_you_came_from() {
        let mut s = session();
        s.new_window();
        s.new_window();
        assert_eq!(s.active, 2);
        s.select_window(0);
        assert!(s.last_window(), "nothing to go back to");
        assert_eq!(s.active, 2, "did not return to the window we left");
        // And again, which is what makes it a toggle.
        s.last_window();
        assert_eq!(s.active, 0);
    }

    #[test]
    fn last_window_is_remembered_by_id_not_by_index() {
        // Closing a window shifts every index after it; an index would point
        // at a different window, which is a silent jump to the wrong place.
        let mut s = session();
        s.new_window();
        s.new_window();
        s.select_window(0); // last = window 3
        s.windows.remove(1); // window 2 closes; 3 is now at index 1
        s.active = 0;
        assert!(s.last_window());
        assert_eq!(s.active_window().id, WindowId(3), "landed on the wrong window");
    }

    #[test]
    fn a_window_that_closed_is_not_somewhere_to_go_back_to() {
        let mut s = session();
        s.new_window();
        s.select_window(0); // last = window 2
        s.windows.retain(|w| w.id != WindowId(2));
        s.active = 0;
        assert!(!s.last_window(), "went back to a window that no longer exists");
    }

    #[test]
    fn renaming_a_window_overrides_the_command_name_and_empty_gives_it_back() {
        let mut s = session();
        let id = s.active_window().id;
        s.rename(Some(id), "build");
        assert_eq!(s.active_window().title.as_deref(), Some("build"));
        s.rename(Some(id), "  ");
        assert_eq!(s.active_window().title, None, "an empty name should restore the default");
    }

    #[test]
    fn renaming_the_session_ignores_an_empty_name() {
        let mut s = session();
        s.rename(None, "deploy");
        assert_eq!(s.name, "deploy");
        s.rename(None, "");
        assert_eq!(s.name, "deploy", "a session with no name is not usable");
    }
}
