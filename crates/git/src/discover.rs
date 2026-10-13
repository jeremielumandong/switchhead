//! Finding repositories: the one a path is in, or every repository under a folder that is
//! not one itself ("open a projects folder").
//!
//! The folder scan walks directories in parallel, never descends into a repository it
//! found (its submodules are listed by the repository itself), skips build and dependency
//! folders, and stops at a depth limit. It only looks for a `.git` entry, so it costs one
//! `read_dir` per directory and opens nothing.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Default depth limit for [`scan`] (the folder itself is depth 0).
pub const DEFAULT_DEPTH: usize = 4;

/// Directory names never descended into.
const SKIP: &[&str] = &[
    "node_modules",
    "target",
    ".cargo",
    ".rustup",
    "vendor",
    "bower_components",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".gradle",
    ".m2",
    "build",
    "dist",
    "out",
    ".next",
    ".nuxt",
    ".cache",
    "Library",
    ".Trash",
    "Pods",
    "DerivedData",
];

/// What a path is, for opening it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Inside a repository; the work tree root (or the git dir of a bare repository).
    Repo(PathBuf),
    /// A folder that is not in a repository.
    Folder(PathBuf),
}

/// A repository found by [`scan`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Found {
    /// The work tree root.
    pub path: PathBuf,
    /// Its path relative to the scanned folder, `/`-separated, for grouping and display.
    pub rel: String,
    /// `.git` is a file: a linked worktree or a submodule checkout.
    pub linked: bool,
}

/// Whether `dir` has a `.git` entry (directory, or a file for worktrees). Returns
/// `Some(linked)`.
fn git_marker(dir: &Path) -> Option<bool> {
    let m = std::fs::symlink_metadata(dir.join(".git")).ok()?;
    if m.is_dir() {
        // A real repository has HEAD; an empty `.git` folder is not one.
        dir.join(".git/HEAD").exists().then_some(false)
    } else if m.is_file() {
        Some(true)
    } else {
        None
    }
}

/// Whether `dir` looks like a bare repository (`HEAD`, `objects/`, `refs/`).
fn is_bare(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

/// What `path` is: the repository it is inside of (walking up), or a plain folder.
pub fn classify(path: &Path) -> Target {
    let start = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut cur = Some(start.as_path());
    while let Some(dir) = cur {
        if git_marker(dir).is_some() {
            return Target::Repo(dir.to_path_buf());
        }
        if is_bare(dir) {
            return Target::Repo(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    Target::Folder(start)
}

fn rel_of(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every repository under `root` (not `root` itself), at most `max_depth` levels down,
/// sorted by relative path. Hidden directories are skipped except `.git` markers.
pub fn scan(root: &Path, max_depth: usize) -> Vec<Found> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let queue: Arc<Mutex<VecDeque<(PathBuf, usize)>>> =
        Arc::new(Mutex::new(VecDeque::from([(root.clone(), 0)])));
    let found: Arc<Mutex<Vec<Found>>> = Arc::default();
    // Directories handed out but not yet finished; the scan ends when the queue is empty
    // and nothing is in flight.
    let busy = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);
    std::thread::scope(|s| {
        for _ in 0..threads {
            let (queue, found, busy, root) = (&queue, &found, &busy, &root);
            s.spawn(move || {
                use std::sync::atomic::Ordering;
                loop {
                    let next = {
                        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                        let n = q.pop_front();
                        if n.is_some() {
                            busy.fetch_add(1, Ordering::SeqCst);
                        }
                        n
                    };
                    let Some((dir, depth)) = next else {
                        if busy.load(Ordering::SeqCst) == 0 {
                            return;
                        }
                        std::thread::yield_now();
                        continue;
                    };
                    let mut children = Vec::new();
                    if let Ok(entries) = std::fs::read_dir(&dir) {
                        for e in entries.flatten() {
                            let Ok(ft) = e.file_type() else { continue };
                            if !ft.is_dir() {
                                continue;
                            }
                            let name = e.file_name();
                            let name = name.to_string_lossy();
                            if name.starts_with('.') || SKIP.contains(&name.as_ref()) {
                                continue;
                            }
                            let path = e.path();
                            if let Some(linked) = git_marker(&path) {
                                found
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .push(Found {
                                        rel: rel_of(root, &path),
                                        path,
                                        linked,
                                    });
                            } else if depth + 1 < max_depth {
                                children.push((path, depth + 1));
                            }
                        }
                    }
                    if !children.is_empty() {
                        queue
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .extend(children);
                    }
                    busy.fetch_sub(1, Ordering::SeqCst);
                }
            });
        }
    });
    let mut out = Arc::try_unwrap(found)
        .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
        .unwrap_or_default();
    out.sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkrepo(p: &Path) {
        std::fs::create_dir_all(p.join(".git/refs")).unwrap();
        std::fs::write(p.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    }

    #[test]
    fn finds_nested_repositories_but_not_inside_them() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        mkrepo(&root.join("a"));
        mkrepo(&root.join("a/inner")); // inside a: not reported
        mkrepo(&root.join("group/b"));
        mkrepo(&root.join("group/deep/x/y/c")); // depth 5: beyond the default
        mkrepo(&root.join("node_modules/pkg"));
        mkrepo(&root.join(".hidden/d"));
        std::fs::create_dir_all(root.join("empty/.git")).unwrap(); // no HEAD
        std::fs::create_dir_all(root.join("wt")).unwrap();
        std::fs::write(root.join("wt/.git"), "gitdir: ../a/.git/worktrees/wt\n").unwrap();
        let found = scan(root, DEFAULT_DEPTH);
        let rels: Vec<(&str, bool)> = found.iter().map(|f| (f.rel.as_str(), f.linked)).collect();
        assert_eq!(rels, [("a", false), ("group/b", false), ("wt", true)]);
        let deeper = scan(root, 6);
        assert!(deeper.iter().any(|f| f.rel == "group/deep/x/y/c"));
    }

    #[test]
    fn classifies_paths() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        mkrepo(&root.join("r"));
        std::fs::create_dir_all(root.join("r/src/deep")).unwrap();
        assert_eq!(classify(&root.join("r/src/deep")), Target::Repo(root.join("r")));
        assert_eq!(classify(&root), Target::Folder(root.clone()));
    }
}
