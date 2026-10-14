//! The commit log: a streaming walk in `git log --date-order` order into a columnar store.
//!
//! [`walk`] reads commits in process with gitoxide (using the commit-graph file when git's
//! config allows it) and hands them to the caller in batches: a small first batch so the
//! graph shows at once, then larger ones. Each batch carries its graph rows
//! ([`crate::graph`]), so the UI only appends and paints.
//!
//! [`CommitLog`] keeps every column in flat vectors: ids, parent ids (one arena with
//! per-commit offsets), commit times, interned authors, and summaries in one string arena.
//! There is no per-commit allocation.

use std::collections::HashMap;
use std::time::Instant;

use gix::bstr::ByteSlice as _;
use gix::prelude::FindExt as _;

use crate::cli::Cancel;
use crate::graph::{GraphRows, Layout};
use crate::{GixExt as _, Oid, Repo, Result};

/// Commits in the first batch: enough to fill a window.
pub const FIRST_BATCH: usize = 300;
/// Commits in each later batch.
pub const BATCH: usize = 2_000;

/// An author (name and email, after `.mailmap`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Author {
    /// Display name.
    pub name: String,
    /// Email address.
    pub email: String,
}

/// Commits in display order, stored by column.
#[derive(Clone, Debug, Default)]
pub struct CommitLog {
    ids: Vec<Oid>,
    /// `parents[parent_start[i]..parent_start[i + 1]]` are commit `i`'s parents.
    parents: Vec<Oid>,
    parent_start: Vec<u32>,
    /// Author time, seconds since the epoch.
    author_time: Vec<i64>,
    /// Committer time, seconds since the epoch.
    commit_time: Vec<i64>,
    author: Vec<u32>,
    authors: Vec<Author>,
    /// `"name\0email"` → index into `authors`.
    author_index: HashMap<String, u32>,
    /// Reused lookup key, so a known author costs no allocation.
    key: String,
    /// `summary_arena[summary_start[i]..summary_start[i + 1]]` is commit `i`'s summary.
    summary_arena: String,
    summary_start: Vec<u32>,
    rows: HashMap<Oid, u32>,
}

impl CommitLog {
    /// An empty log.
    pub fn new() -> Self {
        Self {
            parent_start: vec![0],
            summary_start: vec![0],
            ..Default::default()
        }
    }

    /// Number of commits.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether there are no commits.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Commit id of `row`.
    pub fn id(&self, row: usize) -> Oid {
        self.ids[row]
    }

    /// Every commit id, in display order.
    pub fn ids(&self) -> &[Oid] {
        &self.ids
    }

    /// Parent ids of `row`.
    pub fn parents(&self, row: usize) -> &[Oid] {
        &self.parents[self.parent_start[row] as usize..self.parent_start[row + 1] as usize]
    }

    /// Author time of `row` (seconds since the epoch).
    pub fn author_time(&self, row: usize) -> i64 {
        self.author_time[row]
    }

    /// Committer time of `row` (seconds since the epoch).
    pub fn commit_time(&self, row: usize) -> i64 {
        self.commit_time[row]
    }

    /// Author of `row`.
    pub fn author(&self, row: usize) -> &Author {
        &self.authors[self.author[row] as usize]
    }

    /// Interned author index of `row` (equal indices, same author).
    pub fn author_index(&self, row: usize) -> u32 {
        self.author[row]
    }

    /// Every distinct author, indexed by [`CommitLog::author_index`].
    pub fn authors(&self) -> &[Author] {
        &self.authors
    }

    /// First line of `row`'s message.
    pub fn summary(&self, row: usize) -> &str {
        &self.summary_arena[self.summary_start[row] as usize..self.summary_start[row + 1] as usize]
    }

    /// The row of a commit, if it is in the log.
    pub fn row_of(&self, id: &Oid) -> Option<usize> {
        self.rows.get(id).map(|r| *r as usize)
    }

    fn intern(&mut self, name: &str, email: &str) -> u32 {
        self.key.clear();
        self.key.push_str(name);
        self.key.push('\0');
        self.key.push_str(email);
        if let Some(i) = self.author_index.get(self.key.as_str()) {
            return *i;
        }
        let i = self.authors.len() as u32;
        self.authors.push(Author {
            name: name.to_owned(),
            email: email.to_owned(),
        });
        self.author_index.insert(self.key.clone(), i);
        i
    }

