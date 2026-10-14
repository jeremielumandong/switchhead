//! The reflog of HEAD or a branch, shown in place of the graph: every place the ref
//! pointed, newest first, with the selected entry's commit beside it. Entries a reset or
//! rebase left behind can be restored: reset to it (HEAD and the current branch), move
//! the branch back to it, branch from it, or check it out.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::AppContext as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, UniformListScrollHandle,
    Window, div, px, uniform_list,
};
use switchhead_git::reflog::{self, ReflogEntry};
use switchhead_git::{Oid, Repo};

use crate::actions;
use crate::appearance::{rpx, ts};
use crate::commit_view::{CommitView, Navigate};
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// What the reflog view asks of the repository view.
pub enum ReflogEvent {
    /// Close the view.
    Close,
    /// Select this commit in the graph.
    Reveal(Oid),
    /// Put `reference` (`HEAD` or a branch's short name) back at this commit.
    Restore {
        /// The reflog's ref.
        reference: String,
        /// Where it goes.
        id: Oid,
    },
    /// New branch at this commit.
    NewBranch(Oid),
    /// Check this commit out (detached).
    Checkout(Oid),
}

/// One ref's reflog.
pub struct ReflogView {
    repo: Repo,
    /// `HEAD` or a local branch's short name.
    reference: String,
    /// Refs the picker offers: HEAD, then local branches.
    choices: Vec<String>,
    /// The checked-out branch (restoring it is a reset).
    head_branch: Option<String>,
    entries: Rc<Vec<ReflogEntry>>,
    loading: bool,
    error: Option<SharedString>,
    selected: Option<usize>,
    commit: Entity<CommitView>,
    split: Entity<ResizableState>,
    focus: FocusHandle,
    focus_pending: bool,
    scroll: UniformListScrollHandle,
    now: i64,
    _load: Task<()>,
    _sub: Subscription,
}

impl EventEmitter<ReflogEvent> for ReflogView {}

impl Focusable for ReflogView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The full ref name for a reflog choice.
fn full_name(reference: &str) -> String {
    if reference == "HEAD" {
        reference.to_owned()
    } else {
        format!("refs/heads/{reference}")
    }
}

impl ReflogView {
    /// The reflog of `reference` (`HEAD` or a local branch).
    pub fn new(
        repo: Repo,
        reference: String,
        branches: Vec<String>,
        head_branch: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let commit = cx.new(|cx| CommitView::new(repo.clone(), cx));
        let sub = cx.subscribe(&commit, |_, _, Navigate(id), cx| {
            cx.emit(ReflogEvent::Reveal(*id));
        });
        let mut choices = vec!["HEAD".to_owned()];
        choices.extend(branches);
        let mut v = Self {
            repo,
            reference,
            choices,
            head_branch,
            entries: Rc::default(),
            loading: false,
            error: None,
            selected: None,
            commit,
            split: cx.new(|_| ResizableState::default()),
            focus: cx.focus_handle(),
            focus_pending: true,
            scroll: UniformListScrollHandle::new(),
            now: chrono::Local::now().timestamp(),
            _load: Task::ready(()),
            _sub: sub,
        };
        v.load(cx);
        v
    }

    /// Show another ref's reflog.
    fn set_reference(&mut self, reference: String, cx: &mut Context<Self>) {
        self.reference = reference;
        self.load(cx);
    }

    /// Read the reflog again (after a restore moved the ref).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        let repo = self.repo.clone();
        let name = full_name(&self.reference);
        let reference = self.reference.clone();
        let task = cx
            .background_executor()
            .spawn(async move { reflog::read(&repo, &name) });
        self._load = cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.reference != reference {
                    return;
                }
                v.loading = false;
                match res {
                    Ok(entries) => {
                        // Keep the selection on the same line when the log only grew.
                        let keep = v.selected.and_then(|i| v.entries.get(i)).and_then(|old| {
                            entries.iter().position(|e| {
                                e.new == old.new && e.time == old.time && e.message == old.message
                            })
                        });
                        v.entries = Rc::new(entries);
                        v.selected = None;
                        if !v.entries.is_empty() {
                            v.select(keep.unwrap_or(0), cx);
                        } else {
                            v.commit.update(cx, |c, cx| c.show(None, Vec::new(), cx));
                        }
                    }
                    Err(e) => {
                        v.entries = Rc::default();
                        v.error = Some(e.to_string().into());
                    }
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(e) = self.entries.get(i) else {
            return;
        };
        let id = e.new;
        self.selected = Some(i);
        self.scroll.scroll_to_item(i, ScrollStrategy::Nearest);
        self.commit.update(cx, |c, cx| {
            c.show((!id.is_null()).then_some(id), Vec::new(), cx)
        });
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let next = self.selected.map_or(0, |c| {
            (c as isize)
                .saturating_add(delta)
                .clamp(0, len as isize - 1) as usize
        });
        self.select(next, cx);
    }

