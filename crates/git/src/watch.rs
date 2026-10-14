//! Change detection for a repository: native file events plus a cheap `.git` check.
//!
//! [`Watcher`] listens to the OS's file events (inotify, FSEvents, ReadDirectoryChangesW
//! through `notify`) on the work tree and the git dir, and reports, debounced, whether
//! something in `.git` or in the work tree changed. [`fingerprint`] then tells what in
//! `.git` changed: it hashes the size and modification time of the files git rewrites when
//! refs or the index change (HEAD, the index, packed refs, loose refs, the HEAD reflog,
//! the operation-in-progress markers). Callers also poll it, so a commit, checkout or
//! fetch from a terminal shows up when events are unavailable. Only metadata is read.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use notify::event::{AccessKind, AccessMode, EventKind};
use notify::{RecursiveMode, Watcher as _};

use crate::{Error, Repo, Result};

/// What a burst of file events touched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Something in the git dir (refs, index, HEAD...): compare [`fingerprint`]s.
    pub git: bool,
    /// A file in the work tree: status may differ.
    pub work_tree: bool,
}

/// Events closer together than this are reported once.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// A steady stream of events (a build writing files) is still reported this often.
const MAX_DELAY: Duration = Duration::from_secs(1);

/// The folders a [`Watcher`] tells apart, as given and canonicalized (FSEvents reports
/// canonical paths, e.g. `/private/var/...` for `/var/...`).
#[derive(Debug)]
struct Dirs {
    git: Vec<PathBuf>,
    work_tree: Vec<PathBuf>,
}

impl Dirs {
    fn new(git: &[&Path], work_tree: Option<&Path>) -> Self {
        let both = |p: &Path| {
            let mut v = vec![p.to_path_buf()];
            if let Ok(c) = p.canonicalize()
                && c != p
            {
                v.push(c);
            }
            v
        };
        Self {
            git: git.iter().flat_map(|p| both(p)).collect(),
            work_tree: work_tree.map(both).unwrap_or_default(),
        }
    }

    /// What a change at `path` means, or `None` for noise (object writes, lock files).
    fn classify(&self, path: &Path) -> Option<Changes> {
        if let Some(rel) = self.git.iter().find_map(|g| path.strip_prefix(g).ok()) {
            let noise = rel.starts_with("objects") || rel.extension().is_some_and(|e| e == "lock");
            return (!noise).then_some(Changes {
                git: true,
                work_tree: false,
            });
        }
        self.work_tree
            .iter()
            .any(|w| path.starts_with(w))
            .then_some(Changes {
                git: false,
                work_tree: true,
            })
    }
}

/// Whether an event kind can mean a change. Opening and reading files (git status reading
/// the work tree, the app reading a diff) are reported as access events and are not.
fn is_change(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// Watches a repository's work tree and git dir with native file events; stops when
/// dropped.
pub struct Watcher {
    _inner: notify::RecommendedWatcher,
}

impl Watcher {
    /// Start watching `repo`. `on_change` runs on a watcher thread after each debounced
    /// burst of events. Fails when the OS refuses the watch (on Linux, too many folders for
    /// `fs.inotify.max_user_watches`); callers then rely on polling.
    pub fn start(repo: &Repo, on_change: impl Fn(Changes) + Send + 'static) -> Result<Self> {
        let started = Instant::now();
        let git_dir = repo.git_dir().to_path_buf();
        let common = repo.local().common_dir().to_path_buf();
        let work_tree = repo.has_worktree().then(|| repo.root().to_path_buf());
        let dirs = Dirs::new(&[&git_dir, &common], work_tree.as_deref());

        let (tx, rx) = mpsc::channel::<Changes>();
        let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else {
                return;
            };
            if !is_change(&ev.kind) {
                return;
            }
            let mut c = Changes::default();
            for p in &ev.paths {
                if let Some(k) = dirs.classify(p) {
                    c.git |= k.git;
                    c.work_tree |= k.work_tree;
                }
            }
            if c != Changes::default() {
                let _ = tx.send(c);
            }
        })
        .map_err(watch_error)?;

        // The work tree holds `.git` for an ordinary repository; a linked worktree's git
        // dir and the common dir live elsewhere and are watched on their own.
        let mut roots: Vec<&Path> = Vec::new();
        if let Some(w) = &work_tree {
            roots.push(w);
        }
        for d in [&git_dir, &common] {
            if !roots.iter().any(|r| d.starts_with(r)) {
                roots.push(d);
            }
        }
        for r in &roots {
            inner
                .watch(r, RecursiveMode::Recursive)
                .map_err(watch_error)?;
        }

        std::thread::Builder::new()
            .name("switchhead-watch".into())
            .spawn(move || debounce(&rx, on_change))?;
        tracing::debug!(
            ms = started.elapsed().as_millis() as u64,
            repo = repo.name(),
            "watching"
        );
        Ok(Self { _inner: inner })
    }
}

fn watch_error(e: notify::Error) -> Error {
    Error::Unsupported(format!("cannot watch the repository for changes: {e}"))
}

/// Merge events that arrive within [`DEBOUNCE`] of each other (at most [`MAX_DELAY`]
/// after the first) and report each burst once. Ends when the watcher is dropped.
fn debounce(rx: &mpsc::Receiver<Changes>, on_change: impl Fn(Changes)) {
    while let Ok(first) = rx.recv() {
        let mut acc = first;
        let deadline = Instant::now() + MAX_DELAY;
        loop {
            let wait = DEBOUNCE.min(deadline.saturating_duration_since(Instant::now()));
            match rx.recv_timeout(wait) {
                Ok(c) => {
                    acc.git |= c.git;
                    acc.work_tree |= c.work_tree;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    on_change(acc);
                    return;
                }
            }
        }
        on_change(acc);
    }
}

