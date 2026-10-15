//! The Clone form's "From an account" picker: the signed-in accounts' repositories,
//! filtered as you type; picking one fills in its HTTPS or SSH URL.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, UniformListScrollHandle, Window, div, px, svg, uniform_list,
};
use switchhead_hosting::RemoteRepo;

use crate::accounts;
use crate::appearance::{rpx, ts};
use crate::theme::Palette;
use crate::ui;
use crate::workspace::Workspace;

/// One account's repositories.
pub enum Listing {
    Loading,
    Loaded(Rc<Vec<RemoteRepo>>),
    Failed(String),
}

/// The picker's state, inside the Clone form.
pub struct AccountPicker {
    /// The account shown (index into the accounts).
    account: usize,
    /// Fill in SSH URLs instead of HTTPS ones.
    ssh: bool,
    filter: Entity<InputState>,
    listings: HashMap<String, Listing>,
    scroll: UniformListScrollHandle,
    /// The repository picked (its full name).
    picked: Option<String>,
    _filter: Subscription,
    _tasks: Vec<Task<()>>,
}

impl AccountPicker {
    /// A picker on the first account, its list loading; `None` without accounts.
    pub fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Option<Self> {
        if accounts::accounts(cx).0.is_empty() {
            return None;
        }
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter repositories"));
        let sub = cx.subscribe(&filter, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut picker = Self {
            account: 0,
            ssh: false,
            filter,
            listings: HashMap::new(),
            scroll: UniformListScrollHandle::new(),
            picked: None,
            _filter: sub,
            _tasks: Vec::new(),
        };
        picker.load(cx);
        Some(picker)
    }

    /// Load the shown account's repositories (once).
    fn load(&mut self, cx: &mut Context<Workspace>) {
        let Some(account) = accounts::accounts(cx).0.get(self.account).cloned() else {
            return;
        };
        let key = account.key();
        if self.listings.contains_key(&key) {
            return;
        }
        let Some(client) = accounts::client() else {
            self.listings
                .insert(key, Listing::Failed("HTTPS is not available".into()));
            return;
        };
        self.listings.insert(key.clone(), Listing::Loading);
        let task = cx.background_executor().spawn(async move {
            let token = accounts::token(&account)?;
            client.repositories(&account, &token)
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(p) = ws.manager.clone.as_mut().and_then(|f| f.picker.as_mut()) {
                    p.listings.insert(
                        key,
                        match result {
                            Ok(repos) => Listing::Loaded(Rc::new(repos)),
                            Err(e) => Listing::Failed(e.to_string()),
                        },
                    );
                }
                cx.notify();
            });
        }));
    }
}

/// `repo` matches the filter's words (all of them, anywhere in its name or description).
fn matches(repo: &RemoteRepo, words: &[String]) -> bool {
    words.iter().all(|w| {
        repo.full_name.to_lowercase().contains(w) || repo.description.to_lowercase().contains(w)
    })
}

impl Workspace {
    fn picker(&mut self) -> Option<&mut AccountPicker> {
        self.manager.clone.as_mut()?.picker.as_mut()
    }

    fn pick_account(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(p) = self.picker() {
            p.account = i;
            p.picked = None;
            p.load(cx);
        }
        cx.notify();
    }

    fn pick_repo(&mut self, repo: &RemoteRepo, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.manager.clone else {
            return;
        };
        let Some(p) = &mut form.picker else { return };
        p.picked = Some(repo.full_name.clone());
        let url = if p.ssh && !repo.ssh_url.is_empty() {
            repo.ssh_url.clone()
        } else {
            repo.https_url.clone()
        };
        form.url.update(cx, |i, cx| i.set_value(url, window, cx));
        cx.notify();
    }

    fn set_ssh(&mut self, ssh: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.picker() else { return };
        p.ssh = ssh;
        let picked = p.picked.clone();
        let account = p.account;
        let listing =
            accounts::accounts(cx)
                .0
                .get(account)
                .and_then(|a| match p.listings.get(&a.key()) {
                    Some(Listing::Loaded(r)) => Some(r.clone()),
                    _ => None,
                });
        if let (Some(name), Some(repos)) = (picked, listing)
            && let Some(repo) = repos.iter().find(|r| r.full_name == name)
        {
            let repo = repo.clone();
            self.pick_repo(&repo, window, cx);
        }
        cx.notify();
    }

