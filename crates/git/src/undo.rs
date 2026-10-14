//! Undo for the actions git cannot take back: a hard reset, discarding changes and
//! deleting untracked files.
//!
//! Before such an action, [`snapshot`] records the index and the work tree (untracked
//! files included, ignored ones not) as two commits, the way `git stash` does, and keeps
//! them under `refs/switchhead/undo/<time>` with what the action was, which paths it
//! touched, and where HEAD pointed. [`restore`] puts those paths (or everything, and HEAD,
//! for a reset) back; [`latest`] finds the newest snapshot. Snapshots expire after
//! [`KEEP_DAYS`] or beyond the newest [`KEEP`].

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Error, GixExt as _, Oid, Repo, Result};

/// Where snapshots are kept.
pub const PREFIX: &str = "refs/switchhead/undo/";
/// Snapshots kept at most.
pub const KEEP: usize = 20;
/// Snapshots older than this many days are dropped.
pub const KEEP_DAYS: i64 = 14;

const HEADER: &str = "switchhead undo: ";

/// A recorded state that an action is about to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The ref holding it.
    pub reference: String,
    /// What the action was ("Hard reset master to a1b2c3d").
    pub label: String,
    /// When it was taken (seconds since the epoch).
    pub time: i64,
    /// Where HEAD pointed (`None`: no commits yet).
    pub head: Option<Oid>,
    /// The branch checked out (`None`: detached).
    pub branch: Option<String>,
    /// The paths the action touched; empty means the whole tree and HEAD.
    pub paths: Vec<PathBuf>,
    /// The commit holding the work tree.
    pub work: Oid,
    /// The commit holding the index.
    pub index: Oid,
}

impl Snapshot {
    /// Whether restoring also moves HEAD back (the action was a reset).
    pub fn whole_tree(&self) -> bool {
        self.paths.is_empty()
    }
}

fn now() -> (i64, u128) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.as_millis())
}

/// A fixed identity, so snapshots work in a repository without `user.name`.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "switchhead"),
    ("GIT_AUTHOR_EMAIL", "undo@switchhead.invalid"),
    ("GIT_COMMITTER_NAME", "switchhead"),
    ("GIT_COMMITTER_EMAIL", "undo@switchhead.invalid"),
];

fn text_of(out: &crate::cli::Output) -> String {
    out.text().trim().to_owned()
}

fn parse_oid(s: &str) -> Result<Oid> {
    s.trim()
        .parse()
        .map_err(|_| Error::Parse(format!("not an object id: {s}")))
}

