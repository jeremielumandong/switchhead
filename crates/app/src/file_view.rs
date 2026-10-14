//! A file's history and blame, shown in place of the graph.
//!
//! History lists the commits that changed the file, following renames
//! ([`switchhead_git::blame::file_history`]); the selected one opens in a commit details
//! pane with the file selected. Blame streams `git blame --incremental` on a thread, so
//! lines fill in with their commits while git still searches older history. Clicking a
//! line selects its commit; Show Commit selects it in the graph, and Blame Before This
//! Commit re-blames the file as it was in that commit's parent.

use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::AppContext as _;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ListHorizontalSizingBehavior, ParentElement as _, Render,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use switchhead_git::blame::{self, Blame, BlameCommit, Chunk, FileVersion};
use switchhead_git::cli::Cancel;
use switchhead_git::diff::{self, FileStatus, Source};
use switchhead_git::{Oid, Repo};
use tokio::sync::mpsc::unbounded_channel;

use crate::actions;
use crate::appearance::{rpx, ts};
use crate::commit_view::{CommitView, Navigate, OpenFile};
use crate::diff_view::status_badge;
use crate::theme::{MONO, Palette, SANS, palette};
use crate::ui;

/// What the view asks of the repository view.
pub enum FileViewEvent {
    /// Select this commit in the graph.
    Reveal(Oid),
    /// Close the view.
    Close,
}

/// History or Blame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    /// The commits that changed the file.
    History,
    /// Each line's commit.
    Blame,
}

/// A file's text as one string and where each line starts.
#[derive(Debug, Default, PartialEq, Eq)]
struct Lines {
    text: String,
    starts: Vec<usize>,
    /// The longest line (sizes the horizontal scroll).
    widest: usize,
}

impl Lines {
    fn new(text: String) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        // A final newline ends the last line rather than starting an empty one.
        if starts.len() > 1 && starts.last() == Some(&text.len()) {
            starts.pop();
        }
        if text.is_empty() {
            starts.clear();
        }
        let mut lines = Self {
            text,
            starts,
            widest: 0,
        };
        lines.widest = (0..lines.len())
            .max_by_key(|i| lines.line(*i).len())
            .unwrap_or(0);
        lines
    }

    fn len(&self) -> usize {
        self.starts.len()
    }

    /// Line `i` without its line ending.
    fn line(&self, i: usize) -> &str {
        let Some(&start) = self.starts.get(i) else {
            return "";
        };
        let end = self.starts.get(i + 1).copied().unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }
}

/// What the blame thread sends.
enum Msg {
    Text(Lines),
    /// A chunk, with its commit the first time that commit appears.
    Chunk(Chunk, Option<BlameCommit>),
    Failed(String),
}

/// History and blame of one file.
pub struct FileView {
    repo: Repo,
    /// The file, as named at `rev` (or in the work tree).
    path: PathBuf,
    /// Where history starts (`None`: HEAD).
    rev: Option<Oid>,
    tab: Tab,
    versions: Rc<Vec<FileVersion>>,
    history_loading: bool,
    history_error: Option<SharedString>,
    selected_version: Option<usize>,
    commit: Entity<CommitView>,
    split: Entity<ResizableState>,
    /// The file and revision blame shows (`None`: the work tree).
    blame_at: (PathBuf, Option<Oid>),
    blame_started: bool,
    blame: Rc<Blame>,
    lines: Option<Rc<Lines>>,
    blame_running: bool,
    blame_error: Option<SharedString>,
    selected_line: Option<usize>,
    /// Oldest and newest author times among the blamed commits, for the age bar.
    age_range: (i64, i64),
    cancel: Cancel,
    focus: FocusHandle,
    /// Take focus at the next render.
    focus_pending: bool,
    history_scroll: UniformListScrollHandle,
    blame_scroll: UniformListScrollHandle,
    now: i64,
    _history: Task<()>,
    _blame: Task<()>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<FileViewEvent> for FileView {}

impl Focusable for FileView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for FileView {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl FileView {
    /// The `tab` of `path` at `rev` (`None`: the work tree for blame, HEAD for history).
    pub fn new(
        repo: Repo,
        path: PathBuf,
        rev: Option<Oid>,
        tab: Tab,
        cx: &mut Context<Self>,
    ) -> Self {
        let commit = cx.new(|cx| CommitView::new(repo.clone(), cx));
        let subs = vec![
            cx.subscribe(&commit, |_, _, Navigate(id), cx| {
                cx.emit(FileViewEvent::Reveal(*id));
            }),
            cx.subscribe(&commit, |this, _, ev: &OpenFile, cx| {
                let tab = if ev.blame { Tab::Blame } else { Tab::History };
                this.retarget(ev.path.clone(), Some(ev.rev), tab, cx);
            }),
        ];
        let mut v = Self {
            repo,
            blame_at: (path.clone(), rev),
            path,
            rev,
            tab,
            versions: Rc::default(),
            history_loading: false,
            history_error: None,
            selected_version: None,
            commit,
            split: cx.new(|_| ResizableState::default()),
            blame_started: false,
            blame: Rc::default(),
            lines: None,
            blame_running: false,
            blame_error: None,
            selected_line: None,
            age_range: (0, 0),
            cancel: Cancel::default(),
            focus: cx.focus_handle(),
            focus_pending: true,
            history_scroll: UniformListScrollHandle::new(),
            blame_scroll: UniformListScrollHandle::new(),
            now: chrono::Local::now().timestamp(),
            _history: Task::ready(()),
            _blame: Task::ready(()),
            _subs: subs,
        };
        v.load_history(cx);
        if tab == Tab::Blame {
            v.load_blame(cx);
        }
        v
    }