    /// The picker, above the URL field (`None` without accounts).
    pub(super) fn render_account_repos(
        &self,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.manager.clone.as_ref()?.picker.as_ref()?;
        let all = accounts::accounts(cx).0;
        let account = all.get(picker.account)?;
        let tabs = all
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let ws = cx.entity().downgrade();
                let on_click: ui::OnClick = Box::new(move |_, _, cx| {
                    let _ = ws.update(cx, |ws, cx| ws.pick_account(i, cx));
                });
                (
                    SharedString::from(format!("{} · {}", a.login, a.provider.name())),
                    i == picker.account,
                    on_click,
                )
            })
            .collect();
        let protocols = [("HTTPS", false), ("SSH", true)]
            .into_iter()
            .map(|(label, ssh)| {
                let ws = cx.entity().downgrade();
                let on_click: ui::OnClick = Box::new(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| ws.set_ssh(ssh, window, cx));
                });
                (SharedString::from(label), ssh == picker.ssh, on_click)
            })
            .collect();
        let words: Vec<String> = picker
            .filter
            .read(cx)
            .value()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let body: AnyElement = match picker.listings.get(&account.key()) {
            None | Some(Listing::Loading) => div()
                .flex()
                .items_center()
                .gap(rpx(8.))
                .h(rpx(60.))
                .text_size(ts::BODY)
                .text_color(p.fg2)
                .child(ui::pulse_dot("repos-loading", p.acc, 6.))
                .child("Loading repositories…")
                .into_any_element(),
            Some(Listing::Failed(e)) => div()
                .h(rpx(60.))
                .text_size(ts::BODY)
                .text_color(p.deleted)
                .child(e.clone())
                .into_any_element(),
            Some(Listing::Loaded(repos)) => {
                let shown: Rc<Vec<usize>> = Rc::new(
                    repos
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| matches(r, &words))
                        .map(|(i, _)| i)
                        .collect(),
                );
                if shown.is_empty() {
                    div()
                        .h(rpx(60.))
                        .text_size(ts::BODY)
                        .text_color(p.fg3)
                        .child(if repos.is_empty() {
                            "This account has no repositories."
                        } else {
                            "No repository matches."
                        })
                        .into_any_element()
                } else {
                    let repos = repos.clone();
                    let picked = picker.picked.clone();
                    // Up to seven rows tall; shorter lists take only their rows.
                    let height = (shown.len().min(7) as f32 * 30.).max(30.);
                    uniform_list(
                        "account-repos",
                        shown.len(),
                        cx.processor(move |_, range: std::ops::Range<usize>, _w, cx| {
                            let p = crate::theme::palette(cx);
                            range
                                .filter_map(|row| shown.get(row).map(|i| (row, &repos[*i])))
                                .map(|(row, r)| {
                                    let on = picked.as_deref() == Some(r.full_name.as_str());
                                    let repo = r.clone();
                                    div()
                                        .id(("account-repo", row))
                                        .w_full()
                                        .h(rpx(30.))
                                        .flex()
                                        .items_center()
                                        .gap(rpx(8.))
                                        .px(rpx(8.))
                                        .rounded(px(5.))
                                        .cursor_pointer()
                                        .when(on, |d| d.bg(p.sel))
                                        .hover(|s| s.bg(p.hover))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.pick_repo(&repo, window, cx)
                                        }))
                                        .child(
                                            svg()
                                                .path(if r.private {
                                                    "icons/sh-lock.svg"
                                                } else {
                                                    "icons/sh-git-branch.svg"
                                                })
                                                .flex_none()
                                                .size(rpx(12.))
                                                .text_color(p.fg3),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .max_w(rpx(320.))
                                                .truncate()
                                                .text_size(ts::BODY)
                                                .child(r.full_name.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(ts::SMALL)
                                                .text_color(p.fg3)
                                                .child(r.description.clone()),
                                        )
                                })
                                .collect()
                        }),
                    )
                    .track_scroll(&picker.scroll)
                    .h(rpx(height))
                    .into_any_element()
                }
            }
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap(rpx(6.))
                .child(ui::caption("FROM AN ACCOUNT", p))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(8.))
                        .child(div().flex_1().min_w_0().child(ui::segmented(
                            "clone-account",
                            tabs,
                            22.,
                            p,
                        )))
                        .child(ui::segmented("clone-protocol", protocols, 22., p)),
                )
                .child(Input::new(&picker.filter).text_size(ts::BODY))
                .child(body)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_by_every_word() {
        let r = RemoteRepo {
            full_name: "jeremie/switchhead".into(),
            description: "A fast Git client".into(),
            private: false,
            https_url: String::new(),
            ssh_url: String::new(),
        };
        assert!(matches(&r, &["switch".into(), "git".into()]));
        assert!(!matches(&r, &["switch".into(), "yard".into()]));
        assert!(matches(&r, &[]));
    }
}
