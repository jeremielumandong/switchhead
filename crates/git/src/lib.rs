//! switchhead's git engine.
//!
//! Reads go through [gitoxide](https://github.com/GitoxideLabs/gitoxide) (`gix`) in process:
//! the commit log and graph, refs, commit diffs. They are the hot paths and never spawn a
//! process. Everything that changes a repository goes through the system `git` binary
//! ([`cli::Git`]), so hooks, credential helpers, signing, LFS and the user's config behave
//! exactly as they do on the command line. Working-tree status and index diffs also use
//! `git`, which has fsmonitor and the untracked cache for big trees.
//!
//! Nothing here touches a UI: callers run these functions off the UI thread.

pub mod blame;
pub mod cli;
pub mod commit;
pub mod conflict;
pub mod diff;
pub mod discover;
pub mod flow;
pub mod graph;
pub mod identity;
pub mod lfs;
pub mod log;
pub mod ops;
pub mod rebase;
pub mod reflog;
pub mod refs;
pub mod repo;
pub mod search;
pub mod stage;
pub mod status;
pub mod submodules;
pub mod tree;
pub mod undo;
pub mod watch;
pub mod web;
pub mod worktrees;

use std::fmt::Display;
use std::path::PathBuf;

pub use gix::ObjectId as Oid;
pub use repo::{Repo, RepoState};

/// Errors from the git engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// gitoxide reported an error.
    #[error("{0}")]
    Gix(String),
    /// A `git` command failed.
    #[error("git {args} failed: {message}")]
    Command {
        /// The arguments, for the message.
        args: String,
        /// Exit code (`None`: killed by a signal).
        code: Option<i32>,
        /// What git printed on stderr (or stdout when stderr was empty).
        message: String,
    },
    /// `git` could not be started.
    #[error("git is not installed or not on PATH")]
    GitMissing,
    /// The path is not inside a git repository.
    #[error("not a git repository: {}", .0.display())]
    NotARepo(PathBuf),
    /// The request cannot be done this way (the message says why).
    #[error("{0}")]
    Unsupported(String),
    /// Output from git that could not be read.
    #[error("unexpected output from git: {0}")]
    Parse(String),
    /// File system error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result alias for the git engine.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Turn any displayable error (gitoxide's) into [`Error::Gix`].
pub(crate) trait GixExt<T> {
    fn gix(self) -> Result<T>;
}

impl<T, E: Display> GixExt<T> for std::result::Result<T, E> {
    fn gix(self) -> Result<T> {
        self.map_err(|e| Error::Gix(e.to_string()))
    }
}

/// A repository path from git's bytes (lossy only where the platform cannot hold them).
pub(crate) fn path_from_bytes(bytes: &[u8]) -> std::path::PathBuf {
    gix::path::from_bstr(gix::bstr::BStr::new(bytes))
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_else(|_| PathBuf::from(String::from_utf8_lossy(bytes).into_owned()))
}

/// The first seven hex digits of an id.
pub fn short(id: &Oid) -> String {
    let mut s = id.to_hex().to_string();
    s.truncate(7);
    s
}

#[cfg(test)]
pub(crate) mod testutil {
    //! Build throwaway repositories with the real `git`.

    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// A temporary repository with a fixed identity and dates.
    pub struct TestRepo {
        pub dir: tempfile::TempDir,
        clock: std::cell::Cell<i64>,
    }

    impl TestRepo {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let r = Self {
                dir,
                clock: std::cell::Cell::new(1_700_000_000),
            };
            r.git(&["init", "-q", "-b", "main"]);
            r.git(&["config", "user.name", "Test"]);
            r.git(&["config", "user.email", "test@example.com"]);
            r.git(&["config", "commit.gpgsign", "false"]);
            // Git for Windows turns autocrlf on in its system config, which `cli::Git`
            // (rightly) reads; tests compare bytes, so keep files as written.
            r.git(&["config", "core.autocrlf", "false"]);
            r
        }

        pub fn path(&self) -> &Path {
            self.dir.path()
        }

        pub fn join(&self, p: &str) -> PathBuf {
            self.dir.path().join(p)
        }

        /// Run git, panic on failure, return stdout.
        pub fn git(&self, args: &[&str]) -> String {
            let t = self.clock.get();
            self.clock.set(t + 60);
            let date = format!("{t} +0000");
            let out = Command::new("git")
                .args(args)
                .current_dir(self.dir.path())
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", self.dir.path())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        }

        pub fn write(&self, file: &str, text: &str) {
            let p = self.join(file);
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d).unwrap();
            }
            std::fs::write(p, text).unwrap();
        }

        /// Write, stage and commit one file. Returns the new commit id.
        pub fn commit_file(&self, file: &str, text: &str, msg: &str) -> String {
            self.write(file, text);
            self.git(&["add", file]);
            self.git(&["commit", "-q", "-m", msg]);
            self.git(&["rev-parse", "HEAD"]).trim().to_owned()
        }
    }
}
