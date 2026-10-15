//! Pull requests from a repository tab: Create Pull Request for the current branch (the
//! palette, the toolbar's Commands menu) or any pushed branch (its menu). With an account
//! signed in for the remote's site, the request is created through the site's API and its
//! page opened; without one, the site's own "new pull request" page opens instead.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Focusable as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use switchhead_hosting::{NewPullRequest, RepoRef};

use super::{OpStatus, RepoView};
use crate::accounts;
use crate::appearance::{rpx, ts};
use crate::theme::palette;
use crate::ui;

/// A pull request about to be opened: where from and to.
struct Target {
    repo: RepoRef,
    /// The remote (`origin`).
    remote: String,
    /// The branch on the remote with the changes.
    head: String,
    /// Commits on the local branch not pushed yet.
    unpushed: u32,
    /// The suggested branch to merge into.
    base: String,
    /// The local branch's tip, for the suggested title.
    tip: switchhead_git::Oid,
}

impl RepoView {
    /// The URL of the remote the current branch tracks, else `origin`'s, else the first
    /// remote's.
    pub(crate) fn main_remote_url(&self) -> Option<&str> {
        let upstream_remote = self
            .refs
            .current_branch()
            .and_then(|b| b.tracking.as_ref())
            .and_then(|t| t.upstream.split_once('/').map(|(r, _)| r.to_owned()));
        let names = self.refs.remote_names();
        let remote = upstream_remote
            .or_else(|| {
                names
                    .iter()
                    .find(|n| **n == "origin")
                    .map(|n| (*n).to_owned())
            })
            .or_else(|| names.first().map(|n| (*n).to_owned()))?;
        self.refs.remote_url(&remote)
    }

    /// Where a pull request for local `branch` goes, or why there is none.
    fn pull_request_target(&self, branch: &str, cx: &App) -> Result<Target, String> {
        let b = self
            .refs
            .branches
            .iter()
            .find(|b| b.name == branch)
            .ok_or_else(|| format!("{branch} is not a local branch"))?;
        let tracking =
            b.tracking.as_ref().filter(|t| !t.gone).ok_or_else(|| {
                format!("Push {branch} first: a pull request needs it on the remote")
            })?;
        let (remote, head) = tracking
            .upstream
            .split_once('/')
            .ok_or_else(|| format!("{branch} does not track a remote branch"))?;
        let url = self
            .refs
            .remote_url(remote)
            .ok_or_else(|| format!("{remote} has no URL"))?;
        let repo = accounts::accounts(cx).parse_remote(url).ok_or_else(|| {
            format!("{remote} is not on GitHub, GitLab, Bitbucket or Azure DevOps")
        })?;
        Ok(Target {
            repo,
            remote: remote.to_owned(),
            head: head.to_owned(),
            unpushed: tracking.ahead,
            base: self.default_base(remote, head),
            tip: b.id,
        })
    }

    /// The remote's default branch (what its `HEAD` points at), else main, master or
    /// develop, never `head` itself.
    fn default_base(&self, remote: &str, head: &str) -> String {
        let on_remote: Vec<_> = self
            .refs
            .remotes
            .iter()
            .filter(|r| r.remote == remote)
            .collect();
        let default = on_remote
            .iter()
            .find(|r| r.is_symbolic_head)
            .and_then(|h| {
                on_remote
                    .iter()
                    .filter(|r| !r.is_symbolic_head && r.id == h.id && r.name != head)
                    .min_by_key(|r| !matches!(r.name.as_str(), "main" | "master"))
            })
            .map(|r| r.name.clone());
        default
            .or_else(|| {
                ["main", "master", "develop"]
                    .into_iter()
                    .find(|n| *n != head && on_remote.iter().any(|r| r.name == *n))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "main".to_owned())
    }

    fn pr_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.op = Some(OpStatus::Failed(
            format!("Create Pull Request: {message}").into(),
        ));
        cx.notify();
    }

    /// Create a pull request for `branch` (the current branch when `None`).
    pub fn create_pull_request(
        &mut self,
        branch: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy() {
            return;
        }
        let Some(branch) = branch.or_else(|| self.refs.head_branch.clone()) else {
            self.pr_failed("check out a branch first".into(), cx);
            return;
        };
        let target = match self.pull_request_target(&branch, cx) {
            Ok(t) => t,
            Err(e) => return self.pr_failed(e, cx),
        };
        let all = accounts::accounts(cx);
        let Some(account) = all.for_repo(&target.repo).cloned() else {
            // No account for this site: its own page takes it from here.
            let url = target
                .repo
                .new_pull_request_page(&target.head, &target.base);
            cx.open_url(&url);
            self.op = Some(OpStatus::Done(
                format!(
                    "Opened {}'s new pull request page (sign in under Settings → Accounts to create it here)",
                    target.repo.provider.name()
                )
                .into(),
            ));
            cx.notify();
            return;
        };
        // The tip's summary is the suggested title; read it off the UI thread.
        let repo = self.repo.clone();
        let tip = target.tip;
        let task = cx.background_executor().spawn(async move {
            switchhead_git::commit::info(&repo, tip).map(|i| i.summary().to_owned())
        });
        cx.spawn_in(window, async move |this, cx| {
            let title = task.await.unwrap_or_default();
            let _ = this.update_in(cx, |v, window, cx| {
                v.open_pull_request_dialog(target, account, title, window, cx)
            });
        })
        .detach();
    }

