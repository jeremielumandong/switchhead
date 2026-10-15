//! Patches: write commits as mailbox patches (`git format-patch`), read what patch files
//! hold before applying them, and apply them as commits (`git am`) or to the work tree
//! and index (`git apply`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::{Error, Repo, Result};

/// How [`Repo::apply_patches`] applies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApplyMode {
    /// Make commits with the patches' authors and messages (`git am`; mailbox patches).
    #[default]
    Commit,
    /// Change the work tree only (`git apply`).
    WorkTree,
    /// Change the work tree and stage the changes (`git apply --index`).
    Index,
}

/// One file a patch changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchedFile {
    /// Its path in the repository (the new name of a rename).
    pub path: PathBuf,
    /// Lines added and removed (`None`: binary).
    pub lines: Option<(u32, u32)>,
}

/// What a set of patch files holds.
#[derive(Clone, Debug, Default)]
pub struct PatchInfo {
    /// The files they change.
    pub files: Vec<PatchedFile>,
    /// The commits' subjects when they are mailbox patches (empty for plain diffs).
    pub commits: Vec<String>,
    /// git's message when they do not apply cleanly to the work tree.
    pub problem: Option<String>,
}

/// The subjects of the mails in a mailbox patch (`[PATCH 2/3]` removed), in order. A
/// plain diff has none.
pub fn subjects(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_headers = false;
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("From ") {
            in_headers = true;
            continue;
        }
        if in_headers && line.is_empty() {
            in_headers = false;
            continue;
        }
        let Some(subject) = line.strip_prefix("Subject:").filter(|_| in_headers) else {
            continue;
        };
        let mut subject = subject.trim().to_owned();
        // A long subject is folded over lines that start with a space.
        while let Some(next) = lines.peek().filter(|l| l.starts_with([' ', '\t'])) {
            subject.push(' ');
            subject.push_str(next.trim());
            lines.next();
        }
        out.push(strip_patch_tag(&subject).to_owned());
    }
    out
}

/// `[PATCH 2/3] Subject` → `Subject`.
fn strip_patch_tag(subject: &str) -> &str {
    match subject.strip_prefix('[').and_then(|s| s.split_once(']')) {
        Some((tag, rest)) if tag.contains("PATCH") => rest.trim_start(),
        _ => subject,
    }
}

/// `git apply --numstat -z` output: `added\tremoved\tpath\0`, `-` counts for binary files.
fn parse_numstat(out: &[u8]) -> Vec<PatchedFile> {
    out.split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .filter_map(|record| {
            let text = String::from_utf8_lossy(record);
            let mut parts = text.splitn(3, '\t');
            let (added, removed, path) = (parts.next()?, parts.next()?, parts.next()?);
            let lines = added.parse().ok().zip(removed.parse().ok());
            Some(PatchedFile {
                path: PathBuf::from(path),
                lines,
            })
        })
        .collect()
}

fn args_with(first: &[&str], patches: &[&Path]) -> Vec<OsString> {
    let mut args: Vec<OsString> = first.iter().map(OsString::from).collect();
    args.extend(patches.iter().map(|p| p.as_os_str().to_owned()));
    args
}

impl Repo {
    /// What `patches` change, their commits' subjects, and whether they apply cleanly.
    pub fn inspect_patches(&self, patches: &[&Path]) -> Result<PatchInfo> {
        let git = self.git()?;
        let mut commits = Vec::new();
        for p in patches {
            let bytes = std::fs::read(p)?;
            commits.extend(subjects(&String::from_utf8_lossy(&bytes)));
        }
        let files = parse_numstat(
            &git.run(&args_with(&["apply", "--numstat", "-z"], patches))?
                .stdout,
        );
        let problem = match git.run(&args_with(&["apply", "--check"], patches)) {
            Ok(_) => None,
            Err(Error::Command { message, .. }) => Some(message),
            Err(e) => return Err(e),
        };
        Ok(PatchInfo {
            files,
            commits,
            problem,
        })
    }

    /// Apply `patches` in order. `three_way` falls back to a three-way merge where the
    /// context does not match, leaving conflicts to resolve (`git am` then stops, and
    /// [`Repo::continue_operation`] / `abort_operation` finish it).
    pub fn apply_patches(&self, patches: &[&Path], mode: ApplyMode, three_way: bool) -> Result<()> {
        let mut first = match mode {
            ApplyMode::Commit => vec!["am", "-q"],
            ApplyMode::WorkTree => vec!["apply"],
            ApplyMode::Index => vec!["apply", "--index"],
        };
        if three_way {
            first.push("--3way");
        }
        self.git()?.run(&args_with(&first, patches))?;
        Ok(())
    }

    /// One commit as a mailbox patch (`git format-patch -1 --stdout`).
    pub fn patch_text(&self, rev: &str) -> Result<String> {
        Ok(self
            .git()?
            .run(&["format-patch", "-1", "--stdout", rev])?
            .text())
    }