    /// Add one commit at the end.
    pub fn push(&mut self, c: CommitData<'_>) {
        let row = self.ids.len() as u32;
        self.ids.push(c.id);
        self.rows.insert(c.id, row);
        self.parents.extend_from_slice(c.parents);
        self.parent_start.push(self.parents.len() as u32);
        self.author_time.push(c.author_time);
        self.commit_time.push(c.commit_time);
        let a = self.intern(c.author_name, c.author_email);
        self.author.push(a);
        self.summary_arena.push_str(c.summary);
        self.summary_start.push(self.summary_arena.len() as u32);
    }

    /// Append a batch produced by [`walk`] (or any other log), re-interning its authors.
    pub fn append(&mut self, other: &CommitLog) {
        for row in 0..other.len() {
            let a = other.author(row);
            self.push(CommitData {
                id: other.id(row),
                parents: other.parents(row),
                author_time: other.author_time(row),
                commit_time: other.commit_time(row),
                author_name: &a.name,
                author_email: &a.email,
                summary: other.summary(row),
            });
        }
    }

    /// The rows whose commits match `pred`.
    pub fn rows_where(&self, mut pred: impl FnMut(usize) -> bool) -> Vec<usize> {
        (0..self.len()).filter(|r| pred(*r)).collect()
    }
}

/// One commit, borrowed, for [`CommitLog::push`].
#[derive(Clone, Copy, Debug)]
pub struct CommitData<'a> {
    /// Commit id.
    pub id: Oid,
    /// Parent ids.
    pub parents: &'a [Oid],
    /// Author time (seconds since the epoch).
    pub author_time: i64,
    /// Committer time (seconds since the epoch).
    pub commit_time: i64,
    /// Author name.
    pub author_name: &'a str,
    /// Author email.
    pub author_email: &'a str,
    /// First line of the message.
    pub summary: &'a str,
}

/// Which commits to walk from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// Every branch, remote-tracking branch and tag, plus HEAD (`git log --all` without
    /// the stash).
    #[default]
    All,
    /// HEAD only.
    Head,
    /// These revisions (branch names, full ref names, ids).
    Revs(Vec<String>),
}

/// Commit order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    /// Children before parents, otherwise by commit time (`--date-order`).
    #[default]
    Date,
    /// Children before parents, lines of history kept together (`--topo-order`).
    Topo,
}

/// What to walk and how to batch it.
#[derive(Clone, Debug)]
pub struct LogOptions {
    /// Starting points.
    pub scope: Scope,
    /// Sort order.
    pub order: Order,
    /// Size of the first batch.
    pub first_batch: usize,
    /// Size of later batches.
    pub batch: usize,
    /// Stop after this many commits (`None`: the whole history).
    pub limit: Option<usize>,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            scope: Scope::All,
            order: Order::Date,
            first_batch: FIRST_BATCH,
            batch: BATCH,
            limit: None,
        }
    }
}

/// A batch of commits with their graph rows. Rows continue where the previous batch
/// ended: append both to the caller's [`CommitLog`] and [`GraphRows`].
#[derive(Debug)]
pub struct Batch {
    /// Index of this batch's first commit in the whole log.
    pub start: usize,
    /// The commits.
    pub commits: CommitLog,
    /// One graph row per commit.
    pub graph: GraphRows,
}

/// How a walk ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WalkStats {
    /// Commits sent.
    pub commits: usize,
    /// Milliseconds until the first batch was sent.
    pub first_batch_ms: u64,
    /// Milliseconds for the whole walk.
    pub total_ms: u64,
    /// The walk stopped early: cancelled, or the callback asked to stop.
    pub stopped: bool,
}

