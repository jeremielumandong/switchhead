//! Commit Signing settings (the palette, the toolbar's Commands menu, the commit box's
//! identity menu): GPG, SSH or X.509, the key, the program, SSH's allowed signers file,
//! signing commits and tags, for this repository or all of them, with a test signature.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use switchhead_git::signing::{Format, Settings, Verdict, Verification};

use super::{OpStatus, RepoView};
use crate::appearance::{rpx, ts};
use crate::theme::palette;
use crate::ui::{self, Kind};

/// The test signature's state.
enum Test {
    Idle,
    Running,
    Done(Result<Verification, String>),
}

/// The key field's hint for each format.
fn key_placeholder(f: Format) -> &'static str {
    match f {
        Format::OpenPgp => "Key id, e.g. 3AA5C34371567BD2 (empty: the one for your email)",
        Format::Ssh => "Public key file (.pub), or key::ssh-ed25519 AAAA… for an agent key",
        Format::X509 => "Certificate id (empty: the one for your email)",
    }
}

fn program_placeholder(f: Format) -> &'static str {
    match f {
        Format::OpenPgp => "gpg (empty: found on PATH)",
        Format::Ssh => "ssh-keygen (empty: found on PATH)",
        Format::X509 => "gpgsm (empty: found on PATH)",
    }
}

/// What a test signature means for the user, and whether it is good news.
pub(crate) fn test_outcome(v: &Verification) -> (bool, String) {
    match v.verdict {
        Verdict::Good => (
            true,
            match v.signer.as_str() {
                "" => "Signing works, and the signature verifies.".to_owned(),
                s => format!("Signing works, and the signature verifies as {s}."),
            },
        ),
        Verdict::Untrusted if v.format == Format::Ssh => (
            true,
            "Signing works. The key is not in the allowed signers file, so your commits \
             will show as unverified here."
                .to_owned(),
        ),
        Verdict::Untrusted => (
            true,
            "Signing works. The key is not trusted in GnuPG, so your commits will show as \
             unverified here."
                .to_owned(),
        ),
        Verdict::CannotCheck => (
            true,
            match v.format {
                Format::Ssh => "Signing works. Add an allowed signers file to verify SSH \
                                signatures."
                    .to_owned(),
                _ => format!(
                    "Signing works, but the signature could not be checked: {}",
                    { v.details.lines().next().unwrap_or_default() }
                ),
            },
        ),
        Verdict::Expired | Verdict::ExpiredKey => (false, "The key has expired.".to_owned()),
        Verdict::Revoked => (false, "The key has been revoked.".to_owned()),
        Verdict::Bad => (false, "The signature does not verify.".to_owned()),
    }
}

/// Why the settings cannot be saved.
fn settings_problem(s: &Settings) -> Option<&'static str> {
    (s.format == Format::Ssh && s.key.trim().is_empty() && (s.sign_commits || s.sign_tags))
        .then_some("Enter the SSH key to sign with")
}