/// Record the index and work tree before `label`'s action. `paths` are the files it will
/// change; empty means the whole tree (a hard reset), which also records HEAD for restore.
pub fn snapshot(repo: &Repo, label: &str, paths: &[&Path]) -> Result<Snapshot> {
    let started = std::time::Instant::now();
    let git = repo.git()?;
    let head = git
        .run(&["rev-parse", "-q", "--verify", "HEAD"])
        .ok()
        .map(|o| text_of(&o))
        .filter(|s| !s.is_empty())
        .map(|s| parse_oid(&s))
        .transpose()?;
    let branch = repo.head().ok().and_then(|h| match h {
        crate::repo::Head::Branch { name, .. } if !name.is_empty() => Some(name),
        _ => None,
    });

    // The index as a tree (fails while a merge has conflicts: nothing to record safely).
    let index_tree = text_of(&git.run(&["write-tree"])?);

    // The work tree: the index plus every change under `paths`, staged into a copy.
    let real_index = repo.git_dir().join("index");
    let tmp = repo.git_dir().join("switchhead-undo-index");
    if real_index.exists() {
        std::fs::copy(&real_index, &tmp)?;
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
    let env: Vec<(&str, &OsStr)> = vec![("GIT_INDEX_FILE", tmp.as_os_str())];
    let mut add: Vec<OsString> = vec!["add".into(), "-A".into(), "--".into()];
    if paths.is_empty() {
        add.push(".".into());
    } else {
        add.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
    }
    let work_tree = git
        .run_with(&add, None, &env)
        .and_then(|_| git.run_with(&["write-tree"], None, &env));
    let _ = std::fs::remove_file(&tmp);
    let work_tree = text_of(&work_tree?);

    let ident: Vec<(&str, &OsStr)> = IDENTITY.iter().map(|(k, v)| (*k, OsStr::new(v))).collect();
    let parent: Vec<String> = head
        .map(|h| vec!["-p".into(), h.to_string()])
        .unwrap_or_default();
    let mut args: Vec<String> = vec![
        "commit-tree".into(),
        index_tree,
        "-m".into(),
        "index".into(),
    ];
    args.extend(parent.iter().cloned());
    let index = parse_oid(&text_of(&git.run_with(&args, None, &ident)?))?;

    let mut message = format!("{HEADER}{}\n\n", label.replace('\n', " "));
    if let Some(h) = head {
        message.push_str(&format!("head {h}\n"));
    }
    if let Some(b) = &branch {
        message.push_str(&format!("branch {b}\n"));
    }
    for p in paths {
        let s = p.to_string_lossy();
        if s.contains('\n') {
            return Err(Error::Unsupported(
                "a path with a line break cannot be recorded".into(),
            ));
        }
        message.push_str(&format!("path {s}\n"));
    }
    let mut args: Vec<String> = vec!["commit-tree".into(), work_tree];
    args.extend(parent);
    args.extend(["-p".into(), index.to_string(), "-F".into(), "-".into()]);
    let work = parse_oid(&text_of(&git.run_with(
        &args,
        Some(message.as_bytes()),
        &ident,
    )?))?;

    let (secs, millis) = now();
    let reference = format!("{PREFIX}{millis}");
    git.run(&["update-ref", &reference, &work.to_string()])?;
    if let Err(e) = expire(repo, secs) {
        tracing::warn!(error = %e, "could not expire old undo snapshots");
    }
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        label,
        "undo snapshot"
    );
    Ok(Snapshot {
        reference,
        label: label.to_owned(),
        time: secs,
        head,
        branch,
        paths: paths.iter().map(|p| p.to_path_buf()).collect(),
        work,
        index,
    })
}

/// [`snapshot`], logging rather than failing: an action the user confirmed still runs
/// when its state cannot be recorded (a conflicted index has no tree to save).
pub fn guard(repo: &Repo, label: &str, paths: &[&Path]) {
    if let Err(e) = snapshot(repo, label, paths) {
        tracing::warn!(error = %e, label, "no undo snapshot");
    }
}

/// Every snapshot, newest first.
pub fn list(repo: &Repo) -> Result<Vec<Snapshot>> {
    let r = repo.local();
    let refs = r.references().gix()?;
    let mut out = Vec::new();
    for reference in refs.prefixed(PREFIX).gix()? {
        let Ok(mut reference) = reference else {
            continue;
        };
        let name = reference.name().as_bstr().to_string();
        let Ok(id) = reference.peel_to_id() else {
            continue;
        };
        let Ok(commit) = r.find_commit(id.detach()) else {
            continue;
        };
        let Ok(decoded) = commit.decode() else {
            continue;
        };
        let time = decoded.committer().map(|c| c.seconds()).unwrap_or_default();
        let parents: Vec<Oid> = decoded.parents().collect();
        let message = decoded.message.to_string();
        if let Some(s) = parse(&name, id.detach(), time, &parents, &message) {
            out.push(s);
        }
    }
    // Names end in milliseconds since the epoch.
    out.sort_by_key(|s| std::cmp::Reverse(stamp(&s.reference)));
    Ok(out)
}

