//! Branches, remote-tracking branches, tags, stashes and HEAD, plus the decorations the
//! graph shows next to commits.
//!
//! [`load`] reads every reference in process with gitoxide (packed and loose, tags peeled
//! to their commits) and the stash reflog. Upstreams and ahead/behind counts need a graph
//! walk per branch, so [`load_tracking`] asks `git for-each-ref` for them; the UI can show
//! the refs first and add the counts when they arrive.

use std::collections::HashMap;

use gix::bstr::ByteSlice as _;

use crate::{GixExt as _, Oid, Repo, Result};

/// A local branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// Short name (`feature/x`).
    pub name: String,
    /// The commit it points at.
    pub id: Oid,
    /// HEAD is on this branch.
    pub is_head: bool,
    /// Upstream, filled by [`load_tracking`].
    pub tracking: Option<Tracking>,
}

/// A branch's upstream and how far apart they are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tracking {
    /// Upstream short name (`origin/main`).
    pub upstream: String,
    /// Commits on the branch and not upstream.
    pub ahead: u32,
    /// Commits upstream and not on the branch.
    pub behind: u32,
    /// The upstream branch no longer exists (deleted on the remote and pruned).
    pub gone: bool,
}

/// A remote-tracking branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteBranch {
    /// Remote name (`origin`).
    pub remote: String,
    /// Branch name on the remote (`main`).
    pub name: String,
    /// The commit.
    pub id: Oid,
    /// `refs/remotes/<remote>/HEAD`, a symbolic pointer to the remote's default branch.
    pub is_symbolic_head: bool,
}

impl RemoteBranch {
    /// `origin/main`.
    pub fn short(&self) -> String {
        format!("{}/{}", self.remote, self.name)
    }
}

/// A tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    /// Tag name.
    pub name: String,
    /// The object it finally points at (a commit, for nearly every tag).
    pub id: Oid,
    /// The tag object, for annotated tags.
    pub annotation: Option<Oid>,
}

/// A stash entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    /// Position: `stash@{index}`.
    pub index: usize,
    /// The stash commit.
    pub id: Oid,
    /// Its message ("WIP on main: ..." or "On main: message").
    pub message: String,
    /// When it was made (seconds since the epoch).
    pub time: i64,
}

/// What kind of label a decoration is (for its chip style).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LabelKind {
    /// HEAD detached at this commit.
    Head,
    /// The branch HEAD is on.
    CurrentBranch,
    /// Another local branch.
    Branch,
    /// A remote-tracking branch.
    Remote,
    /// A tag.
    Tag,
    /// A stash entry.
    Stash,
}

/// A label on a commit in the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    /// Kind, for styling and ordering.
    pub kind: LabelKind,
    /// Text (`main`, `origin/main`, `v1.0`, `stash@{0}`).
    pub text: String,
}

/// Every reference of a repository.
#[derive(Clone, Debug, Default)]
pub struct Refs {
    /// HEAD, `None` on an unborn branch.
    pub head: Option<Oid>,
    /// HEAD's branch, `None` when detached.
    pub head_branch: Option<String>,
    /// Local branches, sorted by name.
    pub branches: Vec<Branch>,
    /// Remote-tracking branches, sorted by remote then name.
    pub remotes: Vec<RemoteBranch>,
    /// Tags, sorted by name.
    pub tags: Vec<Tag>,
    /// Stashes, newest first.
    pub stashes: Vec<Stash>,
    /// Worktrees, the main one first; filled by [`load_tracking`].
    pub worktrees: Vec<crate::worktrees::Worktree>,
    /// Submodules; filled by [`load_tracking`].
    pub submodules: Vec<crate::worktrees::Submodule>,
}

impl Refs {
    /// Remote names, in order.
    pub fn remote_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.remotes.iter().map(|r| r.remote.as_str()).collect();
        names.dedup();
        names
    }

    /// The branch HEAD is on.
    pub fn current_branch(&self) -> Option<&Branch> {
        self.branches.iter().find(|b| b.is_head)
    }

    /// Labels by commit, for the graph. Each commit's labels are ordered: HEAD, the
    /// current branch, branches, remote branches, tags, stashes.
    pub fn decorations(&self) -> HashMap<Oid, Vec<Label>> {
        let mut map: HashMap<Oid, Vec<Label>> = HashMap::new();
        let mut add = |id: Oid, kind: LabelKind, text: String| {
            map.entry(id).or_default().push(Label { kind, text });
        };
        if let (Some(id), None) = (self.head, &self.head_branch) {
            add(id, LabelKind::Head, "HEAD".into());
        }
        for b in &self.branches {
            let kind = if b.is_head {
                LabelKind::CurrentBranch
            } else {
                LabelKind::Branch
            };
            add(b.id, kind, b.name.clone());
        }
        for r in &self.remotes {
            if !r.is_symbolic_head {
                add(r.id, LabelKind::Remote, r.short());
            }
        }
        for t in &self.tags {
            add(t.id, LabelKind::Tag, t.name.clone());
        }
        for s in &self.stashes {
            add(s.id, LabelKind::Stash, format!("stash@{{{}}}", s.index));
        }
        for labels in map.values_mut() {
            labels.sort_by_key(|l| l.kind);
        }
        map
    }
}