    /// Write `from` and every commit after it up to HEAD as numbered patches into `dir`
    /// (merges are left out, as `git format-patch` does). Returns the files written.
    pub fn format_patches_to_head(&self, from: &str, dir: &Path) -> Result<Vec<PathBuf>> {
        let git = self.git()?;
        match git.run(&["merge-base", "--is-ancestor", from, "HEAD"]) {
            Ok(_) => {}
            Err(Error::Command { code: Some(1), .. }) => {
                return Err(Error::Unsupported(
                    "this commit is not on the current branch".into(),
                ));
            }
            Err(e) => return Err(e),
        }
        let parent = format!("{from}^");
        let has_parent = git.run(&["rev-parse", "--verify", "-q", &parent]).is_ok();
        let range = if has_parent {
            vec![format!("{parent}..HEAD")]
        } else {
            vec!["--root".to_owned(), "HEAD".to_owned()]
        };
        let mut args = vec![OsString::from("format-patch"), "-o".into(), dir.into()];
        args.extend(range.into_iter().map(OsString::from));
        Ok(git
            .run(&args)?
            .text()
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RepoState;
    use crate::ops::ResetMode;
    use crate::testutil::TestRepo;

    fn repo(t: &TestRepo) -> Repo {
        Repo::open(t.path()).unwrap()
    }

    #[test]
    fn reads_subjects() {
        let text = "From 0123 Mon Sep 17 00:00:00 2001\nFrom: A <a@b>\nSubject: [PATCH 1/2] Fix the\n thing\n\nbody\nSubject: not a header\n---\n\
                    From 4567 Mon Sep 17 00:00:00 2001\nSubject: Plain\n\ndiff --git a/x b/x\n";
        assert_eq!(subjects(text), ["Fix the thing", "Plain"]);
        assert!(subjects("diff --git a/x b/x\n--- a/x\n+++ b/x\n").is_empty());
        assert_eq!(strip_patch_tag("[RFC] Keep"), "[RFC] Keep");
    }

    #[test]
    fn creates_inspects_and_applies() {
        let t = TestRepo::new();
        let r = repo(&t);
        t.commit_file("a.txt", "1\n", "one");
        t.commit_file("a.txt", "1\n2\n", "two");
        t.commit_file("b.txt", "b\n", "three");
        let out = tempfile::tempdir().unwrap();

        let files = r.format_patches_to_head("HEAD~1", out.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(
            r.patch_text("HEAD")
                .unwrap()
                .contains("Subject: [PATCH] three")
        );
        let all = tempfile::tempdir().unwrap();
        let root = r.format_patches_to_head("HEAD~2", all.path()).unwrap();
        assert_eq!(root.len(), 3);

        r.reset("HEAD~2", ResetMode::Hard).unwrap();
        let paths: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
        let info = r.inspect_patches(&paths).unwrap();
        assert_eq!(info.commits, ["two", "three"]);
        assert_eq!(info.problem, None);
        assert_eq!(
            info.files,
            [
                PatchedFile {
                    path: "a.txt".into(),
                    lines: Some((1, 0))
                },
                PatchedFile {
                    path: "b.txt".into(),
                    lines: Some((1, 0))
                },
            ]
        );
        r.apply_patches(&paths, ApplyMode::Commit, false).unwrap();
        assert_eq!(r.last_message().unwrap(), "three");

        // Applied again, they no longer apply: the check says so and `am` stops.
        let info = r.inspect_patches(&paths).unwrap();
        assert!(info.problem.is_some());
        assert!(r.apply_patches(&paths, ApplyMode::Commit, false).is_err());
        assert_eq!(r.state(), RepoState::ApplyMailbox);
        r.abort_operation().unwrap();

        r.reset("HEAD~1", ResetMode::Hard).unwrap();
        r.apply_patches(&paths[1..], ApplyMode::Index, false)
            .unwrap();
        let staged = t.git(&["diff", "--cached", "--name-only"]);
        assert_eq!(staged.trim(), "b.txt");
        r.reset("HEAD", ResetMode::Hard).unwrap();
        r.apply_patches(&paths[1..], ApplyMode::WorkTree, false)
            .unwrap();
        assert!(
            t.git(&["diff", "--cached", "--name-only"])
                .trim()
                .is_empty()
        );
        assert!(t.join("b.txt").exists());

        let err = r.format_patches_to_head("nope", out.path()).unwrap_err();
        assert!(!matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn refuses_commits_off_the_current_branch() {
        let t = TestRepo::new();
        let r = repo(&t);
        t.commit_file("a.txt", "1\n", "one");
        t.git(&["checkout", "-q", "-b", "side"]);
        t.commit_file("s.txt", "s\n", "side");
        t.git(&["checkout", "-q", "-"]);
        let out = tempfile::tempdir().unwrap();
        let err = r.format_patches_to_head("side", out.path()).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }
}
