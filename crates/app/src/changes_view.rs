//! Local Changes: the unstaged and staged file lists with stage / unstage (selected
//! files, a double-clicked file, or everything), and the focused file's diff.
//!
//! Status comes from `git status` on the background executor; staging runs `git add` /
//! `git restore --staged` there too and reloads status when it finishes. The lists keep
//! their selection across reloads ([`Selection::retain`]), so staging walks down the list.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::AppContext as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Render,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use switchhead_git::Repo;
use switchhead_git::diff::{self, FilePatch, LineKind};
use switchhead_git::stage;
use switchhead_git::status::{self, Status, StatusOptions};

mod menu;

use crate::actions;
use crate::appearance::{rpx, ts};
use crate::changes_model::{Badge, LineSelection, Lists, Selection, Side};
use crate::commit_box::{CommitBox, CommitEvent};
use crate::diff_view::{self, Content, Loaded, highlight_theme, theme_key};
use crate::merge_view::{MergeEvent, MergeView};
use crate::store::DiffPrefs;
use crate::theme::{MONO, Palette, SANS, palette};
use crate::ui;

/// What a hunk or line action applies to.
#[derive(Clone, Copy, Debug)]
enum Part {
    /// One hunk (its index).
    Hunk(usize),
    /// The picked lines.
    Lines,
}

/// What the focused file's diff was loaded for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    side: Side,
    path: PathBuf,
}

/// The Local Changes view of a repository tab.
pub struct ChangesView {
    repo: Repo,
    status: Rc<Status>,
    lists: Lists,
    /// Paths of each list, in order (what [`Selection`] works on).
    unstaged_paths: Vec<PathBuf>,
    staged_paths: Vec<PathBuf>,
    selection: Selection,
    shown: Option<Shown>,
    loaded: Option<Rc<Loaded>>,
    /// Status has been read at least once.
    ready: bool,
    /// A status read is running; another was asked for meanwhile.
    reading: bool,
    reread: bool,
    /// A stage / unstage is running.
    busy: bool,
    error: Option<SharedString>,
    unstaged_scroll: UniformListScrollHandle,
    staged_scroll: UniformListScrollHandle,
    diff_scroll: UniformListScrollHandle,
    split: Entity<ResizableState>,
    lists_split: Entity<ResizableState>,
    focus: FocusHandle,
    /// The `git diff` the shown diff came from (for hunk and line staging).
    patch: Option<Arc<FilePatch>>,
    lines: LineSelection,
    _diff: Option<Task<()>>,
    /// The commit box (made on first render: its inputs need the window).
    commit: Option<(Entity<CommitBox>, Subscription)>,
    /// Focus the commit subject once the box exists.
    focus_commit: bool,
    /// Tracked files marked assume-unchanged or skip-worktree.
    hidden: Rc<Vec<status::Hidden>>,
    /// The merge helper for the shown conflicted file.
    merge: Option<(Entity<MergeView>, Subscription)>,
    _subs: Vec<Subscription>,
}

/// What Local Changes reports to the repository view.
pub enum ChangesEvent {
    /// A commit was made: the history changed.
    Committed,
    /// Show a file's history or (work tree) blame.
    OpenFile {
        /// The file.
        path: std::path::PathBuf,
        /// Blame (else history).
        blame: bool,
    },
}

impl gpui_kit::EventEmitter<ChangesEvent> for ChangesView {}

impl ChangesView {
    /// The view for `repo`; [`ChangesView::refresh`] reads status.
    pub fn new(repo: Repo, cx: &mut Context<Self>) -> Self {
        let subs = vec![
            cx.observe_global::<DiffPrefs>(|this, cx| this.refresh_diff(cx)),
            cx.observe_global::<gpui_kit::component::Theme>(|this, cx| this.refresh_diff(cx)),
        ];
        Self {
            repo,
            status: Rc::default(),
            lists: Lists::default(),
            unstaged_paths: Vec::new(),
            staged_paths: Vec::new(),
            selection: Selection::default(),
            shown: None,
            loaded: None,
            ready: false,
            reading: false,
            reread: false,
            busy: false,
            error: None,
            unstaged_scroll: UniformListScrollHandle::new(),
            staged_scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
            split: cx.new(|_| ResizableState::default()),
            lists_split: cx.new(|_| ResizableState::default()),
            focus: cx.focus_handle(),
            patch: None,
            lines: LineSelection::default(),
            _diff: None,
            commit: None,
            focus_commit: false,
            hidden: Rc::default(),
            merge: None,
            _subs: subs,
        }
    }

