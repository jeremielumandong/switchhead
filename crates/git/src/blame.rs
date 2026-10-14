//! Blame and file history.
//!
//! [`blame`] streams `git blame --incremental`: git reports line ranges as it resolves
//! them (recent changes first), so the view fills in while older history is still being
//! searched. Commits are stored once ([`BlameCommit`]); every line holds an index into
//! them. [`file_history`] lists the commits that touched a file, following renames.

use std::path::{Path, PathBuf};

use crate::cli::Cancel;
use crate::{Error, Oid, Repo, Result, path_from_bytes};

/// A commit that blame attributes lines to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameCommit {
    /// Commit id.
    pub id: Oid,
    /// Author name.
    pub author: String,
    /// Author email (without `<>`).
    pub author_email: String,
    /// Author time (seconds since the epoch).
    pub author_time: i64,
    /// First line of the message.
    pub summary: String,
    /// The commit before it and the path there, for "blame the previous revision".
    pub previous: Option<(Oid, PathBuf)>,
    /// The path in this commit (differs from the blamed path across renames).
    pub path: PathBuf,
    /// A boundary commit (the root, or the edge of a limited range).
    pub boundary: bool,
}

/// A run of final lines attributed to one commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// Index into [`Blame::commits`].
    pub commit: usize,
    /// First line in the blamed file (1-based).
    pub final_line: u32,
    /// The same line in the commit's version of the file (1-based).
    pub orig_line: u32,
    /// Number of lines.
    pub lines: u32,
}

/// Blame of a file: commits, and per line the commit it comes from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    /// Commits, in the order git reported them.
    pub commits: Vec<BlameCommit>,
    /// For final line `n` (0-based), its commit index; `u32::MAX` until resolved.
    line_commit: Vec<u32>,
    /// For final line `n` (0-based), its line in that commit (1-based).
    line_orig: Vec<u32>,
}

impl Blame {
    /// Number of lines known so far.
    pub fn len(&self) -> usize {
        self.line_commit.len()
    }

    /// No lines.
    pub fn is_empty(&self) -> bool {
        self.line_commit.is_empty()
    }

    /// The commit of 0-based line `n`, once resolved.
    pub fn commit_of(&self, n: usize) -> Option<&BlameCommit> {
        let i = *self.line_commit.get(n)?;
        self.commits.get(i as usize)
    }

    /// 0-based line `n`'s line number in its commit.
    pub fn orig_line(&self, n: usize) -> Option<u32> {
        self.line_orig.get(n).copied().filter(|l| *l != 0)
    }

    /// Record a chunk.
    pub fn apply(&mut self, c: &Chunk) {
        let end = (c.final_line + c.lines - 1) as usize;
        if self.line_commit.len() < end {
            self.line_commit.resize(end, u32::MAX);
            self.line_orig.resize(end, 0);
        }
        for k in 0..c.lines {
            let n = (c.final_line + k - 1) as usize;
            self.line_commit[n] = c.commit as u32;
            self.line_orig[n] = c.orig_line + k;
        }
    }
}

/// Incremental parser for `git blame --incremental` and `--porcelain` output.
#[derive(Debug, Default)]
pub struct Parser {
    blame: Blame,
    index: std::collections::HashMap<Oid, usize>,
    /// The chunk whose header was read and whose `filename` line has not come yet.
    pending: Option<Chunk>,
}

impl Parser {
    /// A parser with nothing read.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one output line. Returns the chunk it completed, if any.
    pub fn line(&mut self, line: &[u8]) -> Result<Option<Chunk>> {
        // Porcelain content lines start with a tab; the text is not stored here.
        if line.first() == Some(&b'\t') {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(line);
        let (key, value) = text.split_once(' ').unwrap_or((&text, ""));
        if let Some(c) = self.pending.as_mut() {
            let commit = &mut self.blame.commits[c.commit];
            match key {
                "author" => commit.author = value.to_owned(),
                "author-mail" => {
                    commit.author_email = value.trim_matches(['<', '>']).to_owned();
                }
                "author-time" => commit.author_time = value.parse().unwrap_or(0),
                "summary" => commit.summary = value.to_owned(),
                "boundary" => commit.boundary = true,
                "previous" => {
                    if let Some((id, path)) = value.split_once(' ')
                        && let Ok(id) = Oid::from_hex(id.as_bytes())
                    {
                        commit.previous = Some((id, PathBuf::from(path)));
                    }
                }
                "filename" => {
                    commit.path = path_from_bytes(&line[9..]);
                    let c = self.pending.take();
                    if let Some(c) = &c {
                        self.blame.apply(c);
                    }
                    return Ok(c);
                }
                _ => {
                    if key.len() == 40 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
                        // Porcelain repeats only the header for a known commit and no
                        // `filename` line: the previous chunk is complete.
                        let done = self.pending.take();
                        if let Some(c) = &done {
                            self.blame.apply(c);
                        }
                        self.header(key, value)?;
                        return Ok(done);
                    }
                }
            }
            return Ok(None);
        }
        if key.len() == 40 {
            self.header(key, value)?;
            return Ok(None);
        }
        Err(Error::Parse(format!("blame: {text}")))
    }

