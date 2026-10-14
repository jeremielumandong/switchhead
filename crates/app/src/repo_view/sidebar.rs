//! The refs sidebar: Local Changes / All Commits, a filter box, then branches (a tree
//! split on `/`), remotes, tags, stashes, submodules and worktrees.
//!
//! Click selects a ref's commit in the graph, double-click checks it out (or opens a
//! submodule or worktree in a tab), Alt-click shows only that ref's history.

use std::ops::Range;
use std::path::PathBuf;

use gpui_kit::AppContext as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::{Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, svg, uniform_list,
};
use switchhead_git::Oid;
use switchhead_git::log::Scope;
use switchhead_git::worktrees::SubmoduleState;

use super::{Mode, OpStatus, RepoView, RepoViewEvent, ahead_behind};
use crate::appearance::{rpx, ts};
use crate::sidebar_model::{self, TreeRow};
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// A sidebar section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    /// Local branches.
    Branches,
    /// Remote-tracking branches.
    Remotes,
    /// Tags.
    Tags,
    /// Stashes.
    Stashes,
    /// Submodules.
    Submodules,
    /// Worktrees.
    Worktrees,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::Branches => "Branches",
            Section::Remotes => "Remotes",
            Section::Tags => "Tags",
            Section::Stashes => "Stashes",
            Section::Submodules => "Submodules",
            Section::Worktrees => "Worktrees",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::Branches => "icons/sh-git-branch.svg",
            Section::Remotes => "icons/sh-cloud.svg",
            Section::Tags => "icons/sh-tag.svg",
            Section::Stashes => "icons/sh-archive.svg",
            Section::Submodules => "icons/sh-folder-git-2.svg",
            Section::Worktrees => "icons/sh-git-fork.svg",
        }
    }
}

/// What double-clicking a row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Check out a local branch.
    Branch(String),
    /// Check out a remote branch (`origin/x`): its local branch, made tracking if new.
    Remote(String),
    /// Check out a tag (detached; asks first).
    Tag(String),
    /// Stash `stash@{n}`: a click shows it in the details pane.
    Stash(usize),
    /// Open a folder (a worktree or submodule) in a tab.
    Open(PathBuf),
    /// Nothing (the current worktree, an uninitialized submodule).
    None,
}

/// One line of the sidebar (flattened, so the list can be virtualized).
#[derive(Clone, Debug)]
pub enum SideRow {
    /// A section header with its (filtered) count.
    Header(Section, usize),
    /// A folder of a name tree, or a remote's name above its branches.
    Folder {
        key: String,
        name: SharedString,
        depth: usize,
        open: bool,
    },
    /// A ref, submodule or worktree.
    Ref {
        text: SharedString,
        /// The commit to select on click (`None`: not in this repository's graph).
        id: Option<Oid>,
        /// Shown on the right (ahead/behind, `@{0}`, a state).
        detail: Option<SharedString>,
        /// HEAD's branch, or this worktree.
        current: bool,
        /// The revision Alt-click filters the graph to.
        rev: Option<String>,
        depth: usize,
        target: Target,
    },
}

/// A leaf before it becomes a row: its full name (for the tree and the filter) and row.
struct Leaf {
    name: String,
    text: SharedString,
    id: Option<Oid>,
    detail: Option<SharedString>,
    current: bool,
    rev: Option<String>,
    target: Target,
}

impl Leaf {
    fn row(self, text: Option<String>, depth: usize) -> SideRow {
        SideRow::Ref {
            text: text.map_or(self.text, SharedString::from),
            id: self.id,
            detail: self.detail,
            current: self.current,
            rev: self.rev,
            depth,
            target: self.target,
        }
    }
}

