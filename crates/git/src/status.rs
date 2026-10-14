//! Working-tree status from `git status --porcelain=v2 -z --branch`.
//!
//! Status goes through `git` rather than gitoxide for its fsmonitor and untracked-cache
//! support on big trees (see `docs/DECISIONS.md`). [`parse`] reads the NUL-separated v2
//! format: the branch header, ordinary, renamed/copied, unmerged, untracked and ignored
//! entries. [`Status`] splits them into the staged, unstaged and conflicted lists the
//! Local Changes view shows.

use std::path::PathBuf;

use crate::{Error, Oid, Repo, Result};

/// How one side of an entry (index or work tree) differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    /// No change (`.`).
    Unmodified,
    /// Content changed (`M`).
    Modified,
    /// File type changed, e.g. file to symlink (`T`).
    TypeChanged,
    /// Added (`A`).
    Added,
    /// Deleted (`D`).
    Deleted,
    /// Renamed (`R`).
    Renamed,
    /// Copied (`C`).
    Copied,
    /// Unmerged (`U`).
    Unmerged,
}

impl Change {
    fn from_char(c: u8) -> Result<Self> {
        Ok(match c {
            b'.' => Change::Unmodified,
            b'M' => Change::Modified,
            b'T' => Change::TypeChanged,
            b'A' => Change::Added,
            b'D' => Change::Deleted,
            b'R' => Change::Renamed,
            b'C' => Change::Copied,
            b'U' => Change::Unmerged,
            other => {
                return Err(Error::Parse(format!("status code {:?}", char::from(other))));
            }
        })
    }

    /// One-letter code (`M`, `A`...; `.` for none).
    pub fn letter(self) -> char {
        match self {
            Change::Unmodified => '.',
            Change::Modified => 'M',
            Change::TypeChanged => 'T',
            Change::Added => 'A',
            Change::Deleted => 'D',
            Change::Renamed => 'R',
            Change::Copied => 'C',
            Change::Unmerged => 'U',
        }
    }
}

/// How a path is in conflict (the `XY` of an unmerged entry).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Conflict {
    /// Deleted by both (`DD`).
    BothDeleted,
    /// Added by us (`AU`).
    AddedByUs,
    /// Deleted by them (`UD`).
    DeletedByThem,
    /// Added by them (`UA`).
    AddedByThem,
    /// Deleted by us (`DU`).
    DeletedByUs,
    /// Added by both (`AA`).
    BothAdded,
    /// Modified by both (`UU`).
    BothModified,
}

impl Conflict {
    fn from_xy(x: u8, y: u8) -> Result<Self> {
        Ok(match (x, y) {
            (b'D', b'D') => Conflict::BothDeleted,
            (b'A', b'U') => Conflict::AddedByUs,
            (b'U', b'D') => Conflict::DeletedByThem,
            (b'U', b'A') => Conflict::AddedByThem,
            (b'D', b'U') => Conflict::DeletedByUs,
            (b'A', b'A') => Conflict::BothAdded,
            (b'U', b'U') => Conflict::BothModified,
            _ => {
                return Err(Error::Parse(format!(
                    "unmerged code {}{}",
                    char::from(x),
                    char::from(y)
                )));
            }
        })
    }
}

/// What kind of entry a line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// Tracked and changed (`1`).
    Ordinary,
    /// Renamed or copied in the index (`2`), with git's similarity score (0–100).
    Renamed {
        /// Similarity percentage.
        score: u8,
    },
    /// In conflict (`u`).
    Unmerged(Conflict),
    /// Untracked (`?`).
    Untracked,
    /// Ignored (`!`), only when asked for.
    Ignored,
}

/// One changed path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Path relative to the work tree root.
    pub path: PathBuf,
    /// The path before a rename or copy.
    pub orig_path: Option<PathBuf>,
    /// Kind of entry.
    pub kind: EntryKind,
    /// Index (staged) side: HEAD to index.
    pub index: Change,
    /// Work tree (unstaged) side: index to work tree.
    pub worktree: Change,
    /// The path is a submodule.
    pub submodule: bool,
}