fn stamp(reference: &str) -> u128 {
    reference
        .strip_prefix(PREFIX)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Read a snapshot back from its commit.
fn parse(
    reference: &str,
    work: Oid,
    time: i64,
    parents: &[Oid],
    message: &str,
) -> Option<Snapshot> {
    let mut lines = message.lines();
    let label = lines.next()?.strip_prefix(HEADER)?.to_owned();
    let (mut head, mut branch, mut paths) = (None, None, Vec::new());
    for line in lines {
        if let Some(h) = line.strip_prefix("head ") {
            head = h.parse().ok();
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.to_owned());
        } else if let Some(p) = line.strip_prefix("path ") {
            paths.push(PathBuf::from(p));
        }
    }
    // Parents: HEAD (when there was one), then the index commit.
    let index = *parents.last()?;
    Some(Snapshot {
        reference: reference.to_owned(),
        label,
        time,
        head,
        branch,
        paths,
        work,
        index,
    })
}

/// The newest snapshot.
pub fn latest(repo: &Repo) -> Result<Option<Snapshot>> {
    Ok(list(repo)?.into_iter().next())
}

/// Drop snapshots beyond the newest [`KEEP`] and those older than [`KEEP_DAYS`].
fn expire(repo: &Repo, now: i64) -> Result<()> {
    let git = repo.git()?;
    for (i, s) in list(repo)?.into_iter().enumerate() {
        if i >= KEEP || now - s.time > KEEP_DAYS * 24 * 3600 {
            git.run(&["update-ref", "-d", &s.reference])?;
        }
    }
    Ok(())
}

/// Forget a snapshot.
pub fn drop(repo: &Repo, s: &Snapshot) -> Result<()> {
    repo.git()?.run(&["update-ref", "-d", &s.reference])?;
    Ok(())
}

