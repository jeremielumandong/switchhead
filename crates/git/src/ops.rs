//! Every write the app performs, as thin wrappers over the system `git`.
//!
//! Each method builds one `git` command line (so hooks, signing, credential helpers and
//! the user's config apply) and maps failures to [`Error::Command`] with git's own
//! message. Network operations stream progress lines and take a [`Cancel`]. Nothing
//! here decides policy (confirmation, which branch): the UI does that first.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use crate::cli::{Cancel, Output};
use crate::{Error, Repo, RepoState, Result};

/// Options for [`Repo::commit`].
#[derive(Clone, Copy, Debug, Default)]
pub struct CommitOptions {
    /// Replace the last commit (`--amend`).
    pub amend: bool,
    /// Add a `Signed-off-by` trailer (`--signoff`).
    pub signoff: bool,
    /// Skip the pre-commit and commit-msg hooks (`--no-verify`).
    pub no_verify: bool,
    /// Allow a commit with no changes (`--allow-empty`).
    pub allow_empty: bool,
}

/// How [`Repo::merge`] combines histories.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MergeMode {
    /// Fast-forward when possible, else a merge commit (git's default).
    #[default]
    Default,
    /// Only fast-forward (`--ff-only`).
    FastForwardOnly,
    /// Always a merge commit (`--no-ff`).
    NoFastForward,
    /// Squash into the index without committing (`--squash`).
    Squash,
}

/// How [`Repo::reset`] moves HEAD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetMode {
    /// Keep index and work tree.
    Soft,
    /// Reset the index, keep the work tree.
    Mixed,
    /// Reset index and work tree (loses uncommitted changes).
    Hard,
}

/// Options for [`Repo::pull`].
#[derive(Clone, Copy, Debug, Default)]
pub struct PullOptions {
    /// Rebase instead of merging (`--rebase`).
    pub rebase: bool,
    /// Stash and restore local changes around the pull (`--autostash`).
    pub autostash: bool,
}

/// Options for [`Repo::push`].
#[derive(Clone, Debug, Default)]
pub struct PushOptions {
    /// Remote name (`origin`).
    pub remote: String,
    /// Refspec or branch (`main`, `HEAD:refs/heads/x`); empty pushes git's default.
    pub refspec: String,
    /// `--force-with-lease`.
    pub force_with_lease: bool,
    /// Also push tags (`--tags`).
    pub tags: bool,
    /// Record the upstream (`--set-upstream`).
    pub set_upstream: bool,
}

/// Options for [`Repo::stash_push`].
#[derive(Clone, Debug, Default)]
pub struct StashOptions {
    /// Message.
    pub message: Option<String>,
    /// Include untracked files (`--include-untracked`).
    pub untracked: bool,
    /// Only what is staged (`--staged`).
    pub staged_only: bool,
    /// Leave the index as it is (`--keep-index`).
    pub keep_index: bool,
}

/// What [`Repo::bisect_mark`] says about the current commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BisectMark {
    /// Has the bug.
    Bad,
    /// Does not have the bug.
    Good,
    /// Cannot be tested.
    Skip,
}

fn os<S: AsRef<OsStr>>(s: S) -> OsString {
    s.as_ref().to_owned()
}

impl Repo {
    fn run_git(&self, args: Vec<OsString>) -> Result<Output> {
        self.git()?.run(&args)
    }

    fn run_paths(&self, mut args: Vec<OsString>, paths: &[&Path]) -> Result<Output> {
        args.push("--".into());
        args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
        self.run_git(args)
    }

    // ---- Local changes (M3) ----

    /// Stage paths (`git add -A -- paths`: new, changed and deleted files).
    pub fn stage(&self, paths: &[&Path]) -> Result<()> {
        self.run_paths(vec![os("add"), os("-A")], paths)?;
        Ok(())
    }

    /// Stage everything.
    pub fn stage_all(&self) -> Result<()> {
        self.run_git(vec![os("add"), os("-A")])?;
        Ok(())
    }

    /// Unstage paths (`git restore --staged`; `git rm --cached` before the first commit).
    pub fn unstage(&self, paths: &[&Path]) -> Result<()> {
        if self.head()?.id().is_none() {
            self.run_paths(vec![os("rm"), os("-q"), os("--cached"), os("-r")], paths)?;
        } else {
            self.run_paths(vec![os("restore"), os("--staged")], paths)?;
        }
        Ok(())
    }