/// The ids the [`Scope`] starts from, deduplicated, commits only.
pub fn tips(repo: &Repo, scope: &Scope) -> Result<Vec<Oid>> {
    let r = repo.local();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |id: Oid, out: &mut Vec<Oid>| {
        if seen.insert(id) {
            out.push(id);
        }
    };
    match scope {
        Scope::Head => {
            if let Some(id) = r.head_id().ok().map(|i| i.detach()) {
                add(id, &mut out);
            }
        }
        Scope::All => {
            if let Some(id) = r.head_id().ok().map(|i| i.detach()) {
                add(id, &mut out);
            }
            let platform = r.references().gix()?;
            for reference in platform.all().gix()?.peeled().gix()? {
                let Ok(reference) = reference else { continue };
                let name = reference.name().as_bstr();
                if !(name.starts_with(b"refs/heads/")
                    || name.starts_with(b"refs/remotes/")
                    || name.starts_with(b"refs/tags/"))
                {
                    continue;
                }
                let Some(id) = reference.try_id().map(|i| i.detach()) else {
                    continue;
                };
                // Tags can point at trees or blobs; only commits start a walk.
                match r.find_header(id) {
                    Ok(h) if h.kind() == gix::object::Kind::Commit => add(id, &mut out),
                    _ => {}
                }
            }
        }
        Scope::Revs(revs) => {
            for rev in revs {
                add(repo.resolve(rev)?, &mut out);
            }
        }
    }
    Ok(out)
}

/// Walk the log, sending batches to `on_batch` until the history ends, `cancel` is set,
/// the limit is reached, or `on_batch` returns `false`.
pub fn walk(
    repo: &Repo,
    opts: &LogOptions,
    cancel: &Cancel,
    mut on_batch: impl FnMut(Batch) -> bool,
) -> Result<WalkStats> {
    use gix::traverse::commit::topo;

    let started = Instant::now();
    let r = repo.local();
    let tips = tips(repo, &opts.scope)?;
    let mut stats = WalkStats::default();
    if tips.is_empty() {
        stats.first_batch_ms = started.elapsed().as_millis() as u64;
        stats.total_ms = stats.first_batch_ms;
        return Ok(stats);
    }
    let sorting = match opts.order {
        Order::Date => topo::Sorting::DateOrder,
        Order::Topo => topo::Sorting::TopoOrder,
    };
    // A shallow clone lacks the parents of its boundary commits: show those commits as
    // roots instead of failing on the missing objects.
    let shallow: std::collections::HashSet<Oid> = r
        .shallow_commits()
        .gix()?
        .map(|c| c.iter().copied().collect())
        .unwrap_or_default();
    let objects = ShallowCut {
        inner: &r.objects,
        shallow: &shallow,
    };
    let iter = topo::Builder::from_iters(&objects, tips, None::<Vec<Oid>>)
        .sorting(sorting)
        .with_commit_graph(r.commit_graph_if_enabled().gix()?)
        .build()
        .gix()?;
    let mailmap = r.open_mailmap();

    let mut layout = Layout::new();
    // Every id sent so far, for the layout's "parent already shown" check (a parent
    // can only come first in a broken or grafted history).
    let mut shown: std::collections::HashSet<Oid> = std::collections::HashSet::new();
    let mut buf = Vec::new();
    let mut want = opts.first_batch.max(1);
    let mut batch = CommitLog::new();
    let mut graph = GraphRows::new();
    let mut start = 0usize;
    let limit = opts.limit.unwrap_or(usize::MAX);

    for info in iter {
        if cancel.is_cancelled() {
            stats.stopped = true;
            break;
        }
        let info = info.gix()?;
        let commit = objects.find_commit(&info.id, &mut buf).gix()?;
        let author = commit.author().gix()?;
        let committer_time = commit.committer().map(|c| c.seconds()).unwrap_or(0);
        let (name, email) = match mailmap.try_resolve_ref(author) {
            Some(m) => (
                m.name.unwrap_or(author.name).to_str_lossy(),
                m.email.unwrap_or(author.email).to_str_lossy(),
            ),
            None => (author.name.to_str_lossy(), author.email.to_str_lossy()),
        };
        let summary = commit.message().summary();
        let summary = summary.to_str_lossy();
        layout.push(&info.id, &info.parent_ids, &mut graph, |p| {
            shown.contains(p)
        });
        shown.insert(info.id);
        batch.push(CommitData {
            id: info.id,
            parents: &info.parent_ids,
            author_time: author.seconds(),
            commit_time: committer_time,
            author_name: &name,
            author_email: &email,
            summary: &summary,
        });
        stats.commits += 1;

        if batch.len() >= want || stats.commits >= limit {
            let n = batch.len();
            let keep_going = on_batch(Batch {
                start,
                commits: std::mem::replace(&mut batch, CommitLog::new()),
                graph: std::mem::take(&mut graph),
            });
            if start == 0 {
                stats.first_batch_ms = started.elapsed().as_millis() as u64;
            }
            start += n;
            want = opts.batch.max(1);
            if !keep_going || stats.commits >= limit {
                stats.stopped = true;
                break;
            }
        }
    }
    if !batch.is_empty() && !stats.stopped {
        on_batch(Batch {
            start,
            commits: batch,
            graph,
        });
        if start == 0 {
            stats.first_batch_ms = started.elapsed().as_millis() as u64;
        }
    }
    stats.total_ms = started.elapsed().as_millis() as u64;
    tracing::debug!(
        repo = %repo.name(),
        commits = stats.commits,
        first_ms = stats.first_batch_ms,
        ms = stats.total_ms,
        "log walk"
    );
    Ok(stats)
}

