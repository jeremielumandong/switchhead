//! The interactive rebase editor: the commits after a base, newest first like the graph.
//! Drag rows (or Alt+↑/↓) to reorder; set each one to pick, reword, edit, squash, fixup or
//! drop from its menu or with P R E S F D; write new messages below the list. Start Rebase
//! hands the plan to the repository view, which runs it ([`switchhead_git::rebase::run`]).

use std::ops::Range;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use switchhead_git::rebase::{self, Action, TodoItem};
use switchhead_git::{Oid, Repo};

use crate::appearance::{rpx, ts};
use crate::rebase_model::{ACTIONS, Plan, action_label};
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// What the editor asks of the repository view.
pub enum RebaseEditorEvent {
    /// Run this plan (oldest first) onto `base`.
    Start {
        /// The commit the rebased ones go on top of.
        base: Oid,
        /// git's todo list.
        items: Vec<TodoItem>,
        /// Stash local changes first and reapply them after.
        autostash: bool,
    },
    /// Close without rebasing.
    Cancel,
}

/// A row being dragged.
#[derive(Clone)]
struct DragRow {
    from: usize,
    label: SharedString,
}

/// What a dragged row looks like under the pointer.
struct DragPreview(SharedString);

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .px(rpx(10.))
            .py(rpx(3.))
            .rounded(px(4.))
            .bg(p.elev)
            .border_1()
            .border_color(p.acc)
            .text_size(ts::BODY)
            .text_color(p.fg)
            .child(self.0.clone())
    }
}

/// The editor for one interactive rebase.
pub struct RebaseEditor {
    repo: Repo,
    base: Oid,
    /// "abc1234 subject" of the base, for the header.
    base_label: SharedString,
    branch: SharedString,
    plan: Option<Plan>,
    error: Option<SharedString>,
    /// Merge commits in the range (a rebase flattens them).
    merges: usize,
    selected: usize,
    autostash: bool,
    message: Entity<TextareaState>,
    /// The row whose message the text area shows.
    message_row: Option<usize>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    _load: Task<()>,
    _message: Task<()>,
    _sub: Subscription,
}

impl EventEmitter<RebaseEditorEvent> for RebaseEditor {}

