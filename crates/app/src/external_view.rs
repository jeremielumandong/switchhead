//! Settings → External Tools: the terminal, editor, diff tool and merge tool.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    App, AppContext as _, Entity, Focusable as _, ParentElement as _, Styled as _, Window, div,
};

use crate::appearance::{rpx, ts};
use crate::external::{self, ExternalTools};
use crate::theme::palette;

/// Find the automatic choices (in the background), then show the settings.
pub fn open(window: &mut Window, cx: &mut App) {
    let task = cx
        .background_executor()
        .spawn(async { (external::default_terminal(), external::default_editor()) });
    window
        .spawn(cx, async move |cx| {
            let (terminal, editor) = task.await;
            let _ = cx.update(|window, cx| show(terminal, editor, window, cx));
        })
        .detach();
}

fn show(terminal: Option<String>, editor: Option<String>, window: &mut Window, cx: &mut App) {
    let current = cx
        .try_global::<ExternalTools>()
        .cloned()
        .unwrap_or_default();
    let auto = |found: Option<String>, none: &str| match found {
        Some(c) => format!("Automatic: {c}"),
        None => none.to_owned(),
    };
    let mut field = |value: &str, placeholder: String| {
        let value = value.to_owned();
        cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(placeholder);
            s.set_value(value, window, cx);
            s
        })
    };
    let fields: Rc<[(&'static str, Entity<InputState>); 4]> = Rc::new([
        (
            "Terminal",
            field(
                &current.terminal,
                auto(terminal, "None found, e.g. kitty --directory {path}"),
            ),
        ),
        (
            "Editor",
            field(
                &current.editor,
                auto(editor, "None found, e.g. code or zed"),
            ),
        ),
        (
            "Diff tool",
            field(
                &current.diff_tool,
                "git's diff.tool, or e.g. meld, kdiff3, bc, p4merge".to_owned(),
            ),
        ),
        (
            "Merge tool",
            field(
                &current.merge_tool,
                "git's merge.tool, or e.g. meld, kdiff3, bc, p4merge".to_owned(),
            ),
        ),
    ]);
    let first = Cell::new(true);
    window.open_alert_dialog(cx, move |alert, window, cx| {
        if first.replace(false) {
            window.focus(&fields[0].1.focus_handle(cx), cx);
        }
        let p = palette(cx);
        let mut alert = alert
            .confirm()
            .title("External Tools")
            .description(
                "Terminal and editor are command lines; {path} stands for the folder or \
                 file (an editor gets it at the end otherwise). Diff and merge tools are \
                 the names git difftool and mergetool know (git difftool --tool-help).",
            )
            .ok_text("Save");
        for (label, input) in fields.iter() {
            alert = alert.child(
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(10.))
                    .child(
                        div()
                            .flex_none()
                            .w(rpx(90.))
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
        let fields = fields.clone();
        alert.on_ok(move |_, _, cx: &mut App| {
            let get = |i: usize| fields[i].1.read(cx).value().trim().to_owned();
            let tools = ExternalTools {
                terminal: get(0),
                editor: get(1),
                diff_tool: get(2),
                merge_tool: get(3),
            };
            cx.set_global(tools);
            true
        })
    });
}
