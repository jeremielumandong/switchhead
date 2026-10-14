//! The repository manager (tab 0): recent repositories, Open (a repository becomes a
//! tab; a plain folder is scanned and its repositories listed by subfolder), Clone and
//! New repository.

use std::path::PathBuf;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window, div, px, svg, uniform_list,
};
use switchhead_git::cli::Cancel;
use switchhead_git::discover::{self, Found};
use tokio::sync::mpsc::unbounded_channel;

use crate::appearance::{rpx, ts};
use crate::theme::{MONO, Palette, palette};
use crate::ui;
use crate::workspace::{Workspace, is_open};

/// A folder's repositories.
pub struct FolderScan {
    /// The folder.
    pub root: PathBuf,
    /// Repositories found, by relative path.
    pub repos: Vec<Found>,
    /// Still scanning.
    pub scanning: bool,
}

/// The Clone form.
pub struct CloneForm {
    url: Entity<InputState>,
    dest: Entity<InputState>,
    running: bool,
    progress: SharedString,
    error: Option<SharedString>,
    cancel: Cancel,
    _task: Option<Task<()>>,
}

/// The manager's own state (the recent list lives in [`crate::store::State`]).
pub struct ManagerState {
    /// The last folder opened that is not a repository.
    pub scan: Option<FolderScan>,
    /// The Clone form, while open.
    pub clone: Option<CloneForm>,
    _scan: Option<Task<()>>,
}

impl ManagerState {
    /// Nothing scanned, no form.
    pub fn new(_window: &mut Window, _cx: &mut Context<Workspace>) -> Self {
        Self {
            scan: None,
            clone: None,
            _scan: None,
        }
    }

    /// Scan `root` for repositories in the background.
    pub fn start_scan(&mut self, root: PathBuf, cx: &mut Context<Workspace>) {
        self.scan = Some(FolderScan {
            root: root.clone(),
            repos: Vec::new(),
            scanning: true,
        });
        let task = cx
            .background_executor()
            .spawn(async move { discover::scan(&root, discover::DEFAULT_DEPTH) });
        self._scan = Some(cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(s) = &mut ws.manager.scan {
                    s.repos = found;
                    s.scanning = false;
                }
                cx.notify();
            });
        }));
    }

    /// Open the Clone form.
    pub fn show_clone(&mut self, window: &mut Window, cx: &mut Context<Workspace>) {
        if self.clone.is_some() {
            return;
        }
        let default_dest = directories::UserDirs::new()
            .map(|u| u.home_dir().join("src"))
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let url = cx
            .new(|cx| InputState::new(window, cx).placeholder("https://github.com/owner/repo.git"));
        let dest = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Parent folder")
                .default_value(default_dest)
        });
        self.clone = Some(CloneForm {
            url,
            dest,
            running: false,
            progress: SharedString::default(),
            error: None,
            cancel: Cancel::default(),
            _task: None,
        });
        cx.notify();
    }

    /// Close the Clone form (cancelling a clone in progress).
    pub fn dismiss(&mut self) {
        if let Some(f) = self.clone.take() {
            f.cancel.cancel();
        }
    }
}

/// The folder a URL clones into (`…/repo.git` → `repo`).
pub fn clone_dir_name(url: &str) -> Option<String> {
    let last = url
        .trim()
        .trim_end_matches('/')
        .rsplit(['/', ':', '\\'])
        .next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty()).then(|| name.to_owned())
}

