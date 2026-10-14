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
            url: None,
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
            url: None,
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
                    url: None,
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
                    url: None,
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
            url: None,
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

impl RepoView {
    /// Ask for a name and URL, add the remote and (optionally) fetch it.
    pub fn add_remote_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<String> = self
            .refs
            .remote_names()
            .iter()
            .map(|n| (*n).to_owned())
            .collect();
        let form = Form {
            title: "Add Remote".into(),
            description: "Another repository to fetch from and push to.".into(),
            ok: "Add Remote",
            danger: false,
            name: Some(if names.is_empty() {
                "origin".into()
            } else {
                String::new()
            }),
            placeholder: "Name, e.g. upstream",
            taken: names,
            message: None,
            url: Some(String::new()),
            checks: vec![("Fetch it now", true)],
        };
        self.open_form(
            form,
            |v, a, cx| {
                let fetch = a.checks[0];
                v.run_op(
                    "Adding remote",
                    "Remote added",
                    move |r, cancel, on_line| {
                        r.add_remote(&a.name, &a.url)?;
                        if fetch {
                            r.fetch(Some(&a.name), false, cancel, |l| on_line(l))?;
                        }
                        Ok(())
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Rename a remote or change its URL.
    pub(super) fn edit_remote_dialog(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old_url = self.refs.remote_url(&name).unwrap_or_default().to_owned();
        let form = Form {
            title: format!("Edit {name}"),
            description: "Renaming moves its remote branches and the upstream settings that \
                          point at them."
                .into(),
            ok: "Save",
            danger: false,
            name: Some(name.clone()),
            placeholder: "Name",
            taken: self
                .refs
                .remote_names()
                .iter()
                .filter(|n| **n != name)
                .map(|n| (*n).to_owned())
                .collect(),
            message: None,
            url: Some(old_url.clone()),
            checks: Vec::new(),
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let (old, old_url) = (name.clone(), old_url.clone());
                if a.name == old && a.url == old_url {
                    return;
                }
                v.run_op(
                    "Editing remote",
                    "Remote saved",
                    move |r, _, _| {
                        if a.url != old_url {
                            r.set_remote_url(&old, &a.url)?;
                        }
                        if a.name != old {
                            r.rename_remote(&old, &a.name)?;
                        }
                        Ok(())
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Ask, then remove a remote (and its remote-tracking branches).
    pub(super) fn remove_remote_dialog(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = Form {
            title: format!("Remove {name}?"),
            description: format!(
                "Its remote branches disappear from this repository, and branches tracking \
                 them lose their upstream. Nothing changes on {name} itself."
            ),
            ok: "Remove",
            danger: true,
            name: None,
            placeholder: "",
            taken: Vec::new(),
            message: None,
            url: None,
            checks: Vec::new(),
        };
        self.open_form(
            form,
            move |v, _, cx| {
                let name = name.clone();
                v.run_op(
                    "Removing remote",
                    "Remote removed",
                    move |r, _, _| r.remove_remote(&name),
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Fetch one remote, pruning branches deleted there.
    pub(super) fn fetch_remote(&mut self, name: String, cx: &mut Context<Self>) {
        self.run_op(
            "Fetching",
            "Fetched",
            move |r, cancel, on_line| r.fetch(Some(&name), true, cancel, |l| on_line(l)),
            cx,
        );
    }

    /// Drop remote-tracking branches whose branch is gone on the remote.
    pub(super) fn prune_remote(&mut self, name: String, cx: &mut Context<Self>) {
        self.run_op(
            "Pruning",
            "Pruned",
            move |r, _, _| r.prune_remote(&name),
            cx,
        );
    }
}
