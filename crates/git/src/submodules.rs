//! Submodule changes and operations: adding one, updating (cloning and checking out the
//! recorded commits), and the commits between two submodule commits, for diffs.
//!
//! Listing lives in [`crate::worktrees`]. Adding and updating run `git submodule` (they
//! clone and fetch, so credentials and hooks apply); the commit list is a `git log` in the
//! submodule's own clone, run only when a submodule's diff is shown.

use std::path::Path;

use crate::cli::{Cancel, Git};
use crate::{Oid, Repo, Result};

/// Most commits a submodule diff lists on each side.
const MAX_COMMITS: usize = 500;

/// A commit between a submodule's old and new commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmoduleCommit {
    /// The commit.
    pub id: Oid,
    /// Its summary line.
    pub summary: String,
    /// Reachable from the new commit only (else from the old one only: it was removed,
    /// as when the submodule went back or to another branch).
    pub added: bool,
}

/// What changed in a submodule between two commits (and in its work tree).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubmoduleDiff {
    /// The commit before (`None`: the submodule was added).
    pub old: Option<Oid>,
    /// The commit after (`None`: the submodule was removed, or is not cloned).
    pub new: Option<Oid>,
    /// Commits added and removed, newest first.
    pub commits: Vec<SubmoduleCommit>,
    /// More commits than [`SubmoduleDiff::commits`] holds.
    pub truncated: bool,
    /// Why the commits could not be listed (not cloned, commits not fetched).
    pub note: Option<String>,
    /// The submodule's work tree has changes to tracked files.
    pub modified: bool,
    /// The submodule's work tree has untracked files.
    pub untracked: bool,
}

/// The `git submodule add` arguments: `url` cloned at `path`, tracking `branch` if given.
pub fn add_args<'a>(url: &'a str, path: &'a str, branch: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec!["submodule", "add", "--progress"];
    if let Some(b) = branch {
        args.extend(["-b", b]);
    }
    args.extend(["--", url, path]);
    args
}

