//! Opening a file's history and blame ([`FileView`]) in place of the graph.

use std::path::PathBuf;

use gpui_kit::{AppContext as _, Context};
use switchhead_git::Oid;

use super::{Mode, RepoView};
use crate::file_view::{FileView, FileViewEvent, Tab};

impl RepoView {
    /// Show `path`'s history or blame at `rev` (`None`: the work tree / HEAD).
    pub(super) fn open_file_view(
        &mut self,
        path: PathBuf,
        rev: Option<Oid>,
        blame: bool,
        cx: &mut Context<Self>,
    ) {
        let repo = self.repo.clone();
        let tab = if blame { Tab::Blame } else { Tab::History };
        let view = cx.new(|cx| FileView::new(repo, path, rev, tab, cx));
        let sub = cx.subscribe(&view, |this, _, ev, cx| match ev {
            FileViewEvent::Close => this.close_file_view(cx),
            FileViewEvent::Reveal(id) => {
                let id = *id;
                this.close_file_view(cx);
                this.mode = Mode::Commits;
                this.reveal(id, cx);
            }
        });
        self.file_view = Some((view, sub));
        cx.notify();
    }

    /// Back to the graph or Local Changes.
    pub(super) fn close_file_view(&mut self, cx: &mut Context<Self>) {
        if self.file_view.take().is_some() {
            self.refocus = true;
        }
        cx.notify();
    }
}
