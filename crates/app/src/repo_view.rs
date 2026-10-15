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
use std::path::PathBuf;

use gpui_kit::AppContext as _;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::resizable::{ResizableState, resizable_panel, v_resizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, UniformListScrollHandle,
    Window, canvas, div, px, uniform_list,
};
use switchhead_git::cli::Cancel;
use switchhead_git::flow::Kind;
use switchhead_git::graph::{GraphRows, Node, Segment};
use switchhead_git::log::{self, CommitLog, LogOptions, Scope, WalkStats};
use switchhead_git::ops::DiffSides;
use switchhead_git::refs::{self, Label, LabelKind, Refs};
use switchhead_git::watch::{self, Fingerprint};
use switchhead_git::{Oid, Repo, RepoState};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::actions;
use crate::activity::{Activity, Outcome};
use crate::appearance::{rpx, ts};
use crate::changes_view::{ChangesEvent, ChangesView};
use crate::commit_view::{CommitExternal, CommitView, Navigate, OpenFile, RunCustom};
use crate::external::ExternalKind;
use crate::graph_paint;
use crate::theme::{MONO, Palette, palette};
use crate::ui;

mod activity_panel;
mod branch_ops;
mod commit_menu;
mod custom_ops;
mod external_ops;
mod file_history;
mod flow_ops;
mod form;
mod lfs_ops;
mod patch_ops;
mod pr_ops;
mod rebase;
mod reflog;
mod remote_ops;
mod search;
mod section_menu;
mod sidebar;
mod signing_ops;
mod stash_ops;
mod submodule_ops;
mod tag_ops;
mod tree;
mod undo;
mod worktree_ops;

pub(crate) use custom_ops::build_commands_menu;
pub(crate) use flow_ops::build_flow_menu;

use sidebar::{Section, SideRow};

/// Graph row height at 100 % zoom.
const ROW_H: f32 = 24.0;
/// Rows Page Up / Page Down move.
const PAGE: isize = 20;
/// How often `.git` is checked for outside changes when file events are unavailable.
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
/// With file events, the check still runs every this many intervals (a missed event).
const WATCH_EVERY_WITH_EVENTS: u32 = 10;
/// The graph column never grows past this many lanes (wider rows are clipped).
const MAX_LANES: u16 = 24;

/// Something from the background walk.
enum Msg {
    Refs(Box<Refs>),
    Batch(Box<log::Batch>),
    Done(Result<WalkStats, String>),
    Tracking(Box<Refs>),
    /// Whether the repository uses LFS.
    Lfs(bool),
}

/// Something the workspace acts on.
#[derive(Clone, Debug)]
pub enum RepoViewEvent {
    /// Open this folder (a submodule or worktree) in a tab.
    Open(std::path::PathBuf),
}

impl gpui_kit::EventEmitter<RepoViewEvent> for RepoView {}

/// A step to run after an operation succeeds.
type OpThen = Box<dyn FnOnce(&mut RepoView, &mut Context<RepoView>)>;

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
    /// Stopped by the user.
    Cancelled(SharedString),
}

/// What the main area shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The commit graph and the selected commit's details.
    Commits,
    /// Local Changes: unstaged and staged files.
    Changes,
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
    /// Collapsed branch folders (`b/feature`, `r/origin/...`).
    collapsed_folders: HashSet<String>,
    side_filter: Option<(Entity<gpui_kit::component::input::InputState>, Subscription)>,
    side_filter_text: String,
    side_rows: Vec<SideRow>,
    /// The sidebar row a right click landed on (for its context menu).
    menu_row: Option<usize>,
    /// Cancels the running operation.
    op_cancel: Option<Cancel>,
    /// What to do when the running operation succeeds ([`RepoView::run_op_then`]).
    op_then: Option<OpThen>,
    activity: Activity,
    /// The activity panel is open.
    pub activity_open: bool,
    /// Activity entries the user folded.
    activity_collapsed: HashSet<u64>,
    /// Git LFS: whether it is used, and the server's locks.
    pub lfs: lfs_ops::LfsState,
    /// The assistant panel beside the history.
    helper: Option<(Entity<crate::assistant_panel::AssistantPanel>, Subscription)>,
    /// The commit search bar above the graph.
    search: Option<search::Search>,
    /// A file's history and blame or a reflog, shown in place of the graph or Local Changes.
    overlay: Option<(gpui_kit::AnyView, Subscription)>,
    /// The reflog view, when the overlay is one (reloaded when refs change).
    reflog: Option<gpui_kit::WeakEntity<crate::reflog_view::ReflogView>>,
    /// Focus the current mode at the next render (the file view closed).
    refocus: bool,
    /// A custom command a file list asked for, run at the next render (it may ask first,
    /// which needs the window).
    pending_custom: Option<(usize, custom_ops::CustomOn)>,
    /// The interactive rebase editor, shown in place of the graph.
    rebase: Option<(Entity<crate::rebase_editor::RebaseEditor>, Subscription)>,
    details: Entity<CommitView>,
    details_split: Entity<ResizableState>,
    /// The commit the details pane shows.
    shown: Option<Oid>,
    /// Graph or Local Changes.
    pub mode: Mode,
    changes: Entity<ChangesView>,
    /// The `.git` fingerprint the view last matched (see [`RepoView::check_changes`]).
    watched: Option<Fingerprint>,
    checking: bool,
    /// File events are arriving (see [`RepoView::watch_events`]); polling slows down.
    events_live: bool,
    _walk: Option<Task<()>>,
    _watch: Task<()>,
    _events: Task<()>,
    _details: [Subscription; 4],
    _changes: [Subscription; 2],
    _op: Option<Task<()>>,
}

