//! Settings → Assistant: which coding CLI the assistant uses, each CLI's program, model
//! and extra arguments, and the custom CLI (command template, output mapping, MCP config).
//! Ported from Switchyard's page; saving updates the [`AssistantSettings`] global, which
//! the workspace writes to the app state.

use std::collections::HashMap;

use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use switchhead_agents::AgentKind;
use switchhead_agents::custom::{CustomCli, JsonlMapping, McpConfigTemplate, OutputFormat};

use crate::appearance::{rpx, ts};
use crate::assistant::{AssistantSettings, CliSettings};
use crate::theme::{MONO, Palette, palette};
use crate::ui::{self, Kind};

/// The CLIs in display order.
pub const AGENTS: [AgentKind; 4] = [
    AgentKind::ClaudeCode,
    AgentKind::Codex,
    AgentKind::Gemini,
    AgentKind::Custom,
];

/// Split a command line into words (double and single quotes group, backslash escapes).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quote, mut any, mut chars) = (None::<char>, false, s.chars());
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, ' ' | '\t' | '\n') => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (Some(q), c) if c == q => quote = None,
            (q, '\\') if q != Some('\'') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    any = true;
                }
            }
            (_, c) => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// Words back into a command line, quoting where needed.
pub fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if !a.is_empty() && !a.chars().any(|c| c.is_whitespace() || "\"'\\".contains(c)) {
                a.clone()
            } else {
                format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the page reports to the workspace.
pub enum AssistantSettingsEvent {
    /// The close button.
    Close,
}

struct CliInputs {
    path: Entity<InputState>,
    model: Entity<InputState>,
    extra: Entity<InputState>,
}

/// The settings page.
pub struct AssistantSettingsView {
    /// `None`: the first CLI installed.
    agent: Option<AgentKind>,
    clis: HashMap<AgentKind, CliInputs>,
    custom_name: Entity<InputState>,
    custom_program: Entity<InputState>,
    custom_args: Entity<InputState>,
    custom_resume: Entity<InputState>,
    custom_interactive: Entity<InputState>,
    prompt_on_stdin: bool,
    jsonl: bool,
    mapping: Vec<(&'static str, &'static str, Entity<InputState>)>,
    mcp_file: Entity<InputState>,
    mcp_template: Entity<TextareaState>,
    /// Where each CLI was found (`None`: not found), once looked up.
    found: Option<HashMap<AgentKind, Option<String>>>,
    saved: bool,
}

impl EventEmitter<AssistantSettingsEvent> for AssistantSettingsView {}

fn input(
    window: &mut Window,
    cx: &mut Context<AssistantSettingsView>,
    value: &str,
    placeholder: &str,
) -> Entity<InputState> {
    let (v, ph) = (value.to_owned(), placeholder.to_owned());
    cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v))
}

