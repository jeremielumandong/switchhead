//! Opening the reflog view ([`ReflogView`]) and carrying out its restores.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::{AppContext as _, Context, Window};
use switchhead_git::Oid;
use switchhead_git::ops::ResetMode;

use super::{Mode, OpStatus, RepoView};
use crate::reflog_view::{ReflogEvent, ReflogView};

impl RepoView {
    /// Show the reflog of `reference` (`HEAD` or a local branch) in place of the graph.
    pub(super) fn open_reflog(
        &mut self,
        reference: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let repo = self.repo.clone();
        let branches = self.branch_names();
        let head = self.refs.head_branch.clone();
        let view = cx.new(|cx| ReflogView::new(repo, reference, branches, head, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, ev, window, cx| match ev {
            ReflogEvent::Close => this.close_overlay(cx),
            ReflogEvent::Reveal(id) => this.reveal_from_reflog(*id, cx),
            ReflogEvent::Restore { reference, id } => {
                this.restore_from_reflog(reference.clone(), *id, window, cx)
            }
            ReflogEvent::NewBranch(id) => {
                let short = switchhead_git::short(id);
                this.new_branch_dialog(id.to_string(), short, window, cx);
            }
            ReflogEvent::Checkout(id) => this.checkout_commit_dialog(*id, window, cx),
        });
        self.reflog = Some(view.downgrade());
        self.overlay = Some((view.into(), sub));
        cx.notify();
    }

    /// Select `id` in the graph, or say why it is not there.
    fn reveal_from_reflog(&mut self, id: Oid, cx: &mut Context<Self>) {
        if self.log.row_of(&id).is_none() && !self.loading {
            self.op = Some(OpStatus::Failed(
                format!(
                    "{} is not in the graph: no branch or tag reaches it. Restore it or \
                     create a branch there to see it.",
                    switchhead_git::short(&id)
                )
                .into(),
            ));
            cx.notify();
            return;
        }
        self.close_overlay(cx);
        self.mode = Mode::Commits;
        self.reveal(id, cx);
    }

    /// Put `reference` back at `id`: a hard reset for HEAD and the checked-out branch
    /// (it asks first), `git branch -f` for another branch (it asks too).
    fn restore_from_reflog(
        &mut self,
        reference: String,
        id: Oid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if reference == "HEAD" || self.refs.head_branch.as_deref() == Some(reference.as_str()) {
            self.reset_to(id, ResetMode::Hard, window, cx);
            return;
        }
        let short = switchhead_git::short(&id);
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, reference) = (this.clone(), reference.clone());
            alert
                .confirm()
                .title(format!("Move {reference} to {short}?"))
                .description(
                    "The branch will point at this commit. Commits only it reached stay in \
                     its reflog for a while.",
                )
                .ok_text("Move Branch")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let (reference, rev) = (reference.clone(), id.to_string());
                    let _ = this.update(cx, |v, cx| {
                        v.run_op(
                            "Moving branch",
                            "Branch moved",
                            move |r, _, _| r.move_branch(&reference, &rev),
                            cx,
                        )
                    });
                    true
                })
        });
    }
}
