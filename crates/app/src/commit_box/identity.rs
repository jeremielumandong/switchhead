//! The commit box's identity line: who the next commit is from, and a menu to pick a
//! saved identity for this repository (written to its local config), go back to the
//! global one, or save a new one.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, BorrowAppContext as _, Context, Entity, IntoElement, ParentElement as _,
    Styled as _, Window, div,
};
use switchhead_git::identity::Current;
use switchhead_git::{Repo, Result};

use super::CommitBox;
use crate::appearance::{rpx, ts};
use crate::store::{Identities, IdentityProfile};
use crate::theme::palette;

impl CommitBox {
    /// Read the identity in effect (in the background).
    pub(crate) fn load_identity(&mut self, cx: &mut Context<Self>) {
        self.identity_op(|_| Ok(()), cx);
    }

    /// Run `f` on the repository, then read the identity again.
    fn identity_op(
        &mut self,
        f: impl FnOnce(&Repo) -> Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move {
            f(&repo)?;
            repo.current_identity()
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(c) => this.identity = Some(c),
                    Err(e) => this.error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn use_profile(&mut self, p: IdentityProfile, cx: &mut Context<Self>) {
        let id = p.to_git();
        self.identity_op(move |r| r.set_identity(&id), cx);
    }

    /// The line above the subject: "Jane Doe <jane@example.com>" and where it is set.
    pub(super) fn render_identity(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let cur = self.identity.clone();
        let (text, color, scope) = match &cur {
            None => (String::new(), p.fg3, None),
            Some(c) => match (&c.name, &c.email) {
                (Some(n), Some(e)) => (
                    format!("{} <{}>", n.value, e.value),
                    p.fg2,
                    Some(if c.is_local() { "this repo" } else { "global" }),
                ),
                _ => (
                    "No name or email set: choose an identity".to_owned(),
                    p.deleted,
                    None,
                ),
            },
        };
        let signed = cur.as_ref().is_some_and(|c| c.signs_commits);
        let this = cx.entity().downgrade();
        div()
            .flex()
            .items_center()
            .gap(rpx(6.))
            .min_w_0()
            .child(
                Button::new("identity")
                    .ghost()
                    .xsmall()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(ts::SMALL)
                            .text_color(color)
                            .child(text),
                    )
                    .dropdown_menu(move |mut menu, window, cx| {
                        let saved = cx.global::<Identities>().0.clone();
                        let cur = cur.clone();
                        for prof in saved {
                            let checked = cur.as_ref().is_some_and(|c| c.matches(&prof.to_git()));
                            let label = format!("{} <{}>", prof.name, prof.email);
                            let this = this.clone();
                            menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                                move |_, _, cx| {
                                    let prof = prof.clone();
                                    let _ = this.update(cx, |b, cx| b.use_profile(prof, cx));
                                },
                            ));
                        }
                        let local = cur.as_ref().is_some_and(Current::is_local);
                        let saveable = cur
                            .as_ref()
                            .and_then(profile_of)
                            .filter(|p| !cx.global::<Identities>().0.contains(p));
                        let (t1, t2, t3) = (this.clone(), this.clone(), this.clone());
                        menu = menu
                            .separator()
                            .item(
                                PopupMenuItem::new("Use Global Identity")
                                    .disabled(!local)
                                    .on_click(move |_, _, cx| {
                                        let _ = t1.update(cx, |b, cx| {
                                            b.identity_op(|r| r.clear_identity(), cx)
                                        });
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Save Current Identity")
                                    .disabled(saveable.is_none())
                                    .on_click(move |_, _, cx| {
                                        if let Some(p) = saveable.clone() {
                                            cx.update_global::<Identities, _>(|ids, _| ids.save(p));
                                        }
                                        let _ = t2.update(cx, |_, cx| cx.notify());
                                    }),
                            )
                            .item(PopupMenuItem::new("New Identity…").on_click(
                                move |_, window, cx| {
                                    let _ =
                                        t3.update(cx, |b, cx| b.new_identity_dialog(window, cx));
                                },
                            ));
                        menu = menu.item(PopupMenuItem::new("Commit Signing…").on_click(
                            |_, window, cx| {
                                window
                                    .dispatch_action(Box::new(crate::actions::SigningSettings), cx)
                            },
                        ));
                        let saved = cx.global::<Identities>().0.clone();
                        if !saved.is_empty() {
                            menu = menu.submenu(
                                "Remove Saved Identity",
                                window,
                                cx,
                                move |mut m, _, _| {
                                    for prof in &saved {
                                        let email = prof.email.clone();
                                        m = m.item(
                                            PopupMenuItem::new(format!(
                                                "{} <{}>",
                                                prof.name, prof.email
                                            ))
                                            .on_click(move |_, _, cx| {
                                                let email = email.clone();
                                                cx.update_global::<Identities, _>(|ids, _| {
                                                    ids.0.retain(|p| p.email != email)
                                                });
                                            }),
                                        );
                                    }
                                    m
                                },
                            );
                        }
                        menu
                    }),
            )
            .when_some(scope, |d, s| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(ts::CAPTION)
                        .text_color(p.fg3)
                        .child(if signed {
                            format!("{s} · signed")
                        } else {
                            s.to_owned()
                        }),
                )
            })
    }

    /// Ask for a name, email and signing key; save them and use them here.
    fn new_identity_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cur = self
            .identity
            .as_ref()
            .and_then(profile_of)
            .unwrap_or_default();
        let field = |placeholder: &'static str,
                     value: String,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                s.set_value(value, window, cx);
                s
            })
        };
        let name = field("Name", cur.name, window, cx);
        let email = field("Email", cur.email, window, cx);
        let key = field(
            "Signing key (optional): GPG key id or SSH key",
            cur.signing_key.unwrap_or_default(),
            window,
            cx,
        );
        let error: Rc<RefCell<Option<&'static str>>> = Rc::default();
        let this = cx.entity().downgrade();
        let first = std::cell::Cell::new(true);
        window.open_alert_dialog(cx, move |alert, window, cx| {
            if first.replace(false) {
                name.update(cx, |s, cx| s.focus(window, cx));
            }
            let p = palette(cx);
            let (name, email, key, error, this) = (
                name.clone(),
                email.clone(),
                key.clone(),
                error.clone(),
                this.clone(),
            );
            let shown = *error.borrow();
            let row = |label: &'static str, input: &Entity<InputState>| {
                div()
                    .flex()
                    .flex_col()
                    .gap(rpx(3.))
                    .child(div().text_size(ts::SMALL).text_color(p.fg2).child(label))
                    .child(Input::new(input).text_size(ts::BODY))
            };
            alert
                .confirm()
                .title("New Identity")
                .description(
                    "Saved for every repository, and used for commits in this one \
                     (written to its local git config).",
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rpx(8.))
                        .child(row("Name", &name))
                        .child(row("Email", &email))
                        .child(row("Signing key", &key))
                        .when_some(shown, |d, e| {
                            d.child(div().text_size(ts::SMALL).text_color(p.deleted).child(e))
                        }),
                )
                .ok_text("Save and Use")
                .on_ok(move |_, window, cx| {
                    let get = |i: &Entity<InputState>, cx: &gpui_kit::App| {
                        i.read(cx).value().trim().to_owned()
                    };
                    let prof = IdentityProfile {
                        name: get(&name, cx),
                        email: get(&email, cx),
                        signing_key: Some(get(&key, cx)).filter(|k| !k.is_empty()),
                    };
                    let problem = if prof.name.is_empty() {
                        Some("Enter a name")
                    } else if !prof.email.contains('@') {
                        Some("Enter an email address")
                    } else {
                        None
                    };
                    if problem.is_some() {
                        *error.borrow_mut() = problem;
                        window.refresh();
                        return false;
                    }
                    cx.update_global::<Identities, _>(|ids, _| ids.save(prof.clone()));
                    let _ = this.update(cx, |b, cx| b.use_profile(prof, cx));
                    true
                })
        });
    }
}

/// The current identity as a profile, when it has a name and email.
fn profile_of(c: &Current) -> Option<IdentityProfile> {
    Some(IdentityProfile {
        name: c.name.as_ref()?.value.clone(),
        email: c.email.as_ref()?.value.clone(),
        signing_key: c.signing_key.as_ref().map(|k| k.value.clone()),
    })
}
