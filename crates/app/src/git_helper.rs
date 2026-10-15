//! The git command helper: a panel beside the history where the user says what they
//! want to do, the coding CLI answers with an explanation and `git` commands, and each
//! command runs only when its Run button is clicked, with its output shown under it.
//! Commands that can lose work ask first. The CLI itself never changes the repository.

use std::collections::HashMap;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use switchhead_agents::CancelHandle;
use switchhead_git::Repo;
use switchhead_git::cli::Cancel;

use crate::appearance::{rpx, ts};
use crate::changes_view::small_button;
use crate::command_line::{self, CommandLine, Part};
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// What the helper reports to its repository view.
pub enum HelperEvent {
    /// A command ran: the repository may have changed.
    Ran,
    /// The close button.
    Close,
}

/// A command's run: `(part, line)` in the answer.
type Key = (usize, usize);

enum Run {
    Running(Cancel),
    Done { ok: bool, output: String },
}

/// The helper panel for one repository.
pub struct GitHelper {
    repo: Repo,
    input: Entity<InputState>,
    focus: FocusHandle,
    asking: Option<CancelHandle>,
    question: Option<String>,
    parts: Vec<Part>,
    runs: HashMap<Key, Run>,
    error: Option<SharedString>,
    _sub: Subscription,
}

impl EventEmitter<HelperEvent> for GitHelper {}

