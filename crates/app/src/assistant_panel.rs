//! The assistant panel beside the history: a conversation with the coding CLI about the
//! repository. Each answer streams in with the tools the CLI called (switchhead's
//! read-only git tools); prose keeps its Markdown lists and styles, and every `git`
//! command in an answer becomes a card that runs only when its Run button is clicked,
//! with its output shown under it. Commands that can lose work ask first. A follow-up
//! continues the CLI's session and tells it which commands ran meanwhile. Explain Commit
//! and Review Branch start a new conversation here. The CLI itself never changes the
//! repository. Ported from Switchyard's assistant panel; the panel only consumes
//! normalized `AgentEvent`s.

use std::collections::HashMap;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, px,
};
use switchhead_agents::{AgentEvent, AgentKind, CancelHandle};
use switchhead_git::Repo;
use switchhead_git::cli::Cancel;

use crate::appearance::{rpx, ts};
use crate::changes_view::small_button;
use crate::command_line::{self, CommandLine, Inline, Part, Prose};
use crate::rich_text::RichText;
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// What the panel reports to its repository view.
pub enum PanelEvent {
    /// A command ran: the repository may have changed.
    Ran,
    /// The close button.
    Close,
}

/// A command's run: `(turn, part, line)`.
type Key = (usize, usize, usize);