    /// Show another file or revision.
    fn retarget(&mut self, path: PathBuf, rev: Option<Oid>, tab: Tab, cx: &mut Context<Self>) {
        self.path = path.clone();
        self.rev = rev;
        self.blame_at = (path, rev);
        self.blame_started = false;
        self.tab = tab;
        self.load_history(cx);
        if tab == Tab::Blame {
            self.load_blame(cx);
        }
        self.focus_pending = true;
        cx.notify();
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == Tab::Blame && !self.blame_started {
            self.load_blame(cx);
        }
        cx.notify();
    }

    fn load_history(&mut self, cx: &mut Context<Self>) {
        self.history_loading = true;
        self.history_error = None;
        self.versions = Rc::default();
        self.selected_version = None;
        let (repo, path, rev) = (self.repo.clone(), self.path.clone(), self.rev);
        let task = cx.background_executor().spawn(async move {
            let rev = rev.map(|r| r.to_string());
            blame::file_history(&repo, &path, rev.as_deref())
        });
        self._history = cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                v.history_loading = false;
                match res {
                    Ok(versions) => {
                        v.versions = Rc::new(versions);
                        if !v.versions.is_empty() {
                            v.select_version(0, cx);
                        }
                    }
                    Err(e) => v.history_error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        });
    }

    fn select_version(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(version) = self.versions.get(i) else {
            return;
        };
        let (id, path) = (version.id, version.path.clone());
        self.selected_version = Some(i);
        self.history_scroll
            .scroll_to_item(i, ScrollStrategy::Nearest);
        self.commit.update(cx, |c, cx| c.show_file(id, path, cx));
        cx.notify();
    }

    fn load_blame(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        let cancel = Cancel::default();
        self.cancel = cancel.clone();
        self.blame_started = true;
        self.blame = Rc::default();
        self.lines = None;
        self.blame_error = None;
        self.selected_line = None;
        self.age_range = (i64::MAX, i64::MIN);
        self.blame_running = true;
        self.blame_scroll.scroll_to_item(0, ScrollStrategy::Top);
        let (path, rev) = self.blame_at.clone();
        let repo = self.repo.clone();
        let (tx, mut rx) = unbounded_channel::<Msg>();
        let thread_cancel = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("git-blame".into())
            .spawn(move || {
                let bytes = match rev {
                    Some(id) => diff::read_at(&repo, &path, Source::Commit(id)),
                    None => std::fs::read(repo.root().join(&path))
                        .map(Some)
                        .map_err(|e| switchhead_git::Error::Unsupported(e.to_string())),
                };
                let text = match bytes {
                    Ok(Some(b)) if diff::is_binary(&b) => {
                        let _ = tx.send(Msg::Failed("Binary files have no blame.".into()));
                        return;
                    }
                    Ok(Some(b)) => String::from_utf8_lossy(&b).into_owned(),
                    Ok(None) => {
                        let _ = tx.send(Msg::Failed("The file is not in this commit.".into()));
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Failed(e.to_string()));
                        return;
                    }
                };
                let _ = tx.send(Msg::Text(Lines::new(text)));
                let rev = rev.map(|r| r.to_string());
                let mut known = 0;
                let res = blame::blame(&repo, &path, rev.as_deref(), &thread_cancel, |c, b| {
                    let first = (c.commit >= known).then(|| {
                        known = c.commit + 1;
                        b.clone()
                    });
                    let _ = tx.send(Msg::Chunk(*c, first));
                });
                if let Err(e) = res
                    && !thread_cancel.is_cancelled()
                {
                    let _ = tx.send(Msg::Failed(e.to_string()));
                }
            });
        if let Err(e) = spawned {
            self.blame_running = false;
            self.blame_error = Some(e.to_string().into());
            return;
        }
        self._blame = cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.recv().await {
                // Take whatever else is waiting, so a burst of chunks is one repaint.
                let mut batch = vec![msg];
                while let Ok(m) = rx.try_recv() {
                    batch.push(m);
                }
                if cancel.is_cancelled() {
                    return;
                }
                let ok = this.update(cx, |v, cx| {
                    for m in batch {
                        v.apply(m);
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |v, cx| {
                v.blame_running = false;
                cx.notify();
            });
        });
        cx.notify();
    }

