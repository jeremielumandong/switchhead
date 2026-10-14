//! Fetch, pull and push, each through a small options dialog (Fork's toolbar flow). They
//! stream git's progress to the status bar and the activity panel, and can be cancelled.

use switchhead_git::ops::{PullOptions, PushOptions};

use super::RepoView;
use super::form::Form;
use gpui_kit::{Context, Window};

impl RepoView {
    /// The current branch's upstream (`origin/main`), when it has one that still exists.
    fn upstream(&self) -> Option<String> {
        self.refs
            .current_branch()
            .and_then(|b| b.tracking.as_ref())
            .filter(|t| !t.gone)
            .map(|t| t.upstream.clone())
    }

    /// Fetch: all remotes (or the upstream's), pruning deleted branches.
    pub fn fetch_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let remotes = self.refs.remote_names().len();
        let one = self
            .upstream()
            .and_then(|u| u.split_once('/').map(|(r, _)| r.to_owned()))
            .or_else(|| self.refs.remote_names().first().map(|r| (*r).to_owned()));
        let mut checks = vec![("Prune remote branches that were deleted", true)];
        if remotes > 1 {
            checks.insert(0, ("Fetch all remotes", true));
        }
        let form = Form {
            title: "Fetch".into(),
            description: match (&one, remotes) {
                (_, 0) => "This repository has no remotes.".into(),
                (Some(r), 1) => format!("Download new commits and branches from {r}."),
                (Some(r), _) => {
                    format!("Download new commits and branches from every remote, or only {r}.")
                }
                (None, _) => "Download new commits and branches.".into(),
            },
            ok: "Fetch",
            danger: false,
            name: None,
            placeholder: "Branch name",
            taken: Vec::new(),
            message: None,
            checks,
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let (all, prune) = if remotes > 1 {
                    (a.checks[0], a.checks[1])
                } else {
                    (true, a.checks[0])
                };
                let remote = if all { None } else { one.clone() };
                v.run_op(
                    "Fetching",
                    "Fetched",
                    move |r, cancel, on_line| {
                        r.fetch(remote.as_deref(), prune, cancel, |l| on_line(l))
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Pull the current branch's upstream, merging or rebasing.
    pub fn pull_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branch = self.refs.head_branch.clone();
        let description = match (&branch, self.upstream()) {
            (None, _) => "HEAD is detached: check out a branch to pull into.".to_owned(),
            (Some(b), None) => {
                format!("{b} has no upstream. Set one from the branch's menu, or push it first.")
            }
            (Some(b), Some(u)) => format!("Bring {u} into {b}."),
        };
        let form = Form {
            title: "Pull".into(),
            description,
            ok: "Pull",
            danger: false,
            name: None,
            placeholder: "Branch name",
            taken: Vec::new(),
            message: None,
            checks: vec![
                ("Rebase local commits instead of merging", false),
                ("Stash and reapply local changes", true),
            ],
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let opts = PullOptions {
                    rebase: a.checks[0],
                    autostash: a.checks[1],
                };
                v.run_op(
                    "Pulling",
                    "Pulled",
                    move |r, cancel, on_line| r.pull(opts, cancel, |l| on_line(l)),
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Push the current branch to its upstream, or to the default remote with
    /// `--set-upstream` when it has none.
    pub fn push_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(branch) = self.refs.head_branch.clone() else {
            self.open_form(
                Form {
                    title: "Push".into(),
                    description: "HEAD is detached: check out a branch to push.".into(),
                    ok: "OK",
                    danger: false,
                    name: None,
                    placeholder: "Branch name",
                    taken: Vec::new(),
                    message: None,
                    checks: Vec::new(),
                },
                |_, _, _| {},
                window,
                cx,
            );
            return;
        };
        let upstream = self.upstream();
        let default_remote = self
            .refs
            .remote_names()
            .iter()
            .find(|n| **n == "origin")
            .or(self.refs.remote_names().first())
            .map(|r| (*r).to_owned());
        let (remote, target) = match upstream.as_deref().and_then(|u| u.split_once('/')) {
            Some((r, b)) => (Some(r.to_owned()), b.to_owned()),
            None => (default_remote, branch.clone()),
        };
        let Some(remote) = remote else {
            self.open_form(
                Form {
                    title: "Push".into(),
                    description: "This repository has no remotes. Add one first.".into(),
                    ok: "OK",
                    danger: false,
                    name: None,
                    placeholder: "Branch name",
                    taken: Vec::new(),
                    message: None,
                    checks: Vec::new(),
                },
                |_, _, _| {},
                window,
                cx,
            );
            return;
        };
        let has_upstream = upstream.is_some();
        let form = Form {
            title: "Push".into(),
            description: format!("Push {branch} to {remote}/{target}."),
            ok: "Push",
            danger: false,
            name: None,
            placeholder: "Branch name",
            taken: Vec::new(),
            message: None,
            checks: vec![
                ("Track it as the upstream", !has_upstream),
                ("Push all tags", false),
                ("Force, unless the remote moved (--force-with-lease)", false),
            ],
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let opts = PushOptions {
                    remote: remote.clone(),
                    refspec: format!("{branch}:refs/heads/{target}"),
                    set_upstream: a.checks[0],
                    tags: a.checks[1],
                    force_with_lease: a.checks[2],
                };
                v.run_op(
                    "Pushing",
                    "Pushed",
                    move |r, cancel, on_line| r.push(&opts, cancel, |l| on_line(l)),
                    cx,
                );
            },
            window,
            cx,
        );
    }
}