impl RepoView {
    /// Flatten the refs into sidebar rows (after refs or the filter change).
    pub(super) fn rebuild_side(&mut self) {
        let filter = self.side_filter_text.trim().to_owned();
        let filtering = !filter.is_empty();
        let mut rows = Vec::new();
        for section in [
            Section::Branches,
            Section::Remotes,
            Section::Tags,
            Section::Stashes,
            Section::Submodules,
            Section::Worktrees,
        ] {
            let leaves: Vec<Leaf> = self
                .section_leaves(section)
                .into_iter()
                .filter(|l| sidebar_model::matches(&l.name, &filter))
                .collect();
            let always = matches!(
                section,
                Section::Branches | Section::Remotes | Section::Tags | Section::Stashes
            );
            if leaves.is_empty() && (filtering || !always) {
                continue;
            }
            rows.push(SideRow::Header(section, leaves.len()));
            if self.collapsed.contains(&section) && !filtering {
                continue;
            }
            match section {
                Section::Branches => self.push_tree(&mut rows, leaves, "b", 0, filtering),
                Section::Remotes => {
                    // One folder per remote, its branches as a tree under it.
                    let mut groups: Vec<(String, Vec<Leaf>)> = Vec::new();
                    for leaf in leaves {
                        let remote = leaf.name.split('/').next().unwrap_or("").to_owned();
                        match groups.last_mut() {
                            Some((r, ls)) if *r == remote => ls.push(leaf),
                            _ => groups.push((remote, vec![leaf])),
                        }
                    }
                    // Remotes nothing was fetched from yet still get their folder.
                    if !filtering {
                        for name in self.refs.remote_names() {
                            if !groups.iter().any(|(r, _)| r == name) {
                                groups.push((name.to_owned(), Vec::new()));
                            }
                        }
                        groups.sort_by(|a, b| a.0.cmp(&b.0));
                    }
                    for (remote, mut leaves) in groups {
                        let key = format!("r/{remote}");
                        let open = filtering || !self.collapsed_folders.contains(&key);
                        rows.push(SideRow::Folder {
                            key: key.clone(),
                            name: remote.clone().into(),
                            depth: 0,
                            open,
                        });
                        if open {
                            for l in &mut leaves {
                                l.name = l.name[remote.len() + 1..].to_owned();
                            }
                            self.push_tree(&mut rows, leaves, &key, 1, filtering);
                        }
                    }
                }
                Section::Tags | Section::Stashes | Section::Submodules | Section::Worktrees => {
                    rows.extend(leaves.into_iter().map(|l| l.row(None, 0)));
                }
            }
        }
        self.side_rows = rows;
    }

    /// Lay `leaves` out as a `/` tree at `depth`.
    fn push_tree(
        &self,
        rows: &mut Vec<SideRow>,
        leaves: Vec<Leaf>,
        scope: &str,
        depth: usize,
        filtering: bool,
    ) {
        let names: Vec<&str> = leaves.iter().map(|l| l.name.as_str()).collect();
        let tree = sidebar_model::tree(&names, scope, &self.collapsed_folders, filtering);
        let mut leaves: Vec<Option<Leaf>> = leaves.into_iter().map(Some).collect();
        for row in tree {
            match row {
                TreeRow::Folder {
                    key,
                    name,
                    depth: d,
                    open,
                } => rows.push(SideRow::Folder {
                    key,
                    name: name.into(),
                    depth: depth + d,
                    open,
                }),
                TreeRow::Leaf {
                    index,
                    name,
                    depth: d,
                } => {
                    if let Some(leaf) = leaves.get_mut(index).and_then(Option::take) {
                        rows.push(leaf.row(Some(name), depth + d));
                    }
                }
            }
        }
    }

