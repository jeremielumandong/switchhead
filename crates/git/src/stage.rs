//! Partial staging: stage, unstage or discard chosen lines of a diff.
//!
//! The diff the user looks at ([`FilePatch`], from `git diff` or `git diff --cached`) is
//! cut down to the chosen lines and applied with `git apply`. Lines left out must still
//! describe the file the patch applies to:
//!
//! - Staging applies the work-tree diff forward to the index: an unchosen `+` line is
//!   dropped, an unchosen `-` line stays as context.
//! - Unstaging applies the index diff in reverse to the index, and discarding applies the
//!   work-tree diff in reverse to the work tree: an unchosen `-` line is dropped, an
//!   unchosen `+` line stays as context.
//!
//! `--recount` lets git recompute hunk sizes; the numbers written are still exact.

use std::path::Path;

use crate::diff::{FilePatch, LineKind, range_spec};
use crate::{Error, Repo, Result};

/// What to do with the chosen lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Add them to the index (from a work-tree diff).
    Stage,
    /// Take them out of the index (from an index diff).
    Unstage,
    /// Revert them in the work tree (from a work-tree diff). Cannot be undone.
    Discard,
}

impl Action {
    fn reverse(self) -> bool {
        matches!(self, Action::Unstage | Action::Discard)
    }

    fn cached(self) -> bool {
        matches!(self, Action::Stage | Action::Unstage)
    }
}