impl RepoView {
    /// A tab for `repo`; starts loading at once.
    pub fn new(repo: Repo, cx: &mut Context<Self>) -> Self {
        let details = cx.new(|cx| CommitView::new(repo.clone(), cx));
        let open_file = cx.subscribe(&details, |this, _, ev: &OpenFile, cx| {
            this.open_file_view(ev.path.clone(), Some(ev.rev), ev.blame, cx);
        });
        let run_custom = cx.subscribe(&details, |this, _, ev: &RunCustom, cx| {
            this.queue_custom(ev.index, Some(ev.rev), &ev.path, cx)
        });
        let open_external = cx.subscribe(&details, |this, _, ev: &CommitExternal, cx| {
            let sides = DiffSides::Commit {
                base: ev.base,
                rev: ev.rev,
            };
            this.open_external(ev.kind, ev.path.clone(), Some(sides), cx)
        });
        let sub = cx.subscribe(&details, |this, _, Navigate(id), cx| {
            match this.log.row_of(id) {
                Some(row) => this.select(row, cx),
                None => this.pending_select = Some(*id),
            }
            cx.notify();
        });
        let changes = cx.new(|cx| ChangesView::new(repo.clone(), cx));
        let changes_sub = [
            cx.observe(&changes, |_, _, cx| cx.notify()),
            // A commit moved a branch: read refs now rather than at the next poll.
            cx.subscribe(&changes, |this, _, ev: &ChangesEvent, cx| match ev {
                ChangesEvent::Committed => this.check_changes(cx),
                ChangesEvent::OpenFile { path, blame } => {
                    this.open_file_view(path.clone(), None, *blame, cx)
                }
                ChangesEvent::OpenSubmodule(path) => {
                    cx.emit(RepoViewEvent::Open(this.repo.root().join(path)))
                }
                ChangesEvent::UpdateSubmodule(path) => {
                    this.update_submodules(vec![path.clone()], false, cx)
                }
                ChangesEvent::RunCustom { index, path } => {
                    this.queue_custom(*index, None, path, cx)
                }
                ChangesEvent::OpenExternal { kind, path, staged } => {
                    let sides = if *staged {
                        DiffSides::Staged
                    } else {
                        DiffSides::WorkTree
                    };
                    this.open_external(*kind, path.clone(), Some(sides), cx)
                }
                ChangesEvent::LfsLock { path, lock: true } => this.lfs_lock(path.clone(), cx),
                ChangesEvent::LfsLock { path, lock: false } => this.lfs_unlock(path.clone(), cx),
            }),
        ];
        let repo_for_events = repo.clone();
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
            collapsed_folders: HashSet::new(),
            side_filter: None,
            side_filter_text: String::new(),
            side_rows: Vec::new(),
            menu_row: None,
            op_cancel: None,
            op_then: None,
            activity: Activity::default(),
            activity_open: false,
            activity_collapsed: HashSet::new(),
            helper: None,
            lfs: lfs_ops::LfsState::new(),
            rebase: None,
            overlay: None,
            reflog: None,
            refocus: false,
            pending_custom: None,
            search: None,
            details,
            details_split: cx.new(|_| ResizableState::default()),
            shown: None,
            mode: Mode::Commits,
            changes,
            _changes: changes_sub,
            watched: None,
            checking: false,
            events_live: false,
            _watch: Self::watch(cx),
            _events: Self::watch_events(repo_for_events, cx),
            _walk: None,
            _details: [sub, open_file, run_custom, open_external],
            _op: None,
        };
        v.reload(cx);
        v
    }

    /// Check `.git` for changes made outside the app every [`WATCH_INTERVAL`], or less
    /// often while file events arrive.
    fn watch(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut tick = 0u32;
            loop {
                cx.background_executor().timer(WATCH_INTERVAL).await;
                tick = tick.wrapping_add(1);
                let res = this.update(cx, |v, cx| {
                    if !v.events_live || tick.is_multiple_of(WATCH_EVERY_WITH_EVENTS) {
                        v.check_changes(cx);
                    }
                });
                if res.is_err() {
                    break;
                }
            }
        })
    }

    /// Watch the work tree and `.git` with native file events: a change in `.git` checks
    /// refs and the index at once, an edit in the work tree re-reads status. Starting the
    /// watch walks the tree, so it runs off the UI thread; when the OS refuses it (too many
    /// folders for inotify), polling and window focus keep the view current.
    fn watch_events(repo: Repo, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let (tx, mut rx) = unbounded_channel::<watch::Changes>();
            let name = repo.name().to_owned();
            let started = cx
                .background_executor()
                .spawn(async move {
                    watch::Watcher::start(&repo, move |c| {
                        let _ = tx.send(c);
                    })
                })
                .await;
            // Dropped with this task, which stops the watch.
            let _watcher = match started {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!(repo = name, error = %e, "file events unavailable; polling");
                    return;
                }
            };
            if this.update(cx, |v, _| v.events_live = true).is_err() {
                return;
            }
            while let Some(c) = rx.recv().await {
                let res = this.update(cx, |v, cx| {
                    if c.git {
                        v.check_changes(cx);
                    }
                    if c.work_tree {
                        v.changes.update(cx, |ch, cx| ch.refresh(cx));
                    }
                });
                if res.is_err() {
                    break;
                }
            }
        })
    }

    /// Reload when refs changed outside the app (a commit, checkout or fetch in a
    /// terminal). Reads file metadata only, off the UI thread.
    pub fn check_changes(&mut self, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.checking = true;
        let repo = self.repo.clone();
        let task = cx
            .background_executor()
            .spawn(async move { (watch::fingerprint(&repo), repo.state()) });
        cx.spawn(async move |this, cx| {
            let (fp, state) = task.await;
            let _ = this.update(cx, |v, cx| {
                v.checking = false;
                if v.state != state {
                    v.state = state;
                    cx.notify();
                }
                // While a walk or an operation runs, its own reload covers the change;
                // keep the old fingerprint so a change it missed is caught next time.
                if v.loading || matches!(v.op, Some(OpStatus::Running { .. })) {
                    return;
                }
                let old = v.watched.replace(fp);
                if old.is_some_and(|w| w.refs != fp.refs) {
                    tracing::debug!(repo = v.repo.name(), "refs changed outside the app");
                    v.reload(cx);
                    cx.notify();
                } else if old.is_some_and(|w| w.index != fp.index) {
                    v.changes.update(cx, |c, cx| c.refresh(cx));
                }
            });
        })
        .detach();
    }

    /// The window came back to the front: re-read refs if they changed and status
    /// always (files may have been edited elsewhere; status does not touch `.git`).
    pub fn activated(&mut self, cx: &mut Context<Self>) {
        self.check_changes(cx);
        self.changes.update(cx, |c, cx| c.refresh(cx));
    }

    /// Show the graph or Local Changes, and focus it.
    pub fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        self.overlay = None;
        self.reflog = None;
        match mode {
            Mode::Commits => window.focus(&self.focus, cx),
            Mode::Changes => {
                let f = self.changes.focus_handle(cx);
                window.focus(&f, cx);
            }
        }
        cx.notify();
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

    /// Whether the assistant panel is open.
    pub fn helper_open(&self) -> bool {
        self.helper.is_some()
    }

    /// Open the assistant panel (focusing its question) or close it.
    pub fn toggle_helper(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.helper.take().is_some() {
            cx.notify();
            return;
        }
        let repo = self.repo.clone();
        let helper = cx.new(|cx| crate::assistant_panel::AssistantPanel::new(repo, window, cx));
        let sub = cx.subscribe_in(&helper, window, |this, _, ev, window, cx| match ev {
            crate::assistant_panel::PanelEvent::Ran => this.reload(cx),
            crate::assistant_panel::PanelEvent::Close => {
                this.helper = None;
                window.focus(&this.focus, cx);
                cx.notify();
            }
        });
        helper.update(cx, |h, cx| h.focus_input(window, cx));
        self.helper = Some((helper, sub));
        cx.notify();
    }

    /// Show `prompt`'s answer in the assistant panel (opening it), as a new conversation, titled `title`.
    pub(super) fn assist(
        &mut self,
        title: String,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.helper.is_none() {
            self.toggle_helper(window, cx);
        }
        if let Some((helper, _)) = &self.helper {
            helper.update(cx, |h, cx| h.ask_prompt(title, prompt, cx));
        }
    }

    /// Ask the assistant what commit `id` does.
    pub(super) fn explain_commit(
        &mut self,
        id: Oid,
        summary: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let short = switchhead_git::short(&id);
        let prompt = crate::assistant::explain_commit_prompt(&id.to_string(), &summary);
        self.assist(format!("Explain {short} {summary}"), prompt, window, cx);
    }

    /// The branch `branch` is reviewed against, when there is one.
    pub(super) fn review_base(&self, branch: &str) -> Option<String> {
        let locals: Vec<String> = self.refs.branches.iter().map(|b| b.name.clone()).collect();
        let remotes: Vec<String> = self
            .refs
            .remotes
            .iter()
            .filter(|r| !r.is_symbolic_head)
            .map(|r| r.short())
            .collect();
        let upstream = self
            .refs
            .branches
            .iter()
            .find(|b| b.name == branch)
            .and_then(|b| b.tracking.as_ref())
            .map(|t| t.upstream.clone());
        crate::assistant::review_base(branch, &locals, &remotes, upstream.as_deref())
    }

    /// Ask the assistant to review `branch` against `base`.
    pub(super) fn review_branch(
        &mut self,
        branch: String,
        base: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = crate::assistant::review_prompt(&branch, &base);
        self.assist(
            format!("Review {branch} against {base}"),
            prompt,
            window,
            cx,
        );
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
        self.changes.update(cx, |c, cx| c.refresh(cx));
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
                        v.apply(m, &mut fresh, cx);
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        }));
    }

    fn apply(&mut self, msg: Msg, fresh: &mut bool, cx: &mut Context<Self>) {
        match msg {
            Msg::Lfs(used) => self.set_lfs_used(used, cx),
            Msg::Refs(refs) | Msg::Tracking(refs) => {
                // A fresh tab starts on HEAD, like Fork.
                if self.selected.is_none() && self.pending_select.is_none() && *fresh {
                    self.pending_select = refs.head;
                }
                self.decorations = refs.decorations();
                self.refs = *refs;
                if let Some(r) = self.reflog.as_ref().and_then(|r| r.upgrade()) {
                    r.update(cx, |r, cx| r.reload(cx));
                }
                self.rebuild_side();
                self.sync_details(true, cx);
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
                    self.select(row, cx);
                }
                self.sync_details(false, cx);
            }
            Msg::Done(res) => {
                self.loading = false;
                if std::mem::take(fresh) {
                    // Nothing to show (an unborn branch, or a scope with no commits).
                    self.log = CommitLog::new();
                    self.graph = GraphRows::new();
                    self.selected = None;
                }
                self.sync_details(false, cx);
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

    fn select(&mut self, row: usize, cx: &mut Context<Self>) {
        if row < self.log.len() {
            self.selected = Some(row);
            self.scroll.scroll_to_item(row, ScrollStrategy::Center);
            self.sync_details(false, cx);
        }
    }

    /// Point the details pane at the selected commit. `labels`: refs changed, so send its
    /// labels even when the commit did not change.
    fn sync_details(&mut self, labels: bool, cx: &mut Context<Self>) {
        let id = self.selected.map(|i| self.log.id(i));
        if id == self.shown && !labels {
            return;
        }
        self.shown = id;
        let ls = id
            .and_then(|id| self.decorations.get(&id).cloned())
            .unwrap_or_default();
        let remote = self.main_remote_url().map(str::to_owned);
        self.details.update(cx, |d, cx| {
            d.set_remote(remote);
            d.show(id, ls, cx)
        });
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
        self.sync_details(false, cx);
        cx.notify();
    }

    /// Select HEAD's commit (it is loaded first in every scope that contains it).
    pub fn jump_to_head(&mut self, cx: &mut Context<Self>) {
        if let Some(head) = self.refs.head {
            match self.log.row_of(&head) {
                Some(row) => self.select(row, cx),
                None => self.pending_select = Some(head),
            }
            cx.notify();
        }
    }

    /// Select commit `id`: at once when it is loaded, else when its batch arrives. A
    /// commit outside a filtered view switches the graph to every branch first.
    pub fn reveal(&mut self, id: Oid, cx: &mut Context<Self>) {
        match self.log.row_of(&id) {
            Some(row) => self.select(row, cx),
            None => {
                self.pending_select = Some(id);
                if !self.loading && self.scope != Scope::All {
                    self.set_scope(Scope::All, cx);
                }
            }
        }
        cx.notify();
    }

    /// Branches, remote branches and tags, for Quick Launch.
    pub fn palette_refs(&self) -> Vec<crate::palette::RefItem> {
        use crate::palette::RefItem;
        let r = &self.refs;
        r.branches
            .iter()
            .map(|b| RefItem {
                name: b.name.clone(),
                kind: "Branch",
                id: b.id,
            })
            .chain(
                r.remotes
                    .iter()
                    .filter(|b| !b.is_symbolic_head)
                    .map(|b| RefItem {
                        name: b.short(),
                        kind: "Remote branch",
                        id: b.id,
                    }),
            )
            .chain(r.tags.iter().map(|t| RefItem {
                name: t.name.clone(),
                kind: "Tag",
                id: t.id,
            }))
            .collect()
    }

    /// Copy the selected commit's full id.
    fn copy_commit_id(&mut self, cx: &mut Context<Self>) {
        if let Some(i) = self.selected {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                self.log.id(i).to_string(),
            ));
        }
    }

    /// Show only `rev` (`None`: every branch) and reload.
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.reload(cx);
            self.run_search(false, cx);
            cx.notify();
        }
    }

    // ---------------------------------------------------------------- operations

    /// Run a git operation on a thread, show its progress, reload when it ends.
    pub fn run_op(
        &mut self,
        label: impl Into<SharedString>,
        done: impl Into<SharedString>,
        f: impl FnOnce(&Repo, &Cancel, &mut dyn FnMut(&str)) -> switchhead_git::Result<()>
        + Send
        + 'static,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.op, Some(OpStatus::Running { .. })) {
            return;
        }
        let (label, done): (SharedString, SharedString) = (label.into(), done.into());
        self.op = Some(OpStatus::Running {
            label: label.clone(),
            progress: SharedString::default(),
        });
        let entry = self.activity.start(&label);
        cx.notify();
        let (tx, mut rx) = unbounded_channel::<Result<String, String>>();
        let repo = self.repo.clone();
        let cancel = Cancel::default();
        self.op_cancel = Some(cancel.clone());
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
            let msg = format!("{label} failed: {e}");
            self.activity.finish(entry, Outcome::Failed, &msg);
            self.op = Some(OpStatus::Failed(msg.into()));
            self.op_cancel = None;
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
                                v.activity.line(entry, line);
                                v.op = Some(OpStatus::Running {
                                    label: label.clone(),
                                    progress: line.to_owned().into(),
                                });
                            }
                        }
                        Err(e) => {
                            let cancelled = v.op_cancel.take().is_some_and(|c| c.is_cancelled());
                            if cancelled {
                                v.activity.finish(entry, Outcome::Cancelled, "");
                                v.op =
                                    Some(OpStatus::Cancelled(format!("{label}: cancelled").into()));
                            } else if e.is_empty() {
                                v.activity.finish(entry, Outcome::Done, "");
                                v.op = Some(OpStatus::Done(done.clone()));
                                if let Some(then) = v.op_then.take() {
                                    then(v, cx);
                                }
                            } else {
                                v.activity.finish(entry, Outcome::Failed, &e);
                                v.op = Some(OpStatus::Failed(crate::activity::summary(&e).into()));
                            }
                            v.op_then = None;
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

    /// [`RepoView::run_op`], then `then` once it succeeds.
    pub fn run_op_then(
        &mut self,
        label: impl Into<SharedString>,
        done: impl Into<SharedString>,
        f: impl FnOnce(&Repo, &Cancel, &mut dyn FnMut(&str)) -> switchhead_git::Result<()>
        + Send
        + 'static,
        then: impl FnOnce(&mut RepoView, &mut Context<RepoView>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.op, Some(OpStatus::Running { .. })) {
            return;
        }
        self.run_op(label, done, f, cx);
        self.op_then = Some(Box::new(then));
    }

    /// Stop the running operation (git is killed; the repository is left as git leaves it).
    pub fn cancel_op(&mut self) {
        if let Some(c) = &self.op_cancel {
            c.cancel();
        }
    }

    /// Run custom command `index` on the file at `path` (in commit `rev`, else the work
    /// tree) at the next render.
    fn queue_custom(
        &mut self,
        index: usize,
        rev: Option<Oid>,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) {
        let file = path.to_string_lossy().replace('\\', "/");
        self.pending_custom = Some((
            index,
            custom_ops::CustomOn {
                sha: rev.map(|r| r.to_string()),
                reference: rev.map(|r| r.to_string()),
                file: Some(file),
            },
        ));
        cx.notify();
    }

    /// The operation log.
    pub fn activity(&self) -> &Activity {
        &self.activity
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
            Scope::Compare(a, b) => format!("{} ⇄ {}", short_ref(a), short_ref(b)).into(),
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
                            if matches!(self.scope, Scope::Revs(_) | Scope::Compare(..)) {
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
        let dim = self.search.as_ref().is_some_and(|s| s.dims(&id));
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
            .when(dim, |d| d.opacity(0.35))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.selected = Some(i);
                window.focus(&this.focus, cx);
                this.sync_details(false, cx);
                cx.notify();
            }))
            .capture_any_mouse_down(cx.listener(
                move |this, ev: &gpui_kit::MouseDownEvent, window, cx| {
                    if ev.button == gpui_kit::MouseButton::Right {
                        window.focus(&this.focus, cx);
                        this.select_for_menu(i, cx);
                    }
                },
            ))
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
            let list = uniform_list(
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
            .size_full();
            let view = cx.entity().downgrade();
            div()
                .id("graph-menu")
                .flex_1()
                .min_h_0()
                .child(list)
                .context_menu(move |menu, window, cx| {
                    commit_menu::build_commit_menu(menu, &view, window, cx)
                })
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.surface)
            .children(self.render_search(p, cx))
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
    if tx.send(Msg::Lfs(repo.uses_lfs())).is_err() {
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let rem = window.rem_size();
        self.ensure_side_filter(window, cx);
        if let Some((index, on)) = self.pending_custom.take() {
            let view = cx.entity();
            window.defer(cx, move |window, cx| {
                view.update(cx, |v, cx| v.run_custom_command(index, on, window, cx))
            });
        }
        if std::mem::take(&mut self.refocus) {
            let mode = self.mode;
            self.set_mode(mode, window, cx);
        }
        div()
            .id("repo")
            .key_context("Graph")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            // Patch files dropped on the tab are offered for applying.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                let patches: Vec<_> = paths
                    .paths()
                    .iter()
                    .filter(|p| patch_ops::is_patch_file(p))
                    .cloned()
                    .collect();
                this.apply_patches_dialog(patches, window, cx);
            }))
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
            .on_action(cx.listener(|this, _: &actions::ShowAllBranches, _, cx| {
                this.set_scope(Scope::All, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::ShowCurrentBranch, _, cx| {
                this.set_scope(Scope::Head, cx)
            }))
            .on_action(
                cx.listener(|this, _: &actions::CopyCommitId, _, cx| this.copy_commit_id(cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::ShowLocalChanges, window, cx| {
                    this.set_mode(Mode::Changes, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &actions::ShowCommits, window, cx| {
                this.set_mode(Mode::Commits, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &actions::WriteCommitMessage, window, cx| {
                    this.set_mode(Mode::Changes, window, cx);
                    this.changes.update(cx, |c, cx| c.focus_commit(window, cx));
                }),
            )
            .on_action(
                cx.listener(|this, _: &actions::ToggleActivity, _, cx| this.toggle_activity(cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::ToggleAssistant, window, cx| {
                    this.toggle_helper(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &actions::ExplainCommit, window, cx| {
                if let Some(i) = this.selected {
                    let (id, summary) = (this.log.id(i), this.log.summary(i).to_owned());
                    this.explain_commit(id, summary, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &actions::ReviewBranch, window, cx| {
                let Some(branch) = this.refs.head_branch.clone() else {
                    return;
                };
                if let Some(base) = this.review_base(&branch) {
                    this.review_branch(branch, base, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &actions::NewBranch, window, cx| {
                this.new_branch_at_selection(window, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::NewTag, window, cx| {
                this.new_tag_at_selection(window, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::PushAllTags, _, cx| this.push_all_tags(cx)))
            .on_action(cx.listener(|this, _: &actions::AddRemote, window, cx| {
                this.add_remote_dialog(window, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::FindCommits, window, cx| {
                this.open_search(window, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::LfsFetch, _, cx| this.lfs_fetch(false, cx)))
            .on_action(
                cx.listener(|this, _: &actions::LfsFetchAll, _, cx| this.lfs_fetch(true, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::LfsPull, _, cx| this.lfs_pull(cx)))
            .on_action(cx.listener(|this, _: &actions::AddSubmodule, w, cx| {
                this.add_submodule_dialog(w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::AddWorktree, w, cx| {
                this.add_worktree_dialog(None, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &actions::PruneWorktrees, _, cx| this.prune_worktrees(cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::GitFlowSettings, w, cx| {
                    this.flow_init_dialog(w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &actions::StartFeature, w, cx| {
                this.flow_start_dialog(Kind::Feature, w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::StartRelease, w, cx| {
                this.flow_start_dialog(Kind::Release, w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::StartHotfix, w, cx| {
                this.flow_start_dialog(Kind::Hotfix, w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::FinishFlowBranch, w, cx| {
                this.flow_finish_current(w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::UpdateSubmodules, _, cx| {
                this.update_submodules(Vec::new(), true, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::LfsPrune, _, cx| this.lfs_prune(cx)))
            .on_action(cx.listener(|this, _: &actions::ApplyPatch, w, cx| this.pick_patches(w, cx)))
            .on_action(cx.listener(|this, _: &actions::OpenInTerminal, _, cx| {
                this.open_external(ExternalKind::Terminal, PathBuf::new(), None, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::OpenInEditor, _, cx| {
                this.open_external(ExternalKind::Editor, PathBuf::new(), None, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::ShowInFileManager, _, cx| {
                this.open_external(ExternalKind::FileManager, PathBuf::new(), None, cx)
            }))
            .on_action(
                cx.listener(|this, _: &actions::SigningSettings, w, cx| {
                    this.signing_settings(w, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &actions::LfsRefreshLocks, _, cx| this.load_lfs_locks(cx)),
            )
            .on_action(cx.listener(|this, _: &actions::ShowReflog, window, cx| {
                this.open_reflog("HEAD".into(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::BrowseFiles, window, cx| {
                this.browse_selected(window, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::Undo, window, cx| this.undo(window, cx)))
            .on_action(cx.listener(|this, _: &actions::StageAll, _, cx| {
                this.changes.update(cx, |c, cx| c.stage_all(cx))
            }))
            .on_action(cx.listener(|this, _: &actions::UnstageAll, _, cx| {
                this.changes.update(cx, |c, cx| c.unstage_all(cx))
            }))
            .when(self.sidebar, |d| d.child(self.render_sidebar(&p, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .children(self.render_banner(&p, window, cx))
                    .child(self.render_mode(&p, rem, cx))
                    .when(self.activity_open, |d| {
                        d.child(self.render_activity(&p, cx))
                    }),
            )
            .when_some(self.helper.as_ref(), |d, (helper, _)| {
                d.child(
                    div()
                        .flex_none()
                        .w(rpx(420.))
                        .h_full()
                        .border_l_1()
                        .border_color(p.bd)
                        .child(helper.clone()),
                )
            })
    }
}

impl RepoView {
    fn render_mode(
        &self,
        p: &Palette,
        rem: gpui_kit::Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .when_some(self.overlay.as_ref(), |d, (view, _)| {
                d.child(div().flex_1().min_w_0().h_full().child(view.clone()))
            })
            .when(self.overlay.is_none() && self.mode == Mode::Changes, |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(self.changes.clone()),
                )
            })
            .when_some(
                self.rebase
                    .as_ref()
                    .filter(|_| self.mode == Mode::Commits && self.overlay.is_none()),
                |d, (editor, _)| d.child(div().flex_1().min_w_0().h_full().child(editor.clone())),
            )
            .when(
                self.mode == Mode::Commits && self.rebase.is_none() && self.overlay.is_none(),
                |d| {
                    d.child(
                        div().flex_1().min_w_0().h_full().child(
                            v_resizable("graph-details")
                                .with_state(&self.details_split)
                                .child(
                                    resizable_panel()
                                        .size_range(rem * (120. / 13.)..gpui_kit::Pixels::MAX)
                                        .child(self.render_graph(p, cx)),
                                )
                                .child(
                                    resizable_panel()
                                        .size(rem * (400. / 13.))
                                        .size_range(rem * (140. / 13.)..gpui_kit::Pixels::MAX)
                                        .child(self.details.clone()),
                                ),
                        ),
                    )
                },
            )
            .into_any_element()
    }

    /// The bar shown while a merge, rebase, cherry-pick, revert or `am` is stopped:
    /// conflicts left, Continue, Skip and Abort.
    fn render_banner(
        &self,
        p: &Palette,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.state.command()?;
        let conflicts = self.changes.read(cx).conflicts();
        let running = matches!(self.op, Some(OpStatus::Running { .. }));
        let can_skip = !matches!(self.state, RepoState::Merge);
        let what = match conflicts {
            0 => "No conflicts left: continue when ready".to_owned(),
            1 => "1 conflicted file".to_owned(),
            n => format!("{n} conflicted files"),
        };
        let label = self.state.label();
        let button = |id: &'static str, text: &'static str, kind: ui::Kind, enabled: bool| {
            ui::button(id, text, kind, p)
                .h(rpx(22.))
                .when(!enabled, |d| d.opacity(0.5))
                .when(enabled, |d| d.cursor_pointer())
        };
        Some(
            div()
                .flex_none()
                .h(rpx(36.))
                .px(rpx(12.))
                .flex()
                .items_center()
                .gap(rpx(8.))
                .bg(p.modified.opacity(0.12))
                .border_b_1()
                .border_color(p.modified.opacity(0.4))
                .child(ui::chip(label, p.modified))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(ts::BODY)
                        .text_color(if conflicts > 0 { p.fg } else { p.added })
                        .child(what),
                )
                .when(conflicts > 0 && self.mode == Mode::Commits, |d| {
                    d.child(
                        button("op-resolve", "Resolve Conflicts", ui::Kind::Secondary, true)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_mode(Mode::Changes, window, cx)
                            })),
                    )
                })
                .child(
                    button("op-abort", "Abort…", ui::Kind::Secondary, !running).on_click(
                        cx.listener(move |this, _, window, cx| this.confirm_abort(window, cx)),
                    ),
                )
                .when(can_skip, |d| {
                    d.child(
                        button("op-skip", "Skip", ui::Kind::Secondary, !running).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.run_op("Skipping", "Skipped", |r, _, _| r.skip_operation(), cx)
                            }),
                        ),
                    )
                })
                .child(
                    button(
                        "op-continue",
                        "Continue",
                        ui::Kind::Primary,
                        conflicts == 0 && !running,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.changes.read(cx).conflicts() == 0 {
                            this.run_op(
                                "Continuing",
                                "Continued",
                                |r, _, _| r.continue_operation(),
                                cx,
                            );
                        }
                    })),
                )
                .into_any_element(),
        )
    }

    fn confirm_abort(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::button::ButtonVariant;
        if matches!(self.op, Some(OpStatus::Running { .. })) {
            return;
        }
        let label = self.state.label().to_lowercase();
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title(format!("Stop {label}?"))
                .description(
                    "The branch goes back to where it was before, and conflict resolutions \
                     made so far are lost.",
                )
                .ok_text("Abort")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let _ = this.update(cx, |v, cx| {
                        v.run_op("Aborting", "Aborted", |r, _, _| r.abort_operation(), cx)
                    });
                    true
                })
        });
    }
}

/// A ref label chip (graph rows, commit details).
pub(crate) fn label_chip(l: &Label, p: &Palette) -> AnyElement {
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

/// A commit date for the list: "14:02" today, "Oct 9 14:02" this year, "Oct 9 2023" before.
pub(crate) fn fmt_date(secs: i64, now: i64) -> SharedString {
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
    }
}