impl Entry {
    /// Has staged changes.
    pub fn is_staged(&self) -> bool {
        matches!(self.kind, EntryKind::Ordinary | EntryKind::Renamed { .. })
            && self.index != Change::Unmodified
    }

    /// Has unstaged changes (untracked files count).
    pub fn is_unstaged(&self) -> bool {
        match self.kind {
            EntryKind::Ordinary | EntryKind::Renamed { .. } => self.worktree != Change::Unmodified,
            EntryKind::Untracked => true,
            EntryKind::Unmerged(_) | EntryKind::Ignored => false,
        }
    }

    /// In conflict.
    pub fn conflict(&self) -> Option<Conflict> {
        match self.kind {
            EntryKind::Unmerged(c) => Some(c),
            _ => None,
        }
    }
}

/// The `# branch.*` header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchHeader {
    /// HEAD's commit; `None` before the first commit.
    pub oid: Option<Oid>,
    /// Branch name; `None` when detached.
    pub head: Option<String>,
    /// Upstream (`origin/main`).
    pub upstream: Option<String>,
    /// Commits ahead of the upstream.
    pub ahead: u32,
    /// Commits behind the upstream.
    pub behind: u32,
}

/// Parsed status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// Branch header.
    pub branch: BranchHeader,
    /// Every entry, in git's order (sorted by path).
    pub entries: Vec<Entry>,
}

impl Status {
    /// Entries with staged changes.
    pub fn staged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_staged())
    }

    /// Entries with unstaged changes, untracked files included.
    pub fn unstaged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.is_unstaged())
    }

    /// Entries in conflict.
    pub fn conflicted(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.conflict().is_some())
    }

    /// No changes at all (ignored files aside).
    pub fn is_clean(&self) -> bool {
        self.entries.iter().all(|e| e.kind == EntryKind::Ignored)
    }
}

/// Which untracked files to list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Untracked {
    /// None.
    No,
    /// Untracked directories as one entry.
    Normal,
    /// Every untracked file.
    #[default]
    All,
}

/// Options for [`status`].
#[derive(Clone, Copy, Debug, Default)]
pub struct StatusOptions {
    /// Untracked files.
    pub untracked: Untracked,
    /// Also list ignored files.
    pub ignored: bool,
}

/// Run `git status` and parse it.
pub fn status(repo: &Repo, opts: StatusOptions) -> Result<Status> {
    let untracked = match opts.untracked {
        Untracked::No => "--untracked-files=no",
        Untracked::Normal => "--untracked-files=normal",
        Untracked::All => "--untracked-files=all",
    };
    let mut args = vec!["status", "--porcelain=v2", "-z", "--branch", untracked];
    if opts.ignored {
        args.push("--ignored=matching");
    }
    let out = repo.git()?.run(&args)?;
    parse(&out.stdout)
}

fn path_of(bytes: &[u8]) -> PathBuf {
    gix::path::from_bstr(gix::bstr::BStr::new(bytes))
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_else(|_| PathBuf::from(String::from_utf8_lossy(bytes).into_owned()))
}

/// Split off the first `n` space-separated fields; the rest (a path) may contain spaces.
fn fields(line: &[u8], n: usize) -> Result<(Vec<&[u8]>, &[u8])> {
    let mut out = Vec::with_capacity(n);
    let mut rest = line;
    for _ in 0..n {
        let i = rest
            .iter()
            .position(|b| *b == b' ')
            .ok_or_else(|| Error::Parse(String::from_utf8_lossy(line).into_owned()))?;
        out.push(&rest[..i]);
        rest = &rest[i + 1..];
    }
    Ok((out, rest))
}

fn xy(field: &[u8]) -> Result<(u8, u8)> {
    match field {
        [x, y] => Ok((*x, *y)),
        _ => Err(Error::Parse(format!(
            "status XY {:?}",
            String::from_utf8_lossy(field)
        ))),
    }
}

fn is_submodule(field: &[u8]) -> bool {
    field.first() == Some(&b'S')
}

