//! Interactive rebase without an editor.
//!
//! [`plan`] lists the commits a `git rebase -i` would offer, oldest first. The UI edits
//! that list (reorder, change actions, new messages) and [`run`] writes it as git's todo
//! file through `GIT_SEQUENCE_EDITOR`, which copies the prepared file over the one git
//! opens. A new message for a reworded or squashed commit is applied with an `exec git
//! commit --amend -F <file>` line right after it, so no message editor is ever needed and
//! hooks still run (see `docs/DECISIONS.md`). The run stops like `git rebase -i` does: on
//! a conflict, at an `edit`, or when an `exec` fails; [`RebaseOutcome`] says which.

use std::io::Write as _;
use std::path::Path;

use crate::{Error, Oid, Repo, RepoState, Result};

/// What to do with a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Keep it.
    Pick,
    /// Keep it with a new message.
    Reword,
    /// Stop after applying it, to amend it.
    Edit,
    /// Meld into the previous commit, combining messages.
    Squash,
    /// Meld into the previous commit, keeping the previous message.
    Fixup,
    /// Leave it out.
    Drop,
}

impl Action {
    /// The todo-file keyword.
    pub fn keyword(self) -> &'static str {
        match self {
            Action::Pick | Action::Reword => "pick",
            Action::Edit => "edit",
            Action::Squash => "squash",
            Action::Fixup => "fixup",
            Action::Drop => "drop",
        }
    }
}

/// One line of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoItem {
    /// What to do.
    pub action: Action,
    /// The commit.
    pub id: Oid,
    /// Its subject, for display.
    pub summary: String,
    /// New full message: required for [`Action::Reword`]; for [`Action::Squash`] it
    /// replaces the combined message (otherwise git's combination is kept).
    pub message: Option<String>,
}

/// The commits after `base` up to HEAD (`None`: from the root), oldest first, all `pick`.
/// Merge commits are left out, as `git rebase -i` does. `base` must be in HEAD's history.
pub fn plan(repo: &Repo, base: Option<Oid>) -> Result<Vec<TodoItem>> {
    if let Some(b) = base
        && !is_ancestor(repo, b)?
    {
        return Err(Error::Unsupported(format!(
            "{} is not in the current branch's history",
            crate::short(&b)
        )));
    }
    let range = range(base);
    let out = repo.git()?.text(&[
        "log",
        "--reverse",
        "--no-merges",
        "--topo-order",
        "--format=%H%x00%s",
        &range,
    ])?;
    out.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (id, summary) = l.split_once('\0').unwrap_or((l, ""));
            Ok(TodoItem {
                action: Action::Pick,
                id: Oid::from_hex(id.as_bytes())
                    .map_err(|_| Error::Parse(format!("rebase plan: {l}")))?,
                summary: summary.to_owned(),
                message: None,
            })
        })
        .collect()
}

fn range(base: Option<Oid>) -> String {
    match base {
        Some(b) => format!("{b}..HEAD"),
        None => "HEAD".to_owned(),
    }
}

/// `base` is HEAD or one of its ancestors.
fn is_ancestor(repo: &Repo, base: Oid) -> Result<bool> {
    let out = repo
        .git()?
        .text(&["rev-list", "--count", &format!("HEAD..{base}")])?;
    Ok(out.trim() == "0")
}

/// How many merge commits sit after `base` (`None`: in all of HEAD's history). A rebase
/// without `--rebase-merges` flattens them away, which the editor warns about.
pub fn merges_after(repo: &Repo, base: Option<Oid>) -> Result<usize> {
    let out = repo
        .git()?
        .text(&["rev-list", "--merges", "--count", &range(base)])?;
    out.trim()
        .parse()
        .map_err(|_| Error::Parse(format!("rev-list --count: {out}")))
}

/// How a rebase run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RebaseOutcome {
    /// Every step applied.
    Done,
    /// Stopped for the user: conflicts, an `edit`, or a failed step. The repository is
    /// in [`RepoState::RebaseInteractive`]; continue, skip or abort through
    /// [`Repo::rebase_continue`] and friends.
    Stopped {
        /// What git said.
        message: String,
    },
}