    /// Changed files (each path once), for the sidebar.
    pub fn count(&self) -> usize {
        self.status
            .entries
            .iter()
            .filter(|e| e.is_staged() || e.is_unstaged() || e.conflict().is_some())
            .count()
    }

    /// Re-read status (and the focused diff). Calls made while a read runs are folded
    /// into one more read when it ends.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.reading {
            self.reread = true;
            return;
        }
        self.reading = true;
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move {
            let st = status::status(&repo, StatusOptions::default())?;
            // A failed index read only loses the hidden-files list.
            let hidden = status::hidden(&repo).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "could not read hidden files");
                Vec::new()
            });
            Ok::<_, switchhead_git::Error>((st, hidden))
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                v.reading = false;
                match res {
                    Ok((st, hidden)) => {
                        v.hidden = Rc::new(hidden);
                        v.set_status(st, cx)
                    }
                    Err(e) => v.error = Some(e.to_string().into()),
                }
                if std::mem::take(&mut v.reread) {
                    v.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Conflicted files.
    pub fn conflicts(&self) -> usize {
        self.status.conflicted().count()
    }

    /// The shown file, when it is conflicted.
    fn shown_conflict(&self) -> Option<(PathBuf, status::Conflict)> {
        let shown = self.shown.as_ref().filter(|s| s.side == Side::Unstaged)?;
        let e = self.status.entries.iter().find(|e| e.path == shown.path)?;
        Some((e.path.clone(), e.conflict()?))
    }

    /// The merge helper for the shown conflicted file (made on first render).
    fn merge_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<MergeView>> {
        let Some((path, kind)) = self.shown_conflict() else {
            self.merge = None;
            return None;
        };
        if let Some((m, _)) = &self.merge
            && m.read(cx).path == path
        {
            return Some(m.clone());
        }
        let repo = self.repo.clone();
        let m = cx.new(|cx| MergeView::new(repo, path, kind, window, cx));
        let sub = cx.subscribe(&m, |this, _, ev: &MergeEvent, cx| match ev {
            MergeEvent::Resolved => {
                this.merge = None;
                this.refresh(cx);
            }
        });
        self.merge = Some((m.clone(), sub));
        Some(m)
    }

    /// Show the commit box and put the cursor in its subject.
    pub fn focus_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.commit {
            Some((b, _)) => b.update(cx, |b, cx| b.focus(window, cx)),
            None => {
                self.focus_commit = true;
                cx.notify();
            }
        }
    }

    fn commit_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<CommitBox> {
        if let Some((b, _)) = &self.commit {
            return b.clone();
        }
        let staged = self.lists.staged.len();
        let b = cx.new(|cx| {
            let mut b = CommitBox::new(self.repo.clone(), window, cx);
            b.set_staged(staged, cx);
            b
        });
        let sub = cx.subscribe(&b, |this, _, ev: &CommitEvent, cx| match ev {
            CommitEvent::Committed => {
                this.refresh(cx);
                cx.emit(ChangesEvent::Committed);
            }
        });
        if std::mem::take(&mut self.focus_commit) {
            b.update(cx, |b, cx| b.focus(window, cx));
        }
        self.commit = Some((b.clone(), sub));
        b
    }

    fn set_status(&mut self, st: Status, cx: &mut Context<Self>) {
        self.ready = true;
        self.lists = Lists::new(&st);
        if let Some((b, _)) = &self.commit {
            let n = self.lists.staged.len();
            b.update(cx, |b, cx| b.set_staged(n, cx));
        }
        let paths = |ix: &[usize]| ix.iter().map(|i| st.entries[*i].path.clone()).collect();
        self.unstaged_paths = paths(&self.lists.unstaged);
        self.staged_paths = paths(&self.lists.staged);
        self.status = Rc::new(st);
        let list = self.paths(self.selection.side).to_vec();
        let mut cursor = self.selection.retain(&list);
        if cursor.is_none() && self.selection.paths.is_empty() {
            // Nothing left where the selection was: start at the top of a non-empty list.
            let side = if self.unstaged_paths.is_empty() {
                Side::Staged
            } else {
                Side::Unstaged
            };
            if !self.paths(side).is_empty() {
                let list = self.paths(side).to_vec();
                self.selection.click(side, &list, 0);
                cursor = list.first().cloned();
            }
        }
        let shown = cursor.map(|path| Shown {
            side: self.selection.side,
            path,
        });
        // Same file: reload its diff anyway, its content may have changed.
        self.shown = shown;
        self.load_diff(cx);
    }

    fn paths(&self, side: Side) -> &[PathBuf] {
        match side {
            Side::Unstaged => &self.unstaged_paths,
            Side::Staged => &self.staged_paths,
        }
    }

    fn scroll(&self, side: Side) -> &UniformListScrollHandle {
        match side {
            Side::Unstaged => &self.unstaged_scroll,
            Side::Staged => &self.staged_scroll,
        }
    }

    // ---------------------------------------------------------------- selection

    fn on_click(&mut self, side: Side, i: usize, ev: &ClickEvent, cx: &mut Context<Self>) {
        let list = self.paths(side).to_vec();
        let m = ev.modifiers();
        if ev.click_count() >= 2 && !m.shift && !m.secondary() {
            if let Some(p) = list.get(i) {
                self.selection.click(side, &list, i);
                self.apply(side, vec![p.clone()], cx);
            }
            return;
        }
        if m.shift {
            self.selection.extend(side, &list, i);
        } else if m.secondary() {
            self.selection.toggle(side, &list, i);
        } else {
            self.selection.click(side, &list, i);
        }
        self.cursor_changed(cx);
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let side = self.selection.side;
        let list = self.paths(side).to_vec();
        self.selection.step(&list, delta);
        self.cursor_changed(cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let side = self.selection.side;
        let list = self.paths(side).to_vec();
        self.selection.all(side, &list);
        cx.notify();
    }

    /// Show the cursor's diff and keep it in view.
    fn cursor_changed(&mut self, cx: &mut Context<Self>) {
        let side = self.selection.side;
        let shown = self.selection.cursor.clone().map(|(path, i)| {
            self.scroll(side).scroll_to_item(i, ScrollStrategy::Nearest);
            Shown { side, path }
        });
        if shown != self.shown {
            self.shown = shown;
            self.load_diff(cx);
        }
        cx.notify();
    }

    // ---------------------------------------------------------------- staging

    /// Stage (from the unstaged list) or unstage (from the staged list) the selection.
    pub fn toggle_selected(&mut self, cx: &mut Context<Self>) {
        let side = self.selection.side;
        let paths: Vec<PathBuf> = self
            .paths(side)
            .iter()
            .filter(|p| self.selection.paths.contains(*p))
            .cloned()
            .collect();
        self.apply(side, paths, cx);
    }

    /// Stage everything.
    pub fn stage_all(&mut self, cx: &mut Context<Self>) {
        if !self.unstaged_paths.is_empty() {
            self.run(|r| r.stage_all(), cx);
        }
    }

    /// Unstage everything.
    pub fn unstage_all(&mut self, cx: &mut Context<Self>) {
        if !self.staged_paths.is_empty() {
            self.run(|r| r.unstage_all(), cx);
        }
    }

    /// Move `paths` from `side` to the other list.
    fn apply(&mut self, side: Side, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        // A renamed file unstages under both of its names.
        let mut all = paths.clone();
        if side == Side::Staged {
            for &i in &self.lists.staged {
                let e = &self.status.entries[i];
                if let Some(orig) = &e.orig_path
                    && paths.contains(&e.path)
                {
                    all.push(orig.clone());
                }
            }
        }
        self.run(
            move |r| {
                let refs: Vec<&Path> = all.iter().map(PathBuf::as_path).collect();
                match side {
                    Side::Unstaged => r.stage(&refs),
                    Side::Staged => r.unstage(&refs),
                }
            },
            cx,
        );
    }

    fn run(
        &mut self,
        f: impl FnOnce(&Repo) -> switchhead_git::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move { f(&repo) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                if let Err(e) = res {
                    v.error = Some(first_line(&e.to_string()).into());
                }
                v.refresh(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ---------------------------------------------------------------- diff

    fn refresh_diff(&mut self, cx: &mut Context<Self>) {
        let stale = self.loaded.as_ref().is_some_and(|l| {
            l.prefs != *cx.global::<DiffPrefs>() || l.theme != theme_key(&highlight_theme(cx))
        });
        if stale {
            self.load_diff(cx);
        }
        cx.notify();
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = self.shown.clone() else {
            self.loaded = None;
            self.patch = None;
            self.lines.clear();
            self._diff = None;
            return;
        };
        let entry = self
            .status
            .entries
            .iter()
            .find(|e| e.path == shown.path)
            .cloned();
        let Some(entry) = entry else {
            return;
        };
        let prefs = *cx.global::<DiffPrefs>();
        let theme = highlight_theme(cx);
        let key = theme_key(&theme);
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move {
            let started = std::time::Instant::now();
            let (content, patch) = local_diff(&repo, &entry, shown.side, &prefs, &theme);
            tracing::debug!(
                path = %entry.path.display(),
                ms = started.elapsed().as_millis() as u64,
                "local diff"
            );
            (content, patch)
        });
        let for_shown = self.shown.clone();
        self._diff = Some(cx.spawn(async move |this, cx| {
            let (content, patch) = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.shown != for_shown {
                    return;
                }
                let same = v
                    .loaded
                    .as_ref()
                    .is_some_and(|l| l.file == file_key(&v.shown));
                v.patch = patch.map(Arc::new);
                v.lines.clear();
                v.loaded = Some(Rc::new(Loaded {
                    file: file_key(&v.shown),
                    prefs,
                    theme: key,
                    content,
                }));
                if !same {
                    v.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                cx.notify();
            });
        }));
    }

    // ---------------------------------------------------------------- hunks and lines

    /// Clicks on changed lines and hunk buttons, when the shown diff can be staged
    /// in parts.
    fn interact(&self, cx: &mut Context<Self>) -> Option<Rc<diff_view::Interact>> {
        self.patch.as_ref()?;
        let side = self.shown.as_ref()?.side;
        let this = cx.entity().downgrade();
        let on_line: diff_view::OnLine = Rc::new(move |l, ev, _w, cx| {
            let _ = this.update(cx, |v, cx| v.click_line(l, ev, cx));
        });
        let this = cx.entity().downgrade();
        let busy = self.busy;
        let hunk_buttons: diff_view::HunkButtons = Rc::new(move |h, p| {
            let button = |label: &'static str, action: stage::Action| {
                let this = this.clone();
                small_button(("hunk", h * 4 + action as usize), label, !busy, p)
                    .on_click(move |_, window, cx| {
                        let _ = this
                            .update(cx, |v, cx| v.apply_part(action, Part::Hunk(h), window, cx));
                    })
                    .into_any_element()
            };
            match side {
                Side::Unstaged => vec![
                    button("Stage Hunk", stage::Action::Stage),
                    button("Discard", stage::Action::Discard),
                ],
                Side::Staged => vec![button("Unstage Hunk", stage::Action::Unstage)],
            }
        });
        Some(Rc::new(diff_view::Interact {
            selected: self.lines.lines.clone(),
            on_line,
            hunk_buttons,
        }))
    }

    fn click_line(&mut self, l: usize, ev: &ClickEvent, cx: &mut Context<Self>) {
        let m = ev.modifiers();
        if m.shift {
            let changed: Vec<usize> = self.patch.as_ref().map_or_else(Vec::new, |p| {
                (0..p.diff.len())
                    .filter(|i| p.diff.line(*i).kind != LineKind::Context)
                    .collect()
            });
            self.lines.extend(l, &changed);
        } else if m.secondary() {
            self.lines.toggle(l);
        } else {
            self.lines.click(l);
        }
        cx.notify();
    }

    /// "Stage Lines" / "Discard Lines" / "Unstage Lines" while lines are picked.
    fn line_buttons(&self, p: &Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let n = self.lines.lines.len();
        let Some(side) = self.shown.as_ref().map(|s| s.side).filter(|_| n > 0) else {
            return Vec::new();
        };
        let label = |verb: &str| {
            if n == 1 {
                format!("{verb} Line")
            } else {
                format!("{verb} {n} Lines")
            }
        };
        let button = |id: &'static str, text: String, action: stage::Action| {
            small_button(id, text, !self.busy, p)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_part(action, Part::Lines, window, cx)
                }))
                .into_any_element()
        };
        match side {
            Side::Unstaged => vec![
                button("stage-lines", label("Stage"), stage::Action::Stage),
                button("discard-lines", label("Discard"), stage::Action::Discard),
            ],
            Side::Staged => vec![button(
                "unstage-lines",
                label("Unstage"),
                stage::Action::Unstage,
            )],
        }
    }

    /// Stage, unstage or discard a hunk or the picked lines. Discarding asks first and
    /// records an undo snapshot of the file.
    fn apply_part(
        &mut self,
        action: stage::Action,
        part: Part,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(patch) = self.patch.clone() else {
            return;
        };
        let chosen: std::collections::HashSet<usize> = match part {
            Part::Hunk(h) => patch
                .diff
                .hunks()
                .get(h)
                .map(|h| h.lines.clone().collect())
                .unwrap_or_default(),
            Part::Lines => self.lines.lines.clone(),
        };
        if chosen.is_empty() || self.busy {
            return;
        }
        let what = match part {
            Part::Hunk(_) => "this hunk",
            Part::Lines => "the selected lines",
        };
        let file = patch.new_path.clone().or_else(|| patch.old_path.clone());
        let label = format!(
            "Discard {what} in {}",
            file.as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
        let run = move |this: &mut Self, cx: &mut Context<Self>| {
            let (patch, chosen) = (patch.clone(), chosen.clone());
            let (file, label) = (file.clone(), label.clone());
            this.run(
                move |r| {
                    if action == stage::Action::Discard
                        && let Some(f) = &file
                    {
                        switchhead_git::undo::guard(r, &label, &[f.as_path()]);
                    }
                    stage::apply_lines(r, &patch, |i| chosen.contains(&i), action).map(|_| ())
                },
                cx,
            );
        };
        if action != stage::Action::Discard {
            return run(self, cx);
        }
        let this = cx.entity().downgrade();
        let run = Rc::new(run);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let run = run.clone();
            alert
                .confirm()
                .title(format!("Discard {what}?"))
                .description(
                    "The changes are removed from the file. Undo (Ctrl/Cmd+Z) brings them back.",
                )
                .ok_text("Discard")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let run = run.clone();
                    let _ = this.update(cx, |v, cx| run(v, cx));
                    true
                })
        });
    }

    // ---------------------------------------------------------------- render

    fn render_list(
        &self,
        side: Side,
        p: &Palette,
        row_h: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let n = self.paths(side).len();
        let selected = if self.selection.side == side {
            self.selection.paths.len()
        } else {
            0
        };
        let (title, one, all, empty) = match side {
            Side::Unstaged => ("Unstaged", "Stage", "Stage All", "No unstaged changes"),
            Side::Staged => ("Staged", "Unstage", "Unstage All", "Nothing staged"),
        };
        let header = div()
            .h(rpx(30.))
            .flex_none()
            .flex()
            .items_center()
            .gap(rpx(6.))
            .px(rpx(12.))
            .bg(p.panel)
            .border_b_1()
            .border_color(p.bd)
            .child(
                div()
                    .text_size(ts::LABEL)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.fg)
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .font_family(MONO)
                    .text_size(ts::CAPTION)
                    .text_color(p.fg3)
                    .child(n.to_string()),
            )
            .when(selected > 0, |d| {
                d.child(
                    small_button(
                        ("one", side as usize),
                        if selected > 1 {
                            format!("{one} {selected}")
                        } else {
                            one.to_owned()
                        },
                        !self.busy,
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_selected(cx))),
                )
            })
            .when(side == Side::Unstaged, |d| {
                d.children(self.render_hidden_menu(cx))
            })
            .child(
                small_button(("all", side as usize), all, n > 0 && !self.busy, p).on_click(
                    cx.listener(move |this, _, _, cx| match side {
                        Side::Unstaged => this.stage_all(cx),
                        Side::Staged => this.unstage_all(cx),
                    }),
                ),
            );
        let body = if n == 0 {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(ts::SMALL)
                .text_color(p.fg3)
                .child(if self.ready { empty } else { "" })
                .into_any_element()
        } else {
            let list = uniform_list(
                match side {
                    Side::Unstaged => "unstaged",
                    Side::Staged => "staged",
                },
                n,
                cx.processor(move |this, range: std::ops::Range<usize>, _w, cx| {
                    let p = palette(cx);
                    range
                        .map(|i| this.render_row(side, i, &p, row_h, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll(side))
            .size_full()
            .into_any_element();
            let view = cx.entity().downgrade();
            div()
                .id(match side {
                    Side::Unstaged => "unstaged-menu",
                    Side::Staged => "staged-menu",
                })
                .flex_1()
                .min_h_0()
                .child(list)
                .context_menu(move |menu, window, cx| menu::build(menu, &view, window, cx))
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_row(
        &self,
        side: Side,
        i: usize,
        p: &Palette,
        row_h: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(e) = self
            .lists
            .get(side)
            .get(i)
            .and_then(|ix| self.status.entries.get(*ix))
        else {
            return div().into_any_element();
        };
        let selected = self.selection.contains(side, &e.path);
        let cursor = self.selection.side == side
            && self
                .selection
                .cursor
                .as_ref()
                .is_some_and(|(c, _)| *c == e.path);
        let path = e.path.to_string_lossy().into_owned();
        let (dir, name) = match path.rsplit_once('/') {
            Some((d, n)) => (Some(d.to_owned()), n.to_owned()),
            None => (None, path.clone()),
        };
        let badge = Badge::of(e, side);
        div()
            .id((
                match side {
                    Side::Unstaged => "u",
                    Side::Staged => "s",
                },
                i,
            ))
            .w_full()
            .h(px(row_h))
            .flex()
            .items_center()
            .gap(rpx(6.))
            .px(rpx(10.))
            .text_size(ts::BODY)
            .cursor_pointer()
            .when(selected, |d| d.bg(p.sel))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
            .when(cursor && selected, |d| d.border_l_2().border_color(p.acc))
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                window.focus(&this.focus, cx);
                this.on_click(side, i, ev, cx);
            }))
            .capture_any_mouse_down(cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                if ev.button == MouseButton::Right {
                    window.focus(&this.focus, cx);
                    this.select_for_menu(side, i, cx);
                }
            }))
            .child(badge_chip(badge, p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(div().flex_none().text_color(p.fg).child(name))
                    .when_some(dir, |d, dir| {
                        d.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .pl(rpx(6.))
                                .text_color(p.fg3)
                                .child(dir),
                        )
                    }),
            )
            .when_some(
                e.orig_path.as_ref().filter(|_| side == Side::Staged),
                |d, old| {
                    d.child(
                        div()
                            .flex_none()
                            .max_w(rpx(160.))
                            .truncate()
                            .text_size(ts::SMALL)
                            .text_color(p.fg3)
                            .child(SharedString::from(format!(
                                "from {}",
                                old.to_string_lossy()
                            ))),
                    )
                },
            )
            .into_any_element()
    }

    fn render_diff_toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let stats = self.loaded.as_ref().and_then(|l| l.content.stats());
        div()
            .h(rpx(30.))
            .flex_none()
            .flex()
            .items_center()
            .gap(rpx(8.))
            .px(rpx(12.))
            .bg(p.panel)
            .border_b_1()
            .border_color(p.bd)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(rpx(6.))
                    .when_some(self.shown.as_ref(), |d, s| {
                        d.child(ui::chip(
                            match s.side {
                                Side::Unstaged => "Unstaged",
                                Side::Staged => "Staged",
                            },
                            p.fg3,
                        ))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family(MONO)
                                .text_size(ts::SMALL)
                                .text_color(p.fg)
                                .child(SharedString::from(s.path.to_string_lossy().into_owned())),
                        )
                    })
                    .when_some(stats, |d, (a, r)| {
                        d.children(diff_view::stats_chips(a, r, p))
                    }),
            )
            .children(self.line_buttons(p, cx))
            // `git diff` of the work tree ignores the whitespace setting (hunk staging
            // needs the real lines), so it is not offered here.
            .child(diff_view::settings(p, false, cx))
            .into_any_element()
    }

    fn render_diff(&self, p: &Palette, row_h: f32, cx: &mut Context<Self>) -> AnyElement {
        match &self.loaded {
            Some(l) if l.file == file_key(&self.shown) => {
                let interact = self.interact(cx);
                diff_view::body(l, &self.diff_scroll, row_h, p, interact, cx)
            }
            _ if self.shown.is_some() => div().flex_1().into_any_element(),
            _ if self.ready && self.count() == 0 => diff_view::message("No local changes.", p.fg3),
            _ => diff_view::message("Select a file to see its changes.", p.fg3),
        }
    }
}

