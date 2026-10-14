//! Commit graph layout: lanes, nodes and line segments, one row per commit.
//!
//! [`Layout`] is pure and incremental: it takes commits in display order (children before
//! parents) and emits one row each, so the log walk lays out each batch as it streams.
//! A lane holds the next commit it expects. A commit takes the leftmost lane expecting it
//! (lines from the others join it), its first parent continues in that lane with the same
//! color, and further parents join a lane that already expects them or fork a new one. Lane slots never move, so a lane passing through a row is a straight line.
//!
//! Rows are stored in one arena ([`GraphRows`]): a node per row and that row's segments,
//! with per-row offsets. The UI paints segments; it does no layout.

use crate::Oid;

/// How a segment crosses its row. Columns are lane slots; the row is split at its center,
/// where the commit's node sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    /// A lane passing by, top edge to bottom edge in column `from` (`to == from`).
    Through,
    /// From column `from` at the top edge to the node in column `to` (a child's line
    /// arriving at this commit).
    Top,
    /// From the node in column `from` to column `to` at the bottom edge (a line to one of
    /// this commit's parents).
    Bottom,
}

/// One line piece in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Segment {
    /// Shape.
    pub kind: SegmentKind,
    /// Start column.
    pub from: u16,
    /// End column.
    pub to: u16,
    /// Color index (the UI takes it modulo its palette).
    pub color: u16,
}

/// The commit's dot in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    /// Lane column of the commit.
    pub column: u16,
    /// Color index of the commit's lane.
    pub color: u16,
    /// Columns the row uses (the painted graph width, in lanes).
    pub width: u16,
}

/// A row: its node and segments.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    /// The commit's node.
    pub node: Node,
    /// Line pieces, in no particular order.
    pub segments: &'a [Segment],
}

/// Graph rows in one arena.
#[derive(Clone, Debug)]
pub struct GraphRows {
    nodes: Vec<Node>,
    segments: Vec<Segment>,
    /// `segments[start[i]..start[i + 1]]` belong to row `i`.
    start: Vec<u32>,
}

impl Default for GraphRows {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphRows {
    /// No rows.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            segments: Vec::new(),
            start: vec![0],
        }
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Row `i`.
    pub fn row(&self, i: usize) -> Row<'_> {
        Row {
            node: self.nodes[i],
            segments: &self.segments[self.start[i] as usize..self.start[i + 1] as usize],
        }
    }

    /// The widest row, in lanes (sizes the graph column).
    pub fn max_width(&self) -> u16 {
        self.nodes.iter().map(|n| n.width).max().unwrap_or(0)
    }

    /// Append rows from another batch.
    pub fn append(&mut self, other: &GraphRows) {
        let base = self.segments.len() as u32;
        self.nodes.extend_from_slice(&other.nodes);
        self.segments.extend_from_slice(&other.segments);
        self.start
            .extend(other.start[1..].iter().map(|s| base + *s));
    }

    fn push_row(&mut self, node: Node) {
        self.nodes.push(node);
        self.start.push(self.segments.len() as u32);
    }
}

#[derive(Clone, Copy, Debug)]
struct Lane {
    expect: Oid,
    color: u16,
}

/// Incremental lane layout. Feed commits in display order with [`Layout::push`].
#[derive(Clone, Debug, Default)]
pub struct Layout {
    lanes: Vec<Option<Lane>>,
    next_color: u16,
}

impl Layout {
    /// A layout with no lanes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lanes currently open (trailing free slots trimmed).
    pub fn open_lanes(&self) -> usize {
        self.lanes.len()
    }

    fn new_color(&mut self) -> u16 {
        let c = self.next_color;
        self.next_color = self.next_color.wrapping_add(1);
        c
    }

