//! The main window: title bar with repository tabs, the toolbar, the 48px rail, the
//! center (repository manager or a repository) and the status bar.
//!
//! Tab 0 is always the repository manager ([`crate::manager`]); every other tab is a
//! [`RepoView`]. Open tabs, recent repositories, the theme and zoom are saved in
//! [`crate::store::State`].

use std::path::{Path, PathBuf};

use gpui_kit::component::TitleBar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px, svg,
};
use switchhead_git::Repo;
use switchhead_git::discover::{self, Target};

use crate::actions;
use crate::appearance::{self, rpx, ts};
use crate::diff_view::set_prefs;
use crate::manager::ManagerState;
use crate::palette::{Chip, PaletteEvent, PaletteView, RepoItem, Sources};
use crate::repo_view::{OpStatus, RepoView};
use crate::store::{self, DiffPrefs, Paths, State};
use crate::theme::{self, MONO, Palette, SANS, ThemeId, palette};
use crate::ui;

/// A tab.
pub enum Tab {
    /// The repository manager (always tab 0).
    Manager,
    /// An open repository.
    Repo(Entity<RepoView>),
}

/// The window's root view.
pub struct Workspace {
    pub(crate) paths: Paths,
    pub(crate) state: State,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    pub(crate) manager: ManagerState,
    focus: FocusHandle,
    settings_open: bool,
    /// Move keyboard focus to the active tab on the next frame (it just changed).
    focus_tab: bool,
    /// A message for the status bar (an open that failed).
    pub(crate) notice: Option<SharedString>,
    _save: Option<Task<()>>,
    _omarchy: Option<Task<()>>,
    _diff_prefs: gpui_kit::Subscription,
    _messages: gpui_kit::Subscription,
    /// Quick Launch, while open.
    palette: Option<(Entity<PaletteView>, gpui_kit::Subscription)>,
    _activation: gpui_kit::Subscription,
}