/// The object database as the walk sees it: commits on a shallow clone's boundary have
/// their `parent` lines removed (their parents were never fetched).
struct ShallowCut<'a, F> {
    inner: &'a F,
    shallow: &'a std::collections::HashSet<Oid>,
}

impl<F: gix::objs::Find> gix::objs::Find for ShallowCut<'_, F> {
    fn try_find<'a>(
        &self,
        id: &gix::oid,
        buffer: &'a mut Vec<u8>,
    ) -> gix::Result<Option<gix::objs::Data<'a>>> {
        let (kind, hash) = match self.inner.try_find(id, buffer)? {
            Some(d) => (d.kind, d.object_hash),
            None => return Ok(None),
        };
        if kind == gix::object::Kind::Commit && self.shallow.contains(id) {
            strip_parents(buffer);
        }
        Ok(Some(gix::objs::Data::new(buffer, kind, hash)))
    }
}

/// Remove the `parent` lines from a raw commit's header.
fn strip_parents(raw: &mut Vec<u8>) {
    let header_end = raw
        .windows(2)
        .position(|w| w == b"\n\n")
        .map_or(raw.len(), |p| p + 1);
    let mut out = Vec::with_capacity(raw.len());
    let mut pos = 0;
    for line in raw[..header_end].split_inclusive(|b| *b == b'\n') {
        if !line.starts_with(b"parent ") {
            out.extend_from_slice(line);
        }
        pos += line.len();
    }
    out.extend_from_slice(&raw[pos..]);
    *raw = out;
}

