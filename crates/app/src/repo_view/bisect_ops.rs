//! Guided bisect: Start Bisect (commit menu, palette) asks for a bad and a good commit;
//! a step panel above the graph then shows the commit to test, the commits and steps
//! left, and Good / Bad / Skip / Stop, ending on the first bad commit. Marked commits get
//! chips in the graph and commits outside the remaining range are dimmed.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div,
};
use switchhead_git::Oid;
use switchhead_git::bisect::Bisect;
use switchhead_git::ops::BisectMark;

use super::RepoView;
use crate::appearance::{rpx, ts};
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// A bisect session as the view shows it.
pub struct BisectView {
    pub state: Bisect,
    /// [`Bisect::remaining`] for lookups while painting rows.
    remaining: HashSet<Oid>,
}

/// How a graph row takes part in the session.
pub enum RowMark {
    Bad,
    Good,
    Skipped,
    FirstBad,
}

impl BisectView {
    /// The chip for commit `id`, if it is marked.
    pub fn mark(&self, id: &Oid) -> Option<RowMark> {
        let s = &self.state;
        if s.first_bad() == Some(*id) {
            Some(RowMark::FirstBad)
        } else if s.bad == Some(*id) {
            Some(RowMark::Bad)
        } else if s.good.contains(id) {
            Some(RowMark::Good)
        } else if s.skipped.contains(id) {
            Some(RowMark::Skipped)
        } else {
            None
        }
    }

    /// Commit `id` is outside the range still searched (only while narrowing it down).
    pub fn dims(&self, id: &Oid) -> bool {
        self.state.started() && !self.remaining.contains(id)
    }
}

/// A row's bisect chip.
pub fn row_chip(mark: RowMark, p: &Palette) -> AnyElement {
    match mark {
        RowMark::FirstBad => ui::chip("first bad", p.deleted),
        RowMark::Bad => ui::chip("bad", p.deleted),
        RowMark::Good => ui::chip("good", p.added),
        RowMark::Skipped => ui::chip("skipped", p.fg3),
    }
    .into_any_element()
}

fn verb(mark: BisectMark) -> &'static str {
    match mark {
        BisectMark::Bad => "bad",
        BisectMark::Good => "good",
        BisectMark::Skip => "skipped",
    }
}

/// "commit" or "commits".
fn commits(n: usize) -> String {
    if n == 1 {
        "1 commit".to_owned()
    } else {
        format!("{n} commits")
    }
}

