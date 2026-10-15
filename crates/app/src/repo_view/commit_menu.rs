//! The graph's right-click menu for a commit: check out, new branch or tag here,
//! cherry-pick, revert, reset the current branch here, copy its id or message, save it
//! (or it and the commits up to HEAD) as patches or copy it as one, and open it on
//! GitHub / GitLab / Bitbucket.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, ClipboardItem, Context, PathPromptOptions, WeakEntity, Window};
use switchhead_git::ops::ResetMode;
use switchhead_git::{Oid, web};

use super::{OpStatus, RepoView};

impl RepoView {
    /// Select row `i` for its context menu (a right click).
    pub(super) fn select_for_menu(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.selected != Some(i) {
            self.selected = Some(i);
            self.sync_details(false, cx);
            cx.notify();
        }
    }

    pub(super) fn busy(&self) -> bool {
        matches!(self.op, Some(OpStatus::Running { .. }))
    }

    pub(super) fn checkout_commit_dialog(
        &mut self,
        id: Oid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let short = switchhead_git::short(&id);
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title(format!("Check out commit {short}?"))
                .description(
                    "HEAD will be detached at this commit. Create a branch there before \
                     committing, or the new commits belong to no branch.",
                )
                .ok_text("Check Out")
                .on_ok(move |_, _, cx| {
                    let rev = id.to_string();
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

    fn cherry_pick(&mut self, id: Oid, cx: &mut Context<Self>) {
        let rev = id.to_string();
        self.run_op(
            "Cherry-picking",
            "Cherry-picked",
            move |r, _, _| r.cherry_pick(&[&rev]),
            cx,
        );
    }

    fn revert(&mut self, id: Oid, cx: &mut Context<Self>) {
        let rev = id.to_string();
        self.run_op("Reverting", "Reverted", move |r, _, _| r.revert(&rev), cx);
    }

    pub(super) fn reset_to(
        &mut self,
        id: Oid,
        mode: ResetMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rev = id.to_string();
        let label = format!(
            "Hard reset {} to {}",
            self.head_label(),
            switchhead_git::short(&id)
        );
        let run = move |v: &mut RepoView, cx: &mut Context<RepoView>| {
            let (rev, label) = (rev.clone(), label.clone());
            v.run_op(
                "Resetting",
                "Reset",
                move |r, _, _| {
                    if mode == ResetMode::Hard {
                        switchhead_git::undo::guard(r, &label, &[]);
                    }
                    r.reset(&rev, mode)
                },
                cx,
            );
        };
        if mode != ResetMode::Hard {
            run(self, cx);
            return;
        }
        let branch = self.head_label();
        let short = switchhead_git::short(&id);
        let this = cx.entity().downgrade();
        let run = std::rc::Rc::new(run);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, run) = (this.clone(), run.clone());
            alert
                .confirm()
                .title(format!("Hard reset {branch} to {short}?"))
                .description(
                    "Uncommitted changes in tracked files are thrown away, and commits after \
                     this one leave the branch. Undo (Ctrl/Cmd+Z) brings both back.",
                )
                .ok_text("Reset")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let run = run.clone();
                    let _ = this.update(cx, |v, cx| run(v, cx));
                    true
                })
        });
    }

    /// Copy the whole message (read off the UI thread).
    fn copy_message(&mut self, id: Oid, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        let task = cx
            .background_executor()
            .spawn(async move { switchhead_git::commit::info(&repo, id).map(|i| i.message) });
        cx.spawn(async move |_, cx| {
            if let Ok(message) = task.await {
                cx.update(|cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(message.trim_end().into()))
                });
            }
        })
        .detach();
    }

    /// Ask for a folder and write the commit there as a patch (`git format-patch -1`).
    fn save_patch(&mut self, id: Oid, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Save Patch Here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = chosen.await else {
                return;
            };
            let Some(dir) = paths.pop() else { return };
            let _ = this.update(cx, |v, cx| {
                let rev = id.to_string();
                v.run_op(
                    "Saving patch",
                    "Patch saved",
                    move |r, _, on_line| {
                        for f in r.format_patch(&["-1", &rev], &dir)? {
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

    /// The commit's page on the forge of the upstream's remote (or `origin`).
    fn commit_web_page(&self, id: Oid) -> Option<(web::Forge, String)> {
        web::commit_page(self.main_remote_url()?, &id.to_string())
    }
}

/// A menu item's action on the view.
type ViewAction = dyn Fn(&mut RepoView, &mut Window, &mut Context<RepoView>);

/// The menu for the selected commit.
pub(super) fn build_commit_menu(
    mut menu: PopupMenu,
    view: &WeakEntity<RepoView>,
    _window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(v) = view.upgrade() else {
        return menu;
    };
    let (id, is_head, merge, busy, branch, web, summary, in_progress) = {
        let v = v.read(cx);
        let Some(i) = v.selected else {
            return menu;
        };
        let id = v.log.id(i);
        (
            id,
            v.refs.head == Some(id),
            v.log.parents(i).len() > 1,
            v.busy(),
            v.refs.head_branch.clone(),
            v.commit_web_page(id),
            v.log.summary(i).to_owned(),
            v.state.command().is_some(),
        )
    };
    let bisecting = v.read(cx).state == switchhead_git::RepoState::Bisect;
    let clean = v.read(cx).state == switchhead_git::RepoState::Clean;
    let act = |f: Box<ViewAction>| {
        let view = view.clone();
        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| f(v, window, cx));
        }
    };
    let short = switchhead_git::short(&id);
    let target = branch.clone().unwrap_or_else(|| "HEAD".into());
    menu = menu
        .item(
            PopupMenuItem::new("Check Out This Commit…")
                .disabled(busy || is_head)
                .on_click(act(Box::new(move |v, w, cx| {
                    v.checkout_commit_dialog(id, w, cx)
                }))),
        )
        .item(
            PopupMenuItem::new("New Branch Here…")
                .disabled(busy)
                .on_click(act(Box::new(move |v, w, cx| {
                    let short = switchhead_git::short(&id);
                    v.new_branch_dialog(id.to_string(), format!("commit {short}"), w, cx)
                }))),
        )
        .item(
            PopupMenuItem::new("New Tag Here…")
                .disabled(busy)
                .on_click(act(Box::new(move |v, w, cx| {
                    let short = switchhead_git::short(&id);
                    v.new_tag_dialog(id.to_string(), format!("commit {short}"), w, cx)
                }))),
        )
        .separator()
        .item(
            PopupMenuItem::new(if merge {
                "Cherry-Pick (not for merge commits)".to_owned()
            } else {
                format!("Cherry-Pick onto {target}")
            })
            .disabled(busy || is_head || merge)
            .on_click(act(Box::new(move |v, _, cx| v.cherry_pick(id, cx)))),
        )
        .item(
            PopupMenuItem::new(if merge {
                "Revert (not for merge commits)"
            } else {
                "Revert Commit"
            })
            .disabled(busy || merge)
            .on_click(act(Box::new(move |v, _, cx| v.revert(id, cx)))),
        );
    let view_r = view.clone();
    menu = menu.submenu(
        format!("Reset {target} to Here"),
        _window,
        cx,
        move |mut m, _, _| {
            for (label, mode) in [
                ("Soft: keep changes staged", ResetMode::Soft),
                ("Mixed: keep changes unstaged", ResetMode::Mixed),
                ("Hard: discard all changes…", ResetMode::Hard),
            ] {
                let view = view_r.clone();
                m = m.item(PopupMenuItem::new(label).disabled(busy).on_click(
                    move |_, window, cx| {
                        let _ = view.update(cx, |v, cx| v.reset_to(id, mode, window, cx));
                    },
                ));
            }
            m
        },
    );
    let rebase_summary = summary.clone();
    let summary_e = summary.clone();
    menu = menu.item(
        PopupMenuItem::new(format!("Interactive Rebase {target} from Here…"))
            .disabled(busy || is_head || in_progress)
            .on_click(act(Box::new(move |v, w, cx| {
                v.open_rebase_editor(id, rebase_summary.clone(), w, cx)
            }))),
    );
    menu = menu.separator();
    menu = if bisecting {
        use switchhead_git::ops::BisectMark;
        let mark = |label: &'static str, m: BisectMark| {
            PopupMenuItem::new(label)
                .disabled(busy)
                .on_click(act(Box::new(move |v, _, cx| {
                    v.bisect_mark(m, Some(id), cx)
                })))
        };
        menu.item(mark("Bisect: Mark as Bad", BisectMark::Bad))
            .item(mark("Bisect: Mark as Good", BisectMark::Good))
            .item(mark("Bisect: Skip This Commit", BisectMark::Skip))
            .item(
                PopupMenuItem::new("Stop Bisect")
                    .disabled(busy)
                    .on_click(act(Box::new(|v, _, cx| v.stop_bisect(cx)))),
            )
    } else {
        menu.item(
            PopupMenuItem::new("Start Bisect…")
                .disabled(busy || !clean)
                .on_click(act(Box::new(move |v, w, cx| {
                    v.start_bisect_dialog(Some(id), w, cx)
                }))),
        )
    };
    let full = id.to_string();
    let short_s = short.clone();
    menu = menu
        .separator()
        .item(
            PopupMenuItem::new("Copy Commit ID").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(full.clone()))
            }),
        )
        .item(
            PopupMenuItem::new("Copy Short ID").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(short_s.clone()))
            }),
        )
        .item(
            PopupMenuItem::new("Copy Summary").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(summary.clone()))
            }),
        )
        .item(
            PopupMenuItem::new("Copy Message")
                .on_click(act(Box::new(move |v, _, cx| v.copy_message(id, cx)))),
        )
        .separator()
        .item(PopupMenuItem::new("Explain Commit").on_click(act(Box::new({
            let summary = summary_e.clone();
            move |v, w, cx| v.explain_commit(id, summary.clone(), w, cx)
        }))))
        .item(
            PopupMenuItem::new("Browse Files at This Commit")
                .on_click(act(Box::new(move |v, w, cx| v.open_tree(id, w, cx)))),
        )
        .item(
            PopupMenuItem::new("Save as Patch…")
                .disabled(busy)
                .on_click(act(Box::new(move |v, w, cx| v.save_patch(id, w, cx)))),
        )
        .item(
            PopupMenuItem::new("Save Patches from Here to HEAD…")
                .disabled(busy || is_head)
                .on_click(act(Box::new(move |v, w, cx| {
                    v.save_patches_to_head(id, w, cx)
                }))),
        )
        .item(
            PopupMenuItem::new("Copy as Patch")
                .on_click(act(Box::new(move |v, _, cx| v.copy_patch(id, cx)))),
        );
    if let Some((forge, url)) = web {
        menu = menu.item(
            PopupMenuItem::new(format!("Open on {}", forge.name()))
                .on_click(move |_, _, cx| cx.open_url(&url)),
        );
    }
    let view_c = view.clone();
    crate::custom_commands::add_to_menu(
        menu,
        crate::custom_commands::CommandTarget::Commit,
        busy,
        move |i, window, cx| {
            let on = super::custom_ops::CustomOn {
                sha: Some(id.to_string()),
                reference: Some(id.to_string()),
                file: None,
            };
            let _ = view_c.update(cx, |v, cx| v.run_custom_command(i, on, window, cx));
        },
        cx,
    )
}
