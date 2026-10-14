//! Opening a commit's file tree ([`TreeView`]) in place of the graph.

use gpui_kit::{AppContext as _, Context, Window};
use switchhead_git::Oid;

use super::RepoView;
use crate::tree_view::{TreeView, TreeViewEvent};

impl RepoView {
    /// Browse the files of `id`.
    pub(super) fn open_tree(&mut self, id: Oid, window: &mut Window, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        let summary = self
            .log
            .row_of(&id)
            .map(|row| self.log.summary(row).to_owned())
            .unwrap_or_default();
        let view = cx.new(|cx| TreeView::new(repo, id, summary, window, cx));
        let sub = cx.subscribe(&view, move |this, _, ev, cx| match ev {
            TreeViewEvent::Close => this.close_overlay(cx),
            TreeViewEvent::OpenFile { path, blame } => {
                this.open_file_view(path.clone(), Some(id), *blame, cx)
            }
        });
        self.reflog = None;
        self.overlay = Some((view.into(), sub));
        cx.notify();
    }

    /// Browse the files of the selected commit, or HEAD's when nothing is selected.
    pub(super) fn browse_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.selected.map(|row| self.log.id(row)).or(self.refs.head);
        if let Some(id) = id {
            self.open_tree(id, window, cx);
        }
    }
}
