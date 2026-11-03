//! Pane trees and the layout solver. See `docs/PLAN.md` §10.
//!
//! N-ary splits with fractional weights, not a binary tree: "three even
//! columns" is then exact, and repeated resizes do not drift the way nested
//! binary splits do.

use haven_proto::{PaneId, PaneRect};

/// Which axis a split divides.
///
/// Named after what the children *are*, not after the line between them, which
/// is the ambiguity that makes `-h`/`-v` confusing in other tools.
/// `Columns` is tmux's `split-window -h`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Children side by side, divided along x.
    Columns,
    /// Children stacked, divided along y.
    Rows,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    Left,
    Right,
    Up,
    Down,
}

impl Edge {
    fn axis(self) -> Direction {
        match self {
            Self::Left | Self::Right => Direction::Columns,
            Self::Up | Self::Down => Direction::Rows,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Child {
    pub node: PaneNode,
    /// Relative share of the parent's space. Only ratios matter.
    pub weight: f32,
}

#[derive(Clone, PartialEq, Debug)]
pub enum PaneNode {
    Leaf(PaneId),
    Split { dir: Direction, children: Vec<Child> },
}

/// One cell of separator between siblings.
pub const SEPARATOR: u16 = 1;

impl PaneNode {
    pub fn leaf(id: u32) -> Self {
        Self::Leaf(PaneId(id))
    }

    /// Every pane in the tree, left-to-right then top-to-bottom.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Leaf(id) => out.push(*id),
            Self::Split { children, .. } => {
                for child in children {
                    child.node.collect(out);
                }
            }
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.panes().contains(&pane)
    }

    /// Exchange two panes' places in the tree, keeping both weights where
    /// they are: swapping is about *position*, so the layout must not move.
    pub fn swap(&mut self, a: PaneId, b: PaneId) -> bool {
        if a == b || !self.contains(a) || !self.contains(b) {
            return false;
        }
        self.relabel(a, b);
        true
    }

    fn relabel(&mut self, a: PaneId, b: PaneId) {
        match self {
            Self::Leaf(id) if *id == a => *id = b,
            Self::Leaf(id) if *id == b => *id = a,
            Self::Leaf(_) => {}
            Self::Split { children, .. } => {
                for child in children.iter_mut() {
                    child.node.relabel(a, b);
                }
            }
        }
    }

    /// Turn the tree inside out one level: the children of the outermost
    /// split move round by one. tmux calls it `rotate-window`.
    pub fn rotate(&mut self, backwards: bool) -> bool {
        let Self::Split { children, .. } = self else { return false };
        if children.len() < 2 {
            return false;
        }
        // The weights stay with their *positions*, not with their panes, so
        // rotating a wide-left layout keeps the wide slot on the left.
        let nodes: Vec<PaneNode> = children.iter().map(|c| c.node.clone()).collect();
        let n = nodes.len();
        for (i, child) in children.iter_mut().enumerate() {
            let from = if backwards { (i + 1) % n } else { (i + n - 1) % n };
            child.node = nodes[from].clone();
        }
        true
    }

    /// Solve the tree into cell rectangles.
    ///
    /// Rounding remainder is distributed deterministically from the first child
    /// onwards, so the same tree and size always produce the same rectangles —
    /// which the snapshot tests depend on, and which stops a pane jittering by
    /// a cell as unrelated panes resize.
    pub fn solve(&self, area: PaneRect) -> Vec<PaneRect> {
        let mut out = Vec::new();
        self.solve_into(area, &mut out);
        out
    }

    fn solve_into(&self, area: PaneRect, out: &mut Vec<PaneRect>) {
        match self {
            Self::Leaf(id) => out.push(PaneRect { pane: *id, ..area }),
            Self::Split { dir, children } => {
                if children.is_empty() {
                    return;
                }
                let spans = divide(area, *dir, children);
                for (child, span) in children.iter().zip(spans) {
                    child.node.solve_into(span, out);
                }
            }
        }
    }

    /// Replace `target` with a split containing it and `new`.
    ///
    /// When the target's parent already splits along the same axis, the new
    /// pane joins it as a sibling instead of nesting another level. That is
    /// what keeps repeated splits in one direction evenly divisible.
    pub fn split(&mut self, target: PaneId, dir: Direction, new: PaneId, ratio: f32) -> bool {
        let ratio = ratio.clamp(0.05, 0.95);
        if let Self::Split { dir: own_dir, children } = self {
            if *own_dir == dir {
                if let Some(index) = children.iter().position(|c| c.node == Self::Leaf(target)) {
                    let weight = children[index].weight;
                    children[index].weight = weight * (1.0 - ratio);
                    children
                        .insert(index + 1, Child { node: Self::Leaf(new), weight: weight * ratio });
                    return true;
                }
            }
            return children.iter_mut().any(|c| c.node.split(target, dir, new, ratio));
        }
        if *self == Self::Leaf(target) {
            *self = Self::Split {
                dir,
                children: vec![
                    Child { node: Self::Leaf(target), weight: 1.0 - ratio },
                    Child { node: Self::Leaf(new), weight: ratio },
                ],
            };
            return true;
        }
        false
    }

    /// Remove a pane, giving its space to its siblings and collapsing any
    /// split left with a single child.
    pub fn close(&mut self, target: PaneId) -> bool {
        let Self::Split { children, .. } = self else { return false };

        if let Some(index) = children.iter().position(|c| c.node == Self::Leaf(target)) {
            let freed = children.remove(index).weight;
            // Hand the space to the remaining siblings in proportion, so the
            // others keep their relative sizes.
            let total: f32 = children.iter().map(|c| c.weight).sum();
            if total > 0.0 {
                for child in children.iter_mut() {
                    child.weight += freed * (child.weight / total);
                }
            }
        } else if !children.iter_mut().any(|c| c.node.close(target)) {
            return false;
        }

        // Collapse: a split with one child is just that child.
        if children.len() == 1 {
            let only = children.remove(0).node;
            *self = only;
        }
        true
    }

    /// Grow or shrink a pane along an axis, taking from the neighbour that
    /// shares the boundary. Returns false when no boundary exists that way.
    pub fn resize(&mut self, target: PaneId, edge: Edge, fraction: f32) -> bool {
        let Self::Split { dir, children } = self else { return false };
        if *dir == edge.axis() {
            if let Some(index) = children.iter().position(|c| c.node.contains(target)) {
                let neighbour = match edge {
                    Edge::Right | Edge::Down => index + 1,
                    Edge::Left | Edge::Up => index.checked_sub(1).unwrap_or(usize::MAX),
                };
                if neighbour < children.len() {
                    // Never let a pane be squeezed out of existence.
                    let take = fraction.min(children[neighbour].weight * 0.9);
                    children[index].weight += take;
                    children[neighbour].weight -= take;
                    return true;
                }
            }
        }
        children.iter_mut().any(|c| c.node.resize(target, edge, fraction))
    }

    /// Rebuild as evenly-weighted children along one axis.
    pub fn even(panes: &[PaneId], dir: Direction) -> Option<Self> {
        match panes.len() {
            0 => None,
            1 => Some(Self::Leaf(panes[0])),
            _ => Some(Self::Split {
                dir,
                children: panes
                    .iter()
                    .map(|&pane| Child { node: Self::Leaf(pane), weight: 1.0 })
                    .collect(),
            }),
        }
    }

    /// A tmux-style `tiled` layout: rows of roughly equal columns.
    pub fn tiled(panes: &[PaneId]) -> Option<Self> {
        if panes.len() <= 1 {
            return panes.first().map(|&p| Self::Leaf(p));
        }
        let rows = (panes.len() as f64).sqrt().round().max(1.0) as usize;
        let per_row = panes.len().div_ceil(rows);
        let children: Vec<Child> = panes
            .chunks(per_row)
            .filter_map(|chunk| Self::even(chunk, Direction::Columns))
            .map(|node| Child { node, weight: 1.0 })
            .collect();
        Some(Self::Split { dir: Direction::Rows, children })
    }
}

/// Split one rectangle among weighted children.
///
/// When the area is too small for every child, the trailing children are given
/// zero size rather than pushed off the screen: a pane that cannot fit is not
/// drawn, which is what tmux does and what a frontend can actually render.
/// Found by the property test in `tests/layout_props.rs` — the earlier version
/// kept a one-cell floor per pane and happily laid panes out past the edge.
fn divide(area: PaneRect, dir: Direction, children: &[Child]) -> Vec<PaneRect> {
    let count = children.len();
    let dim = match dir {
        Direction::Columns => area.cols,
        Direction::Rows => area.rows,
    };

    // Each visible pane needs one cell, and each gap between two of them needs
    // a separator: `visible + (visible - 1) <= dim`.
    let visible = count.min((dim as usize).div_ceil(2)).max(usize::from(dim > 0));
    let separators = visible.saturating_sub(1) as u16;
    let available = dim.saturating_sub(separators);

    let shown = &children[..visible];
    let total_weight: f32 = shown.iter().map(|c| c.weight.max(0.0)).sum();
    let total_weight = if total_weight <= 0.0 { visible as f32 } else { total_weight };

    // Largest-remainder apportionment: floor every share, then give the spare
    // cells to whoever was rounded down hardest.
    //
    // Handing the remainder out from the first child onwards would also be
    // deterministic, but it biases the leftmost panes regardless of how close
    // they were to deserving the cell. Largest-remainder is the standard
    // answer and keeps sizes in the order the weights imply.
    let mut sizes: Vec<u16> = Vec::with_capacity(count);
    let mut remainders: Vec<(usize, f32)> = Vec::with_capacity(visible);
    for (index, child) in shown.iter().enumerate() {
        let exact = available as f32 * child.weight.max(0.0) / total_weight;
        let floor = exact.floor().max(0.0);
        sizes.push(floor as u16);
        remainders.push((index, exact - floor));
    }
    sizes.resize(count, 0);

    let mut assigned: u16 = sizes.iter().sum();
    // Largest remainder first; ties go to the earlier child, so the same tree
    // and size always produce the same rectangles.
    remainders.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut next = 0;
    while assigned < available && !remainders.is_empty() {
        sizes[remainders[next % remainders.len()].0] += 1;
        assigned += 1;
        next += 1;
    }

    // Every visible pane needs at least one cell; take from the largest.
    for index in 0..visible {
        if sizes[index] != 0 {
            continue;
        }
        let Some((donor, _)) =
            sizes[..visible].iter().enumerate().filter(|(_, s)| **s > 1).max_by_key(|(_, s)| **s)
        else {
            break;
        };
        sizes[donor] -= 1;
        sizes[index] += 1;
    }

    let mut out = Vec::with_capacity(count);
    let mut offset = 0u16;
    for size in sizes {
        // A hidden pane is parked at the far edge with zero size, so even its
        // degenerate rectangle stays inside the area.
        let at = if size == 0 { offset.min(dim) } else { offset };
        out.push(match dir {
            Direction::Columns => PaneRect { col: area.col + at, cols: size, ..area },
            Direction::Rows => PaneRect { row: area.row + at, rows: size, ..area },
        });
        if size > 0 {
            offset += size + SEPARATOR;
        }
    }
    out
}

/// The pane nearest `from` in a direction, chosen geometrically from the solved
/// rectangles.
///
/// Geometry rather than tree walking: with n-ary splits and zoom, "the pane to
/// the left" is a question about the screen, and users judge it by what they
/// see.
pub fn neighbour(rects: &[PaneRect], from: PaneId, edge: Edge) -> Option<PaneId> {
    let current = rects.iter().find(|r| r.pane == from)?;
    let (cx, cy) = (
        current.col as i32 + current.cols as i32 / 2,
        current.row as i32 + current.rows as i32 / 2,
    );

    rects
        .iter()
        .filter(|r| r.pane != from)
        .filter(|r| match edge {
            Edge::Left => (r.col + r.cols) <= current.col + 1,
            Edge::Right => r.col >= current.col + current.cols,
            Edge::Up => (r.row + r.rows) <= current.row + 1,
            Edge::Down => r.row >= current.row + current.rows,
        })
        .min_by_key(|r| {
            let (rx, ry) = (r.col as i32 + r.cols as i32 / 2, r.row as i32 + r.rows as i32 / 2);
            // Distance along the axis of travel dominates; the cross-axis
            // offset only breaks ties, so moving right picks the pane beside
            // you rather than one far above it.
            let (primary, cross) = match edge {
                Edge::Left | Edge::Right => ((rx - cx).abs(), (ry - cy).abs()),
                Edge::Up | Edge::Down => ((ry - cy).abs(), (rx - cx).abs()),
            };
            primary * 1000 + cross
        })
        .map(|r| r.pane)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(cols: u16, rows: u16) -> PaneRect {
        PaneRect { pane: PaneId(0), col: 0, row: 0, cols, rows }
    }

    fn rect_of(rects: &[PaneRect], id: u32) -> PaneRect {
        *rects.iter().find(|r| r.pane == PaneId(id)).expect("pane is in the layout")
    }

    #[test]
    fn a_single_pane_fills_the_area() {
        let rects = PaneNode::leaf(1).solve(area(80, 24));
        assert_eq!(rects.len(), 1);
        assert_eq!((rects[0].cols, rects[0].rows), (80, 24));
    }

    #[test]
    fn three_even_columns_divide_exactly() {
        // 80 columns, two separators: 78 cells across three panes.
        let node = PaneNode::even(&[PaneId(1), PaneId(2), PaneId(3)], Direction::Columns).unwrap();
        let rects = node.solve(area(80, 24));
        let widths: Vec<u16> = rects.iter().map(|r| r.cols).collect();
        assert_eq!(widths.iter().sum::<u16>(), 78);
        assert_eq!(widths, vec![26, 26, 26]);
        // And they tile without gaps or overlaps, separators aside.
        assert_eq!(rects[0].col, 0);
        assert_eq!(rects[1].col, 27);
        assert_eq!(rects[2].col, 54);
    }

    #[test]
    fn rounding_remainder_is_handed_out_deterministically() {
        let node = PaneNode::even(&[PaneId(1), PaneId(2), PaneId(3)], Direction::Columns).unwrap();
        // 82 columns, two separators: 80 across three is 26 remainder 2.
        let widths: Vec<u16> = node.solve(area(82, 24)).iter().map(|r| r.cols).collect();
        assert_eq!(widths, vec![27, 27, 26]);
        // Same input, same answer, every time.
        for _ in 0..5 {
            let again: Vec<u16> = node.solve(area(82, 24)).iter().map(|r| r.cols).collect();
            assert_eq!(again, widths);
        }
    }

    #[test]
    fn splitting_in_the_same_direction_extends_the_existing_split() {
        let mut node = PaneNode::leaf(1);
        node.split(PaneId(1), Direction::Columns, PaneId(2), 0.5);
        node.split(PaneId(2), Direction::Columns, PaneId(3), 0.5);
        // One three-way split, not a binary tree two levels deep.
        match &node {
            PaneNode::Split { children, .. } => assert_eq!(children.len(), 3),
            other => panic!("expected one split, got {other:?}"),
        }
    }

    #[test]
    fn splitting_across_directions_nests() {
        let mut node = PaneNode::leaf(1);
        node.split(PaneId(1), Direction::Columns, PaneId(2), 0.5);
        node.split(PaneId(2), Direction::Rows, PaneId(3), 0.5);
        let rects = node.solve(area(80, 24));
        assert_eq!(rects.len(), 3);
        // Pane 1 keeps the full height; 2 and 3 share the right-hand column.
        assert_eq!(rect_of(&rects, 1).rows, 24);
        assert_eq!(rect_of(&rects, 2).col, rect_of(&rects, 3).col);
        assert!(rect_of(&rects, 3).row > rect_of(&rects, 2).row);
    }

    #[test]
    fn closing_a_pane_gives_its_space_to_the_others() {
        let mut node =
            PaneNode::even(&[PaneId(1), PaneId(2), PaneId(3)], Direction::Columns).unwrap();
        assert!(node.close(PaneId(2)));
        let rects = node.solve(area(81, 24));
        assert_eq!(rects.len(), 2);
        assert_eq!(rects.iter().map(|r| r.cols).sum::<u16>(), 80);
    }

    #[test]
    fn closing_down_to_one_pane_collapses_the_split() {
        let mut node = PaneNode::even(&[PaneId(1), PaneId(2)], Direction::Rows).unwrap();
        node.close(PaneId(2));
        assert_eq!(node, PaneNode::Leaf(PaneId(1)), "a lone child must not stay wrapped");
    }

    #[test]
    fn resizing_takes_from_the_neighbour_that_shares_the_boundary() {
        let mut node = PaneNode::even(&[PaneId(1), PaneId(2)], Direction::Columns).unwrap();
        let before = rect_of(&node.solve(area(80, 24)), 1).cols;
        assert!(node.resize(PaneId(1), Edge::Right, 0.1));
        let after = node.solve(area(80, 24));
        assert!(rect_of(&after, 1).cols > before);
        assert_eq!(after.iter().map(|r| r.cols).sum::<u16>(), 79);
    }

    #[test]
    fn resizing_against_the_screen_edge_does_nothing() {
        let mut node = PaneNode::even(&[PaneId(1), PaneId(2)], Direction::Columns).unwrap();
        assert!(!node.resize(PaneId(1), Edge::Left, 0.1), "nothing to the left to take from");
        assert!(!node.resize(PaneId(1), Edge::Up, 0.1), "wrong axis entirely");
    }

    #[test]
    fn pressing_grow_repeatedly_visibly_grows_the_pane() {
        // The behaviour a user judges the resize key by, on a layout simple
        // enough for the answer to be unambiguous.
        let mut node = PaneNode::even(&[PaneId(1), PaneId(2)], Direction::Columns).unwrap();
        let width = |n: &PaneNode| rect_of(&n.solve(area(80, 24)), 1).cols;

        let start = width(&node);
        let mut last = start;
        for press in 1..=5 {
            node.resize(PaneId(1), Edge::Right, 0.05);
            let now = width(&node);
            assert!(now >= last, "press {press} shrank the pane: {last} -> {now}");
            last = now;
        }
        // Each press moves a share of what is left, so the steps get smaller;
        // what matters is that five of them are plainly visible.
        assert!(last >= start + 8, "five presses barely moved it: {start} -> {last}");
    }

    #[test]
    fn resizing_cannot_squeeze_a_pane_out_of_existence() {
        let mut node = PaneNode::even(&[PaneId(1), PaneId(2)], Direction::Columns).unwrap();
        for _ in 0..50 {
            node.resize(PaneId(1), Edge::Right, 0.5);
        }
        let rects = node.solve(area(80, 24));
        assert_eq!(rects.len(), 2);
        assert!(rect_of(&rects, 2).cols >= 1, "neighbour was eliminated");
    }

    #[test]
    fn focus_movement_picks_the_pane_beside_you_not_one_far_away() {
        // 1 | 2
        // 1 | 3
        let mut node = PaneNode::leaf(1);
        node.split(PaneId(1), Direction::Columns, PaneId(2), 0.5);
        node.split(PaneId(2), Direction::Rows, PaneId(3), 0.5);
        let rects = node.solve(area(80, 24));

        assert_eq!(neighbour(&rects, PaneId(1), Edge::Right), Some(PaneId(2)));
        assert_eq!(neighbour(&rects, PaneId(2), Edge::Left), Some(PaneId(1)));
        assert_eq!(neighbour(&rects, PaneId(2), Edge::Down), Some(PaneId(3)));
        assert_eq!(neighbour(&rects, PaneId(3), Edge::Up), Some(PaneId(2)));
        // Nothing beyond the edges.
        assert_eq!(neighbour(&rects, PaneId(1), Edge::Left), None);
        assert_eq!(neighbour(&rects, PaneId(3), Edge::Right), None);
    }

    #[test]
    fn tiled_layout_spreads_panes_over_rows_and_columns() {
        let panes: Vec<PaneId> = (1..=4).map(PaneId).collect();
        let node = PaneNode::tiled(&panes).unwrap();
        let rects = node.solve(area(80, 24));
        assert_eq!(rects.len(), 4);
        // Two rows of two, so no pane spans the full width.
        assert!(rects.iter().all(|r| r.cols < 80));
        assert!(rects.iter().all(|r| r.rows < 24));
    }

    #[test]
    fn an_area_too_small_for_every_pane_hides_the_extras() {
        let node =
            PaneNode::even(&(1..=5).map(PaneId).collect::<Vec<_>>(), Direction::Columns).unwrap();
        let rects = node.solve(area(4, 2));
        // Every pane is still accounted for...
        assert_eq!(rects.len(), 5);
        // ...but only the ones that fit have a size, and nothing is laid out
        // past the edge of the area.
        assert!(rects.iter().any(|r| r.cols == 0), "nothing was hidden: {rects:?}");
        assert!(rects.iter().all(|r| r.col + r.cols <= 4), "drew outside the area: {rects:?}");
    }

    #[test]
    fn a_small_but_sufficient_area_gives_every_pane_a_cell() {
        let node =
            PaneNode::even(&(1..=3).map(PaneId).collect::<Vec<_>>(), Direction::Columns).unwrap();
        let rects = node.solve(area(5, 2));
        assert!(rects.iter().all(|r| r.cols >= 1), "{rects:?}");
        assert!(rects.iter().all(|r| r.col + r.cols <= 5));
    }
}

#[cfg(test)]
mod rearrange_tests {
    use super::*;

    fn tree() -> PaneNode {
        // Three in a row, with the middle one twice the width.
        PaneNode::Split {
            dir: Direction::Columns,
            children: vec![
                Child { node: PaneNode::leaf(1), weight: 1.0 },
                Child { node: PaneNode::leaf(2), weight: 2.0 },
                Child { node: PaneNode::leaf(3), weight: 1.0 },
            ],
        }
    }

    fn weights(node: &PaneNode) -> Vec<(u32, f32)> {
        let PaneNode::Split { children, .. } = node else { panic!("not a split") };
        children
            .iter()
            .map(|c| match &c.node {
                PaneNode::Leaf(id) => (id.0, c.weight),
                _ => (0, c.weight),
            })
            .collect()
    }

    #[test]
    fn swapping_moves_the_panes_and_leaves_the_sizes_alone() {
        // The property that makes swap useful: you are moving a pane into a
        // *place*, so the places must not move with it.
        let mut node = tree();
        assert!(node.swap(PaneId(1), PaneId(2)));
        assert_eq!(weights(&node), vec![(2, 1.0), (1, 2.0), (3, 1.0)]);
    }

    #[test]
    fn swapping_a_pane_with_itself_or_a_stranger_does_nothing() {
        let mut node = tree();
        assert!(!node.swap(PaneId(1), PaneId(1)));
        assert!(!node.swap(PaneId(1), PaneId(9)));
        assert_eq!(weights(&node), weights(&tree()));
    }

    #[test]
    fn swapping_works_across_nesting() {
        let mut node = PaneNode::Split {
            dir: Direction::Columns,
            children: vec![
                Child { node: PaneNode::leaf(1), weight: 1.0 },
                Child {
                    node: PaneNode::Split {
                        dir: Direction::Rows,
                        children: vec![
                            Child { node: PaneNode::leaf(2), weight: 1.0 },
                            Child { node: PaneNode::leaf(3), weight: 1.0 },
                        ],
                    },
                    weight: 1.0,
                },
            ],
        };
        assert!(node.swap(PaneId(1), PaneId(3)));
        assert_eq!(node.panes(), vec![PaneId(3), PaneId(2), PaneId(1)]);
    }

    #[test]
    fn rotating_moves_panes_round_the_positions() {
        let mut node = tree();
        assert!(node.rotate(false));
        // Each pane moves one place along; the weights stay with the places.
        assert_eq!(weights(&node), vec![(3, 1.0), (1, 2.0), (2, 1.0)]);
        assert!(node.rotate(true));
        assert_eq!(weights(&node), weights(&tree()), "rotating back should undo it");
    }

    #[test]
    fn a_single_pane_cannot_be_rotated() {
        let mut node = PaneNode::leaf(1);
        assert!(!node.rotate(false));
    }
}
