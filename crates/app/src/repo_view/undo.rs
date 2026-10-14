//! Undo (Ctrl/Cmd+Z): put back what the last hard reset or discard changed, from the
//! snapshot taken before it ([`switchhead_git::undo`]).

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::{Context, Window};
use switchhead_git::undo::{self, Snapshot};

use super::{OpStatus, RepoView};

impl RepoView {
    /// Find the newest snapshot and, after asking, restore it.
    pub(super) fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let repo = self.repo.clone();
        let task = cx
            .background_executor()
            .spawn(async move { undo::latest(&repo) });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            let _ = this.update_in(cx, |v, window, cx| match res {
                Ok(Some(s)) => v.confirm_undo(s, window, cx),
                Ok(None) => {
                    v.op = Some(OpStatus::Done("Nothing to undo".into()));
                    cx.notify();
                }
                Err(e) => {
                    v.op = Some(OpStatus::Failed(e.to_string().into()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn confirm_undo(&mut self, s: Snapshot, window: &mut Window, cx: &mut Context<Self>) {
        let now = chrono::Local::now().timestamp();
        let when = super::fmt_date(s.time, now);
        let description = if s.whole_tree() {
            let at = s
                .head
                .map(|h| format!(" to {}", switchhead_git::short(&h)))
                .unwrap_or_default();
            format!(
                "{} goes back{at}, and the index and files return to how they were \
                 ({when}). Changes made since are replaced.",
                s.branch.as_deref().unwrap_or("HEAD")
            )
        } else {
            let (files, verb) = match s.paths.len() {
                1 => (s.paths[0].display().to_string(), "returns to how it was"),
                n => (format!("{n} files"), "return to how they were"),
            };
            format!("{files} {verb} ({when}). Changes made since are replaced.")
        };
        let this = cx.entity().downgrade();
        let s = std::rc::Rc::new(s);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, s) = (this.clone(), s.clone());
            alert
                .confirm()
                .title(format!("Undo “{}”?", s.label))
                .description(description.clone())
                .ok_text("Undo")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let s = (*s).clone();
                    let _ = this.update(cx, |v, cx| {
                        v.run_op("Undoing", "Undone", move |r, _, _| undo::restore(r, &s), cx);
                    });
                    true
                })
        });
    }
}
