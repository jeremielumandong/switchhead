//! The commit box under the staged files: subject and body, Amend (loads the last
//! message), Sign-off and Skip hooks, the repository's commit template, recent messages
//! and a subject length counter. Committing runs `git commit` on the background executor
//! ([`Repo::commit`]) and emits [`CommitEvent::Committed`].

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, BorrowAppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use switchhead_git::Repo;
use switchhead_git::ops::CommitOptions;

use switchhead_git::identity::Current;

use crate::appearance::{rpx, ts};
use crate::store::RecentMessages;
use crate::theme::{MONO, Palette, palette};
use crate::ui;

mod identity;

/// Subject length git tooling suggests.
pub const SUBJECT_SOFT: usize = 50;
/// Subject length past which most tools truncate.
pub const SUBJECT_HARD: usize = 72;

/// What the commit box reports to the Local Changes view.
pub enum CommitEvent {
    /// A commit was made (or amended): status and history changed.
    Committed,
}

/// The commit message editor and its options.
pub struct CommitBox {
    repo: Repo,
    summary: Entity<InputState>,
    body: Entity<TextareaState>,
    amend: bool,
    signoff: bool,
    no_verify: bool,
    /// What the fields held before Amend loaded the last message (restored when Amend
    /// is turned off).
    draft: Option<(String, String)>,
    /// The repository's `commit.template`, put in empty fields.
    template: Option<String>,
    /// Files staged (set by the Local Changes view).
    staged: usize,
    busy: bool,
    error: Option<SharedString>,
    /// Who commits (`None` until read).
    identity: Option<Current>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<CommitEvent> for CommitBox {}

impl CommitBox {
    /// An empty box for `repo`; the commit template is read in the background.
    pub fn new(repo: Repo, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let summary = cx.new(|cx| InputState::new(window, cx).placeholder("Commit subject"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 12)
                .placeholder("Description")
        });
        let subs = vec![
            cx.subscribe_in(&summary, window, |this, _, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.commit(window, cx),
                    // Enter in the subject goes on to the description, as in an editor.
                    InputEvent::PressEnter { .. } => {
                        this.body.update(cx, |b, cx| b.focus(window, cx))
                    }
                    InputEvent::Change => {
                        this.error = None;
                        cx.notify();
                    }
                    _ => {}
                }
            }),
            cx.subscribe_in(
                &body,
                window,
                |this, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.commit(window, cx),
                    InputEvent::Change => {
                        this.error = None;
                        cx.notify();
                    }
                    _ => {}
                },
            ),
        ];
        let mut this = Self {
            repo,
            summary,
            body,
            amend: false,
            signoff: false,
            no_verify: false,
            draft: None,
            template: None,
            staged: 0,
            busy: false,
            error: None,
            identity: None,
            _subs: subs,
        };
        this.load_template(window, cx);
        this.load_identity(cx);
        this
    }

    /// Tell the box how many files are staged (it enables Commit).
    pub fn set_staged(&mut self, staged: usize, cx: &mut Context<Self>) {
        if self.staged != staged {
            self.staged = staged;
            cx.notify();
        }
    }

    /// Put the cursor in the subject.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.summary.update(cx, |s, cx| s.focus(window, cx));
    }

    fn message(&self, cx: &Context<Self>) -> String {
        compose(&self.summary.read(cx).value(), &self.body.read(cx).value())
    }

    fn set_message(&self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (s, b) = split(message);
        self.summary
            .update(cx, |i, cx| i.set_value(s.to_owned(), window, cx));
        self.body
            .update(cx, |i, cx| i.set_value(b.to_owned(), window, cx));
    }

    fn load_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        let task = cx
            .background_executor()
            .spawn(async move { repo.commit_template() });
        cx.spawn_in(window, async move |this, cx| {
            let t = task.await;
            let _ = this.update_in(cx, |this, window, cx| match t {
                Ok(t) => {
                    this.template = t;
                    this.fill_template(window, cx);
                }
                Err(e) => tracing::warn!(error = %e, "could not read commit.template"),
            });
        })
        .detach();
    }

    /// Put the template in the fields if they are empty.
    fn fill_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.template.clone()
            && is_blank(&self.message(cx))
        {
            self.set_message(&t, window, cx);
        }
    }

    fn toggle_amend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.error = None;
        if self.amend {
            self.amend = false;
            let (s, b) = self.draft.take().unwrap_or_default();
            self.set_message(&compose(&s, &b), window, cx);
            cx.notify();
            return;
        }
        self.amend = true;
        self.draft = Some((
            self.summary.read(cx).value().to_string(),
            self.body.read(cx).value().to_string(),
        ));
        let repo = self.repo.clone();
        let task = cx
            .background_executor()
            .spawn(async move { repo.last_message() });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.amend {
                    return;
                }
                match res {
                    Ok(m) => this.set_message(&m, window, cx),
                    Err(_) => {
                        this.amend = false;
                        this.draft = None;
                        this.error = Some("There is no commit to amend yet.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Whether the message says something: not blank, and not the template untouched.
    fn has_message(&self, cx: &Context<Self>) -> bool {
        let m = clean(&self.message(cx));
        !m.is_empty() && self.template.as_deref().is_none_or(|t| clean(t) != m)
    }

    fn can_commit(&self, cx: &Context<Self>) -> bool {
        !self.busy && (self.staged > 0 || self.amend) && self.has_message(cx)
    }

    /// Commit the staged files with the message in the box.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_commit(cx) {
            if !self.busy && !self.has_message(cx) {
                self.error = Some("Write a commit message first.".into());
                cx.notify();
            }
            return;
        }
        let message = self.message(cx);
        let opts = CommitOptions {
            amend: self.amend,
            signoff: self.signoff,
            no_verify: self.no_verify,
            allow_empty: false,
        };
        self.busy = true;
        self.error = None;
        let repo = self.repo.clone();
        let msg = message.clone();
        let task = cx
            .background_executor()
            .spawn(async move { repo.commit(&msg, opts) });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match res {
                    Ok(()) => {
                        cx.update_global::<RecentMessages, _>(|m, _| m.push(&clean(&message)));
                        this.amend = false;
                        this.draft = None;
                        this.set_message("", window, cx);
                        this.fill_template(window, cx);
                        cx.emit(CommitEvent::Committed);
                    }
                    // Hooks print several lines; show them all.
                    Err(e) => this.error = Some(e.to_string().trim().to_owned().into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_recent(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has = !cx.global::<RecentMessages>().0.is_empty();
        let this = cx.entity().downgrade();
        Button::new("recent-messages")
            .ghost()
            .xsmall()
            .label("Recent")
            .disabled(!has)
            .dropdown_menu(move |mut menu, _, cx| {
                let recent: Vec<String> = cx.global::<RecentMessages>().0.clone();
                for m in recent {
                    let label = menu_label(&m);
                    let this = this.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        let _ = this.update(cx, |b, cx| {
                            b.set_message(&m, window, cx);
                            b.focus(window, cx);
                            cx.notify();
                        });
                    }));
                }
                menu
            })
    }
}