    fn selected_id(&self) -> Option<Oid> {
        self.selected
            .and_then(|i| self.entries.get(i))
            .map(|e| e.new)
            .filter(|id| !id.is_null())
    }

    /// The restore button's label: a reset for HEAD and the checked-out branch.
    fn restore_label(&self) -> String {
        let current = self.reference == "HEAD"
            || self.head_branch.as_deref() == Some(self.reference.as_str());
        if current {
            let target = self.head_branch.as_deref().unwrap_or("HEAD");
            format!("Reset {target} to Here…")
        } else {
            format!("Move {} Here…", self.reference)
        }
    }

    fn render_header(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        let choices = self.choices.clone();
        let current = self.reference.clone();
        let picker = Button::new("reflog-ref")
            .outline()
            .small()
            .label(SharedString::from(format!("{} ▾", self.reference)))
            .dropdown_menu(move |mut menu, _, _| {
                for c in &choices {
                    let view = view.clone();
                    let name = c.clone();
                    menu = menu.item(
                        PopupMenuItem::new(c.clone())
                            .checked(*c == current)
                            .on_click(move |_, _, cx| {
                                let name = name.clone();
                                let _ = view.update(cx, |v, cx| v.set_reference(name, cx));
                            }),
                    );
                }
                menu
            });
        let id = self.selected_id();
        // The newest entry is where the ref already is.
        let restorable = id.is_some() && self.selected != Some(0);
        let emit = move |ev: fn(Oid) -> ReflogEvent| {
            move |_: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                if let Some(id) = id {
                    cx.emit(ev(id));
                }
            }
        };
        div()
            .flex_none()
            .h(rpx(40.))
            .px(rpx(12.))
            .flex()
            .items_center()
            .gap(rpx(8.))
            .border_b_1()
            .border_color(p.bd)
            .child(
                div()
                    .flex_none()
                    .text_size(ts::TITLE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.fg)
                    .child("Reflog"),
            )
            .child(picker)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ts::SMALL)
                    .text_color(p.fg3)
                    .child(match self.entries.len() {
                        1 => "1 entry".to_owned(),
                        n => format!("{n} entries"),
                    }),
            )
            .child(
                ui::button(
                    "reflog-restore",
                    self.restore_label(),
                    ui::Kind::Secondary,
                    p,
                )
                .when(!restorable, |d| d.opacity(0.5))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let (true, Some(id)) = (restorable, id) {
                        cx.emit(ReflogEvent::Restore {
                            reference: this.reference.clone(),
                            id,
                        });
                    }
                })),
            )
            .child(
                ui::button("reflog-branch", "New Branch Here…", ui::Kind::Secondary, p)
                    .when(id.is_none(), |d| d.opacity(0.5))
                    .on_click(cx.listener(emit(ReflogEvent::NewBranch))),
            )
            .child(
                ui::button("reflog-checkout", "Check Out…", ui::Kind::Secondary, p)
                    .when(id.is_none(), |d| d.opacity(0.5))
                    .on_click(cx.listener(emit(ReflogEvent::Checkout))),
            )
            .child(
                ui::button("reflog-show", "Show in Graph", ui::Kind::Secondary, p)
                    .when(id.is_none(), |d| d.opacity(0.5))
                    .on_click(cx.listener(emit(ReflogEvent::Reveal))),
            )
            .child(
                ui::button("reflog-close", "Close", ui::Kind::Secondary, p)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ReflogEvent::Close))),
            )
            .into_any_element()
    }

    fn render_list(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        if let Some(e) = &self.error {
            return message(e.clone(), p.deleted);
        }
        if self.entries.is_empty() {
            return message(
                if self.loading {
                    ""
                } else {
                    "This ref has no reflog."
                },
                p.fg3,
            );
        }
        let row_h = (40. * crate::appearance::zoom(cx)).round();
        let entries = self.entries.clone();
        let selected = self.selected;
        let now = self.now;
        let reference: SharedString = self.reference.clone().into();
        uniform_list(
            "reflog",
            entries.len(),
            cx.processor(move |_, range: Range<usize>, _, cx| {
                let p = palette(cx);
                range
                    .filter_map(|i| entries.get(i).map(|e| (i, e)))
                    .map(|(i, e)| {
                        render_entry(i, e, &reference, selected == Some(i), now, row_h, &p, cx)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full()
        .into_any_element()
    }
}

impl Render for ReflogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_pending) {
            window.focus(&self.focus, cx);
        }
        let p = palette(cx);
        let rem = window.rem_size();
        let list = self.render_list(&p, cx);
        div()
            .id("reflog-view")
            .key_context("Files")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.surface)
            .on_action(
                cx.listener(|this, _: &actions::SelectUp, _, cx| this.move_selection(-1, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::SelectDown, _, cx| this.move_selection(1, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::SelectFirst, _, cx| {
                this.move_selection(isize::MIN / 2, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::SelectLast, _, cx| {
                this.move_selection(isize::MAX / 2, cx)
            }))
            .on_action(cx.listener(|_, _: &actions::Dismiss, _, cx| cx.emit(ReflogEvent::Close)))
            .child(self.render_header(&p, cx))
            .when(self.loading, |d| {
                d.child(ui::loading_bar("reflog-loading", &p))
            })
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("reflog-split")
                        .with_state(&self.split)
                        .child(
                            resizable_panel()
                                .size(rem * (420. / 13.))
                                .size_range(rem * (240. / 13.)..rem * (900. / 13.))
                                .child(div().size_full().flex().flex_col().child(list)),
                        )
                        .child(resizable_panel().child(self.commit.clone())),
                ),
            )
    }
}

fn message(text: impl Into<SharedString>, color: gpui_kit::Hsla) -> AnyElement {
    div()
        .flex_1()
        .p(rpx(16.))
        .text_size(ts::BODY)
        .text_color(color)
        .child(text.into())
        .into_any_element()
}

/// The action part of a reflog message, and the rest: `("reset", "moving to HEAD~1")`.
fn split_message(m: &str) -> (&str, &str) {
    match m.split_once(": ") {
        Some((action, rest)) if !action.contains(' ') || action.contains('(') => (action, rest),
        _ => ("", m),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_entry(
    i: usize,
    e: &ReflogEntry,
    reference: &SharedString,
    selected: bool,
    now: i64,
    row_h: f32,
    p: &Palette,
    cx: &mut Context<ReflogView>,
) -> AnyElement {
    let (action, rest) = split_message(&e.message);
    div()
        .id(("reflog-entry", i))
        .w_full()
        .h(px(row_h))
        .flex()
        .flex_col()
        .justify_center()
        .px(rpx(10.))
        .cursor_pointer()
        .when(selected, |d| d.bg(p.sel))
        .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
            window.focus(&this.focus, cx);
            this.select(i, cx);
            if ev.click_count() > 1
                && let Some(id) = this.selected_id()
            {
                cx.emit(ReflogEvent::Reveal(id));
            }
        }))
        .child(
            div()
                .flex()
                .gap(rpx(6.))
                .min_w_0()
                .text_size(ts::BODY)
                .when(!action.is_empty(), |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_color(p.acc)
                            .child(SharedString::from(action.to_owned())),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(p.fg)
                        .child(SharedString::from(rest.to_owned())),
                ),
        )
        .child(
            div()
                .flex()
                .gap(rpx(6.))
                .text_size(ts::SMALL)
                .text_color(p.fg3)
                .child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .child(format!("{reference}@{{{i}}}")),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .child(switchhead_git::short(&e.new)),
                )
                .child(
                    div()
                        .flex_none()
                        .child(crate::repo_view::fmt_date(e.time, now)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(e.committer.clone())),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_reflog_messages() {
        assert_eq!(
            split_message("reset: moving to HEAD~1"),
            ("reset", "moving to HEAD~1")
        );
        assert_eq!(
            split_message("commit (amend): Fix: a thing"),
            ("commit (amend)", "Fix: a thing")
        );
        assert_eq!(
            split_message("rebase (finish): returning to refs/heads/main"),
            ("rebase (finish)", "returning to refs/heads/main")
        );
        assert_eq!(
            split_message("plain words: here"),
            ("", "plain words: here")
        );
        assert_eq!(full_name("HEAD"), "HEAD");
        assert_eq!(full_name("main"), "refs/heads/main");
    }
}