    fn open_pull_request_dialog(
        &mut self,
        target: Target,
        account: switchhead_hosting::Account,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let base = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Branch to merge into")
                .default_value(target.base.clone())
        });
        let title_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Title")
                .default_value(title)
        });
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 12)
                .placeholder("Description (optional)")
        });
        let draft = Rc::new(Cell::new(false));
        let error: Rc<std::cell::RefCell<Option<String>>> = Rc::default();
        let remote_branches: Rc<Vec<String>> = Rc::new(
            self.refs
                .remotes
                .iter()
                .filter(|r| r.remote == target.remote && !r.is_symbolic_head)
                .map(|r| r.name.clone())
                .collect(),
        );
        let target = Rc::new(target);
        let first = Cell::new(true);
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, window, cx| {
            if first.replace(false) {
                window.focus(&title_input.focus_handle(cx), cx);
            }
            let p = palette(cx);
            let kind = match target.repo.provider {
                switchhead_hosting::Provider::GitLab => "Merge Request",
                _ => "Pull Request",
            };
            let label = |text: &'static str| {
                div()
                    .flex_none()
                    .w(rpx(90.))
                    .text_size(ts::BODY)
                    .text_color(p.fg2)
                    .child(text)
            };
            let row = || div().flex().items_center().gap(rpx(10.));
            let flag = draft.clone();
            let (base, title_input, body, draft, error, target, view, remote_branches) = (
                base.clone(),
                title_input.clone(),
                body.clone(),
                draft.clone(),
                error.clone(),
                target.clone(),
                view.clone(),
                remote_branches.clone(),
            );
            let shown_error = error.borrow().clone();
            alert
                .confirm()
                .title(format!("Create {kind}"))
                .description(format!(
                    "From {} into a branch of {}, as {}.",
                    target.head,
                    target.repo.web_url(),
                    account.label()
                ))
                .ok_text(format!("Create {kind}"))
                .when(target.unpushed > 0, |a| {
                    a.child(
                        div()
                            .text_size(ts::SMALL)
                            .text_color(p.modified)
                            .child(match target.unpushed {
                                1 => "1 commit on this branch is not pushed yet and will not be in it.".to_owned(),
                                n => format!("{n} commits on this branch are not pushed yet and will not be in it."),
                            }),
                    )
                })
                .child(
                    row()
                        .child(label("Into"))
                        .child(div().flex_1().min_w_0().child(Input::new(&base).text_size(ts::BODY))),
                )
                .child(
                    row()
                        .child(label("Title"))
                        .child(div().flex_1().min_w_0().child(Input::new(&title_input).text_size(ts::BODY))),
                )
                .child(Textarea::new(&body).text_size(ts::BODY))
                .child(
                    ui::checkbox("pr-draft", flag.get(), "Open as a draft", &p)
                        .cursor_pointer()
                        .on_click(move |_, window, _| {
                            flag.set(!flag.get());
                            window.refresh();
                        }),
                )
                .when_some(shown_error, |a, e| {
                    a.child(div().text_size(ts::SMALL).text_color(p.deleted).child(e))
                })
                .on_ok({
                    let account = account.clone();
                    move |_, window, cx: &mut App| {
                        let pr = NewPullRequest {
                            head: target.head.clone(),
                            base: base.read(cx).value().trim().to_owned(),
                            title: title_input.read(cx).value().trim().to_owned(),
                            body: body.read(cx).value().trim().to_owned(),
                            draft: draft.get(),
                        };
                        let problem = if pr.title.is_empty() {
                            Some("Enter a title".to_owned())
                        } else if pr.base.is_empty() || pr.base == pr.head {
                            Some("Choose another branch to merge into".to_owned())
                        } else if !remote_branches.contains(&pr.base) {
                            Some(format!("{}/{} does not exist (fetch first?)", target.remote, pr.base))
                        } else {
                            None
                        };
                        if let Some(problem) = problem {
                            *error.borrow_mut() = Some(problem);
                            window.refresh();
                            return false;
                        }
                        let (account, repo) = (account.clone(), target.repo.clone());
                        let _ = view.update(cx, |v, cx| v.submit_pull_request(account, repo, pr, cx));
                        true
                    }
                })
        });
    }

    fn submit_pull_request(
        &mut self,
        account: switchhead_hosting::Account,
        repo: RepoRef,
        pr: NewPullRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = accounts::client() else {
            return self.pr_failed("HTTPS is not available on this system".into(), cx);
        };
        // The thread hands the new page's address to the step after it.
        let created: Arc<Mutex<Option<String>>> = Arc::default();
        let out = created.clone();
        self.run_op_then(
            format!("Creating pull request on {}", repo.provider.name()),
            "Pull request created",
            move |_, _, on_line| {
                let fail = |e: switchhead_hosting::Error| {
                    switchhead_git::Error::Unsupported(e.to_string())
                };
                let token = accounts::token(&account).map_err(fail)?;
                let url = client
                    .create_pull_request(&account, &token, &repo, &pr)
                    .map_err(fail)?;
                on_line(&url);
                if let Ok(mut o) = out.lock() {
                    *o = Some(url);
                }
                Ok(())
            },
            move |_, cx| {
                if let Some(url) = created.lock().ok().and_then(|mut o| o.take()) {
                    cx.open_url(&url);
                }
            },
            cx,
        );
    }
}