impl Workspace {
    /// The window's workspace. Reopens the saved tabs, then `open` (a path from the
    /// command line).
    pub fn new(
        paths: Paths,
        state: State,
        open: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let omarchy = crate::omarchy::supported().then(|| {
            cx.spawn_in(window, async move |this, cx| {
                let mut last = None;
                loop {
                    let colors = cx
                        .background_executor()
                        .spawn(async { crate::omarchy::load() })
                        .await;
                    if colors != last {
                        last = colors.clone();
                        let alive = this.update_in(cx, |_, window, cx| {
                            match crate::omarchy::set(colors, cx) {
                                Some(p) if palette(cx).id == ThemeId::Omarchy => {
                                    theme::apply(p, Some(window), cx)
                                }
                                _ => cx.notify(),
                            }
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                    cx.background_executor().timer(crate::omarchy::POLL).await;
                }
            })
        });
        // Diff settings are changed from any tab; save them here.
        let diff_prefs = cx.observe_global::<crate::store::DiffPrefs>(|this, cx| {
            let prefs = *cx.global::<crate::store::DiffPrefs>();
            if this.state.diff != prefs {
                this.state.diff = prefs;
                this.save(cx);
            }
        });
        let messages = cx.observe_global::<store::RecentMessages>(|this, cx| {
            let m = cx.global::<store::RecentMessages>();
            if this.state.messages != *m {
                this.state.messages = m.clone();
                this.save(cx);
            }
        });
        // Coming back to the window: pick up what changed in a terminal meanwhile.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active()
                && let Some(v) = this.active_repo()
            {
                v.update(cx, |v, cx| v.activated(cx));
            }
        });
        let reopen = state.open_tabs.clone();
        let mut ws = Self {
            manager: ManagerState::new(window, cx),
            paths,
            state,
            tabs: vec![Tab::Manager],
            active: 0,
            focus: cx.focus_handle(),
            settings_open: false,
            focus_tab: false,
            notice: None,
            _save: None,
            _omarchy: omarchy,
            _diff_prefs: diff_prefs,
            _messages: messages,
            palette: None,
            _activation: activation,
        };
        for path in reopen {
            ws.open_path(path, false, cx);
        }
        if let Some(path) = open {
            ws.open_path(path, true, cx);
        }
        window.focus(&ws.focus, cx);
        ws
    }

    // ---------------------------------------------------------------- tabs

    /// Open `path`: a repository becomes a tab (or activates its tab); a plain folder is
    /// scanned and its repositories listed in the manager. `activate`: switch to it.
    pub fn open_path(&mut self, path: PathBuf, activate: bool, cx: &mut Context<Self>) {
        self.notice = None;
        let task = cx.background_executor().spawn(async move {
            match discover::classify(&path) {
                Target::Repo(root) => (path, Repo::open(&root).map(Some)),
                Target::Folder(dir) => (dir, Ok(None)),
            }
        });
        cx.spawn(async move |this, cx| {
            let (path, opened) = task.await;
            let _ = this.update(cx, |ws, cx| {
                match opened {
                    Ok(Some(repo)) => ws.add_repo_tab(repo, activate, cx),
                    Ok(None) => {
                        // Not a repository: list the repositories under it.
                        ws.active = 0;
                        ws.manager.start_scan(path, cx);
                    }
                    Err(e) => {
                        ws.notice = Some(format!("Could not open {}: {e}", path.display()).into());
                        ws.state.open_tabs.retain(|p| p != &path);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_repo_tab(&mut self, repo: Repo, activate: bool, cx: &mut Context<Self>) {
        let root = repo.root().to_path_buf();
        let existing = self.tabs.iter().position(|t| match t {
            Tab::Repo(v) => v.read(cx).repo.root() == root,
            Tab::Manager => false,
        });
        let ix = match existing {
            Some(ix) => ix,
            None => {
                let name = repo.name().to_owned();
                let view = cx.new(|cx| RepoView::new(repo, cx));
                self.tabs.push(Tab::Repo(view));
                self.state.touch(&root, &name, store::now());
                self.tabs.len() - 1
            }
        };
        if activate || self.active == 0 {
            self.activate(ix, cx);
        }
        self.save_tabs(cx);
    }

    /// Switch to tab `ix`.
    pub fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.tabs.len() {
            self.active = ix;
            self.settings_open = false;
            self.focus_tab = true;
            cx.notify();
        }
    }

    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix == 0 || ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.active >= ix {
            self.active = self
                .active
                .saturating_sub(1)
                .max(usize::from(self.tabs.len() > 1));
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.focus_tab = true;
        self.save_tabs(cx);
        cx.notify();
    }

    fn cycle_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        let next = (self.active as isize + delta).rem_euclid(n) as usize;
        self.activate(next, cx);
    }

    pub(crate) fn active_repo(&self) -> Option<&Entity<RepoView>> {
        match self.tabs.get(self.active) {
            Some(Tab::Repo(v)) => Some(v),
            _ => None,
        }
    }

    // ---------------------------------------------------------------- quick launch

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let mut repos: Vec<RepoItem> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| match t {
                Tab::Repo(v) => {
                    let r = &v.read(cx).repo;
                    Some(RepoItem {
                        name: r.name().to_owned(),
                        path: r.root().to_path_buf(),
                        tab: Some(i),
                    })
                }
                Tab::Manager => None,
            })
            .collect();
        for r in &self.state.recent {
            if !repos.iter().any(|o| o.path == r.path) {
                repos.push(RepoItem {
                    name: r.name.clone(),
                    path: r.path.clone(),
                    tab: None,
                });
            }
        }
        let sources = Sources {
            has_repo: self.active_repo().is_some(),
            repos,
            refs: self
                .active_repo()
                .map(|v| v.read(cx).palette_refs())
                .unwrap_or_default(),
        };
        let view = cx.new(|cx| PaletteView::new(Chip::All, sources, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, ev: &PaletteEvent, window, cx| {
            this.close_palette(window, cx);
            match ev {
                PaletteEvent::Run(action) => window.dispatch_action(action.boxed_clone(), cx),
                PaletteEvent::SwitchTab(i) => this.activate(*i, cx),
                PaletteEvent::Open(path) => this.open_path(path.clone(), true, cx),
                PaletteEvent::Reveal(id) => {
                    if let Some(v) = this.active_repo() {
                        v.update(cx, |v, cx| v.reveal(*id, cx));
                    }
                }
                PaletteEvent::Dismiss => {}
            }
        });
        self.palette = Some((view, sub));
        cx.notify();
    }

    /// Close Quick Launch and give focus back to the active tab.
    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            match self.active_repo() {
                Some(v) => window.focus(&v.focus_handle(cx), cx),
                None => window.focus(&self.focus, cx),
            }
            cx.notify();
        }
    }

    // ---------------------------------------------------------------- saving

    fn save_tabs(&mut self, cx: &mut Context<Self>) {
        self.state.open_tabs = self
            .tabs
            .iter()
            .filter_map(|t| match t {
                Tab::Repo(v) => Some(v.read(cx).repo.root().to_path_buf()),
                Tab::Manager => None,
            })
            .collect();
        self.save(cx);
    }

    /// Write the state file in the background.
    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let file = self.paths.state_file();
        self._save = Some(cx.background_executor().spawn(async move {
            if let Err(e) = state.save(&file) {
                tracing::warn!(error = %e, path = %file.display(), "could not save the state file");
            }
        }));
    }

    // ---------------------------------------------------------------- appearance

    fn set_theme(&mut self, id: ThemeId, window: &mut Window, cx: &mut Context<Self>) {
        theme::apply(theme::resolve(id, cx), Some(window), cx);
        self.state.theme = Some(id.key().to_owned());
        self.save(cx);
        cx.notify();
    }

    fn set_zoom(&mut self, z: f32, window: &mut Window, cx: &mut Context<Self>) {
        let mut s = appearance::current(cx);
        s.zoom = appearance::clamp_zoom(z);
        appearance::set(s.clone(), cx);
        theme::apply_fonts(Some(window), cx);
        self.state.appearance = s;
        self.save(cx);
        cx.notify();
    }

    // ---------------------------------------------------------------- toolbar

    fn repo_op(&mut self, op: ToolbarOp, cx: &mut Context<Self>) {
        let Some(view) = self.active_repo().cloned() else {
            return;
        };
        view.update(cx, |v, cx| match op {
            ToolbarOp::Fetch => v.run_op(
                "Fetching",
                "Fetched",
                |r, c, f| r.fetch(None, true, c, |l| f(l)),
                cx,
            ),
            ToolbarOp::Pull => v.run_op(
                "Pulling",
                "Pulled",
                |r, c, f| r.pull(Default::default(), c, |l| f(l)),
                cx,
            ),
            ToolbarOp::Push => v.run_op(
                "Pushing",
                "Pushed",
                |r, c, f| r.push(&Default::default(), c, |l| f(l)),
                cx,
            ),
            ToolbarOp::Stash => v.run_op(
                "Stashing",
                "Stashed local changes",
                |r, _, _| r.stash_push(&Default::default()),
                cx,
            ),
            ToolbarOp::Pop => v.run_op(
                "Popping the stash",
                "Applied and dropped stash@{0}",
                |r, _, _| r.stash_apply(0, true),
                cx,
            ),
            ToolbarOp::Refresh => v.reload(cx),
        });
    }

    fn render_toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let busy = self
            .active_repo()
            .is_some_and(|v| matches!(v.read(cx).op, Some(OpStatus::Running { .. })));
        let tool = |id: &'static str,
                    icon: &'static str,
                    label: &'static str,
                    tip: &'static str,
                    op: Option<ToolbarOp>,
                    cx: &mut Context<Self>| {
            let enabled = op.is_some() && !(busy && op != Some(ToolbarOp::Refresh));
            div()
                .id(id)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(rpx(2.))
                .w(rpx(52.))
                .h(rpx(42.))
                .rounded(px(6.))
                .text_size(ts::CAPTION)
                .text_color(if enabled { p.fg2 } else { p.fg3.opacity(0.6) })
                .when(enabled, |d| {
                    d.cursor_pointer().hover(|s| s.bg(p.hover).text_color(p.fg))
                })
                .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
                .when_some(op.filter(|_| enabled), |d, op| {
                    d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.repo_op(op, cx)))
                })
                .child(svg().path(icon).size(rpx(17.)).text_color(if enabled {
                    p.fg2
                } else {
                    p.fg3.opacity(0.6)
                }))
                .child(label)
        };
        div()
            .h(rpx(50.))
            .flex_none()
            .flex()
            .items_center()
            .gap(rpx(2.))
            .px(rpx(10.))
            .bg(p.panel)
            .border_b_1()
            .border_color(p.bd)
            .child(tool(
                "tool-fetch",
                "icons/sh-arrow-down-to-line.svg",
                "Fetch",
                "Fetch all remotes and prune",
                Some(ToolbarOp::Fetch),
                cx,
            ))
            .child(tool(
                "tool-pull",
                "icons/sh-arrow-down.svg",
                "Pull",
                "Pull the current branch",
                Some(ToolbarOp::Pull),
                cx,
            ))
            .child(tool(
                "tool-push",
                "icons/sh-arrow-up.svg",
                "Push",
                "Push the current branch",
                Some(ToolbarOp::Push),
                cx,
            ))
            .child(ui::vdivider(p, 26.).mx(rpx(6.)))
            .child(tool(
                "tool-stash",
                "icons/sh-archive.svg",
                "Stash",
                "Stash local changes",
                Some(ToolbarOp::Stash),
                cx,
            ))
            .child(tool(
                "tool-pop",
                "icons/sh-archive-restore.svg",
                "Pop",
                "Apply and drop the latest stash",
                Some(ToolbarOp::Pop),
                cx,
            ))
            .child(tool(
                "tool-branch",
                "icons/sh-git-branch.svg",
                "Branch",
                "New branch (coming with branch operations)",
                None,
                cx,
            ))
            .child(div().flex_1())
            .child(tool(
                "tool-refresh",
                "icons/sh-refresh-cw.svg",
                "Refresh",
                "Reload refs and history",
                Some(ToolbarOp::Refresh),
                cx,
            ))
            .child(tool(
                "tool-terminal",
                "icons/square-terminal.svg",
                "Terminal",
                "Open a terminal here (coming soon)",
                None,
                cx,
            ))
            .child(tool(
                "tool-assistant",
                "icons/bot.svg",
                "Assistant",
                "Coding-CLI assistant (coming with milestone 5)",
                None,
                cx,
            ))
            .into_any_element()
    }

    // ---------------------------------------------------------------- chrome

    fn render_title_bar(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        // The title bar row is the window's drag area: every control occludes it, or
        // Windows takes clicks on it as a window drag (see Switchyard's gotchas).
        let tabs = div()
            .id("tabs")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_end()
            .overflow_x_scroll()
            .children(self.tabs.iter().enumerate().map(|(i, tab)| {
                let active = i == self.active;
                let (label, detail): (SharedString, Option<SharedString>) = match tab {
                    Tab::Manager => ("Repositories".into(), None),
                    Tab::Repo(v) => {
                        let v = v.read(cx);
                        (v.title(), Some(v.head_label()))
                    }
                };
                div()
                    .id(("tab", i))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(rpx(7.))
                    .h(rpx(30.))
                    .pl(rpx(12.))
                    .pr(rpx(if i == 0 { 12. } else { 6. }))
                    .max_w(rpx(260.))
                    .rounded_t(px(6.))
                    .occlude()
                    .cursor_pointer()
                    .text_size(ts::UI)
                    .text_color(if active { p.fg } else { p.fg2 })
                    .when(active, |d| {
                        d.bg(p.surface).border_1().border_b_0().border_color(p.bd)
                    })
                    .when(!active, |d| d.hover(|s| s.bg(p.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.activate(i, cx)))
                    .child(
                        svg()
                            .path(if i == 0 {
                                "icons/sh-folder-git-2.svg"
                            } else {
                                "icons/sh-git-branch.svg"
                            })
                            .size(rpx(13.))
                            .text_color(if active { p.acc } else { p.fg3 }),
                    )
                    .child(
                        div()
                            .truncate()
                            .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                            .child(label),
                    )
                    .when_some(detail, |d, detail| {
                        d.child(
                            div()
                                .flex_none()
                                .max_w(rpx(110.))
                                .truncate()
                                .font_family(MONO)
                                .text_size(ts::CAPTION)
                                .text_color(p.fg3)
                                .child(detail),
                        )
                    })
                    .when(i > 0, |d| {
                        d.child(
                            div()
                                .id(("tab-close", i))
                                .flex_none()
                                .px(rpx(4.))
                                .rounded(px(4.))
                                .text_color(p.fg3)
                                .text_size(ts::SMALL)
                                .hover(|s| s.bg(p.hover).text_color(p.fg))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(i, cx);
                                }))
                                .child("×"),
                        )
                    })
            }))
            .child(
                div()
                    .id("tab-open")
                    .flex_none()
                    .h(rpx(30.))
                    .px(rpx(10.))
                    .flex()
                    .items_center()
                    .occlude()
                    .cursor_pointer()
                    .text_color(p.fg3)
                    .font_family(MONO)
                    .text_size(ts::BASE)
                    .hover(|s| s.text_color(p.fg))
                    .tooltip(|w, cx| {
                        Tooltip::new("Open a repository or folder")
                            .action(&actions::OpenRepository, None)
                            .build(w, cx)
                    })
                    .on_click(cx.listener(|this, _, w, cx| this.prompt_open(w, cx)))
                    .child("+"),
            );
        TitleBar::new()
            .h(rpx(38.))
            .bg(p.panel)
            .border_color(p.bd)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .h_full()
                    .items_end()
                    .gap(rpx(10.))
                    .pr(rpx(10.))
                    .font_family(SANS)
                    .child(
                        div()
                            .flex_none()
                            .h_full()
                            .flex()
                            .items_center()
                            .gap(rpx(7.))
                            .px(rpx(6.))
                            .child(
                                svg()
                                    .path("icons/sh-git-fork.svg")
                                    .size(rpx(15.))
                                    .text_color(p.acc),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(ts::UI)
                                    .child("switchhead"),
                            ),
                    )
                    .child(tabs),
            )
            .into_any_element()
    }

    fn render_rail(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let repo_sidebar = self.active_repo().map(|v| v.read(cx).sidebar);
        let item = |id: &'static str,
                    icon: &'static str,
                    tip: &'static str,
                    on: bool,
                    enabled: bool,
                    rail: RailItem,
                    cx: &mut Context<Self>| {
            div()
                .id(id)
                .relative()
                .size(rpx(36.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .when(on, |d| d.bg(p.hover))
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(p.hover)))
                .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
                .when(enabled, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| this.rail_click(rail, cx)))
                })
                .child(
                    div()
                        .absolute()
                        .left(rpx(-6.))
                        .top(rpx(9.))
                        .bottom(rpx(9.))
                        .w(rpx(2.))
                        .rounded(px(2.))
                        .when(on, |d| d.bg(p.fg)),
                )
                .child(svg().path(icon).size(rpx(18.)).text_color(if on {
                    p.fg
                } else if enabled {
                    p.fg3
                } else {
                    p.fg3.opacity(0.45)
                }))
        };
        div()
            .w(rpx(48.))
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap(rpx(4.))
            .py(rpx(8.))
            .bg(p.panel)
            .border_r_1()
            .border_color(p.bd)
            .child(item(
                "rail-repos",
                "icons/sh-folder-git-2.svg",
                "Repositories",
                self.active == 0,
                true,
                RailItem::Repos,
                cx,
            ))
            .child(item(
                "rail-branches",
                "icons/sh-git-branch.svg",
                "Branches, remotes, tags and stashes",
                repo_sidebar == Some(true),
                repo_sidebar.is_some(),
                RailItem::Branches,
                cx,
            ))
            .child(item(
                "rail-search",
                "icons/search.svg",
                "Search commits (coming soon)",
                false,
                false,
                RailItem::Search,
                cx,
            ))
            .child(item(
                "rail-assistant",
                "icons/bot.svg",
                "Assistant (coming with milestone 5)",
                false,
                false,
                RailItem::Assistant,
                cx,
            ))
            .child(div().flex_1())
            .child(item(
                "rail-settings",
                "icons/sh-sliders-horizontal.svg",
                "Appearance",
                self.settings_open,
                true,
                RailItem::Settings,
                cx,
            ))
            .into_any_element()
    }

    fn rail_click(&mut self, item: RailItem, cx: &mut Context<Self>) {
        match item {
            RailItem::Repos => self.activate(0, cx),
            RailItem::Branches => {
                if let Some(v) = self.active_repo().cloned() {
                    v.update(cx, |v, cx| {
                        v.sidebar = !v.sidebar;
                        cx.notify();
                    });
                }
            }
            RailItem::Settings => {
                self.settings_open = !self.settings_open;
                cx.notify();
            }
            RailItem::Search | RailItem::Assistant => {}
        }
    }

    fn render_settings(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let current = palette(cx).id;
        let zoom = appearance::zoom(cx);
        let mut themes: Vec<ThemeId> = ThemeId::ALL.to_vec();
        if crate::omarchy::supported() {
            themes.push(ThemeId::Omarchy);
        }
        div()
            .id("settings")
            .absolute()
            .left(rpx(52.))
            .bottom(rpx(30.))
            .w(rpx(260.))
            .occlude()
            .flex()
            .flex_col()
            .gap(rpx(2.))
            .p(rpx(8.))
            .bg(p.elev)
            .border_1()
            .border_color(p.bd2)
            .rounded(px(10.))
            .shadow(ui::shadow(p))
            .child(ui::caption("THEME", p).px(rpx(6.)).py(rpx(4.)))
            .children(themes.into_iter().map(|id| {
                let on = id == current;
                div()
                    .id(SharedString::from(format!("theme-{}", id.key())))
                    .flex()
                    .items_center()
                    .gap(rpx(8.))
                    .h(rpx(26.))
                    .px(rpx(6.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .text_size(ts::UI)
                    .when(on, |d| d.bg(p.sel))
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |this, _, w, cx| this.set_theme(id, w, cx)))
                    .child(theme_swatch(id, cx))
                    .child(div().flex_1().child(id.label()))
                    .when(on, |d| d.child(div().text_color(p.acc).child("✓")))
            }))
            .child(div().h(rpx(1.)).my(rpx(6.)).bg(p.bd))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(6.))
                    .px(rpx(6.))
                    .child(ui::caption("ZOOM", p).flex_1())
                    .child(
                        ui::button("zoom-out", "−", ui::Kind::Secondary, p).on_click(cx.listener(
                            move |this, _, w, cx| this.set_zoom(appearance::zoom_out(zoom), w, cx),
                        )),
                    )
                    .child(
                        div()
                            .w(rpx(44.))
                            .text_center()
                            .font_family(MONO)
                            .text_size(ts::BODY)
                            .child(format!("{}%", appearance::percent(zoom))),
                    )
                    .child(ui::button("zoom-in", "+", ui::Kind::Secondary, p).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.set_zoom(appearance::zoom_in(zoom), w, cx)
                        }),
                    )),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let mut left: Vec<AnyElement> = Vec::new();
        let mut right: Vec<AnyElement> = Vec::new();
        if let Some(v) = self.active_repo() {
            let v = v.read(cx);
            left.push(
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(rpx(6.))
                    .px(rpx(10.))
                    .bg(p.hover)
                    .text_color(p.fg)
                    .font_family(MONO)
                    .text_size(ts::CAPTION_PLUS)
                    .child(
                        svg()
                            .path("icons/sh-git-branch.svg")
                            .size(rpx(12.))
                            .text_color(p.acc),
                    )
                    .child(v.head_label())
                    .into_any_element(),
            );
            if v.state != switchhead_git::RepoState::Clean {
                left.push(ui::chip(v.state.label(), p.modified).into_any_element());
            }
            match &v.op {
                Some(OpStatus::Running { label, progress }) => left.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(6.))
                        .min_w_0()
                        .child(ui::pulse_dot("op", p.acc, 6.))
                        .child(label.clone())
                        .child(div().truncate().text_color(p.fg3).child(progress.clone()))
                        .into_any_element(),
                ),
                Some(OpStatus::Done(m)) => left.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(5.))
                        .text_color(p.added)
                        .child(ui::dot(p.added, 6.))
                        .child(m.clone())
                        .into_any_element(),
                ),
                Some(OpStatus::Failed(m)) => left.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(5.))
                        .min_w_0()
                        .text_color(p.deleted)
                        .child(ui::dot(p.deleted, 6.))
                        .child(div().truncate().child(m.clone()))
                        .into_any_element(),
                ),
                None => {}
            }
            let count = ui::thousands(v.commit_count() as u64);
            right.push(
                div()
                    .font_family(MONO)
                    .child(if v.loading {
                        format!("{count} commits…")
                    } else {
                        format!("{count} commits")
                    })
                    .into_any_element(),
            );
            if let Some(s) = v.stats {
                right.push(
                    div()
                        .font_family(MONO)
                        .text_color(p.fg3)
                        .child(format!("first rows {} ms", s.first_batch_ms))
                        .into_any_element(),
                );
            }
            right.push(
                div()
                    .text_color(p.fg3)
                    .truncate()
                    .max_w(rpx(320.))
                    .child(SharedString::from(v.repo.root().display().to_string()))
                    .into_any_element(),
            );
        } else {
            left.push(
                div()
                    .pl(rpx(10.))
                    .text_color(p.fg2)
                    .child(format!("{} recent repositories", self.state.recent.len()))
                    .into_any_element(),
            );
        }
        if let Some(n) = &self.notice {
            left.push(
                div()
                    .text_color(p.deleted)
                    .truncate()
                    .child(n.clone())
                    .into_any_element(),
            );
        }
        div()
            .h(rpx(24.))
            .flex_none()
            .flex()
            .items_center()
            .gap(rpx(14.))
            .border_t_1()
            .border_color(p.bd)
            .bg(p.panel)
            .text_size(ts::LABEL)
            .text_color(p.fg2)
            .whitespace_nowrap()
            .overflow_hidden()
            .children(left)
            .child(div().flex_1())
            .children(right)
            .child(div().w(rpx(4.)))
            .into_any_element()
    }
}

