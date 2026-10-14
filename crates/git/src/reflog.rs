//! Reflogs: where HEAD and each branch pointed over time, read in process. A commit a
//! reset or rebase left behind is still in the reflog, so the app can bring it back
//! (reset to it, move a branch to it, or branch from it).

use crate::{Error, GixExt as _, Oid, Repo, Result};

/// One reflog line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflogEntry {
    /// `n` in `<ref>@{n}` (0 is the newest).
    pub index: usize,
    /// Where the ref pointed before (all zeros when it was created).
    pub old: Oid,
    /// Where the ref pointed after.
    pub new: Oid,
    /// What moved it ("commit: Add x", "reset: moving to HEAD~1", ...).
    pub message: String,
    /// Who moved it.
    pub committer: String,
    /// When (seconds since the epoch).
    pub time: i64,
}

/// The reflog of `name` (`HEAD`, or a full ref name like `refs/heads/main`), newest
/// first. A ref without a reflog gives an empty list.
pub fn read(repo: &Repo, name: &str) -> Result<Vec<ReflogEntry>> {
    let started = std::time::Instant::now();
    let r = repo.local();
    let reference = match r.try_find_reference(name) {
        Ok(Some(reference)) => reference,
        Ok(None) => return Ok(Vec::new()),
        Err(e) => return Err(Error::Parse(format!("reflog of {name}: {e}"))),
    };
    let mut platform = reference.log_iter();
    let Some(lines) = platform.all().gix()? else {
        return Ok(Vec::new());
    };
    let mut out: Vec<ReflogEntry> = lines
        .filter_map(|l| l.ok())
        .map(|l| ReflogEntry {
            index: 0,
            old: l.previous_oid(),
            new: l.new_oid(),
            message: l.message.to_string(),
            committer: l.signature.name.to_string(),
            time: l.signature.seconds(),
        })
        .collect();
    // The file is oldest first; `@{0}` is the newest.
    out.reverse();
    for (i, e) in out.iter_mut().enumerate() {
        e.index = i;
    }
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        entries = out.len(),
        "reflog"
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn reads_head_and_branch_reflogs_newest_first() {
        let t = TestRepo::new();
        let a = t.commit_file("a.txt", "1", "first");
        let b = t.commit_file("a.txt", "2", "second");
        t.git(&["reset", "-q", "--hard", "HEAD~1"]);
        t.git(&["checkout", "-q", "-b", "side"]);
        let repo = Repo::open(t.path()).unwrap();

        let head = read(&repo, "HEAD").unwrap();
        let got: Vec<(usize, &str)> = head.iter().map(|e| (e.index, e.message.as_str())).collect();
        assert_eq!(
            got,
            [
                (0, "checkout: moving from main to side"),
                (1, "reset: moving to HEAD~1"),
                (2, "commit: second"),
                (3, "commit (initial): first"),
            ]
        );
        // The reset left `second` behind; the reflog still has it.
        assert_eq!(head[1].old.to_string(), b);
        assert_eq!(head[1].new.to_string(), a);
        assert!(head[3].old.is_null());
        assert_eq!(head[0].committer, "Test");
        assert!(head[0].time >= head[3].time);

        let main = read(&repo, "refs/heads/main").unwrap();
        assert_eq!(main.len(), 3);
        assert_eq!(main[0].message, "reset: moving to HEAD~1");
        assert!(read(&repo, "refs/heads/nope").unwrap().is_empty());

        // Restoring: move a branch back to the commit the reset left.
        repo.move_branch("main", &b).unwrap();
        assert_eq!(t.git(&["rev-parse", "main"]).trim(), b);
        assert!(repo.move_branch("side", &b).is_err());
    }
}