/// Read every reference with gitoxide. Upstreams are left empty; see [`load_tracking`].
pub fn load(repo: &Repo) -> Result<Refs> {
    let started = std::time::Instant::now();
    let r = repo.local();
    let mut refs = Refs::default();
    if let Ok(head) = r.head() {
        refs.head = head.id().map(|i| i.detach());
        refs.head_branch = head.referent_name().map(|n| n.shorten().to_string());
    }
    let platform = r.references().gix()?;
    for reference in platform.all().gix()? {
        let Ok(mut reference) = reference else {
            continue;
        };
        let full = reference.name().as_bstr().to_str_lossy().into_owned();
        let symbolic = matches!(reference.target(), gix::refs::TargetRef::Symbolic(_));
        // The id the ref names before peeling (an annotated tag's own object).
        let direct = reference.try_id().map(|i| i.detach());
        let Ok(peeled) = reference.peel_to_id() else {
            continue;
        };
        let id = peeled.detach();
        if let Some(name) = full.strip_prefix("refs/heads/") {
            refs.branches.push(Branch {
                name: name.to_owned(),
                id,
                is_head: refs.head_branch.as_deref() == Some(name),
                tracking: None,
            });
        } else if let Some(rest) = full.strip_prefix("refs/remotes/") {
            let Some((remote, name)) = rest.split_once('/') else {
                continue;
            };
            refs.remotes.push(RemoteBranch {
                remote: remote.to_owned(),
                name: name.to_owned(),
                id,
                is_symbolic_head: symbolic && name == "HEAD",
            });
        } else if let Some(name) = full.strip_prefix("refs/tags/") {
            refs.tags.push(Tag {
                name: name.to_owned(),
                id,
                annotation: direct.filter(|d| *d != id),
            });
        }
    }
    refs.stashes = stashes(&r)?;
    refs.branches.sort_by(|a, b| a.name.cmp(&b.name));
    refs.remotes
        .sort_by(|a, b| (&a.remote, &a.name).cmp(&(&b.remote, &b.name)));
    refs.tags.sort_by(|a, b| a.name.cmp(&b.name));
    tracing::debug!(
        repo = %repo.name(),
        branches = refs.branches.len(),
        remotes = refs.remotes.len(),
        tags = refs.tags.len(),
        ms = started.elapsed().as_millis() as u64,
        "refs"
    );
    Ok(refs)
}

/// Stash entries from the `refs/stash` reflog, newest first.
fn stashes(r: &gix::Repository) -> Result<Vec<Stash>> {
    let Ok(Some(stash)) = r.try_find_reference("refs/stash") else {
        return Ok(Vec::new());
    };
    let mut platform = stash.log_iter();
    let Some(lines) = platform.all()? else {
        return Ok(Vec::new());
    };
    let mut out: Vec<Stash> = lines
        .filter_map(|l| l.ok())
        .map(|l| Stash {
            index: 0,
            id: l.new_oid(),
            message: l.message.to_str_lossy().into_owned(),
            time: l.signature.seconds(),
        })
        .collect();
    // The reflog is oldest first; stash@{0} is the newest.
    out.reverse();
    for (i, s) in out.iter_mut().enumerate() {
        s.index = i;
    }
    Ok(out)
}

/// Fill each branch's upstream and ahead/behind counts with one `git for-each-ref`, and
/// the worktrees and submodules (these are best effort: a failure leaves them empty).
pub fn load_tracking(repo: &Repo, refs: &mut Refs) -> Result<()> {
    refs.worktrees = crate::worktrees::list_worktrees(repo).unwrap_or_else(|e| {
        tracing::debug!(error = %e, "worktrees unavailable");
        Vec::new()
    });
    refs.submodules = crate::worktrees::list_submodules(repo).unwrap_or_else(|e| {
        tracing::debug!(error = %e, "submodules unavailable");
        Vec::new()
    });
    let out = repo.git()?.run(&[
        "for-each-ref",
        "--format=%(refname:short)%00%(upstream:short)%00%(upstream:track,nobracket)",
        "refs/heads",
    ])?;
    let text = out.text();
    let tracking = parse_tracking(&text);
    for b in &mut refs.branches {
        b.tracking = tracking.get(b.name.as_str()).cloned();
    }
    Ok(())
}