impl RepoView {
    /// Read the signing settings (in the background), then show them.
    pub fn signing_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        let task = cx
            .background_executor()
            .spawn(async move { repo.signing_settings() });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            let _ = this.update_in(cx, |v, window, cx| match res {
                Ok(s) => v.open_signing_dialog(s, window, cx),
                Err(e) => {
                    v.op = Some(OpStatus::Failed(format!("Commit Signing: {e}").into()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open_signing_dialog(&mut self, s: Settings, window: &mut Window, cx: &mut Context<Self>) {
        let mut field = |value: &str, placeholder: &'static str| {
            let value = value.to_owned();
            cx.new(|cx| {
                let mut st = InputState::new(window, cx).placeholder(placeholder);
                st.set_value(value, window, cx);
                st
            })
        };
        let key = field(&s.key, key_placeholder(s.format));
        let programs: Rc<[Entity<InputState>; 3]> =
            Rc::new(Format::ALL.map(|f| field(&s.programs[f as usize], program_placeholder(f))));
        let allowed = field(
            &s.allowed_signers,
            "File listing who may sign, to verify SSH signatures",
        );
        let format = Rc::new(Cell::new(s.format));
        let sign_commits = Rc::new(Cell::new(s.sign_commits));
        let sign_tags = Rc::new(Cell::new(s.sign_tags));
        // Settings set here edit this repository's; otherwise the user's.
        let everywhere = Rc::new(Cell::new(!s.local));
        let test = Rc::new(RefCell::new(Test::Idle));
        let error: Rc<RefCell<Option<&'static str>>> = Rc::default();
        let repo = self.repo.clone();
        let first = Cell::new(true);
        let view = cx.entity().downgrade();
        // The dialog's current settings.
        let read = {
            let (key, programs, allowed, format, sign_commits, sign_tags) = (
                key.clone(),
                programs.clone(),
                allowed.clone(),
                format.clone(),
                sign_commits.clone(),
                sign_tags.clone(),
            );
            Rc::new(move |cx: &App| {
                let get = |i: &Entity<InputState>| i.read(cx).value().trim().to_owned();
                Settings {
                    format: format.get(),
                    key: get(&key),
                    programs: [get(&programs[0]), get(&programs[1]), get(&programs[2])],
                    allowed_signers: get(&allowed),
                    sign_commits: sign_commits.get(),
                    sign_tags: sign_tags.get(),
                    local: false,
                }
            })
        };
        window.open_alert_dialog(cx, move |alert, window, cx| {
            if first.replace(false) {
                window.focus(&key.focus_handle(cx), cx);
            }
            let p = palette(cx);
            let f = format.get();
            let label = |text: &'static str| {
                div()
                    .flex_none()
                    .w(rpx(110.))
                    .text_size(ts::BODY)
                    .text_color(p.fg2)
                    .child(text)
            };
            let row = |text: &'static str, input: &Entity<InputState>| {
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(10.))
                    .child(label(text))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(input).text_size(ts::BODY)),
                    )
            };
            let formats = Format::ALL
                .into_iter()
                .map(|x| {
                    let (format, key) = (format.clone(), key.clone());
                    let on_click: ui::OnClick = Box::new(move |_, window, cx| {
                        format.set(x);
                        key.update(cx, |k, cx| {
                            k.set_placeholder(key_placeholder(x), window, cx)
                        });
                        window.refresh();
                    });
                    (SharedString::from(x.name()), x == f, on_click)
                })
                .collect();
            let scopes = [("This Repository", false), ("All Repositories", true)]
                .into_iter()
                .map(|(text, all)| {
                    let on = everywhere.get() == all;
                    let everywhere = everywhere.clone();
                    let on_click: ui::OnClick = Box::new(move |_, window, _| {
                        everywhere.set(all);
                        window.refresh();
                    });
                    (SharedString::from(text), on, on_click)
                })
                .collect();
            let check = |id: &'static str, text: &'static str, flag: &Rc<Cell<bool>>| {
                let flag = flag.clone();
                ui::checkbox(id, flag.get(), text, &p)
                    .cursor_pointer()
                    .on_click(move |_, window, _| {
                        flag.set(!flag.get());
                        window.refresh();
                    })
            };
            let test_line = match &*test.borrow() {
                Test::Idle => None,
                Test::Running => Some((p.fg2, "Signing a test commit…".to_owned())),
                Test::Done(Ok(v)) => {
                    let (good, text) = test_outcome(v);
                    Some((if good { p.added } else { p.deleted }, text))
                }
                Test::Done(Err(e)) => Some((p.deleted, e.clone())),
            };
            let run_test = {
                let (test, read, repo) = (test.clone(), read.clone(), repo.clone());
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    if matches!(*test.borrow(), Test::Running) {
                        return;
                    }
                    *test.borrow_mut() = Test::Running;
                    window.refresh();
                    let s = read(cx);
                    let repo = repo.clone();
                    let task = cx
                        .background_executor()
                        .spawn(async move { repo.test_signing(&s).map_err(|e| e.to_string()) });
                    let test = test.clone();
                    window
                        .spawn(cx, async move |cx| {
                            let res = task.await;
                            *test.borrow_mut() = Test::Done(res.map_err(|e| clean_error(&e)));
                            let _ = cx.update(|window, _| window.refresh());
                        })
                        .detach();
                }
            };
            let shown_error = *error.borrow();
            let (read, error, view, everywhere) = (
                read.clone(),
                error.clone(),
                view.clone(),
                everywhere.clone(),
            );
            alert
                .confirm()
                .title("Commit Signing")
                .description(
                    "Signed commits and tags show who made them. These are git's own \
                     settings, so the command line signs the same way.",
                )
                .ok_text("Save")
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(10.))
                        .child(label("Sign with"))
                        .child(ui::segmented("signing-format", formats, 22., &p)),
                )
                .child(row("Key", &key))
                .child(row("Program", &programs[f as usize]))
                .when(f == Format::Ssh, |a| {
                    a.child(row("Allowed signers", &allowed))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(16.))
                        .child(check("sign-commits", "Sign every commit", &sign_commits))
                        .child(check("sign-tags", "Sign annotated tags", &sign_tags)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(10.))
                        .child(label("Save for"))
                        .child(ui::segmented("signing-scope", scopes, 22., &p)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(10.))
                        .child(
                            ui::button("signing-test", "Test Signing", Kind::Secondary, &p)
                                .on_click(run_test),
                        )
                        .when_some(test_line, |d, (color, text)| {
                            d.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(ts::SMALL)
                                    .text_color(color)
                                    .child(text),
                            )
                        }),
                )
                .when_some(shown_error, |a, e| {
                    a.child(div().text_size(ts::SMALL).text_color(p.deleted).child(e))
                })
                .on_ok(move |_, window, cx: &mut App| {
                    let s = read(cx);
                    if let Some(problem) = settings_problem(&s) {
                        *error.borrow_mut() = Some(problem);
                        window.refresh();
                        return false;
                    }
                    let all = everywhere.get();
                    let _ = view.update(cx, |v, cx| {
                        v.run_op_then(
                            "Saving signing settings",
                            if all {
                                "Signing settings saved for all repositories"
                            } else {
                                "Signing settings saved for this repository"
                            },
                            move |r, _, _| r.save_signing_settings(&s, all),
                            |v, cx| v.signing_changed(cx),
                            cx,
                        )
                    });
                    true
                })
        });
    }

    /// The commit box's identity line and the shown commit's signature may have changed.
    fn signing_changed(&mut self, cx: &mut Context<Self>) {
        self.changes.update(cx, |c, cx| c.reload_identity(cx));
        self.details.update(cx, |d, cx| d.reverify(cx));
    }
}

/// The first line of git's message for a failed test signature (the cause), without
/// the command line.
fn clean_error(e: &str) -> String {
    let message = e.split_once(" failed: ").map_or(e, |(_, m)| m);
    message
        .lines()
        .map(|l| {
            l.trim_start_matches("error: ")
                .trim_start_matches("fatal: ")
        })
        .find(|l| !l.trim().is_empty())
        .unwrap_or(message)
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_needs_a_key_to_sign() {
        let mut s = Settings {
            format: Format::Ssh,
            sign_commits: true,
            ..Settings::default()
        };
        assert!(settings_problem(&s).is_some());
        s.key = "key::ssh-ed25519 AAAA".into();
        assert!(settings_problem(&s).is_none());
        s.key.clear();
        s.format = Format::OpenPgp;
        assert!(settings_problem(&s).is_none());
    }

    #[test]
    fn cleans_git_errors() {
        assert_eq!(
            clean_error(
                "git commit-tree -S x failed: error: Load key \"/k\": No such file\n\
                 fatal: failed to write commit object"
            ),
            "Load key \"/k\": No such file"
        );
    }
}
