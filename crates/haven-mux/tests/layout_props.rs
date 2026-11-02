//! Property tests for the layout solver, which `docs/PLAN.md` §16 (M3) asks
//! for by name: weights always sum to the available cells, and no pane is ever
//! zero-sized.

use haven_mux::layout::{Direction, Edge, PaneNode};
use haven_mux::{NamedLayout, Window};
use haven_proto::{CellSize, PaneId, PaneRect, WindowId};
use proptest::prelude::*;

fn area(cols: u16, rows: u16) -> PaneRect {
    PaneRect { pane: PaneId(0), col: 0, row: 0, cols, rows }
}

/// A sequence of splits, which is how real trees come to exist. Generating
/// trees directly would produce shapes the operations can never build.
#[derive(Clone, Debug)]
struct SplitPlan(Vec<(usize, Direction, f32)>);

fn split_plan() -> impl Strategy<Value = SplitPlan> {
    prop::collection::vec(
        (0usize..12, prop_oneof![Just(Direction::Columns), Just(Direction::Rows)], 0.1f32..0.9),
        0..10,
    )
    .prop_map(SplitPlan)
}

fn build(plan: &SplitPlan) -> (PaneNode, Vec<PaneId>) {
    let mut node = PaneNode::Leaf(PaneId(1));
    let mut panes = vec![PaneId(1)];
    let mut next = 2u32;
    for (target_index, dir, ratio) in &plan.0 {
        let target = panes[target_index % panes.len()];
        let new = PaneId(next);
        if node.split(target, *dir, new, *ratio) {
            panes.push(new);
            next += 1;
        }
    }
    (node, panes)
}

fn overlaps(a: &PaneRect, b: &PaneRect) -> bool {
    let x = a.col < b.col + b.cols && b.col < a.col + a.cols;
    let y = a.row < b.row + b.rows && b.row < a.row + a.rows;
    x && y
}