impl Focusable for GitHelper {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for GitHelper {
    fn drop(&mut self) {
        if let Some(c) = self.asking.take() {
            c.cancel();
        }
        for run in self.runs.values() {
            if let Run::Running(c) = run {
                c.cancel();
            }
        }
    }
}

impl GitHelper {
    /// An empty helper for `repo`.
    pub fn new(repo: Repo, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder("What do you want to do with git?"));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.ask(window, cx);
            }
        });
        Self {
            repo,
            input,
            focus: cx.focus_handle(),
            asking: None,
            question: None,
            parts: Vec::new(),
            runs: HashMap::new(),
            error: None,
            _sub: sub,
        }
    }

    /// Put the cursor in the question field.
    pub fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Ask the coding CLI; a second click stops it.
    fn ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = self.asking.take() {
            c.cancel();
            cx.notify();
            return;
        }
        let question = self.input.read(cx).value().trim().to_owned();
        if question.is_empty() {
            return;
        }
        let prompt = crate::assistant::command_prompt(&question);
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        self.ask_prompt(question, prompt, cx);
    }

    /// Run `prompt` (shown as `title`) in place of the current answer, stopping a run
    /// still in progress. Explain Commit and Review Branch come through here.
    pub fn ask_prompt(&mut self, title: String, prompt: String, cx: &mut Context<Self>) {
        if let Some(c) = self.asking.take() {
            c.cancel();
        }
        let settings = cx.global::<crate::assistant::AssistantSettings>().clone();
        let (cancel, rx) = match crate::assistant::spawn(&settings, self.repo.root(), prompt) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(e.into());
                cx.notify();
                return;
            }
        };
        self.asking = Some(cancel);
        self.error = None;
        self.question = Some(title);
        self.parts.clear();
        // A command still running finishes, but its output belonged to the old answer.
        self.runs.clear();
        cx.spawn(async move |this, cx| {
            let res = rx
                .await
                .unwrap_or_else(|_| Err("The assistant stopped.".into()));
            let _ = this.update(cx, |v, cx| {
                // `None`: stopped from the button.
                if v.asking.take().is_some() {
                    match res {
                        Ok(a) => v.parts = command_line::parts(&a),
                        Err(e) => v.error = Some(e.trim().to_owned().into()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn command(&self, key: Key) -> Option<&CommandLine> {
        match self.parts.get(key.0)? {
            Part::Commands(c) => c.get(key.1),
            _ => None,
        }
    }

    /// Run a command, after a confirmation when it can lose work.
    fn run(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cmd) = self.command(key).cloned() else {
            return;
        };
        if self.runs.values().any(|r| matches!(r, Run::Running(_))) {
            return;
        }
        if !cmd.is_destructive() {
            return self.start(key, cx);
        }
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .confirm()
                .title("Run this command?")
                .description(format!(
                    "{}\n\nIt can discard changes or rewrite history.",
                    cmd.line
                ))
                .ok_text("Run")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let _ = this.update(cx, |v, cx| v.start(key, cx));
                    true
                })
        });
    }

    fn start(&mut self, key: Key, cx: &mut Context<Self>) {
        let Some(args) = self.command(key).and_then(|c| c.args.clone()) else {
            return;
        };
        let cancel = Cancel::default();
        self.runs.insert(key, Run::Running(cancel.clone()));
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move {
            let res = repo
                .git()
                .and_then(|g| g.run_streaming(&args, &cancel, |_| {}));
            match res {
                Ok(out) => (true, joined(&out.text(), &out.err_text())),
                Err(switchhead_git::Error::Command { message, .. }) => (false, message),
                Err(e) => (false, e.to_string()),
            }
        });
        cx.spawn(async move |this, cx| {
            let (ok, output) = task.await;
            let _ = this.update(cx, |v, cx| {
                if matches!(v.runs.get(&key), Some(Run::Running(_))) {
                    v.runs.insert(key, Run::Done { ok, output });
                }
                cx.emit(HelperEvent::Ran);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn stop(&mut self, key: Key, cx: &mut Context<Self>) {
        if let Some(Run::Running(c)) = self.runs.get(&key) {
            c.cancel();
        }
        cx.notify();
    }

    fn render_header(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let asking = self.asking.is_some();
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(rpx(8.))
            .p(rpx(10.))
            .border_b_1()
            .border_color(p.bd)
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(ts::BODY)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.fg)
                            .child("Git Helper"),
                    )
                    .child(
                        ui::icon_button("helper-close", IconName::Close, false, p)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(HelperEvent::Close))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.input).small().text_size(ts::BODY)),
                    )
                    .child(
                        ui::button(
                            "helper-ask",
                            if asking { "Stop" } else { "Ask" },
                            if asking {
                                ui::Kind::Secondary
                            } else {
                                ui::Kind::Primary
                            },
                            p,
                        )
                        .h(rpx(24.))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| this.ask(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_commands(
        &self,
        part: usize,
        cmds: &[CommandLine],
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.runs.values().any(|r| matches!(r, Run::Running(_)));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(rpx(6.))
            .children(cmds.iter().enumerate().map(|(i, c)| {
                let key = (part, i);
                let run = self.runs.get(&key);
                let running = matches!(run, Some(Run::Running(_)));
                let line = c.line.clone();
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(p.bd)
                    .rounded(px(6.))
                    .bg(p.panel)
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(rpx(6.))
                            .px(rpx(8.))
                            .py(rpx(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family(MONO)
                                    .text_size(ts::SMALL)
                                    .text_color(if c.args.is_some() { p.fg } else { p.fg2 })
                                    .child(c.line.clone()),
                            )
                            .when(c.args.is_some(), |d| {
                                d.child(if running {
                                    small_button(("stop", part * 1000 + i), "Stop", true, p)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| this.stop(key, cx)),
                                        )
                                } else {
                                    let label = if c.is_destructive() { "Run…" } else { "Run" };
                                    small_button(("run", part * 1000 + i), label, !busy, p)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.run(key, window, cx)
                                        }))
                                })
                            })
                            .child(
                                small_button(("copy", part * 1000 + i), "Copy", true, p).on_click(
                                    move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            line.clone(),
                                        ))
                                    },
                                ),
                            ),
                    )
                    .when_some(c.why_not, |d, why| {
                        d.child(
                            div()
                                .px(rpx(8.))
                                .pb(rpx(6.))
                                .text_size(ts::CAPTION_PLUS)
                                .text_color(p.fg3)
                                .child(why),
                        )
                    })
                    .when(running, |d| {
                        d.child(ui::loading_bar(("cmd-busy", part * 1000 + i), p))
                    })
                    .when_some(
                        match run {
                            Some(Run::Done { ok, output }) => Some((*ok, output.clone())),
                            _ => None,
                        },
                        |d, (ok, output)| d.child(render_output(ok, &output, p)),
                    )
            }))
            .into_any_element()
    }

    fn render_answer(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let parts: Vec<AnyElement> = self
            .parts
            .iter()
            .enumerate()
            .map(|(i, part)| match part {
                Part::Text(t) => div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(rpx(4.))
                    .text_size(ts::BODY)
                    .text_color(p.fg)
                    .children(
                        command_line::prose_blocks(t)
                            .into_iter()
                            .map(|b| div().child(b))
                            .collect::<Vec<_>>(),
                    )
                    .into_any_element(),
                Part::Code(code) => div()
                    .flex_none()
                    .p(rpx(8.))
                    .rounded(px(6.))
                    .bg(p.panel)
                    .font_family(MONO)
                    .text_size(ts::SMALL)
                    .text_color(p.fg2)
                    .children(code.lines().map(|l| div().child(l.to_owned())))
                    .into_any_element(),
                Part::Commands(cmds) => self.render_commands(i, cmds, p, cx),
            })
            .collect();
        let empty = self.parts.is_empty() && self.asking.is_none() && self.error.is_none();
        div()
            .id("helper-answer")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(rpx(10.))
            .flex()
            .flex_col()
            .gap(rpx(10.))
            .when(empty, |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(rpx(6.))
                        .text_size(ts::SMALL)
                        .text_color(p.fg3)
                        .child(
                            "Describe what you want to do. The assistant reads the \
                             repository and suggests git commands; nothing runs until you \
                             click Run.",
                        )
                        .children(EXAMPLES.iter().enumerate().map(|(i, e)| {
                            div()
                                .id(("example", i))
                                .text_color(p.acc)
                                .cursor_pointer()
                                .child(*e)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.input
                                        .update(cx, |s, cx| s.set_value(EXAMPLES[i], window, cx));
                                    this.ask(window, cx);
                                }))
                        })),
                )
            })
            .when_some(self.question.clone(), |d, q| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(ts::SMALL)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.fg2)
                        .child(q),
                )
            })
            .when(self.asking.is_some(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(ts::SMALL)
                        .text_color(p.fg3)
                        .child("Reading the repository…"),
                )
            })
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(ts::SMALL)
                        .text_color(p.deleted)
                        .child(e),
                )
            })
            .children(parts)
            .into_any_element()
    }
}

