//! Git LFS in a repository tab: fetch, pull and prune objects (with progress in the
//! status bar), and file locks (the sidebar's LFS Locks section, Lock / Unlock in Local
//! Changes). Locks are read from the server once LFS is detected and after each change.

use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::{Context, Window};
use switchhead_git::lfs::Lock;

use super::RepoView;

/// What the tab knows about LFS.
#[derive(Default)]
pub struct LfsState {
    /// The repository uses LFS (`.gitattributes` or a local store).
    pub used: bool,
    /// The server's locks, once read.
    pub locks: Vec<Lock>,
    /// Read the locks again at the next detection (initially, and after a change).
    dirty: bool,
    loading: bool,
}

impl LfsState {
    /// A fresh state: locks are read once LFS is detected.
    pub fn new() -> Self {
        Self {
            dirty: true,
            ..Self::default()
        }
    }

    /// The lock on `path`, if any.
    pub fn lock_on(&self, path: &std::path::Path) -> Option<&Lock> {
        self.locks.iter().find(|l| l.path == path)
    }
}

impl RepoView {
    /// The walk found whether the repository uses LFS.
    pub(super) fn set_lfs_used(&mut self, used: bool, cx: &mut Context<Self>) {
        let was = self.lfs.used;
        self.lfs.used = used;
        if used && self.lfs.dirty {
            self.load_lfs_locks(cx);
        }
        if was != used {
            self.rebuild_side();
            self.sync_lfs_changes(cx);
        }
    }

    /// Read the locks from the server in the background.
    pub fn load_lfs_locks(&mut self, cx: &mut Context<Self>) {
        if self.lfs.loading {
            return;
        }
        self.lfs.dirty = false;
        self.lfs.loading = true;
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move {
            let started = std::time::Instant::now();
            let res = repo.lfs_locks();
            tracing::debug!(ms = started.elapsed().as_millis() as u64, "lfs locks");
            res
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                v.lfs.loading = false;
                match res {
                    Ok(locks) => v.lfs.locks = locks,
                    // No lock API on the server, offline, or no credentials: no section.
                    Err(e) => {
                        tracing::debug!(error = %e, "lfs locks unavailable");
                        v.lfs.locks.clear();
                    }
                }
                v.rebuild_side();
                v.sync_lfs_changes(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Tell Local Changes whether LFS is used and what is locked.
    fn sync_lfs_changes(&mut self, cx: &mut Context<Self>) {
        let locked = self
            .lfs
            .used
            .then(|| self.lfs.locks.iter().map(|l| l.path.clone()).collect());
        self.changes.update(cx, |c, cx| c.set_lfs(locked, cx));
    }

    /// `git lfs fetch`: download the objects the current branch needs, or (`all`) the
    /// objects of every commit, which older versions in diffs need.
    pub fn lfs_fetch(&mut self, all: bool, cx: &mut Context<Self>) {
        let args: &'static [&'static str] = if all { &["fetch", "--all"] } else { &["fetch"] };
        self.run_op(
            "Fetching LFS objects",
            "LFS objects fetched",
            move |r, cancel, on_line| r.lfs(args, cancel, |l| on_line(l)),
            cx,
        );
    }

    /// `git lfs pull`: download and check out the current branch's objects.
    pub fn lfs_pull(&mut self, cx: &mut Context<Self>) {
        self.run_op(
            "Pulling LFS objects",
            "LFS objects pulled",
            |r, cancel, on_line| r.lfs(&["pull"], cancel, |l| on_line(l)),
            cx,
        );
    }

    /// `git lfs prune`: delete old local objects that are already on the server.
    pub fn lfs_prune(&mut self, cx: &mut Context<Self>) {
        self.run_op(
            "Pruning LFS objects",
            "LFS objects pruned",
            |r, cancel, on_line| r.lfs(&["prune"], cancel, |l| on_line(l)),
            cx,
        );
    }

    /// Lock `path` on the server.
    pub fn lfs_lock(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.lfs.dirty = true;
        self.run_op(
            "Locking",
            "File locked",
            move |r, _, _| r.lfs_lock(&path),
            cx,
        );
    }

    /// Unlock `path` (git refuses when someone else holds the lock).
    pub fn lfs_unlock(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.run_unlock(path, false, cx);
    }

    /// Break the lock on `path`, whoever holds it, after asking.
    pub fn lfs_force_unlock(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let owner = self
            .lfs
            .lock_on(&path)
            .and_then(|l| l.owner.clone())
            .unwrap_or_else(|| "someone".into());
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, path) = (this.clone(), path.clone());
            alert
                .confirm()
                .title("Break this lock?")
                .description(format!(
                    "{} is locked by {owner}. Breaking the lock lets others change the file \
                     while {owner} may still be editing it.",
                    path.display()
                ))
                .ok_text("Force Unlock")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let path = path.clone();
                    let _ = this.update(cx, |v, cx| v.run_unlock(path, true, cx));
                    true
                })
        });
    }

    fn run_unlock(&mut self, path: PathBuf, force: bool, cx: &mut Context<Self>) {
        self.lfs.dirty = true;
        let id = self.lfs.lock_on(&path).map(|l| l.id.clone());
        self.run_op(
            "Unlocking",
            "File unlocked",
            move |r, _, _| r.lfs_unlock(&path, id.as_deref(), force),
            cx,
        );
    }
}