enum Run {
    Running(Cancel),
    Done { ok: bool, output: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolState {
    Running,
    Ok,
    Failed,
}

/// A tool call the CLI made.
struct Tool {
    id: String,
    /// `diff`, `log`, …
    name: String,
    /// Its arguments, shortened.
    detail: String,
    state: ToolState,
}

/// One question and its answer.
struct Turn {
    /// The question as shown.
    asked: String,
    tools: Vec<Tool>,
    /// The answer so far.
    text: String,
    /// `text` split into prose, code and commands.
    parts: Vec<Part>,
    error: Option<String>,
    running: bool,
}

/// The assistant panel for one repository.
pub struct AssistantPanel {
    repo: Repo,
    input: Entity<InputState>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    turns: Vec<Turn>,
    /// The CLI's conversation to continue, and which CLI it belongs to.
    session: Option<(AgentKind, String)>,
    /// The run in progress.
    asking: Option<CancelHandle>,
    /// Counts runs, so events of a stopped one are dropped.
    generation: u64,
    /// The CLI of the current conversation, for the header.
    agent: Option<AgentKind>,
    runs: HashMap<Key, Run>,
    /// Commands run since the last question: `(line, ok)`.
    ran: Vec<(String, bool)>,
    _sub: Subscription,
}

impl EventEmitter<PanelEvent> for AssistantPanel {}

impl Focusable for AssistantPanel {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for AssistantPanel {
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

impl AssistantPanel {
    /// An empty panel for `repo`.
    pub fn new(repo: Repo, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Ask about this repository or a git task")
        });
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.ask(window, cx);
            }
        });
        Self {
            repo,
            input,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            turns: Vec::new(),
            session: None,
            asking: None,
            generation: 0,
            agent: None,
            runs: HashMap::new(),
            ran: Vec::new(),
            _sub: sub,
        }
    }

    /// Put the cursor in the question field.
    pub fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Ask the question in the field: a follow-up when there is a conversation. While a
    /// run is in progress the button stops it instead.
    fn ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.asking.is_some() {
            return self.stop_asking(cx);
        }
        let question = self.input.read(cx).value().trim().to_owned();
        if question.is_empty() {
            return;
        }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        let prompt = if self.session.is_some() {
            crate::assistant::follow_up_prompt(&question, &self.ran)
        } else {
            crate::assistant::command_prompt(&question)
        };
        self.send(question, prompt, true, cx);
    }

    /// Ask `prompt` (shown as `title`) in a new conversation, stopping a run still in
    /// progress. Explain Commit and Review Branch come through here.
    pub fn ask_prompt(&mut self, title: String, prompt: String, cx: &mut Context<Self>) {
        if self.asking.is_some() {
            self.stop_asking(cx);
        }
        self.send(title, prompt, false, cx);
    }

    /// Clear the conversation.
    fn reset(&mut self, cx: &mut Context<Self>) {
        if self.asking.is_some() {
            self.stop_asking(cx);
        }
        self.turns.clear();
        self.session = None;
        self.agent = None;
        self.ran.clear();
        // A command still running finishes, but its card is gone.
        self.runs.clear();
        cx.notify();
    }

    fn send(&mut self, asked: String, prompt: String, follow_up: bool, cx: &mut Context<Self>) {
        let settings = cx.global::<crate::assistant::AssistantSettings>().clone();
        // A session only resumes on the CLI that started it.
        let resume = self
            .session
            .as_ref()
            .filter(|(k, _)| follow_up && settings.agent() == Some(*k))
            .map(|(_, id)| id.clone());
        let mut turn = Turn {
            asked,
            tools: Vec::new(),
            text: String::new(),
            parts: Vec::new(),
            error: None,
            running: true,
        };
        let started = crate::assistant::stream(&settings, self.repo.root(), prompt, resume);
        self.ran.clear();
        self.scroll.scroll_to_bottom();
        let (cancel, kind, mut rx) = match started {
            Ok(r) => r,
            Err(e) => {
                turn.running = false;
                turn.error = Some(e);
                self.turns.push(turn);
                cx.notify();
                return;
            }
        };
        if !follow_up || self.agent != Some(kind) {
            self.session = None;
        }
        self.agent = Some(kind);
        self.turns.push(turn);
        self.asking = Some(cancel);
        self.generation += 1;
        let (generation, ix) = (self.generation, self.turns.len() - 1);
        cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.recv().await {
                let alive = this.update(cx, |v, cx| {
                    if v.generation == generation {
                        v.apply(ix, kind, ev, cx);
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |v, cx| {
                if v.generation == generation {
                    v.finish(ix, cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Whether the answer is scrolled to (near) its end, so new text should follow.
    fn at_bottom(&self) -> bool {
        let max = self.scroll.max_offset().y;
        -self.scroll.offset().y >= max - px(48.)
    }

    fn apply(&mut self, ix: usize, kind: AgentKind, ev: AgentEvent, cx: &mut Context<Self>) {
        let follow = self.at_bottom();
        let Some(turn) = self.turns.get_mut(ix) else {
            return;
        };
        match ev {
            AgentEvent::Started {
                session_id: Some(id),
                ..
            } => self.session = Some((kind, id)),
            AgentEvent::Text(t) => {
                turn.text.push_str(&t);
                turn.parts = command_line::parts(&turn.text);
            }
            AgentEvent::ToolCall {
                id,
                name,
                arguments,
            } => turn.tools.push(Tool {
                id,
                name,
                detail: tool_detail(&arguments),
                state: ToolState::Running,
            }),
            AgentEvent::ToolResult { id, is_error, .. } => {
                if let Some(t) = turn.tools.iter_mut().find(|t| t.id == id) {
                    t.state = if is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Ok
                    };
                }
            }
            AgentEvent::Done(s) => {
                // The final answer, without the text written between tool calls.
                if !s.text.trim().is_empty() {
                    turn.text = s.text;
                    turn.parts = command_line::parts(&turn.text);
                }
                if let Some(id) = s.session_id {
                    self.session = Some((kind, id));
                }
            }
            AgentEvent::Error(e) => turn.error = Some(e.trim().to_owned()),
            AgentEvent::Exited(_) => return self.finish(ix, cx),
            AgentEvent::Thinking(_) | AgentEvent::Log(_) | AgentEvent::Started { .. } => return,
        }
        if follow {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    fn finish(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.asking = None;
        if let Some(turn) = self.turns.get_mut(ix)
            && turn.running
        {
            turn.running = false;
            for t in &mut turn.tools {
                if t.state == ToolState::Running {
                    t.state = ToolState::Ok;
                }
            }
            if turn.text.trim().is_empty() && turn.error.is_none() {
                turn.error = Some("The assistant gave no answer.".into());
            }
        }
        cx.notify();
    }

    fn stop_asking(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.asking.take() {
            c.cancel();
        }
        // Events still on their way belong to the stopped run.
        self.generation += 1;
        if let Some(turn) = self.turns.last_mut()
            && turn.running
        {
            turn.running = false;
            if turn.error.is_none() {
                turn.error = Some("Stopped.".into());
            }
        }
        cx.notify();
    }

    fn command(&self, key: Key) -> Option<&CommandLine> {
        match self.turns.get(key.0)?.parts.get(key.1)? {
            Part::Commands(c) => c.get(key.2),
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
        let Some(cmd) = self.command(key).cloned() else {
            return;
        };
        let Some(args) = cmd.args else {
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
        let line = cmd.line;
        cx.spawn(async move |this, cx| {
            let (ok, output) = task.await;
            let _ = this.update(cx, |v, cx| {
                if matches!(v.runs.get(&key), Some(Run::Running(_))) {
                    v.runs.insert(key, Run::Done { ok, output });
                    v.ran.push((line, ok));
                }
                cx.emit(PanelEvent::Ran);
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
        let agent = self
            .agent
            .or_else(|| cx.global::<crate::assistant::AssistantSettings>().agent)
            .map(|k| k.display_name());
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
                    .gap(rpx(4.))
                    .child(
                        div()
                            .flex_none()
                            .text_size(ts::BODY)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.fg)
                            .child("Assistant"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .pl(rpx(4.))
                            .text_size(ts::SMALL)
                            .text_color(p.fg3)
                            .children(agent),
                    )
                    .when(!self.turns.is_empty(), |d| {
                        d.child(
                            small_button("asst-new", "New", true, p)
                                .on_click(cx.listener(|this, _, _, cx| this.reset(cx))),
                        )
                    })
                    .child(
                        ui::icon_button("asst-settings", IconName::Settings, false, p).on_click(
                            |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(crate::actions::ShowAssistantSettings),
                                    cx,
                                )
                            },
                        ),
                    )
                    .child(
                        ui::icon_button("asst-close", IconName::Close, false, p)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PanelEvent::Close))),
                    ),
            )
            .into_any_element()
    }

    fn render_input(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let asking = self.asking.is_some();
        let follow_up = self.session.is_some();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(rpx(6.))
            .p(rpx(10.))
            .border_t_1()
            .border_color(p.bd)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.input).small().text_size(ts::BODY)),
            )
            .child(
                ui::button(
                    "asst-ask",
                    if asking {
                        "Stop"
                    } else if follow_up {
                        "Reply"
                    } else {
                        "Ask"
                    },
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
            )
            .into_any_element()
    }

    fn render_commands(
        &self,
        turn: usize,
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
                let key = (turn, part, i);
                let id = |what: &str| SharedString::from(format!("{what}-{turn}-{part}-{i}"));
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
                                    small_button(id("stop"), "Stop", true, p).on_click(
                                        cx.listener(move |this, _, _, cx| this.stop(key, cx)),
                                    )
                                } else {
                                    let label = if c.is_destructive() { "Run…" } else { "Run" };
                                    small_button(id("run"), label, !busy, p).on_click(cx.listener(
                                        move |this, _, window, cx| this.run(key, window, cx),
                                    ))
                                })
                            })
                            .child(small_button(id("copy"), "Copy", true, p).on_click(
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(line.clone()))
                                },
                            )),
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
                    .when(running, |d| d.child(ui::loading_bar(id("busy"), p)))
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

    fn render_turn(
        &self,
        ix: usize,
        turn: &Turn,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let order = std::cell::Cell::new(ix as u64 * 10_000);
        let next = || {
            order.set(order.get() + 1);
            order.get()
        };
        let parts: Vec<AnyElement> = turn
            .parts
            .iter()
            .enumerate()
            .map(|(i, part)| match part {
                Part::Text(t) => div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(rpx(5.))
                    .text_size(ts::BODY)
                    .text_color(p.fg)
                    .children(
                        command_line::prose_blocks(t)
                            .into_iter()
                            .enumerate()
                            .map(|(b, block)| prose(block, format!("t{ix}-{i}-{b}"), next(), p))
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
                    .whitespace_normal()
                    .child(
                        gpui_kit::base::SelectableText::new(
                            SharedString::from(format!("c{ix}-{i}")),
                            code.clone(),
                        )
                        .document_order(next()),
                    )
                    .into_any_element(),
                Part::Commands(cmds) => self.render_commands(ix, i, cmds, p, cx),
            })
            .collect();
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(rpx(8.))
            .child(
                div()
                    .flex_none()
                    .self_end()
                    .max_w(gpui_kit::relative(0.9))
                    .px(rpx(9.))
                    .py(rpx(6.))
                    .rounded(px(8.))
                    .bg(p.sel)
                    .text_size(ts::BODY)
                    .text_color(p.fg)
                    .whitespace_normal()
                    .child(turn.asked.clone()),
            )
            .when(!turn.tools.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(rpx(2.))
                        .children(turn.tools.iter().map(|t| render_tool(t, p))),
                )
            })
            .when(turn.running && turn.text.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(ts::SMALL)
                        .text_color(p.fg3)
                        .child("Reading the repository…"),
                )
            })
            .children(parts)
            .when_some(turn.error.clone(), |d, e| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(ts::SMALL)
                        .text_color(if e == "Stopped." { p.fg3 } else { p.deleted })
                        .whitespace_normal()
                        .child(e),
                )
            })
            .into_any_element()
    }

    fn render_conversation(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let turns: Vec<AnyElement> = self
            .turns
            .iter()
            .enumerate()
            .map(|(i, t)| self.render_turn(i, t, p, cx))
            .collect();
        div()
            .id("asst-conversation")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(rpx(10.))
            .flex()
            .flex_col()
            .gap(rpx(14.))
            .when(self.turns.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(rpx(6.))
                        .text_size(ts::SMALL)
                        .text_color(p.fg3)
                        .child(div().whitespace_normal().child(
                            "Ask about the repository or describe what you want to do. \
                                 The assistant reads the repository and suggests git commands; \
                                 nothing runs until you click Run.",
                        ))
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
            .children(turns)
            .into_any_element()
    }
}