    /// A section's entries, in order.
    fn section_leaves(&self, section: Section) -> Vec<Leaf> {
        let r = &self.refs;
        match section {
            Section::Branches => r
                .branches
                .iter()
                .map(|b| Leaf {
                    name: b.name.clone(),
                    text: b.name.clone().into(),
                    id: Some(b.id),
                    detail: b.tracking.as_ref().and_then(|t| {
                        if t.gone {
                            Some("gone".into())
                        } else if t.ahead + t.behind > 0 {
                            Some(ahead_behind(t.ahead, t.behind).into())
                        } else {
                            None
                        }
                    }),
                    current: b.is_head,
                    rev: Some(format!("refs/heads/{}", b.name)),
                    target: Target::Branch(b.name.clone()),
                })
                .collect(),
            Section::Remotes => r
                .remotes
                .iter()
                .filter(|b| !b.is_symbolic_head)
                .map(|b| Leaf {
                    name: b.short(),
                    text: b.name.clone().into(),
                    id: Some(b.id),
                    detail: None,
                    current: false,
                    rev: Some(format!("refs/remotes/{}", b.short())),
                    target: Target::Remote(b.short()),
                })
                .collect(),
            Section::Tags => r
                .tags
                .iter()
                .map(|t| Leaf {
                    name: t.name.clone(),
                    text: t.name.clone().into(),
                    id: Some(t.id),
                    detail: None,
                    current: false,
                    rev: Some(format!("refs/tags/{}", t.name)),
                    target: Target::Tag(t.name.clone()),
                })
                .collect(),
            Section::Stashes => r
                .stashes
                .iter()
                .map(|s| Leaf {
                    name: s.message.clone(),
                    text: s.message.clone().into(),
                    id: Some(s.id),
                    detail: Some(format!("@{{{}}}", s.index).into()),
                    current: false,
                    rev: Some(s.id.to_string()),
                    target: Target::Stash(s.index),
                })
                .collect(),
            Section::Submodules => r
                .submodules
                .iter()
                .map(|s| {
                    let name = s.path.to_string_lossy().into_owned();
                    let detail = match s.state {
                        SubmoduleState::Current => None,
                        SubmoduleState::Uninitialized => Some("not cloned"),
                        SubmoduleState::Modified => Some("moved"),
                        SubmoduleState::Conflicted => Some("conflict"),
                    };
                    Leaf {
                        text: name.clone().into(),
                        name,
                        id: None,
                        detail: detail.map(SharedString::from),
                        current: false,
                        rev: None,
                        target: if s.state == SubmoduleState::Uninitialized {
                            Target::None
                        } else {
                            Target::Open(self.repo.root().join(&s.path))
                        },
                    }
                })
                .collect(),
            // Only worth a section when there is more than the repository's own.
            Section::Worktrees if r.worktrees.len() < 2 => Vec::new(),
            Section::Worktrees => r
                .worktrees
                .iter()
                .map(|w| {
                    let here = w.is_current;
                    let folder = w
                        .path
                        .file_name()
                        .map_or_else(|| w.path.to_string_lossy(), |n| n.to_string_lossy())
                        .into_owned();
                    let detail = if w.prunable {
                        Some("missing".to_owned())
                    } else {
                        w.branch
                            .clone()
                            .or_else(|| w.head.map(|h| format!("at {}", switchhead_git::short(&h))))
                    };
                    Leaf {
                        name: format!("{folder} {}", w.branch.as_deref().unwrap_or("")),
                        text: folder.into(),
                        id: w.head,
                        detail: detail.map(SharedString::from),
                        current: here,
                        rev: None,
                        target: if here || w.prunable {
                            Target::None
                        } else {
                            Target::Open(w.path.clone())
                        },
                    }
                })
                .collect(),
        }
    }