    fn header(&mut self, id: &str, rest: &str) -> Result<()> {
        let bad = || Error::Parse(format!("blame header: {id} {rest}"));
        let id = Oid::from_hex(id.as_bytes()).map_err(|_| bad())?;
        let mut nums = rest.split(' ').map(|n| n.parse::<u32>());
        let orig = nums.next().and_then(|n| n.ok()).ok_or_else(bad)?;
        let fin = nums.next().and_then(|n| n.ok()).ok_or_else(bad)?;
        let lines = nums.next().and_then(|n| n.ok()).unwrap_or(1);
        let commit = match self.index.get(&id) {
            Some(i) => *i,
            None => {
                self.blame.commits.push(BlameCommit {
                    id,
                    author: String::new(),
                    author_email: String::new(),
                    author_time: 0,
                    summary: String::new(),
                    previous: None,
                    path: PathBuf::new(),
                    boundary: false,
                });
                let i = self.blame.commits.len() - 1;
                self.index.insert(id, i);
                i
            }
        };
        self.pending = Some(Chunk {
            commit,
            final_line: fin,
            orig_line: orig,
            lines,
        });
        Ok(())
    }

    /// The blame so far.
    pub fn blame(&self) -> &Blame {
        &self.blame
    }

    /// Finish (porcelain output ends without a closing `filename`).
    pub fn finish(mut self) -> Blame {
        if let Some(c) = self.pending.take() {
            self.blame.apply(&c);
        }
        self.blame
    }
}

/// Blame `path` at `rev` (`None`: the work tree file), sending each resolved chunk with
/// its commit to `on_chunk` as git reports it. Returns the full blame.
pub fn blame(
    repo: &Repo,
    path: &Path,
    rev: Option<&str>,
    cancel: &Cancel,
    mut on_chunk: impl FnMut(&Chunk, &BlameCommit),
) -> Result<Blame> {
    let mut args: Vec<std::ffi::OsString> = vec!["blame".into(), "--incremental".into()];
    if let Some(rev) = rev {
        args.push(rev.into());
    }
    args.push("--".into());
    args.push(path.as_os_str().to_owned());
    let mut parser = Parser::new();
    let mut err = None;
    repo.git()?
        .run_lines(&args, cancel, |line| match parser.line(line) {
            Ok(Some(c)) => {
                on_chunk(&c, &parser.blame().commits[c.commit]);
                true
            }
            Ok(None) => true,
            Err(e) => {
                err = Some(e);
                false
            }
        })?;
    if let Some(e) = err {
        return Err(e);
    }
    Ok(parser.finish())
}

/// A commit in a file's history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileVersion {
    /// The commit.
    pub id: Oid,
    /// What happened to the file (`A`, `M`, `D`, `R`, `C`, `T`).
    pub status: char,
    /// The file's path in this commit.
    pub path: PathBuf,
    /// The path before, for a rename or copy.
    pub old_path: Option<PathBuf>,
    /// Author name.
    pub author: String,
    /// Author time (seconds since the epoch).
    pub author_time: i64,
    /// First line of the message.
    pub summary: String,
}

/// The commits that changed `path`, newest first, following renames (`log --follow`).
pub fn file_history(repo: &Repo, path: &Path, rev: Option<&str>) -> Result<Vec<FileVersion>> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "log".into(),
        "--follow".into(),
        "-z".into(),
        "--name-status".into(),
        "--format=%x01%H%x02%an%x02%at%x02%s".into(),
    ];
    if let Some(rev) = rev {
        args.push(rev.into());
    }
    args.push("--".into());
    args.push(path.as_os_str().to_owned());
    let out = repo.git()?.run(&args)?;
    parse_history(&out.stdout)
}