impl Focusable for RebaseEditor {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl RebaseEditor {
    /// An editor for the commits after `base` on `branch`; the plan loads in the
    /// background.
    pub fn new(
        repo: Repo,
        base: Oid,
        base_summary: &str,
        branch: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let message = cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 10));
        let sub = cx.subscribe_in(&message, window, |this, m, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev
                && let (Some(row), Some(plan)) = (this.message_row, this.plan.as_mut())
            {
                plan.set_message(row, &m.read(cx).value());
                cx.notify();
            }
        });
        let load = {
            let repo = repo.clone();
            let task = cx.background_executor().spawn(async move {
                let items = rebase::plan(&repo, Some(base))?;
                let merges = rebase::merges_after(&repo, Some(base)).unwrap_or(0);
                Ok::<_, switchhead_git::Error>((items, merges))
            });
            cx.spawn(async move |this, cx| {
                let res = task.await;
                let _ = this.update(cx, |e, cx| {
                    match res {
                        Ok((items, merges)) => {
                            e.plan = Some(Plan::new(items));
                            e.merges = merges;
                        }
                        Err(err) => e.error = Some(err.to_string().into()),
                    }
                    cx.notify();
                });
            })
        };
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            repo,
            base,
            base_label: format!("{} {}", switchhead_git::short(&base), base_summary).into(),
            branch,
            plan: None,
            error: None,
            merges: 0,
            selected: 0,
            autostash: true,
            message,
            message_row: None,
            focus,
            scroll: UniformListScrollHandle::new(),
            _load: load,
            _message: Task::ready(()),
            _sub: sub,
        }
    }

    fn select(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(len) = self.plan.as_ref().map(Plan::len) else {
            return;
        };
        if len == 0 {
            return;
        }
        self.selected = i.min(len - 1);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        self.bind_message(window, cx);
        cx.notify();
    }

    fn set_action(&mut self, i: usize, a: Action, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(plan) = self.plan.as_mut() {
            plan.set_action(i, a);
        }
        self.selected = i;
        self.bind_message(window, cx);
        if a == Action::Reword {
            self.message.update(cx, |m, cx| m.focus(window, cx));
        }
        cx.notify();
    }

    fn move_row(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.plan.as_mut() else {
            return;
        };
        let to = to.min(plan.len().saturating_sub(1));
        plan.move_row(from, to);
        self.message_row = None;
        self.select(to, window, cx);
    }

    /// Point the message area at the selected row when it takes a message (reword, or
    /// squash for the combined commit). A reword starts from the commit's own message.
    fn bind_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.plan.as_ref().and_then(|p| p.rows().get(self.selected)) else {
            return;
        };
        let takes = matches!(row.action, Action::Reword | Action::Squash);
        let target = takes.then_some(self.selected);
        if target == self.message_row {
            return;
        }
        self.message_row = target;
        let (id, existing, action) = (row.id, row.message.clone(), row.action);
        let placeholder = if action == Action::Squash {
            "Message of the combined commit. Leave empty to keep every message."
        } else {
            "New commit message"
        };
        self.message.update(cx, |m, cx| {
            m.set_placeholder(placeholder, window, cx);
            m.set_value(existing.clone().unwrap_or_default(), window, cx);
        });
        if existing.is_some() || action != Action::Reword {
            return;
        }
        let repo = self.repo.clone();
        let task = cx
            .background_executor()
            .spawn(async move { switchhead_git::commit::info(&repo, id).map(|i| i.message) });
        self._message = cx.spawn_in(window, async move |this, cx| {
            let Ok(message) = task.await else { return };
            let _ = this.update_in(cx, |e, window, cx| {
                let still = e
                    .message_row
                    .and_then(|r| e.plan.as_ref()?.rows().get(r))
                    .is_some_and(|r| r.id == id && r.message.is_none());
                if still {
                    let text = message.trim_end().to_owned();
                    e.message
                        .update(cx, |m, cx| m.set_value(text.clone(), window, cx));
                    if let (Some(r), Some(plan)) = (e.message_row, e.plan.as_mut()) {
                        plan.set_message(r, &text);
                    }
                    cx.notify();
                }
            });
        });
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(plan) = &self.plan else { return };
        if !plan.is_changed() || plan.problem().is_some() {
            return;
        }
        cx.emit(RebaseEditorEvent::Start {
            base: self.base,
            items: plan.todo(),
            autostash: self.autostash,
        });
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keys typed into the message area are its own.
        if !self.focus.is_focused(window) {
            return;
        }
        let k = &ev.keystroke;
        let alt = k.modifiers.alt;
        let i = self.selected;
        let action = match k.key.as_str() {
            "p" => Some(Action::Pick),
            "r" => Some(Action::Reword),
            "e" => Some(Action::Edit),
            "s" => Some(Action::Squash),
            "f" => Some(Action::Fixup),
            "d" | "delete" | "backspace" => Some(Action::Drop),
            _ => None,
        };
        if let Some(a) = action
            && !k.modifiers.secondary()
            && !alt
        {
            self.set_action(i, a, window, cx);
            cx.stop_propagation();
            return;
        }
        match (k.key.as_str(), alt) {
            ("up", true) if i > 0 => self.move_row(i, i - 1, window, cx),
            ("down", true) => self.move_row(i, i + 1, window, cx),
            ("up", false) => self.select(i.saturating_sub(1), window, cx),
            ("down", false) => self.select(i + 1, window, cx),
            ("escape", _) => cx.emit(RebaseEditorEvent::Cancel),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for RebaseEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let count = self.plan.as_ref().map_or(0, Plan::len);
        let problem = self.plan.as_ref().and_then(Plan::problem);
        let changed = self.plan.as_ref().is_some_and(Plan::is_changed);
        let can_start = changed && problem.is_none();
        let header = div()
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
                    .child("Interactive Rebase"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ts::BODY)
                    .text_color(p.fg2)
                    .child(match count {
                        1 => format!("{}: 1 commit after {}", self.branch, self.base_label),
                        n => format!("{}: {n} commits after {}", self.branch, self.base_label),
                    }),
            )
            .child(
                ui::button("rebase-cancel", "Cancel", ui::Kind::Secondary, &p)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(RebaseEditorEvent::Cancel))),
            )
            .child(
                ui::button("rebase-start", "Start Rebase", ui::Kind::Primary, &p)
                    .when(!can_start, |d| d.opacity(0.5))
                    .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
            );
        let body: AnyElement = if let Some(err) = &self.error {
            div()
                .flex_1()
                .p(rpx(16.))
                .text_size(ts::BODY)
                .text_color(p.deleted)
                .child(err.clone())
                .into_any_element()
        } else if self.plan.is_none() {
            div().flex_1().into_any_element()
        } else if self.plan.as_ref().is_some_and(Plan::is_empty) {
            div()
                .flex_1()
                .p(rpx(16.))
                .text_size(ts::BODY)
                .text_color(p.fg3)
                .child("There are no commits after this one to rebase.")
                .into_any_element()
        } else {
            self.render_list(cx)
        };
        let show_message = self.message_row.is_some();
        let message_title = self
            .message_row
            .and_then(|r| self.plan.as_ref()?.rows().get(r))
            .map(|r| {
                if r.action == Action::Squash {
                    "MESSAGE OF THE COMBINED COMMIT"
                } else {
                    "NEW MESSAGE"
                }
            })
            .unwrap_or_default();
        let result = self.plan.as_ref().map(|p| (p.len(), p.result_count()));
        div()
            .id("rebase-editor")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .bg(p.bg)
            .child(header)
            .when(self.merges > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px(rpx(12.))
                        .py(rpx(6.))
                        .bg(p.modified.opacity(0.12))
                        .text_size(ts::BODY)
                        .text_color(p.fg)
                        .child(format!(
                            "This range holds {} merge commit{}: the rebase replays their \
                             changes as plain commits and the merges themselves go away.",
                            self.merges,
                            if self.merges == 1 { "" } else { "s" }
                        )),
                )
            })
            .child(body)
            .when(show_message, |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(rpx(6.))
                        .p(rpx(12.))
                        .border_t_1()
                        .border_color(p.bd)
                        .bg(p.panel)
                        .child(ui::caption(message_title, &p))
                        .child(
                            Textarea::new(&self.message)
                                .small()
                                .text_size(ts::BODY)
                                .font_family(MONO),
                        ),
                )
            })
            .child(
                div()
                    .flex_none()
                    .h(rpx(32.))
                    .px(rpx(12.))
                    .flex()
                    .items_center()
                    .gap(rpx(12.))
                    .border_t_1()
                    .border_color(p.bd)
                    .bg(p.panel)
                    .text_size(ts::SMALL)
                    .text_color(p.fg3)
                    .child(
                        ui::checkbox(
                            "rebase-autostash",
                            self.autostash,
                            "Stash and reapply local changes",
                            &p,
                        )
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.autostash = !this.autostash;
                            cx.notify();
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when_some(problem.clone(), |d, pr| d.text_color(p.deleted).child(pr))
                            .when(problem.is_none(), |d| {
                                d.child(
                                    "Drag rows or Alt+↑/↓ to reorder · \
                                     P R E S F D set the action · Esc cancels",
                                )
                            }),
                    )
                    .when_some(result, |d, (from, to)| {
                        d.child(
                            div()
                                .flex_none()
                                .font_family(MONO)
                                .child(format!("{from} → {to} commits")),
                        )
                    }),
            )
    }
}

