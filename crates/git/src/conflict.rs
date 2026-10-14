//! Merge conflicts: conflict markers parsed into regions, resolved per region, and the
//! index stages (`:1:` base, `:2:` ours, `:3:` theirs) of a conflicted path.
//!
//! [`parse`] understands git's `merge`, `diff3` and `zdiff3` marker styles and any
//! `conflict-marker-size` of seven or more (the size is taken from the first marker).
//! Line endings are kept exactly, so a file resolved to "ours" everywhere is byte for
//! byte the ours side.

use std::path::Path;

use crate::{Error, Oid, Repo, Result};

/// One conflicted region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// Our side's lines.
    pub ours: String,
    /// The merge base's lines (`diff3` / `zdiff3` styles only).
    pub base: Option<String>,
    /// Their side's lines.
    pub theirs: String,
    /// Label after `<<<<<<<` (usually `HEAD`).
    pub ours_label: String,
    /// Label after `|||||||`.
    pub base_label: String,
    /// Label after `>>>>>>>` (the merged branch or commit).
    pub theirs_label: String,
    /// 1-based line of the `<<<<<<<` marker in the file.
    pub line: usize,
}

/// A piece of a conflicted file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Region {
    /// Text both sides agree on.
    Clean(String),
    /// A conflict.
    Conflict(Conflict),
}

/// How to resolve one conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Keep our side.
    Ours,
    /// Keep their side.
    Theirs,
    /// Keep the merge base (drop both changes).
    Base,
    /// Ours, then theirs.
    OursThenTheirs,
    /// Theirs, then ours.
    TheirsThenOurs,
    /// This text.
    Custom(String),
}

/// A file with conflict markers, split into regions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConflictFile {
    /// Regions in file order.
    pub regions: Vec<Region>,
    marker_size: usize,
}

impl ConflictFile {
    /// The conflicts, in order.
    pub fn conflicts(&self) -> impl Iterator<Item = &Conflict> {
        self.regions.iter().filter_map(|r| match r {
            Region::Conflict(c) => Some(c),
            Region::Clean(_) => None,
        })
    }

    /// Number of conflicts.
    pub fn count(&self) -> usize {
        self.conflicts().count()
    }

    /// The file with conflict `i` resolved by `choices[i]`; a `None` choice (or a missing
    /// one) keeps that conflict's markers.
    pub fn resolve(&self, choices: &[Option<Choice>]) -> String {
        let mut out = String::new();
        let mut n = 0;
        for r in &self.regions {
            match r {
                Region::Clean(t) => out.push_str(t),
                Region::Conflict(c) => {
                    match choices.get(n).cloned().flatten() {
                        Some(Choice::Ours) => out.push_str(&c.ours),
                        Some(Choice::Theirs) => out.push_str(&c.theirs),
                        Some(Choice::Base) => out.push_str(c.base.as_deref().unwrap_or("")),
                        Some(Choice::OursThenTheirs) => {
                            out.push_str(&c.ours);
                            out.push_str(&c.theirs);
                        }
                        Some(Choice::TheirsThenOurs) => {
                            out.push_str(&c.theirs);
                            out.push_str(&c.ours);
                        }
                        Some(Choice::Custom(t)) => out.push_str(&t),
                        None => self.write_markers(c, &mut out),
                    }
                    n += 1;
                }
            }
        }
        out
    }

    /// Resolve every conflict the same way.
    pub fn resolve_all(&self, choice: Choice) -> String {
        self.resolve(&vec![Some(choice); self.count()])
    }

    fn write_markers(&self, c: &Conflict, out: &mut String) {
        let size = self.marker_size.max(7);
        let marker = |ch: char, label: &str, out: &mut String| {
            out.extend(std::iter::repeat_n(ch, size));
            if !label.is_empty() {
                out.push(' ');
                out.push_str(label);
            }
            out.push('\n');
        };
        marker('<', &c.ours_label, out);
        out.push_str(&c.ours);
        if let Some(base) = &c.base {
            marker('|', &c.base_label, out);
            out.push_str(base);
        }
        marker('=', "", out);
        out.push_str(&c.theirs);
        marker('>', &c.theirs_label, out);
    }
}

/// If `line` is a marker of `ch` repeated `size` times (any size ≥ 7 when `size` is 0),
/// return its size and label.
fn marker(line: &str, ch: char, size: usize) -> Option<(usize, String)> {
    let body = line.trim_end_matches(['\n', '\r']);
    let run = body.chars().take_while(|c| *c == ch).count();
    if run < 7 || (size != 0 && run != size) {
        return None;
    }
    let rest = &body[run..];
    if rest.is_empty() {
        return Some((run, String::new()));
    }
    // `=======` has no label; the others are followed by a space.
    if ch == '=' {
        return rest.trim().is_empty().then(|| (run, String::new()));
    }
    rest.strip_prefix(' ').map(|l| (run, l.to_owned()))
}

