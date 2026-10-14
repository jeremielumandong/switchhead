//! A repository tab: the refs sidebar and the commit graph.
//!
//! Opening the tab starts a log walk on its own thread. Refs arrive first, then the
//! first batch of commits with their graph rows (the list renders as soon as it lands),
//! then the rest in larger batches; upstream counts come last. Nothing here blocks the UI
//! thread: work runs on threads and comes back over a channel to a GPUI task.
//!
//! The graph is a virtualized list. Each visible row paints its lanes on a canvas from
//! the precomputed [`GraphRows`]; the rest of the row is text read from the columnar
//! [`CommitLog`].

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, UniformListScrollHandle, Window, canvas,
    div, px, svg, uniform_list,
};
use switchhead_git::cli::Cancel;
use switchhead_git::graph::{GraphRows, Node, Segment};
use switchhead_git::log::{self, CommitLog, LogOptions, Scope, WalkStats};
use switchhead_git::refs::{self, Label, LabelKind, Refs};
use switchhead_git::{Oid, Repo, RepoState};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::actions;
use crate::appearance::{rpx, ts};
use crate::graph_paint;
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// Graph row height at 100 % zoom.
const ROW_H: f32 = 24.0;
/// Rows Page Up / Page Down move.
const PAGE: isize = 20;
/// The graph column never grows past this many lanes (wider rows are clipped).
const MAX_LANES: u16 = 24;

/// Something from the background walk.
enum Msg {
    Refs(Box<Refs>),
    Batch(Box<log::Batch>),
    Done(Result<WalkStats, String>),
    Tracking(Box<Refs>),
}

/// A git operation started from the toolbar.
#[derive(Clone, Debug)]
pub enum OpStatus {
    /// Running; the latest progress line.
    Running {
        /// What runs ("Fetching").
        label: SharedString,
        /// Last progress line from git.
        progress: SharedString,
    },
    /// Finished well.
    Done(SharedString),
    /// Failed with git's message.
    Failed(SharedString),
}

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
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::Branches => "Branches",
            Section::Remotes => "Remotes",
            Section::Tags => "Tags",
            Section::Stashes => "Stashes",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::Branches => "icons/sh-git-branch.svg",
            Section::Remotes => "icons/sh-cloud.svg",
            Section::Tags => "icons/sh-tag.svg",
            Section::Stashes => "icons/sh-archive.svg",
        }
    }
}

/// One line of the sidebar (flattened, so the list can be virtualized).
#[derive(Clone, Debug)]
enum SideRow {
    Header(Section, usize),
    /// A ref: label, the commit, a detail on the right (ahead/behind), current branch,
    /// and the revision to filter the graph by.
    Ref {
        text: SharedString,
        id: Oid,
        detail: Option<SharedString>,
        current: bool,
        rev: String,
        indent: bool,
    },
    /// A remote's name above its branches.
    Group(SharedString),
}

/// A repository tab.
pub struct RepoView {
    /// The repository.
    pub repo: Repo,
    log: CommitLog,
    graph: GraphRows,
    refs: Refs,
    decorations: HashMap<Oid, Vec<Label>>,
    /// What the graph shows.
    pub scope: Scope,
    /// The walk is still sending commits.
    pub loading: bool,
    /// The last finished walk.
    pub stats: Option<WalkStats>,
    /// Why the log could not be read.
    pub error: Option<SharedString>,
    /// The operation in progress (merge, rebase...).
    pub state: RepoState,
    /// Toolbar operation status.
    pub op: Option<OpStatus>,
    /// Show the refs sidebar.
    pub sidebar: bool,
    selected: Option<usize>,
    /// A commit to select once its batch arrives.
    pending_select: Option<Oid>,
    scroll: UniformListScrollHandle,
    side_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    cancel: Cancel,
    collapsed: HashSet<Section>,
    side_rows: Vec<SideRow>,
    _walk: Option<Task<()>>,
    _op: Option<Task<()>>,
}

impl RepoView {
    /// A tab for `repo`; starts loading at once.
    pub fn new(repo: Repo, cx: &mut Context<Self>) -> Self {
        let mut v = Self {
            state: repo.state(),
            repo,
            log: CommitLog::new(),
            graph: GraphRows::new(),
            refs: Refs::default(),
            decorations: HashMap::new(),
            scope: Scope::All,
            loading: false,
            stats: None,
            error: None,
            op: None,
            sidebar: true,
            selected: None,
            pending_select: None,
            scroll: UniformListScrollHandle::new(),
            side_scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            cancel: Cancel::default(),
            collapsed: HashSet::from([Section::Tags]),
            side_rows: Vec::new(),
            _walk: None,
            _op: None,
        };
        v.reload(cx);
        v
    }