/// Parse `git status --porcelain=v2 -z --branch` output.
pub fn parse(out: &[u8]) -> Result<Status> {
    let mut status = Status::default();
    let mut records = out.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(rec) = records.next() {
        let text = || String::from_utf8_lossy(rec).into_owned();
        match rec[0] {
            b'#' => {
                let line = String::from_utf8_lossy(rec);
                let mut words = line.splitn(3, ' ');
                let (_, key, value) = (words.next(), words.next(), words.next().unwrap_or(""));
                let b = &mut status.branch;
                match key {
                    Some("branch.oid") if value != "(initial)" => {
                        b.oid = Some(
                            Oid::from_hex(value.as_bytes())
                                .map_err(|_| Error::Parse(line.to_string()))?,
                        );
                    }
                    Some("branch.head") if value != "(detached)" => {
                        b.head = Some(value.to_owned());
                    }
                    Some("branch.upstream") => b.upstream = Some(value.to_owned()),
                    Some("branch.ab") => {
                        for n in value.split(' ') {
                            if let Some(a) = n.strip_prefix('+') {
                                b.ahead = a.parse().unwrap_or(0);
                            } else if let Some(n) = n.strip_prefix('-') {
                                b.behind = n.parse().unwrap_or(0);
                            }
                        }
                    }
                    _ => {}
                }
            }
            b'1' => {
                // 1 XY sub mH mI mW hH hI path
                let (f, path) = fields(rec, 8)?;
                let (x, y) = xy(f[1])?;
                status.entries.push(Entry {
                    path: path_of(path),
                    orig_path: None,
                    kind: EntryKind::Ordinary,
                    index: Change::from_char(x)?,
                    worktree: Change::from_char(y)?,
                    submodule: is_submodule(f[2]),
                });
            }
            b'2' => {
                // 2 XY sub mH mI mW hH hI Xscore path NUL origPath
                let (f, path) = fields(rec, 9)?;
                let (x, y) = xy(f[1])?;
                let score = f[8]
                    .get(1..)
                    .and_then(|s| std::str::from_utf8(s).ok())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let orig = records.next().ok_or_else(|| Error::Parse(text()))?;
                status.entries.push(Entry {
                    path: path_of(path),
                    orig_path: Some(path_of(orig)),
                    kind: EntryKind::Renamed { score },
                    index: Change::from_char(x)?,
                    worktree: Change::from_char(y)?,
                    submodule: is_submodule(f[2]),
                });
            }
            b'u' => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let (f, path) = fields(rec, 10)?;
                let (x, y) = xy(f[1])?;
                status.entries.push(Entry {
                    path: path_of(path),
                    orig_path: None,
                    kind: EntryKind::Unmerged(Conflict::from_xy(x, y)?),
                    index: Change::Unmerged,
                    worktree: Change::Unmerged,
                    submodule: is_submodule(f[2]),
                });
            }
            b'?' | b'!' => {
                let path = rec.get(2..).ok_or_else(|| Error::Parse(text()))?;
                let (kind, change) = if rec[0] == b'?' {
                    (EntryKind::Untracked, Change::Added)
                } else {
                    (EntryKind::Ignored, Change::Unmodified)
                };
                status.entries.push(Entry {
                    path: path_of(path),
                    orig_path: None,
                    kind,
                    index: Change::Unmodified,
                    worktree: change,
                    submodule: false,
                });
            }
            _ => return Err(Error::Parse(text())),
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    fn paths<'a>(it: impl Iterator<Item = &'a Entry>) -> Vec<String> {
        it.map(|e| e.path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn parses_every_entry_kind() {
        let out = b"# branch.oid 1111111111111111111111111111111111111111\0\
# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 M. N... 100644 100644 100644 aaaa bbbb staged.txt\0\
1 .M N... 100644 100644 100644 aaaa aaaa with space.txt\0\
1 MD N... 100644 100644 000000 aaaa bbbb both.txt\0\
1 .M SC.. 160000 160000 160000 aaaa aaaa sub\0\
2 R. N... 100644 100644 100644 aaaa aaaa R87 new name.txt\0old name.txt\0\
u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflict.txt\0\
? untracked.txt\0! ignored.log\0";
        let s = parse(out).unwrap();
        assert_eq!(
            s.branch,
            BranchHeader {
                oid: Some(Oid::from_hex(b"1111111111111111111111111111111111111111").unwrap()),
                head: Some("main".into()),
                upstream: Some("origin/main".into()),
                ahead: 2,
                behind: 1,
            }
        );
        assert_eq!(
            paths(s.staged()),
            ["staged.txt", "both.txt", "new name.txt"]
        );
        assert_eq!(
            paths(s.unstaged()),
            ["with space.txt", "both.txt", "sub", "untracked.txt"]
        );
        assert_eq!(paths(s.conflicted()), ["conflict.txt"]);
        let renamed = &s.entries[4];
        assert_eq!(renamed.kind, EntryKind::Renamed { score: 87 });
        assert_eq!(renamed.orig_path, Some(PathBuf::from("old name.txt")));
        assert!(s.entries[3].submodule);
        assert_eq!(s.entries[2].worktree, Change::Deleted);
        assert_eq!(s.entries[5].conflict(), Some(Conflict::BothModified));
        assert_eq!(s.entries[7].kind, EntryKind::Ignored);
        assert!(!s.is_clean());
    }

    #[test]
    fn detached_and_initial_headers() {
        let s = parse(b"# branch.oid (initial)\0# branch.head (detached)\0").unwrap();
        assert_eq!(s.branch, BranchHeader::default());
        assert!(s.is_clean());
        assert!(parse(b"X what\0").is_err());
        assert!(parse(b"1 M.\0").is_err());
    }

    #[test]
    fn reads_a_real_work_tree() {
        let t = TestRepo::new();
        t.commit_file("a.txt", "a\n", "one");
        t.commit_file("gone.txt", "g\n", "two");
        t.commit_file("old.txt", "some content that is long enough\n", "three");
        t.write("a.txt", "a2\n");
        t.git(&["add", "a.txt"]);
        t.write("a.txt", "a3\n");
        t.git(&["rm", "-q", "gone.txt"]);
        t.git(&["mv", "old.txt", "new.txt"]);
        t.write("dir/untracked.txt", "u\n");
        t.write("ignored.log", "i\n");
        t.write(".gitignore", "*.log\n");
        let repo = Repo::open(t.path()).unwrap();
        let s = status(&repo, StatusOptions::default()).unwrap();
        assert_eq!(s.branch.head.as_deref(), Some("main"));
        assert!(s.branch.oid.is_some());
        assert_eq!(paths(s.staged()), ["a.txt", "gone.txt", "new.txt"]);
        assert_eq!(
            paths(s.unstaged()),
            ["a.txt", ".gitignore", "dir/untracked.txt"]
        );
        let renamed = s.staged().find(|e| e.orig_path.is_some()).unwrap();
        assert_eq!(renamed.orig_path, Some(PathBuf::from("old.txt")));
        let with_ignored = status(
            &repo,
            StatusOptions {
                ignored: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            with_ignored
                .entries
                .iter()
                .any(|e| e.kind == EntryKind::Ignored
                    && e.path.as_path() == std::path::Path::new("ignored.log"))
        );
    }

    #[test]
    fn reads_merge_conflicts() {
        let t = TestRepo::new();
        t.commit_file("f.txt", "base\n", "base");
        t.git(&["checkout", "-q", "-b", "other"]);
        t.commit_file("f.txt", "theirs\n", "theirs");
        t.git(&["checkout", "-q", "main"]);
        t.commit_file("f.txt", "ours\n", "ours");
        let out = std::process::Command::new("git")
            .args(["merge", "-q", "other"])
            .current_dir(t.path())
            .env("HOME", t.path())
            .output()
            .unwrap();
        assert!(!out.status.success());
        let repo = Repo::open(t.path()).unwrap();
        let s = status(&repo, StatusOptions::default()).unwrap();
        assert_eq!(paths(s.conflicted()), ["f.txt"]);
        assert_eq!(s.staged().count(), 0);
        assert_eq!(s.unstaged().count(), 0);
    }
}