impl Render for CommitBox {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let len = self.summary.read(cx).value().chars().count();
        let can = self.can_commit(cx);
        let label = match (self.amend, self.staged) {
            (true, _) => "Amend Last Commit".to_owned(),
            (false, 0) => "Commit".to_owned(),
            (false, 1) => "Commit 1 File".to_owned(),
            (false, n) => format!("Commit {n} Files"),
        };
        let toggle = |id: &'static str, on: bool, text: &'static str, p: &Palette| {
            ui::checkbox(id, on, text, p)
                .text_size(ts::SMALL)
                .cursor_pointer()
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(rpx(6.))
            .p(rpx(10.))
            .bg(p.panel)
            .border_t_1()
            .border_color(p.bd)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(6.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(ts::LABEL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.fg)
                            .child(if self.amend { "Amend" } else { "Commit" }),
                    )
                    .child(self.render_recent(cx))
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(ts::CAPTION)
                            .text_color(counter_color(len, &p))
                            .child(len.to_string()),
                    ),
            )
            .child(self.render_identity(cx))
            .child(Input::new(&self.summary).small().text_size(ts::BODY))
            .child(Textarea::new(&self.body).small().text_size(ts::BODY))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(rpx(12.))
                    .gap_y(rpx(4.))
                    .child(
                        toggle("amend", self.amend, "Amend", &p).on_click(
                            cx.listener(|this, _, window, cx| this.toggle_amend(window, cx)),
                        ),
                    )
                    .child(
                        toggle("signoff", self.signoff, "Sign-off", &p).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.signoff = !this.signoff;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        toggle("no-verify", self.no_verify, "Skip hooks", &p).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.no_verify = !this.no_verify;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    div()
                        .id("commit-error")
                        .max_h(rpx(120.))
                        .overflow_y_scroll()
                        .px(rpx(8.))
                        .py(rpx(6.))
                        .rounded(px(4.))
                        .bg(p.deleted.opacity(0.12))
                        .font_family(MONO)
                        .text_size(ts::SMALL)
                        .text_color(p.deleted)
                        .child(e),
                )
            })
            .when(self.busy, |d| d.child(ui::loading_bar("committing", &p)))
            .child(
                ui::button_with_key(
                    "commit",
                    label,
                    ui::keys("⌘↵", "Ctrl+Enter"),
                    if can {
                        ui::Kind::Primary
                    } else {
                        ui::Kind::Secondary
                    },
                    &p,
                )
                .w_full()
                .when(!can, |d| d.opacity(0.6))
                .when(can, |d| d.cursor_pointer())
                .on_click(cx.listener(|this, _, window, cx| this.commit(window, cx))),
            )
    }
}

