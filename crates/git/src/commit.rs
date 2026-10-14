//! One commit in full: what the details pane shows above the changed files.
//!
//! Read in process with gitoxide. Author and committer go through `.mailmap`, like the log.

use crate::{GixExt, Oid, Repo, Result};

/// A person and a moment (author or committer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    /// Name (after `.mailmap`).
    pub name: String,
    /// Email (after `.mailmap`).
    pub email: String,
    /// Seconds since the epoch.
    pub time: i64,
    /// The recorded time zone, in seconds east of UTC.
    pub offset: i32,
}

/// A commit's header and full message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    /// The commit.
    pub id: Oid,
    /// Its parents, in order. A shallow clone's boundary commits list parents that are
    /// not in the repository: see [`CommitInfo::diff_base`].
    pub parents: Vec<Oid>,
    /// Author.
    pub author: Signature,
    /// Committer.
    pub committer: Signature,
    /// The whole message, as written (lossy UTF-8).
    pub message: String,
    /// The commit carries a signature (`gpgsig`). Not verified.
    pub signed: bool,
    /// The first parent, when the repository has it: what the commit's changes are
    /// shown against (`None`: a root commit, or a shallow boundary).
    pub diff_base: Option<Oid>,
}

impl CommitInfo {
    /// The first line of the message.
    pub fn summary(&self) -> &str {
        self.message.lines().next().unwrap_or_default()
    }

    /// The message after its first paragraph, trimmed (empty when there is none).
    pub fn body(&self) -> &str {
        let m = self.message.trim_end();
        match m.split_once("\n\n") {
            Some((_, body)) => body.trim_matches('\n'),
            None => "",
        }
    }

    /// Author and committer differ (rebased, cherry-picked or applied commits).
    pub fn committed_by_other(&self) -> bool {
        self.author.name != self.committer.name
            || self.author.email != self.committer.email
            || self.author.time != self.committer.time
    }
}

/// Read commit `id`.
pub fn info(repo: &Repo, id: Oid) -> Result<CommitInfo> {
    let started = std::time::Instant::now();
    let r = repo.local();
    let commit = r.find_commit(id).gix()?;
    let c = commit.decode().gix()?;
    let mailmap = r.open_mailmap();
    let sig = |s: gix::actor::SignatureRef<'_>| -> Signature {
        let (name, email) = match mailmap.try_resolve_ref(s) {
            Some(m) => (
                m.name.unwrap_or(s.name).to_string(),
                m.email.unwrap_or(s.email).to_string(),
            ),
            None => (s.name.to_string(), s.email.to_string()),
        };
        let time = s.time().unwrap_or_default();
        Signature {
            name,
            email,
            time: time.seconds,
            offset: time.offset,
        }
    };
    let author = sig(c.author().gix()?);
    let committer = sig(c.committer().gix()?);
    let parents: Vec<Oid> = c.parents().collect();
    let diff_base = parents.first().copied().filter(|p| r.has_object(p));
    let info = CommitInfo {
        id,
        author,
        committer,
        message: c.message.to_string(),
        signed: c.extra_headers().pgp_signature().is_some(),
        parents,
        diff_base,
    };
    tracing::debug!(ms = started.elapsed().as_millis() as u64, "commit info");
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn reads_header_message_and_mailmap() {
        let t = TestRepo::new();
        let first = t.commit_file("a.txt", "a\n", "first");
        t.write(
            ".mailmap",
            "Real Name <real@example.com> <test@example.com>\n",
        );
        t.write("b.txt", "b\n");
        t.git(&["add", "."]);
        t.git(&[
            "-c",
            "user.name=Other",
            "-c",
            "user.email=other@example.com",
            "commit",
            "-q",
            "--author",
            "Test <test@example.com>",
            "-m",
            "Add b\n\nWhy b matters.\n\nMore.",
        ]);
        let head = t.git(&["rev-parse", "HEAD"]).trim().to_owned();
        let repo = Repo::open(t.path()).unwrap();
        let first: Oid = first.parse().unwrap();
        let i = info(&repo, head.parse().unwrap()).unwrap();
        assert_eq!(i.summary(), "Add b");
        assert_eq!(i.body(), "Why b matters.\n\nMore.");
        assert_eq!(i.author.name, "Real Name");
        assert_eq!(i.author.email, "real@example.com");
        assert_eq!(i.committer.name, "Other");
        assert!(i.committed_by_other());
        assert_eq!(i.parents, vec![first]);
        assert_eq!(i.diff_base, Some(first));
        assert!(!i.signed);
        assert_eq!(i.author.offset, 0);

        let root = info(&repo, first).unwrap();
        assert!(root.parents.is_empty() && root.diff_base.is_none());
        assert_eq!(root.body(), "");
        assert!(!root.committed_by_other());
    }
}