/// Parse `log -z --name-status --format=%x01%H%x02%an%x02%at%x02%s` output:
/// `\x01<id>\x02<author>\x02<time>\x02<summary>\0\n<status>\0<path>\0` with a second
/// path after renames and copies.
fn parse_history(out: &[u8]) -> Result<Vec<FileVersion>> {
    let mut v = Vec::new();
    let mut fields = out.split(|b| *b == 0).peekable();
    while let Some(f) = fields.next() {
        let Some(header) = f.strip_prefix(b"\x01") else {
            continue;
        };
        let mut parts = header.splitn(4, |b| *b == 2);
        let hex = parts.next().unwrap_or_default();
        let id = Oid::from_hex(hex).map_err(|_| Error::Parse("file history id".into()))?;
        let mut text = || String::from_utf8_lossy(parts.next().unwrap_or_default()).into_owned();
        let (author, time, summary) = (text(), text(), text());
        let Some(status) = fields.next() else { break };
        let status = status.trim_ascii();
        let Some(&code) = status.first() else {
            continue;
        };
        let first = fields.next().map(path_from_bytes).unwrap_or_default();
        let (path, old_path) = if matches!(code, b'R' | b'C') {
            let second = fields.next().map(path_from_bytes).unwrap_or_default();
            (second, Some(first))
        } else {
            (first, None)
        };
        v.push(FileVersion {
            id,
            status: char::from(code),
            path,
            old_path,
            author,
            author_time: time.parse().unwrap_or(0),
            summary,
        });
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn blames_lines_incrementally() {
        let t = TestRepo::new();
        let first = t.commit_file("a.txt", "one\ntwo\nthree\n", "first");
        let second = t.commit_file("a.txt", "one\nTWO\nthree\nfour\n", "second");
        let repo = Repo::open(t.path()).unwrap();
        let mut chunks = Vec::new();
        let b = blame(
            &repo,
            Path::new("a.txt"),
            None,
            &Cancel::default(),
            |c, commit| {
                chunks.push((c.final_line, c.lines, commit.summary.clone()));
            },
        )
        .unwrap();
        assert!(!chunks.is_empty());
        assert_eq!(b.len(), 4);
        let ids: Vec<String> = (0..4)
            .map(|n| b.commit_of(n).unwrap().id.to_string())
            .collect();
        // The work tree matches HEAD, so lines come from the two commits.
        assert_eq!(ids, [&first, &second, &first, &second].map(String::clone));
        let c = b.commit_of(1).unwrap();
        assert_eq!(
            (
                c.author.as_str(),
                c.author_email.as_str(),
                c.summary.as_str()
            ),
            ("Test", "test@example.com", "second")
        );
        assert_eq!(
            c.previous.as_ref().map(|p| p.0.to_string()),
            Some(first.clone())
        );
        assert!(b.commit_of(0).unwrap().boundary);
        assert_eq!(b.orig_line(2), Some(3));

        // Uncommitted lines belong to the all-zero id.
        t.write("a.txt", "one\nTWO\nthree\nfour\nfive\n");
        let b = blame(
            &repo,
            Path::new("a.txt"),
            None,
            &Cancel::default(),
            |_, _| {},
        )
        .unwrap();
        assert!(b.commit_of(4).unwrap().id.is_null());
        // At a revision.
        let b = blame(
            &repo,
            Path::new("a.txt"),
            Some("HEAD~1"),
            &Cancel::default(),
            |_, _| {},
        )
        .unwrap();
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn parses_porcelain_output() {
        let id = "a".repeat(40);
        let out = format!(
            "{id} 1 1 2\nauthor A\nauthor-mail <a@x>\nauthor-time 5\nsummary s\nfilename f.txt\n\tline1\n{id} 2 2\n\tline2\n"
        );
        let mut p = Parser::new();
        for l in out.lines() {
            p.line(l.as_bytes()).unwrap();
        }
        let b = p.finish();
        assert_eq!(b.len(), 2);
        assert_eq!(b.commits.len(), 1);
        assert_eq!(b.commit_of(1).unwrap().author_time, 5);
        assert!(Parser::new().line(b"garbage").is_err());
    }

    #[test]
    fn history_follows_renames() {
        let t = TestRepo::new();
        let c1 = t.commit_file("a.txt", "content that stays the same\nline\n", "add");
        t.git(&["mv", "a.txt", "b.txt"]);
        t.git(&["commit", "-q", "-m", "rename"]);
        let c3 = t.commit_file("b.txt", "content that stays the same\nline\nmore\n", "edit");
        t.commit_file("other.txt", "x", "unrelated");
        let repo = Repo::open(t.path()).unwrap();
        let h = file_history(&repo, Path::new("b.txt"), None).unwrap();
        let got: Vec<(char, PathBuf, Option<PathBuf>)> = h
            .iter()
            .map(|v| (v.status, v.path.clone(), v.old_path.clone()))
            .collect();
        assert_eq!(
            got,
            [
                ('M', "b.txt".into(), None),
                ('R', "b.txt".into(), Some("a.txt".into())),
                ('A', "a.txt".into(), None),
            ]
        );
        assert_eq!(h[0].id.to_string(), c3);
        assert_eq!(
            (h[0].author.as_str(), h[0].summary.as_str()),
            ("Test", "edit")
        );
        assert!(h[0].author_time > 0);
        // The file as it was in each version.
        let at = |v: &FileVersion| {
            crate::diff::read_at(&repo, &v.path, crate::diff::Source::Commit(v.id)).unwrap()
        };
        assert_eq!(
            at(&h[2]).as_deref(),
            Some(&b"content that stays the same\nline\n"[..])
        );
        assert_eq!(
            crate::diff::read_at(
                &repo,
                Path::new("b.txt"),
                crate::diff::Source::Commit(h[2].id)
            )
            .unwrap(),
            None
        );
        assert_eq!(h[2].id.to_string(), c1);
    }
}
