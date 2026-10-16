//! An open repository: a thread-safe gitoxide handle plus the `git` runner, and the
//! summary the title bar and status bar show (HEAD, operation in progress).

use std::path::{Path, PathBuf};

use crate::cli::Git;
use crate::{Error, GixExt as _, Oid, Result};

/// A repository, cheap to clone and safe to share across threads.
#[derive(Clone)]
pub struct Repo {
    sync: gix::ThreadSafeRepository,
    root: PathBuf,
    git_dir: PathBuf,
    name: String,
}

impl std::fmt::Debug for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repo").field("root", &self.root).finish()
    }
}

/// An operation git is in the middle of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoState {
    /// Nothing in progress.
    Clean,
    /// A merge stopped (conflicts, or `--no-commit`).
    Merge,
    /// A rebase stopped.
    Rebase,
    /// An interactive rebase stopped.
    RebaseInteractive,
    /// A cherry-pick stopped.
    CherryPick,
    /// A revert stopped.
    Revert,
    /// `git am` is applying patches.
    ApplyMailbox,
    /// A bisect session is running.
    Bisect,
}

impl RepoState {
    /// Short label for banners ("Merging").
    pub fn label(self) -> &'static str {
        match self {
            RepoState::Clean => "",
            RepoState::Merge => "Merging",
            RepoState::Rebase | RepoState::RebaseInteractive => "Rebasing",
            RepoState::CherryPick => "Cherry-picking",
            RepoState::Revert => "Reverting",
            RepoState::ApplyMailbox => "Applying patches",
            RepoState::Bisect => "Bisecting",
        }
    }

    /// The `git` subcommand whose `--continue` / `--abort` ends this state.
    pub fn command(self) -> Option<&'static str> {
        match self {
            RepoState::Merge => Some("merge"),
            RepoState::Rebase | RepoState::RebaseInteractive => Some("rebase"),
            RepoState::CherryPick => Some("cherry-pick"),
            RepoState::Revert => Some("revert"),
            RepoState::ApplyMailbox => Some("am"),
            RepoState::Bisect | RepoState::Clean => None,
        }
    }
}

/// Where HEAD points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    /// On a branch (short name) at a commit.
    Branch {
        /// Short branch name (`main`).
        name: String,
        /// The commit, `None` on an unborn branch.
        id: Option<Oid>,
    },
    /// Detached at a commit.
    Detached(Oid),
}

impl Head {
    /// The commit HEAD points at.
    pub fn id(&self) -> Option<Oid> {
        match self {
            Head::Branch { id, .. } => *id,
            Head::Detached(id) => Some(*id),
        }
    }

    /// The branch name, when on one.
    pub fn branch(&self) -> Option<&str> {
        match self {
            Head::Branch { name, .. } => Some(name),
            Head::Detached(_) => None,
        }
    }
}

impl Repo {
    /// Open the repository at `path` (its work tree root, a subdirectory, or a git dir).
    pub fn open(path: &Path) -> Result<Self> {
        let repo = gix::discover(path).map_err(|_| Error::NotARepo(path.to_path_buf()))?;
        let git_dir = repo.git_dir().to_path_buf();
        let root = repo
            .workdir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| git_dir.clone());
        let root = crate::paths::canonicalize(&root).unwrap_or(root);
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        Ok(Self {
            sync: repo.into_sync(),
            root,
            git_dir,
            name,
        })
    }

    /// A gitoxide handle for this thread.
    pub fn local(&self) -> gix::Repository {
        let mut r = self.sync.to_thread_local();
        // A larger object cache pays off when walking and diffing many commits.
        r.object_cache_size_if_unset(32 * 1024 * 1024);
        r
    }

    /// The work tree root (or the git dir of a bare repository).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `.git` directory.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Display name (the root folder's name).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether there is a work tree.
    pub fn has_worktree(&self) -> bool {
        self.root != self.git_dir
    }

    /// A `git` runner for this repository.
    pub fn git(&self) -> Result<Git> {
        Git::new(&self.root)
    }

    /// Where HEAD points.
    pub fn head(&self) -> Result<Head> {
        let r = self.local();
        let head = r.head().gix()?;
        let id = head.id().map(|i| i.detach());
        Ok(match head.referent_name() {
            Some(name) => Head::Branch {
                name: name.shorten().to_string(),
                id,
            },
            None => match id {
                Some(id) => Head::Detached(id),
                None => Head::Branch {
                    name: String::new(),
                    id: None,
                },
            },
        })
    }

    /// The operation in progress, if any.
    pub fn state(&self) -> RepoState {
        use gix::state::InProgress as P;
        match self.local().state() {
            None => RepoState::Clean,
            Some(P::Merge) => RepoState::Merge,
            Some(P::Rebase) => RepoState::Rebase,
            Some(P::RebaseInteractive) => RepoState::RebaseInteractive,
            Some(P::CherryPick | P::CherryPickSequence) => RepoState::CherryPick,
            Some(P::Revert | P::RevertSequence) => RepoState::Revert,
            Some(P::ApplyMailbox | P::ApplyMailboxRebase) => RepoState::ApplyMailbox,
            Some(P::Bisect) => RepoState::Bisect,
        }
    }

    /// The configured `user.name` and `user.email`.
    pub fn identity(&self) -> (Option<String>, Option<String>) {
        let r = self.local();
        let cfg = r.config_snapshot();
        (
            cfg.string("user.name").map(|s| s.to_string()),
            cfg.string("user.email").map(|s| s.to_string()),
        )
    }

    /// A text file inside the git dir (`MERGE_MSG`, `SQUASH_MSG`), if present.
    pub fn read_git_file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.git_dir.join(name)).ok()
    }

    /// Resolve a revision (`HEAD~2`, a branch, an abbreviated id) to a commit id.
    pub fn resolve(&self, rev: &str) -> Result<Oid> {
        let r = self.local();
        let id = r.rev_parse_single(rev).gix()?;
        let commit = id.object().gix()?.peel_to_commit().gix()?;
        Ok(commit.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn opens_and_reads_head() {
        let t = TestRepo::new();
        let id = t.commit_file("a.txt", "a\n", "first");
        std::fs::create_dir_all(t.join("sub/dir")).unwrap();
        let r = Repo::open(&t.join("sub/dir")).unwrap();
        assert_eq!(r.root(), crate::paths::canonicalize(t.path()).unwrap());
        assert!(r.has_worktree());
        assert_eq!(
            r.head().unwrap(),
            Head::Branch {
                name: "main".into(),
                id: Some(Oid::from_hex(id.as_bytes()).unwrap())
            }
        );
        assert_eq!(r.state(), RepoState::Clean);
        assert_eq!(r.resolve("HEAD").unwrap().to_string(), id);
        assert_eq!(r.identity().0.as_deref(), Some("Test"));
        t.git(&["checkout", "-q", "--detach"]);
        assert!(matches!(r.head().unwrap(), Head::Detached(_)));
    }

    #[test]
    fn unborn_and_not_a_repo() {
        let t = TestRepo::new();
        let r = Repo::open(t.path()).unwrap();
        assert_eq!(
            r.head().unwrap(),
            Head::Branch {
                name: "main".into(),
                id: None
            }
        );
        let plain = tempfile::tempdir().unwrap();
        assert!(matches!(Repo::open(plain.path()), Err(Error::NotARepo(_))));
    }
}
