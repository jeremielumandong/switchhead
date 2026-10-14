//! Cheap change detection for a repository's `.git` folder.
//!
//! [`fingerprint`] hashes the size and modification time of the files git rewrites when
//! refs or the index change (HEAD, the index, packed refs, loose refs, the HEAD reflog,
//! the operation-in-progress markers). Callers poll it and reload when it changes: a
//! commit, checkout, fetch or rebase run from a terminal shows up without a watcher. Only
//! metadata is read; no file contents.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::time::SystemTime;

use crate::Repo;

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
