//! The diff minimap: a narrow strip beside a diff that marks added, removed and changed
//! rows over the whole diff and shows the visible part. Clicking or dragging on it scrolls
//! the diff there.

use std::cell::Cell;
use std::sync::Arc;

use gpui_kit::{
    App, Bounds, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Styled as _, UniformListScrollHandle, Window, canvas, fill, point, px, size,
};

use crate::appearance::rpx;
use crate::diff_model::{Mark, MarkKind};
use crate::theme::Palette;

thread_local! {
    /// A drag that started on a minimap is in progress.
    static DRAGGING: Cell<bool> = const { Cell::new(false) };
}

/// The strip for a list of `rows` rows of `row_h` pixels, scrolled by `scroll`.
pub(crate) fn minimap(
    marks: Arc<Vec<Mark>>,
    rows: usize,
    row_h: f32,
    scroll: &UniformListScrollHandle,
    p: &Palette,
) -> impl IntoElement {
    let scroll = scroll.clone();
    let colors = Colors {
        added: p.added,
        removed: p.deleted,
        changed: p.modified,
        thumb: p.fg3.opacity(0.3),
        edge: p.bd,
    };
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            paint(&marks, rows, row_h, &scroll, &colors, bounds, window);
            let total = rows as f32 * row_h;
            let jump = {
                let scroll = scroll.clone();
                move |y: Pixels, window: &mut Window| {
                    scroll_to(&scroll, bounds, total, y);
                    window.refresh();
                }
            };
            let down = jump.clone();
            window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, _: &mut App| {
                if phase.bubble() && ev.button == MouseButton::Left && bounds.contains(&ev.position)
                {
                    DRAGGING.set(true);
                    down(ev.position.y, window);
                }
            });
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, _: &mut App| {
                if phase.bubble() && DRAGGING.get() {
                    if ev.pressed_button == Some(MouseButton::Left) {
                        jump(ev.position.y, window);
                    } else {
                        DRAGGING.set(false);
                    }
                }
            });
            window.on_mouse_event(|_: &MouseUpEvent, _, _, _: &mut App| DRAGGING.set(false));
        },
    )
    .flex_none()
    .w(rpx(12.))
    .h_full()
}

struct Colors {
    added: Hsla,
    removed: Hsla,
    changed: Hsla,
    thumb: Hsla,
    edge: Hsla,
}

/// Scroll so the row at `y` (window coordinates over the strip) sits mid-viewport.
fn scroll_to(scroll: &UniformListScrollHandle, strip: Bounds<Pixels>, total: f32, y: Pixels) {
    let h = f32::from(strip.size.height);
    if h <= 0. || total <= 0. {
        return;
    }
    let state = scroll.0.borrow();
    let handle = &state.base_handle;
    let view = f32::from(handle.bounds().size.height);
    let frac = (f32::from(y - strip.origin.y) / h).clamp(0., 1.);
    let top = (frac * total - view / 2.).clamp(0., (total - view).max(0.));
    let x = handle.offset().x;
    handle.set_offset(point(x, px(-top)));
}

fn paint(
    marks: &[Mark],
    rows: usize,
    row_h: f32,
    scroll: &UniformListScrollHandle,
    c: &Colors,
    b: Bounds<Pixels>,
    window: &mut Window,
) {
    window.paint_quad(fill(
        Bounds::new(b.origin, size(px(1.), b.size.height)),
        c.edge,
    ));
    let h = f32::from(b.size.height);
    if rows == 0 || h <= 0. {
        return;
    }
    let total = rows as f32 * row_h;
    // Rows fill the strip when the diff is taller than it; a short diff keeps its scale.
    let scale = (h / total).min(1.);
    let x = b.origin.x + px(3.);
    let w = b.size.width - px(5.);
    for m in marks {
        let top = m.start as f32 * row_h * scale;
        let height = ((m.end - m.start) as f32 * row_h * scale).max(2.);
        let color = match m.kind {
            MarkKind::Added => c.added,
            MarkKind::Removed => c.removed,
            MarkKind::Changed => c.changed,
        };
        window.paint_quad(fill(
            Bounds::new(point(x, b.origin.y + px(top)), size(w, px(height))),
            color,
        ));
    }
    let handle = &scroll.0.borrow().base_handle;
    let view = f32::from(handle.bounds().size.height);
    if view > 0. && view < total {
        let top = -f32::from(handle.offset().y) * scale;
        window.paint_quad(fill(
            Bounds::new(
                point(b.origin.x + px(1.), b.origin.y + px(top)),
                size(b.size.width - px(1.), px((view * scale).max(8.))),
            ),
            c.thumb,
        ));
    }
}