/// Suggestions shown before the first question.
const EXAMPLES: [&str; 3] = [
    "Undo my last commit but keep its changes",
    "Which branches are already merged into the current one?",
    "Rename the current branch",
];

/// Most output lines shown under a command.
const MAX_OUTPUT_LINES: usize = 200;

fn joined(stdout: &str, stderr: &str) -> String {
    match (stdout.trim_end(), stderr.trim_end()) {
        ("", e) => e.to_owned(),
        (o, "") => o.to_owned(),
        (o, e) => format!("{o}\n{e}"),
    }
}

fn render_output(ok: bool, output: &str, p: &Palette) -> AnyElement {
    let lines: Vec<&str> = output.lines().collect();
    let more = lines.len().saturating_sub(MAX_OUTPUT_LINES);
    div()
        .flex_none()
        .px(rpx(8.))
        .py(rpx(6.))
        .border_t_1()
        .border_color(p.bd)
        .bg(p.surface)
        .font_family(MONO)
        .text_size(ts::SMALL)
        .text_color(if ok { p.fg2 } else { p.deleted })
        .child(
            div()
                .text_color(if ok { p.added } else { p.deleted })
                .child(if ok { "Done" } else { "Failed" }),
        )
        .children(
            lines
                .iter()
                .take(MAX_OUTPUT_LINES)
                .map(|l| div().child(crate::merge_view::card_line(l))),
        )
        .when(more > 0, |d| {
            d.child(
                div()
                    .text_color(p.fg3)
                    .child(format!("… {more} more lines")),
            )
        })
        .into_any_element()
}

impl Render for GitHelper {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(self.render_header(&p, cx))
            .when(self.asking.is_some(), |d| {
                d.child(ui::loading_bar("helper-busy", &p))
            })
            .child(self.render_answer(&p, cx))
    }
}