    /// Unstage everything.
    pub fn unstage_all(&self) -> Result<()> {
        if self.head()?.id().is_none() {
            self.run_git(vec![
                os("rm"),
                os("-q"),
                os("--cached"),
                os("-r"),
                os("."),
                os("--ignore-unmatch"),
            ])?;
        } else {
            self.run_git(vec![os("reset"), os("-q")])?;
        }
        Ok(())
    }

    /// Throw away unstaged changes to tracked paths (`git restore`). Cannot be undone.
    pub fn discard(&self, paths: &[&Path]) -> Result<()> {
        self.run_paths(vec![os("restore")], paths)?;
        Ok(())
    }

    /// Delete untracked files (`git clean -f`, `-d` for directories). Cannot be undone.
    pub fn delete_untracked(&self, paths: &[&Path]) -> Result<()> {
        self.run_paths(vec![os("clean"), os("-f"), os("-d"), os("-q")], paths)?;
        Ok(())
    }

    /// Append a pattern to the work tree's `.gitignore` (creating it).
    pub fn ignore(&self, pattern: &str) -> Result<()> {
        let path = self.root().join(".gitignore");
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.lines().any(|l| l == pattern) {
            return Ok(());
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(pattern);
        text.push('\n');
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Set or clear `--assume-unchanged` on paths.
    pub fn set_assume_unchanged(&self, paths: &[&Path], on: bool) -> Result<()> {
        let flag = if on {
            "--assume-unchanged"
        } else {
            "--no-assume-unchanged"
        };
        self.run_paths(vec![os("update-index"), os(flag)], paths)?;
        Ok(())
    }

    /// Set or clear `--skip-worktree` on paths.
    pub fn set_skip_worktree(&self, paths: &[&Path], on: bool) -> Result<()> {
        let flag = if on {
            "--skip-worktree"
        } else {
            "--no-skip-worktree"
        };
        self.run_paths(vec![os("update-index"), os(flag)], paths)?;
        Ok(())
    }

    /// Commit the index with `message` (passed on stdin, so any text is safe).
    pub fn commit(&self, message: &str, opts: CommitOptions) -> Result<()> {
        let mut args = vec!["commit", "-q", "--cleanup=strip", "-F", "-"];
        if opts.amend {
            args.push("--amend");
        }
        if opts.signoff {
            args.push("--signoff");
        }
        if opts.no_verify {
            args.push("--no-verify");
        }
        if opts.allow_empty {
            args.push("--allow-empty");
        }
        self.git()?.run_with(&args, Some(message.as_bytes()), &[])?;
        Ok(())
    }

    /// The last commit's full message (for amend).
    pub fn last_message(&self) -> Result<String> {
        Ok(self
            .git()?
            .text(&["log", "-1", "--format=%B"])?
            .trim_end()
            .to_owned())
    }

    /// The commit message template (`commit.template`), if one is set and readable.
    /// A relative path is relative to the work tree, as for `git commit`.
    pub fn commit_template(&self) -> Result<Option<String>> {
        let path = match self
            .git()?
            .text(&["config", "--path", "--get", "commit.template"])
        {
            Ok(p) => p.trim_end_matches(['\n', '\r']).to_owned(),
            // Exit code 1: the key is not set.
            Err(Error::Command { code: Some(1), .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        if path.is_empty() {
            return Ok(None);
        }
        match std::fs::read(self.root().join(path)) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(e) => {
                tracing::warn!(error = %e, "could not read the commit template");
                Ok(None)
            }
        }
    }

    /// Mark conflicted paths resolved (stage them).
    pub fn mark_resolved(&self, paths: &[&Path]) -> Result<()> {
        self.run_paths(vec![os("add")], paths)?;
        Ok(())
    }

    /// Check out the ours (`--ours`) or theirs (`--theirs`) side of conflicted paths.
    pub fn checkout_side(&self, paths: &[&Path], theirs: bool) -> Result<()> {
        let side = if theirs { "--theirs" } else { "--ours" };
        self.run_paths(vec![os("checkout"), os(side)], paths)?;
        Ok(())
    }

    fn sequence(&self, verb: &str) -> Result<()> {
        let Some(cmd) = self.state().command() else {
            return Err(Error::Unsupported("no operation in progress".into()));
        };
        self.run_git(vec![os(cmd), os(verb)])?;
        Ok(())
    }

    /// Continue the merge, rebase, cherry-pick, revert or `am` in progress.
    pub fn continue_operation(&self) -> Result<()> {
        if self.state() == RepoState::Merge {
            // `merge --continue` would open an editor; commit with the prepared message.
            self.run_git(vec![os("commit"), os("--no-edit"), os("-q")])?;
            return Ok(());
        }
        self.sequence("--continue")
    }

    /// Abort the operation in progress.
    pub fn abort_operation(&self) -> Result<()> {
        self.sequence("--abort")
    }

    /// Skip the current commit of a rebase, cherry-pick, revert or `am`.
    pub fn skip_operation(&self) -> Result<()> {
        self.sequence("--skip")
    }

    /// `git rebase --continue` (alias of [`Repo::continue_operation`] during a rebase).
    pub fn rebase_continue(&self) -> Result<()> {
        self.continue_operation()
    }

    // ---- Branches, commits, history (M4) ----

    /// Check out a branch, tag or commit. A remote branch (`origin/x`) creates a local
    /// tracking branch `x`.
    pub fn checkout(&self, rev: &str) -> Result<()> {
        let is_remote = self
            .git()?
            .run(&["show-ref", "--verify", "-q", &format!("refs/remotes/{rev}")])
            .is_ok();
        if is_remote && let Some((_, name)) = rev.split_once('/') {
            let local_exists = self
                .git()?
                .run(&["show-ref", "--verify", "-q", &format!("refs/heads/{name}")])
                .is_ok();
            if !local_exists {
                self.run_git(vec![os("checkout"), os("-q"), os("--track"), os(rev)])?;
                return Ok(());
            }
            self.run_git(vec![os("checkout"), os("-q"), os(name)])?;
            return Ok(());
        }
        self.run_git(vec![os("checkout"), os("-q"), os(rev)])?;
        Ok(())
    }

    /// Create a branch at `start` (a rev), optionally checking it out.
    pub fn create_branch(&self, name: &str, start: &str, checkout: bool) -> Result<()> {
        if checkout {
            self.run_git(vec![
                os("checkout"),
                os("-q"),
                os("-b"),
                os(name),
                os(start),
            ])?;
        } else {
            self.run_git(vec![os("branch"), os(name), os(start)])?;
        }
        Ok(())
    }

    /// Rename a local branch.
    pub fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        self.run_git(vec![os("branch"), os("-m"), os(old), os(new)])?;
        Ok(())
    }

    /// Delete a local branch (`force`: even if unmerged).
    pub fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        let flag = if force { "-D" } else { "-d" };
        self.run_git(vec![os("branch"), os(flag), os(name)])?;
        Ok(())
    }

    /// Set (or with `None`, unset) a branch's upstream.
    pub fn set_upstream(&self, branch: &str, upstream: Option<&str>) -> Result<()> {
        match upstream {
            Some(u) => self.run_git(vec![
                os("branch"),
                os(format!("--set-upstream-to={u}")),
                os(branch),
            ])?,
            None => self.run_git(vec![os("branch"), os("--unset-upstream"), os(branch)])?,
        };
        Ok(())
    }

    /// Merge `rev` into the current branch.
    pub fn merge(&self, rev: &str, mode: MergeMode) -> Result<()> {
        let mut args = vec![os("merge"), os("-q"), os("--no-edit")];
        match mode {
            MergeMode::Default => {}
            MergeMode::FastForwardOnly => args.push(os("--ff-only")),
            MergeMode::NoFastForward => args.push(os("--no-ff")),
            MergeMode::Squash => args.push(os("--squash")),
        }
        args.push(os(rev));
        self.run_git(args)?;
        Ok(())
    }

    /// Rebase the current branch onto `onto`.
    pub fn rebase_onto(&self, onto: &str) -> Result<()> {
        self.run_git(vec![os("rebase"), os("-q"), os(onto)])?;
        Ok(())
    }

    /// Cherry-pick commits, oldest first.
    pub fn cherry_pick(&self, revs: &[&str]) -> Result<()> {
        let mut args = vec![os("cherry-pick")];
        args.extend(revs.iter().map(os));
        self.run_git(args)?;
        Ok(())
    }

    /// Revert a commit (a new commit undoing it).
    pub fn revert(&self, rev: &str) -> Result<()> {
        self.run_git(vec![os("revert"), os("--no-edit"), os(rev)])?;
        Ok(())
    }

    /// Reset the current branch to `rev`.
    pub fn reset(&self, rev: &str, mode: ResetMode) -> Result<()> {
        let flag = match mode {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        };
        self.run_git(vec![os("reset"), os("-q"), os(flag), os(rev)])?;
        Ok(())
    }

    /// Create a tag at `rev`; annotated when `message` is given.
    pub fn create_tag(&self, name: &str, rev: &str, message: Option<&str>) -> Result<()> {
        match message {
            Some(m) => {
                self.git()?.run_with(
                    &["tag", "-a", name, rev, "-F", "-"],
                    Some(m.as_bytes()),
                    &[],
                )?;
            }
            None => {
                self.run_git(vec![os("tag"), os(name), os(rev)])?;
            }
        }
        Ok(())
    }

    /// Delete a local tag.
    pub fn delete_tag(&self, name: &str) -> Result<()> {
        self.run_git(vec![os("tag"), os("-d"), os(name)])?;
        Ok(())
    }

    /// Stash changes.
    pub fn stash_push(&self, opts: &StashOptions) -> Result<()> {
        let mut args = vec![os("stash"), os("push"), os("-q")];
        if opts.untracked {
            args.push(os("--include-untracked"));
        }
        if opts.staged_only {
            args.push(os("--staged"));
        }
        if opts.keep_index {
            args.push(os("--keep-index"));
        }
        if let Some(m) = &opts.message {
            args.push(os("-m"));
            args.push(os(m));
        }
        self.run_git(args)?;
        Ok(())
    }

    /// Apply `stash@{index}` (`pop`: and drop it on success).
    pub fn stash_apply(&self, index: usize, pop: bool) -> Result<()> {
        let verb = if pop { "pop" } else { "apply" };
        self.run_git(vec![
            os("stash"),
            os(verb),
            os("-q"),
            os(format!("stash@{{{index}}}")),
        ])?;
        Ok(())
    }

    /// Drop `stash@{index}`.
    pub fn stash_drop(&self, index: usize) -> Result<()> {
        self.run_git(vec![
            os("stash"),
            os("drop"),
            os("-q"),
            os(format!("stash@{{{index}}}")),
        ])?;
        Ok(())
    }

    /// Create and check out a branch from `stash@{index}` (and drop the stash).
    pub fn stash_branch(&self, name: &str, index: usize) -> Result<()> {
        self.run_git(vec![
            os("stash"),
            os("branch"),
            os(name),
            os(format!("stash@{{{index}}}")),
        ])?;
        Ok(())
    }

    /// Add a remote.
    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.run_git(vec![os("remote"), os("add"), os(name), os(url)])?;
        Ok(())
    }

    /// Change a remote's URL.
    pub fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
        self.run_git(vec![os("remote"), os("set-url"), os(name), os(url)])?;
        Ok(())
    }