impl RebaseEditor {
    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(plan) = &self.plan else {
            return div().into_any_element();
        };
        let rows: Vec<(TodoItem, Option<SharedString>)> = (0..plan.len())
            .map(|i| {
                let into = plan
                    .meld_target(i)
                    .map(|j| switchhead_git::short(&plan.rows()[j].id).into());
                (plan.rows()[i].clone(), into)
            })
            .collect();
        let selected = self.selected;
        uniform_list(
            "rebase-rows",
            rows.len(),
            cx.processor(move |_, range: Range<usize>, _, cx| {
                let p = palette(cx);
                range
                    .filter_map(|i| rows.get(i).cloned().map(|r| (i, r)))
                    .map(|(i, (item, into))| render_row(i, &item, into, i == selected, &p, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .min_h_0()
        .py(rpx(4.))
        .into_any_element()
    }
}

fn action_color(a: Action, p: &Palette) -> gpui_kit::Hsla {
    match a {
        Action::Pick => p.fg2,
        Action::Reword | Action::Edit => p.acc,
        Action::Squash | Action::Fixup => p.modified,
        Action::Drop => p.deleted,
    }
}

fn render_row(
    i: usize,
    item: &TodoItem,
    into: Option<SharedString>,
    selected: bool,
    p: &Palette,
    cx: &mut Context<RebaseEditor>,
) -> AnyElement {
    let (label, _) = action_label(item.action);
    let color = action_color(item.action, p);
    let dropped = item.action == Action::Drop;
    let melded = matches!(item.action, Action::Squash | Action::Fixup);
    let drag = DragRow {
        from: i,
        label: item.summary.clone().into(),
    };
    let entity = cx.entity().downgrade();
    let menu = Button::new(("rebase-action", i))
        .ghost()
        .xsmall()
        .label(label)
        .text_color(color)
        .dropdown_menu(move |mut menu, _, _| {
            for a in ACTIONS {
                let (name, key) = action_label(a);
                let entity = entity.clone();
                menu = menu.item(PopupMenuItem::new(format!("{name}  ({key})")).on_click(
                    move |_, window, cx| {
                        let _ = entity.update(cx, |e, cx| e.set_action(i, a, window, cx));
                    },
                ));
            }
            menu
        });
    let acc = p.acc;
    div()
        .id(("rebase-row", i))
        .w_full()
        .h(rpx(28.))
        .flex()
        .items_center()
        .gap(rpx(8.))
        .px(rpx(8.))
        .when(selected, |d| d.bg(p.sel))
        .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
        .on_mouse_down(
            gpui_kit::MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.select(i, window, cx);
            }),
        )
        .on_drag(drag, |d, _, _, cx| cx.new(|_| DragPreview(d.label.clone())))
        .drag_over::<DragRow>(move |s, d, _, _| {
            // The row lands where this one is: below it when dragged down, above when up.
            let s = s.border_color(acc);
            if d.from < i {
                s.border_b_2()
            } else {
                s.border_t_2()
            }
        })
        .on_drop(
            cx.listener(move |this, d: &DragRow, window, cx| this.move_row(d.from, i, window, cx)),
        )
        .child(
            div()
                .flex_none()
                .w(rpx(12.))
                .cursor_grab()
                .text_size(ts::BODY)
                .text_color(p.fg3)
                .child("⋮⋮"),
        )
        .child(div().flex_none().w(rpx(72.)).child(menu))
        .child(
            div()
                .flex_none()
                .font_family(MONO)
                .text_size(ts::SMALL)
                .text_color(p.fg3)
                .child(switchhead_git::short(&item.id)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(ts::BODY)
                .text_color(if dropped || melded { p.fg3 } else { p.fg })
                .when(dropped, |d| d.line_through())
                .child(
                    item.message
                        .as_deref()
                        .and_then(|m| m.lines().next())
                        .filter(|_| item.action == Action::Reword)
                        .unwrap_or(&item.summary)
                        .to_owned(),
                ),
        )
        .when_some(into, |d, into| {
            d.child(
                div()
                    .flex_none()
                    .text_size(ts::SMALL)
                    .text_color(p.modified)
                    .child(format!("into {into} ↓")),
            )
        })
        .when(
            item.message.is_some() && item.action == Action::Squash,
            |d| d.child(ui::chip("new message", p.acc)),
        )
        .into_any_element()
}
