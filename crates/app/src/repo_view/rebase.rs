//! Interactive rebase from the graph: opens [`RebaseEditor`] in place of the graph and
//! runs the plan it hands back.

use gpui_kit::{AppContext as _, Context, Window};
use switchhead_git::rebase::{self, RebaseOutcome};
use switchhead_git::{Error, Oid};

use super::RepoView;
use crate::rebase_editor::{RebaseEditor, RebaseEditorEvent};

impl RepoView {
    /// Edit the commits after `base` (the selected commit) and rebase them.
    pub(super) fn open_rebase_editor(
        &mut self,
        base: Oid,
        summary: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let branch = self.head_label();
        let repo = self.repo.clone();
        let editor = cx.new(|cx| RebaseEditor::new(repo, base, &summary, branch, window, cx));
        let sub = cx.subscribe_in(&editor, window, |this, _, ev, window, cx| match ev {
            RebaseEditorEvent::Cancel => this.close_rebase_editor(window, cx),
            RebaseEditorEvent::Start {
                base,
                items,
                autostash,
            } => {
                let (base, items, autostash) = (*base, items.clone(), *autostash);
                this.close_rebase_editor(window, cx);
                this.run_op(
                    "Rebasing",
                    "Rebased",
                    move |r, _, on_line| match rebase::run(r, Some(base), &items, autostash)? {
                        RebaseOutcome::Done => Ok(()),
                        RebaseOutcome::Stopped { message } => {
                            for l in message.lines() {
                                on_line(l);
                            }
                            Err(Error::Unsupported(stop_reason(&message)))
                        }
                    },
                    cx,
                );
            }
        });
        self.rebase = Some((editor, sub));
        cx.notify();
    }

    pub(super) fn close_rebase_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rebase = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }
}

/// The status-bar line for a rebase that stopped: the conflict, or where it paused.
fn stop_reason(message: &str) -> String {
    // git rewrites its "Rebasing (1/2)" counter with `\r`.
    let lines = || {
        message
            .split(['\n', '\r'])
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("Rebasing ("))
    };
    let line = lines()
        .find(|l| l.starts_with("CONFLICT"))
        .or_else(|| lines().find(|l| l.starts_with("Stopped at")))
        .or_else(|| lines().find(|l| l.starts_with("error:")))
        .or_else(|| lines().next())
        .unwrap_or("stopped");
    format!("Rebase paused: {line}")
}

#[cfg(test)]
mod tests {
    use super::stop_reason;

    #[test]
    fn says_why_the_rebase_stopped() {
        assert_eq!(
            stop_reason("Auto-merging a.txt\nCONFLICT (content): Merge conflict in a.txt\n"),
            "Rebase paused: CONFLICT (content): Merge conflict in a.txt"
        );
        assert_eq!(
            stop_reason("Stopped at 1234567...  add b\nYou can amend the commit now"),
            "Rebase paused: Stopped at 1234567...  add b"
        );
        assert_eq!(
            stop_reason("Rebasing (1/2)\rerror: could not apply 7203feb... edit\nhint: x"),
            "Rebase paused: error: could not apply 7203feb... edit"
        );
    }
}