    /// Rename a remote.
    pub fn rename_remote(&self, old: &str, new: &str) -> Result<()> {
        self.run_git(vec![os("remote"), os("rename"), os(old), os(new)])?;
        Ok(())
    }

    /// Remove a remote and its remote-tracking branches.
    pub fn remove_remote(&self, name: &str) -> Result<()> {
        self.run_git(vec![os("remote"), os("remove"), os(name)])?;
        Ok(())
    }

    // ---- Network (M4-3); progress lines go to `on_line` ----

    fn streaming(&self, args: &[&str], cancel: &Cancel, on_line: impl FnMut(&str)) -> Result<()> {
        self.git()?.run_streaming(args, cancel, on_line)?;
        Ok(())
    }

    /// Fetch a remote (`None`: all remotes), optionally pruning deleted branches.
    pub fn fetch(
        &self,
        remote: Option<&str>,
        prune: bool,
        cancel: &Cancel,
        on_line: impl FnMut(&str),
    ) -> Result<()> {
        let mut args = vec!["fetch", "--progress"];
        if prune {
            args.push("--prune");
        }
        match remote {
            Some(r) => args.push(r),
            None => args.push("--all"),
        }
        self.streaming(&args, cancel, on_line)
    }

    /// Pull the current branch's upstream.
    pub fn pull(
        &self,
        opts: PullOptions,
        cancel: &Cancel,
        on_line: impl FnMut(&str),
    ) -> Result<()> {
        let mut args = vec!["pull", "--progress", "--no-edit"];
        args.push(if opts.rebase {
            "--rebase"
        } else {
            "--no-rebase"
        });
        if opts.autostash {
            args.push("--autostash");
        }
        self.streaming(&args, cancel, on_line)
    }