/// Quote a path for a patch header the way git does when it must.
fn header_path(prefix: &str, p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if !s
        .chars()
        .any(|c| matches!(c, '"' | '\\' | '\t' | '\n') || c.is_control())
    {
        return format!("{prefix}{s}");
    }
    let mut out = String::from("\"");
    out.push_str(prefix);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => out.push_str(&format!("\\{:03o}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Build a patch of the lines `chosen` selects (indices into `patch.diff`), for `action`.
/// `None` when no changed line is chosen.
pub fn build_patch(
    patch: &FilePatch,
    chosen: impl Fn(usize) -> bool,
    action: Action,
) -> Result<Option<String>> {
    let d = &patch.diff;
    let changed: Vec<usize> = (0..d.len())
        .filter(|i| d.line(*i).kind != LineKind::Context)
        .collect();
    let picked = changed.iter().filter(|i| chosen(**i)).count();
    if picked == 0 {
        return Ok(None);
    }
    if patch.binary {
        return Err(Error::Unsupported(
            "lines of a binary file cannot be staged".into(),
        ));
    }
    let created_or_deleted = patch.old_path.is_none() || patch.new_path.is_none();
    if created_or_deleted && picked < changed.len() {
        return Err(Error::Unsupported(
            "only the whole of a new or deleted file can be staged".into(),
        ));
    }
    let reverse = action.reverse();
    let old = patch.old_path.as_deref();
    let new = patch.new_path.as_deref();
    let a = old.or(new).unwrap_or(Path::new(""));
    let b = new.or(old).unwrap_or(Path::new(""));
    let mut out = format!(
        "diff --git {} {}\n",
        header_path("a/", a),
        header_path("b/", b)
    );
    for m in &patch.mode_lines {
        out.push_str(m);
        out.push('\n');
    }
    out.push_str(&match old {
        Some(p) => format!("--- {}\n", header_path("a/", p)),
        None => "--- /dev/null\n".into(),
    });
    out.push_str(&match new {
        Some(p) => format!("+++ {}\n", header_path("b/", p)),
        None => "+++ /dev/null\n".into(),
    });

    // Offset of the side the patch does not apply to, from earlier hunks.
    let mut delta: i64 = 0;
    for h in d.hunks() {
        let mut body = String::new();
        let (mut old_n, mut new_n, mut any) = (0u32, 0u32, false);
        for i in h.lines.clone() {
            let line = d.line(i);
            let pick = line.kind != LineKind::Context && chosen(i);
            let kind = match (line.kind, pick, reverse) {
                (LineKind::Context, ..) => Some(' '),
                (k, true, _) => Some(k.prefix()),
                (LineKind::Added, false, false) | (LineKind::Removed, false, true) => None,
                (LineKind::Added, false, true) | (LineKind::Removed, false, false) => Some(' '),
            };
            let Some(prefix) = kind else { continue };
            any |= pick;
            match prefix {
                ' ' => {
                    old_n += 1;
                    new_n += 1;
                }
                '-' => old_n += 1,
                _ => new_n += 1,
            }
            body.push(prefix);
            body.push_str(d.text(i));
            body.push('\n');
            if line.no_eol {
                body.push_str("\\ No newline at end of file\n");
            }
        }
        if !any {
            continue;
        }
        // Forward patches apply to the old side, reverse ones to the new side. That side
        // keeps git's start; the other follows the lines changed by earlier hunks. An
        // empty side's start names the line before it, as git writes it.
        let (ref_start, ref_n, other_n) = if reverse {
            (h.new_start, new_n, old_n)
        } else {
            (h.old_start, old_n, new_n)
        };
        let first = i64::from(if ref_n == 0 { ref_start + 1 } else { ref_start });
        let other_first = first + delta;
        let other_start = if other_n == 0 {
            other_first - 1
        } else {
            other_first
        }
        .max(0) as u32;
        delta += i64::from(other_n) - i64::from(ref_n);
        let (old_start, new_start) = if reverse {
            (other_start, ref_start)
        } else {
            (ref_start, other_start)
        };
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            range_spec(old_start, old_n),
            range_spec(new_start, new_n)
        ));
        out.push_str(&body);
    }
    Ok(Some(out))
}

/// Apply a patch built by [`build_patch`] for `action`.
pub fn apply(repo: &Repo, patch: &str, action: Action) -> Result<()> {
    let mut args = vec!["apply", "--recount", "--whitespace=nowarn"];
    if action.cached() {
        args.push("--cached");
    }
    if action.reverse() {
        args.push("--reverse");
    }
    // Hunks without context (a diff made with -U0) need this to apply.
    if patch.lines().any(|l| l.starts_with("@@")) && !patch.lines().any(|l| l.starts_with(' ')) {
        args.push("--unidiff-zero");
    }
    args.push("-");
    repo.git()?.run_with(&args, Some(patch.as_bytes()), &[])?;
    Ok(())
}

/// Build and apply in one step. Returns `false` when nothing was chosen.
pub fn apply_lines(
    repo: &Repo,
    patch: &FilePatch,
    chosen: impl Fn(usize) -> bool,
    action: Action,
) -> Result<bool> {
    match build_patch(patch, chosen, action)? {
        Some(p) => {
            apply(repo, &p, action)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Apply whole hunks: the lines of hunks `hunks` (indices into `patch.diff.hunks()`).
pub fn apply_hunks(
    repo: &Repo,
    patch: &FilePatch,
    hunks: &[usize],
    action: Action,
) -> Result<bool> {
    let ranges: Vec<_> = hunks
        .iter()
        .filter_map(|h| patch.diff.hunks().get(*h).map(|h| h.lines.clone()))
        .collect();
    apply_lines(
        repo,
        patch,
        |i| ranges.iter().any(|r| r.contains(&i)),
        action,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{index_diff, worktree_diff};
    use crate::testutil::TestRepo;

    const BASE: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n";

    fn setup() -> (TestRepo, Repo) {
        let t = TestRepo::new();
        t.commit_file("f.txt", BASE, "base");
        // Two hunks: line 2 changed, line 11 removed and two lines added at the end.
        t.write("f.txt", "1\nTWO\n3\n4\n5\n6\n7\n8\n9\n10\n12\n13\n14\n");
        let repo = Repo::open(t.path()).unwrap();
        (t, repo)
    }

    fn lines_with(p: &FilePatch, kind: LineKind, text: &str) -> usize {
        (0..p.diff.len())
            .find(|i| p.diff.line(*i).kind == kind && p.diff.text(*i) == text)
            .unwrap()
    }

    fn show(t: &TestRepo, spec: &str) -> String {
        t.git(&["show", spec])
    }

    #[test]
    fn stages_single_lines() {
        let (t, repo) = setup();
        let p = &worktree_diff(&repo, &[Path::new("f.txt")], 3).unwrap()[0];
        // Stage only "+13" (second hunk), not the removal of 11 or "+14".
        let pick = lines_with(p, LineKind::Added, "13");
        assert!(apply_lines(&repo, p, |i| i == pick, Action::Stage).unwrap());
        assert_eq!(
            show(&t, ":f.txt"),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n"
        );
        // The work tree is untouched.
        assert!(
            std::fs::read_to_string(t.join("f.txt"))
                .unwrap()
                .contains("TWO")
        );

        // Now stage the removal of "11" only.
        let p = &worktree_diff(&repo, &[Path::new("f.txt")], 3).unwrap()[0];
        let pick = lines_with(p, LineKind::Removed, "11");
        apply_lines(&repo, p, |i| i == pick, Action::Stage).unwrap();
        assert_eq!(
            show(&t, ":f.txt"),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n12\n13\n"
        );

        // Nothing chosen: nothing happens.
        assert!(!apply_lines(&repo, p, |_| false, Action::Stage).unwrap());
    }

    #[test]
    fn stages_a_whole_hunk_and_unstages_a_line() {
        let (t, repo) = setup();
        let p = &worktree_diff(&repo, &[Path::new("f.txt")], 3).unwrap()[0];
        assert_eq!(p.diff.hunks().len(), 2);
        apply_hunks(&repo, p, &[1], Action::Stage).unwrap();
        assert_eq!(
            show(&t, ":f.txt"),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n12\n13\n14\n"
        );
        // Unstage just "+14".
        let staged = &index_diff(&repo, &[Path::new("f.txt")], 3).unwrap()[0];
        let pick = lines_with(staged, LineKind::Added, "14");
        apply_lines(&repo, staged, |i| i == pick, Action::Unstage).unwrap();
        assert_eq!(
            show(&t, ":f.txt"),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n12\n13\n"
        );
        // Unstage the removal of "11" (keeps "+13" staged).
        let staged = &index_diff(&repo, &[Path::new("f.txt")], 3).unwrap()[0];
        let pick = lines_with(staged, LineKind::Removed, "11");
        apply_lines(&repo, staged, |i| i == pick, Action::Unstage).unwrap();
        assert_eq!(
            show(&t, ":f.txt"),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n"
        );
    }

    #[test]
    fn discards_lines_in_the_work_tree() {
        let (t, repo) = setup();
        let p = &worktree_diff(&repo, &[Path::new("f.txt")], 3).unwrap()[0];
        // Discard the first hunk's change (2 -> TWO) only.
        let removed = lines_with(p, LineKind::Removed, "2");
        let added = lines_with(p, LineKind::Added, "TWO");
        apply_lines(&repo, p, |i| i == removed || i == added, Action::Discard).unwrap();
        assert_eq!(
            std::fs::read_to_string(t.join("f.txt")).unwrap(),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n12\n13\n14\n"
        );
        // Discard "+14" only.
        let p = &worktree_diff(&repo, &[Path::new("f.txt")], 3).unwrap()[0];
        let pick = lines_with(p, LineKind::Added, "14");
        apply_lines(&repo, p, |i| i == pick, Action::Discard).unwrap();
        assert_eq!(
            std::fs::read_to_string(t.join("f.txt")).unwrap(),
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n12\n13\n"
        );
    }

    #[test]
    fn works_without_context_and_at_end_of_file() {
        let t = TestRepo::new();
        t.commit_file("e.txt", "a\nb", "no newline");
        t.write("e.txt", "a\nb\nc");
        let repo = Repo::open(t.path()).unwrap();
        let p = &worktree_diff(&repo, &[Path::new("e.txt")], 0).unwrap()[0];
        apply_hunks(&repo, p, &[0], Action::Stage).unwrap();
        assert_eq!(show(&t, ":e.txt"), "a\nb\nc");
    }

    #[test]
    fn new_files_stage_only_whole() {
        let t = TestRepo::new();
        t.commit_file("a.txt", "a\n", "one");
        t.write("n.txt", "x\ny\n");
        t.git(&["add", "-N", "n.txt"]);
        let repo = Repo::open(t.path()).unwrap();
        let p = &worktree_diff(&repo, &[Path::new("n.txt")], 3).unwrap()[0];
        assert!(matches!(
            build_patch(p, |i| i == 0, Action::Stage),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn quotes_header_paths() {
        assert_eq!(header_path("a/", Path::new("sp ace.txt")), "a/sp ace.txt");
        assert_eq!(header_path("b/", Path::new("t\tab")), "\"b/t\\tab\"");
    }
}