/// Parse `name\0upstream\0track` lines. Branches without an upstream are left out.
fn parse_tracking(text: &str) -> HashMap<&str, Tracking> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut parts = line.split('\0');
        let (Some(name), Some(upstream)) = (parts.next(), parts.next()) else {
            continue;
        };
        if upstream.is_empty() {
            continue;
        }
        let mut t = Tracking {
            upstream: upstream.to_owned(),
            ..Default::default()
        };
        // "ahead 2, behind 1", "ahead 2", "behind 1", "gone", or empty (in sync).
        for item in parts.next().unwrap_or("").split(", ") {
            let mut words = item.split(' ');
            match (words.next(), words.next().and_then(|n| n.parse().ok())) {
                (Some("ahead"), Some(n)) => t.ahead = n,
                (Some("behind"), Some(n)) => t.behind = n,
                (Some("gone"), _) => t.gone = true,
                _ => {}
            }
        }
        map.insert(name, t);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn reads_branches_tags_remotes_and_stashes() {
        let upstream = TestRepo::new();
        upstream.commit_file("a", "1", "one");
        let t = TestRepo::new();
        t.git(&[
            "remote",
            "add",
            "origin",
            &upstream.path().to_string_lossy(),
        ]);
        t.git(&["fetch", "-q", "origin"]);
        t.git(&["checkout", "-q", "-b", "main", "--track", "origin/main"]);
        t.git(&["remote", "set-head", "origin", "main"]);
        let one = t.git(&["rev-parse", "HEAD"]).trim().to_owned();
        let two = t.commit_file("a", "2", "two");
        t.git(&["tag", "light"]);
        t.git(&["tag", "-a", "-m", "annotated", "v1.0", &one]);
        t.git(&["branch", "feature/x", &one]);
        t.write("a", "dirty");
        t.git(&["stash", "push", "-q", "-m", "first"]);
        t.write("a", "dirty again");
        t.git(&["stash", "push", "-q", "-m", "second"]);

        let repo = Repo::open(t.path()).unwrap();
        let mut refs = load(&repo).unwrap();
        let oid = |s: &str| Oid::from_hex(s.as_bytes()).unwrap();
        assert_eq!(refs.head, Some(oid(&two)));
        assert_eq!(refs.head_branch.as_deref(), Some("main"));
        let names: Vec<&str> = refs.branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["feature/x", "main"]);
        assert!(refs.current_branch().is_some_and(|b| b.name == "main"));
        let remotes: Vec<(String, bool)> = refs
            .remotes
            .iter()
            .map(|r| (r.short(), r.is_symbolic_head))
            .collect();
        assert_eq!(
            remotes,
            [("origin/HEAD".into(), true), ("origin/main".into(), false)]
        );
        assert_eq!(refs.remote_names(), ["origin"]);
        let tags: Vec<(&str, Oid, bool)> = refs
            .tags
            .iter()
            .map(|t| (t.name.as_str(), t.id, t.annotation.is_some()))
            .collect();
        assert_eq!(
            tags,
            [("light", oid(&two), false), ("v1.0", oid(&one), true)]
        );
        let stashes: Vec<(usize, bool)> = refs
            .stashes
            .iter()
            .map(|s| (s.index, s.message.contains("second")))
            .collect();
        assert_eq!(stashes, [(0, true), (1, false)]);

        load_tracking(&repo, &mut refs).unwrap();
        let main = refs.current_branch().unwrap();
        assert_eq!(
            main.tracking,
            Some(Tracking {
                upstream: "origin/main".into(),
                ahead: 1,
                behind: 0,
                gone: false
            })
        );
        assert!(refs.branches[0].tracking.is_none());

        let deco = refs.decorations();
        let labels: Vec<(LabelKind, &str)> = deco[&oid(&two)]
            .iter()
            .map(|l| (l.kind, l.text.as_str()))
            .collect();
        assert_eq!(
            labels,
            [
                (LabelKind::CurrentBranch, "main"),
                (LabelKind::Tag, "light")
            ]
        );
        let labels: Vec<(LabelKind, &str)> = deco[&oid(&one)]
            .iter()
            .map(|l| (l.kind, l.text.as_str()))
            .collect();
        assert_eq!(
            labels,
            [
                (LabelKind::Branch, "feature/x"),
                (LabelKind::Remote, "origin/main"),
                (LabelKind::Tag, "v1.0")
            ]
        );

        t.git(&["checkout", "-q", "--detach", &one]);
        let refs = load(&repo).unwrap();
        assert!(refs.head_branch.is_none());
        assert_eq!(refs.decorations()[&oid(&one)][0].kind, LabelKind::Head);
    }

    #[test]
    fn parses_track_strings() {
        let text = "a\0origin/a\0ahead 2, behind 3\nb\0origin/b\0gone\nc\0\0\nd\0origin/d\0\n";
        let m = parse_tracking(text);
        assert_eq!((m["a"].ahead, m["a"].behind, m["a"].gone), (2, 3, false));
        assert!(m["b"].gone);
        assert!(!m.contains_key("c"));
        assert_eq!(
            m["d"],
            Tracking {
                upstream: "origin/d".into(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn empty_repository_has_no_refs() {
        let t = TestRepo::new();
        let repo = Repo::open(t.path()).unwrap();
        let refs = load(&repo).unwrap();
        assert!(refs.head.is_none() && refs.branches.is_empty() && refs.stashes.is_empty());
        assert_eq!(refs.head_branch.as_deref(), Some("main"));
    }
}