    fn apply(&mut self, m: Msg) {
        match m {
            Msg::Text(lines) => self.lines = Some(Rc::new(lines)),
            Msg::Chunk(c, first) => {
                let blame = Rc::make_mut(&mut self.blame);
                if let Some(b) = first {
                    if blame.commits.len() != c.commit {
                        return;
                    }
                    if !b.id.is_null() {
                        self.age_range.0 = self.age_range.0.min(b.author_time);
                        self.age_range.1 = self.age_range.1.max(b.author_time);
                    }
                    blame.commits.push(b);
                }
                if c.commit < blame.commits.len() {
                    blame.apply(&c);
                }
            }
            Msg::Failed(e) => self.blame_error = Some(e.into()),
        }
    }

    /// The commit the toolbar acts on: the selected line's or version's.
    fn current_commit(&self) -> Option<Oid> {
        match self.tab {
            Tab::History => self
                .selected_version
                .and_then(|i| self.versions.get(i))
                .map(|v| v.id),
            Tab::Blame => self
                .selected_line
                .and_then(|n| self.blame.commit_of(n))
                .map(|c| c.id)
                .filter(|id| !id.is_null()),
        }
    }

    /// Blame the file as it was before the selected line's commit.
    fn blame_previous(&mut self, cx: &mut Context<Self>) {
        let Some(prev) = self
            .selected_line
            .and_then(|n| self.blame.commit_of(n))
            .and_then(|c| c.previous.clone())
        else {
            return;
        };
        let line = self.selected_line;
        self.blame_at = (prev.1, Some(prev.0));
        self.load_blame(cx);
        // Stay near the same place in the file.
        if let Some(n) = line {
            self.blame_scroll.scroll_to_item(n, ScrollStrategy::Center);
        }
    }

    /// Blame the selected version of the file.
    fn blame_version(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.selected_version.and_then(|i| self.versions.get(i)) else {
            return;
        };
        if v.status == 'D' {
            return;
        }
        self.blame_at = (v.path.clone(), Some(v.id));
        self.tab = Tab::Blame;
        self.load_blame(cx);
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = match self.tab {
            Tab::History => self.versions.len(),
            Tab::Blame => self.lines.as_ref().map_or(0, |l| l.len()),
        };
        if len == 0 {
            return;
        }
        let cur = match self.tab {
            Tab::History => self.selected_version,
            Tab::Blame => self.selected_line,
        };
        let next = cur.map_or(0, |c| {
            (c as isize)
                .saturating_add(delta)
                .clamp(0, len as isize - 1) as usize
        });
        match self.tab {
            Tab::History => self.select_version(next, cx),
            Tab::Blame => {
                self.selected_line = Some(next);
                self.blame_scroll
                    .scroll_to_item(next, ScrollStrategy::Nearest);
                cx.notify();
            }
        }
    }

    // ---------------------------------------------------------------- render

