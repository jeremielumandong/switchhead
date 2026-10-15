//! Linked worktrees in a repository tab: add one (an existing branch, or a new branch from
//! HEAD, in a folder next to the repository), remove, lock and unlock them, and prune the
//! records of deleted folders. A new worktree can open in a tab when it is ready.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Focusable as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use switchhead_git::worktrees::default_path;

use super::{RepoView, RepoViewEvent};
use crate::appearance::{rpx, ts};
use crate::sidebar_model::branch_name_problem;
use crate::theme::palette;
use crate::ui;

impl RepoView {
    /// Ask for a branch and a folder, then add a worktree there: the branch is checked out
    /// when it exists, else created from HEAD.
    pub fn add_worktree_dialog(
        &mut self,
        branch: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.repo.root().to_path_buf();
        let locals: Rc<Vec<String>> =
            Rc::new(self.refs.branches.iter().map(|b| b.name.clone()).collect());
        let initial = branch.unwrap_or_default();
        let branch_input = cx.new(|cx| {
            let mut s = InputState::new(window, cx)
                .placeholder("Branch: an existing one, or a new one to create from HEAD");
            s.set_value(initial.clone(), window, cx);
            s
        });
        let folder = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Folder (default: next to this repository)")
        });
        let open = Rc::new(Cell::new(true));
        let error: Rc<RefCell<Option<String>>> = Rc::default();
        {
            let error = error.clone();
            cx.subscribe_in(
                &branch_input,
                window,
                move |_, _, ev: &InputEvent, window, _| {
                    if matches!(ev, InputEvent::Change) {
                        error.borrow_mut().take();
                        // The hint under the field follows the name.
                        window.refresh();
                    }
                },
            )
            .detach();
        }
        let first = Cell::new(true);
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, window, cx| {
            if first.replace(false) {
                window.focus(&branch_input.focus_handle(cx), cx);
            }
            let p = palette(cx);
            let name = branch_input.read(cx).value().trim().to_owned();
            let hint = if name.is_empty() {
                None
            } else if locals.contains(&name) {
                Some(format!("Checks out {name}."))
            } else {
                Some(format!("Creates {name} from HEAD."))
            };
            let shown_error = error.borrow().clone();
            let shown_folder = default_path(&root, &name);
            let on = open.get();
            let (branch_input, folder, open, error, view, locals, root) = (
                branch_input.clone(),
                folder.clone(),
                open.clone(),
                error.clone(),
                view.clone(),
                locals.clone(),
                root.clone(),
            );
            alert
                .confirm()
                .title("Add Worktree")
                .description(
                    "Checks a branch out in another folder, so you can work on two branches \
                     at once. Both folders share this repository's history.",
                )
                .ok_text("Add Worktree")
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rpx(4.))
                        .child(Input::new(&branch_input).text_size(ts::BODY))
                        .map(|d| match (shown_error, hint) {
                            (Some(e), _) => {
                                d.child(div().text_size(ts::SMALL).text_color(p.deleted).child(e))
                            }
                            (None, Some(h)) => {
                                d.child(div().text_size(ts::SMALL).text_color(p.fg3).child(h))
                            }
                            _ => d,
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rpx(4.))
                        .child(Input::new(&folder).text_size(ts::BODY))
                        .child(
                            div()
                                .text_size(ts::SMALL)
                                .text_color(p.fg3)
                                .child(format!("Default: {}", shown_folder.display())),
                        ),
                )
                .child({
                    let open = open.clone();
                    ui::checkbox("worktree-open", on, "Open it in a new tab", &p)
                        .cursor_pointer()
                        .on_click(move |_, window, _| {
                            open.set(!open.get());
                            window.refresh();
                        })
                })
                .on_ok(move |_, window, cx: &mut App| {
                    let name = branch_input.read(cx).value().trim().to_owned();
                    let exists = locals.contains(&name);
                    if let Some(problem) = (!exists).then(|| branch_name_problem(&name)).flatten() {
                        *error.borrow_mut() = Some(problem.to_owned());
                        window.refresh();
                        return false;
                    }
                    let typed = folder.read(cx).value().trim().to_owned();
                    let path = if typed.is_empty() {
                        default_path(&root, &name)
                    } else {
                        let p = PathBuf::from(typed);
                        if p.is_absolute() { p } else { root.join(p) }
                    };
                    let open = open.get();
                    let _ = view.update(cx, |v, cx| v.add_worktree(path, name, exists, open, cx));
                    true
                })
        });
    }

    fn add_worktree(
        &mut self,
        path: PathBuf,
        branch: String,
        exists: bool,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        let target = path.clone();
        self.run_op_then(
            "Adding worktree",
            "Worktree added",
            move |r, _, _| {
                if exists {
                    r.worktree_add(&path, &branch, None)
                } else {
                    r.worktree_add(&path, "HEAD", Some(&branch))
                }
            },
            move |_, cx| {
                if open {
                    cx.emit(RepoViewEvent::Open(target));
                }
            },
            cx,
        );
    }

    /// Remove the worktree at `path` after asking; `force` is offered for one with changes.
    pub fn remove_worktree_dialog(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let force = Rc::new(Cell::new(false));
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let p = palette(cx);
            let on = force.get();
            let (this, path, force) = (this.clone(), path.clone(), force.clone());
            alert
                .confirm()
                .title("Remove this worktree?")
                .description(format!(
                    "Deletes the folder {}. Its branch and commits stay in the repository.",
                    path.display()
                ))
                .child({
                    let force = force.clone();
                    ui::checkbox(
                        "worktree-force",
                        on,
                        "Even with uncommitted changes or untracked files (they are lost)",
                        &p,
                    )
                    .cursor_pointer()
                    .on_click(move |_, window, _| {
                        force.set(!force.get());
                        window.refresh();
                    })
                })
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let (path, force) = (path.clone(), force.get());
                    let _ = this.update(cx, |v, cx| {
                        v.run_op(
                            "Removing worktree",
                            "Worktree removed",
                            move |r, _, _| r.worktree_remove(&path, force),
                            cx,
                        )
                    });
                    true
                })
        });
    }

    /// Lock (`true`) the worktree at `path` against pruning, or unlock it.
    pub fn lock_worktree(&mut self, path: PathBuf, lock: bool, cx: &mut Context<Self>) {
        let (label, done) = if lock {
            ("Locking worktree", "Worktree locked")
        } else {
            ("Unlocking worktree", "Worktree unlocked")
        };
        self.run_op(
            label,
            done,
            move |r, _, _| {
                if lock {
                    r.worktree_lock(&path, None)
                } else {
                    r.worktree_unlock(&path)
                }
            },
            cx,
        );
    }

    /// Forget worktrees whose folders were deleted.
    pub fn prune_worktrees(&mut self, cx: &mut Context<Self>) {
        self.run_op(
            "Pruning worktrees",
            "Worktrees pruned",
            |r, _, _| r.worktree_prune(),
            cx,
        );
    }
}