/// A tool call's arguments in one short line: `rev: HEAD~1, max: 15`.
fn tool_detail(args: &serde_json::Value) -> String {
    let Some(map) = args.as_object() else {
        return String::new();
    };
    let text = map
        .iter()
        .map(|(k, v)| match v {
            serde_json::Value::String(s) => format!("{k}: {s}"),
            v => format!("{k}: {v}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    ellipsis(&text, 60)
}

fn ellipsis(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

fn render_tool(t: &Tool, p: &Palette) -> AnyElement {
    let (mark, color) = match t.state {
        ToolState::Running => ("…", p.fg3),
        ToolState::Ok => ("✓", p.added),
        ToolState::Failed => ("✕", p.deleted),
    };
    div()
        .flex()
        .items_center()
        .gap(rpx(6.))
        .min_w_0()
        .text_size(ts::CAPTION_PLUS)
        .text_color(p.fg3)
        .child(div().flex_none().w(rpx(10.)).text_color(color).child(mark))
        .child(
            div()
                .flex_none()
                .font_family(MONO)
                .text_color(p.fg2)
                .child(t.name.clone()),
        )
        .child(div().min_w_0().truncate().child(t.detail.clone()))
        .into_any_element()
}

/// One selectable run of styled prose.
fn rich(id: String, text: Inline, order: u64, p: &Palette) -> RichText {
    use gpui_kit::{FontStyle, HighlightStyle, UnderlineStyle};
    let mut highlights = Vec::new();
    let mut fonts = Vec::new();
    let mut links = Vec::new();
    for (range, m) in text.spans {
        let mut h = HighlightStyle::default();
        if m.bold {
            h.font_weight = Some(FontWeight::BOLD);
        }
        if m.italic {
            h.font_style = Some(FontStyle::Italic);
        }
        if m.code {
            h.background_color = Some(p.panel);
            fonts.push((range.clone(), SharedString::from(MONO)));
        }
        if let Some(url) = m.link {
            h.color = Some(p.acc);
            h.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(p.acc),
                wavy: false,
            });
            links.push((range.clone(), url));
        }
        highlights.push((range, h));
    }
    RichText::new(SharedString::from(id), text.text, highlights, fonts, links).document_order(order)
}

/// A paragraph, heading or list item.
fn prose(block: Prose, id: String, order: u64, p: &Palette) -> AnyElement {
    match block {
        Prose::Paragraph(text) => div()
            .whitespace_normal()
            .child(rich(id, text, order, p))
            .into_any_element(),
        Prose::Heading(text) => div()
            .pt(rpx(2.))
            .font_weight(FontWeight::SEMIBOLD)
            .whitespace_normal()
            .child(rich(id, text, order, p))
            .into_any_element(),
        Prose::Item {
            marker,
            depth,
            text,
        } => div()
            .flex()
            .gap(rpx(6.))
            .pl(rpx(2. + 14. * depth as f32))
            .child(
                div()
                    .flex_none()
                    .min_w(rpx(10.))
                    .text_color(p.fg2)
                    .child(marker),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .child(rich(id, text, order, p)),
            )
            .into_any_element(),
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

impl Render for AssistantPanel {
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
                d.child(ui::loading_bar("asst-busy", &p))
            })
            .child(self.render_conversation(&p, cx))
            .child(self.render_input(&p, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_tool_arguments() {
        let v = serde_json::json!({"mode": "staged"});
        assert_eq!(tool_detail(&v), "mode: staged");
        assert_eq!(tool_detail(&serde_json::json!({"max": 15})), "max: 15");
        let long = serde_json::json!({ "path": "a".repeat(80) });
        assert_eq!(tool_detail(&long).chars().count(), 61);
        assert_eq!(tool_detail(&serde_json::json!(null)), "");
    }
}
