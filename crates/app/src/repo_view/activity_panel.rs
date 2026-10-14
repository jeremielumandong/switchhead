//! The activity panel under the graph: every operation the tab ran, newest first, with
//! git's progress and output (errors in full). Opened from the status bar.

use std::ops::Range;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px, uniform_list,
};

use super::RepoView;
use crate::activity::Outcome;
use crate::appearance::{rpx, ts};
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// A flattened row of the panel.
#[derive(Clone)]
enum Row {
    Entry {
        id: u64,
        label: SharedString,
        at: SharedString,
        ms: Option<u64>,
        outcome: Outcome,
        open: bool,
    },
    Line(SharedString),
}

impl RepoView {
    /// Show or hide the activity panel.
    pub fn toggle_activity(&mut self, cx: &mut Context<Self>) {
        self.activity_open = !self.activity_open;
        cx.notify();
    }

    fn toggle_entry(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.activity_collapsed.remove(&id) {
            self.activity_collapsed.insert(id);
        }
        cx.notify();
    }

    fn activity_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for e in self.activity.entries() {
            let open = !self.activity_collapsed.contains(&e.id);
            rows.push(Row::Entry {
                id: e.id,
                label: e.label.clone().into(),
                at: e.at.clone().into(),
                ms: e.ms,
                outcome: e.outcome,
                open,
            });
            if open {
                rows.extend(e.lines.iter().map(|l| Row::Line(l.clone().into())));
            }
        }
        rows
    }

    pub(super) fn render_activity(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.activity_rows();
        let count = rows.len();
        let list = uniform_list(
            "activity",
            count,
            cx.processor(move |_, range: Range<usize>, _, cx| {
                let p = palette(cx);
                range
                    .filter_map(|i| rows.get(i).cloned().map(|r| (i, r)))
                    .map(|(i, r)| render_row(i, r, &p, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .py(rpx(2.));
        div()
            .h(rpx(200.))
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(p.bd)
            .bg(p.panel)
            .child(
                div()
                    .h(rpx(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(rpx(12.))
                    .border_b_1()
                    .border_color(p.bd)
                    .child(
                        div()
                            .flex_1()
                            .text_size(ts::CAPTION)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.fg3)
                            .child("ACTIVITY"),
                    )
                    .child(
                        div()
                            .id("activity-close")
                            .px(rpx(6.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_size(ts::SMALL)
                            .text_color(p.fg2)
                            .hover(|s| s.bg(p.hover).text_color(p.fg))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_activity(cx)))
                            .child("Close"),
                    ),
            )
            .when(count == 0, |d| {
                d.child(
                    div()
                        .p(rpx(12.))
                        .text_size(ts::SMALL)
                        .text_color(p.fg3)
                        .child("Fetch, pull, push and other operations show up here."),
                )
            })
            .child(list)
            .into_any_element()
    }
}

fn render_row(i: usize, row: Row, p: &Palette, cx: &mut Context<RepoView>) -> AnyElement {
    let base = div()
        .id(("activity-row", i))
        .w_full()
        .h(rpx(20.))
        .flex()
        .items_center()
        .gap(rpx(6.))
        .px(rpx(12.));
    match row {
        Row::Entry {
            id,
            label,
            at,
            ms,
            outcome,
            open,
        } => {
            let (color, what) = match outcome {
                Outcome::Running => (p.acc, "running"),
                Outcome::Done => (p.added, "done"),
                Outcome::Failed => (p.deleted, "failed"),
                Outcome::Cancelled => (p.fg3, "cancelled"),
            };
            base.cursor_pointer()
                .hover(|s| s.bg(p.hover))
                .text_size(ts::SMALL)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_entry(id, cx)))
                .child(
                    div()
                        .w(rpx(10.))
                        .text_size(ts::CAPTION)
                        .text_color(p.fg3)
                        .child(if open { "▾" } else { "▸" }),
                )
                .child(ui::dot(color, 6.))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.fg)
                        .child(label),
                )
                .child(div().text_color(color).child(what))
                .child(div().flex_1())
                .when_some(ms, |d, ms| {
                    d.child(
                        div()
                            .font_family(MONO)
                            .text_color(p.fg3)
                            .child(format!("{ms} ms")),
                    )
                })
                .child(div().font_family(MONO).text_color(p.fg3).child(at))
                .into_any_element()
        }
        Row::Line(text) => base
            .pl(rpx(40.))
            .font_family(MONO)
            .text_size(ts::CAPTION_PLUS)
            .text_color(p.fg2)
            .child(div().min_w_0().truncate().child(text))
            .into_any_element(),
    }
}