impl AssistantSettingsView {
    /// A view of the current settings.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let s = cx.global::<AssistantSettings>().clone();
        let mut clis = HashMap::new();
        for kind in AGENTS {
            let c = s.cli(kind);
            let program_hint = match kind {
                AgentKind::ClaudeCode => "claude (from PATH)",
                AgentKind::Codex => "codex (from PATH)",
                AgentKind::Gemini => "gemini (from PATH)",
                AgentKind::Custom => "",
            };
            clis.insert(
                kind,
                CliInputs {
                    path: input(window, cx, &c.path, program_hint),
                    model: input(window, cx, &c.model, "CLI default"),
                    extra: input(window, cx, &join_args(&c.extra_args), "none"),
                },
            );
        }
        let m = match &s.custom.output {
            OutputFormat::Jsonl(m) => (**m).clone(),
            OutputFormat::Text => JsonlMapping::default(),
        };
        let mapping_fields: [(&'static str, &'static str, &str); 13] = [
            ("type_field", "Type field", &m.type_field),
            ("text_type", "Text: type", &m.text_type),
            ("text_field", "Text: field", &m.text_field),
            ("tool_type", "Tool call: type", &m.tool_type),
            ("tool_name_field", "Tool call: name", &m.tool_name_field),
            (
                "tool_args_field",
                "Tool call: arguments",
                &m.tool_args_field,
            ),
            ("tool_id_field", "Tool call: id", &m.tool_id_field),
            ("result_type", "Tool result: type", &m.result_type),
            ("result_field", "Tool result: text", &m.result_field),
            ("done_type", "Done: type", &m.done_type),
            ("session_field", "Session id field", &m.session_field),
            ("error_type", "Error: type", &m.error_type),
            ("error_field", "Error: message", &m.error_field),
        ];
        let mapping = mapping_fields
            .into_iter()
            .map(|(k, label, v)| {
                let ph = if k.ends_with("field") {
                    "/json/pointer"
                } else {
                    "event type"
                };
                (k, label, input(window, cx, v, ph))
            })
            .collect();
        let tpl = s.custom.mcp_config.clone().unwrap_or_default();
        let template = tpl.template.clone();
        let mcp_template = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("{\"mcpServers\":{\"{server}\":{\"command\":{command_json},\"args\":{args_json},\"env\":{env_json}}}}")
                .default_value(template)
        });
        let this = Self {
            agent: s.agent,
            clis,
            custom_name: input(window, cx, &s.custom.name, "My CLI"),
            custom_program: input(window, cx, &s.custom.program, "mycli"),
            custom_args: input(window, cx, &join_args(&s.custom.args), "--print {prompt}"),
            custom_resume: input(
                window,
                cx,
                &join_args(&s.custom.resume_args),
                "--resume {session}",
            ),
            custom_interactive: input(window, cx, &join_args(&s.custom.interactive_args), "none"),
            prompt_on_stdin: s.custom.prompt_on_stdin,
            jsonl: matches!(s.custom.output, OutputFormat::Jsonl(_)),
            mapping,
            mcp_file: input(window, cx, &tpl.file, "mcp.json"),
            mcp_template,
            found: None,
            saved: false,
        };
        this.look_up(s, cx);
        this
    }

    /// Find each CLI's executable off the UI thread.
    fn look_up(&self, s: AssistantSettings, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move {
            AGENTS
                .into_iter()
                .map(|k| (k, s.program(k).map(|p| p.display().to_string())))
                .collect::<HashMap<_, _>>()
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |v, cx| {
                v.found = Some(found);
                cx.notify();
            });
        })
        .detach();
    }

    fn value(e: &Entity<InputState>, cx: &Context<Self>) -> String {
        e.read(cx).value().trim().to_owned()
    }

    /// The settings as entered, over `base` (fields this page does not show).
    pub fn settings(&self, base: &AssistantSettings, cx: &Context<Self>) -> AssistantSettings {
        let mut s = base.clone();
        s.agent = self.agent;
        for kind in AGENTS {
            let i = &self.clis[&kind];
            *s.cli_mut(kind) = CliSettings {
                path: Self::value(&i.path, cx),
                model: Self::value(&i.model, cx),
                extra_args: split_args(&Self::value(&i.extra, cx)),
            };
        }
        let get = |k: &str| {
            self.mapping
                .iter()
                .find(|(key, _, _)| *key == k)
                .map(|(_, _, e)| Self::value(e, cx))
                .unwrap_or_default()
        };
        let file = Self::value(&self.mcp_file, cx);
        let template = self.mcp_template.read(cx).value().to_string();
        s.custom = CustomCli {
            name: Self::value(&self.custom_name, cx),
            program: Self::value(&self.custom_program, cx),
            args: split_args(&Self::value(&self.custom_args, cx)),
            resume_args: split_args(&Self::value(&self.custom_resume, cx)),
            interactive_args: split_args(&Self::value(&self.custom_interactive, cx)),
            prompt_on_stdin: self.prompt_on_stdin,
            output: if self.jsonl {
                OutputFormat::Jsonl(Box::new(JsonlMapping {
                    type_field: get("type_field"),
                    text_type: get("text_type"),
                    text_field: get("text_field"),
                    tool_type: get("tool_type"),
                    tool_name_field: get("tool_name_field"),
                    tool_args_field: get("tool_args_field"),
                    tool_id_field: get("tool_id_field"),
                    result_type: get("result_type"),
                    result_field: get("result_field"),
                    done_type: get("done_type"),
                    session_field: get("session_field"),
                    error_type: get("error_type"),
                    error_field: get("error_field"),
                }))
            } else {
                OutputFormat::Text
            },
            mcp_config: (!file.is_empty() || !template.trim().is_empty())
                .then_some(McpConfigTemplate { file, template }),
        };
        s
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let s = self.settings(cx.global::<AssistantSettings>(), cx);
        self.look_up(s.clone(), cx);
        cx.set_global(s);
        self.saved = true;
        cx.notify();
    }

    fn status(&self, kind: AgentKind) -> String {
        match self.found.as_ref().map(|f| f.get(&kind)) {
            None => "Checking…".into(),
            Some(Some(Some(path))) => format!("Found: {path}"),
            Some(_) if kind == AgentKind::Custom => "Your command".into(),
            Some(_) => "Not found".into(),
        }
    }
}