/// The diff of one entry on one side.
fn local_diff(
    repo: &Repo,
    e: &status::Entry,
    side: Side,
    prefs: &DiffPrefs,
    theme: &std::sync::Arc<gpui_kit::component::highlighter::HighlightTheme>,
) -> (Content, Option<FilePatch>) {
    if e.conflict().is_some() {
        let note = "This file has conflicts. Resolve them in the file, then stage it.";
        return (Content::Failed(note.into()), None);
    }
    if e.kind == status::EntryKind::Untracked {
        let full = repo.root().join(&e.path);
        let content = match std::fs::read(&full) {
            Ok(bytes) => Content::from_bytes(&e.path, None, Some(&bytes), prefs, theme),
            Err(err) => Content::Failed(format!("Cannot read {}: {err}", e.path.display())),
        };
        return (content, None);
    }
    let mut paths: Vec<&Path> = vec![e.path.as_path()];
    if side == Side::Staged
        && let Some(orig) = &e.orig_path
    {
        paths.push(orig);
    }
    let res = match side {
        Side::Unstaged => diff::worktree_diff(repo, &paths, prefs.context),
        Side::Staged => diff::index_diff(repo, &paths, prefs.context),
    };
    match res {
        Ok(files) => match files.into_iter().next() {
            Some(f) if f.binary => (local_binary(repo, e, side, prefs, theme), None),
            Some(f) => (Content::text(f.diff.clone(), &e.path, None, theme), Some(f)),
            None => (
                Content::text(Default::default(), &e.path, None, theme),
                None,
            ),
        },
        Err(err) => (Content::Failed(err.to_string()), None),
    }
}

