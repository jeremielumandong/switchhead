//! Painting one row of the commit graph: lane lines and the commit's dot.
//!
//! [`switchhead_git::graph`] does the layout; this module only turns a row's segments into
//! pixel geometry inside the row's bounds. Lines that change column are drawn as a short
//! curve, the way Fork and `git log --graph` read.

use gpui_kit::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, point, px};
use switchhead_git::graph::{Node, Segment, SegmentKind};

use crate::theme::Palette;

/// Lane width as a fraction of the row height.
const LANE: f32 = 0.6;
/// Left padding before the first lane, in lane widths.
const PAD: f32 = 0.6;

/// Width of the graph column for `lanes` lanes at row height `row_h`.
pub fn column_width(lanes: u16, row_h: f32) -> f32 {
    (lanes.max(1) as f32 - 1.0 + 2.0 * PAD) * LANE * row_h
}

/// Center x of lane `col`, relative to the row's left edge.
fn x_of(col: u16, row_h: f32) -> f32 {
    (PAD + col as f32) * LANE * row_h
}

/// A point relative to the row's origin.
pub type Pt = (f32, f32);

/// The polyline (start, control, end) a segment is drawn along, relative to the row's
/// origin. `None` control: a straight line.
pub fn segment_geometry(s: &Segment, row_h: f32) -> (Pt, Option<Pt>, Pt) {
    let mid = row_h / 2.0;
    let (xf, xt) = (x_of(s.from, row_h), x_of(s.to, row_h));
    match s.kind {
        SegmentKind::Through => ((xf, 0.0), None, (xf, row_h)),
        // From the top edge in `from` to the node in `to`.
        SegmentKind::Top if s.from == s.to => ((xf, 0.0), None, (xt, mid)),
        SegmentKind::Top => ((xf, 0.0), Some((xf, mid)), (xt, mid)),
        // From the node in `from` to the bottom edge in `to`.
        SegmentKind::Bottom if s.from == s.to => ((xf, mid), None, (xt, row_h)),
        SegmentKind::Bottom => ((xf, mid), Some((xt, mid)), (xt, row_h)),
    }
}

/// Colors a row is painted with.
#[derive(Clone, Copy, Debug)]
pub struct GraphColors {
    /// Lane colors, by color index modulo 8.
    pub lanes: [Hsla; 8],
    /// The list background (inside HEAD's hollow dot).
    pub bg: Hsla,
}

/// The graph colors of a palette.
pub fn colors(p: &Palette) -> GraphColors {
    GraphColors {
        bg: p.surface,
        lanes: [
            p.acc, p.added, p.modified, p.sx_kw, p.deleted, p.sx_fn, p.sx_num, p.sx_str,
        ],
    }
}

/// Paint a row's lines and node inside `bounds`. `head`: the node is HEAD (drawn hollow
/// with a ring); `merge`: a merge commit (smaller dot).
pub fn paint_row(
    bounds: Bounds<Pixels>,
    node: Node,
    segments: &[Segment],
    (head, merge): (bool, bool),
    colors: &GraphColors,
    window: &mut Window,
) {
    let row_h: f32 = bounds.size.height.into();
    let o = bounds.origin;
    let at = |(x, y): (f32, f32)| -> Point<Pixels> { o + point(px(x), px(y)) };
    let width = (row_h / 12.0).clamp(1.2, 3.0);
    let color = |i: u16| colors.lanes[i as usize % colors.lanes.len()];
    for s in segments {
        let (a, ctrl, b) = segment_geometry(s, row_h);
        let mut path = PathBuilder::stroke(px(width));
        path.move_to(at(a));
        match ctrl {
            Some(c) => path.curve_to(at(b), at(c)),
            None => path.line_to(at(b)),
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color(s.color));
        }
    }
    let c = color(node.color);
    let center = at((x_of(node.column, row_h), row_h / 2.0));
    let r = row_h * if merge { 0.15 } else { 0.2 };
    paint_circle(center, r, c, window);
    if head {
        paint_circle(center, r - width, colors.bg, window);
    }
}

fn paint_circle(center: Point<Pixels>, r: f32, color: Hsla, window: &mut Window) {
    let b = Bounds::new(
        center - point(px(r), px(r)),
        gpui_kit::size(px(2.0 * r), px(2.0 * r)),
    );
    window.paint_quad(gpui_kit::fill(b, color).corner_radii(px(r)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(kind: SegmentKind, from: u16, to: u16) -> Segment {
        Segment {
            kind,
            from,
            to,
            color: 0,
        }
    }

    #[test]
    fn geometry_spans_the_row() {
        let h = 20.0;
        let lane = LANE * h;
        let x0 = PAD * lane;
        assert_eq!(
            segment_geometry(&seg(SegmentKind::Through, 1, 1), h),
            ((x0 + lane, 0.0), None, (x0 + lane, h))
        );
        // A line arriving from lane 2 bends into the node in lane 0.
        let (a, c, b) = segment_geometry(&seg(SegmentKind::Top, 2, 0), h);
        assert_eq!(a, (x0 + 2.0 * lane, 0.0));
        assert_eq!(b, (x0, h / 2.0));
        assert!(c.is_some());
        // A line to a parent leaves the node and reaches the bottom edge in its lane.
        let (a, _, b) = segment_geometry(&seg(SegmentKind::Bottom, 0, 1), h);
        assert_eq!(a, (x0, h / 2.0));
        assert_eq!(b, (x0 + lane, h));
        assert!(column_width(3, h) > column_width(1, h));
        assert_eq!(column_width(0, h), column_width(1, h));
    }
}