/// Split a file with conflict markers into regions.
pub fn parse(text: &str) -> Result<ConflictFile> {
    #[derive(PartialEq)]
    enum At {
        Clean,
        Ours,
        Base,
        Theirs,
    }
    let mut file = ConflictFile::default();
    let mut clean = String::new();
    let mut cur: Option<Conflict> = None;
    let mut at = At::Clean;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let size = file.marker_size;
        match at {
            At::Clean => {
                if let Some((n, label)) = marker(line, '<', size) {
                    file.marker_size = n;
                    if !clean.is_empty() {
                        file.regions.push(Region::Clean(std::mem::take(&mut clean)));
                    }
                    cur = Some(Conflict {
                        ours: String::new(),
                        base: None,
                        theirs: String::new(),
                        ours_label: label,
                        base_label: String::new(),
                        theirs_label: String::new(),
                        line: i + 1,
                    });
                    at = At::Ours;
                } else {
                    clean.push_str(line);
                }
            }
            At::Ours | At::Base => {
                let Some(c) = cur.as_mut() else {
                    return Err(Error::Parse("conflict state".into()));
                };
                if at == At::Ours
                    && let Some((_, label)) = marker(line, '|', size)
                {
                    c.base = Some(String::new());
                    c.base_label = label;
                    at = At::Base;
                } else if marker(line, '=', size).is_some() {
                    at = At::Theirs;
                } else if at == At::Ours {
                    c.ours.push_str(line);
                } else if let Some(b) = c.base.as_mut() {
                    b.push_str(line);
                }
            }
            At::Theirs => {
                let Some(c) = cur.as_mut() else {
                    return Err(Error::Parse("conflict state".into()));
                };
                if let Some((_, label)) = marker(line, '>', size) {
                    c.theirs_label = label;
                    if let Some(c) = cur.take() {
                        file.regions.push(Region::Conflict(c));
                    }
                    at = At::Clean;
                } else {
                    c.theirs.push_str(line);
                }
            }
        }
    }
    if at != At::Clean {
        let line = cur.map(|c| c.line).unwrap_or(0);
        return Err(Error::Parse(format!(
            "conflict starting at line {line} has no end marker"
        )));
    }
    if !clean.is_empty() {
        file.regions.push(Region::Clean(clean));
    }
    Ok(file)
}

/// The index stages of a conflicted path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stages {
    /// Stage 1: the merge base (`None`: added on both sides).
    pub base: Option<Oid>,
    /// Stage 2: ours (`None`: deleted by us).
    pub ours: Option<Oid>,
    /// Stage 3: theirs (`None`: deleted by them).
    pub theirs: Option<Oid>,
}

/// Read the conflict stages of `path` from the index (`git ls-files -u`).
pub fn stages(repo: &Repo, path: &Path) -> Result<Stages> {
    let out = repo.git()?.run(&[
        std::ffi::OsStr::new("ls-files"),
        std::ffi::OsStr::new("-u"),
        std::ffi::OsStr::new("-z"),
        std::ffi::OsStr::new("--"),
        path.as_os_str(),
    ])?;
    parse_stages(&out.stdout)
}

