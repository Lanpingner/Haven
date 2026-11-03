//! What one client has already been told.
//!
//! The daemon is frontend-blind, so synchronisation is purely a function of
//! what each client last acknowledged — not of what any frontend needs.
//! Per-row revisions (rather than a dirty flag) are what make this correct
//! with several clients attached at once: each one tracks its own position in
//! the pane's history (`docs/PLAN.md` §6).

use std::collections::BTreeMap;

use haven_proto::{Layout, PaneId};

#[derive(Debug, Default)]
pub struct PaneSync {
    /// Revision last sent, per row.
    pub rows: Vec<u32>,
    /// How many style and cluster entries this client knows about.
    pub styles_sent: u16,
    pub clusters_sent: u16,
    pub cursor: Option<haven_core::Cursor>,
    pub title: Option<String>,
    pub modes: Option<crate::PaneModes>,
    /// False until the client has had a full snapshot.
    pub snapshotted: bool,
    /// How far back this client is reading, in rows above the live bottom.
    pub offset: usize,
    /// Rows of history when the view was last sent. Row revisions are
    /// per-row counters, so two different rows can share one: comparing them
    /// by index is only sound while the view sits still. When history grows
    /// under a scrolled view, every index means a different row, so the
    /// whole thing is resent rather than diffed.
    pub anchor: usize,
    /// What the client was last told its position is.
    pub reported: Option<(u32, u32)>,
}

#[derive(Debug, Default)]
pub struct SyncState {
    panes: BTreeMap<u32, PaneSync>,
    layout: Option<Layout>,
}

impl SyncState {
    pub fn pane(&mut self, pane: PaneId) -> &mut PaneSync {
        self.panes.entry(pane.0).or_default()
    }

    /// Returns true when the layout differs from what the client has.
    pub fn layout_changed(&mut self, layout: &Layout) -> bool {
        if self.layout.as_ref() == Some(layout) {
            return false;
        }
        self.layout = Some(layout.clone());
        true
    }

    /// Forget a pane, so a reused id can never inherit stale row revisions.
    pub fn forget(&mut self, pane: PaneId) {
        self.panes.remove(&pane.0);
    }

    /// Start again from scratch: used on resize and on resync.
    pub fn invalidate(&mut self) {
        self.panes.clear();
        self.layout = None;
    }

    pub fn known_panes(&self) -> Vec<PaneId> {
        self.panes.keys().map(|id| PaneId(*id)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_proto::{PaneRect, SessionId};

    fn layout(panes: &[u32]) -> Layout {
        let _ = SessionId(1);
        Layout {
            window: 0,
            panes: panes
                .iter()
                .map(|&p| PaneRect { pane: PaneId(p), col: 0, row: 0, cols: 10, rows: 5 })
                .collect(),
            active: panes.first().map(|&p| PaneId(p)),
        }
    }

    #[test]
    fn the_layout_is_only_resent_when_it_changes() {
        let mut sync = SyncState::default();
        assert!(sync.layout_changed(&layout(&[1])), "the first layout is always news");
        assert!(!sync.layout_changed(&layout(&[1])), "an identical layout costs nothing");
        assert!(sync.layout_changed(&layout(&[1, 2])));
    }

    #[test]
    fn forgetting_a_pane_drops_its_row_history() {
        let mut sync = SyncState::default();
        sync.pane(PaneId(3)).rows = vec![1, 2, 3];
        sync.forget(PaneId(3));
        assert!(sync.pane(PaneId(3)).rows.is_empty(), "a reused id must not inherit revisions");
    }

    #[test]
    fn invalidating_forces_a_fresh_snapshot() {
        let mut sync = SyncState::default();
        sync.pane(PaneId(1)).snapshotted = true;
        sync.layout_changed(&layout(&[1]));
        sync.invalidate();
        assert!(!sync.pane(PaneId(1)).snapshotted);
        assert!(sync.layout_changed(&layout(&[1])));
    }
}