/// A binary file's two versions (index and work tree, or HEAD and index), for an image
/// comparison or the binary file note.
fn local_binary(
    repo: &Repo,
    e: &status::Entry,
    side: Side,
    prefs: &DiffPrefs,
    theme: &std::sync::Arc<gpui_kit::component::highlighter::HighlightTheme>,
) -> Content {
    use switchhead_git::diff::{Source, read_at};
    let old_path = e.orig_path.as_deref().unwrap_or(&e.path);
    let (old, new) = match side {
        Side::Unstaged => (
            read_at(repo, &e.path, Source::Index),
            Ok(std::fs::read(repo.root().join(&e.path)).ok()),
        ),
        Side::Staged => (
            read_at(repo, old_path, Source::Head),
            read_at(repo, &e.path, Source::Index),
        ),
    };
    match (old, new) {
        (Ok(old), Ok(new)) => {
            Content::from_bytes(&e.path, old.as_deref(), new.as_deref(), prefs, theme)
        }
        (Err(err), _) | (_, Err(err)) => Content::Failed(err.to_string()),
    }
}

/// A key for [`Loaded::file`] that changes with the shown file and side.
fn file_key(shown: &Option<Shown>) -> usize {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::hash::DefaultHasher::new();
    shown.hash(&mut h);
    h.finish() as usize
}