/// Parse `git ls-files -u -z` records: `mode id stage\tpath\0`.
fn parse_stages(out: &[u8]) -> Result<Stages> {
    let mut s = Stages::default();
    for rec in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let text = String::from_utf8_lossy(rec);
        let meta = text.split('\t').next().unwrap_or("");
        let mut parts = meta.split(' ');
        let (_mode, id, stage) = (parts.next(), parts.next(), parts.next());
        let id = id
            .and_then(|h| Oid::from_hex(h.as_bytes()).ok())
            .ok_or_else(|| Error::Parse(text.to_string()))?;
        match stage {
            Some("1") => s.base = Some(id),
            Some("2") => s.ours = Some(id),
            Some("3") => s.theirs = Some(id),
            _ => return Err(Error::Parse(text.to_string())),
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::read_blob;
    use crate::testutil::TestRepo;

    fn conflicted(style: &str) -> (TestRepo, Repo) {
        let t = TestRepo::new();
        t.git(&["config", "merge.conflictStyle", style]);
        t.commit_file("f.txt", "top\nbase\nm1\nm2\nm3\nm4\nbase2\nend\n", "base");
        t.git(&["checkout", "-q", "-b", "other"]);
        t.commit_file(
            "f.txt",
            "top\ntheirs\nm1\nm2\nm3\nm4\ntheirs2\nend\n",
            "theirs",
        );
        t.git(&["checkout", "-q", "main"]);
        t.commit_file("f.txt", "top\nours\nm1\nm2\nm3\nm4\nours2\nend\n", "ours");
        let out = std::process::Command::new("git")
            .args(["merge", "-q", "other"])
            .current_dir(t.path())
            .env("HOME", t.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(!out.status.success());
        let repo = Repo::open(t.path()).unwrap();
        (t, repo)
    }

    #[test]
    fn parses_and_resolves_every_style() {
        for style in ["merge", "diff3", "zdiff3"] {
            let (t, _repo) = conflicted(style);
            let text = std::fs::read_to_string(t.join("f.txt")).unwrap();
            let f = parse(&text).unwrap();
            assert_eq!(f.count(), 2, "{style}");
            let c = f.conflicts().next().unwrap();
            assert_eq!((c.ours.as_str(), c.theirs.as_str()), ("ours\n", "theirs\n"));
            assert_eq!(c.ours_label, "HEAD");
            assert_eq!(c.theirs_label, "other");
            assert_eq!(c.line, 2);
            if style == "merge" {
                assert_eq!(c.base, None);
            } else {
                assert_eq!(c.base.as_deref(), Some("base\n"), "{style}");
            }
            // Unresolved, the file renders back exactly.
            assert_eq!(f.resolve(&[]), text, "{style}");
            assert_eq!(
                f.resolve_all(Choice::Ours),
                "top\nours\nm1\nm2\nm3\nm4\nours2\nend\n"
            );
            assert_eq!(
                f.resolve(&[Some(Choice::Theirs), Some(Choice::OursThenTheirs)]),
                "top\ntheirs\nm1\nm2\nm3\nm4\nours2\ntheirs2\nend\n"
            );
            assert_eq!(
                f.resolve(&[
                    Some(Choice::Custom("mine\n".into())),
                    Some(Choice::TheirsThenOurs)
                ]),
                "top\nmine\nm1\nm2\nm3\nm4\ntheirs2\nours2\nend\n"
            );
        }
    }

    #[test]
    fn reads_index_stages() {
        let (t, repo) = conflicted("merge");
        let s = stages(&repo, Path::new("f.txt")).unwrap();
        let read =
            |id: Option<Oid>| String::from_utf8(read_blob(&repo, id.unwrap()).unwrap()).unwrap();
        assert_eq!(read(s.base), "top\nbase\nm1\nm2\nm3\nm4\nbase2\nend\n");
        assert_eq!(read(s.ours), "top\nours\nm1\nm2\nm3\nm4\nours2\nend\n");
        assert_eq!(
            read(s.theirs),
            "top\ntheirs\nm1\nm2\nm3\nm4\ntheirs2\nend\n"
        );
        assert_eq!(
            stages(&repo, Path::new("missing.txt")).unwrap(),
            Stages::default()
        );
        drop(t);
    }

    #[test]
    fn custom_marker_size_crlf_and_errors() {
        let text = "a\r\n<<<<<<<<<< ours\r\nx\r\n<<<<<<< not a marker\r\n==========\r\ny\r\n>>>>>>>>>> theirs\r\nb\r\n";
        let f = parse(text).unwrap();
        assert_eq!(f.count(), 1);
        let c = f.conflicts().next().unwrap();
        assert_eq!(c.ours, "x\r\n<<<<<<< not a marker\r\n");
        assert_eq!(c.theirs, "y\r\n");
        assert_eq!(f.resolve_all(Choice::Theirs), "a\r\ny\r\nb\r\n");
        assert!(parse("<<<<<<< a\nx\n=======\ny\n").is_err());
        assert_eq!(parse("no conflicts\n").unwrap().count(), 0);
        // A line of eight '=' is content outside a conflict.
        assert_eq!(parse("========\n").unwrap().count(), 0);
    }

    #[test]
    fn parses_stage_records() {
        let id1 = "1".repeat(40);
        let id3 = "3".repeat(40);
        let out = format!("100644 {id1} 1\tf.txt\0100644 {id3} 3\tf.txt\0");
        let s = parse_stages(out.as_bytes()).unwrap();
        assert!(s.base.is_some() && s.ours.is_none() && s.theirs.is_some());
    }
}