    /// The tab's title.
    pub fn title(&self) -> SharedString {
        self.repo.name().to_owned().into()
    }

    /// HEAD's branch, or a short id when detached.
    pub fn head_label(&self) -> SharedString {
        match (&self.refs.head_branch, self.refs.head) {
            (Some(b), _) => b.clone().into(),
            (None, Some(id)) => format!("detached at {}", switchhead_git::short(&id)).into(),
            (None, None) => "no commits yet".into(),
        }
    }

    /// Commits loaded so far.
    pub fn commit_count(&self) -> usize {
        self.log.len()
    }

    /// Re-read refs and the log (keeps the selection when the commit is still there).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        let cancel = Cancel::default();
        self.cancel = cancel.clone();
        self.pending_select = self.selected.map(|i| self.log.id(i));
        self.loading = true;
        self.error = None;
        self.state = self.repo.state();
        let (tx, mut rx) = unbounded_channel();
        let repo = self.repo.clone();
        let opts = LogOptions {
            scope: self.scope.clone(),
            ..LogOptions::default()
        };
        let spawned = std::thread::Builder::new()
            .name("log-walk".into())
            .spawn(move || walk_thread(repo, opts, cancel, tx));
        if let Err(e) = spawned {
            self.loading = false;
            self.error = Some(format!("could not start the log walk: {e}").into());
            return;
        }
        let mut fresh = true;
        self._walk = Some(cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.recv().await {
                // Take whatever else is already waiting, then redraw once.
                let mut msgs = vec![msg];
                while let Ok(m) = rx.try_recv() {
                    msgs.push(m);
                }
                let ok = this.update(cx, |v, cx| {
                    for m in msgs {
                        v.apply(m, &mut fresh);
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        }));
    }

    fn apply(&mut self, msg: Msg, fresh: &mut bool) {
        match msg {
            Msg::Refs(refs) | Msg::Tracking(refs) => {
                self.decorations = refs.decorations();
                self.refs = *refs;
                self.rebuild_side();
            }
            Msg::Batch(b) => {
                if std::mem::take(fresh) {
                    // The first batch of a reload replaces the old log.
                    self.log = CommitLog::new();
                    self.graph = GraphRows::new();
                    self.selected = None;
                }
                self.log.append(&b.commits);
                self.graph.append(&b.graph);
                if let Some(id) = self.pending_select
                    && let Some(row) = self.log.row_of(&id)
                {
                    self.pending_select = None;
                    self.select(row);
                }
            }
            Msg::Done(res) => {
                self.loading = false;
                if std::mem::take(fresh) {
                    // Nothing to show (an unborn branch, or a scope with no commits).
                    self.log = CommitLog::new();
                    self.graph = GraphRows::new();
                    self.selected = None;
                }
                match res {
                    Ok(stats) => {
                        tracing::debug!(
                            repo = self.repo.name(),
                            commits = stats.commits,
                            first_ms = stats.first_batch_ms,
                            ms = stats.total_ms,
                            "log loaded"
                        );
                        self.stats = Some(stats);
                    }
                    Err(e) => self.error = Some(e.into()),
                }
            }
        }
    }

    fn select(&mut self, row: usize) {
        if row < self.log.len() {
            self.selected = Some(row);
            self.scroll.scroll_to_item(row, ScrollStrategy::Center);
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.log.len();
        if n == 0 {
            return;
        }
        let cur = self.selected.map_or(-1, |s| s as isize);
        let next = (cur + delta).clamp(0, n as isize - 1) as usize;
        self.selected = Some(next);
        self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Select HEAD's commit (it is loaded first in every scope that contains it).
    pub fn jump_to_head(&mut self, cx: &mut Context<Self>) {
        if let Some(head) = self.refs.head {
            match self.log.row_of(&head) {
                Some(row) => self.select(row),
                None => self.pending_select = Some(head),
            }
            cx.notify();
        }
    }

    /// Show only `rev` (`None`: every branch) and reload.
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.reload(cx);
            cx.notify();
        }
    }

    // ---------------------------------------------------------------- operations

    /// Run a git operation on a thread, show its progress, reload when it ends.
    pub fn run_op(
        &mut self,
        label: &'static str,
        done: &'static str,
        f: impl FnOnce(&Repo, &Cancel, &mut dyn FnMut(&str)) -> switchhead_git::Result<()>
        + Send
        + 'static,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.op, Some(OpStatus::Running { .. })) {
            return;
        }
        self.op = Some(OpStatus::Running {
            label: label.into(),
            progress: SharedString::default(),
        });
        cx.notify();
        let (tx, mut rx) = unbounded_channel::<Result<String, String>>();
        let repo = self.repo.clone();
        let cancel = Cancel::default();
        let spawned = std::thread::Builder::new()
            .name("git-op".into())
            .spawn(move || {
                let progress = tx.clone();
                let mut on_line = |l: &str| {
                    let _ = progress.send(Ok(l.to_owned()));
                };
                let res = f(&repo, &cancel, &mut on_line).map_err(|e| e.to_string());
                let _ = tx.send(match res {
                    Ok(()) => Err(String::new()),
                    Err(e) if e.is_empty() => Err("failed".into()),
                    Err(e) => Err(e),
                });
            });
        if let Err(e) = spawned {
            self.op = Some(OpStatus::Failed(format!("{label} failed: {e}").into()));
            return;
        }
        // Progress arrives as `Ok(line)`; the end as `Err(message)`, empty on success.
        self._op = Some(cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.recv().await {
                let end = msg.is_err();
                let ok = this.update(cx, |v, cx| {
                    match msg {
                        Ok(line) => {
                            let line = line.trim();
                            if !line.is_empty() {
                                v.op = Some(OpStatus::Running {
                                    label: label.into(),
                                    progress: line.to_owned().into(),
                                });
                            }
                        }
                        Err(e) if e.is_empty() => {
                            v.op = Some(OpStatus::Done(done.into()));
                            v.reload(cx);
                        }
                        Err(e) => {
                            v.op = Some(OpStatus::Failed(first_line(&e).into()));
                            v.reload(cx);
                        }
                    }
                    cx.notify();
                });
                if ok.is_err() || end {
                    break;
                }
            }
        }));
    }

    // ---------------------------------------------------------------- sidebar

    fn rebuild_side(&mut self) {
        let mut rows = Vec::new();
        let r = &self.refs;
        let sections = [
            (Section::Branches, r.branches.len()),
            (
                Section::Remotes,
                r.remotes.iter().filter(|b| !b.is_symbolic_head).count(),
            ),
            (Section::Tags, r.tags.len()),
            (Section::Stashes, r.stashes.len()),
        ];
        for (section, count) in sections {
            rows.push(SideRow::Header(section, count));
            if self.collapsed.contains(&section) {
                continue;
            }
            match section {
                Section::Branches => {
                    for b in &r.branches {
                        let detail = b.tracking.as_ref().and_then(|t| {
                            if t.gone {
                                Some("gone".into())
                            } else if t.ahead + t.behind > 0 {
                                Some(ahead_behind(t.ahead, t.behind).into())
                            } else {
                                None
                            }
                        });
                        rows.push(SideRow::Ref {
                            text: b.name.clone().into(),
                            id: b.id,
                            detail,
                            current: b.is_head,
                            rev: format!("refs/heads/{}", b.name),
                            indent: false,
                        });
                    }
                }
                Section::Remotes => {
                    let mut last: Option<&str> = None;
                    for b in r.remotes.iter().filter(|b| !b.is_symbolic_head) {
                        if last != Some(b.remote.as_str()) {
                            rows.push(SideRow::Group(b.remote.clone().into()));
                            last = Some(b.remote.as_str());
                        }
                        rows.push(SideRow::Ref {
                            text: b.name.clone().into(),
                            id: b.id,
                            detail: None,
                            current: false,
                            rev: format!("refs/remotes/{}", b.short()),
                            indent: true,
                        });
                    }
                }
                Section::Tags => {
                    for t in &r.tags {
                        rows.push(SideRow::Ref {
                            text: t.name.clone().into(),
                            id: t.id,
                            detail: None,
                            current: false,
                            rev: format!("refs/tags/{}", t.name),
                            indent: false,
                        });
                    }
                }
                Section::Stashes => {
                    for s in &r.stashes {
                        rows.push(SideRow::Ref {
                            text: s.message.clone().into(),
                            id: s.id,
                            detail: Some(format!("@{{{}}}", s.index).into()),
                            current: false,
                            rev: s.id.to_string(),
                            indent: false,
                        });
                    }
                }
            }
        }
        self.side_rows = rows;
    }

    fn toggle_section(&mut self, s: Section, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&s) {
            self.collapsed.insert(s);
        }
        self.rebuild_side();
        cx.notify();
    }

    fn render_sidebar(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
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
                    .when(self.state != RepoState::Clean, |d| {
                        d.child(ui::chip(self.state.label(), p.modified))
                    }),
            )
            .child(list)
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
        match row {
            SideRow::Header(section, n) => {
                let open = !self.collapsed.contains(&section);
                base.mt(rpx(if i == 0 { 0. } else { 6. }))
                    .cursor_pointer()
                    .text_color(p.fg2)
                    .hover(|s| s.text_color(p.fg))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx)))
                    .child(
                        div()
                            .w(rpx(10.))
                            .text_size(ts::CAPTION)
                            .text_color(p.fg3)
                            .child(if open { "▾" } else { "▸" }),
                    )
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
            SideRow::Group(name) => base
                .pl(rpx(28.))
                .text_color(p.fg3)
                .text_size(ts::SMALL)
                .child(name)
                .into_any_element(),
            SideRow::Ref {
                text,
                id,
                detail,
                current,
                rev,
                indent,
            } => {
                let filtered = self.scope == Scope::Revs(vec![rev.clone()]);
                let selected = self.selected.is_some_and(|s| self.log.id(s) == id);
                base.pl(rpx(if indent { 40. } else { 28. }))
                    .cursor_pointer()
                    .rounded(px(4.))
                    .text_color(if current { p.fg } else { p.fg2 })
                    .when(current, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .when(selected, |d| d.bg(p.sel))
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                        if ev.click_count() >= 2 {
                            // Double click: show only this ref's history (again: all).
                            let scope = if this.scope == Scope::Revs(vec![rev.clone()]) {
                                Scope::All
                            } else {
                                Scope::Revs(vec![rev.clone()])
                            };
                            this.pending_select = Some(id);
                            this.set_scope(scope, cx);
                        } else {
                            match this.log.row_of(&id) {
                                Some(row) => this.select(row),
                                None => this.pending_select = Some(id),
                            }
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
                                .font_family(MONO)
                                .text_size(ts::CAPTION)
                                .text_color(p.fg3)
                                .child(t),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    // ---------------------------------------------------------------- graph

    fn render_graph_header(&self, p: &Palette, graph_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let col = |label: &'static str| {
            div()
                .flex_none()
                .text_size(ts::CAPTION)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.fg3)
                .child(label)
        };
        let scope_label: SharedString = match &self.scope {
            Scope::All => "All branches".into(),
            Scope::Head => "Current branch".into(),
            Scope::Revs(r) => r
                .iter()
                .map(|r| short_ref(r))
                .collect::<Vec<_>>()
                .join(", ")
                .into(),
        };
        let current_only = self.scope == Scope::Head;
        div()
            .h(rpx(30.))
            .flex_none()
            .flex()
            .items_center()
            .gap(rpx(10.))
            .pr(rpx(12.))
            .bg(p.panel)
            .border_b_1()
            .border_color(p.bd)
            .child(
                div()
                    .flex_none()
                    .w(px(graph_w))
                    .min_w(rpx(56.))
                    .pl(rpx(10.))
                    .child(col("GRAPH")),
            )
            .child(div().flex_1().min_w_0().child(col("DESCRIPTION")))
            .child(
                ui::segmented(
                    "scope",
                    vec![
                        (
                            if matches!(self.scope, Scope::Revs(_)) {
                                scope_label.clone()
                            } else {
                                "All".into()
                            },
                            !current_only,
                            Box::new(cx.listener(|this, _, _, cx| this.set_scope(Scope::All, cx))),
                        ),
                        (
                            "Current".into(),
                            current_only,
                            Box::new(cx.listener(|this, _, _, cx| this.set_scope(Scope::Head, cx))),
                        ),
                    ],
                    18.,
                    p,
                )
                .tooltip(move |w, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(format!(
                        "Showing: {scope_label}. Double-click a branch to show only it."
                    ))
                    .build(w, cx)
                }),
            )
            .child(
                div()
                    .id("jump-head")
                    .flex_none()
                    .px(rpx(6.))
                    .h(rpx(20.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .text_size(ts::SMALL)
                    .text_color(p.fg2)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover).text_color(p.fg))
                    .tooltip(|w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Jump to HEAD")
                            .action(&actions::JumpToHead, None)
                            .build(w, cx)
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.jump_to_head(cx)))
                    .child("HEAD"),
            )
            .child(div().w(rpx(130.)).flex_none().child(col("AUTHOR")))
            .child(div().w(rpx(118.)).flex_none().child(col("DATE")))
            .child(div().w(rpx(60.)).flex_none().child(col("COMMIT")))
            .into_any_element()
    }

    fn render_graph_row(
        &self,
        i: usize,
        p: &Palette,
        graph_w: f32,
        row_h: f32,
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = self.log.id(i);
        let row = self.graph.row(i);
        let node: Node = row.node;
        let segments: Vec<Segment> = row.segments.to_vec();
        let head = self.refs.head == Some(id);
        let merge = self.log.parents(i).len() > 1;
        let colors = graph_paint::colors(p);
        let selected = self.selected == Some(i);
        let graph = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let b = gpui_kit::Bounds::new(
                    bounds.origin,
                    gpui_kit::size(bounds.size.width, px(row_h)),
                );
                window.with_content_mask(Some(gpui_kit::ContentMask { bounds }), |window| {
                    graph_paint::paint_row(b, node, &segments, (head, merge), &colors, window)
                });
            },
        )
        .flex_none()
        .w(px(graph_w))
        .h_full();
        let labels = self.decorations.get(&id).map(|ls| {
            div()
                .flex_none()
                .flex()
                .gap(rpx(4.))
                .children(ls.iter().take(4).map(|l| label_chip(l, p)))
                .when(ls.len() > 4, |d| {
                    d.child(ui::chip(format!("+{}", ls.len() - 4), p.fg3))
                })
        });
        let author = self.log.author(i);
        div()
            .id(("commit", i))
            .w_full()
            .h(px(row_h))
            .flex()
            .items_center()
            .gap(rpx(10.))
            .pr(rpx(12.))
            .text_size(ts::BODY)
            .when(selected, |d| d.bg(p.sel))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.selected = Some(i);
                window.focus(&this.focus, cx);
                cx.notify();
            }))
            .child(graph)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(rpx(6.))
                    .children(labels)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(if merge { p.fg2 } else { p.fg })
                            .child(SharedString::from(self.log.summary(i).to_owned())),
                    ),
            )
            .child(
                div()
                    .w(rpx(130.))
                    .flex_none()
                    .truncate()
                    .text_color(p.fg2)
                    .child(SharedString::from(author.name.clone())),
            )
            .child(
                div()
                    .w(rpx(118.))
                    .flex_none()
                    .truncate()
                    .text_color(p.fg2)
                    .child(fmt_date(self.log.author_time(i), now)),
            )
            .child(
                div()
                    .w(rpx(60.))
                    .flex_none()
                    .font_family(MONO)
                    .text_size(ts::SMALL)
                    .text_color(p.fg3)
                    .child(switchhead_git::short(&id)),
            )
            .into_any_element()
    }

    fn render_graph(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let zoom = crate::appearance::zoom(cx);
        let row_h = ROW_H * zoom;
        let lanes = self.graph.max_width().min(MAX_LANES);
        let graph_w = graph_paint::column_width(lanes, row_h);
        let count = self.log.len();
        let body: AnyElement = if count == 0 {
            let msg: SharedString = if let Some(e) = &self.error {
                e.clone()
            } else if self.loading {
                "Reading history…".into()
            } else {
                "No commits yet.".into()
            };
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(if self.error.is_some() {
                    p.deleted
                } else {
                    p.fg3
                })
                .child(msg)
                .into_any_element()
        } else {
            uniform_list(
                "graph",
                count,
                cx.processor(move |this, range: Range<usize>, _w, cx| {
                    let p = palette(cx);
                    let now = crate::store::now();
                    range
                        .map(|i| this.render_graph_row(i, &p, graph_w, row_h, now, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(self.render_graph_header(p, graph_w, cx))
            .when(self.loading && count > 0, |d| {
                d.child(ui::loading_bar("graph-loading", p))
            })
            .child(body)
            .into_any_element()
    }
}

/// The walk thread: refs, then batches, then upstream counts.
fn walk_thread(repo: Repo, opts: LogOptions, cancel: Cancel, tx: UnboundedSender<Msg>) {
    let mut refs = match refs::load(&repo) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Msg::Done(Err(e.to_string())));
            return;
        }
    };
    if tx.send(Msg::Refs(Box::new(refs.clone()))).is_err() {
        return;
    }
    let res = log::walk(&repo, &opts, &cancel, |b| {
        tx.send(Msg::Batch(Box::new(b))).is_ok()
    });
    let failed = res.is_err();
    let _ = tx.send(Msg::Done(res.map_err(|e| e.to_string())));
    if failed || cancel.is_cancelled() {
        return;
    }
    match refs::load_tracking(&repo, &mut refs) {
        Ok(()) => {
            let _ = tx.send(Msg::Tracking(Box::new(refs)));
        }
        Err(e) => tracing::debug!(error = %e, "upstream counts unavailable"),
    }
}

impl Focusable for RepoView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RepoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .id("repo")
            .key_context("Graph")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .on_action(
                cx.listener(|this, _: &actions::SelectUp, _, cx| this.move_selection(-1, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::SelectDown, _, cx| this.move_selection(1, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::PageUp, _, cx| this.move_selection(-PAGE, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::PageDown, _, cx| this.move_selection(PAGE, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::SelectFirst, _, cx| {
                this.move_selection(isize::MIN / 2, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::SelectLast, _, cx| {
                this.move_selection(isize::MAX / 2, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::JumpToHead, _, cx| this.jump_to_head(cx)))
            .when(self.sidebar, |d| d.child(self.render_sidebar(&p, cx)))
            .child(self.render_graph(&p, cx))
    }
}

fn label_chip(l: &Label, p: &Palette) -> AnyElement {
    let (fg, bg): (Hsla, Option<Hsla>) = match l.kind {
        LabelKind::Head => (p.deleted, None),
        LabelKind::CurrentBranch => (p.acc_fg, Some(p.acc)),
        LabelKind::Branch => (p.acc, None),
        LabelKind::Remote => (p.fg3, None),
        LabelKind::Tag => (p.modified, None),
        LabelKind::Stash => (p.sx_kw, None),
    };
    let chip = div()
        .flex_none()
        .max_w(rpx(180.))
        .truncate()
        .px(rpx(5.))
        .h(rpx(16.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(bg.unwrap_or(fg))
        .text_color(fg)
        .font_family(MONO)
        .font_weight(FontWeight::MEDIUM)
        .text_size(ts::CAPTION)
        .child(SharedString::from(l.text.clone()));
    match bg {
        Some(bg) => chip.bg(bg).into_any_element(),
        None => chip.into_any_element(),
    }
}

/// `↑2 ↓1`.
fn ahead_behind(ahead: u32, behind: u32) -> String {
    match (ahead, behind) {
        (0, b) => format!("↓{b}"),
        (a, 0) => format!("↑{a}"),
        (a, b) => format!("↑{a} ↓{b}"),
    }
}

/// `refs/heads/main` → `main`.
fn short_ref(r: &str) -> &str {
    ["refs/heads/", "refs/remotes/", "refs/tags/"]
        .iter()
        .find_map(|p| r.strip_prefix(p))
        .unwrap_or(r)
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("failed")
        .to_owned()
}

/// A commit date for the list: "14:02" today, "Oct 9 14:02" this year, "Oct 9 2023" before.
fn fmt_date(secs: i64, now: i64) -> SharedString {
    use chrono::{Datelike as _, Local, TimeZone as _};
    let (Some(t), Some(n)) = (
        Local.timestamp_opt(secs, 0).single(),
        Local.timestamp_opt(now, 0).single(),
    ) else {
        return SharedString::default();
    };
    let s = if t.date_naive() == n.date_naive() {
        t.format("Today %H:%M").to_string()
    } else if t.year() == n.year() {
        t.format("%b %-d %H:%M").to_string()
    } else {
        t.format("%b %-d %Y").to_string()
    };
    s.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_dates_relative_to_now() {
        use chrono::{Local, TimeZone as _};
        let now = Local
            .with_ymd_and_hms(2026, 10, 14, 15, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(fmt_date(now - 3_600, now).as_ref(), "Today 14:00");
        let earlier = Local
            .with_ymd_and_hms(2026, 3, 9, 8, 5, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(fmt_date(earlier, now).as_ref(), "Mar 9 08:05");
        let old = Local
            .with_ymd_and_hms(2023, 11, 2, 8, 5, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(fmt_date(old, now).as_ref(), "Nov 2 2023");
    }

    #[test]
    fn small_formatters() {
        assert_eq!(ahead_behind(2, 0), "↑2");
        assert_eq!(ahead_behind(0, 3), "↓3");
        assert_eq!(ahead_behind(1, 1), "↑1 ↓1");
        assert_eq!(short_ref("refs/remotes/origin/main"), "origin/main");
        assert_eq!(short_ref("abc"), "abc");
        assert_eq!(first_line("\n  fatal: no remote\nmore"), "fatal: no remote");
    }
}
