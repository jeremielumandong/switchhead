//! Settings → Custom Commands: add, edit and remove the user's commands (name, what they
//! run on, the shell command, whether to ask first). Saving updates the [`CustomCommands`]
//! global, which the workspace writes to the app state.

use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::appearance::{rpx, ts};
use crate::custom_commands::{CommandTarget, CustomCommand, CustomCommands, VARIABLES};
use crate::theme::{MONO, palette};
use crate::ui::{self, Kind};

/// What the page asks of the workspace.
pub enum CustomCommandsEvent {
    /// The close button.
    Close,
}

/// The settings page.
pub struct CustomCommandsView {
    /// The commands being edited (saved on Save).
    commands: Vec<CustomCommand>,
    /// The one in the editor.
    selected: Option<usize>,
    name: Entity<InputState>,
    script: Entity<TextareaState>,
    saved: bool,
}

impl EventEmitter<CustomCommandsEvent> for CustomCommandsView {}

impl CustomCommandsView {
    /// A view of the saved commands.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let commands = cx
            .try_global::<CustomCommands>()
            .map(|c| c.0.clone())
            .unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name in menus"));
        let script = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("Shell command, e.g. git log --oneline ${ref} -10")
        });
        let mut this = Self {
            commands,
            selected: None,
            name,
            script,
            saved: false,
        };
        if !this.commands.is_empty() {
            this.select(Some(0), window, cx);
        }
        this
    }

    /// Write the editor's fields back into the selected command.
    fn store_fields(&mut self, cx: &mut Context<Self>) {
        let (name, script) = (
            self.name.read(cx).value().to_string(),
            self.script.read(cx).value().to_string(),
        );
        if let Some(c) = self.selected.and_then(|i| self.commands.get_mut(i)) {
            c.name = name;
            c.script = script;
        }
    }

    fn select(&mut self, i: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.store_fields(cx);
        self.selected = i.filter(|i| *i < self.commands.len());
        let c = self
            .selected
            .and_then(|i| self.commands.get(i))
            .cloned()
            .unwrap_or_default();
        self.name
            .update(cx, |s, cx| s.set_value(c.name.clone(), window, cx));
        self.script
            .update(cx, |s, cx| s.set_value(c.script.clone(), window, cx));
        cx.notify();
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.store_fields(cx);
        self.commands.push(CustomCommand {
            name: "New Command".into(),
            ..Default::default()
        });
        self.saved = false;
        self.select(Some(self.commands.len() - 1), window, cx);
        let focus = self.name.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }

    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.selected else {
            return;
        };
        self.selected = None;
        self.commands.remove(i);
        self.saved = false;
        let next = (!self.commands.is_empty()).then(|| i.min(self.commands.len() - 1));
        self.select(next, window, cx);
    }

    fn edit(&mut self, f: impl FnOnce(&mut CustomCommand), cx: &mut Context<Self>) {
        if let Some(c) = self.selected.and_then(|i| self.commands.get_mut(i)) {
            f(c);
            self.saved = false;
            cx.notify();
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.store_fields(cx);
        cx.set_global(CustomCommands(self.commands.clone()));
        self.saved = true;
        cx.notify();
    }
}

