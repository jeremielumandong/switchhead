//! Commit search over the graph's scope with `git log`: full message, author, file path,
//! and content (pickaxe `-S` for a string's count changing, `-G` for a regex in the diff).
//! Matches stream back in log order so the graph can highlight them while git runs.

use crate::cli::Cancel;
use crate::log::Scope;
use crate::{Error, Oid, Repo, Result};

/// What a search looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SearchKind {
    /// The whole message, case-insensitive (`--grep`).
    #[default]
    Message,
    /// Author name or email, case-insensitive (`--author`).
    Author,
    /// Commit id prefix. Answered from the loaded graph, not by git.
    Id,
    /// Commits that touch a path (a file or folder, `-- <path>`).
    Path,
    /// Commits that add or remove this text (`-S`).
    Content,
    /// Commits whose diff has a line matching this regex (`-G`).
    ContentRegex,
}

impl SearchKind {
    /// Every kind, in menu order.
    pub const ALL: [SearchKind; 6] = [
        SearchKind::Message,
        SearchKind::Author,
        SearchKind::Id,
        SearchKind::Path,
        SearchKind::Content,
        SearchKind::ContentRegex,
    ];

    /// Its name for menus.
    pub fn label(self) -> &'static str {
        match self {
            SearchKind::Message => "Message",
            SearchKind::Author => "Author",
            SearchKind::Id => "Commit ID",
            SearchKind::Path => "File Path",
            SearchKind::Content => "Content",
            SearchKind::ContentRegex => "Content (regex)",
        }
    }
}

/// `git log` arguments for the commits a scope shows.
fn scope_args(scope: &Scope) -> Vec<String> {
    match scope {
        // Like the graph: every ref and HEAD, not the stash.
        Scope::All => vec!["--exclude=refs/stash".into(), "--all".into()],
        Scope::Head => vec!["HEAD".into()],
        Scope::Revs(revs) => revs.clone(),
        Scope::Compare(a, b) => vec![format!("{a}...{b}")],
    }
}

/// The full `git log` command line for a search.
fn args(scope: &Scope, kind: SearchKind, text: &str) -> Result<Vec<String>> {
    let mut a: Vec<String> = vec!["log".into(), "--format=%H".into()];
    match kind {
        SearchKind::Message => {
            a.extend(["--regexp-ignore-case".into(), "--fixed-strings".into()]);
            a.push(format!("--grep={text}"));
        }
        SearchKind::Author => {
            a.extend(["--regexp-ignore-case".into(), "--fixed-strings".into()]);
            a.push(format!("--author={text}"));
        }
        SearchKind::Content => a.push(format!("-S{text}")),
        SearchKind::ContentRegex => a.push(format!("-G{text}")),
        SearchKind::Path => {}
        SearchKind::Id => {
            return Err(Error::Unsupported(
                "commit ids are matched in the loaded graph".into(),
            ));
        }
    }
    a.extend(scope_args(scope));
    a.push("--".into());
    if kind == SearchKind::Path {
        a.push(text.into());
    }
    Ok(a)
}

/// Run a search, handing matches to `on_batch` in log order, a few hundred at a time.
/// `cancel` stops it (not an error).
pub fn search(
    repo: &Repo,
    scope: &Scope,
    kind: SearchKind,
    text: &str,
    cancel: &Cancel,
    mut on_batch: impl FnMut(Vec<Oid>),
) -> Result<()> {
    let args = args(scope, kind, text)?;
    let mut batch = Vec::new();
    repo.git()?.run_lines(&args, cancel, |line| {
        if let Ok(id) = Oid::from_hex(line) {
            batch.push(id);
            if batch.len() >= 200 {
                on_batch(std::mem::take(&mut batch));
            }
        }
        true
    })?;
    if !batch.is_empty() && !cancel.is_cancelled() {
        on_batch(batch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    fn found(repo: &Repo, scope: &Scope, kind: SearchKind, text: &str) -> Vec<String> {
        let mut ids = Vec::new();
        search(repo, scope, kind, text, &Cancel::default(), |b| {
            ids.extend(b)
        })
        .unwrap();
        ids.iter()
            .map(|id| {
                repo.git()
                    .unwrap()
                    .text(&["log", "-1", "--format=%s", &id.to_string()])
                    .unwrap()
                    .trim()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn finds_by_message_author_path_and_content() {
        let t = TestRepo::new();
        t.commit_file("src/a.rs", "fn alpha() {}\n", "Add alpha");
        t.commit_file(
            "docs/b.md",
            "notes\n",
            "Write notes\n\nMentions BETA in the body",
        );
        t.git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "by ada",
            "--author=Ada Lovelace <ada@example.com>",
        ]);
        t.commit_file("src/a.rs", "fn alpha() {}\nfn gamma() {}\n", "Add gamma");
        t.git(&["checkout", "-q", "-b", "side", "HEAD~3"]);
        t.commit_file("side.txt", "alpha here too\n", "Side work");
        t.git(&["checkout", "-q", "main"]);
        let repo = Repo::open(t.path()).unwrap();
        let all = Scope::All;

        assert_eq!(
            found(&repo, &all, SearchKind::Message, "beta"),
            ["Write notes"]
        );
        assert_eq!(found(&repo, &all, SearchKind::Author, "ADA@"), ["by ada"]);
        assert_eq!(
            found(&repo, &all, SearchKind::Path, "src"),
            ["Add gamma", "Add alpha"]
        );
        assert_eq!(
            found(&repo, &all, SearchKind::Content, "alpha"),
            ["Side work", "Add alpha"]
        );
        // Only on HEAD's history.
        assert_eq!(
            found(&repo, &Scope::Head, SearchKind::Content, "alpha"),
            ["Add alpha"]
        );
        assert_eq!(
            found(&repo, &all, SearchKind::ContentRegex, "fn g[a-z]+"),
            ["Add gamma"]
        );
        assert!(found(&repo, &all, SearchKind::Message, "nothing like it").is_empty());
        assert!(
            search(
                &repo,
                &all,
                SearchKind::Id,
                "abc",
                &Cancel::default(),
                |_| {}
            )
            .is_err()
        );
    }
}
