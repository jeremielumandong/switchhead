//! One small dialog for branch and remote operations: a title, a description, an
//! optional name field (checked like a branch name) and checkboxes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Input, InputEvent, InputState};
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
    pub checks: Vec<(&'static str, bool)>,
}

/// What a confirmed [`Form`] hands back.
pub(super) struct Answer {
    pub name: String,
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
            if first.replace(false)
                && let Some(input) = &input
            {
                let f = input.focus_handle(cx);
                window.focus(&f, cx);
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
            let (input, checks, error, view, on_ok, form) = (
                input.clone(),
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
                let answer = Answer {
                    name,
                    checks: checks.borrow().clone(),
                };
                let on_ok = on_ok.clone();
                let _ = view.update(cx, |v, cx| on_ok(v, answer, cx));
                true
            })
        });
    }
}