/// The `git submodule update` arguments: clone missing ones (`init`), and their own
/// submodules (`recursive`); every submodule when `paths` is empty.
pub fn update_args<'a>(paths: &'a [&'a str], init: bool, recursive: bool) -> Vec<&'a str> {
    let mut args = vec!["submodule", "update", "--progress"];
    if init {
        args.push("--init");
    }
    if recursive {
        args.push("--recursive");
    }
    if !paths.is_empty() {
        args.push("--");
        args.extend(paths);
    }
    args
}

/// The path a submodule cloned from `url` gets by default: the URL's last component
/// without `.git` (what `git submodule add` picks).
pub fn default_path(url: &str) -> String {
    let url = url.trim().trim_end_matches(['/', '\\']);
    let last = url.rsplit(['/', '\\', ':']).next().unwrap_or(url);
    last.strip_suffix(".git").unwrap_or(last).to_owned()
}

impl Repo {
    /// Add the submodule at `url` as `path`, tracking `branch` if given.
    pub fn submodule_add(
        &self,
        url: &str,
        path: &str,
        branch: Option<&str>,
        cancel: &Cancel,
        on_line: impl FnMut(&str),
    ) -> Result<()> {
        self.git()?
            .run_streaming(&add_args(url, path, branch), cancel, on_line)?;
        Ok(())
    }

    /// Check out the commits the superproject records (`git submodule update`), cloning
    /// missing ones when `init`; `paths` empty means every submodule.
    pub fn submodule_update(
        &self,
        paths: &[&str],
        init: bool,
        recursive: bool,
        cancel: &Cancel,
        on_line: impl FnMut(&str),
    ) -> Result<()> {
        self.git()?
            .run_streaming(&update_args(paths, init, recursive), cancel, on_line)?;
        Ok(())
    }

    /// The commit the submodule at `path` has checked out (`None`: not cloned).
    pub fn submodule_head(&self, path: &Path) -> Option<Oid> {
        let dir = self.root().join(path);
        if !dir.join(".git").exists() {
            return None;
        }
        let text = Git::new(dir).ok()?.text(&["rev-parse", "HEAD"]).ok()?;
        Oid::from_hex(text.trim().as_bytes()).ok()
    }

    /// The commits between `old` and `new` in the submodule at `path`, read from its own
    /// clone. Never fails: what cannot be listed becomes [`SubmoduleDiff::note`].
    pub fn submodule_diff(&self, path: &Path, old: Option<Oid>, new: Option<Oid>) -> SubmoduleDiff {
        let mut d = SubmoduleDiff {
            old,
            new,
            ..SubmoduleDiff::default()
        };
        let range = match (old, new) {
            (Some(o), Some(n)) if o == n => return d,
            (Some(o), Some(n)) => format!("{o}...{n}"),
            // Added: its history up to the new commit; removed: nothing to list.
            (None, Some(n)) => n.to_string(),
            _ => return d,
        };
        let dir = self.root().join(path);
        if !dir.join(".git").exists() {
            d.note = Some("The submodule is not cloned: update it to see its commits.".into());
            return d;
        }
        let limit = (MAX_COMMITS + 1).to_string();
        let res = Git::new(dir).and_then(|g| {
            g.text(&[
                "log",
                "--left-right",
                "--format=%m%H %s",
                "-n",
                &limit,
                &range,
                "--",
            ])
        });
        match res {
            Ok(text) => {
                d.commits = parse_log(&text);
                d.truncated = d.commits.len() > MAX_COMMITS;
                d.commits.truncate(MAX_COMMITS);
            }
            Err(_) => {
                d.note = Some(
                    "These commits are not in the submodule's clone: fetch in the submodule \
                     to see them."
                        .into(),
                );
            }
        }
        d
    }
}

/// Parse `log --left-right --format=%m%H %s`: `>` marks the right side (added), `<` the
/// left (removed); without `--left-right` symmetry every line is `>`.
fn parse_log(text: &str) -> Vec<SubmoduleCommit> {
    text.lines()
        .filter_map(|line| {
            let mut chars = line.chars();
            let added = match chars.next()? {
                '>' => true,
                '<' => false,
                _ => return None,
            };
            let rest = chars.as_str();
            let (id, summary) = rest.split_once(' ').unwrap_or((rest, ""));
            Some(SubmoduleCommit {
                id: Oid::from_hex(id.as_bytes()).ok()?,
                summary: summary.to_owned(),
                added,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn builds_arguments() {
        assert_eq!(
            add_args("git@h:o/lib.git", "libs/lib", Some("main")),
            [
                "submodule",
                "add",
                "--progress",
                "-b",
                "main",
                "--",
                "git@h:o/lib.git",
                "libs/lib"
            ]
        );
        assert_eq!(
            update_args(&[], true, true),
            ["submodule", "update", "--progress", "--init", "--recursive"]
        );
        assert_eq!(
            update_args(&["a b"], false, false),
            ["submodule", "update", "--progress", "--", "a b"]
        );
        assert_eq!(default_path("https://h/o/lib.git"), "lib");
        assert_eq!(default_path("git@h:lib.git/"), "lib");
        assert_eq!(default_path("../other"), "other");
    }

    #[test]
    fn parses_left_right_log() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let c = parse_log(&format!(">{a} Add x\n<{b} Old work\nnoise\n"));
        assert_eq!(c.len(), 2);
        assert!(c[0].added && c[0].summary == "Add x");
        assert!(!c[1].added && c[1].summary == "Old work");
    }

    #[test]
    fn diffs_and_updates_a_submodule() {
        let sub = TestRepo::new();
        let one = Oid::from_hex(sub.commit_file("s", "1", "one").as_bytes()).unwrap();
        let t = TestRepo::new();
        t.commit_file("a", "1", "root");
        t.git(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &sub.path().to_string_lossy(),
            "dep",
        ]);
        t.git(&["commit", "-qm", "Add dep"]);
        let repo = Repo::open(t.path()).unwrap();
        assert_eq!(repo.submodule_head(Path::new("dep")), Some(one));

        // Two new commits in the submodule's clone: the diff lists them as added.
        let dep = t.path().join("dep");
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(["-c", "user.name=T", "-c", "user.email=t@t", "-C"])
                .arg(&dep)
                .args(args)
                .status()
                .unwrap();
            assert!(st.success());
        };
        git(&["commit", "-q", "--allow-empty", "-m", "two"]);
        git(&["commit", "-q", "--allow-empty", "-m", "three"]);
        let head = repo.submodule_head(Path::new("dep")).unwrap();
        let d = repo.submodule_diff(Path::new("dep"), Some(one), Some(head));
        let summaries: Vec<(&str, bool)> = d
            .commits
            .iter()
            .map(|c| (c.summary.as_str(), c.added))
            .collect();
        assert_eq!(summaries, [("three", true), ("two", true)]);
        assert!(d.note.is_none());

        // The other way round they were removed.
        let back = repo.submodule_diff(Path::new("dep"), Some(head), Some(one));
        assert!(back.commits.iter().all(|c| !c.added) && back.commits.len() == 2);

        // An unknown commit: a note, not an error.
        let missing = Oid::from_hex("1".repeat(40).as_bytes()).unwrap();
        let d = repo.submodule_diff(Path::new("dep"), Some(missing), Some(head));
        assert!(d.note.is_some() && d.commits.is_empty());

        // Update checks the recorded commit out again.
        repo.submodule_update(&["dep"], true, false, &Cancel::default(), |_| {})
            .unwrap();
        assert_eq!(repo.submodule_head(Path::new("dep")), Some(one));
    }
}