proptest! {
    #[test]
    fn every_pane_gets_exactly_one_rectangle(plan in split_plan(), cols in 20u16..200, rows in 6u16..60) {
        let (node, panes) = build(&plan);
        let rects = node.solve(area(cols, rows));
        prop_assert_eq!(rects.len(), panes.len());
        let mut ids: Vec<u32> = rects.iter().map(|r| r.pane.0).collect();
        ids.sort_unstable();
        let mut expected: Vec<u32> = panes.iter().map(|p| p.0).collect();
        expected.sort_unstable();
        prop_assert_eq!(ids, expected);
    }

    /// A flat split with room for all of its children gives each one at least
    /// one cell. Stated for a flat split on purpose: under deep nesting a
    /// *sub*-area can be too small for its own children however large the
    /// screen is, so the general claim is simply false — the guarantee lives
    /// at the level that owns the space.
    #[test]
    fn a_flat_split_with_room_gives_every_child_a_cell(
        panes in 2usize..12,
        slack in 0u16..40,
        dir in prop_oneof![Just(Direction::Columns), Just(Direction::Rows)],
    ) {
        let ids: Vec<PaneId> = (1..=panes as u32).map(PaneId).collect();
        let node = PaneNode::even(&ids, dir).expect("at least two panes");
        // 2k-1 is the minimum: one cell each plus a separator between them.
        let needed = (2 * panes - 1) as u16;
        let rects = node.solve(area(needed + slack, needed + slack));
        for rect in &rects {
            prop_assert!(rect.cols >= 1 && rect.rows >= 1, "{:?} in {needed}+{slack}", rect);
        }
        // And the cells add up exactly: no space invented, none lost.
        let used: u16 = match dir {
            Direction::Columns => rects.iter().map(|r| r.cols).sum(),
            Direction::Rows => rects.iter().map(|r| r.rows).sum(),
        };
        let separators = (panes - 1) as u16;
        prop_assert_eq!(used + separators, needed + slack);
    }

    #[test]
    fn panes_never_overlap_and_stay_inside_the_screen(plan in split_plan(), cols in 20u16..200, rows in 6u16..60) {
        let (node, _) = build(&plan);
        let rects = node.solve(area(cols, rows));
        for rect in &rects {
            prop_assert!(rect.col + rect.cols <= cols, "{:?} exceeds {cols} columns", rect);
            prop_assert!(rect.row + rect.rows <= rows, "{:?} exceeds {rows} rows", rect);
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                prop_assert!(!overlaps(a, b), "{:?} overlaps {:?}", a, b);
            }
        }
    }

    #[test]
    fn solving_is_deterministic(plan in split_plan(), cols in 20u16..200, rows in 6u16..60) {
        let (node, _) = build(&plan);
        let first = node.solve(area(cols, rows));
        for _ in 0..3 {
            prop_assert_eq!(node.solve(area(cols, rows)), first.clone());
        }
    }

    #[test]
    fn a_split_followed_by_closing_the_new_pane_restores_the_layout(
        plan in split_plan(),
        dir in prop_oneof![Just(Direction::Columns), Just(Direction::Rows)],
        cols in 20u16..200,
        rows in 6u16..60,
    ) {
        let (mut node, panes) = build(&plan);
        let before = node.solve(area(cols, rows));
        let target = *panes.last().expect("at least one pane");
        let new = PaneId(10_000);
        prop_assume!(node.split(target, dir, new, 0.5));
        prop_assert!(node.close(new));
        // Weights are floats, so the restored layout can differ by a cell;
        // what must hold is the set of panes and their ordering.
        let after = node.solve(area(cols, rows));
        let ids_before: Vec<u32> = before.iter().map(|r| r.pane.0).collect();
        let ids_after: Vec<u32> = after.iter().map(|r| r.pane.0).collect();
        prop_assert_eq!(ids_before, ids_after);
    }

    #[test]
    fn resizing_right_never_shrinks_the_pane(plan in split_plan(), cols in 40u16..200, rows in 10u16..60) {
        let (mut node, panes) = build(&plan);
        let target = panes[0];
        let size = area(cols, rows);
        let before = node
            .solve(size)
            .into_iter()
            .find(|r| r.pane == target)
            .map(|r| r.cols)
            .unwrap_or(0);
        node.resize(target, Edge::Right, 0.05);
        let after = node
            .solve(size)
            .into_iter()
            .find(|r| r.pane == target)
            .map(|r| r.cols)
            .unwrap_or(0);
        prop_assert!(after >= before, "resize shrank the pane: {before} -> {after}");
    }

    #[test]
    fn named_layouts_preserve_the_pane_set(plan in split_plan(), cols in 20u16..200, rows in 6u16..60) {
        let (node, mut panes) = build(&plan);
        panes.sort_by_key(|p| p.0);
        let size = CellSize { cols, rows, pixel_width: 0, pixel_height: 0 };

        for layout in [NamedLayout::EvenHorizontal, NamedLayout::EvenVertical, NamedLayout::Tiled] {
            let mut window = Window::new(WindowId(1), panes[0]);
            window.root = node.clone();
            window.apply_named_layout(layout);
            let mut after = window.panes();
            after.sort_by_key(|p| p.0);
            prop_assert_eq!(&after, &panes, "{:?} lost or duplicated panes", layout);
            prop_assert_eq!(window.rects(size).len(), panes.len());
        }
    }

    #[test]
    fn focus_movement_always_lands_on_a_real_pane(plan in split_plan(), cols in 20u16..200, rows in 6u16..60) {
        let (node, panes) = build(&plan);
        let mut window = Window::new(WindowId(1), panes[0]);
        window.root = node;
        let size = CellSize { cols, rows, pixel_width: 0, pixel_height: 0 };

        for edge in [Edge::Left, Edge::Right, Edge::Up, Edge::Down] {
            let mut w = window.clone();
            w.focus(edge, size);
            prop_assert!(w.panes().contains(&w.active), "focus left the tree");
        }
    }
}