/// Single-quote for the POSIX shell git runs editors and `exec` lines with (Git for
/// Windows ships one too). Backslashes become slashes, which Windows accepts.
fn sh_quote(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Check the plan before running it.
fn validate(items: &[TodoItem]) -> Result<()> {
    let first_kept = items.iter().find(|i| i.action != Action::Drop);
    if first_kept.is_some_and(|i| matches!(i.action, Action::Squash | Action::Fixup)) {
        return Err(Error::Unsupported(
            "the first commit cannot be squashed or fixed up: there is nothing before it".into(),
        ));
    }
    if let Some(i) = items
        .iter()
        .find(|i| i.action == Action::Reword && i.message.as_deref().is_none_or(str::is_empty))
    {
        return Err(Error::Unsupported(format!(
            "reword of {} needs a message",
            crate::short(&i.id)
        )));
    }
    Ok(())
}

/// The todo file text; message files are written into `dir`.
fn todo_text(items: &[TodoItem], dir: &Path) -> Result<String> {
    let mut out = String::new();
    let amend = |out: &mut String, n: usize, msg: &str| -> Result<()> {
        let file = dir.join(format!("msg-{n}.txt"));
        std::fs::write(&file, msg)?;
        out.push_str(&format!(
            "exec git commit --amend --allow-empty --cleanup=strip -q -F {}\n",
            sh_quote(&file)
        ));
        Ok(())
    };
    // A squash message is the combined commit's: it waits for the group's last
    // squash/fixup (the newest squash's message wins).
    let mut group_msg: Option<&str> = None;
    for (n, item) in items.iter().enumerate() {
        out.push_str(&format!(
            "{} {} {}\n",
            item.action.keyword(),
            item.id,
            item.summary
        ));
        match (item.action, item.message.as_deref()) {
            (Action::Drop, _) => continue,
            (Action::Reword, Some(msg)) => amend(&mut out, n, msg)?,
            (Action::Squash, Some(msg)) => group_msg = Some(msg),
            _ => {}
        }
        let group_continues = items[n + 1..]
            .iter()
            .find(|i| i.action != Action::Drop)
            .is_some_and(|i| matches!(i.action, Action::Squash | Action::Fixup));
        if !group_continues && let Some(msg) = group_msg.take() {
            amend(&mut out, n, msg)?;
        }
    }
    Ok(out)
}

/// Run `git rebase -i` onto `base` (`None`: `--root`) with this plan. With `autostash`,
/// local changes are stashed first and reapplied at the end (git refuses to start with
/// them otherwise).
pub fn run(
    repo: &Repo,
    base: Option<Oid>,
    items: &[TodoItem],
    autostash: bool,
) -> Result<RebaseOutcome> {
    validate(items)?;
    let dir = tempfile_dir(repo)?;
    let todo = dir.join("todo");
    let text = todo_text(items, &dir)?;
    std::fs::File::create(&todo)?.write_all(text.as_bytes())?;
    let editor = format!("cp {}", sh_quote(&todo));
    let base_arg = match base {
        Some(b) => b.to_string(),
        None => "--root".to_owned(),
    };
    let res = repo.git()?.run_with(
        &[
            "rebase",
            "-i",
            "--no-autosquash",
            if autostash {
                "--autostash"
            } else {
                "--no-autostash"
            },
            &base_arg,
        ],
        None,
        &[("GIT_SEQUENCE_EDITOR", std::ffi::OsStr::new(&editor))],
    );
    let state = repo.state();
    let outcome = match res {
        Ok(_) if state == RepoState::Clean => Ok(RebaseOutcome::Done),
        Ok(out) => Ok(RebaseOutcome::Stopped {
            message: out.err_text().trim().to_owned(),
        }),
        Err(Error::Command { message, .. })
            if matches!(state, RepoState::Rebase | RepoState::RebaseInteractive) =>
        {
            Ok(RebaseOutcome::Stopped { message })
        }
        Err(e) => Err(e),
    };
    // Message files are read by `exec` lines that may run after a stop, so they stay
    // until the rebase is over.
    if !matches!(outcome, Ok(RebaseOutcome::Stopped { .. })) {
        let _ = std::fs::remove_dir_all(&dir);
    }
    outcome
}

/// A scratch directory inside the git dir for todo and message files.
fn tempfile_dir(repo: &Repo) -> Result<std::path::PathBuf> {
    let base = repo.git_dir().join("switchhead");
    // Files of a rebase that stopped and was then aborted or finished elsewhere.
    if repo.state() == RepoState::Clean
        && let Ok(old) = std::fs::read_dir(&base)
    {
        for e in old.flatten() {
            if e.file_name().to_string_lossy().starts_with("rebase-") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    std::fs::create_dir_all(&base)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = base.join(format!("rebase-{stamp}"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    fn setup() -> (TestRepo, Repo, Oid) {
        let t = TestRepo::new();
        t.commit_file("base.txt", "base\n", "base");
        let base = t.git(&["rev-parse", "HEAD"]).trim().to_owned();
        t.commit_file("a.txt", "a\n", "add a");
        t.commit_file("b.txt", "b\n", "add b");
        t.commit_file("c.txt", "c\n", "add c");
        let repo = Repo::open(t.path()).unwrap();
        (t, repo, Oid::from_hex(base.as_bytes()).unwrap())
    }

    fn subjects(t: &TestRepo) -> Vec<String> {
        t.git(&["log", "--format=%s"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn plans_oldest_first() {
        let (_t, repo, base) = setup();
        let p = plan(&repo, Some(base)).unwrap();
        let s: Vec<&str> = p.iter().map(|i| i.summary.as_str()).collect();
        assert_eq!(s, ["add a", "add b", "add c"]);
        assert_eq!(plan(&repo, None).unwrap().len(), 4);
        assert_eq!(merges_after(&repo, Some(base)).unwrap(), 0);
    }

    #[test]
    fn refuses_a_base_off_the_branch() {
        let (t, repo, base) = setup();
        t.git(&["checkout", "-q", "-b", "side", &base.to_string()]);
        t.commit_file("s.txt", "s\n", "side");
        let side = Oid::from_hex(t.git(&["rev-parse", "HEAD"]).trim().as_bytes()).unwrap();
        t.git(&["checkout", "-q", "-"]);
        assert!(matches!(
            plan(&repo, Some(side)),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn autostashes_local_changes() {
        let (t, repo, base) = setup();
        std::fs::write(t.join("a.txt"), "local\n").unwrap();
        let mut p = plan(&repo, Some(base)).unwrap();
        p[2].action = Action::Drop;
        assert_eq!(
            run(&repo, Some(base), &p, true).unwrap(),
            RebaseOutcome::Done
        );
        assert_eq!(subjects(&t), ["add b", "add a", "base"]);
        assert_eq!(std::fs::read_to_string(t.join("a.txt")).unwrap(), "local\n");
    }

    #[test]
    fn reorders_drops_rewords_and_squashes() {
        let (t, repo, base) = setup();
        let mut p = plan(&repo, Some(base)).unwrap();
        // c first (reworded), then a with b squashed into it under a new message.
        let (a, b, c) = (p[0].clone(), p[1].clone(), p[2].clone());
        p = vec![
            TodoItem {
                action: Action::Reword,
                message: Some("C, reworded\n\nwith a body".into()),
                ..c
            },
            a,
            TodoItem {
                action: Action::Squash,
                message: Some("a and b".into()),
                ..b
            },
        ];
        assert_eq!(
            run(&repo, Some(base), &p, false).unwrap(),
            RebaseOutcome::Done
        );
        assert_eq!(subjects(&t), ["a and b", "C, reworded", "base"]);
        assert_eq!(
            t.git(&["log", "-1", "--skip=1", "--format=%B"]).trim(),
            "C, reworded\n\nwith a body"
        );
        assert_eq!(
            t.git(&["show", "--format=", "--name-only", "HEAD"]).trim(),
            "a.txt\nb.txt"
        );

        // Drop and fixup.
        let mut p = plan(&repo, Some(base)).unwrap();
        p[0].action = Action::Drop;
        let mut p2 = p.clone();
        p2[1].action = Action::Pick;
        assert_eq!(
            run(&repo, Some(base), &p2, false).unwrap(),
            RebaseOutcome::Done
        );
        assert_eq!(subjects(&t), ["a and b", "base"]);
        assert!(!t.join("c.txt").exists());
    }

    #[test]
    fn keeps_a_squash_message_through_a_later_fixup() {
        let (t, repo, base) = setup();
        let mut p = plan(&repo, Some(base)).unwrap();
        p[1].action = Action::Squash;
        p[1].message = Some("a, b and c".into());
        p[2].action = Action::Fixup;
        assert_eq!(
            run(&repo, Some(base), &p, false).unwrap(),
            RebaseOutcome::Done
        );
        assert_eq!(subjects(&t), ["a, b and c", "base"]);
    }

    #[test]
    fn stops_at_edit_and_on_conflict() {
        let (t, repo, base) = setup();
        let mut p = plan(&repo, Some(base)).unwrap();
        p[1].action = Action::Edit;
        assert!(matches!(
            run(&repo, Some(base), &p, false).unwrap(),
            RebaseOutcome::Stopped { .. }
        ));
        assert_eq!(repo.state(), RepoState::RebaseInteractive);
        t.git(&["rebase", "--abort"]);

        // Two commits editing the same line, swapped: the second pick conflicts.
        t.commit_file("a.txt", "a2\n", "edit a");
        t.commit_file("a.txt", "a3\n", "edit a again");
        let mut p = plan(&repo, Some(base)).unwrap();
        let n = p.len();
        p.swap(n - 1, n - 2);
        assert!(matches!(
            run(&repo, Some(base), &p, false).unwrap(),
            RebaseOutcome::Stopped { .. }
        ));
        t.git(&["rebase", "--abort"]);
        assert_eq!(repo.state(), RepoState::Clean);
        // The next run clears what the stopped ones left.
        let p = plan(&repo, Some(base)).unwrap();
        let mut p2 = p.clone();
        p2[p.len() - 1].action = Action::Drop;
        assert_eq!(
            run(&repo, Some(base), &p2, false).unwrap(),
            RebaseOutcome::Done
        );
        let left = std::fs::read_dir(repo.git_dir().join("switchhead"))
            .unwrap()
            .count();
        assert_eq!(left, 0);
    }

    #[test]
    fn rejects_bad_plans() {
        let (_t, repo, base) = setup();
        let mut p = plan(&repo, Some(base)).unwrap();
        p[0].action = Action::Fixup;
        assert!(matches!(
            run(&repo, Some(base), &p, false),
            Err(Error::Unsupported(_))
        ));
        let mut p = plan(&repo, Some(base)).unwrap();
        p[1].action = Action::Reword;
        assert!(matches!(
            run(&repo, Some(base), &p, false),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(repo.state(), RepoState::Clean);
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(sh_quote(Path::new("/a b/it's")), r"'/a b/it'\''s'");
    }
}
