//! Settings → Accounts: sign in to GitHub, GitLab, Bitbucket or Azure DevOps with an
//! access token (checked against the service, then kept in the keychain), and sign out.

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, BorrowAppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, img, px,
};
use switchhead_hosting::{Account, Provider, Token, secrets};

use crate::accounts::{self, Accounts};
use crate::appearance::{rpx, ts};
use crate::theme::{Palette, palette};
use crate::ui::{self, Kind};

/// What the page asks of the workspace.
pub enum AccountsEvent {
    /// The close button.
    Close,
}

/// The page's last outcome.
enum Status {
    Idle,
    Signing,
    Error(String),
    Done(String),
}

/// The settings page.
pub struct AccountsView {
    provider: Provider,
    host: Entity<InputState>,
    organization: Entity<InputState>,
    user: Entity<InputState>,
    token: Entity<InputState>,
    status: Status,
    _task: Option<Task<()>>,
}

impl EventEmitter<AccountsEvent> for AccountsView {}

impl AccountsView {
    /// The page, with the GitHub form showing.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let provider = Provider::GitHub;
        let host = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://github.example.com")
                .default_value(provider.default_host())
        });
        let organization =
            cx.new(|cx| InputState::new(window, cx).placeholder("Organization, e.g. contoso"));
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("Atlassian account email"));
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Paste the token")
                .masked(true)
        });
        Self {
            provider,
            host,
            organization,
            user,
            token,
            status: Status::Idle,
            _task: None,
        }
    }

    fn set_provider(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        if provider == self.provider {
            return;
        }
        self.provider = provider;
        self.host
            .update(cx, |s, cx| s.set_value(provider.default_host(), window, cx));
        self.status = Status::Idle;
        cx.notify();
    }

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Signing) {
            return;
        }
        let Some(client) = accounts::client() else {
            self.status = Status::Error("HTTPS is not available on this system".into());
            cx.notify();
            return;
        };
        let provider = self.provider;
        let host = if provider.self_hosted() {
            self.host.read(cx).value().trim().to_owned()
        } else {
            provider.default_host().to_owned()
        };
        if !host.starts_with("https://") && !host.starts_with("http://") {
            self.status = Status::Error("The site's address starts with https://".into());
            cx.notify();
            return;
        }
        let organization = self.organization.read(cx).value().to_string();
        let user = self.user.read(cx).value().to_string();
        let token = Token::new(&self.token.read(cx).value());
        self.status = Status::Signing;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            let account = client.sign_in(provider, &host, &organization, &user, &token)?;
            let kept = secrets::store(&account.key(), &token)?;
            Ok::<_, switchhead_hosting::Error>((account, kept))
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.status = match result {
                    Ok((account, kept)) => {
                        let label = account.label();
                        cx.update_global::<Accounts, _>(|a, _| a.upsert(account));
                        // A token is now at hand: authors without an avatar may have one.
                        accounts::forget_author_lookups(cx);
                        this.token.update(cx, |s, cx| s.set_value("", window, cx));
                        Status::Done(if kept {
                            format!("Signed in as {label}.")
                        } else {
                            format!(
                                "Signed in as {label} for this session only: no keychain is \
                                 available, so the token is not saved."
                            )
                        })
                    }
                    Err(switchhead_hosting::Error::Unauthorized(_)) => Status::Error(
                        "The token was refused. Check that it is current and has the access listed above."
                            .into(),
                    ),
                    Err(e) => Status::Error(e.to_string()),
                };
                cx.notify();
            });
        }));
    }

    fn remove(&mut self, account: Account, cx: &mut Context<Self>) {
        let key = account.key();
        cx.update_global::<Accounts, _>(|a, _| a.0.retain(|x| x.key() != key));
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = secrets::delete(&key) {
                    tracing::warn!(error = %e, "could not remove the token");
                }
            })
            .detach();
        self.status = Status::Done(format!("Signed out {}.", account.label()));
        cx.notify();
    }

    fn render_account(
        &self,
        i: usize,
        a: &Account,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let picture = a
            .avatar_url
            .as_deref()
            .and_then(|u| accounts::avatar(u, cx));
        let initial: String = a
            .login
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_default();
        let account = a.clone();
        div()
            .id(("account", i))
            .flex()
            .items_center()
            .gap(rpx(10.))
            .px(rpx(10.))
            .py(rpx(8.))
            .rounded(px(6.))
            .bg(p.panel)
            .border_1()
            .border_color(p.bd)
            .child(match picture {
                Some(path) => img(path).size(rpx(28.)).rounded_full().into_any_element(),
                None => div()
                    .size(rpx(28.))
                    .flex_none()
                    .rounded_full()
                    .bg(p.sel)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(ts::UI)
                    .child(initial)
                    .into_any_element(),
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_size(ts::UI).truncate().child(a.label()))
                    .when(!a.name.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(ts::SMALL)
                                .text_color(p.fg3)
                                .truncate()
                                .child(a.name.clone()),
                        )
                    }),
            )
            .child(
                ui::button(("account-remove", i), "Sign Out", Kind::Secondary, p)
                    .on_click(cx.listener(move |this, _, _, cx| this.remove(account.clone(), cx))),
            )
            .into_any_element()
    }
}