fn field(label: &str, e: &Entity<InputState>, width: f32, p: &Palette) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(rpx(4.))
        .w(rpx(width))
        .child(
            div()
                .text_size(ts::SMALL)
                .text_color(p.fg3)
                .child(label.to_owned()),
        )
        .child(
            div()
                .h(rpx(28.))
                .flex()
                .items_center()
                .px(rpx(8.))
                .border_1()
                .border_color(p.bd2)
                .rounded(px(6.))
                .bg(p.bg)
                .font_family(MONO)
                .text_size(ts::BODY)
                .child(Input::new(e).appearance(false).text_size(ts::BODY)),
        )
}

fn heading(text: &str, p: &Palette) -> impl IntoElement {
    div()
        .text_size(ts::BODY)
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.fg2)
        .child(text.to_owned())
}

fn note(text: &'static str, p: &Palette) -> impl IntoElement {
    div()
        .text_size(ts::SMALL)
        .text_color(p.fg3)
        .whitespace_normal()
        .child(text)
}

impl Render for AssistantSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let choice = |id: &str, on: bool, title: String, status: String| {
            div()
                .id(SharedString::from(format!("asst-agent-{id}")))
                .flex()
                .flex_col()
                .gap(rpx(2.))
                .flex_1()
                .min_w_0()
                .px(rpx(10.))
                .py(rpx(7.))
                .border_1()
                .border_color(if on { p.acc } else { p.bd2 })
                .rounded(px(6.))
                .bg(if on { p.sel } else { p.bg })
                .cursor_pointer()
                .child(div().text_size(ts::UI).child(title))
                .child(
                    div()
                        .text_size(ts::SMALL)
                        .text_color(p.fg3)
                        .truncate()
                        .child(status),
                )
        };
        let auto = {
            let first = self.found.as_ref().and_then(|f| {
                AGENTS[..3]
                    .iter()
                    .find(|k| matches!(f.get(k), Some(Some(_))))
                    .map(|k| k.display_name().to_owned())
            });
            let status = match (&self.found, first) {
                (None, _) => "Checking…".to_owned(),
                (_, Some(name)) => format!("Uses {name}"),
                (_, None) => "None installed".to_owned(),
            };
            choice("auto", self.agent.is_none(), "Automatic".into(), status).on_click(cx.listener(
                |this, _, _, cx| {
                    this.agent = None;
                    this.saved = false;
                    cx.notify();
                },
            ))
        };
        let picker = div()
            .flex()
            .gap(rpx(6.))
            .child(auto)
            .children(AGENTS.map(|k| {
                choice(
                    k.id(),
                    self.agent == Some(k),
                    k.display_name().to_owned(),
                    self.status(k),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.agent = Some(k);
                    this.saved = false;
                    cx.notify();
                }))
            }));
        let cli_rows = AGENTS[..3].iter().map(|k| {
            let i = &self.clis[k];
            div()
                .flex()
                .flex_col()
                .gap(rpx(6.))
                .child(heading(k.display_name(), &p))
                .child(
                    div()
                        .flex()
                        .gap(rpx(10.))
                        .child(field("Program", &i.path, 220., &p))
                        .child(field("Model", &i.model, 150., &p))
                        .child(field("Extra arguments", &i.extra, 220., &p)),
                )
        });
        let custom = &self.clis[&AgentKind::Custom];
        let custom_section = div()
            .flex()
            .flex_col()
            .gap(rpx(8.))
            .child(heading("Custom CLI", &p))
            .child(note(
                "Placeholders: {prompt} {model} {session} {workdir} {mcp_config} {system_prompt}; in the MCP template also {command_json} {args_json} {env_json} {env_names_json} {server}.",
                &p,
            ))
            .child(
                div()
                    .flex()
                    .gap(rpx(10.))
                    .child(field("Name", &self.custom_name, 150., &p))
                    .child(field("Program", &self.custom_program, 200., &p))
                    .child(field("Model", &custom.model, 120., &p))
                    .child(field("Extra arguments", &custom.extra, 140., &p)),
            )
            .child(
                div()
                    .flex()
                    .gap(rpx(10.))
                    .child(field("Arguments", &self.custom_args, 300., &p))
                    .child(field("When continuing", &self.custom_resume, 150., &p))
                    .child(field("Interactive (terminal)", &self.custom_interactive, 170., &p)),
            )
            .child(
                div()
                    .flex()
                    .gap(rpx(16.))
                    .child(
                        ui::checkbox("asst-stdin", self.prompt_on_stdin, "Prompt on standard input", &p)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prompt_on_stdin = !this.prompt_on_stdin;
                                this.saved = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::checkbox("asst-jsonl", self.jsonl, "Output is JSON lines (map fields below)", &p)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.jsonl = !this.jsonl;
                                this.saved = false;
                                cx.notify();
                            })),
                    ),
            )
            .when(self.jsonl, |d| {
                d.child(
                    div().flex().flex_wrap().gap(rpx(10.)).children(
                        self.mapping
                            .iter()
                            .map(|(_, label, e)| field(label, e, 150., &p)),
                    ),
                )
            })
            .child(
                div()
                    .flex()
                    .gap(rpx(10.))
                    .items_start()
                    .child(field("MCP config file", &self.mcp_file, 150., &p))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rpx(4.))
                            .flex_1()
                            .child(div().text_size(ts::SMALL).text_color(p.fg3).child("MCP config template"))
                            .child(
                                div()
                                    .px(rpx(8.))
                                    .py(rpx(5.))
                                    .border_1()
                                    .border_color(p.bd2)
                                    .rounded(px(6.))
                                    .bg(p.bg)
                                    .font_family(MONO)
                                    .text_size(ts::BODY)
                                    .child(Textarea::new(&self.mcp_template).appearance(false).text_size(ts::BODY)),
                            ),
                    ),
            );
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
                            .child("Assistant"),
                    )
                    .child(
                        ui::button("asst-close", "Close", Kind::Secondary, &p).on_click(
                            cx.listener(|_, _, _, cx| cx.emit(AssistantSettingsEvent::Close)),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rpx(6.))
                    .child(heading("Coding CLI", &p))
                    .child(note(
                        "Each CLI uses its own sign-in. It runs in a private empty folder, and its only tools are switchhead's read-only git tools for the repository you ask about.",
                        &p,
                    ))
                    .child(picker),
            )
            .children(cli_rows)
            .child(custom_section)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rpx(10.))
                    .child(
                        ui::button("asst-save", "Save", Kind::Primary, &p)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )
                    .when(self.saved, |d| {
                        d.child(div().text_size(ts::BODY).text_color(p.added).child("Saved"))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_round_trip() {
        let a = split_args(r#"--print "{prompt}" --flag='a b' x\ y"#);
        assert_eq!(a, ["--print", "{prompt}", "--flag=a b", "x y"]);
        let v = vec![
            "--tools".to_owned(),
            String::new(),
            "a b".into(),
            "q\"".into(),
        ];
        assert_eq!(split_args(&join_args(&v)), v);
        assert!(split_args("   ").is_empty());
    }
}
