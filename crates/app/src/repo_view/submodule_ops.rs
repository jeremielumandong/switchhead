//! Submodules in a repository tab: add one (a URL, a path and an optional branch), and
//! update them (clone missing ones and check out the commits the repository records),
//! with progress in the status bar.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Focusable as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use switchhead_git::submodules::default_path;

use super::RepoView;
use crate::appearance::{rpx, ts};
use crate::theme::palette;
use crate::ui;

impl RepoView {
    /// `git submodule update --init`: clone missing submodules and check out the commits
    /// the repository records, for `paths` (every submodule when empty), and their own
    /// submodules when `recursive`.
    pub fn update_submodules(
        &mut self,
        paths: Vec<PathBuf>,
        recursive: bool,
        cx: &mut Context<Self>,
    ) {
        self.run_op(
            "Updating submodules",
            "Submodules updated",
            move |r, cancel, on_line| {
                let paths: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .collect();
                let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
                r.submodule_update(&paths, true, recursive, cancel, |l| on_line(l))
            },
            cx,
        );
    }

    /// Ask for a URL, a path and a branch, then add the submodule.
    pub fn add_submodule_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let url = field("URL, e.g. git@github.com:owner/lib.git", window, cx);
        let path = field(
            "Path in this repository (default: the URL's name)",
            window,
            cx,
        );
        let branch = field("Branch to track (optional)", window, cx);
        let recursive = Rc::new(Cell::new(true));
        let error: Rc<RefCell<Option<String>>> = Rc::default();
        {
            let error = error.clone();
            cx.subscribe_in(&url, window, move |_, _, ev: &InputEvent, window, _| {
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
                window.focus(&url.focus_handle(cx), cx);
            }
            let p = palette(cx);
            let on = recursive.get();
            let shown_error = error.borrow().clone();
            let (url, path, branch, recursive, error, view) = (
                url.clone(),
                path.clone(),
                branch.clone(),
                recursive.clone(),
                error.clone(),
                view.clone(),
            );
            alert
                .confirm()
                .title("Add Submodule")
                .description(
                    "Clones another repository into a folder of this one and records its \
                     commit. Commit the new .gitmodules and folder to share it.",
                )
                .ok_text("Add Submodule")
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rpx(4.))
                        .child(Input::new(&url).text_size(ts::BODY))
                        .when_some(shown_error, |d, e| {
                            d.child(div().text_size(ts::SMALL).text_color(p.deleted).child(e))
                        }),
                )
                .child(Input::new(&path).text_size(ts::BODY))
                .child(Input::new(&branch).text_size(ts::BODY))
                .child({
                    let recursive = recursive.clone();
                    ui::checkbox("submodule-recursive", on, "Also clone its submodules", &p)
                        .cursor_pointer()
                        .on_click(move |_, window, _| {
                            recursive.set(!recursive.get());
                            window.refresh();
                        })
                })
                .on_ok(move |_, window, cx: &mut App| {
                    let text = |i: &gpui_kit::Entity<InputState>, cx: &App| {
                        i.read(cx).value().trim().to_owned()
                    };
                    let url_text = text(&url, cx);
                    if url_text.is_empty() {
                        *error.borrow_mut() = Some("Enter the repository's URL".into());
                        window.refresh();
                        return false;
                    }
                    let mut path_text = text(&path, cx);
                    if path_text.is_empty() {
                        path_text = default_path(&url_text);
                    }
                    let branch_text = text(&branch, cx);
                    let recursive = recursive.get();
                    let _ = view.update(cx, |v, cx| {
                        v.add_submodule(url_text, path_text, branch_text, recursive, cx)
                    });
                    true
                })
        });
    }

    fn add_submodule(
        &mut self,
        url: String,
        path: String,
        branch: String,
        recursive: bool,
        cx: &mut Context<Self>,
    ) {
        self.run_op(
            "Adding submodule",
            "Submodule added",
            move |r, cancel, on_line| {
                let branch = (!branch.is_empty()).then_some(branch.as_str());
                r.submodule_add(&url, &path, branch, cancel, |l| on_line(l))?;
                if recursive {
                    r.submodule_update(&[path.as_str()], true, true, cancel, |l| on_line(l))?;
                }
                Ok(())
            },
            cx,
        );
    }
}