impl RepoView {
    /// Re-read the bisect session (in the background) when one may be running.
    pub(super) fn load_bisect(&mut self, cx: &mut Context<Self>) {
        if self.state != switchhead_git::RepoState::Bisect {
            if self.bisect.take().is_some() {
                cx.notify();
            }
            return;
        }
        let repo = self.repo.clone();
        let task = cx
            .background_executor()
            .spawn(async move { repo.bisect_state() });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                v.bisect = match res {
                    Ok(Some(state)) => Some(Rc::new(BisectView {
                        remaining: state.remaining.iter().copied().collect(),
                        state,
                    })),
                    Ok(None) => None,
                    Err(e) => {
                        tracing::warn!(error = %e, "could not read the bisect state");
                        None
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Ask for a bad and a good commit, then start. `good`: the commit to suggest.
    pub fn start_bisect_dialog(
        &mut self,
        good: Option<Oid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy() || self.state != switchhead_git::RepoState::Clean {
            return;
        }
        let good = good
            .filter(|g| self.refs.head != Some(*g))
            .map(|g| switchhead_git::short(&g));
        let mut field = |value: String, placeholder: &'static str| {
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                s.set_value(value, window, cx);
                s
            })
        };
        let bad = field("HEAD".to_owned(), "A commit with the bug, e.g. HEAD");
        let good_input = field(
            good.clone().unwrap_or_default(),
            "A commit without it: a commit id, tag or branch",
        );
        let error: Rc<RefCell<Option<&'static str>>> = Rc::default();
        for input in [&bad, &good_input] {
            let error = error.clone();
            cx.subscribe_in(input, window, move |_, _, ev: &InputEvent, window, _| {
                if matches!(ev, InputEvent::Change) && error.borrow_mut().take().is_some() {
                    window.refresh();
                }
            })
            .detach();
        }
        let first = Cell::new(true);
        let view = cx.entity().downgrade();
        let focus_good = good.is_none();
        window.open_alert_dialog(cx, move |alert, window, cx| {
            if first.replace(false) {
                let f = if focus_good { &good_input } else { &bad };
                window.focus(&f.focus_handle(cx), cx);
            }
            let p = palette(cx);
            let row = |label: &'static str, input: &Entity<InputState>| {
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(10.))
                    .child(
                        div()
                            .flex_none()
                            .w(rpx(110.))
                            .text_size(ts::BODY)
                            .text_color(p.fg2)
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(input).text_size(ts::BODY)),
                    )
            };
            let shown = *error.borrow();
            let (bad, good_input, error, view) =
                (bad.clone(), good_input.clone(), error.clone(), view.clone());
            alert
                .confirm()
                .title("Start Bisect")
                .description(
                    "Find the commit that introduced a bug. switchhead checks out commits \
                     between these two; test each one and mark it good or bad until the \
                     first bad commit is found.",
                )
                .ok_text("Start")
                .child(row("Bad (has it)", &bad))
                .child(row("Good (does not)", &good_input))
                .when_some(shown, |a, e| {
                    a.child(div().text_size(ts::SMALL).text_color(p.deleted).child(e))
                })
                .on_ok(move |_, window, cx: &mut App| {
                    let b = bad.read(cx).value().trim().to_owned();
                    let g = good_input.read(cx).value().trim().to_owned();
                    let problem = if b.is_empty() {
                        Some("Enter a bad commit")
                    } else if g.is_empty() {
                        Some("Enter a good commit")
                    } else if b.starts_with('-') || g.starts_with('-') {
                        Some("Enter commit ids, tags or branch names")
                    } else {
                        None
                    };
                    if problem.is_some() {
                        *error.borrow_mut() = problem;
                        window.refresh();
                        return false;
                    }
                    let _ = view.update(cx, |v, cx| {
                        v.run_op(
                            "Starting bisect",
                            "Bisecting: test the checked-out commit",
                            move |r, _, _| r.bisect_start(&b, &g),
                            cx,
                        )
                    });
                    true
                })
        });
    }

    /// Mark `rev` (the commit being tested when `None`).
    pub fn bisect_mark(&mut self, mark: BisectMark, rev: Option<Oid>, cx: &mut Context<Self>) {
        if self.state != switchhead_git::RepoState::Bisect {
            return;
        }
        let what = rev.map_or_else(
            || "the tested commit".to_owned(),
            |r| switchhead_git::short(&r),
        );
        self.run_op(
            format!("Marking {what} {}", verb(mark)),
            format!("Marked {what} {}", verb(mark)),
            move |r, _, _| {
                r.bisect_mark(mark, rev.map(|r| r.to_string()).as_deref())
                    .map(drop)
            },
            cx,
        );
    }

    /// End the session and go back to where it started.
    pub fn stop_bisect(&mut self, cx: &mut Context<Self>) {
        if self.state != switchhead_git::RepoState::Bisect {
            return;
        }
        self.run_op(
            "Ending bisect",
            "Bisect ended",
            |r, _, _| r.bisect_reset(),
            cx,
        );
    }

    fn summary_of(&self, id: Oid) -> String {
        self.log
            .row_of(&id)
            .map(|r| self.log.summary(r).to_owned())
            .unwrap_or_default()
    }

    /// The step panel above the graph while bisecting.
    pub(super) fn render_bisect_panel(
        &self,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.state != switchhead_git::RepoState::Bisect {
            return None;
        }
        let running = self.busy();
        let button = |id: &'static str, text: &'static str, kind: ui::Kind| {
            ui::button(id, text, kind, p)
                .h(rpx(22.))
                .when(running, |d| d.opacity(0.5))
                .when(!running, |d| d.cursor_pointer())
        };
        let commit = |id: Oid, summary: String| {
            div()
                .id(("bisect-commit", 0usize))
                .flex()
                .items_center()
                .gap(rpx(6.))
                .min_w_0()
                .cursor_pointer()
                .child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .text_size(ts::SMALL)
                        .text_color(p.acc)
                        .child(switchhead_git::short(&id)),
                )
                .child(div().min_w_0().truncate().child(summary))
                .on_click(cx.listener(move |v, _, _, cx| v.reveal(id, cx)))
        };
        let b = self.bisect.as_ref().map(|b| &b.state);
        let (chip, color) = match b {
            Some(b) if b.first_bad().is_some() => ("Found", p.deleted),
            Some(b) if b.only_skipped() => ("Stuck", p.modified),
            _ => ("Bisecting", p.modified),
        };
        let mut panel = div()
            .flex_none()
            .min_h(rpx(36.))
            .px(rpx(12.))
            .py(rpx(6.))
            .flex()
            .items_center()
            .gap(rpx(10.))
            .bg(color.opacity(0.12))
            .border_b_1()
            .border_color(color.opacity(0.4))
            .text_size(ts::BODY)
            .child(ui::chip(chip, color));
        let finish = |label: &'static str| {
            button("bisect-stop", label, ui::Kind::Secondary)
                .on_click(cx.listener(|v, _, _, cx| v.stop_bisect(cx)))
        };
        let found = b.and_then(Bisect::first_bad);
        match (b, found) {
            (None, _) => {
                panel = panel
                    .child(
                        div()
                            .flex_1()
                            .text_color(p.fg2)
                            .child("Reading the bisect session…"),
                    )
                    .child(finish("Stop"));
            }
            (Some(b), _) if !b.started() => {
                let missing = if b.bad.is_none() {
                    "Mark a commit that has the bug as bad (right-click it in the graph)"
                } else {
                    "Mark a commit without the bug as good (right-click it in the graph)"
                };
                panel = panel
                    .child(div().flex_1().min_w_0().text_color(p.fg2).child(missing))
                    .child(finish("Stop"));
            }
            (Some(_), Some(id)) => {
                panel = panel
                    .child(
                        div()
                            .flex_none()
                            .text_color(p.fg2)
                            .child("First bad commit:"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(commit(id, self.summary_of(id))),
                    )
                    .child(
                        button("bisect-show", "Show", ui::Kind::Secondary)
                            .on_click(cx.listener(move |v, _, _, cx| v.reveal(id, cx))),
                    )
                    .child(finish("Finish"));
            }
            (Some(b), None) if b.only_skipped() => {
                let n = b.remaining.len();
                panel = panel
                    .child(div().flex_1().min_w_0().text_color(p.fg2).child(format!(
                        "Only skipped commits are left: the first bad commit is one of these {}.",
                        commits(n)
                    )))
                    .child(finish("Finish"));
            }
            (Some(b), None) => {
                let left = b.untested();
                let steps = b.steps_left();
                let current = b.current;
                panel = panel
                    .child(div().flex_none().text_color(p.fg2).child("Testing"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when_some(current, |d, id| d.child(commit(id, self.summary_of(id)))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ts::SMALL)
                            .text_color(p.fg2)
                            .child(SharedString::from(format!(
                                "{} left · about {} {}",
                                commits(left),
                                steps,
                                if steps == 1 { "step" } else { "steps" }
                            ))),
                    )
                    .child(button("bisect-good", "Good", ui::Kind::Secondary).on_click(
                        cx.listener(|v, _, _, cx| v.bisect_mark(BisectMark::Good, None, cx)),
                    ))
                    .child(button("bisect-bad", "Bad", ui::Kind::Secondary).on_click(
                        cx.listener(|v, _, _, cx| v.bisect_mark(BisectMark::Bad, None, cx)),
                    ))
                    .child(button("bisect-skip", "Skip", ui::Kind::Ghost).on_click(
                        cx.listener(|v, _, _, cx| v.bisect_mark(BisectMark::Skip, None, cx)),
                    ))
                    .child(finish("Stop"));
            }
        }
        Some(panel.into_any_element())
    }
}