    /// Push.
    pub fn push(
        &self,
        opts: &PushOptions,
        cancel: &Cancel,
        on_line: impl FnMut(&str),
    ) -> Result<()> {
        let mut args = vec!["push", "--progress"];
        if opts.force_with_lease {
            args.push("--force-with-lease");
        }
        if opts.tags {
            args.push("--tags");
        }
        if opts.set_upstream {
            args.push("--set-upstream");
        }
        if !opts.remote.is_empty() {
            args.push(&opts.remote);
            if !opts.refspec.is_empty() {
                args.push(&opts.refspec);
            }
        }
        self.streaming(&args, cancel, on_line)
    }

    /// Delete a branch or tag on a remote (`git push <remote> --delete <name>`).
    pub fn delete_remote_ref(
        &self,
        remote: &str,
        name: &str,
        cancel: &Cancel,
        on_line: impl FnMut(&str),
    ) -> Result<()> {
        self.streaming(
            &["push", "--progress", remote, "--delete", name],
            cancel,
            on_line,
        )
    }

    /// Remove remote-tracking branches whose remote branch is gone.
    pub fn prune_remote(&self, remote: &str) -> Result<()> {
        self.run_git(vec![os("remote"), os("prune"), os(remote)])?;
        Ok(())
    }

    // ---- Advanced (M6) ----