impl Render for AccountsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let list = accounts::accounts(cx).0;
        let provider = self.provider;
        let host_text = self.host.read(cx).value().to_string();
        let (token_url, token_hint) = provider.token_help(if provider.self_hosted() {
            &host_text
        } else {
            provider.default_host()
        });
        let providers = Provider::ALL
            .into_iter()
            .map(|pr| {
                let view = cx.entity().downgrade();
                let on_click: ui::OnClick = Box::new(move |_, window, cx| {
                    let _ = view.update(cx, |v, cx| v.set_provider(pr, window, cx));
                });
                (SharedString::from(pr.name()), pr == provider, on_click)
            })
            .collect();
        let field = |label: &'static str, input: &Entity<InputState>| {
            div()
                .flex()
                .items_center()
                .gap(rpx(10.))
                .child(
                    div()
                        .flex_none()
                        .w(rpx(110.))
                        .text_size(ts::BODY)
                        .text_color(p.fg2)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(input).text_size(ts::BODY)),
                )
        };
        let signing = matches!(self.status, Status::Signing);
        let rows: Vec<AnyElement> = list
            .iter()
            .enumerate()
            .map(|(i, a)| self.render_account(i, a, &p, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(rpx(14.))
            .p(rpx(18.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(ts::TITLE)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Accounts"),
                    )
                    .child(
                        ui::button("accounts-close", "Close", Kind::Secondary, &p)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::Close))),
                    ),
            )
            .child(div().text_size(ts::BODY).text_color(p.fg3).child(
                "Accounts list your repositories to clone, open pull requests and show \
                         avatars. Tokens are kept in the system keychain. Pushing and pulling \
                         still use git's own credentials.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rpx(6.))
                    .children(rows)
                    .when(list.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(ts::BODY)
                                .text_color(p.fg3)
                                .child("No accounts yet."),
                        )
                    }),
            )
            .child(div().h(rpx(1.)).bg(p.bd))
            .child(ui::caption("ADD AN ACCOUNT", &p))
            .child(ui::segmented("account-provider", providers, 24., &p))
            .when(provider.self_hosted(), |d| {
                d.child(field("Site", &self.host))
            })
            .when(provider.needs_organization(), |d| {
                d.child(field("Organization", &self.organization))
            })
            .when(provider.needs_user(), |d| {
                d.child(field("Email", &self.user))
            })
            .child(field("Token", &self.token))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(8.))
                    .text_size(ts::SMALL)
                    .text_color(p.fg3)
                    .child(div().flex_1().min_w_0().child(token_hint))
                    .child(
                        ui::button("account-token-page", "Create a Token…", Kind::Ghost, &p)
                            .on_click(move |_, _, cx| cx.open_url(&token_url)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(10.))
                    .child(
                        ui::button("account-sign-in", "Sign In", Kind::Primary, &p)
                            .on_click(cx.listener(|this, _, w, cx| this.sign_in(w, cx))),
                    )
                    .when(signing, |d| {
                        d.child(ui::pulse_dot("account-signing", p.acc, 6.)).child(
                            div()
                                .text_size(ts::BODY)
                                .text_color(p.fg2)
                                .child("Checking the token…"),
                        )
                    })
                    .map(|d| match &self.status {
                        Status::Error(e) => d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(ts::BODY)
                                .text_color(p.deleted)
                                .child(e.clone()),
                        ),
                        Status::Done(m) => d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(ts::BODY)
                                .text_color(p.added)
                                .child(m.clone()),
                        ),
                        Status::Idle | Status::Signing => d,
                    }),
            )
    }
}
