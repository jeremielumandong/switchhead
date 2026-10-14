//! Quick Launch (Ctrl/Cmd+P): one fuzzy list of commands, repositories and the active
//! repository's branches and tags, narrowed by chips (Tab cycles them) or by a leading
//! `>` for commands. Ported from Switchyard's palette; the items are git's.

use std::path::PathBuf;

use gpui_kit::AsKeystroke as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyElement, AppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use switchhead_git::Oid;

use crate::actions;
use crate::appearance::{rpx, ts};
use crate::theme::{MONO, palette};
use crate::ui;

/// Most rows the list shows; the count says how many matched.
const MAX_ROWS: usize = 200;

/// The palette's filter chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    /// Everything.
    All,
    /// Commands.
    Commands,
    /// Open tabs and recent repositories.
    Repositories,
    /// The active repository's branches and tags.
    Refs,
}

impl Chip {
    const ALL: [Chip; 4] = [Chip::All, Chip::Commands, Chip::Repositories, Chip::Refs];

    fn label(self) -> &'static str {
        match self {
            Chip::All => "All",
            Chip::Commands => "Commands",
            Chip::Repositories => "Repositories",
            Chip::Refs => "Branches & Tags",
        }
    }

    fn next(self) -> Chip {
        let i = Chip::ALL.iter().position(|c| *c == self).unwrap_or(0);
        Chip::ALL[(i + 1) % Chip::ALL.len()]
    }
}

/// The chip in effect: a leading `>` means commands. Returns it and the query to match.
pub fn effective(chip: Chip, query: &str) -> (Chip, &str) {
    match query.strip_prefix('>') {
        Some(rest) => (Chip::Commands, rest.trim()),
        None => (chip, query.trim()),
    }
}

/// Whether the characters of `query` appear in `text` in order (case-insensitive).
pub fn fuzzy(query: &str, text: &str) -> bool {
    let mut q = query.chars().flat_map(char::to_lowercase).peekable();
    for ch in text.chars().flat_map(char::to_lowercase) {
        match q.peek() {
            None => return true,
            Some(c) if *c == ch => {
                q.next();
            }
            Some(_) => {}
        }
    }
    q.peek().is_none()
}

/// Fuzzy score: lower is better; `None` if no match. Prefers contiguous and early matches.
pub fn fuzzy_score(query: &str, text: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    let t = text.to_lowercase();
    if let Some(pos) = t.find(&query.to_lowercase()) {
        return Some(pos);
    }
    fuzzy(query, text).then(|| 100 + t.len())
}

/// A repository the palette can switch to or open.
#[derive(Clone, Debug)]
pub struct RepoItem {
    /// Folder name.
    pub name: String,
    /// Where it is.
    pub path: PathBuf,
    /// Its tab, when open.
    pub tab: Option<usize>,
}

/// A ref of the active repository.
#[derive(Clone, Debug)]
pub struct RefItem {
    /// Short name (`main`, `origin/main`, `v1.0`).
    pub name: String,
    /// "Branch", "Remote branch" or "Tag".
    pub kind: &'static str,
    /// Its commit.
    pub id: Oid,
}

/// What the palette lists besides commands.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    /// A repository tab is active (repository commands apply).
    pub has_repo: bool,
    /// Open tabs first, then recent repositories.
    pub repos: Vec<RepoItem>,
    /// The active repository's refs.
    pub refs: Vec<RefItem>,
}

/// What the user picked.
pub enum PaletteEvent {
    /// Run a command.
    Run(Box<dyn Action>),
    /// Switch to an open tab.
    SwitchTab(usize),
    /// Open a repository.
    Open(PathBuf),
    /// Select a commit in the active repository's graph.
    Reveal(Oid),
    /// Closed without a choice.
    Dismiss,
}

#[derive(Clone)]
enum Target {
    Command(usize),
    Repo(usize),
    Ref(usize),
}

struct Item {
    badge: SharedString,
    label: SharedString,
    group: SharedString,
    key: SharedString,
    target: Target,
}

/// The Quick Launch view.
pub struct PaletteView {
    chip: Chip,
    input: Entity<InputState>,
    selected: usize,
    commands: Vec<actions::Command>,
    /// Shortcut text per command, read once when the palette opens.
    keys: Vec<SharedString>,
    sources: Sources,
    _sub: Subscription,
}

impl EventEmitter<PaletteEvent> for PaletteView {}

