//! The merge helper for one conflicted file: a card per conflict (ours, base, theirs,
//! with a button per resolution) above an editable result, plus whole-file choices, the
//! external merge tool and Mark Resolved.
//!
//! The result editor is the source of truth. Its text is re-parsed after every change,
//! so the cards always show the conflicts still marked in it, and picking a side
//! rewrites only that conflict, keeping edits made elsewhere by hand.
//!
//! Resolve with AI asks the coding CLI for the whole resolved file and shows it as a diff
//! against the result; Use Proposal puts it in the editor, where it can still be edited
//! before Mark Resolved. Nothing is written until then.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, UniformListScrollHandle, Window,
    div, px,
};
use switchhead_agents::CancelHandle;
use switchhead_git::Repo;
use switchhead_git::conflict::{self, Choice, ConflictFile};
use switchhead_git::status::Conflict as Kind;

use crate::appearance::{rpx, ts};
use crate::changes_view::{first_line, small_button};
use crate::diff_view::{self, Content, highlight_theme, theme_key};
use crate::store::DiffPrefs;
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// What the merge helper reports to Local Changes.
pub enum MergeEvent {
    /// The file was resolved (staged or deleted): status changed.
    Resolved,
}

/// What was read from the work tree.
enum Loaded {
    /// Still reading.
    Reading,
    /// Text with (or without) conflict markers, now in the editor.
    Text,
    /// A file that is not text, or not in the work tree: whole-file choices only.
    WholeFile(String),
}

/// A resolved file proposed by the assistant, not yet in the editor.
struct Proposal {
    text: String,
    /// What the assistant said about it.
    note: String,
    /// The proposal as a diff against the result it was asked for.
    diff: Rc<diff_view::Loaded>,
}

/// The merge helper for one conflicted path.
pub struct MergeView {
    repo: Repo,
    /// The conflicted path.
    pub path: PathBuf,
    kind: Kind,
    result: Entity<EditorState>,
    /// The conflicts still marked in the result.
    remaining: Option<ConflictFile>,
    loaded: Loaded,
    tool: Option<String>,
    busy: bool,
    error: Option<SharedString>,
    /// A Resolve with AI run in progress.
    resolving: Option<CancelHandle>,
    proposal: Option<Proposal>,
    proposal_scroll: UniformListScrollHandle,
    _sub: Subscription,
}

impl Drop for MergeView {
    fn drop(&mut self) {
        if let Some(c) = self.resolving.take() {
            c.cancel();
        }
    }
}

impl EventEmitter<MergeEvent> for MergeView {}