/// Group found repositories by their first path component (`""`: directly in the folder).
pub fn group_by_subfolder(repos: &[Found]) -> Vec<(String, Vec<&Found>)> {
    let mut groups: Vec<(String, Vec<&Found>)> = Vec::new();
    for f in repos {
        let group = match f.rel.split_once('/') {
            Some((first, _)) => first.to_owned(),
            None => String::new(),
        };
        match groups.iter_mut().find(|(g, _)| *g == group) {
            Some((_, v)) => v.push(f),
            None => groups.push((group, vec![f])),
        }
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    groups
}

impl Workspace {
    /// Ask for a folder and open it (a repository, or a folder to scan).
    pub(crate) fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |ws, cx| {
                for p in paths {
                    ws.open_path(p, true, cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn prompt_init(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Create repository here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            let res = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move { switchhead_git::ops::init(&path, None) }
                })
                .await;
            let _ = this.update(cx, |ws, cx| match res {
                Ok(_) => ws.open_path(path, true, cx),
                Err(e) => {
                    ws.notice = Some(format!("Could not create a repository: {e}").into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn start_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.manager.clone else {
            return;
        };
        let url = form.url.read(cx).value().trim().to_owned();
        let parent = form.dest.read(cx).value().trim().to_owned();
        let Some(name) = clone_dir_name(&url) else {
            form.error = Some("Enter the repository's URL.".into());
            cx.notify();
            return;
        };
        if parent.is_empty() {
            form.error = Some("Choose the folder to clone into.".into());
            cx.notify();
            return;
        }
        let dest = PathBuf::from(parent).join(name);
        form.running = true;
        form.error = None;
        form.progress = "Starting…".into();
        let cancel = Cancel::default();
        form.cancel = cancel.clone();
        let (tx, mut rx) = unbounded_channel::<Result<String, Result<PathBuf, String>>>();
        let spawned = std::thread::Builder::new()
            .name("git-clone".into())
            .spawn(move || {
                let progress = tx.clone();
                let res = switchhead_git::ops::clone(&url, &dest, &cancel, |l| {
                    let _ = progress.send(Ok(l.to_owned()));
                });
                let _ = tx.send(Err(res
                    .map(|r| r.root().to_path_buf())
                    .map_err(|e| e.to_string())));
            });
        if let Err(e) = spawned {
            form.running = false;
            form.error = Some(format!("Could not start git: {e}").into());
            return;
        }
        form._task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(msg) = rx.recv().await {
                let end = msg.is_err();
                let ok = this.update(cx, |ws, cx| {
                    match msg {
                        Ok(line) => {
                            if let Some(f) = &mut ws.manager.clone {
                                let line = line.trim();
                                if !line.is_empty() {
                                    f.progress = line.to_owned().into();
                                }
                            }
                        }
                        Err(Ok(root)) => {
                            ws.manager.clone = None;
                            ws.open_path(root, true, cx);
                        }
                        Err(Err(e)) => {
                            if let Some(f) = &mut ws.manager.clone {
                                f.running = false;
                                f.error = Some(e.into());
                            }
                        }
                    }
                    cx.notify();
                });
                if ok.is_err() || end {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn browse_clone_dest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Clone into".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            let _ = this.update_in(cx, |ws, window, cx| {
                if let Some(f) = &ws.manager.clone {
                    f.dest.update(cx, |i, cx| {
                        i.set_value(path.display().to_string(), window, cx)
                    });
                }
            });
        })
        .detach();
    }

    fn render_clone_form(&self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.manager.clone.as_ref()?;
        let field = |label: &'static str, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap(rpx(4.))
                .child(ui::caption(label, p))
                .child(Input::new(input).text_size(ts::BODY))
        };
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(rpx(10.))
                .p(rpx(14.))
                .bg(p.panel)
                .border_1()
                .border_color(p.bd)
                .rounded(px(8.))
                .child(
                    div()
                        .text_size(ts::TITLE)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Clone a repository"),
                )
                .child(field("REPOSITORY URL", &f.url))
                .child(
                    div()
                        .flex()
                        .items_end()
                        .gap(rpx(8.))
                        .child(div().flex_1().child(field("INTO FOLDER", &f.dest)))
                        .child(
                            ui::button("clone-browse", "Browse…", ui::Kind::Secondary, p).on_click(
                                cx.listener(|this, _, w, cx| this.browse_clone_dest(w, cx)),
                            ),
                        ),
                )
                .when_some(f.error.clone(), |d, e| {
                    d.child(div().text_color(p.deleted).text_size(ts::BODY).child(e))
                })
                .when(f.running, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(rpx(8.))
                            .text_size(ts::BODY)
                            .text_color(p.fg2)
                            .child(ui::pulse_dot("clone-run", p.acc, 6.))
                            .child(div().truncate().font_family(MONO).child(f.progress.clone())),
                    )
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(rpx(8.))
                        .child(
                            ui::button("clone-cancel", "Cancel", ui::Kind::Ghost, p).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.manager.dismiss();
                                    cx.notify();
                                }),
                            ),
                        )
                        .when(!f.running, |d| {
                            d.child(
                                ui::button("clone-go", "Clone", ui::Kind::Primary, p).on_click(
                                    cx.listener(|this, _, w, cx| this.start_clone(w, cx)),
                                ),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_scan(&self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.manager.scan.as_ref()?;
        let groups = group_by_subfolder(&s.repos);
        let mut rows: Vec<AnyElement> = Vec::new();
        for (group, repos) in groups {
            if !group.is_empty() {
                rows.push(
                    div()
                        .pt(rpx(6.))
                        .pb(rpx(2.))
                        .text_size(ts::SMALL)
                        .text_color(p.fg3)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("{group}/"))
                        .into_any_element(),
                );
            }
            for f in repos {
                let path = f.path.clone();
                let open = is_open(self, &path, cx);
                rows.push(
                    div()
                        .id(SharedString::from(format!("scan-{}", f.rel)))
                        .flex()
                        .items_center()
                        .gap(rpx(8.))
                        .h(rpx(26.))
                        .px(rpx(8.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(p.hover))
                        .on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.open_path(path.clone(), true, cx)
                            }),
                        )
                        .child(
                            svg()
                                .path("icons/sh-git-branch.svg")
                                .size(rpx(13.))
                                .text_color(p.fg3),
                        )
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_size(ts::BODY)
                                .child(f.rel.clone()),
                        )
                        .when(f.linked, |d| d.child(ui::chip("worktree", p.fg3)))
                        .when(open, |d| d.child(ui::chip("open", p.acc)))
                        .into_any_element(),
                );
            }
        }
        let title = format!("Repositories in {}", s.root.display());
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(rpx(2.))
                .p(rpx(12.))
                .bg(p.panel)
                .border_1()
                .border_color(p.bd)
                .rounded(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(8.))
                        .pb(rpx(4.))
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_size(ts::TITLE)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(ts::SMALL)
                                .text_color(p.fg3)
                                .child(if s.scanning {
                                    "Scanning…".to_owned()
                                } else {
                                    format!("{} found", s.repos.len())
                                }),
                        )
                        .child(
                            div()
                                .id("scan-close")
                                .px(rpx(4.))
                                .rounded(px(4.))
                                .cursor_pointer()
                                .text_color(p.fg3)
                                .hover(|s| s.bg(p.hover).text_color(p.fg))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.manager.scan = None;
                                    cx.notify();
                                }))
                                .child("×"),
                        ),
                )
                .when(!s.scanning && s.repos.is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(ts::BODY)
                            .text_color(p.fg3)
                            .child("No repositories in this folder."),
                    )
                })
                .children(rows)
                .into_any_element(),
        )
    }

    /// The manager page.
    pub(crate) fn render_manager(
        &self,
        p: &Palette,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = self.state.recent.len();
        let recent: AnyElement = if count == 0 {
            div()
                .p(rpx(16.))
                .text_size(ts::BODY)
                .text_color(p.fg3)
                .child("Repositories you open show up here.")
                .into_any_element()
        } else {
            uniform_list(
                "recent",
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, _w, cx| {
                    let p = palette(cx);
                    range
                        .filter_map(|i| this.state.recent.get(i).cloned().map(|r| (i, r)))
                        .map(|(i, r)| {
                            let open = is_open(this, &r.path, cx);
                            let path = r.path.clone();
                            let forget = r.path.clone();
                            div()
                                .id(("recent", i))
                                .w_full()
                                .h(rpx(44.))
                                .flex()
                                .items_center()
                                .gap(rpx(10.))
                                .px(rpx(12.))
                                .rounded(px(6.))
                                .cursor_pointer()
                                .hover(|s| s.bg(p.hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_path(path.clone(), true, cx)
                                }))
                                .child(
                                    svg()
                                        .path("icons/sh-folder-git-2.svg")
                                        .size(rpx(18.))
                                        .text_color(if open { p.acc } else { p.fg3 }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(ts::UI)
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(r.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(ts::SMALL)
                                                .text_color(p.fg3)
                                                .child(r.path.display().to_string()),
                                        ),
                                )
                                .when(open, |d| d.child(ui::chip("open", p.acc)))
                                .child(
                                    div()
                                        .id(("recent-forget", i))
                                        .px(rpx(6.))
                                        .rounded(px(4.))
                                        .text_color(p.fg3)
                                        .hover(|s| s.bg(p.hover).text_color(p.fg))
                                        .tooltip(|w, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(
                                                "Remove from the list",
                                            )
                                            .build(w, cx)
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.state.forget(&forget);
                                            this.save(cx);
                                            cx.notify();
                                        }))
                                        .child("×"),
                                )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let actions = div()
            .flex()
            .gap(rpx(8.))
            .child(
                ui::button("mgr-open", "Open…", ui::Kind::Primary, p)
                    .on_click(cx.listener(|this, _, w, cx| this.prompt_open(w, cx))),
            )
            .child(
                ui::button("mgr-clone", "Clone…", ui::Kind::Secondary, p)
                    .on_click(cx.listener(|this, _, w, cx| this.manager.show_clone(w, cx))),
            )
            .child(
                ui::button("mgr-init", "New repository…", ui::Kind::Secondary, p)
                    .on_click(cx.listener(|this, _, w, cx| this.prompt_init(w, cx))),
            );
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .justify_center()
            .bg(p.surface)
            .child(
                div()
                    .w_full()
                    .max_w(rpx(760.))
                    .flex()
                    .flex_col()
                    .gap(rpx(14.))
                    .px(rpx(24.))
                    .pt(rpx(28.))
                    .pb(rpx(12.))
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap(rpx(12.))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap(rpx(4.))
                                    .child(
                                        div()
                                            .text_size(ts::DISPLAY_S)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Repositories"),
                                    )
                                    .child(div().text_size(ts::BODY).text_color(p.fg3).child(
                                        "Open a repository, or a folder to find every repository in it.",
                                    )),
                            )
                            .child(actions),
                    )
                    .children(self.render_clone_form(p, cx))
                    .children(self.render_scan(p, cx))
                    .child(ui::caption("RECENT", p))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .border_1()
                            .border_color(p.bd)
                            .rounded(px(8.))
                            .p(rpx(4.))
                            .child(recent),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_folder_names() {
        assert_eq!(
            clone_dir_name("https://github.com/o/repo.git").as_deref(),
            Some("repo")
        );
        assert_eq!(
            clone_dir_name("git@github.com:o/repo").as_deref(),
            Some("repo")
        );
        assert_eq!(
            clone_dir_name("https://host/o/repo/").as_deref(),
            Some("repo")
        );
        assert_eq!(clone_dir_name("../local.git").as_deref(), Some("local"));
        assert_eq!(clone_dir_name("  "), None);
    }

    #[test]
    fn groups_by_first_folder() {
        let f = |rel: &str| Found {
            path: PathBuf::from("/x").join(rel),
            rel: rel.into(),
            linked: false,
        };
        let repos = vec![f("top"), f("work/a"), f("work/b"), f("oss/c/d")];
        let g = group_by_subfolder(&repos);
        let names: Vec<(&str, usize)> = g.iter().map(|(n, v)| (n.as_str(), v.len())).collect();
        assert_eq!(names, [("", 1), ("oss", 1), ("work", 2)]);
    }
}