    pub(super) fn toggle_section(&mut self, s: Section, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&s) {
            self.collapsed.insert(s);
        }
        self.rebuild_side();
        cx.notify();
    }

    pub(super) fn toggle_folder(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed_folders.remove(key) {
            self.collapsed_folders.insert(key.to_owned());
        }
        self.rebuild_side();
        cx.notify();
    }

    /// The filter box (made on first render: an input needs the window).
    pub(super) fn ensure_side_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.side_filter.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let sub = cx.subscribe(&input, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let text = input.read(cx).value().to_string();
                if text != this.side_filter_text {
                    this.side_filter_text = text;
                    this.rebuild_side();
                    cx.notify();
                }
            }
        });
        self.side_filter = Some((input, sub));
    }

    /// Double-click: check out, or open in a tab.
    pub(super) fn activate_target(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.op, Some(OpStatus::Running { .. })) {
            return;
        }
        match target {
            Target::Branch(name) => {
                if self.refs.head_branch.as_deref() != Some(name.as_str()) {
                    self.run_op(
                        "Checking out",
                        "Checked out",
                        move |r, _, _| r.checkout(&name),
                        cx,
                    );
                }
            }
            Target::Remote(short) => {
                self.run_op(
                    "Checking out",
                    "Checked out",
                    move |r, _, _| r.checkout(&short),
                    cx,
                );
            }
            Target::Tag(name) => self.confirm_tag_checkout(name, window, cx),
            Target::Open(path) => cx.emit(RepoViewEvent::Open(path)),
            Target::Stash(_) | Target::None => {}
        }
    }

    fn confirm_tag_checkout(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let name = name.clone();
            alert
                .confirm()
                .title(format!("Check out tag {name}?"))
                .description(
                    "HEAD will be detached at the tag's commit. Create a branch there before \
                     committing, or the new commits belong to no branch.",
                )
                .ok_text("Check Out")
                .on_ok(move |_, _, cx| {
                    let rev = format!("refs/tags/{name}");
                    let _ = this.update(cx, |v, cx| {
                        v.run_op(
                            "Checking out",
                            "Checked out",
                            move |r, _, _| r.checkout(&rev),
                            cx,
                        )
                    });
                    true
                })
        });
    }

    pub(super) fn render_sidebar(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let count = self.side_rows.len();
        let list = uniform_list(
            "refs",
            count,
            cx.processor(move |this, range: Range<usize>, _w, cx| {
                let p = palette(cx);
                range
                    .filter_map(|i| this.side_rows.get(i).cloned().map(|r| (i, r)))
                    .map(|(i, row)| this.render_side_row(i, row, &p, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.side_scroll)
        .flex_1()
        .min_h_0()
        .py(rpx(4.));
        div()
            .w(rpx(240.))
            .flex_none()
            .flex()
            .flex_col()
            .bg(p.panel)
            .border_r_1()
            .border_color(p.bd)
            .child(
                div()
                    .h(rpx(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(rpx(12.))
                    .gap(rpx(8.))
                    .border_b_1()
                    .border_color(p.bd)
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_size(ts::UI)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title()),
                    )
                    .when(self.state != switchhead_git::RepoState::Clean, |d| {
                        d.child(ui::chip(self.state.label(), p.modified))
                    }),
            )
            .child(self.render_nav(p, cx))
            .when_some(self.side_filter.as_ref(), |d, (input, _)| {
                d.child(
                    div().flex_none().px(rpx(8.)).pt(rpx(6.)).child(
                        Input::new(input)
                            .small()
                            .cleanable(true)
                            .text_size(ts::BODY),
                    ),
                )
            })
            .when(
                count == 0 && !self.side_filter_text.trim().is_empty(),
                |d| {
                    d.child(
                        div()
                            .px(rpx(12.))
                            .py(rpx(8.))
                            .text_size(ts::SMALL)
                            .text_color(p.fg3)
                            .child("No refs match"),
                    )
                },
            )
            .child({
                let view = cx.entity().downgrade();
                div()
                    .id("refs-menu")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(list)
                    .context_menu(move |menu, window, cx| {
                        super::branch_ops::build_ref_menu(menu, &view, window, cx)
                    })
            })
            .into_any_element()
    }

    /// "Local Changes" and "All Commits" above the refs.
    fn render_nav(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let changed = self.changes.read(cx).count();
        let item = |id: &'static str, icon: &'static str, label: &'static str, mode: Mode| {
            let active = self.mode == mode;
            div()
                .id(id)
                .h(rpx(26.))
                .flex()
                .items_center()
                .gap(rpx(8.))
                .px(rpx(10.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(ts::BODY)
                .text_color(if active { p.fg } else { p.fg2 })
                .when(active, |d| d.bg(p.sel).font_weight(FontWeight::SEMIBOLD))
                .when(!active, |d| d.hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |this, _, window, cx| this.set_mode(mode, window, cx)))
                .child(svg().path(icon).size(rpx(14.)).text_color(p.fg3))
                .child(div().flex_1().child(label))
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(rpx(1.))
            .p(rpx(4.))
            .border_b_1()
            .border_color(p.bd)
            .child(
                item(
                    "nav-changes",
                    "icons/sh-file-diff.svg",
                    "Local Changes",
                    Mode::Changes,
                )
                .when(changed > 0, |d| {
                    d.child(
                        div()
                            .px(rpx(6.))
                            .rounded(px(8.))
                            .bg(p.acc.opacity(0.16))
                            .font_family(MONO)
                            .text_size(ts::CAPTION)
                            .text_color(p.acc)
                            .child(changed.to_string()),
                    )
                }),
            )
            .child(item(
                "nav-commits",
                "icons/sh-git-commit-horizontal.svg",
                "All Commits",
                Mode::Commits,
            ))
            .into_any_element()
    }

    fn render_side_row(
        &self,
        i: usize,
        row: SideRow,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let base = div()
            .id(("side", i))
            .w_full()
            .h(rpx(24.))
            .flex()
            .items_center()
            .gap(rpx(6.))
            .px(rpx(10.))
            .text_size(ts::BODY);
        let indent = |depth: usize| rpx(28. + 12. * depth as f32);
        match row {
            SideRow::Header(section, n) => {
                let open = !self.collapsed.contains(&section);
                base.mt(rpx(if i == 0 { 0. } else { 6. }))
                    .cursor_pointer()
                    .text_color(p.fg2)
                    .hover(|s| s.text_color(p.fg))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx)))
                    .capture_any_mouse_down(cx.listener(move |this, ev: &MouseDownEvent, _, _| {
                        if ev.button == MouseButton::Right {
                            this.menu_row = Some(i);
                        }
                    }))
                    .child(disclosure(open, p))
                    .child(svg().path(section.icon()).size(rpx(13.)).text_color(p.fg3))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(ts::LABEL)
                            .child(section.title()),
                    )
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(ts::CAPTION)
                            .text_color(p.fg3)
                            .child(n.to_string()),
                    )
                    .into_any_element()
            }
            SideRow::Folder {
                key,
                name,
                depth,
                open,
            } => base
                .pl(rpx(14. + 12. * depth as f32))
                .cursor_pointer()
                .rounded(px(4.))
                .text_color(p.fg3)
                .hover(|s| s.bg(p.hover).text_color(p.fg2))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_folder(&key, cx)))
                .capture_any_mouse_down(cx.listener(move |this, ev: &MouseDownEvent, _, _| {
                    if ev.button == MouseButton::Right {
                        this.menu_row = Some(i);
                    }
                }))
                .child(disclosure(open, p))
                .child(div().flex_1().min_w_0().truncate().child(name))
                .into_any_element(),
            SideRow::Ref {
                text,
                id,
                detail,
                current,
                rev,
                depth,
                target,
            } => {
                let filtered = rev
                    .as_ref()
                    .is_some_and(|r| self.scope == Scope::Revs(vec![r.clone()]));
                let selected =
                    id.is_some_and(|id| self.selected.is_some_and(|s| self.log.id(s) == id));
                let gone = detail
                    .as_ref()
                    .is_some_and(|d| d == "gone" || d == "missing");
                base.pl(indent(depth))
                    .cursor_pointer()
                    .rounded(px(4.))
                    .text_color(if current { p.fg } else { p.fg2 })
                    .when(current, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .when(selected, |d| d.bg(p.sel))
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        this.click_row(ev, id, rev.clone(), target.clone(), window, cx)
                    }))
                    .capture_any_mouse_down(cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        if ev.button == MouseButton::Right {
                            this.menu_row = Some(i);
                            cx.notify();
                        }
                    }))
                    .child(ui::dot(
                        if current {
                            p.acc
                        } else {
                            gpui_kit::transparent_black()
                        },
                        6.,
                    ))
                    .child(div().flex_1().min_w_0().truncate().child(text))
                    .when(filtered, |d| d.child(ui::chip("only", p.acc)))
                    .when_some(detail, |d, t| {
                        d.child(
                            div()
                                .flex_none()
                                .px(rpx(5.))
                                .rounded(px(7.))
                                .bg(if gone { p.deleted } else { p.fg3 }.opacity(0.14))
                                .font_family(MONO)
                                .text_size(ts::CAPTION)
                                .text_color(if gone { p.deleted } else { p.fg2 })
                                .child(t),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    fn click_row(
        &mut self,
        ev: &ClickEvent,
        id: Option<Oid>,
        rev: Option<String>,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ev.modifiers().alt {
            // Alt-click: show only this ref's history (again: all).
            if let Some(rev) = rev {
                if self.mode != Mode::Commits {
                    self.set_mode(Mode::Commits, window, cx);
                }
                let scope = if self.scope == Scope::Revs(vec![rev.clone()]) {
                    Scope::All
                } else {
                    Scope::Revs(vec![rev])
                };
                self.pending_select = id;
                self.set_scope(scope, cx);
            }
            return;
        }
        if ev.click_count() >= 2 {
            self.activate_target(target, window, cx);
            return;
        }
        if let (Target::Stash(_), Some(id)) = (&target, id) {
            if self.mode != Mode::Commits {
                self.set_mode(Mode::Commits, window, cx);
            }
            self.show_stash(id, cx);
            return;
        }
        if let Some(id) = id {
            if self.mode != Mode::Commits {
                self.set_mode(Mode::Commits, window, cx);
            }
            match self.log.row_of(&id) {
                Some(row) => self.select(row, cx),
                None => self.pending_select = Some(id),
            }
            cx.notify();
        }
    }
}

/// ▾ / ▸.
fn disclosure(open: bool, p: &Palette) -> AnyElement {
    div()
        .w(rpx(10.))
        .flex_none()
        .text_size(ts::CAPTION)
        .text_color(p.fg3)
        .child(if open { "▾" } else { "▸" })
        .into_any_element()
}