    fn free_slot(&mut self) -> usize {
        match self.lanes.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                self.lanes.push(None);
                self.lanes.len() - 1
            }
        }
    }

    /// Lay out commit `id` with `parents` and append its row to `out`. `shown` says
    /// whether a parent was already laid out (only in an out-of-order history); such a
    /// parent gets no lane, since nothing below would ever close it.
    pub fn push(
        &mut self,
        id: &Oid,
        parents: &[Oid],
        out: &mut GraphRows,
        shown: impl Fn(&Oid) -> bool,
    ) {
        let before = self.lanes.len();
        let first_hit = self
            .lanes
            .iter()
            .position(|l| l.is_some_and(|l| l.expect == *id));
        let (column, color) = match first_hit {
            Some(i) => (i, self.lanes[i].map(|l| l.color).unwrap_or(0)),
            None => {
                let c = self.new_color();
                (self.free_slot(), c)
            }
        };

        // Lines arriving from above: every lane expecting this commit joins the node,
        // the rest pass by.
        for (i, lane) in self.lanes.iter_mut().enumerate() {
            let Some(l) = *lane else { continue };
            if l.expect == *id {
                out.segments.push(Segment {
                    kind: SegmentKind::Top,
                    from: i as u16,
                    to: column as u16,
                    color: l.color,
                });
                *lane = None;
            } else {
                out.segments.push(Segment {
                    kind: SegmentKind::Through,
                    from: i as u16,
                    to: i as u16,
                    color: l.color,
                });
            }
        }

        // Lines leaving for the parents.
        for (n, p) in parents.iter().enumerate() {
            if shown(p) || parents[..n].contains(p) {
                continue;
            }
            let continues = self.lanes.get(column).is_none_or(Option::is_none)
                && (n == 0 || first_parent_gone(parents, &shown));
            let waiting = self
                .lanes
                .iter()
                .position(|l| l.is_some_and(|l| l.expect == *p));
            let (to, lane_color) = if continues {
                // The first parent continues the commit's lane and color, even when
                // another lane waits for it too: the lanes join at the parent's row, in
                // the leftmost one, so a branch line does not jump right.
                if column >= self.lanes.len() {
                    self.lanes.resize(column + 1, None);
                }
                self.lanes[column] = Some(Lane { expect: *p, color });
                (column, color)
            } else if let Some(j) = waiting {
                // A merge parent another lane already waits for: join that lane.
                (j, self.lanes[j].map(|l| l.color).unwrap_or(color))
            } else {
                let c = self.new_color();
                let j = self.free_slot();
                self.lanes[j] = Some(Lane {
                    expect: *p,
                    color: c,
                });
                (j, c)
            };
            out.segments.push(Segment {
                kind: SegmentKind::Bottom,
                from: column as u16,
                to: to as u16,
                color: lane_color,
            });
        }

        while self.lanes.last().is_some_and(Option::is_none) {
            self.lanes.pop();
        }
        let width = before.max(self.lanes.len()).max(column + 1);
        out.push_row(Node {
            column: column as u16,
            color,
            width: width as u16,
        });
    }
}