/// Walk the whole log into one [`CommitLog`] and [`GraphRows`] (tests, small repos).
pub fn collect(repo: &Repo, opts: &LogOptions) -> Result<(CommitLog, GraphRows)> {
    let mut log = CommitLog::new();
    let mut graph = GraphRows::new();
    walk(repo, opts, &Cancel::default(), |b| {
        log.append(&b.commits);
        graph.append(&b.graph);
        true
    })?;
    Ok((log, graph))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    fn summaries(log: &CommitLog) -> Vec<&str> {
        (0..log.len()).map(|r| log.summary(r)).collect()
    }

    #[test]
    fn walks_all_refs_in_date_order() {
        let t = TestRepo::new();
        t.commit_file("a", "1", "one");
        t.git(&["checkout", "-q", "-b", "side"]);
        t.commit_file("b", "1", "side one");
        t.git(&["checkout", "-q", "main"]);
        t.commit_file("a", "2", "two");
        t.git(&["checkout", "-q", "side"]);
        t.commit_file("b", "2", "side two\n\nbody text");
        t.git(&["checkout", "-q", "main"]);
        t.git(&["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        t.git(&["tag", "v1"]);
        let repo = Repo::open(t.path()).unwrap();
        let (log, graph) = collect(&repo, &LogOptions::default()).unwrap();
        assert_eq!(
            summaries(&log),
            ["merge side", "side two", "two", "side one", "one"]
        );
        assert_eq!(graph.len(), log.len());
        assert_eq!(log.parents(0).len(), 2);
        assert!(log.parents(4).is_empty());
        assert_eq!(log.author(0).name, "Test");
        assert_eq!(log.authors().len(), 1);
        assert_eq!(log.row_of(&log.id(3)), Some(3));
        assert!(log.author_time(0) > log.author_time(4));
        // `git log --date-order` agrees.
        let git = t.git(&["log", "--all", "--date-order", "--format=%s"]);
        assert_eq!(git.lines().collect::<Vec<_>>(), summaries(&log));
    }

    #[test]
    fn shallow_clones_end_at_their_boundary() {
        let t = TestRepo::new();
        for i in 0..5 {
            t.commit_file("a", &i.to_string(), &format!("c{i}"));
        }
        let dest = tempfile::tempdir().unwrap();
        // `file://` makes git honor --depth; Windows paths need a third slash.
        let path = t.path().display().to_string().replace('\\', "/");
        let url = if path.starts_with('/') {
            format!("file://{path}")
        } else {
            format!("file:///{path}")
        };
        t.git(&[
            "clone",
            "-q",
            "--depth",
            "2",
            &url,
            &dest.path().join("s").to_string_lossy(),
        ]);
        let repo = Repo::open(&dest.path().join("s")).unwrap();
        let (log, graph) = collect(&repo, &LogOptions::default()).unwrap();
        assert_eq!(summaries(&log), ["c4", "c3"]);
        // The boundary commit is a root here.
        assert!(log.parents(1).is_empty());
        assert_eq!(graph.len(), 2);
    }

    #[test]
    fn strips_only_header_parent_lines() {
        let mut raw = b"tree 1\nparent 2\nparent 3\nauthor a\n\nparent in body\n".to_vec();
        strip_parents(&mut raw);
        assert_eq!(raw, b"tree 1\nauthor a\n\nparent in body\n");
    }

    #[test]
    fn batches_limit_and_cancel() {
        let t = TestRepo::new();
        for i in 0..25 {
            t.commit_file("a", &i.to_string(), &format!("c{i}"));
        }
        let repo = Repo::open(t.path()).unwrap();
        let opts = LogOptions {
            first_batch: 4,
            batch: 10,
            ..Default::default()
        };
        let mut sizes = Vec::new();
        let mut starts = Vec::new();
        let stats = walk(&repo, &opts, &Cancel::default(), |b| {
            sizes.push(b.commits.len());
            starts.push(b.start);
            assert_eq!(b.graph.len(), b.commits.len());
            true
        })
        .unwrap();
        assert_eq!(sizes, [4, 10, 10, 1]);
        assert_eq!(starts, [0, 4, 14, 24]);
        assert_eq!(stats.commits, 25);
        assert!(!stats.stopped);

        let limited = LogOptions {
            limit: Some(6),
            ..opts.clone()
        };
        let mut n = 0;
        let stats = walk(&repo, &limited, &Cancel::default(), |b| {
            n += b.commits.len();
            true
        })
        .unwrap();
        assert_eq!((n, stats.commits), (6, 6));

        // The callback stops the walk after the first batch.
        let mut batches = 0;
        let stats = walk(&repo, &opts, &Cancel::default(), |_| {
            batches += 1;
            false
        })
        .unwrap();
        assert_eq!((batches, stats.stopped), (1, true));

        let cancel = Cancel::default();
        cancel.cancel();
        let stats = walk(&repo, &opts, &cancel, |_| true).unwrap();
        assert_eq!((stats.commits, stats.stopped), (0, true));
    }

    #[test]
    fn scopes_and_mailmap() {
        let t = TestRepo::new();
        t.commit_file("a", "1", "one");
        t.git(&["checkout", "-q", "-b", "side"]);
        t.commit_file("b", "1", "side");
        t.git(&["checkout", "-q", "main"]);
        t.write(
            ".mailmap",
            "Real Name <real@example.com> <test@example.com>\n",
        );
        t.git(&["add", ".mailmap"]);
        t.git(&["commit", "-q", "-m", "mailmap"]);
        let repo = Repo::open(t.path()).unwrap();
        let head = LogOptions {
            scope: Scope::Head,
            ..Default::default()
        };
        let (log, _) = collect(&repo, &head).unwrap();
        assert_eq!(summaries(&log), ["mailmap", "one"]);
        assert_eq!(
            log.author(0),
            &Author {
                name: "Real Name".into(),
                email: "real@example.com".into()
            }
        );
        let revs = LogOptions {
            scope: Scope::Revs(vec!["side".into()]),
            ..Default::default()
        };
        let (log, _) = collect(&repo, &revs).unwrap();
        assert_eq!(summaries(&log), ["side", "one"]);
    }

    #[test]
    fn empty_repository() {
        let t = TestRepo::new();
        let repo = Repo::open(t.path()).unwrap();
        let (log, graph) = collect(&repo, &LogOptions::default()).unwrap();
        assert!(log.is_empty() && graph.is_empty());
    }
}
