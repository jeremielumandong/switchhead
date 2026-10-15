//! A bisect session as the step panel and the graph show it: the marked commits (read in
//! process from `refs/bisect/*`), the commit being tested, the commits that may still be
//! the first bad one, and the first bad commit once it is found.
//!
//! Starting, marking and stopping run `git bisect` (`ops.rs`), so a session started on
//! the command line shows here too, and the other way round.

use crate::{GixExt as _, Oid, Repo, Result};

/// The state of a bisect session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bisect {
    /// The commit marked bad (git keeps only the oldest bad one).
    pub bad: Option<Oid>,
    /// Commits marked good.
    pub good: Vec<Oid>,
    /// Commits skipped (cannot be tested).
    pub skipped: Vec<Oid>,
    /// The commit checked out to test (HEAD).
    pub current: Option<Oid>,
    /// Commits that may still be the first bad one, newest first: those reachable from
    /// the bad commit and not from a good one, the bad commit included. Empty until both
    /// a bad and a good commit are marked.
    pub remaining: Vec<Oid>,
}

impl Bisect {
    /// Both ends are marked, so git is narrowing the range down.
    pub fn started(&self) -> bool {
        self.bad.is_some() && !self.good.is_empty()
    }

    /// The first bad commit, once only it is left.
    pub fn first_bad(&self) -> Option<Oid> {
        match self.remaining.as_slice() {
            [only] if self.started() && Some(*only) == self.bad => Some(*only),
            _ => None,
        }
    }

    /// Commits still to test: the remaining ones other than the bad and skipped ones.
    pub fn untested(&self) -> usize {
        self.remaining
            .iter()
            .filter(|c| Some(**c) != self.bad && !self.skipped.contains(c))
            .count()
    }

    /// Only skipped commits are left: the first bad commit is one of them or the bad one.
    pub fn only_skipped(&self) -> bool {
        self.started() && self.first_bad().is_none() && self.untested() == 0
    }

    /// About how many more steps there are, at worst (`⌈log2(untested + 1)⌉`).
    pub fn steps_left(&self) -> u32 {
        let n = self.untested() as u32;
        if n == 0 {
            0
        } else {
            u32::BITS - n.leading_zeros()
        }
    }
}

impl Repo {
    /// The bisect session, if one is running (`refs/bisect/*` or `BISECT_START`).
    pub fn bisect_state(&self) -> Result<Option<Bisect>> {
        let started = std::time::Instant::now();
        if !self.git_dir().join("BISECT_START").exists() {
            return Ok(None);
        }
        let r = self.local();
        let mut b = Bisect {
            current: r.head_id().ok().map(|id| id.detach()),
            ..Bisect::default()
        };
        let refs = r.references().gix()?;
        for reference in refs.prefixed("refs/bisect/").gix()? {
            let Ok(mut reference) = reference else {
                continue;
            };
            let name = reference.name().as_bstr().to_string();
            let Ok(id) = reference.peel_to_id() else {
                continue;
            };
            let id = id.detach();
            match name.strip_prefix("refs/bisect/") {
                Some("bad") => b.bad = Some(id),
                Some(n) if n.starts_with("good-") => b.good.push(id),
                Some(n) if n.starts_with("skip-") => b.skipped.push(id),
                _ => {}
            }
        }
        if let (Some(bad), false) = (b.bad, b.good.is_empty()) {
            let mut args = vec!["rev-list".to_owned(), bad.to_string(), "--not".to_owned()];
            args.extend(b.good.iter().map(Oid::to_string));
            let out = self.git()?.text(&args)?;
            b.remaining = out.lines().filter_map(|l| l.trim().parse().ok()).collect();
        }
        tracing::debug!(ms = started.elapsed().as_millis() as u64, "bisect state");
        Ok(Some(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::BisectMark;
    use crate::testutil::TestRepo;

    /// Ten commits; `bug.txt` turns bad in the seventh.
    fn history(t: &TestRepo) -> Vec<Oid> {
        (1..=10)
            .map(|i| {
                let text = if i >= 7 { "bad\n" } else { "good\n" };
                t.write("bug.txt", text);
                t.git(&["add", "bug.txt"]);
                t.commit_file("n.txt", &format!("{i}\n"), &format!("commit {i}"))
                    .parse()
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn walks_to_the_first_bad_commit() {
        let t = TestRepo::new();
        let ids = history(&t);
        let repo = Repo::open(t.path()).unwrap();
        assert_eq!(repo.bisect_state().unwrap(), None);

        repo.bisect_start(&ids[9].to_string(), &ids[0].to_string())
            .unwrap();
        let mut b = repo.bisect_state().unwrap().unwrap();
        assert_eq!(b.bad, Some(ids[9]));
        assert_eq!(b.good, vec![ids[0]]);
        assert_eq!(b.remaining.len(), 9);
        assert_eq!(b.untested(), 8);
        assert_eq!(b.steps_left(), 4);
        let mut steps = 0;
        while b.first_bad().is_none() {
            steps += 1;
            assert!(steps < 6, "bisect did not converge");
            let current = b.current.unwrap();
            assert!(b.remaining.contains(&current));
            let text = std::fs::read_to_string(t.join("bug.txt")).unwrap();
            let mark = if text == "bad\n" {
                BisectMark::Bad
            } else {
                BisectMark::Good
            };
            repo.bisect_mark(mark, None).unwrap();
            b = repo.bisect_state().unwrap().unwrap();
        }
        assert_eq!(b.first_bad(), Some(ids[6]));
        assert!(!b.only_skipped());
        repo.bisect_reset().unwrap();
        assert_eq!(repo.bisect_state().unwrap(), None);
    }

    #[test]
    fn marks_other_commits_and_skips() {
        let t = TestRepo::new();
        let ids = history(&t);
        let repo = Repo::open(t.path()).unwrap();
        repo.bisect_start(&ids[9].to_string(), &ids[5].to_string())
            .unwrap();
        // Mark a commit that is not checked out.
        repo.bisect_mark(BisectMark::Bad, Some(&ids[7].to_string()))
            .unwrap();
        let b = repo.bisect_state().unwrap().unwrap();
        assert_eq!(b.bad, Some(ids[7]));
        assert_eq!(b.remaining, vec![ids[7], ids[6]]);
        // Skipping the only candidate leaves only skipped commits.
        repo.bisect_mark(BisectMark::Skip, Some(&ids[6].to_string()))
            .unwrap();
        let b = repo.bisect_state().unwrap().unwrap();
        assert_eq!(b.skipped, vec![ids[6]]);
        assert!(b.only_skipped());
        assert_eq!(b.first_bad(), None);
        repo.bisect_reset().unwrap();
    }

    #[test]
    fn counts_steps() {
        let ids: Vec<Oid> = (1..=9u8)
            .map(|i| Oid::from_bytes_or_panic(&[i; 20]))
            .collect();
        let with = |untested: usize| Bisect {
            bad: Some(ids[0]),
            good: vec![ids[8]],
            remaining: ids[..=untested].to_vec(),
            ..Bisect::default()
        };
        let steps: Vec<u32> = [0, 1, 2, 3, 7].map(|n| with(n).steps_left()).to_vec();
        assert_eq!(steps, [0, 1, 2, 2, 3]);
        assert_eq!(with(0).first_bad(), Some(ids[0]));
    }
}