    /// Initialise and update submodules (recursively).
    pub fn submodule_update(&self, cancel: &Cancel, on_line: impl FnMut(&str)) -> Result<()> {
        self.streaming(
            &["submodule", "update", "--init", "--recursive", "--progress"],
            cancel,
            on_line,
        )
    }

    /// Add a submodule at `path`.
    pub fn submodule_add(&self, url: &str, path: &Path) -> Result<()> {
        self.run_git(vec![
            os("submodule"),
            os("add"),
            os("-q"),
            os(url),
            path.as_os_str().to_owned(),
        ])?;
        Ok(())
    }

    /// Add a linked worktree at `path` for `rev` (`new_branch`: create it there).
    pub fn worktree_add(&self, path: &Path, rev: &str, new_branch: Option<&str>) -> Result<()> {
        let mut args = vec![os("worktree"), os("add"), os("-q")];
        if let Some(b) = new_branch {
            args.push(os("-b"));
            args.push(os(b));
        }
        args.push(path.as_os_str().to_owned());
        args.push(os(rev));
        self.run_git(args)?;
        Ok(())
    }

    /// Remove a linked worktree (`force`: even with changes).
    pub fn worktree_remove(&self, path: &Path, force: bool) -> Result<()> {
        let mut args = vec![os("worktree"), os("remove")];
        if force {
            args.push(os("--force"));
        }
        args.push(path.as_os_str().to_owned());
        self.run_git(args)?;
        Ok(())
    }

    /// Run a `git lfs` subcommand (`pull`, `fetch`, `prune`, `lock <path>`...).
    pub fn lfs(&self, args: &[&str], cancel: &Cancel, on_line: impl FnMut(&str)) -> Result<()> {
        let mut all = vec!["lfs"];
        all.extend_from_slice(args);
        self.streaming(&all, cancel, on_line)
    }

    /// Write patches for `range` (`a..b`, or one commit with `-1 <id>`) into `dir`.
    /// Returns the files written.
    pub fn format_patch(&self, range: &[&str], dir: &Path) -> Result<Vec<std::path::PathBuf>> {
        let mut args = vec![os("format-patch"), os("-o"), dir.as_os_str().to_owned()];
        args.extend(range.iter().map(os));
        let out = self.run_git(args)?;
        Ok(out
            .text()
            .lines()
            .filter(|l| !l.is_empty())
            .map(std::path::PathBuf::from)
            .collect())
    }

    /// Apply mailbox patches as commits (`git am`).
    pub fn apply_mailbox(&self, patches: &[&Path]) -> Result<()> {
        let mut args = vec![os("am"), os("-q")];
        args.extend(patches.iter().map(|p| p.as_os_str().to_owned()));
        self.run_git(args)?;
        Ok(())
    }

    /// Apply a plain patch to the work tree (`index`: also to the index).
    pub fn apply_patch(&self, patch: &Path, index: bool) -> Result<()> {
        let mut args = vec![os("apply")];
        if index {
            args.push(os("--index"));
        }
        args.push(patch.as_os_str().to_owned());
        self.run_git(args)?;
        Ok(())
    }

    /// Start bisecting between a bad and a good commit.
    pub fn bisect_start(&self, bad: &str, good: &str) -> Result<()> {
        self.run_git(vec![os("bisect"), os("start"), os(bad), os(good)])?;
        Ok(())
    }