/// What changed between two [`Fingerprint`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    /// HEAD, refs, reflogs and operation markers: the graph and sidebar need a reload.
    pub refs: u64,
    /// The index: staged changes (and status) may differ.
    pub index: u64,
}

/// Files in the git dir (per worktree) whose change means the refs view is stale.
const GIT_DIR_FILES: &[&str] = &[
    "HEAD",
    "ORIG_HEAD",
    "FETCH_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "logs/HEAD",
    "rebase-merge",
    "rebase-apply",
];

/// Files and folders in the common dir shared by all worktrees.
const COMMON_FILES: &[&str] = &["packed-refs", "logs/refs/stash"];

/// Hash the metadata of `repo`'s refs and index.
pub fn fingerprint(repo: &Repo) -> Fingerprint {
    let git_dir = repo.git_dir();
    let common = repo.local().common_dir().to_path_buf();
    let mut refs = DefaultHasher::new();
    for f in GIT_DIR_FILES {
        stat(&git_dir.join(f), &mut refs);
    }
    for f in COMMON_FILES {
        stat(&common.join(f), &mut refs);
    }
    walk(&common.join("refs"), &mut refs);
    let mut index = DefaultHasher::new();
    stat(&git_dir.join("index"), &mut index);
    Fingerprint {
        refs: refs.finish(),
        index: index.finish(),
    }
}

/// Hash one path's existence, size and modification time.
fn stat(path: &Path, h: &mut DefaultHasher) {
    match std::fs::metadata(path) {
        Ok(m) => {
            true.hash(h);
            m.len().hash(h);
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .hash(h);
        }
        Err(_) => false.hash(h),
    }
}

/// Hash every loose ref under `dir` (names and metadata), in a stable order.
fn walk(dir: &Path, h: &mut DefaultHasher) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        e.file_name().hash(h);
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&path, h),
            _ => stat(&path, h),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn classifies_paths() {
        let dirs = Dirs::new(&[Path::new("/r/.git")], Some(Path::new("/r")));
        let git = Some(Changes {
            git: true,
            work_tree: false,
        });
        let tree = Some(Changes {
            git: false,
            work_tree: true,
        });
        assert_eq!(dirs.classify(Path::new("/r/.git/HEAD")), git);
        assert_eq!(dirs.classify(Path::new("/r/.git/refs/heads/main")), git);
        assert_eq!(dirs.classify(Path::new("/r/.git/index")), git);
        assert_eq!(dirs.classify(Path::new("/r/.git/index.lock")), None);
        assert_eq!(dirs.classify(Path::new("/r/.git/objects/ab/cdef")), None);
        assert_eq!(dirs.classify(Path::new("/r/src/main.rs")), tree);
        assert_eq!(dirs.classify(Path::new("/r/.gitignore")), tree);
        assert_eq!(dirs.classify(Path::new("/elsewhere/x")), None);
        assert!(!is_change(&EventKind::Access(AccessKind::Open(
            AccessMode::Any
        ))));
        assert!(is_change(&EventKind::Access(AccessKind::Close(
            AccessMode::Write
        ))));
    }

    /// Wait for a report that satisfies `want`, up to a few seconds.
    fn wait_for(rx: &mpsc::Receiver<Changes>, want: impl Fn(Changes) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(c) if want(c) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    #[test]
    fn reports_work_tree_and_git_changes() {
        let t = TestRepo::new();
        t.commit_file("a.txt", "a\n", "first");
        let repo = Repo::open(t.path()).unwrap();
        let (tx, rx) = mpsc::channel();
        let _w = Watcher::start(&repo, move |c| {
            let _ = tx.send(c);
        })
        .unwrap();

        t.write("a.txt", "a\nb\n");
        assert!(
            wait_for(&rx, |c| c.work_tree),
            "an edit is a work tree change"
        );

        // Drain the edit's reports, then commit from "a terminal".
        while rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
        t.git(&["commit", "-qam", "second"]);
        assert!(wait_for(&rx, |c| c.git), "a commit is a git change");

        // Reading files is not a change, nor is status as the app runs it (without optional
        // locks; a plain `git status` may rewrite the index, which is a real change).
        while rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
        let _ = std::fs::read(t.path().join("a.txt")).unwrap();
        t.git(&["--no-optional-locks", "status", "--porcelain"]);
        assert!(
            rx.recv_timeout(Duration::from_millis(600)).is_err(),
            "reads are not reported"
        );
    }

    #[test]
    fn refs_and_index_changes_are_told_apart() {
        let t = TestRepo::new();
        t.commit_file("a.txt", "a\n", "first");
        let repo = Repo::open(t.path()).unwrap();
        let start = fingerprint(&repo);
        assert_eq!(fingerprint(&repo), start, "nothing changed");

        // Staging touches only the index.
        t.write("a.txt", "a\nb\n");
        t.git(&["add", "a.txt"]);
        let staged = fingerprint(&repo);
        assert_eq!(staged.refs, start.refs);
        assert_ne!(staged.index, start.index);

        // A new branch is a new loose ref.
        t.git(&["branch", "topic"]);
        let branched = fingerprint(&repo);
        assert_ne!(branched.refs, staged.refs);

        // Checking it out rewrites HEAD.
        t.git(&["checkout", "-q", "topic"]);
        assert_ne!(fingerprint(&repo).refs, branched.refs);
    }
}
