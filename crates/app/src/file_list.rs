//! Rows of a changed-file list, flat or as a folder tree (flattened, so the list can be
//! virtualized). Folders with a single subfolder and no files are merged (`src/app`).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use switchhead_git::diff::FileChange;

/// One row of the file list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileRow {
    /// A folder (tree mode only).
    Dir {
        /// Folder path, relative to the repository (the key for collapsing).
        path: PathBuf,
        /// Shown name: one or more merged path components.
        name: String,
        /// Nesting depth.
        depth: u16,
        /// Expanded.
        open: bool,
        /// Files below it.
        files: usize,
    },
    /// A file: index into the change list.
    File {
        /// Index into the files.
        index: usize,
        /// Nesting depth.
        depth: u16,
    },
}

/// Flat list: every file at depth 0.
pub fn flat(files: &[FileChange]) -> Vec<FileRow> {
    (0..files.len())
        .map(|index| FileRow::File { index, depth: 0 })
        .collect()
}

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<usize>,
}

impl Node {
    fn count(&self) -> usize {
        self.files.len() + self.dirs.values().map(Node::count).sum::<usize>()
    }
}

/// Folder tree: folders first, then files, each sorted by name.
pub fn tree(files: &[FileChange], collapsed: &HashSet<PathBuf>) -> Vec<FileRow> {
    let mut root = Node::default();
    for (i, f) in files.iter().enumerate() {
        let mut node = &mut root;
        let parts: Vec<String> = f
            .path
            .parent()
            .map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        for part in parts {
            node = node.dirs.entry(part).or_default();
        }
        node.files.push(i);
    }
    let mut out = Vec::with_capacity(files.len() * 2);
    emit(&root, Path::new(""), 0, files, collapsed, &mut out);
    out
}

fn emit(
    node: &Node,
    at: &Path,
    depth: u16,
    files: &[FileChange],
    collapsed: &HashSet<PathBuf>,
    out: &mut Vec<FileRow>,
) {
    for (name, child) in &node.dirs {
        let mut name = name.clone();
        let mut path = at.join(&name);
        let mut child = child;
        while child.files.is_empty() && child.dirs.len() == 1 {
            let Some((sub, next)) = child.dirs.iter().next() else {
                break;
            };
            name = format!("{name}/{sub}");
            path = path.join(sub);
            child = next;
        }
        let open = !collapsed.contains(&path);
        out.push(FileRow::Dir {
            path: path.clone(),
            name,
            depth,
            open,
            files: child.count(),
        });
        if open {
            emit(child, &path, depth + 1, files, collapsed, out);
        }
    }
    let mut own = node.files.clone();
    own.sort_by(|a, b| files[*a].path.file_name().cmp(&files[*b].path.file_name()));
    out.extend(own.into_iter().map(|index| FileRow::File { index, depth }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchhead_git::diff::FileStatus;

    fn change(p: &str) -> FileChange {
        FileChange {
            path: p.into(),
            old_path: None,
            status: FileStatus::Modified,
            old_id: None,
            new_id: None,
            submodule: false,
        }
    }

    #[test]
    fn builds_a_compacted_tree() {
        let files: Vec<FileChange> = [
            "README.md",
            "crates/app/src/main.rs",
            "crates/app/src/ui.rs",
            "crates/git/src/lib.rs",
            "docs/a.md",
        ]
        .into_iter()
        .map(change)
        .collect();
        let rows = tree(&files, &HashSet::new());
        let shown: Vec<String> = rows
            .iter()
            .map(|r| match r {
                FileRow::Dir {
                    name, depth, files, ..
                } => format!("{depth}:{name}/ ({files})"),
                FileRow::File { index, depth } => {
                    format!("{depth}:{}", files[*index].path.display())
                }
            })
            .collect();
        assert_eq!(
            shown,
            [
                "0:crates/ (3)",
                "1:app/src/ (2)",
                "2:crates/app/src/main.rs",
                "2:crates/app/src/ui.rs",
                "1:git/src/ (1)",
                "2:crates/git/src/lib.rs",
                "0:docs/ (1)",
                "1:docs/a.md",
                "0:README.md",
            ]
        );
        let collapsed = HashSet::from([PathBuf::from("crates/app/src")]);
        let rows = tree(&files, &collapsed);
        assert_eq!(rows.len(), 7);
        assert!(matches!(&rows[1], FileRow::Dir { open: false, .. }));
        assert_eq!(flat(&files).len(), 5);
    }
}
