//! Git-flow in a repository tab: set it up (branch names and prefixes), start a feature,
//! release or hotfix, and finish one, from the toolbar's Git-flow menu, a branch's menu and
//! the palette. The steps themselves are `switchhead_git::flow`'s, run as one operation.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, ParentElement as _, Styled as _,
    WeakEntity, Window, div,
};
use switchhead_git::flow::{Config, FinishOptions, Kind};

use super::RepoView;
use super::form::Form;
use crate::appearance::{rpx, ts};
use crate::sidebar_model::branch_name_problem;
use crate::theme::palette;

impl RepoView {
    /// The current branch as a git-flow branch: its kind and name without the prefix.
    pub fn flow_current(&self) -> Option<(Kind, String)> {
        let cfg = self.refs.flow.as_ref()?;
        let branch = self.refs.head_branch.as_deref()?;
        cfg.classify(branch).map(|(k, n)| (k, n.to_owned()))
    }

    /// Ask for the production and development branches and the prefixes, then set up
    /// git-flow (or change its settings).
    pub fn flow_init_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branches: Rc<Vec<String>> =
            Rc::new(self.refs.branches.iter().map(|b| b.name.clone()).collect());
        let editing = self.refs.flow.is_some();
        let cfg = self
            .refs
            .flow
            .clone()
            .unwrap_or_else(|| Config::defaults(branches.iter().map(String::as_str)));
        let mut field = |value: &str, placeholder: &'static str| {
            let value = value.to_owned();
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                s.set_value(value, window, cx);
                s
            })
        };
        let fields: Rc<[(&'static str, Entity<InputState>); 6]> = Rc::new([
            ("Production branch", field(&cfg.master, "main")),
            ("Development branch", field(&cfg.develop, "develop")),
            ("Feature prefix", field(&cfg.feature, "feature/")),
            ("Release prefix", field(&cfg.release, "release/")),
            ("Hotfix prefix", field(&cfg.hotfix, "hotfix/")),
            (
                "Version tag prefix",
                field(&cfg.version_tag, "Optional, e.g. v"),
            ),
        ]);
        let error: Rc<RefCell<Option<String>>> = Rc::default();
        for (_, input) in fields.iter() {
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
        window.open_alert_dialog(cx, move |alert, window, cx| {
            if first.replace(false) {
                window.focus(&fields[0].1.focus_handle(cx), cx);
            }
            let p = palette(cx);
            let shown_error = error.borrow().clone();
            let mut alert = alert
                .confirm()
                .title(if editing {
                    "Git-flow Settings"
                } else {
                    "Initialize Git-flow"
                })
                .description(
                    "Features start from the development branch and merge back into it. \
                     Releases and hotfixes merge into the production branch with a tag. \
                     The settings are stored in this repository's config, where the git \
                     flow command line tool reads them too.",
                )
                .ok_text(if editing { "Save" } else { "Initialize" });
            for (label, input) in fields.iter() {
                alert = alert.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rpx(10.))
                        .child(
                            div()
                                .flex_none()
                                .w(rpx(130.))
                                .text_size(ts::BODY)
                                .text_color(p.fg2)
                                .child(*label),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(input).text_size(ts::BODY)),
                        ),
                );
            }
            let (fields, error, view, branches) = (
                fields.clone(),
                error.clone(),
                view.clone(),
                branches.clone(),
            );
            alert
                .when_some(shown_error, |a, e| {
                    a.child(div().text_size(ts::SMALL).text_color(p.deleted).child(e))
                })
                .on_ok(move |_, window, cx: &mut App| {
                    let v: Vec<String> = fields
                        .iter()
                        .map(|(_, i)| i.read(cx).value().trim().to_owned())
                        .collect();
                    let cfg = Config {
                        master: v[0].clone(),
                        develop: v[1].clone(),
                        feature: v[2].clone(),
                        release: v[3].clone(),
                        hotfix: v[4].clone(),
                        version_tag: v[5].clone(),
                    };
                    if let Some(problem) = init_problem(&cfg, &branches) {
                        *error.borrow_mut() = Some(problem);
                        window.refresh();
                        return false;
                    }
                    let _ = view.update(cx, |v, cx| {
                        v.run_op(
                            "Setting up git-flow",
                            "Git-flow is set up",
                            move |r, _, _| r.flow_init(&cfg),
                            cx,
                        )
                    });
                    true
                })
        });
    }

    /// Ask for a name, then start a `kind` branch from its base and check it out. Opens the
    /// setup first when git-flow is not set up.
    pub fn flow_start_dialog(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let Some(cfg) = self.refs.flow.clone() else {
            self.flow_init_dialog(window, cx);
            return;
        };
        let prefix = cfg.prefix(kind).to_owned();
        let form = Form {
            title: format!("Start {}", kind.label()),
            description: format!(
                "Creates {prefix}<name> from {} and checks it out.",
                cfg.base(kind)
            ),
            ok: "Start",
            danger: false,
            name: Some(String::new()),
            placeholder: match kind {
                Kind::Feature => "Feature name, e.g. login",
                Kind::Release => "Version, e.g. 1.2.0",
                Kind::Hotfix => "Version, e.g. 1.2.1",
            },
            taken: self
                .refs
                .branches
                .iter()
                .filter_map(|b| b.name.strip_prefix(&prefix).map(str::to_owned))
                .collect(),
            message: None,
            url: None,
            checks: Vec::new(),
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let cfg = cfg.clone();
                let (label, done) = match kind {
                    Kind::Feature => ("Starting feature", "Feature started"),
                    Kind::Release => ("Starting release", "Release started"),
                    Kind::Hotfix => ("Starting hotfix", "Hotfix started"),
                };
                v.run_op(
                    label,
                    done,
                    move |r, _, _| r.flow_start(&cfg, kind, &a.name, None).map(drop),
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Finish the `kind` branch `name` after asking (the tag's message, keep the branch).
    pub fn flow_finish_dialog(
        &mut self,
        kind: Kind,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy() {
            return;
        }
        let Some(cfg) = self.refs.flow.clone() else {
            return;
        };
        let branch = cfg.branch(kind, &name);
        let tag = cfg.tag(&name);
        let description = match kind {
            Kind::Feature => format!(
                "Merges {branch} into {} with a merge commit and checks out {}.",
                cfg.develop, cfg.develop
            ),
            Kind::Release => format!(
                "Merges {branch} into {} and tags it {tag}, then merges it into {} and \
                 checks out {}.",
                cfg.master, cfg.develop, cfg.develop
            ),
            Kind::Hotfix => format!(
                "Merges {branch} into {} and tags it {tag}, then merges it into {} (or into \
                 the release branch in progress) and checks out {}.",
                cfg.master, cfg.develop, cfg.develop
            ),
        };
        let form = Form {
            title: format!("Finish {} {name}", kind.label()),
            description: format!(
                "{description} If a merge stops on conflicts, resolve them, commit, and \
                 finish again."
            ),
            ok: "Finish",
            danger: false,
            name: None,
            placeholder: "",
            taken: Vec::new(),
            message: kind.tags().then_some("Tag message (optional)"),
            url: None,
            checks: vec![("Delete the branch", true)],
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let (cfg, name) = (cfg.clone(), name.clone());
                let opts = FinishOptions {
                    tag_message: Some(a.message).filter(|m| !m.is_empty()),
                    keep_branch: !a.checks.first().copied().unwrap_or(true),
                };
                let (label, done) = match kind {
                    Kind::Feature => ("Finishing feature", "Feature finished"),
                    Kind::Release => ("Finishing release", "Release finished"),
                    Kind::Hotfix => ("Finishing hotfix", "Hotfix finished"),
                };
                v.run_op(
                    label,
                    done,
                    move |r, _, _| r.flow_finish(&cfg, kind, &name, &opts),
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Finish the current branch when it is a git-flow branch (the palette's command).
    pub fn flow_finish_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refs.flow.is_none() {
            self.flow_init_dialog(window, cx);
            return;
        }
        match self.flow_current() {
            Some((kind, name)) => self.flow_finish_dialog(kind, name, window, cx),
            None => {
                self.op = Some(super::OpStatus::Failed(
                    "The current branch is not a git-flow feature, release or hotfix".into(),
                ));
                cx.notify();
            }
        }
    }
}

/// Why `cfg` cannot be saved, given the local `branches`.
fn init_problem(cfg: &Config, branches: &[String]) -> Option<String> {
    for (what, name) in [("production", &cfg.master), ("development", &cfg.develop)] {
        if let Some(p) = branch_name_problem(name) {
            return Some(format!("The {what} branch: {p}"));
        }
    }
    if cfg.master == cfg.develop {
        return Some("The production and development branches must differ".into());
    }
    if !branches.contains(&cfg.master) {
        return Some(format!("There is no branch {}", cfg.master));
    }
    for kind in Kind::ALL {
        let prefix = cfg.prefix(kind);
        if prefix.is_empty() {
            return Some(format!("Enter a {} prefix", kind.label().to_lowercase()));
        }
        if let Some(p) = branch_name_problem(&format!("{prefix}x")) {
            return Some(format!("The {} prefix: {p}", kind.label().to_lowercase()));
        }
    }
    if cfg.version_tag.chars().any(char::is_whitespace) {
        return Some("The version tag prefix cannot contain spaces".into());
    }
    None
}

/// The toolbar's Git-flow menu: set up, or start and finish branches.
pub(crate) fn build_flow_menu(
    mut menu: PopupMenu,
    view: &WeakEntity<RepoView>,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(v) = view.upgrade() else {
        return menu;
    };
    let (set_up, current, busy) = {
        let v = v.read(cx);
        (v.refs.flow.is_some(), v.flow_current(), v.busy())
    };
    let act = |f: Box<super::branch_ops::ViewAction>| {
        let view = view.clone();
        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| f(v, window, cx));
        }
    };
    if !set_up {
        return menu.item(
            PopupMenuItem::new("Initialize Git-flow…")
                .disabled(busy)
                .on_click(act(Box::new(|v, w, cx| v.flow_init_dialog(w, cx)))),
        );
    }
    for kind in Kind::ALL {
        menu = menu.item(
            PopupMenuItem::new(format!("Start {}…", kind.label()))
                .disabled(busy)
                .on_click(act(Box::new(move |v, w, cx| {
                    v.flow_start_dialog(kind, w, cx)
                }))),
        );
    }
    menu = menu.separator();
    menu = match current {
        Some((kind, name)) => menu.item(
            PopupMenuItem::new(format!("Finish {} {name}…", kind.label()))
                .disabled(busy)
                .on_click(act(Box::new(move |v, w, cx| {
                    v.flow_finish_dialog(kind, name.clone(), w, cx)
                }))),
        ),
        None => {
            menu.item(PopupMenuItem::new("Finish (check out a git-flow branch)").disabled(true))
        }
    };
    menu.separator().item(
        PopupMenuItem::new("Git-flow Settings…")
            .disabled(busy)
            .on_click(act(Box::new(|v, w, cx| v.flow_init_dialog(w, cx)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_the_settings() {
        let branches = vec!["main".to_owned()];
        let ok = Config::defaults(["main"]);
        assert_eq!(init_problem(&ok, &branches), None);
        let mut c = ok.clone();
        c.develop = "main".into();
        assert!(init_problem(&c, &branches).is_some());
        let mut c = ok.clone();
        c.master = "trunk".into();
        assert_eq!(
            init_problem(&c, &branches).as_deref(),
            Some("There is no branch trunk")
        );
        let mut c = ok.clone();
        c.feature = "my feature/".into();
        assert!(init_problem(&c, &branches).is_some());
        let mut c = ok;
        c.release = String::new();
        assert!(init_problem(&c, &branches).is_some());
    }
}
