//! Linked worktrees and submodules, for the sidebar.
//!
//! Both come from `git` (`worktree list --porcelain`, `submodule status`): they are read
//! once per refresh, after the log, so the spawn is off the hot path. A repository without
//! a `.gitmodules` file skips the submodule spawn.

use std::path::PathBuf;

use crate::{Oid, Repo, Result};

/// A worktree of the repository (the main one included).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    /// Its folder.
    pub path: PathBuf,
    /// The commit it has checked out (`None` for a bare main repository).
    pub head: Option<Oid>,
    /// Its branch's short name, `None` when detached or bare.
    pub branch: Option<String>,
    /// The repository's own worktree (the first one listed).
    pub is_main: bool,
    /// The worktree this [`Repo`] was opened in.
    pub is_current: bool,
    /// Locked against pruning.
    pub locked: bool,
    /// Its folder is gone; `git worktree prune` would remove it.
    pub prunable: bool,
}

/// How a submodule's checkout compares with the commit the superproject records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmoduleState {
    /// Checked out at the recorded commit.
    Current,
    /// Not initialized (`git submodule update --init` would clone it).
    Uninitialized,
    /// Checked out at another commit.
    Modified,
    /// Has merge conflicts.
    Conflicted,
}

/// A submodule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submodule {
    /// Path relative to the superproject's root.
    pub path: PathBuf,
    /// The commit checked out (or recorded, when uninitialized).
    pub id: Option<Oid>,
    /// Checkout state.
    pub state: SubmoduleState,
}

/// Every worktree, the main one first.
pub fn list_worktrees(repo: &Repo) -> Result<Vec<Worktree>> {
    let text = repo
        .git()?
        .text(&["worktree", "list", "--porcelain", "-z"])?;
    let mut list = parse_worktrees(&text);
    let here = repo.root().canonicalize().ok();
    for w in &mut list {
        w.is_current = here.is_some() && w.path.canonicalize().ok() == here;
    }
    Ok(list)
}

/// Every submodule, by path. Empty (and no spawn) without `.gitmodules`.
pub fn list_submodules(repo: &Repo) -> Result<Vec<Submodule>> {
    if !repo.root().join(".gitmodules").is_file() {
        return Ok(Vec::new());
    }
    let text = repo.git()?.text(&["submodule", "status"])?;
    Ok(parse_submodules(&text))
}

/// Parse `worktree list --porcelain -z`: attributes end with NUL, records with an empty one.
fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = Vec::new();
    let mut cur: Option<Worktree> = None;
    for field in text.split('\0') {
        if field.is_empty() {
            out.extend(cur.take());
            continue;
        }
        let (key, value) = field.split_once(' ').unwrap_or((field, ""));
        if key == "worktree" {
            out.extend(cur.take());
            cur = Some(Worktree {
                path: PathBuf::from(value),
                head: None,
                branch: None,
                is_main: out.is_empty(),
                is_current: false,
                locked: false,
                prunable: false,
            });
            continue;
        }
        let Some(w) = cur.as_mut() else {
            continue;
        };
        match key {
            "HEAD" => w.head = Oid::from_hex(value.as_bytes()).ok(),
            "branch" => {
                w.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_owned(),
                );
            }
            "locked" => w.locked = true,
            "prunable" => w.prunable = true,
            _ => {}
        }
    }
    out.extend(cur);
    out
}

/// Parse `submodule status` lines: a state character, the id, the path, and an optional
/// ` (describe)`.
fn parse_submodules(text: &str) -> Vec<Submodule> {
    text.lines()
        .filter_map(|line| {
            let mut chars = line.chars();
            let state = match chars.next()? {
                ' ' => SubmoduleState::Current,
                '-' => SubmoduleState::Uninitialized,
                '+' => SubmoduleState::Modified,
                'U' => SubmoduleState::Conflicted,
                _ => return None,
            };
            let (id, rest) = chars.as_str().split_once(' ')?;
            let path = match rest.rfind(" (") {
                Some(i) if rest.ends_with(')') => &rest[..i],
                _ => rest,
            };
            Some(Submodule {
                path: PathBuf::from(path),
                id: Oid::from_hex(id.as_bytes()).ok(),
                state,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn parses_worktree_records() {
        let a = "1".repeat(40);
        let b = "2".repeat(40);
        let text = format!(
            "worktree /r\0HEAD {a}\0branch refs/heads/main\0\0\
             worktree /r-wt\0HEAD {b}\0detached\0locked\0\0\
             worktree /gone\0HEAD {b}\0branch refs/heads/feature/x\0prunable gitdir file points to non-existent location\0\0"
        );
        let w = parse_worktrees(&text);
        assert_eq!(w.len(), 3);
        assert!(w[0].is_main && w[0].branch.as_deref() == Some("main"));
        assert_eq!(w[0].head, Oid::from_hex(a.as_bytes()).ok());
        assert!(!w[1].is_main && w[1].branch.is_none() && w[1].locked);
        assert_eq!(w[2].branch.as_deref(), Some("feature/x"));
        assert!(w[2].prunable && !w[2].locked);
    }

    #[test]
    fn parses_submodule_lines() {
        let a = "a".repeat(40);
        let text =
            format!(" {a} libs/one (v1.0)\n-{a} libs/two\n+{a} my lib (heads/main)\nU{a} c\n");
        let s = parse_submodules(&text);
        let got: Vec<(&str, SubmoduleState)> = s
            .iter()
            .map(|s| (s.path.to_str().unwrap(), s.state))
            .collect();
        assert_eq!(
            got,
            [
                ("libs/one", SubmoduleState::Current),
                ("libs/two", SubmoduleState::Uninitialized),
                ("my lib", SubmoduleState::Modified),
                ("c", SubmoduleState::Conflicted),
            ]
        );
    }

    #[test]
    fn lists_worktrees_and_submodules() {
        let sub = TestRepo::new();
        sub.commit_file("s", "1", "sub");
        let t = TestRepo::new();
        t.commit_file("a", "1", "one");
        let wt = t.path().with_extension("wt");
        t.git(&["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()]);
        t.git(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &sub.path().to_string_lossy(),
            "dep",
        ]);
        let repo = Repo::open(t.path()).unwrap();
        let w = list_worktrees(&repo).unwrap();
        assert_eq!(w.len(), 2);
        assert!(w[0].is_main && w[0].is_current && w[0].branch.as_deref() == Some("main"));
        assert_eq!(w[1].branch.as_deref(), Some("side"));
        assert!(!w[1].is_current);
        assert_eq!(
            w[1].path.canonicalize().unwrap(),
            wt.canonicalize().unwrap()
        );
        let s = list_submodules(&repo).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].path, PathBuf::from("dep"));
        assert_eq!(s[0].state, SubmoduleState::Current);
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn no_gitmodules_means_no_submodules() {
        let t = TestRepo::new();
        t.commit_file("a", "1", "one");
        let repo = Repo::open(t.path()).unwrap();
        assert!(list_submodules(&repo).unwrap().is_empty());
    }
}
