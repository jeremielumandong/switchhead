//! Opening a file's history and blame ([`FileView`]) in place of the graph, and closing
//! it or the reflog view.

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
            FileViewEvent::Close => this.close_overlay(cx),
            FileViewEvent::Reveal(id) => {
                let id = *id;
                this.close_overlay(cx);
                this.mode = Mode::Commits;
                this.reveal(id, cx);
            }
        });
        self.overlay = Some((view.into(), sub));
        self.reflog = None;
        cx.notify();
    }

    /// Close the file or reflog view: back to the graph or Local Changes.
    pub(super) fn close_overlay(&mut self, cx: &mut Context<Self>) {
        self.reflog = None;
        if self.overlay.take().is_some() {
            self.refocus = true;
        }
        cx.notify();
    }
}