impl Render for CustomCommandsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        // The list shows names as typed, before Save.
        let typed = self.name.read(cx).value().to_string();
        let list =
            div()
                .flex()
                .flex_col()
                .w(rpx(210.))
                .flex_none()
                .gap(rpx(2.))
                .children(self.commands.iter().enumerate().map(|(i, c)| {
                    let on = self.selected == Some(i);
                    let name = if on { typed.clone() } else { c.name.clone() };
                    div()
                        .id(("custom-cmd", i))
                        .flex()
                        .flex_col()
                        .px(rpx(8.))
                        .py(rpx(5.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .when(on, |d| d.bg(p.sel))
                        .hover(|s| s.bg(p.hover))
                        .on_click(cx.listener(move |this, _, w, cx| this.select(Some(i), w, cx)))
                        .child(div().text_size(ts::UI).truncate().child(
                            if name.trim().is_empty() {
                                "Untitled".to_owned()
                            } else {
                                name
                            },
                        ))
                        .child(
                            div()
                                .text_size(ts::SMALL)
                                .text_color(p.fg3)
                                .child(c.target.label()),
                        )
                }))
                .when(self.commands.is_empty(), |d| {
                    d.child(
                        div()
                            .px(rpx(8.))
                            .py(rpx(5.))
                            .text_size(ts::BODY)
                            .text_color(p.fg3)
                            .child("No commands yet"),
                    )
                })
                .child(
                    div()
                        .flex()
                        .gap(rpx(6.))
                        .pt(rpx(8.))
                        .child(
                            ui::button("custom-add", "Add", Kind::Secondary, &p)
                                .on_click(cx.listener(|this, _, w, cx| this.add(w, cx))),
                        )
                        .when(self.selected.is_some(), |d| {
                            d.child(
                                ui::button("custom-remove", "Remove", Kind::Secondary, &p)
                                    .on_click(cx.listener(|this, _, w, cx| this.remove(w, cx))),
                            )
                        }),
                );
        let label = |text: &'static str| div().text_size(ts::SMALL).text_color(p.fg3).child(text);
        let editor = match self.selected.and_then(|i| self.commands.get(i)) {
            None => div()
                .flex_1()
                .text_size(ts::BODY)
                .text_color(p.fg3)
                .child("Add a command to run a shell command on the repository, a commit, a branch or tag, or a file."),
            Some(c) => {
                let (target, confirm) = (c.target, c.confirm);
                let targets = CommandTarget::ALL
                    .into_iter()
                    .map(|t| {
                        let view = cx.entity().downgrade();
                        let on_click: ui::OnClick = Box::new(move |_, _, cx| {
                            let _ = view.update(cx, |v, cx| v.edit(|c| c.target = t, cx));
                        });
                        (SharedString::from(t.label()), t == target, on_click)
                    })
                    .collect();
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(rpx(10.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rpx(4.))
                            .child(label("Name"))
                            .child(Input::new(&self.name).text_size(ts::BODY)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rpx(4.))
                            .child(label("Runs on (where it appears)"))
                            .child(ui::segmented("custom-target", targets, 22., &p)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rpx(4.))
                            .child(label("Command (runs with sh -c in the repository's folder; cmd on Windows)"))
                            .child(
                                div()
                                    .font_family(MONO)
                                    .child(Textarea::new(&self.script).text_size(ts::BODY)),
                            ),
                    )
                    .child(
                        ui::checkbox("custom-confirm", confirm, "Ask before running (shows the command)", &p)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit(|c| c.confirm = !c.confirm, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rpx(2.))
                            .text_size(ts::SMALL)
                            .text_color(p.fg3)
                            .child("Variables (filled in quoted when needed; output goes to the activity log):")
                            .children(VARIABLES.iter().map(|(v, what)| {
                                div()
                                    .flex()
                                    .gap(rpx(8.))
                                    .child(div().w(rpx(70.)).font_family(MONO).text_color(p.fg2).child(*v))
                                    .child(*what)
                            })),
                    )
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(rpx(16.))
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
                            .child("Custom Commands"),
                    )
                    .child(
                        ui::button("custom-close", "Close", Kind::Secondary, &p).on_click(
                            cx.listener(|_, _, _, cx| cx.emit(CustomCommandsEvent::Close)),
                        ),
                    ),
            )
            .child(div().flex().gap(rpx(16.)).child(list).child(editor))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(10.))
                    .child(
                        ui::button("custom-save", "Save", Kind::Primary, &p)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )
                    .when(self.saved, |d| {
                        d.child(div().text_size(ts::BODY).text_color(p.added).child("Saved"))
                    }),
            )
    }
}
