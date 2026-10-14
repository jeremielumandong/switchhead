//! The refs sidebar's pure parts: branch names as a folder tree (split on `/`) and the
//! filter. The view flattens the tree into rows so the list stays virtualized.

use std::collections::HashSet;

/// One row of a name tree, in display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRow {
    /// A folder (`feature` for `feature/x`). `key` names it in the collapsed set.
    Folder {
        key: String,
        name: String,
        depth: usize,
        open: bool,
    },
    /// A name: its index in the input and the part after its folders.
    Leaf {
        index: usize,
        name: String,
        depth: usize,
    },
}

/// Lay `names` (sorted, so a folder's names are contiguous) out as a tree. Folder keys
/// are `scope` + `/` + the folder path, so sections with the same folder names do not
/// share their collapsed state. Rows under a collapsed folder are left out; with
/// `expand_all` (a filter is active) every folder is open.
pub fn tree(
    names: &[&str],
    scope: &str,
    collapsed: &HashSet<String>,
    expand_all: bool,
) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    // The open folder path, and how deep the first collapsed folder in it sits.
    let mut stack: Vec<&str> = Vec::new();
    let mut hidden_below: Option<usize> = None;
    for (index, full) in names.iter().enumerate() {
        let parts: Vec<&str> = full.split('/').collect();
        let (leaf, folders) = parts.split_last().unwrap_or((full, &[]));
        let common = stack
            .iter()
            .zip(folders)
            .take_while(|(a, b)| a == b)
            .count();
        stack.truncate(common);
        if hidden_below.is_some_and(|d| d >= common) {
            hidden_below = None;
        }
        for folder in &folders[common..] {
            let depth = stack.len();
            stack.push(folder);
            let key = format!("{scope}/{}", stack.join("/"));
            let open = expand_all || !collapsed.contains(&key);
            if hidden_below.is_none() {
                rows.push(TreeRow::Folder {
                    key,
                    name: (*folder).to_owned(),
                    depth,
                    open,
                });
                if !open {
                    hidden_below = Some(depth);
                }
            }
        }
        if hidden_below.is_none() {
            rows.push(TreeRow::Leaf {
                index,
                name: (*leaf).to_owned(),
                depth: stack.len(),
            });
        }
    }
    rows
}

/// The filter matches `text`: every word of the filter appears in it, ignoring case.
pub fn matches(text: &str, filter: &str) -> bool {
    let text = text.to_lowercase();
    filter
        .split_whitespace()
        .all(|w| text.contains(&w.to_lowercase()))
}

/// Why `name` cannot be a branch name, following `git check-ref-format --branch`
/// (`None`: it can).
pub fn branch_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("Enter a name");
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("Names cannot contain spaces");
    }
    if name
        .chars()
        .any(|c| matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return Some("Names cannot contain ~ ^ : ? * [ or \\");
    }
    if name.starts_with('-') {
        return Some("Names cannot start with -");
    }
    if name == "@" || name.contains("@{") || name.contains("..") {
        return Some("Names cannot contain .. or @{");
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Some("Folders need a name on both sides of /");
    }
    if name.ends_with('.') || name.ends_with(".lock") {
        return Some("Names cannot end with . or .lock");
    }
    if name
        .split('/')
        .any(|part| part.starts_with('.') || part.ends_with(".lock"))
    {
        return Some("No part of a name can start with . or end with .lock");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                TreeRow::Folder {
                    name, depth, open, ..
                } => format!(
                    "{}{name}/{}",
                    "  ".repeat(*depth),
                    if *open { "" } else { "+" }
                ),
                TreeRow::Leaf { name, depth, index } => {
                    format!("{}{name}#{index}", "  ".repeat(*depth))
                }
            })
            .collect()
    }

    const NAMES: [&str; 6] = [
        "feature-y",
        "feature/a",
        "feature/ui/b",
        "feature/ui/c",
        "main",
        "release/1.0",
    ];

    #[test]
    fn splits_names_into_folders() {
        let rows = tree(&NAMES, "b", &HashSet::new(), false);
        assert_eq!(
            show(&rows),
            [
                "feature-y#0",
                "feature/",
                "  a#1",
                "  ui/",
                "    b#2",
                "    c#3",
                "main#4",
                "release/",
                "  1.0#5",
            ]
        );
        let keys: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                TreeRow::Folder { key, .. } => Some(key.as_str()),
                TreeRow::Leaf { .. } => None,
            })
            .collect();
        assert_eq!(keys, ["b/feature", "b/feature/ui", "b/release"]);
    }

    #[test]
    fn collapsed_folders_hide_their_rows() {
        let collapsed = HashSet::from(["b/feature/ui".to_owned()]);
        assert_eq!(
            show(&tree(&NAMES, "b", &collapsed, false)),
            [
                "feature-y#0",
                "feature/",
                "  a#1",
                "  ui/+",
                "main#4",
                "release/",
                "  1.0#5",
            ]
        );
        let collapsed = HashSet::from(["b/feature".to_owned(), "b/feature/ui".to_owned()]);
        assert_eq!(
            show(&tree(&NAMES, "b", &collapsed, false)),
            ["feature-y#0", "feature/+", "main#4", "release/", "  1.0#5"]
        );
        // A filter opens everything.
        assert_eq!(tree(&NAMES, "b", &collapsed, true).len(), 9);
    }

    #[test]
    fn sibling_folders_after_a_collapsed_one_show() {
        let names = ["a/x/1", "a/y/2", "b"];
        let collapsed = HashSet::from(["r/a/x".to_owned()]);
        assert_eq!(
            show(&tree(&names, "r", &collapsed, false)),
            ["a/", "  x/+", "  y/", "    2#1", "b#2"]
        );
    }

    #[test]
    fn filter_matches_every_word() {
        assert!(matches("feature/Login-Form", "login"));
        assert!(matches("feature/Login-Form", "feat form"));
        assert!(!matches("feature/Login-Form", "feat main"));
        assert!(matches("anything", "  "));
    }

    #[test]
    fn checks_branch_names() {
        for ok in ["main", "feature/login-form", "v1.2", "fix_#12", "a/b/c"] {
            assert_eq!(branch_name_problem(ok), None, "{ok}");
        }
        for bad in [
            "",
            "has space",
            "a..b",
            "-x",
            "a/",
            "/a",
            "a//b",
            "x.lock",
            "a/.hidden",
            "x.",
            "what?",
            "a~1",
            "@",
            "a@{1}",
            "tab\t",
        ] {
            assert!(branch_name_problem(bad).is_some(), "{bad:?}");
        }
    }
}
