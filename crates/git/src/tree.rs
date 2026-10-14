//! The files of a commit's tree, read in process, for browsing a repository as it was at
//! any commit.

use std::path::PathBuf;

use gix::bstr::ByteSlice as _;
use gix::traverse::tree::Recorder;

use crate::{GixExt as _, Oid, Repo, Result};

/// What a tree entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// An executable file.
    Executable,
    /// A symbolic link (its blob holds the target).
    Link,
    /// A submodule (its id is a commit in another repository).
    Submodule,
}

/// One file in a commit's tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// Path from the repository root.
    pub path: PathBuf,
    /// The blob (or, for a submodule, the commit) it points at.
    pub id: Oid,
    /// File, executable, link or submodule.
    pub kind: EntryKind,
}

/// Every file in `commit`'s tree (folders are implied by the paths), sorted by path.
pub fn list(repo: &Repo, commit: Oid) -> Result<Vec<TreeEntry>> {
    let started = std::time::Instant::now();
    let r = repo.local();
    let tree = r.find_commit(commit).gix()?.tree().gix()?;
    let mut rec = Recorder::default();
    tree.traverse().breadthfirst(&mut rec).gix()?;
    let mut out: Vec<TreeEntry> = rec
        .records
        .into_iter()
        .filter_map(|e| {
            let kind = if e.mode.is_tree() {
                return None;
            } else if e.mode.is_commit() {
                EntryKind::Submodule
            } else if e.mode.is_link() {
                EntryKind::Link
            } else if e.mode.is_executable() {
                EntryKind::Executable
            } else {
                EntryKind::File
            };
            // Non-UTF-8 names fail on Windows; show them lossily rather than drop them.
            let path = gix::path::from_bstr(e.filepath.as_bstr())
                .map(|p| p.into_owned())
                .unwrap_or_else(|_| PathBuf::from(e.filepath.to_string()));
            Some(TreeEntry {
                path,
                id: e.oid,
                kind,
            })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        files = out.len(),
        "tree list"
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn lists_files_of_any_commit() {
        let t = TestRepo::new();
        let first = t.commit_file("src/a.rs", "a\n", "first");
        t.write("README", "hi\n");
        t.write("src/sub/b.rs", "b\n");
        t.git(&["add", "."]);
        t.git(&["commit", "-qm", "second"]);
        let repo = Repo::open(t.path()).unwrap();

        let head = repo.resolve("HEAD").unwrap();
        let files = list(&repo, head).unwrap();
        let paths: Vec<_> = files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, ["README", "src/a.rs", "src/sub/b.rs"]);
        assert!(files.iter().all(|f| f.kind == EntryKind::File));
        assert_eq!(crate::diff::read_blob(&repo, files[0].id).unwrap(), b"hi\n");

        let old = list(&repo, first.parse().unwrap()).unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].path, PathBuf::from("src/a.rs"));
    }
}