    fn render_header(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        let tab = |t: Tab| -> ui::OnClick {
            let view = view.clone();
            Box::new(move |_, _, cx: &mut App| {
                let _ = view.update(cx, |v, cx| v.set_tab(t, cx));
            })
        };
        let (path, rev) = match self.tab {
            Tab::History => (&self.path, self.rev),
            Tab::Blame => (&self.blame_at.0, self.blame_at.1),
        };
        let at = match (self.tab, rev) {
            (_, Some(id)) => format!("at {}", switchhead_git::short(&id)),
            (Tab::Blame, None) => "in the work tree".into(),
            (Tab::History, None) => "up to HEAD".into(),
        };
        let current = self.current_commit();
        let previous = self.tab == Tab::Blame
            && self
                .selected_line
                .and_then(|n| self.blame.commit_of(n))
                .is_some_and(|c| c.previous.is_some());
        let version_blamable = self.tab == Tab::History
            && self
                .selected_version
                .and_then(|i| self.versions.get(i))
                .is_some_and(|v| v.status != 'D');
        div()
            .flex_none()
            .h(rpx(40.))
            .px(rpx(12.))
            .flex()
            .items_center()
            .gap(rpx(8.))
            .border_b_1()
            .border_color(p.bd)
            .child(ui::segmented(
                "file-view-tab",
                vec![
                    (
                        "History".into(),
                        self.tab == Tab::History,
                        tab(Tab::History),
                    ),
                    ("Blame".into(), self.tab == Tab::Blame, tab(Tab::Blame)),
                ],
                22.,
                p,
            ))
            .child(
                div()
                    .flex_none()
                    .max_w(rpx(420.))
                    .truncate()
                    .font_family(MONO)
                    .text_size(ts::BODY)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.fg)
                    .child(SharedString::from(path.to_string_lossy().into_owned())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ts::SMALL)
                    .text_color(p.fg3)
                    .child(at),
            )
            .when(previous, |d| {
                d.child(
                    ui::button(
                        "blame-previous",
                        "Blame Before This Commit",
                        ui::Kind::Secondary,
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.blame_previous(cx))),
                )
            })
            .when(version_blamable, |d| {
                d.child(
                    ui::button(
                        "blame-version",
                        "Blame This Version",
                        ui::Kind::Secondary,
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.blame_version(cx))),
                )
            })
            .child(
                ui::button("file-view-show", "Show Commit", ui::Kind::Secondary, p)
                    .when(current.is_none(), |d| d.opacity(0.5))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(id) = current {
                            cx.emit(FileViewEvent::Reveal(id));
                        }
                    })),
            )
            .child(
                ui::button("file-view-close", "Close", ui::Kind::Secondary, p)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FileViewEvent::Close))),
            )
            .into_any_element()
    }

    fn render_history(
        &self,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(e) = &self.history_error {
            return message(e.clone(), p.deleted);
        }
        if self.versions.is_empty() {
            return message(
                if self.history_loading {
                    ""
                } else {
                    "No commit changed this file."
                },
                p.fg3,
            );
        }
        let row_h = (40. * crate::appearance::zoom(cx)).round();
        let versions = self.versions.clone();
        let selected = self.selected_version;
        let now = self.now;
        let list = uniform_list(
            "file-history",
            versions.len(),
            cx.processor(move |_, range: Range<usize>, _, cx| {
                let p = palette(cx);
                range
                    .filter_map(|i| versions.get(i).map(|v| (i, v)))
                    .map(|(i, v)| render_version(i, v, selected == Some(i), now, row_h, &p, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.history_scroll)
        .size_full();
        let rem = window.rem_size();
        h_resizable("file-history-split")
            .with_state(&self.split)
            .child(
                resizable_panel()
                    .size(rem * (340. / 13.))
                    .size_range(rem * (220. / 13.)..rem * (900. / 13.))
                    .child(div().size_full().bg(p.surface).child(list)),
            )
            .child(resizable_panel().child(self.commit.clone()))
            .into_any_element()
    }

    fn render_blame(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(lines) = self.lines.clone() else {
            return match &self.blame_error {
                Some(e) => message(e.clone(), p.deleted),
                None => message("", p.fg3),
            };
        };
        if lines.len() == 0 {
            return message("The file is empty.", p.fg3);
        }
        let row_h = crate::diff_view::row_height(cx);
        let digits = lines.len().to_string().len() as f32;
        let ctx = Rc::new(BlameRows {
            lines: lines.clone(),
            blame: self.blame.clone(),
            selected: self.selected_line,
            selected_commit: self
                .selected_line
                .and_then(|n| self.blame.commit_of(n))
                .map(|c| c.id),
            age_range: self.age_range,
            now: self.now,
            digits,
        });
        let widest = lines.widest;
        let list = uniform_list(
            "blame",
            lines.len(),
            cx.processor(move |_, range: Range<usize>, _, cx| {
                let p = palette(cx);
                range
                    .map(|n| render_blame_row(&ctx, n, row_h, &p, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.blame_scroll)
        .flex_1()
        .min_h_0()
        .font_family(crate::appearance::editor_font_family(cx))
        .text_size(crate::appearance::editor_font_size(cx))
        .with_width_from_item(Some(widest))
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when_some(self.blame_error.clone(), |d, e| {
                d.child(
                    div()
                        .flex_none()
                        .px(rpx(12.))
                        .py(rpx(4.))
                        .text_size(ts::SMALL)
                        .text_color(p.deleted)
                        .child(e),
                )
            })
            .child(list)
            .child(self.render_blame_footer(p))
            .into_any_element()
    }

    /// The selected line's commit, in full.
    fn render_blame_footer(&self, p: &Palette) -> AnyElement {
        let text: SharedString = match self.selected_line.and_then(|n| self.blame.commit_of(n)) {
            Some(c) if c.id.is_null() => "Not committed yet".into(),
            Some(c) => format!(
                "{}  {} · {} · {}",
                switchhead_git::short(&c.id),
                c.summary,
                c.author,
                crate::repo_view::fmt_date(c.author_time, self.now)
            )
            .into(),
            None if self.blame_running => "Blaming…".into(),
            None => "Click a line to see its commit. Double-click shows it in the graph.".into(),
        };
        div()
            .flex_none()
            .h(rpx(28.))
            .px(rpx(12.))
            .flex()
            .items_center()
            .border_t_1()
            .border_color(p.bd)
            .bg(p.panel)
            .text_size(ts::SMALL)
            .text_color(p.fg2)
            .child(div().flex_1().min_w_0().truncate().child(text))
            .into_any_element()
    }
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_pending) {
            window.focus(&self.focus, cx);
        }
        let p = palette(cx);
        let body = match self.tab {
            Tab::History => self.render_history(&p, window, cx),
            Tab::Blame => self.render_blame(&p, cx),
        };
        let running = match self.tab {
            Tab::History => self.history_loading,
            Tab::Blame => self.blame_running,
        };
        div()
            .id("file-view")
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
            .on_action(cx.listener(|_, _: &actions::Dismiss, _, cx| cx.emit(FileViewEvent::Close)))
            .child(self.render_header(&p, cx))
            .when(running, |d| {
                d.child(ui::loading_bar("file-view-loading", &p))
            })
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
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

fn version_status(c: char) -> FileStatus {
    match c {
        'A' => FileStatus::Added,
        'D' => FileStatus::Deleted,
        'R' => FileStatus::Renamed,
        'C' => FileStatus::Copied,
        'T' => FileStatus::TypeChanged,
        _ => FileStatus::Modified,
    }
}

fn render_version(
    i: usize,
    v: &FileVersion,
    selected: bool,
    now: i64,
    row_h: f32,
    p: &Palette,
    cx: &mut Context<FileView>,
) -> AnyElement {
    let renamed = v
        .old_path
        .as_ref()
        .map(|o| format!("from {}", o.to_string_lossy()));
    let id = v.id;
    div()
        .id(("version", i))
        .w_full()
        .h(px(row_h))
        .flex()
        .items_center()
        .gap(rpx(8.))
        .px(rpx(10.))
        .cursor_pointer()
        .when(selected, |d| d.bg(p.sel))
        .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
            window.focus(&this.focus, cx);
            if ev.click_count() > 1 {
                cx.emit(FileViewEvent::Reveal(id));
            } else {
                this.select_version(i, cx);
            }
        }))
        .child(status_badge(version_status(v.status), p))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_size(ts::BODY)
                        .text_color(p.fg)
                        .child(SharedString::from(v.summary.clone())),
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
                                .child(switchhead_git::short(&v.id)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .child(crate::repo_view::fmt_date(v.author_time, now)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(v.author.clone())),
                        )
                        .when_some(renamed, |d, r| {
                            d.child(div().min_w_0().truncate().text_color(p.acc).child(r))
                        }),
                ),
        )
        .into_any_element()
}

/// What blame rows render from.
struct BlameRows {
    lines: Rc<Lines>,
    blame: Rc<Blame>,
    selected: Option<usize>,
    selected_commit: Option<Oid>,
    age_range: (i64, i64),
    now: i64,
    digits: f32,
}

/// How recent `t` is between `oldest` and `newest`: 0 (oldest) to 1 (newest).
fn age(t: i64, (oldest, newest): (i64, i64)) -> f32 {
    if newest <= oldest {
        return 1.;
    }
    ((t - oldest) as f32 / (newest - oldest) as f32).clamp(0., 1.)
}

fn render_blame_row(
    rows: &BlameRows,
    n: usize,
    row_h: f32,
    p: &Palette,
    cx: &mut Context<FileView>,
) -> AnyElement {
    let commit = rows.blame.commit_of(n);
    let first = n == 0 || commit.map(|c| c.id) != rows.blame.commit_of(n - 1).map(|c| c.id);
    let selected = rows.selected == Some(n);
    let same = commit.is_some_and(|c| Some(c.id) == rows.selected_commit);
    let bar = match commit {
        Some(c) if c.id.is_null() => p.modified,
        Some(c) => p
            .acc
            .opacity(0.15 + 0.85 * age(c.author_time, rows.age_range)),
        None => p.bd,
    };
    let gutter: AnyElement = match commit.filter(|_| first) {
        Some(c) if c.id.is_null() => div()
            .text_color(p.modified)
            .child("Not committed yet")
            .into_any_element(),
        Some(c) => div()
            .flex()
            .gap(rpx(6.))
            .min_w_0()
            .child(
                div()
                    .flex_none()
                    .text_color(p.fg3)
                    .child(switchhead_git::short(&c.id)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(p.fg3)
                    .child(crate::repo_view::fmt_date(c.author_time, rows.now)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(p.fg2)
                    .child(SharedString::from(c.author.clone())),
            )
            .into_any_element(),
        None => div().into_any_element(),
    };
    let id = commit.map(|c| c.id).filter(|id| !id.is_null());
    let text = rows.lines.line(n).replace('\t', "    ");
    div()
        .id(("blame-line", n))
        .w_full()
        .h(px(row_h))
        .flex()
        .items_center()
        .whitespace_nowrap()
        .cursor_pointer()
        .when(selected, |d| d.bg(p.sel))
        .when(same && !selected, |d| d.bg(p.sel.opacity(0.45)))
        .when(!same && !selected, |d| d.hover(|s| s.bg(p.hover)))
        .when(first && n > 0, |d| d.border_t_1().border_color(p.line))
        .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
            window.focus(&this.focus, cx);
            this.selected_line = Some(n);
            if ev.click_count() > 1
                && let Some(id) = id
            {
                cx.emit(FileViewEvent::Reveal(id));
            }
            cx.notify();
        }))
        .child(div().flex_none().w(px(3.)).h_full().bg(bar))
        .child(
            div()
                .flex_none()
                .w(rpx(250.))
                .px(rpx(8.))
                .overflow_hidden()
                .font_family(SANS)
                .text_size(ts::SMALL)
                .child(gutter),
        )
        .child(
            div()
                .flex_none()
                .w(rpx(8. + 8. * rows.digits))
                .pr(rpx(8.))
                .flex()
                .justify_end()
                .text_color(p.fg3)
                .child((n + 1).to_string()),
        )
        .child(div().flex_none().pr(rpx(16.)).text_color(p.fg).child(text))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines() {
        let l = Lines::new("one\r\ntwo\n\nlongest line\n".into());
        assert_eq!(l.len(), 4);
        assert_eq!(
            (0..4).map(|i| l.line(i)).collect::<Vec<_>>(),
            ["one", "two", "", "longest line"]
        );
        assert_eq!(l.widest, 3);
        // No final newline.
        let l = Lines::new("a\nb".into());
        assert_eq!((l.len(), l.line(1)), (2, "b"));
        assert_eq!(Lines::new(String::new()).len(), 0);
        assert_eq!(Lines::new("\n".into()).len(), 1);
    }

    #[test]
    fn ages_between_oldest_and_newest() {
        assert_eq!(age(10, (10, 20)), 0.);
        assert_eq!(age(20, (10, 20)), 1.);
        assert_eq!(age(15, (10, 20)), 0.5);
        assert_eq!(age(5, (5, 5)), 1.);
    }
}