    /// Mark the current commit; returns git's report (the next commit, or the culprit).
    pub fn bisect_mark(&self, mark: BisectMark) -> Result<String> {
        let verb = match mark {
            BisectMark::Bad => "bad",
            BisectMark::Good => "good",
            BisectMark::Skip => "skip",
        };
        Ok(self.run_git(vec![os("bisect"), os(verb)])?.text())
    }

    /// End bisecting and return to where it started.
    pub fn bisect_reset(&self) -> Result<()> {
        self.run_git(vec![os("bisect"), os("reset")])?;
        Ok(())
    }
}

/// Clone `url` into `dest`, streaming progress.
pub fn clone(url: &str, dest: &Path, cancel: &Cancel, on_line: impl FnMut(&str)) -> Result<Repo> {
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let git = crate::cli::Git::new(parent)?;
    git.run_streaming(
        &[
            os("clone"),
            os("--progress"),
            os(url),
            dest.as_os_str().to_owned(),
        ],
        cancel,
        on_line,
    )?;
    Repo::open(dest)
}

/// Create a repository at `path` (with an initial branch name, when given).
pub fn init(path: &Path, initial_branch: Option<&str>) -> Result<Repo> {
    std::fs::create_dir_all(path)?;
    let git = crate::cli::Git::new(path)?;
    let mut args = vec![os("init"), os("-q")];
    if let Some(b) = initial_branch {
        args.push(os("-b"));
        args.push(os(b));
    }
    git.run(&args)?;
    Repo::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{StatusOptions, status};
    use crate::testutil::TestRepo;

    fn repo(t: &TestRepo) -> Repo {
        Repo::open(t.path()).unwrap()
    }

    fn quiet() -> (Cancel, impl FnMut(&str)) {
        (Cancel::default(), |_: &str| {})
    }

    #[test]
    fn reads_the_commit_template() {
        let t = TestRepo::new();
        let r = repo(&t);
        assert_eq!(r.commit_template().unwrap(), None);
        t.write(".gitmessage", "Subject\n\n# Why?\n");
        t.git(&["config", "commit.template", ".gitmessage"]);
        assert_eq!(
            r.commit_template().unwrap().as_deref(),
            Some("Subject\n\n# Why?\n")
        );
        t.git(&["config", "commit.template", "missing.txt"]);
        assert_eq!(r.commit_template().unwrap(), None);
    }

    #[test]
    fn local_changes_round_trip() {
        let t = TestRepo::new();
        let r = repo(&t);
        t.write("a.txt", "a\n");
        // Before the first commit, unstage uses `rm --cached`.
        r.stage(&[Path::new("a.txt")]).unwrap();
        r.unstage(&[Path::new("a.txt")]).unwrap();
        r.stage_all().unwrap();
        r.commit("first\n\nbody", CommitOptions::default()).unwrap();
        assert_eq!(r.last_message().unwrap(), "first\n\nbody");
        r.commit(
            "first, amended",
            CommitOptions {
                amend: true,
                signoff: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            r.last_message()
                .unwrap()
                .starts_with("first, amended\n\nSigned-off-by: Test")
        );
        assert_eq!(t.git(&["rev-list", "--count", "HEAD"]).trim(), "1");

        t.write("a.txt", "changed\n");
        t.write("junk/x.tmp", "x");
        r.stage(&[Path::new("a.txt")]).unwrap();
        r.unstage_all().unwrap();
        r.discard(&[Path::new("a.txt")]).unwrap();
        assert_eq!(std::fs::read_to_string(t.join("a.txt")).unwrap(), "a\n");
        r.delete_untracked(&[Path::new("junk")]).unwrap();
        assert!(!t.join("junk").exists());

        r.ignore("*.log").unwrap();
        r.ignore("*.log").unwrap();
        assert_eq!(
            std::fs::read_to_string(t.join(".gitignore")).unwrap(),
            "*.log\n"
        );

        r.set_assume_unchanged(&[Path::new("a.txt")], true).unwrap();
        t.write("a.txt", "hidden\n");
        let s = status(&r, StatusOptions::default()).unwrap();
        assert!(s.entries.iter().all(|e| e.path != Path::new("a.txt")));
        r.set_assume_unchanged(&[Path::new("a.txt")], false)
            .unwrap();
        r.set_skip_worktree(&[Path::new("a.txt")], true).unwrap();
        r.set_skip_worktree(&[Path::new("a.txt")], false).unwrap();
        assert!(matches!(
            r.commit("nothing", CommitOptions::default()),
            Err(Error::Command { .. })
        ));
        r.commit(
            "empty",
            CommitOptions {
                allow_empty: true,
                no_verify: true,
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn branches_merge_reset_tags_and_conflicts() {
        let t = TestRepo::new();
        let r = repo(&t);
        t.commit_file("f.txt", "base\n", "base");
        r.create_branch("feature", "HEAD", true).unwrap();
        t.commit_file("g.txt", "g\n", "feature work");
        r.checkout("main").unwrap();
        r.merge("feature", MergeMode::NoFastForward).unwrap();
        assert_eq!(
            t.git(&["rev-list", "--count", "--merges", "HEAD"]).trim(),
            "1"
        );
        r.rename_branch("feature", "feat").unwrap();
        r.delete_branch("feat", false).unwrap();

        r.create_tag("v1", "HEAD", Some("release one")).unwrap();
        r.create_tag("light", "HEAD~1", None).unwrap();
        assert_eq!(t.git(&["tag"]).trim(), "light\nv1");
        r.delete_tag("light").unwrap();

        r.reset("HEAD~1", ResetMode::Hard).unwrap();
        assert!(!t.join("g.txt").exists());
        let tip = t.git(&["rev-parse", "v1^2"]).trim().to_owned();
        r.cherry_pick(&[&tip]).unwrap();
        assert!(t.join("g.txt").exists());
        r.revert("HEAD").unwrap();
        assert!(!t.join("g.txt").exists());

        // A conflict, resolved with "theirs" and continued.
        r.create_branch("other", "HEAD", false).unwrap();
        t.commit_file("f.txt", "ours\n", "ours");
        r.checkout("other").unwrap();
        t.commit_file("f.txt", "theirs\n", "theirs");
        r.checkout("main").unwrap();
        assert!(r.merge("other", MergeMode::Default).is_err());
        assert_eq!(r.state(), RepoState::Merge);
        r.checkout_side(&[Path::new("f.txt")], true).unwrap();
        r.mark_resolved(&[Path::new("f.txt")]).unwrap();
        r.continue_operation().unwrap();
        assert_eq!(r.state(), RepoState::Clean);
        assert_eq!(
            std::fs::read_to_string(t.join("f.txt")).unwrap(),
            "theirs\n"
        );
        assert!(matches!(r.abort_operation(), Err(Error::Unsupported(_))));

        // Rebase onto, and an aborted cherry-pick.
        r.create_branch("topic", "HEAD~2", true).unwrap();
        t.commit_file("t.txt", "t\n", "topic");
        r.rebase_onto("main").unwrap();
        assert_eq!(
            t.git(&["log", "-1", "--format=%s", "HEAD~1"]).trim(),
            "Merge branch 'other'"
        );
        r.reset("HEAD", ResetMode::Mixed).unwrap();
        r.reset("HEAD", ResetMode::Soft).unwrap();
    }

    #[test]
    fn stashes() {
        let t = TestRepo::new();
        let r = repo(&t);
        t.commit_file("a.txt", "a\n", "one");
        t.write("a.txt", "dirty\n");
        t.write("new.txt", "n\n");
        r.stash_push(&StashOptions {
            message: Some("work".into()),
            untracked: true,
            ..Default::default()
        })
        .unwrap();
        assert!(!t.join("new.txt").exists());
        r.stash_apply(0, false).unwrap();
        assert!(t.join("new.txt").exists());
        r.stash_drop(0).unwrap();
        r.stash_push(&StashOptions {
            untracked: true,
            ..Default::default()
        })
        .unwrap();
        r.stash_branch("from-stash", 0).unwrap();
        assert_eq!(r.head().unwrap().branch(), Some("from-stash"));
        assert_eq!(std::fs::read_to_string(t.join("a.txt")).unwrap(), "dirty\n");
    }

    #[test]
    fn remotes_fetch_pull_push_clone() {
        let origin = TestRepo::new();
        origin.commit_file("a.txt", "1\n", "one");
        let bare = tempfile::tempdir().unwrap();
        let bare_path = bare.path().join("origin.git");
        origin.git(&["clone", "-q", "--bare", ".", &bare_path.to_string_lossy()]);

        let work = tempfile::tempdir().unwrap();
        let (cancel, sink) = quiet();
        let r = clone(
            &bare_path.to_string_lossy(),
            &work.path().join("clone"),
            &cancel,
            sink,
        )
        .unwrap();
        // Identity for commits in the clone.
        r.git()
            .unwrap()
            .run(&["config", "user.name", "Test"])
            .unwrap();
        r.git()
            .unwrap()
            .run(&["config", "user.email", "t@example.com"])
            .unwrap();
        r.git()
            .unwrap()
            .run(&["config", "commit.gpgsign", "false"])
            .unwrap();

        std::fs::write(r.root().join("b.txt"), "b\n").unwrap();
        r.stage_all().unwrap();
        r.commit("two", CommitOptions::default()).unwrap();
        let mut lines = Vec::new();
        r.push(
            &PushOptions {
                remote: "origin".into(),
                refspec: "main".into(),
                ..Default::default()
            },
            &Cancel::default(),
            |l| lines.push(l.to_owned()),
        )
        .unwrap();

        // A second clone pulls it.
        let (cancel, sink) = quiet();
        let r2 = clone(
            &bare_path.to_string_lossy(),
            &work.path().join("two"),
            &cancel,
            sink,
        )
        .unwrap();
        assert!(r2.root().join("b.txt").exists());

        r.create_branch("topic", "HEAD", false).unwrap();
        let (cancel, sink) = quiet();
        r.push(
            &PushOptions {
                remote: "origin".into(),
                refspec: "topic".into(),
                set_upstream: true,
                ..Default::default()
            },
            &cancel,
            sink,
        )
        .unwrap();
        let (cancel, sink) = quiet();
        r2.fetch(None, true, &cancel, sink).unwrap();
        r2.checkout("origin/topic").unwrap();
        assert_eq!(r2.head().unwrap().branch(), Some("topic"));
        let (cancel, sink) = quiet();
        r2.pull(
            PullOptions {
                rebase: true,
                autostash: true,
            },
            &cancel,
            sink,
        )
        .unwrap();

        let (cancel, sink) = quiet();
        r.delete_remote_ref("origin", "topic", &cancel, sink)
            .unwrap();
        r2.prune_remote("origin").unwrap();
        assert!(
            !r2.git()
                .unwrap()
                .text(&["branch", "-r"])
                .unwrap()
                .contains("origin/topic")
        );
        r2.set_upstream("topic", None).unwrap();

        r.add_remote("mirror", "https://example.invalid/x.git")
            .unwrap();
        r.set_remote_url("mirror", "https://example.invalid/y.git")
            .unwrap();
        r.rename_remote("mirror", "backup").unwrap();
        r.remove_remote("backup").unwrap();

        // Cancelled before it starts: reported as an error, nothing hangs.
        let cancel = Cancel::default();
        cancel.cancel();
        assert!(r.fetch(Some("origin"), false, &cancel, |_| {}).is_err());
    }

    #[test]
    fn patches_worktrees_bisect_init() {
        let t = TestRepo::new();
        let r = repo(&t);
        t.commit_file("a.txt", "1\n", "one");
        t.commit_file("a.txt", "2\n", "two");
        t.commit_file("a.txt", "3\n", "three");
        let out = tempfile::tempdir().unwrap();
        let files = r.format_patch(&["-1", "HEAD"], out.path()).unwrap();
        assert_eq!(files.len(), 1);
        r.reset("HEAD~1", ResetMode::Hard).unwrap();
        r.apply_mailbox(&[files[0].as_path()]).unwrap();
        assert_eq!(r.last_message().unwrap(), "three");

        r.reset("HEAD~1", ResetMode::Hard).unwrap();
        let p = out.path().join("plain.patch");
        std::fs::write(&p, t.git(&["diff", "HEAD", "HEAD@{1}"])).unwrap();
        r.apply_patch(&p, true).unwrap();
        assert_eq!(std::fs::read_to_string(t.join("a.txt")).unwrap(), "3\n");
        r.reset("HEAD", ResetMode::Hard).unwrap();

        let wt = out.path().join("wt");
        r.worktree_add(&wt, "HEAD", Some("wt-branch")).unwrap();
        assert!(wt.join("a.txt").exists());
        r.worktree_remove(&wt, false).unwrap();

        r.bisect_start("HEAD", "HEAD~1").unwrap();
        assert_eq!(r.state(), RepoState::Bisect);
        let report = r.bisect_mark(BisectMark::Bad).unwrap();
        assert!(report.contains("is the first"), "{report}");
        r.bisect_reset().unwrap();
        assert_eq!(r.state(), RepoState::Clean);

        let fresh = init(&out.path().join("new"), Some("trunk")).unwrap();
        assert_eq!(fresh.head().unwrap().branch(), Some("trunk"));
    }
}