impl std::hash::Hash for Shown {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.side.hash(h);
        self.path.hash(h);
    }
}

fn badge_chip(b: Badge, p: &Palette) -> AnyElement {
    let color: Hsla = match b {
        Badge::Added | Badge::Untracked => p.added,
        Badge::Deleted | Badge::Conflict => p.deleted,
        Badge::Renamed => p.acc,
        Badge::Modified => p.modified,
    };
    div()
        .flex_none()
        .size(rpx(15.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .bg(color.opacity(0.16))
        .text_color(color)
        .font_family(MONO)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(ts::CAPTION)
        .child(b.letter())
        .into_any_element()
}

/// A compact header button.
pub(crate) fn small_button(
    id: impl Into<gpui_kit::ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    p: &Palette,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id.into())
        .flex_none()
        .h(rpx(20.))
        .px(rpx(8.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(p.bd2)
        .font_family(SANS)
        .text_size(ts::SMALL)
        .font_weight(FontWeight::MEDIUM)
        .text_color(if enabled { p.fg } else { p.fg3 })
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(p.hover)))
        .child(label.into())
}

pub(crate) fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("failed")
        .to_owned()
}

impl Focusable for ChangesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChangesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let rem = window.rem_size();
        let row_h = diff_view::row_height(cx);
        let file_row_h = (22. * crate::appearance::zoom(cx)).round();
        let commit_box = self.commit_box(window, cx);
        let left =
            div()
                .size_full()
                .flex()
                .flex_col()
                .bg(p.surface)
                .when_some(self.error.clone(), |d, e| {
                    d.child(
                        div()
                            .flex_none()
                            .px(rpx(12.))
                            .py(rpx(6.))
                            .bg(p.deleted.opacity(0.12))
                            .text_size(ts::SMALL)
                            .text_color(p.deleted)
                            .child(e),
                    )
                })
                .when(self.busy, |d| d.child(ui::loading_bar("staging", &p)))
                .child(
                    div().flex_1().min_h_0().child(
                        gpui_kit::component::resizable::v_resizable("change-lists")
                            .with_state(&self.lists_split)
                            .child(
                                resizable_panel()
                                    .size_range(rem * (80. / 13.)..gpui_kit::Pixels::MAX)
                                    .child(self.render_list(Side::Unstaged, &p, file_row_h, cx)),
                            )
                            .child(
                                resizable_panel()
                                    .size_range(rem * (80. / 13.)..gpui_kit::Pixels::MAX)
                                    .child(
                                        div()
                                            .size_full()
                                            .flex()
                                            .flex_col()
                                            .border_t_1()
                                            .border_color(p.bd)
                                            .child(div().flex_1().min_h_0().child(
                                                self.render_list(Side::Staged, &p, file_row_h, cx),
                                            ))
                                            .child(commit_box),
                                    ),
                            ),
                    ),
                );
        let right = match self.merge_view(window, cx) {
            Some(m) => div().size_full().child(m),
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .bg(p.surface)
                .child(self.render_diff_toolbar(&p, cx))
                .child(self.render_diff(&p, row_h, cx)),
        };
        div()
            .id("changes")
            .key_context("Changes")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(p.surface)
            .on_action(cx.listener(|this, _: &actions::SelectUp, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &actions::SelectDown, _, cx| this.step(1, cx)))
            .on_action(
                cx.listener(|this, _: &actions::SelectFirst, _, cx| this.step(isize::MIN / 2, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::SelectLast, _, cx| this.step(isize::MAX / 2, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::SelectAllFiles, _, cx| this.select_all(cx)))
            .on_action(
                cx.listener(|this, _: &actions::ToggleStaged, _, cx| this.toggle_selected(cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::DiscardSelected, window, cx| {
                    this.discard_selected(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &actions::SwitchFileList, _, cx| {
                let side = this.selection.side.other();
                let list = this.paths(side).to_vec();
                if !list.is_empty() {
                    this.selection.click(side, &list, 0);
                    this.cursor_changed(cx);
                }
            }))
            .child(
                h_resizable("changes-split")
                    .with_state(&self.split)
                    .child(
                        resizable_panel()
                            .size(rem * (340. / 13.))
                            .size_range(rem * (200. / 13.)..rem * (900. / 13.))
                            .child(left),
                    )
                    .child(resizable_panel().child(right)),
            )
    }
}