/// Put back what `s` recorded: its paths in the index and work tree (changes made to
/// them since are replaced), and for a whole-tree snapshot also the branch's position.
/// Refuses when a reset snapshot's branch is no longer checked out. The snapshot is
/// dropped once restored.
pub fn restore(repo: &Repo, s: &Snapshot) -> Result<()> {
    let git = repo.git()?;
    if s.whole_tree() {
        let branch = repo.head().ok().and_then(|h| match h {
            crate::repo::Head::Branch { name, .. } if !name.is_empty() => Some(name),
            _ => None,
        });
        if branch != s.branch {
            return Err(Error::Unsupported(format!(
                "{} is no longer checked out; restore it from the reflog instead",
                s.branch.as_deref().unwrap_or("the detached HEAD")
            )));
        }
        if let Some(h) = s.head {
            git.run(&["reset", "-q", "--soft", &h.to_string()])?;
        }
    }
    let spec: Vec<OsString> = if s.paths.is_empty() {
        vec![".".into()]
    } else {
        s.paths.iter().map(|p| p.as_os_str().to_owned()).collect()
    };
    // `git restore` fails on a pathspec that matches nothing in the source or the index,
    // so each pass names the files that are in one of them.
    let known = |source: &Oid| -> Result<Vec<OsString>> {
        let mut tree: Vec<OsString> = vec![
            "ls-tree".into(),
            "-r".into(),
            "-z".into(),
            "--name-only".into(),
            source.to_string().into(),
            "--".into(),
        ];
        tree.extend(spec.iter().cloned());
        let mut index: Vec<OsString> = vec!["ls-files".into(), "-z".into(), "--".into()];
        index.extend(spec.iter().cloned());
        let mut names: Vec<Vec<u8>> = Vec::new();
        for out in [git.run(&tree)?, git.run(&index)?] {
            names.extend(
                out.stdout
                    .split(|b| *b == 0)
                    .filter(|n| !n.is_empty())
                    .map(<[u8]>::to_vec),
            );
        }
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|n| {
                gix::path::from_byte_slice(&n)
                    .map(|p| p.as_os_str().to_owned())
                    .gix()
            })
            .collect()
    };
    let restore = |source: &Oid, flag: &str| -> Result<()> {
        let files = known(source)?;
        if files.is_empty() {
            return Ok(());
        }
        let mut args: Vec<OsString> = vec![
            "restore".into(),
            format!("--source={source}").into(),
            flag.into(),
            "--".into(),
        ];
        args.extend(files);
        git.run(&args)?;
        Ok(())
    };
    // The index first: a path the work tree commit has but the index does not is then
    // untracked again, as it was.
    restore(&s.index, "--staged")?;
    restore(&s.work, "--worktree")?;
    drop(repo, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    fn read(t: &TestRepo, f: &str) -> Option<String> {
        std::fs::read_to_string(t.path().join(f)).ok()
    }

    #[test]
    fn undoes_a_hard_reset_and_a_clean() {
        let t = TestRepo::new();
        t.commit_file("a.txt", "1\n", "first");
        let second = t.commit_file("a.txt", "2\n", "second");
        t.write("a.txt", "staged\n");
        t.git(&["add", "a.txt"]);
        t.write("a.txt", "working\n");
        t.write("new.txt", "untracked\n");
        let repo = Repo::open(t.path()).unwrap();

        let s = snapshot(&repo, "Hard reset main to first", &[]).unwrap();
        assert!(s.whole_tree());
        assert_eq!(s.head.unwrap().to_string(), second);
        assert_eq!(s.branch.as_deref(), Some("main"));
        t.git(&["reset", "-q", "--hard", "HEAD~1"]);
        t.git(&["clean", "-fdq"]);
        assert_eq!(read(&t, "a.txt").as_deref(), Some("1\n"));
        assert!(read(&t, "new.txt").is_none());

        let found = latest(&repo).unwrap().unwrap();
        assert_eq!(found, s);
        restore(&repo, &found).unwrap();
        assert_eq!(t.git(&["rev-parse", "HEAD"]).trim(), second);
        assert_eq!(read(&t, "a.txt").as_deref(), Some("working\n"));
        assert_eq!(read(&t, "new.txt").as_deref(), Some("untracked\n"));
        assert_eq!(t.git(&["show", ":a.txt"]), "staged\n");
        let status = t.git(&["status", "--porcelain"]);
        assert!(status.contains("MM a.txt"), "{status}");
        assert!(status.contains("?? new.txt"), "{status}");
        assert!(
            latest(&repo).unwrap().is_none(),
            "a restored snapshot is dropped"
        );
    }

    #[test]
    fn undoes_a_discard_of_some_files_only() {
        let t = TestRepo::new();
        t.write("a.txt", "a\n");
        t.write("b.txt", "b\n");
        t.git(&["add", "."]);
        t.git(&["commit", "-qm", "first"]);
        t.write("a.txt", "a changed\n");
        t.write("b.txt", "b changed\n");
        let repo = Repo::open(t.path()).unwrap();

        let s = snapshot(&repo, "Discard a.txt", &[Path::new("a.txt")]).unwrap();
        assert_eq!(s.paths, [PathBuf::from("a.txt")]);
        repo.discard(&[Path::new("a.txt")]).unwrap();
        t.write("b.txt", "b changed again\n");

        restore(&repo, &s).unwrap();
        assert_eq!(read(&t, "a.txt").as_deref(), Some("a changed\n"));
        assert_eq!(
            read(&t, "b.txt").as_deref(),
            Some("b changed again\n"),
            "untouched"
        );
        assert_eq!(t.git(&["diff", "--cached", "--name-only"]), "");
    }

    #[test]
    fn keeps_the_newest_and_refuses_a_moved_branch() {
        let t = TestRepo::new();
        t.commit_file("a.txt", "1\n", "first");
        let repo = Repo::open(t.path()).unwrap();
        for i in 0..KEEP + 2 {
            snapshot(&repo, &format!("action {i}"), &[Path::new("a.txt")]).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let all = list(&repo).unwrap();
        assert_eq!(all.len(), KEEP);
        assert_eq!(all[0].label, format!("action {}", KEEP + 1));

        let reset = snapshot(&repo, "Hard reset", &[]).unwrap();
        t.git(&["checkout", "-q", "-b", "other"]);
        assert!(restore(&repo, &reset).is_err());
    }
}