/// The first parent gets no lane (already shown), so the next parent may take the
/// commit's lane.
fn first_parent_gone(parents: &[Oid], shown: &impl Fn(&Oid) -> bool) -> bool {
    parents.first().is_some_and(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u32) -> Oid {
        let mut b = [0u8; 20];
        b[16..].copy_from_slice(&n.to_be_bytes());
        Oid::from_bytes_or_panic(&b)
    }

    /// Lay out `(id, parents)` in order.
    fn layout(history: &[(u32, &[u32])]) -> GraphRows {
        let mut l = Layout::new();
        let mut out = GraphRows::new();
        let mut seen = std::collections::HashSet::new();
        for (id, parents) in history {
            let ps: Vec<Oid> = parents.iter().map(|p| oid(*p)).collect();
            l.push(&oid(*id), &ps, &mut out, |p| seen.contains(p));
            seen.insert(oid(*id));
        }
        out
    }

    fn columns(g: &GraphRows) -> Vec<u16> {
        (0..g.len()).map(|i| g.row(i).node.column).collect()
    }

    /// Columns where lines cross the row's top and bottom edges.
    fn edges(r: Row<'_>) -> (Vec<u16>, Vec<u16>) {
        let mut top = Vec::new();
        let mut bottom = Vec::new();
        for s in r.segments {
            match s.kind {
                SegmentKind::Through => {
                    top.push(s.from);
                    bottom.push(s.to);
                }
                SegmentKind::Top => top.push(s.from),
                SegmentKind::Bottom => bottom.push(s.to),
            }
        }
        top.sort_unstable();
        top.dedup();
        bottom.sort_unstable();
        bottom.dedup();
        (top, bottom)
    }

    /// Every line leaving a row's bottom edge enters the next row's top edge, nothing
    /// enters the first row, and nothing is left open after the root commits.
    fn assert_connected(g: &GraphRows) {
        assert!(edges(g.row(0)).0.is_empty(), "lines above the first row");
        for i in 1..g.len() {
            assert_eq!(
                edges(g.row(i - 1)).1,
                edges(g.row(i)).0,
                "rows {} and {i} do not connect",
                i - 1
            );
        }
        assert!(
            edges(g.row(g.len() - 1)).1.is_empty(),
            "lines below the last row"
        );
        for i in 0..g.len() {
            let r = g.row(i);
            for s in r.segments {
                assert!(
                    s.from < r.node.width && s.to < r.node.width,
                    "row {i}: {s:?}"
                );
            }
        }
    }

    #[test]
    fn linear_history_is_one_lane() {
        let g = layout(&[(3, &[2]), (2, &[1]), (1, &[])]);
        assert_connected(&g);
        assert_eq!(columns(&g), [0, 0, 0]);
        assert_eq!(g.max_width(), 1);
        let colors: Vec<u16> = (0..3).map(|i| g.row(i).node.color).collect();
        assert_eq!(colors, [0, 0, 0]);
        assert_eq!(
            g.row(1).segments,
            [
                Segment {
                    kind: SegmentKind::Top,
                    from: 0,
                    to: 0,
                    color: 0
                },
                Segment {
                    kind: SegmentKind::Bottom,
                    from: 0,
                    to: 0,
                    color: 0
                }
            ]
        );
    }

    #[test]
    fn branch_and_merge() {
        // 5 merges 4 (main) and 3 (side); both fork from 1.
        //   5
        //   |\
        //   4 |
        //   | 3
        //   | 2
        //   |/
        //   1
        let g = layout(&[(5, &[4, 3]), (4, &[1]), (3, &[2]), (2, &[1]), (1, &[])]);
        assert_connected(&g);
        assert_eq!(columns(&g), [0, 0, 1, 1, 0]);
        assert_eq!(g.max_width(), 2);
        // The first-parent line keeps its color; the side branch gets a new one.
        assert_eq!(g.row(1).node.color, g.row(0).node.color);
        assert_ne!(g.row(2).node.color, g.row(0).node.color);
        assert_eq!(g.row(3).node.color, g.row(2).node.color);
        // 1 is where two lanes join.
        let tops = g
            .row(4)
            .segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Top);
        assert_eq!(tops.count(), 2);
        assert_eq!(g.row(4).node.width, 2);
    }

    #[test]
    fn two_branch_tips() {
        // Two tips (main 3, feature 4) sharing 1.
        let g = layout(&[(4, &[2]), (3, &[1]), (2, &[1]), (1, &[])]);
        assert_connected(&g);
        assert_eq!(columns(&g), [0, 1, 0, 0]);
    }

    #[test]
    fn octopus_merge() {
        let g = layout(&[(9, &[3, 2, 1]), (3, &[0]), (2, &[0]), (1, &[0]), (0, &[])]);
        assert_connected(&g);
        assert_eq!(columns(&g), [0, 0, 1, 2, 0]);
        let bottoms = g
            .row(0)
            .segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Bottom);
        assert_eq!(bottoms.count(), 3);
        assert_eq!(g.max_width(), 3);
    }

    #[test]
    fn criss_cross_merges() {
        // 6 = merge(4, 5), 5 = merge(3, 2), 4 = merge(2, 3); 3 and 2 both from 1.
        let g = layout(&[
            (6, &[4, 5]),
            (5, &[3, 2]),
            (4, &[2, 3]),
            (3, &[1]),
            (2, &[1]),
            (1, &[]),
        ]);
        assert_connected(&g);
        assert_eq!(columns(&g)[0], 0);
        assert_eq!(*columns(&g).last().unwrap(), 0);
    }

    #[test]
    fn out_of_order_parent_gets_no_lane() {
        // 1 is shown before its child 2 (a grafted or broken history).
        let g = layout(&[(1, &[]), (2, &[1]), (3, &[2])]);
        assert_eq!(columns(&g), [0, 0, 0]);
        assert!(g.row(1).segments.is_empty());
        // 3 opens a lane for 2, which was also shown already, so the last row is empty.
        assert!(g.row(2).segments.is_empty());
    }

    #[test]
    fn freed_lanes_are_reused() {
        // Two short side branches one after another reuse lane 1.
        let g = layout(&[
            (8, &[7, 6]),
            (7, &[5]),
            (6, &[5]),
            (5, &[4, 3]),
            (4, &[1]),
            (3, &[1]),
            (1, &[]),
        ]);
        assert_connected(&g);
        assert_eq!(columns(&g), [0, 0, 1, 0, 0, 1, 0]);
        assert_eq!(g.max_width(), 2);
    }

    #[test]
    fn appending_batches_matches_one_pass() {
        let history: &[(u32, &[u32])] = &[(5, &[4, 3]), (4, &[1]), (3, &[2]), (2, &[1]), (1, &[])];
        let whole = layout(history);
        let mut l = Layout::new();
        let mut all = GraphRows::new();
        for chunk in history.chunks(2) {
            let mut part = GraphRows::new();
            for (id, ps) in chunk {
                let ps: Vec<Oid> = ps.iter().map(|p| oid(*p)).collect();
                l.push(&oid(*id), &ps, &mut part, |_| false);
            }
            all.append(&part);
        }
        assert_eq!(all.len(), whole.len());
        for i in 0..whole.len() {
            assert_eq!(all.row(i).node, whole.row(i).node);
            assert_eq!(all.row(i).segments, whole.row(i).segments);
        }
    }
}