/// The message from its subject and description: `subject`, a blank line, `body`.
pub fn compose(subject: &str, body: &str) -> String {
    let subject = subject.trim();
    let body = body.trim_end();
    let body = body.trim_start_matches(['\n', '\r']);
    match (subject.is_empty(), body.is_empty()) {
        (_, true) => subject.to_owned(),
        (true, false) => body.to_owned(),
        (false, false) => format!("{subject}\n\n{body}"),
    }
}

/// Split a message into its subject (first line) and description (the rest, without
/// the blank lines after the subject).
pub fn split(message: &str) -> (&str, &str) {
    let message = message.trim_end();
    match message.split_once('\n') {
        Some((s, b)) => (s.trim_end_matches('\r'), b.trim_start_matches(['\n', '\r'])),
        None => (message, ""),
    }
}

/// `message` as git keeps it with `--cleanup=strip`: comment lines dropped, trailing
/// spaces and leading, trailing and repeated blank lines removed.
pub fn clean(message: &str) -> String {
    let mut out = String::new();
    let mut blank = false;
    for line in message.lines().filter(|l| !l.starts_with('#')) {
        let line = line.trim_end();
        if line.is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

/// Whether git would find nothing in `message` once comments are stripped.
pub fn is_blank(message: &str) -> bool {
    clean(message).is_empty()
}

fn counter_color(len: usize, p: &Palette) -> gpui_kit::Hsla {
    if len > SUBJECT_HARD {
        p.deleted
    } else if len > SUBJECT_SOFT {
        p.modified
    } else {
        p.fg3
    }
}

/// A recent message as a menu item: its subject, shortened.
fn menu_label(message: &str) -> String {
    let (s, _) = split(message);
    let mut out: String = s.chars().take(60).collect();
    if s.chars().count() > 60 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_and_splits_messages() {
        assert_eq!(compose("  Fix it ", ""), "Fix it");
        assert_eq!(compose("Fix it", "\nWhy.\n\n"), "Fix it\n\nWhy.");
        assert_eq!(compose("", "only body"), "only body");
        assert_eq!(
            split("Fix it\n\n\nWhy.\nMore.\n"),
            ("Fix it", "Why.\nMore.")
        );
        assert_eq!(split("One line"), ("One line", ""));
        assert_eq!(split("Subj\r\n\r\nBody"), ("Subj", "Body"));
        let (s, b) = split("Fix it\n\nWhy.");
        assert_eq!(compose(s, b), "Fix it\n\nWhy.");
    }

    #[test]
    fn cleans_like_git() {
        assert_eq!(clean("\n\nFix  \n# c\n\n\n\nWhy.\n\n"), "Fix\n\nWhy.");
        assert_eq!(clean("# only\n"), "");
    }

    #[test]
    fn comments_alone_are_blank() {
        assert!(is_blank(""));
        assert!(is_blank("\n# Why?\n  \n"));
        assert!(!is_blank("Fix\n# Why?"));
    }

    #[test]
    fn shortens_menu_labels() {
        assert_eq!(menu_label("Short\n\nbody"), "Short");
        let long = "x".repeat(80);
        assert_eq!(menu_label(&long).chars().count(), 61);
    }
}
