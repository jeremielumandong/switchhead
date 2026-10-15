//! Patches in a repository tab: save commits as patch files or copy one to the clipboard
//! (the graph's commit menu), and apply patch files as commits or to the work tree (the
//! palette, the Commands menu, or dropping files on the tab). Applying first shows what
//! the patches change and whether they apply cleanly.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClipboardItem, Context, ParentElement as _, PathPromptOptions, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use switchhead_git::Oid;
use switchhead_git::patch::{ApplyMode, PatchInfo};

use super::{OpStatus, RepoView};
use crate::appearance::{rpx, ts};
use crate::theme::palette;
use crate::ui;

/// Rows of a list the dialog shows before "and N more".
const SHOWN: usize = 8;

impl RepoView {
    /// Ask for a folder and write `id` and the commits after it up to HEAD there as
    /// numbered patches.
    pub(super) fn save_patches_to_head(
        &mut self,
        id: Oid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Save Patches Here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = chosen.await else {
                return;
            };
            let Some(dir) = paths.pop() else { return };
            let _ = this.update(cx, |v, cx| {
                let rev = id.to_string();
                v.run_op(
                    "Saving patches",
                    "Patches saved",
                    move |r, _, on_line| {
                        for f in r.format_patches_to_head(&rev, &dir)? {
                            on_line(&f.to_string_lossy());
                        }
                        Ok(())
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    /// Copy `id` as a mailbox patch (read off the UI thread).
    pub(super) fn copy_patch(&mut self, id: Oid, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        let task = cx
            .background_executor()
            .spawn(async move { repo.patch_text(&id.to_string()) });
        cx.spawn(async move |this, cx| match task.await {
            Ok(text) => cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text))),
            Err(e) => {
                let _ = this.update(cx, |v, cx| {
                    v.op = Some(OpStatus::Failed(format!("Copy as Patch: {e}").into()));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Ask for patch files, then [`RepoView::apply_patches_dialog`].
    pub fn pick_patches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Apply".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = this.update_in(cx, |v, window, cx| {
                v.apply_patches_dialog(paths, window, cx)
            });
        })
        .detach();
    }

    /// Read what `paths` change (off the UI thread), then ask how to apply them.
    pub fn apply_patches_dialog(
        &mut self,
        mut paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy() || paths.is_empty() {
            return;
        }
        // `git format-patch` numbers its files, so their names give the order.
        paths.sort();
        let repo = self.repo.clone();
        let read = paths.clone();
        let task = cx.background_executor().spawn(async move {
            let refs: Vec<&Path> = read.iter().map(PathBuf::as_path).collect();
            repo.inspect_patches(&refs)
        });
        cx.spawn_in(window, async move |this, cx| {
            let info = task.await;
            let _ = this.update_in(cx, |v, window, cx| match info {
                Ok(info) if !info.files.is_empty() => v.open_apply_dialog(paths, info, window, cx),
                Ok(_) => {
                    v.op = Some(OpStatus::Failed(
                        "Apply Patch: no changes in that file".into(),
                    ));
                    cx.notify();
                }
                Err(e) => {
                    v.op = Some(OpStatus::Failed(format!("Apply Patch: {e}").into()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open_apply_dialog(
        &mut self,
        paths: Vec<PathBuf>,
        info: PatchInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mailbox = !info.commits.is_empty();
        let mode = Rc::new(Cell::new(if mailbox {
            ApplyMode::Commit
        } else {
            ApplyMode::WorkTree
        }));
        let three_way = Rc::new(Cell::new(info.problem.is_some()));
        let names = paths
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(", ");
        let (paths, info) = (Rc::new(paths), Rc::new(info));
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let p = palette(cx);
            let current = mode.get();
            let mut modes = Vec::new();
            if mailbox {
                modes.push(("Commit", ApplyMode::Commit));
            }
            modes.push(("Apply", ApplyMode::WorkTree));
            modes.push(("Apply and Stage", ApplyMode::Index));
            let options = modes
                .into_iter()
                .map(|(label, m)| {
                    let mode = mode.clone();
                    let on_click: ui::OnClick = Box::new(move |_, window, _| {
                        mode.set(m);
                        window.refresh();
                    });
                    (SharedString::from(label), m == current, on_click)
                })
                .collect();
            let explain = match current {
                ApplyMode::Commit => {
                    "Makes a commit per patch with its author and message (git am)."
                }
                ApplyMode::WorkTree => {
                    "Changes the files; nothing is staged or committed (git apply)."
                }
                ApplyMode::Index => "Changes the files and stages the changes (git apply --index).",
            };
            let heading = |text: String| div().text_size(ts::SMALL).text_color(p.fg3).child(text);
            let row = |text: String| div().text_size(ts::BODY).truncate().child(text);
            let more = |n: usize| {
                (n > SHOWN).then(|| {
                    div()
                        .text_size(ts::SMALL)
                        .text_color(p.fg3)
                        .child(format!("and {} more", n - SHOWN))
                })
            };
            let commits = &info.commits;
            let files = &info.files;
            let three = three_way.get();
            let (paths, info_ok, mode_ok, three_ok, view) = (
                paths.clone(),
                info.clone(),
                mode.clone(),
                three_way.clone(),
                view.clone(),
            );
            let flag = three_way.clone();
            alert
                .confirm()
                .title(if paths.len() == 1 {
                    "Apply Patch".to_owned()
                } else {
                    format!("Apply {} Patches", paths.len())
                })
                .description(names.clone())
                .ok_text(if current == ApplyMode::Commit {
                    "Commit"
                } else {
                    "Apply"
                })
                .when(!commits.is_empty(), |a| {
                    a.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rpx(2.))
                            .child(heading(match commits.len() {
                                1 => "1 commit".to_owned(),
                                n => format!("{n} commits"),
                            }))
                            .children(commits.iter().take(SHOWN).map(|s| row(s.clone())))
                            .children(more(commits.len())),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rpx(2.))
                        .child(heading(match files.len() {
                            1 => "Changes 1 file".to_owned(),
                            n => format!("Changes {n} files"),
                        }))
                        .children(files.iter().take(SHOWN).map(|f| {
                            div()
                                .flex()
                                .gap(rpx(8.))
                                .text_size(ts::BODY)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(f.path.to_string_lossy().into_owned()),
                                )
                                .child(match f.lines {
                                    Some((a, r)) => div()
                                        .flex()
                                        .gap(rpx(6.))
                                        .child(div().text_color(p.added).child(format!("+{a}")))
                                        .child(div().text_color(p.deleted).child(format!("−{r}"))),
                                    None => div().text_color(p.fg3).child("binary"),
                                })
                        }))
                        .children(more(files.len())),
                )
                .when_some(info.problem.clone(), |a, problem| {
                    a.child(
                        div()
                            .text_size(ts::SMALL)
                            .text_color(p.modified)
                            .child(format!(
                                "Does not apply cleanly: {}",
                                problem
                                    .lines()
                                    .take(3)
                                    .map(|l| l.trim_start_matches("error: "))
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            )),
                    )
                })
                .child(ui::segmented("apply-mode", options, 22., &p))
                .child(div().text_size(ts::SMALL).text_color(p.fg3).child(explain))
                .child(
                    ui::checkbox(
                        "apply-3way",
                        three,
                        "Three-way merge if it does not apply cleanly",
                        &p,
                    )
                    .cursor_pointer()
                    .on_click(move |_, window, _| {
                        flag.set(!flag.get());
                        window.refresh();
                    }),
                )
                .when(three, |a| {
                    a.child(
                        div()
                            .text_size(ts::SMALL)
                            .text_color(p.fg3)
                            .child("Conflicts are left in the files to resolve, as after a merge."),
                    )
                })
                .on_ok(move |_, _, cx: &mut App| {
                    let (mode, three_way) = (mode_ok.get(), three_ok.get());
                    let paths: Vec<PathBuf> = paths.as_ref().clone();
                    let commits = info_ok.commits.len();
                    let (label, done) = match mode {
                        ApplyMode::Commit => (
                            "Applying patches",
                            match commits {
                                1 => "Committed 1 patch".to_owned(),
                                n => format!("Committed {n} patches"),
                            },
                        ),
                        ApplyMode::WorkTree | ApplyMode::Index => {
                            ("Applying patch", "Patch applied".to_owned())
                        }
                    };
                    let _ = view.update(cx, |v, cx| {
                        v.run_op(
                            label,
                            done,
                            move |r, _, _| {
                                let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
                                r.apply_patches(&refs, mode, three_way)
                            },
                            cx,
                        )
                    });
                    true
                })
        });
    }
}

/// Whether a dropped file looks like a patch (by its extension).
pub fn is_patch_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "patch" | "diff" | "eml" | "mbox"
        )
    })
}
