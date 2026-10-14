//! One small dialog for branch and remote operations: a title, a description, an
//! optional name field (checked like a branch name), an optional message and checkboxes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Focusable as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::RepoView;
use crate::appearance::{rpx, ts};
use crate::sidebar_model::branch_name_problem;
use crate::theme::palette;
use crate::ui;

/// A small dialog: an optional name field and checkboxes.
pub(super) struct Form {
    pub title: String,
    pub description: String,
    pub ok: &'static str,
    pub danger: bool,
    /// The name field's starting text (`None`: no field).
    pub name: Option<String>,
    /// The name field's placeholder.
    pub placeholder: &'static str,
    /// Names the field must not take (existing branches).
    pub taken: Vec<String>,
    /// A multi-line message field's placeholder (`None`: no field).
    pub message: Option<&'static str>,
    /// A required URL field's starting text (`None`: no field).
    pub url: Option<String>,
    pub checks: Vec<(&'static str, bool)>,
}

/// What a confirmed [`Form`] hands back.
pub(super) struct Answer {
    pub name: String,
    /// The message field's text, trimmed (empty without the field).
    pub message: String,
    /// The URL field's text, trimmed (empty without the field).
    pub url: String,
    pub checks: Vec<bool>,
}

impl RepoView {
    pub(super) fn open_form(
        &mut self,
        form: Form,
        on_ok: impl Fn(&mut RepoView, Answer, &mut Context<RepoView>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = form.placeholder;
        let input = form.name.as_ref().map(|initial| {
            let initial = initial.clone();
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                s.set_value(initial, window, cx);
                s
            })
        });
        let message = form.message.map(|placeholder| {
            cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 8)
                    .placeholder(placeholder)
            })
        });
        let url = form.url.as_ref().map(|initial| {
            let initial = initial.clone();
            cx.new(|cx| {
                let mut s = InputState::new(window, cx)
                    .placeholder("URL, e.g. git@github.com:owner/repo.git");
                s.set_value(initial, window, cx);
                s
            })
        });
        let url_error: Rc<RefCell<Option<String>>> = Rc::default();
        if let Some(url) = &url {
            let url_error = url_error.clone();
            cx.subscribe_in(url, window, move |_, _, ev: &InputEvent, window, _| {
                if matches!(ev, InputEvent::Change) && url_error.borrow_mut().take().is_some() {
                    window.refresh();
                }
            })
            .detach();
        }
        let checks = Rc::new(RefCell::new(
            form.checks.iter().map(|(_, on)| *on).collect::<Vec<_>>(),
        ));
        let error: Rc<RefCell<Option<String>>> = Rc::default();
        if let Some(input) = &input {
            // Typing clears the last complaint.
            let error = error.clone();
            cx.subscribe_in(input, window, move |_, _, ev: &InputEvent, window, _| {
                if matches!(ev, InputEvent::Change) && error.borrow_mut().take().is_some() {
                    window.refresh();
                }
            })
            .detach();
        }
        let first = Cell::new(true);
        let view = cx.entity().downgrade();
        let on_ok = Rc::new(on_ok);
        let form = Rc::new(form);
        window.open_alert_dialog(cx, move |alert, window, cx| {
            if first.replace(false) {
                // The name field first, else the message.
                let f = match (&input, &message) {
                    (Some(i), _) => Some(i.focus_handle(cx)),
                    (None, Some(m)) => Some(m.focus_handle(cx)),
                    _ => None,
                };
                if let Some(f) = f {
                    window.focus(&f, cx);
                }
            }
            let p = palette(cx);
            let mut alert = alert
                .confirm()
                .title(form.title.clone())
                .description(form.description.clone())
                .ok_text(form.ok)
                .when(form.danger, |a| a.ok_variant(ButtonVariant::Danger));
            if let Some(input) = &input {
                alert = alert.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rpx(4.))
                        .child(Input::new(input).text_size(ts::BODY))
                        .when_some(error.borrow().clone(), |d, e| {
                            d.child(div().text_size(ts::SMALL).text_color(p.deleted).child(e))
                        }),
                );
            }
            if let Some(url) = &url {
                alert = alert.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rpx(4.))
                        .child(Input::new(url).text_size(ts::BODY))
                        .when_some(url_error.borrow().clone(), |d, e| {
                            d.child(div().text_size(ts::SMALL).text_color(p.deleted).child(e))
                        }),
                );
            }
            if let Some(message) = &message {
                alert = alert.child(Textarea::new(message).text_size(ts::BODY));
            }
            for (i, (label, _)) in form.checks.iter().enumerate() {
                let on = checks.borrow().get(i).copied().unwrap_or(false);
                let checks = checks.clone();
                alert = alert.child(
                    ui::checkbox(("form-check", i), on, *label, &p)
                        .cursor_pointer()
                        .on_click(move |_, window, _| {
                            if let Some(c) = checks.borrow_mut().get_mut(i) {
                                *c = !*c;
                            }
                            window.refresh();
                        }),
                );
            }
            let (input, url, url_error, message, checks, error, view, on_ok, form) = (
                input.clone(),
                url.clone(),
                url_error.clone(),
                message.clone(),
                checks.clone(),
                error.clone(),
                view.clone(),
                on_ok.clone(),
                form.clone(),
            );
            alert.on_ok(move |_, window, cx: &mut App| {
                let name = input
                    .as_ref()
                    .map(|i| i.read(cx).value().trim().to_owned())
                    .unwrap_or_default();
                if input.is_some() {
                    let problem = branch_name_problem(&name).map(str::to_owned).or_else(|| {
                        form.taken
                            .contains(&name)
                            .then(|| format!("{name} already exists"))
                    });
                    if problem.is_some() {
                        *error.borrow_mut() = problem;
                        window.refresh();
                        return false;
                    }
                }
                let url_text = url
                    .as_ref()
                    .map(|u| u.read(cx).value().trim().to_owned())
                    .unwrap_or_default();
                if url.is_some() && url_text.is_empty() {
                    *url_error.borrow_mut() = Some("Enter the remote's URL".into());
                    window.refresh();
                    return false;
                }
                let answer = Answer {
                    name,
                    url: url_text,
                    message: message
                        .as_ref()
                        .map(|m| m.read(cx).value().trim().to_owned())
                        .unwrap_or_default(),
                    checks: checks.borrow().clone(),
                };
                let on_ok = on_ok.clone();
                let _ = view.update(cx, |v, cx| on_ok(v, answer, cx));
                true
            })
        });
    }
}
