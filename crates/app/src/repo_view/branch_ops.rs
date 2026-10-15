//! Branch operations from the sidebar's context menu and the toolbar: create, rename,
//! delete (local and on the remote), set upstream, merge into the current branch, rebase
//! it, push, and compare two branches in the graph.
//!
//! Each runs `git` through [`RepoView::run_op`]; dialogs that need a name or options use
//! one small form ([`Form`]).

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, ClipboardItem, Context, WeakEntity, Window};
use switchhead_git::log::Scope;
use switchhead_git::ops::{MergeMode, PushOptions};

use super::RepoView;
use super::form::Form;
use super::sidebar::{SideRow, Target};

impl RepoView {
    pub(super) fn branch_names(&self) -> Vec<String> {
        self.refs.branches.iter().map(|b| b.name.clone()).collect()
    }

    /// New branch at `start` (a revision), described as `from` in the dialog.
    pub(super) fn new_branch_dialog(
        &mut self,
        start: String,
        from: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = Form {
            title: "New Branch".into(),
            description: format!("Starts at {from}."),
            ok: "Create Branch",
            danger: false,
            name: Some(String::new()),
            placeholder: "Branch name",
            taken: self.branch_names(),
            message: None,
            url: None,
            checks: vec![("Check out the new branch", true)],
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let start = start.clone();
                let checkout = a.checks.first().copied().unwrap_or(true);
                v.run_op(
                    "Creating branch",
                    "Branch created",
                    move |r, _, _| r.create_branch(&a.name, &start, checkout),
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// New branch at the selected commit (HEAD when nothing is selected).
    pub fn new_branch_at_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.op, Some(super::OpStatus::Running { .. })) {
            return;
        }
        let (start, from) = match self.selected.map(|i| self.log.id(i)) {
            Some(id) if Some(id) != self.refs.head => {
                let short = switchhead_git::short(&id);
                (id.to_string(), format!("commit {short}"))
            }
            _ => ("HEAD".to_owned(), self.head_label().to_string()),
        };
        self.new_branch_dialog(start, from, window, cx);
    }

    fn rename_branch_dialog(&mut self, old: String, window: &mut Window, cx: &mut Context<Self>) {
        let taken = self
            .branch_names()
            .into_iter()
            .filter(|n| *n != old)
            .collect();
        let form = Form {
            title: format!("Rename {old}"),
            description: "Its upstream setting moves with it; the remote branch keeps its name."
                .into(),
            ok: "Rename",
            danger: false,
            name: Some(old.clone()),
            placeholder: "Branch name",
            taken,
            message: None,
            url: None,
            checks: Vec::new(),
        };
        self.open_form(
            form,
            move |v, a, cx| {
                if a.name == old {
                    return;
                }
                let old = old.clone();
                v.run_op(
                    "Renaming branch",
                    "Branch renamed",
                    move |r, _, _| r.rename_branch(&old, &a.name),
                    cx,
                );
            },
            window,
            cx,
        );
    }

    fn delete_branch_dialog(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        // The upstream, when it is still there, can go too.
        let upstream = self
            .refs
            .branches
            .iter()
            .find(|b| b.name == name)
            .and_then(|b| b.tracking.as_ref())
            .filter(|t| !t.gone)
            .map(|t| t.upstream.clone());
        let mut checks = vec![("Delete even if not merged", false)];
        if upstream.is_some() {
            checks.push(("Also delete its upstream on the remote", false));
        }
        let form = Form {
            title: format!("Delete {name}?"),
            description: match &upstream {
                Some(u) => format!("The local branch is deleted. Its upstream is {u}."),
                None => "The local branch is deleted.".into(),
            },
            ok: "Delete",
            danger: true,
            name: None,
            placeholder: "Branch name",
            taken: Vec::new(),
            message: None,
            url: None,
            checks,
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let force = a.checks.first().copied().unwrap_or(false);
                let remote = upstream
                    .clone()
                    .filter(|_| a.checks.get(1).copied().unwrap_or(false))
                    .and_then(|u| split_remote(&u));
                let name = name.clone();
                v.run_op(
                    "Deleting branch",
                    "Branch deleted",
                    move |r, cancel, on_line| {
                        r.delete_branch(&name, force)?;
                        if let Some((remote, branch)) = remote {
                            r.delete_remote_ref(&remote, &branch, cancel, on_line)?;
                        }
                        Ok(())
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }

    fn delete_remote_branch_dialog(
        &mut self,
        short: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((remote, branch)) = split_remote(&short) else {
            return;
        };
        let form = Form {
            title: format!("Delete {branch} from {remote}?"),
            description: format!(
                "The branch is deleted on {remote} for everyone (git push {remote} --delete \
                 {branch})."
            ),
            ok: "Delete",
            danger: true,
            name: None,
            placeholder: "Branch name",
            taken: Vec::new(),
            message: None,
            url: None,
            checks: Vec::new(),
        };
        self.open_form(
            form,
            move |v, _, cx| {
                let (remote, branch) = (remote.clone(), branch.clone());
                v.run_op(
                    "Deleting remote branch",
                    "Remote branch deleted",
                    move |r, cancel, on_line| {
                        r.delete_remote_ref(&remote, &branch, cancel, on_line)
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }

    fn merge_into_current(&mut self, rev: String, mode: MergeMode, cx: &mut Context<Self>) {
        self.run_op("Merging", "Merged", move |r, _, _| r.merge(&rev, mode), cx);
    }

    fn rebase_current_onto(&mut self, rev: String, cx: &mut Context<Self>) {
        self.run_op(
            "Rebasing",
            "Rebased",
            move |r, _, _| r.rebase_onto(&rev),
            cx,
        );
    }

    fn set_upstream(&mut self, branch: String, upstream: Option<String>, cx: &mut Context<Self>) {
        self.run_op(
            "Setting upstream",
            "Upstream set",
            move |r, _, _| r.set_upstream(&branch, upstream.as_deref()),
            cx,
        );
    }

    /// Push `branch` to its upstream's remote, or to the first remote with
    /// `--set-upstream` when it has none.
    fn push_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        let tracking = self
            .refs
            .branches
            .iter()
            .find(|b| b.name == branch)
            .and_then(|b| b.tracking.clone());
        let opts = match tracking.as_ref().and_then(|t| split_remote(&t.upstream)) {
            Some((remote, up)) => PushOptions {
                remote,
                refspec: format!("{branch}:{up}"),
                ..Default::default()
            },
            None => PushOptions {
                remote: self.default_remote().unwrap_or_default(),
                refspec: branch,
                set_upstream: true,
                ..Default::default()
            },
        };
        if opts.remote.is_empty() {
            return;
        }
        self.run_op(
            "Pushing",
            "Pushed",
            move |r, cancel, on_line| r.push(&opts, cancel, on_line),
            cx,
        );
    }

    /// `origin` when there is one, else the first remote.
    pub(super) fn default_remote(&self) -> Option<String> {
        let names = self.refs.remote_names();
        names
            .iter()
            .find(|n| **n == "origin")
            .or(names.first())
            .map(|n| (*n).to_owned())
    }

    /// Show the commits on either `rev` or the current branch but not both.
    fn compare_with_current(&mut self, rev: String, cx: &mut Context<Self>) {
        let head = self
            .refs
            .head_branch
            .clone()
            .unwrap_or_else(|| "HEAD".into());
        self.set_scope(Scope::Compare(head, rev), cx);
    }
}

/// A menu item's action on the view.
pub(super) type ViewAction = dyn Fn(&mut RepoView, &mut Window, &mut Context<RepoView>);

/// `origin/feature/x` → (`origin`, `feature/x`).
fn split_remote(short: &str) -> Option<(String, String)> {
    let (remote, branch) = short.split_once('/')?;
    Some((remote.to_owned(), branch.to_owned()))
}

/// The sidebar's context menu for the row the right click landed on.
pub(super) fn build_ref_menu(
    mut menu: PopupMenu,
    view: &WeakEntity<RepoView>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(v) = view.upgrade() else {
        return menu;
    };
    let (row, current, has_remote, remotes) = {
        let v = v.read(cx);
        let row = v.menu_row.and_then(|i| v.side_rows.get(i).cloned());
        let remotes: Vec<String> = v
            .refs
            .remotes
            .iter()
            .filter(|r| !r.is_symbolic_head)
            .map(|r| r.short())
            .collect();
        (
            row,
            v.refs.head_branch.clone(),
            !v.refs.remote_names().is_empty(),
            remotes,
        )
    };
    let (target, rev, id) = match row {
        Some(SideRow::Ref {
            target, rev, id, ..
        }) => (target, rev, id),
        Some(row) => return super::section_menu::build_section_menu(menu, row, view, cx),
        None => return menu,
    };
    let act = |f: Box<ViewAction>| {
        let view = view.clone();
        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| f(v, window, cx));
        }
    };
    if let Target::Worktree {
        path,
        current,
        main,
        locked,
        missing,
    } = &target
    {
        let busy = v.read(cx).busy();
        let text = path.to_string_lossy().into_owned();
        let (open, remove, lock) = (path.clone(), path.clone(), path.clone());
        let (locked, missing) = (*locked, *missing);
        let mut menu = menu;
        if !*current && !missing {
            menu = menu
                .item(PopupMenuItem::new("Open in New Tab").on_click(act(Box::new(
                    move |_, _, cx| cx.emit(super::RepoViewEvent::Open(open.clone())),
                ))))
                .separator();
        }
        if !*main {
            menu = menu
                .item(
                    PopupMenuItem::new(if locked { "Unlock" } else { "Lock" })
                        .disabled(busy)
                        .on_click(act(Box::new(move |v, _, cx| {
                            v.lock_worktree(lock.clone(), !locked, cx)
                        }))),
                )
                .item(
                    PopupMenuItem::new(if missing { "Prune" } else { "Remove…" })
                        .disabled(busy || *current || locked)
                        .on_click(act(Box::new(move |v, w, cx| {
                            if missing {
                                v.prune_worktrees(cx)
                            } else {
                                v.remove_worktree_dialog(remove.clone(), w, cx)
                            }
                        }))),
                )
                .separator();
        }
        return menu.item(PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.clone()))
        }));
    }
    if let Target::Submodule { path, cloned } = &target {
        let busy = v.read(cx).busy();
        let full = v.read(cx).repo.root().join(path);
        let (one, deep) = (vec![path.clone()], vec![path.clone()]);
        let text = path.to_string_lossy().into_owned();
        let menu = if *cloned {
            menu.item(PopupMenuItem::new("Open in New Tab").on_click(act(Box::new(
                move |_, _, cx| cx.emit(super::RepoViewEvent::Open(full.clone())),
            ))))
            .separator()
        } else {
            menu
        };
        return menu
            .item(
                PopupMenuItem::new(if *cloned { "Update" } else { "Clone" })
                    .disabled(busy)
                    .on_click(act(Box::new(move |v, _, cx| {
                        v.update_submodules(one.clone(), false, cx)
                    }))),
            )
            .item(
                PopupMenuItem::new(if *cloned {
                    "Update Recursively"
                } else {
                    "Clone Recursively"
                })
                .disabled(busy)
                .on_click(act(Box::new(move |v, _, cx| {
                    v.update_submodules(deep.clone(), true, cx)
                }))),
            )
            .separator()
            .item(PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.clone()))
            }));
    }
    if let Target::LfsLock(path) = &target {
        let busy = v.read(cx).busy();
        let (unlock, force) = (path.clone(), path.clone());
        let text = path.to_string_lossy().into_owned();
        return menu
            .item(
                PopupMenuItem::new("Unlock")
                    .disabled(busy)
                    .on_click(act(Box::new(move |v, _, cx| {
                        v.lfs_unlock(unlock.clone(), cx)
                    }))),
            )
            .item(
                PopupMenuItem::new("Force Unlock…")
                    .disabled(busy)
                    .on_click(act(Box::new(move |v, w, cx| {
                        v.lfs_force_unlock(force.clone(), w, cx)
                    }))),
            )
            .separator()
            .item(PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.clone()))
            }));
    }
    if let (Target::Stash(index), Some(id)) = (&target, id) {
        return super::stash_ops::build_stash_menu(menu, *index, id, &act, view, cx);
    }
    let (name, rev_for_ops) = match &target {
        Target::Branch(n) => (n.clone(), n.clone()),
        Target::Remote(s) => (s.clone(), s.clone()),
        Target::Tag(t) => (t.clone(), format!("refs/tags/{t}")),
        _ => return menu,
    };
    let is_current = matches!(&target, Target::Branch(n) if Some(n) == current.as_ref());
    let current_label = current.clone().unwrap_or_else(|| "HEAD".into());

    // Check out.
    {
        let target = target.clone();
        menu = menu.item(
            PopupMenuItem::new(if matches!(target, Target::Tag(_)) {
                "Check Out…"
            } else {
                "Check Out"
            })
            .disabled(is_current)
            .on_click(act(Box::new(move |v, window, cx| {
                v.activate_target(target.clone(), window, cx)
            }))),
        );
    }
    // Merge and rebase with the current branch.
    if !is_current {
        let modes = [
            ("Merge", MergeMode::Default),
            ("Fast-Forward Only", MergeMode::FastForwardOnly),
            ("Always Create a Merge Commit", MergeMode::NoFastForward),
            ("Squash (stage the changes, no commit)", MergeMode::Squash),
        ];
        let view_m = view.clone();
        let rev_m = rev_for_ops.clone();
        menu = menu.submenu(
            format!("Merge {name} into {current_label}"),
            window,
            cx,
            move |mut m, _, _| {
                for (label, mode) in modes {
                    let view = view_m.clone();
                    let rev = rev_m.clone();
                    m = m.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let rev = rev.clone();
                        let _ = view.update(cx, |v, cx| v.merge_into_current(rev, mode, cx));
                    }));
                }
                m
            },
        );
        let rev_r = rev_for_ops.clone();
        menu = menu.item(
            PopupMenuItem::new(format!("Rebase {current_label} onto {name}"))
                .disabled(current.is_none())
                .on_click(act(Box::new(move |v, _, cx| {
                    v.rebase_current_onto(rev_r.clone(), cx)
                }))),
        );
    }
    menu = menu.separator();
    {
        let (start, from) = (rev_for_ops.clone(), name.clone());
        menu = menu.item(
            PopupMenuItem::new("New Branch from Here…").on_click(act(Box::new(
                move |v, window, cx| v.new_branch_dialog(start.clone(), from.clone(), window, cx),
            ))),
        );
    }
    match &target {
        Target::Branch(b) => {
            let b1 = b.clone();
            menu = menu.item(PopupMenuItem::new("Rename…").on_click(act(Box::new(
                move |v, window, cx| v.rename_branch_dialog(b1.clone(), window, cx),
            ))));
            let base = v.read(cx).review_base(b);
            let b5 = b.clone();
            menu = menu.item(
                PopupMenuItem::new(match &base {
                    Some(base) => format!("Review Against {base}"),
                    None => "Review Branch (no base branch)".to_owned(),
                })
                .disabled(base.is_none())
                .on_click(act(Box::new(move |v, window, cx| {
                    if let Some(base) = base.clone() {
                        v.review_branch(b5.clone(), base, window, cx)
                    }
                }))),
            );
            let b6 = b.clone();
            let elsewhere = v
                .read(cx)
                .refs
                .worktrees
                .iter()
                .any(|w| w.branch.as_deref() == Some(b.as_str()));
            menu = menu.item(
                PopupMenuItem::new("Check Out in New Worktree…")
                    .disabled(is_current || elsewhere)
                    .on_click(act(Box::new(move |v, window, cx| {
                        v.add_worktree_dialog(Some(b6.clone()), window, cx)
                    }))),
            );
            let flow = v
                .read(cx)
                .refs
                .flow
                .as_ref()
                .and_then(|f| f.classify(b))
                .map(|(k, n)| (k, n.to_owned()));
            if let Some((kind, flow_name)) = flow {
                menu = menu.item(
                    PopupMenuItem::new(format!("Finish {} {flow_name}…", kind.label()))
                        .disabled(v.read(cx).busy())
                        .on_click(act(Box::new(move |v, window, cx| {
                            v.flow_finish_dialog(kind, flow_name.clone(), window, cx)
                        }))),
                );
            }
            let b4 = b.clone();
            menu = menu.item(PopupMenuItem::new("Show Reflog").on_click(act(Box::new(
                move |v, window, cx| v.open_reflog(b4.clone(), window, cx),
            ))));
            let b2 = b.clone();
            menu = menu.item(
                PopupMenuItem::new("Delete…")
                    .disabled(is_current)
                    .on_click(act(Box::new(move |v, window, cx| {
                        v.delete_branch_dialog(b2.clone(), window, cx)
                    }))),
            );
            if has_remote {
                menu = menu.separator();
                let b3 = b.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!("Push {b}"))
                        .on_click(act(Box::new(move |v, _, cx| v.push_branch(b3.clone(), cx)))),
                );
                let b5 = b.clone();
                menu = menu.item(PopupMenuItem::new("Create Pull Request…").on_click(act(
                    Box::new(move |v, window, cx| {
                        v.create_pull_request(Some(b5.clone()), window, cx)
                    }),
                )));
                let view_u = view.clone();
                let branch = b.clone();
                menu = menu.submenu("Set Upstream", window, cx, move |mut m, _, _| {
                    for up in &remotes {
                        let view = view_u.clone();
                        let (branch, up) = (branch.clone(), up.clone());
                        m = m.item(PopupMenuItem::new(up.clone()).on_click(move |_, _, cx| {
                            let (branch, up) = (branch.clone(), up.clone());
                            let _ = view.update(cx, |v, cx| v.set_upstream(branch, Some(up), cx));
                        }));
                    }
                    let view = view_u.clone();
                    let branch = branch.clone();
                    m.separator()
                        .item(PopupMenuItem::new("Unset").on_click(move |_, _, cx| {
                            let branch = branch.clone();
                            let _ = view.update(cx, |v, cx| v.set_upstream(branch, None, cx));
                        }))
                });
            }
        }
        Target::Tag(t) => {
            if has_remote {
                let t1 = t.clone();
                menu = menu.item(
                    PopupMenuItem::new("Push Tag")
                        .on_click(act(Box::new(move |v, _, cx| v.push_tag(t1.clone(), cx)))),
                );
            }
            let t2 = t.clone();
            menu = menu.item(PopupMenuItem::new("Delete…").on_click(act(Box::new(
                move |v, window, cx| v.delete_tag_dialog(t2.clone(), window, cx),
            ))));
        }
        Target::Remote(short) => {
            let s = short.clone();
            menu = menu.item(
                PopupMenuItem::new("Delete from Remote…").on_click(act(Box::new(
                    move |v, window, cx| v.delete_remote_branch_dialog(s.clone(), window, cx),
                ))),
            );
        }
        _ => {}
    }
    menu = menu.separator();
    if !is_current {
        let rev_c = rev_for_ops.clone();
        menu = menu.item(
            PopupMenuItem::new(format!("Compare with {current_label}")).on_click(act(Box::new(
                move |v, _, cx| v.compare_with_current(rev_c.clone(), cx),
            ))),
        );
    }
    if let Some(rev) = rev {
        menu = menu.item(
            PopupMenuItem::new("Show Only This History").on_click(act(Box::new(
                move |v, _, cx| v.set_scope(Scope::Revs(vec![rev.clone()]), cx),
            ))),
        );
    }
    let copy = name.clone();
    menu =
        menu.item(PopupMenuItem::new("Copy Name").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
        }));
    let (view_c, on) = (
        view.clone(),
        super::custom_ops::CustomOn {
            sha: id.map(|i| i.to_string()),
            reference: Some(name.clone()),
            file: None,
        },
    );
    crate::custom_commands::add_to_menu(
        menu,
        crate::custom_commands::CommandTarget::Ref,
        v.read(cx).busy(),
        move |i, window, cx| {
            let on = on.clone();
            let _ = view_c.update(cx, |v, cx| v.run_custom_command(i, on, window, cx));
        },
        cx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_remote_names() {
        assert_eq!(
            split_remote("origin/feature/x"),
            Some(("origin".into(), "feature/x".into()))
        );
        assert_eq!(split_remote("main"), None);
    }
}