impl PaletteView {
    /// A palette over `sources`, filtered by `chip`.
    pub fn new(chip: Chip, sources: Sources, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search commands, repositories, branches…  > for commands")
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        let sub = cx.subscribe(&input, |this, _, ev: &InputEvent, cx| match ev {
            InputEvent::Change => {
                this.selected = 0;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.confirm(cx),
            _ => {}
        });
        let commands: Vec<actions::Command> = actions::commands()
            .into_iter()
            .filter(|c| sources.has_repo || !c.needs_repo)
            .collect();
        let keys = commands
            .iter()
            .map(|c| shortcut(&*c.action, window))
            .collect();
        Self {
            chip,
            input,
            selected: 0,
            commands,
            keys,
            sources,
            _sub: sub,
        }
    }

    fn items(&self, query: &str) -> Vec<Item> {
        let (chip, query) = effective(self.chip, query);
        let want = |c: Chip| chip == Chip::All || chip == c;
        let mut scored: Vec<(usize, Item)> = Vec::new();
        if want(Chip::Repositories) {
            scored.extend(self.sources.repos.iter().enumerate().filter_map(|(i, r)| {
                fuzzy_score(query, &r.name).map(|s| {
                    (
                        s,
                        Item {
                            badge: "R".into(),
                            label: r.name.clone().into(),
                            group: if r.tab.is_some() {
                                "Open tab".into()
                            } else {
                                r.path.to_string_lossy().into_owned().into()
                            },
                            key: SharedString::default(),
                            target: Target::Repo(i),
                        },
                    )
                })
            }));
        }
        if want(Chip::Refs) {
            scored.extend(self.sources.refs.iter().enumerate().filter_map(|(i, r)| {
                fuzzy_score(query, &r.name).map(|s| {
                    (
                        s,
                        Item {
                            badge: match r.kind {
                                "Tag" => "T",
                                _ => "B",
                            }
                            .into(),
                            label: r.name.clone().into(),
                            group: r.kind.into(),
                            key: switchhead_git::short(&r.id).into(),
                            target: Target::Ref(i),
                        },
                    )
                })
            }));
        }
        if want(Chip::Commands) {
            scored.extend(self.commands.iter().enumerate().filter_map(|(i, c)| {
                fuzzy_score(query, c.label).map(|s| {
                    (
                        s,
                        Item {
                            badge: ">".into(),
                            label: c.label.into(),
                            group: c.group.into(),
                            key: self.keys.get(i).cloned().unwrap_or_default(),
                            target: Target::Command(i),
                        },
                    )
                })
            }));
        }
        if !query.is_empty() {
            // Stable: equal scores keep the order above.
            scored.sort_by_key(|(s, _)| *s);
        }
        scored.into_iter().map(|(_, i)| i).collect()
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let q = self.input.read(cx).value().to_string();
        let Some(item) = self.items(&q).into_iter().nth(self.selected) else {
            return;
        };
        let event = match item.target {
            Target::Command(i) => match self.commands.get(i) {
                Some(c) => PaletteEvent::Run(c.action.boxed_clone()),
                None => return,
            },
            Target::Repo(i) => match self.sources.repos.get(i) {
                Some(RepoItem { tab: Some(t), .. }) => PaletteEvent::SwitchTab(*t),
                Some(r) => PaletteEvent::Open(r.path.clone()),
                None => return,
            },
            Target::Ref(i) => match self.sources.refs.get(i) {
                Some(r) => PaletteEvent::Reveal(r.id),
                None => return,
            },
        };
        cx.emit(event);
    }

    fn set_chip(&mut self, chip: Chip, window: &mut Window, cx: &mut Context<Self>) {
        self.chip = chip;
        self.selected = 0;
        // A `>` prefix would override the chip.
        if self.input.read(cx).value().starts_with('>') {
            self.input.update(cx, |i, cx| i.set_value("", window, cx));
        }
        cx.notify();
    }
}

/// The shortcut of `action` as text ("Ctrl+Shift+F", "⇧⌘F"), or empty.
fn shortcut(action: &dyn Action, window: &Window) -> SharedString {
    window
        .highest_precedence_binding_for_action(action)
        .and_then(|b| {
            let keys: Vec<String> = b
                .keystrokes()
                .iter()
                .map(|k| gpui_kit::component::kbd::Kbd::format(k.as_keystroke()))
                .collect();
            (!keys.is_empty()).then(|| keys.join(" "))
        })
        .unwrap_or_default()
        .into()
}

impl Render for PaletteView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let q = self.input.read(cx).value().to_string();
        let items = self.items(&q);
        let n = items.len();
        self.selected = self.selected.min(n.saturating_sub(1));
        let (chip, _) = effective(self.chip, &q);
        let rows: Vec<AnyElement> = items
            .into_iter()
            .take(MAX_ROWS)
            .enumerate()
            .map(|(i, it)| {
                let sel = i == self.selected;
                div()
                    .id(("pal-item", i))
                    .h(rpx(32.))
                    .flex()
                    .items_center()
                    .gap(rpx(10.))
                    .px(rpx(10.))
                    .rounded(px(6.))
                    .text_size(ts::BASE)
                    .when(sel, |d| d.bg(p.sel))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.selected != i {
                            this.selected = i;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = i;
                        this.confirm(cx);
                    }))
                    .child(ui::monogram(it.badge, 28., &p))
                    .child(div().flex_1().min_w_0().truncate().child(it.label))
                    .child(
                        div()
                            .max_w(rpx(240.))
                            .truncate()
                            .text_size(ts::LABEL)
                            .text_color(p.fg3)
                            .child(it.group),
                    )
                    .when(!it.key.is_empty(), |d| {
                        d.child(
                            div()
                                .min_w(rpx(44.))
                                .flex()
                                .justify_end()
                                .font_family(MONO)
                                .text_size(ts::CAPTION_PLUS)
                                .text_color(p.fg2)
                                .child(it.key),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let chips = Chip::ALL.into_iter().map(|c| {
            let on = c == chip;
            div()
                .id(c.label())
                .h(rpx(22.))
                .px(rpx(9.))
                .flex()
                .items_center()
                .border_1()
                .border_color(if on { p.acc } else { p.bd2 })
                .rounded(px(11.))
                .when(on, |d| d.bg(p.sel))
                .text_size(ts::LABEL)
                .font_weight(FontWeight::MEDIUM)
                .text_color(if on { p.fg } else { p.fg2 })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, w, cx| this.set_chip(c, w, cx)))
                .child(c.label())
        });
        let shown = n.min(MAX_ROWS);
        div()
            .id("palette")
            .w(rpx(640.))
            .bg(p.elev)
            .rounded(px(10.))
            .shadow(ui::shadow(&p))
            .overflow_hidden()
            .capture_key_down(cx.listener(move |this, ev: &KeyDownEvent, w, cx| {
                match ev.keystroke.key.as_str() {
                    "down" => {
                        this.selected = (this.selected + 1).min(shown.saturating_sub(1));
                        cx.stop_propagation();
                        cx.notify();
                    }
                    "up" => {
                        this.selected = this.selected.saturating_sub(1);
                        cx.stop_propagation();
                        cx.notify();
                    }
                    "escape" => {
                        cx.emit(PaletteEvent::Dismiss);
                        cx.stop_propagation();
                    }
                    "tab" => {
                        let q = this.input.read(cx).value().to_string();
                        let next = effective(this.chip, &q).0.next();
                        this.set_chip(next, w, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .h(rpx(46.))
                    .flex()
                    .items_center()
                    .px(rpx(14.))
                    .border_b_1()
                    .border_color(p.bd)
                    .child(
                        div().flex_1().child(
                            Input::new(&self.input)
                                .appearance(false)
                                .text_size(ts::TITLE),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(rpx(6.))
                    .px(rpx(12.))
                    .py(rpx(8.))
                    .border_b_1()
                    .border_color(p.bd)
                    .children(chips),
            )
            .child(
                div()
                    .id("pal-list")
                    .max_h(rpx(360.))
                    .overflow_y_scroll()
                    .p(rpx(4.))
                    .children(rows)
                    .when(n == 0, |d| {
                        d.child(
                            div()
                                .py(rpx(28.))
                                .px(rpx(16.))
                                .flex()
                                .flex_col()
                                .items_center()
                                .text_color(p.fg2)
                                .text_size(ts::BASE)
                                .child(format!("No matches for “{q}”"))
                                .child(
                                    div()
                                        .mt(rpx(4.))
                                        .text_color(p.fg3)
                                        .text_size(ts::BODY)
                                        .child("Tab switches the filter; > searches commands"),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .gap(rpx(14.))
                    .px(rpx(14.))
                    .py(rpx(8.))
                    .border_t_1()
                    .border_color(p.bd)
                    .text_size(ts::SMALL)
                    .text_color(p.fg3)
                    .child("↑↓ navigate")
                    .child("↵ run")
                    .child("tab next filter")
                    .child("esc close")
                    .child(div().flex_1())
                    .child(if n > shown {
                        format!("{shown} of {n} results")
                    } else {
                        format!("{n} results")
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_angle_bracket_searches_commands() {
        assert_eq!(effective(Chip::All, "> fetch"), (Chip::Commands, "fetch"));
        assert_eq!(effective(Chip::Refs, " main "), (Chip::Refs, "main"));
    }

    #[test]
    fn tab_cycles_the_chips() {
        let mut c = Chip::All;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(c);
            c = c.next();
        }
        assert_eq!(seen, Chip::ALL);
        assert_eq!(c, Chip::All);
    }

    #[test]
    fn fuzzy_prefers_contiguous_matches() {
        assert!(fuzzy("fch", "Fetch"));
        assert!(!fuzzy("xyz", "Fetch"));
        assert!(fuzzy_score("pu", "Pull").unwrap() < fuzzy_score("pl", "Pull").unwrap());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("zz", "Push"), None);
    }
}