/// A small preview of a theme: its background with the accent.
fn theme_swatch(id: ThemeId, cx: &gpui_kit::App) -> AnyElement {
    let t = theme::resolve(id, cx);
    div()
        .flex_none()
        .size(rpx(16.))
        .rounded(px(4.))
        .border_1()
        .border_color(t.bd2)
        .bg(t.bg)
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(rpx(6.)).rounded_full().bg(t.acc))
        .into_any_element()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarOp {
    Fetch,
    Pull,
    Push,
    Stash,
    Pop,
    Refresh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RailItem {
    Repos,
    Branches,
    Search,
    Assistant,
    Settings,
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        if std::mem::take(&mut self.focus_tab) {
            match self.active_repo() {
                Some(v) => window.focus(&v.focus_handle(cx), cx),
                None => window.focus(&self.focus, cx),
            }
        }
        let center: AnyElement = match self.tabs.get(self.active) {
            Some(Tab::Repo(v)) => div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(self.render_toolbar(&p, cx))
                .child(div().flex_1().min_h_0().flex().child(v.clone()))
                .into_any_element(),
            _ => self.render_manager(&p, window, cx),
        };
        let settings = self.settings_open.then(|| self.render_settings(&p, cx));
        let title = self.render_title_bar(&p, cx);
        let rail = self.render_rail(&p, cx);
        let status = self.render_status_bar(&p, cx);
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(p.bg)
            .text_color(p.fg)
            .font_family(SANS)
            .text_size(appearance::ui_font_size(cx))
            .on_action(
                cx.listener(|this, _: &actions::OpenRepository, w, cx| this.prompt_open(w, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::CloneRepository, w, cx| {
                this.activate(0, cx);
                this.manager.show_clone(w, cx);
            }))
            .on_action(cx.listener(|this, _: &actions::ShowManager, _, cx| this.activate(0, cx)))
            .on_action(
                cx.listener(|this, _: &actions::CloseTab, _, cx| this.close_tab(this.active, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::NextTab, _, cx| this.cycle_tab(1, cx)))
            .on_action(cx.listener(|this, _: &actions::PreviousTab, _, cx| this.cycle_tab(-1, cx)))
            .on_action(
                cx.listener(|this, _: &actions::Refresh, _, cx| {
                    this.repo_op(ToolbarOp::Refresh, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &actions::Fetch, _, cx| this.repo_op(ToolbarOp::Fetch, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::Pull, _, cx| this.repo_op(ToolbarOp::Pull, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::Push, _, cx| this.repo_op(ToolbarOp::Push, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::Stash, _, cx| this.repo_op(ToolbarOp::Stash, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::PopStash, _, cx| this.repo_op(ToolbarOp::Pop, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::ToggleSidebar, _, cx| {
                this.rail_click(RailItem::Branches, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::ZoomIn, w, cx| {
                this.set_zoom(appearance::zoom_in(appearance::zoom(cx)), w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::ZoomOut, w, cx| {
                this.set_zoom(appearance::zoom_out(appearance::zoom(cx)), w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::ZoomReset, w, cx| this.set_zoom(1.0, w, cx)))
            .on_action(
                cx.listener(|this, _: &actions::QuickLaunch, w, cx| this.open_palette(w, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::NewRepository, w, cx| {
                this.activate(0, cx);
                this.prompt_init(w, cx);
            }))
            .on_action(cx.listener(|this, _: &actions::ShowSettings, _, cx| {
                this.settings_open = true;
                cx.notify();
            }))
            .on_action(|_: &actions::ToggleDiffLayout, _, cx| {
                set_prefs(cx, |p| p.side_by_side = !p.side_by_side)
            })
            .on_action(|_: &actions::ToggleFileTree, _, cx| set_prefs(cx, |p| p.tree = !p.tree))
            .on_action(|_: &actions::CycleWhitespace, _, cx| {
                set_prefs(cx, |p| p.whitespace = p.whitespace.next())
            })
            .on_action(|_: &actions::MoreContext, _, cx| {
                set_prefs(cx, |p| {
                    p.context = (p.context + 1).min(DiffPrefs::MAX_CONTEXT)
                })
            })
            .on_action(|_: &actions::LessContext, _, cx| {
                set_prefs(cx, |p| p.context = p.context.saturating_sub(1))
            })
            .on_action(cx.listener(|this, _: &actions::Dismiss, w, cx| {
                this.close_palette(w, cx);
                this.settings_open = false;
                this.manager.dismiss();
                cx.notify();
            }))
            .child(title)
            .child(div().flex_1().min_h_0().flex().child(rail).child(center))
            .child(status)
            .children(settings)
            .when_some(self.palette.as_ref(), |d, (view, _)| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .justify_center()
                        .items_start()
                        .pt(rpx(72.))
                        .child(
                            div()
                                .on_mouse_down_out(
                                    cx.listener(|this, _, w, cx| this.close_palette(w, cx)),
                                )
                                .child(view.clone()),
                        ),
                )
            })
    }
}

impl gpui_kit::Focusable for Workspace {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Whether `path` is the root of one of the open tabs.
pub(crate) fn is_open(ws: &Workspace, path: &Path, cx: &gpui_kit::App) -> bool {
    ws.tabs.iter().any(|t| match t {
        Tab::Repo(v) => v.read(cx).repo.root() == path,
        Tab::Manager => false,
    })
}