impl MergeView {
    /// The helper for `path`; the file is read in the background.
    pub fn new(
        repo: Repo,
        path: PathBuf,
        kind: Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = crate::diff_model::language_for(&path).unwrap_or("text");
        let result = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(lang)
                .line_number(true)
                .soft_wrap(false)
        });
        let sub = cx.subscribe(&result, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.reparse(cx);
            }
        });
        let mut this = Self {
            repo,
            path,
            kind,
            result,
            remaining: None,
            loaded: Loaded::Reading,
            tool: None,
            busy: false,
            error: None,
            resolving: None,
            proposal: None,
            proposal_scroll: UniformListScrollHandle::new(),
            _sub: sub,
        };
        this.load(window, cx);
        this
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        let full = repo.root().join(&self.path);
        let chosen = external_merge_tool(cx);
        let task = cx.background_executor().spawn(async move {
            let tool = chosen.or_else(|| repo.merge_tool());
            let bytes = std::fs::read(&full);
            (tool, bytes)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (tool, bytes) = task.await;
            let _ = this.update_in(cx, |v, window, cx| {
                v.tool = tool;
                v.loaded = match bytes {
                    // One side deleted the file: keep it or delete it, nothing to merge.
                    _ if is_delete(v.kind) => Loaded::WholeFile(delete_hint(v.kind).into()),
                    Ok(b) if !b.contains(&0) => match String::from_utf8(b) {
                        Ok(text) => {
                            v.result.update(cx, |e, cx| e.set_value(text, window, cx));
                            v.reparse(cx);
                            Loaded::Text
                        }
                        Err(_) => Loaded::WholeFile("The file is not UTF-8 text.".into()),
                    },
                    Ok(_) => Loaded::WholeFile("Binary file.".into()),
                    Err(_) => Loaded::WholeFile(missing_note(v.kind).into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn text(&self, cx: &Context<Self>) -> String {
        self.result.read(cx).value().to_string()
    }

    fn reparse(&mut self, cx: &mut Context<Self>) {
        self.remaining = conflict::parse(&self.text(cx)).ok();
        cx.notify();
    }

    /// Conflicts still marked in the result (`None`: the markers do not parse).
    fn left(&self) -> Option<usize> {
        self.remaining.as_ref().map(ConflictFile::count)
    }

    fn choose(&mut self, i: usize, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = &self.remaining else {
            return;
        };
        let mut choices = vec![None; file.count()];
        if let Some(c) = choices.get_mut(i) {
            *c = Some(choice);
        }
        let text = file.resolve(&choices);
        self.result
            .update(cx, |e, cx| e.set_value(text, window, cx));
        self.reparse(cx);
    }

    fn choose_all(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = &self.remaining else {
            return;
        };
        let text = file.resolve_all(choice);
        self.result
            .update(cx, |e, cx| e.set_value(text, window, cx));
        self.reparse(cx);
    }

    /// Ask the coding CLI for the resolved file; a second click stops it.
    fn resolve_with_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = self.resolving.take() {
            c.cancel();
            cx.notify();
            return;
        }
        if self.busy {
            return;
        }
        let settings = cx.global::<crate::assistant::AssistantSettings>().clone();
        let prompt = crate::assistant::conflict_prompt(&self.path.to_string_lossy());
        let (cancel, rx) = match crate::assistant::spawn(&settings, self.repo.root(), prompt) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(e.into());
                cx.notify();
                return;
            }
        };
        self.resolving = Some(cancel);
        self.proposal = None;
        self.error = None;
        let path = self.path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let res = rx
                .await
                .unwrap_or_else(|_| Err("The assistant stopped.".into()));
            let Ok(current) = this.update(cx, |v, cx| v.text(cx)) else {
                return;
            };
            let (prefs, theme) =
                match this.update(cx, |_, cx| (*cx.global::<DiffPrefs>(), highlight_theme(cx))) {
                    Ok(t) => t,
                    Err(_) => return,
                };
            let proposal = res.and_then(|answer| check_proposal(&answer));
            let proposal = match proposal {
                Ok((note, text)) => {
                    let key = theme_key(&theme);
                    let (old, new) = (current, text.clone());
                    let content = cx
                        .background_executor()
                        .spawn(async move {
                            Content::from_bytes(
                                &path,
                                Some(old.as_bytes()),
                                Some(new.as_bytes()),
                                &prefs,
                                &theme,
                            )
                        })
                        .await;
                    Ok(Proposal {
                        text,
                        note,
                        diff: Rc::new(diff_view::Loaded {
                            file: 0,
                            prefs,
                            theme: key,
                            content,
                        }),
                    })
                }
                Err(e) => Err(e),
            };
            let _ = this.update(cx, |v, cx| {
                // `None`: stopped from the button.
                if v.resolving.take().is_some() {
                    match proposal {
                        Ok(p) => v.proposal = Some(p),
                        Err(e) => v.error = Some(first_line(e.trim()).into()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Put the proposal in the editor, to edit or mark resolved.
    fn use_proposal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.proposal.take() else {
            return;
        };
        self.result
            .update(cx, |e, cx| e.set_value(p.text, window, cx));
        self.reparse(cx);
    }

    fn discard_proposal(&mut self, cx: &mut Context<Self>) {
        self.proposal = None;
        cx.notify();
    }

    /// Run `f` in the background; on success the file counts as resolved.
    fn run(
        &mut self,
        f: impl FnOnce(&Repo) -> switchhead_git::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move { f(&repo) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match res {
                    Ok(()) => cx.emit(MergeEvent::Resolved),
                    Err(e) => v.error = Some(first_line(&e.to_string()).into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Write the result and stage it. Asks first while markers are left.
    fn mark_resolved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let path = self.path.clone();
        let save = move |v: &mut Self, cx: &mut Context<Self>| {
            let (text, path) = (text.clone(), path.clone());
            v.run(
                move |r| {
                    std::fs::write(r.root().join(&path), text.as_bytes())?;
                    r.mark_resolved(&[path.as_path()])
                },
                cx,
            );
        };
        let left = self.left();
        if left == Some(0) {
            return save(self, cx);
        }
        let what = match left {
            Some(1) => "1 conflict is still marked".to_owned(),
            Some(n) => format!("{n} conflicts are still marked"),
            None => "The conflict markers are incomplete".to_owned(),
        };
        let this = cx.entity().downgrade();
        let save = Rc::new(save);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, save) = (this.clone(), save.clone());
            alert
                .confirm()
                .title("Mark resolved with markers left?")
                .description(format!(
                    "{what} in the result. Staging it commits the markers as text."
                ))
                .ok_text("Mark Resolved")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let _ = this.update(cx, |v, cx| save(v, cx));
                    true
                })
        });
    }

    /// Take one side of the whole file (binary files, delete conflicts).
    fn take_side(&mut self, theirs: bool, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self.run(
            move |r| {
                r.checkout_side(&[path.as_path()], theirs)?;
                r.mark_resolved(&[path.as_path()])
            },
            cx,
        );
    }

    fn keep_file(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self.run(move |r| r.mark_resolved(&[path.as_path()]), cx);
    }

    fn delete_file(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self.run(move |r| r.resolve_by_deleting(&[path.as_path()]), cx);
    }

    fn open_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let repo = self.repo.clone();
        let path = self.path.clone();
        let tool = external_merge_tool(cx);
        let task = cx
            .background_executor()
            .spawn(async move { repo.run_merge_tool(tool.as_deref(), &path) });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            let _ = this.update_in(cx, |v, window, cx| {
                v.busy = false;
                match res {
                    // git stages the file when the tool reports success.
                    Ok(()) => cx.emit(MergeEvent::Resolved),
                    Err(e) => {
                        v.error = Some(first_line(&e.to_string()).into());
                        v.load(window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let text = matches!(self.loaded, Loaded::Text);
        let status: SharedString = match (&self.loaded, self.left()) {
            (Loaded::Reading, _) => "".into(),
            (Loaded::WholeFile(_), _) => kind_label(self.kind).into(),
            (Loaded::Text, Some(0)) => "No conflicts left".into(),
            (Loaded::Text, Some(1)) => "1 conflict left".into(),
            (Loaded::Text, Some(n)) => format!("{n} conflicts left").into(),
            (Loaded::Text, None) => "Markers incomplete".into(),
        };
        let left = self.left().unwrap_or(0);
        let enabled = !self.busy;
        let resolving = self.resolving.is_some();
        div()
            .flex_none()
            .h(rpx(34.))
            .px(rpx(12.))
            .flex()
            .items_center()
            .gap(rpx(6.))
            .bg(p.panel)
            .border_b_1()
            .border_color(p.bd)
            .child(ui::chip("Conflict", p.deleted))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .text_size(ts::BODY)
                    .text_color(p.fg)
                    .child(self.path.to_string_lossy().into_owned()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ts::SMALL)
                    .text_color(if left == 0 && text { p.added } else { p.fg3 })
                    .child(status),
            )
            .when(
                text && left > 0 && !resolving && self.proposal.is_none(),
                |d| {
                    d.child(small_button("all-ours", "All Ours", enabled, p).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.choose_all(Choice::Ours, window, cx)
                        }),
                    ))
                    .child(
                        small_button("all-theirs", "All Theirs", enabled, p).on_click(cx.listener(
                            |this, _, window, cx| this.choose_all(Choice::Theirs, window, cx),
                        )),
                    )
                },
            )
            .when(
                text && self.proposal.is_none() && (left > 0 || self.resolving.is_some()),
                |d| {
                    let running = self.resolving.is_some();
                    d.child(
                        small_button(
                            "resolve-ai",
                            if running { "Stop" } else { "Resolve with AI" },
                            enabled,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.resolve_with_ai(window, cx)),
                        ),
                    )
                },
            )
            .when_some(self.tool.clone(), |d, tool| {
                d.child(
                    small_button("merge-tool", format!("Open in {tool}"), enabled, p)
                        .on_click(cx.listener(|this, _, window, cx| this.open_tool(window, cx))),
                )
            })
            .when(text, |d| {
                d.child(
                    ui::button("mark-resolved", "Mark Resolved", ui::Kind::Primary, p)
                        .h(rpx(22.))
                        .when(enabled, |d| d.cursor_pointer())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.mark_resolved(window, cx)),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_card(
        &self,
        i: usize,
        c: &conflict::Conflict,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let column = |title: String, body: &str, color: Hsla| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .border_1()
                .border_color(p.bd)
                .rounded(px(4.))
                .overflow_hidden()
                .child(
                    div()
                        .px(rpx(8.))
                        .py(rpx(3.))
                        .bg(color.opacity(0.12))
                        .text_size(ts::SMALL)
                        .text_color(color)
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .px(rpx(8.))
                        .py(rpx(4.))
                        .font_family(MONO)
                        .text_size(ts::SMALL)
                        .text_color(p.fg)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .children(
                            body.lines()
                                .take(MAX_CARD_LINES)
                                .map(|l| div().h(rpx(17.)).child(card_line(l))),
                        )
                        .when(body.lines().count() > MAX_CARD_LINES, |d| {
                            d.child(div().text_color(p.fg3).child(format!(
                                "… {} more lines",
                                body.lines().count() - MAX_CARD_LINES
                            )))
                        })
                        .when(body.is_empty(), |d| {
                            d.child(div().text_color(p.fg3).child("(nothing)"))
                        }),
                )
        };
        let label = |l: &str, fallback: &str| {
            if l.is_empty() {
                fallback.to_owned()
            } else {
                format!("{fallback} · {l}")
            }
        };
        let pick = |id: &'static str, text: &'static str, choice: Choice| {
            small_button((id, i), text, !self.busy, p).on_click(
                cx.listener(move |this, _, window, cx| this.choose(i, choice.clone(), window, cx)),
            )
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(rpx(6.))
            .p(rpx(8.))
            .bg(p.surface)
            .border_1()
            .border_color(p.bd)
            .rounded(px(6.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(rpx(6.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(ts::SMALL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.fg)
                            .child(format!("Conflict {} · line {}", i + 1, c.line)),
                    )
                    .child(pick("ours", "Ours", Choice::Ours))
                    .child(pick("theirs", "Theirs", Choice::Theirs))
                    .child(pick("both", "Ours + Theirs", Choice::OursThenTheirs))
                    .child(pick("both-rev", "Theirs + Ours", Choice::TheirsThenOurs))
                    .when(c.base.is_some(), |d| {
                        d.child(pick("base", "Base", Choice::Base))
                    }),
            )
            .child(
                div()
                    .flex()
                    .gap(rpx(6.))
                    .child(column(label(&c.ours_label, "Ours"), &c.ours, p.added))
                    .when_some(c.base.as_ref(), |d, b| {
                        d.child(column(label(&c.base_label, "Base"), b, p.fg3))
                    })
                    .child(column(
                        label(&c.theirs_label, "Theirs"),
                        &c.theirs,
                        p.modified,
                    )),
            )
            .into_any_element()
    }

    fn render_proposal(&self, prop: &Proposal, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let row_h = diff_view::row_height(cx);
        let unchanged =
            matches!(&prop.diff.content, Content::Text { diff, .. } if diff.hunks().is_empty());
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(rpx(12.))
                    .py(rpx(8.))
                    .flex()
                    .items_start()
                    .gap(rpx(8.))
                    .bg(p.panel)
                    .border_b_1()
                    .border_color(p.bd)
                    .child(
                        div()
                            .id("proposal-note")
                            .flex_1()
                            .min_w_0()
                            .max_h(rpx(120.))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(rpx(2.))
                            .child(
                                div()
                                    .text_size(ts::SMALL)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.fg)
                                    .child("Proposed resolution"),
                            )
                            .child(div().text_size(ts::SMALL).text_color(p.fg2).child(
                                if prop.note.is_empty() {
                                    "The assistant gave no explanation.".to_owned()
                                } else {
                                    prop.note.clone()
                                },
                            )),
                    )
                    .child(
                        ui::button("use-proposal", "Use Proposal", ui::Kind::Primary, p)
                            .h(rpx(22.))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.use_proposal(window, cx)),
                            ),
                    )
                    .child(
                        small_button("discard-proposal", "Discard", true, p)
                            .on_click(cx.listener(|this, _, _, cx| this.discard_proposal(cx))),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px(rpx(12.))
                    .py(rpx(4.))
                    .text_size(ts::SMALL)
                    .text_color(p.fg3)
                    .child(
                        "Changes from the conflicted result. Use Proposal puts it in the editor; \
                         nothing is saved until Mark Resolved.",
                    ),
            )
            .child(if unchanged {
                diff_view::message("The proposal is the same as the result.", p.fg3)
            } else {
                diff_view::body(&prop.diff, &self.proposal_scroll, row_h, p, None, cx)
            })
            .into_any_element()
    }

    fn render_whole_file(&self, note: &str, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let enabled = !self.busy;
        let (keep, delete) = (is_delete(self.kind), is_delete(self.kind));
        let exists = !matches!(self.kind, Kind::BothDeleted);
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(rpx(12.))
            .child(
                div()
                    .text_size(ts::BODY)
                    .text_color(p.fg2)
                    .child(note.to_owned()),
            )
            .child(
                div()
                    .flex()
                    .gap(rpx(8.))
                    .when(!keep, |d| {
                        d.child(
                            small_button("use-ours", "Use Ours", enabled, p)
                                .on_click(cx.listener(|this, _, _, cx| this.take_side(false, cx))),
                        )
                        .child(
                            small_button("use-theirs", "Use Theirs", enabled, p)
                                .on_click(cx.listener(|this, _, _, cx| this.take_side(true, cx))),
                        )
                    })
                    .when(keep && exists, |d| {
                        d.child(
                            small_button("keep", "Keep File", enabled, p)
                                .on_click(cx.listener(|this, _, _, cx| this.keep_file(cx))),
                        )
                    })
                    .when(delete, |d| {
                        d.child(
                            small_button("delete", "Delete File", enabled, p)
                                .on_click(cx.listener(|this, _, _, cx| this.delete_file(cx))),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// The note and resolved file in an assistant's answer; an error when it has no file or
/// conflict markers are left in it.
fn check_proposal(answer: &str) -> Result<(String, String), String> {
    let (note, text) = crate::assistant::fenced_block(answer)
        .ok_or("The assistant's answer has no resolved file.")?;
    let marked = text
        .lines()
        .any(|l| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>"));
    if marked {
        return Err("The assistant's proposal still has conflict markers.".into());
    }
    Ok((note, text))
}

/// Lines shown per side in a conflict card.
const MAX_CARD_LINES: usize = 12;

fn delete_hint(kind: Kind) -> &'static str {
    match kind {
        Kind::DeletedByUs => {
            "Keep File keeps their changed version; Delete File keeps our deletion."
        }
        Kind::DeletedByThem => {
            "Keep File keeps our changed version; Delete File takes their deletion."
        }
        _ => "Delete File removes it.",
    }
}

fn is_delete(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::DeletedByUs | Kind::DeletedByThem | Kind::BothDeleted
    )
}

/// A line for a conflict card, with its indentation kept (spaces would collapse).
pub(crate) fn card_line(l: &str) -> SharedString {
    let indent = l.len() - l.trim_start_matches([' ', '\t']).len();
    let lead: String = l[..indent]
        .chars()
        .map(|c| {
            if c == '\t' {
                "\u{a0}\u{a0}\u{a0}\u{a0}"
            } else {
                "\u{a0}"
            }
        })
        .collect();
    format!("{lead}{}", &l[indent..]).into()
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::BothDeleted => "Deleted on both sides",
        Kind::AddedByUs => "Added by us",
        Kind::DeletedByThem => "Changed by us, deleted by them",
        Kind::AddedByThem => "Added by them",
        Kind::DeletedByUs => "Deleted by us, changed by them",
        Kind::BothAdded => "Added on both sides",
        Kind::BothModified => "Changed on both sides",
    }
}

fn missing_note(kind: Kind) -> &'static str {
    match kind {
        Kind::BothDeleted => "Both sides deleted this file.",
        _ => "The file is not in the work tree.",
    }
}

impl Render for MergeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let body: AnyElement = match &self.loaded {
            Loaded::Reading => div().flex_1().into_any_element(),
            Loaded::WholeFile(note) => {
                let note = format!("{}. {note}", kind_label(self.kind));
                self.render_whole_file(&note, &p, cx)
            }
            Loaded::Text if self.proposal.is_some() => match &self.proposal {
                Some(prop) => self.render_proposal(prop, &p, cx),
                None => div().into_any_element(),
            },
            Loaded::Text => {
                let cards: Vec<AnyElement> = self
                    .remaining
                    .clone()
                    .map(|f| {
                        f.conflicts()
                            .enumerate()
                            .map(|(i, c)| self.render_card(i, c, &p, cx))
                            .collect()
                    })
                    .unwrap_or_default();
                let font = crate::appearance::editor_font_family(cx);
                let size = crate::appearance::editor_font_size(cx);
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(!cards.is_empty(), |d| {
                        d.child(
                            div()
                                .id("conflict-cards")
                                .flex_none()
                                .max_h(gpui_kit::relative(0.55))
                                .overflow_y_scroll()
                                .p(rpx(10.))
                                .flex()
                                .flex_col()
                                .gap(rpx(8.))
                                .border_b_1()
                                .border_color(p.bd)
                                .children(cards),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .px(rpx(12.))
                            .py(rpx(4.))
                            .text_size(ts::SMALL)
                            .text_color(p.fg3)
                            .child("Result (editable; saved to the file by Mark Resolved)"),
                    )
                    .child(
                        div().flex_1().min_h_0().child(
                            Editor::new(&self.result)
                                .bordered(false)
                                .size_full()
                                .font_family(font)
                                .text_size(size),
                        ),
                    )
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(self.render_toolbar(&p, cx))
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    div()
                        .flex_none()
                        .px(rpx(12.))
                        .py(rpx(6.))
                        .bg(p.deleted.opacity(0.12))
                        .text_size(ts::SMALL)
                        .text_color(p.deleted)
                        .child(e),
                )
            })
            .when(self.busy || self.resolving.is_some(), |d| {
                d.child(ui::loading_bar("merge-busy", &p))
            })
            .child(body)
    }
}

/// The merge tool chosen in Settings → External Tools, if any.
fn external_merge_tool(cx: &gpui_kit::App) -> Option<String> {
    cx.try_global::<crate::external::ExternalTools>()
        .and_then(|t| t.merge_tool().map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposals_must_be_resolved() {
        let ok = "Kept both.\n```\na\nb\n```";
        assert_eq!(
            check_proposal(ok),
            Ok(("Kept both.".into(), "a\nb\n".into()))
        );
        let marked = "```\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> x\n```";
        assert!(check_proposal(marked).is_err());
        assert!(check_proposal("I could not resolve it.").is_err());
    }
}
