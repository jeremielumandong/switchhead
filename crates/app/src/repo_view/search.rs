//! Commit search above the graph (Ctrl/Cmd+F, the rail's Search, the palette): message,
//! author, commit id, file path or content. Matching rows stay bright while the others
//! dim; Enter and Shift+Enter step through the matches. git does the work
//! ([`switchhead_git::search`]) except for ids, which are matched in the loaded graph.

use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, px,
};
use switchhead_git::Oid;
use switchhead_git::cli::Cancel;
use switchhead_git::search::{self, SearchKind};
use tokio::sync::mpsc::unbounded_channel;

use super::RepoView;
use crate::appearance::{rpx, ts};
use crate::theme::{MONO, Palette};

/// The search bar's state.
pub(super) struct Search {
    input: Entity<InputState>,
    kind: SearchKind,
    /// The text the matches are for (empty: no search, nothing dims).
    query: String,
    /// Matches in log order.
    ids: Vec<Oid>,
    set: HashSet<Oid>,
    /// An id search: matched against the loaded graph as it loads.
    prefix: Option<Vec<u8>>,
    running: bool,
    error: Option<SharedString>,
    cancel: Cancel,
    _task: Task<()>,
    _sub: Subscription,
}

impl Search {
    /// Whether row `id` should dim: a search is active and it does not match.
    pub(super) fn dims(&self, id: &Oid) -> bool {
        !self.query.is_empty() && !self.matches(id)
    }

    fn matches(&self, id: &Oid) -> bool {
        match &self.prefix {
            Some(nib) => has_prefix(id, nib),
            None => self.set.contains(id),
        }
    }
}

/// `abc1` as nibbles, or `None` when it is not hex.
fn nibbles(prefix: &str) -> Option<Vec<u8>> {
    prefix
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect()
}

/// `id` starts with these nibbles.
fn has_prefix(id: &Oid, nib: &[u8]) -> bool {
    let bytes = id.as_bytes();
    nib.iter().enumerate().all(|(i, n)| {
        let b = bytes.get(i / 2).copied().unwrap_or(0);
        let got = if i % 2 == 0 { b >> 4 } else { b & 0x0f };
        got == *n
    })
}

impl RepoView {
    /// Show the search bar and put the cursor in it.
    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode != super::Mode::Commits {
            self.set_mode(super::Mode::Commits, window, cx);
        }
        if self.search.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search commits"));
            let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| match ev {
                InputEvent::Change => this.run_search(true, cx),
                InputEvent::PressEnter { shift, .. } => {
                    this.step_match(if *shift { -1 } else { 1 }, cx)
                }
                _ => {}
            });
            self.search = Some(Search {
                input,
                kind: SearchKind::default(),
                query: String::new(),
                ids: Vec::new(),
                set: HashSet::new(),
                prefix: None,
                running: false,
                error: None,
                cancel: Cancel::default(),
                _task: Task::ready(()),
                _sub: sub,
            });
        }
        if let Some(s) = &self.search {
            s.input.update(cx, |i, cx| i.focus(window, cx));
        }
        cx.notify();
    }

    /// The search bar is showing.
    pub fn search_open(&self) -> bool {
        self.search.is_some()
    }

    /// Hide the search bar; rows stop dimming.
    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = self.search.take() {
            s.cancel.cancel();
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn set_search_kind(&mut self, kind: SearchKind, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = &mut self.search {
            s.kind = kind;
            let placeholder = match kind {
                SearchKind::Message => "Search commit messages",
                SearchKind::Author => "Author name or email",
                SearchKind::Id => "Commit id prefix",
                SearchKind::Path => "File or folder path",
                SearchKind::Content => "Text added or removed",
                SearchKind::ContentRegex => "Regex in changed lines",
            };
            s.input.update(cx, |i, cx| {
                i.set_placeholder(placeholder, window, cx);
                i.focus(window, cx);
            });
        }
        self.run_search(false, cx);
    }

    /// Search again (after typing, a kind or scope change). `debounce` waits for typing
    /// to pause first.
    pub(super) fn run_search(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let scope = self.scope.clone();
        let repo = self.repo.clone();
        let Some(s) = &mut self.search else { return };
        s.cancel.cancel();
        let text = s.input.read(cx).value().trim().to_owned();
        s.ids.clear();
        s.set.clear();
        s.error = None;
        s.prefix = None;
        s.query = text.clone();
        s.running = false;
        if text.is_empty() {
            s._task = Task::ready(());
            cx.notify();
            return;
        }
        if s.kind == SearchKind::Id {
            match nibbles(&text) {
                Some(nib) => s.prefix = Some(nib),
                None => s.error = Some("A commit id is hexadecimal".into()),
            }
            s._task = Task::ready(());
            self.step_match(0, cx);
            cx.notify();
            return;
        }
        let cancel = Cancel::default();
        s.cancel = cancel.clone();
        s.running = true;
        let kind = s.kind;
        let (tx, mut rx) = unbounded_channel::<Result<Vec<Oid>, String>>();
        s._task = cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if cancel.is_cancelled() {
                    return;
                }
            }
            let thread_cancel = cancel.clone();
            let spawned = std::thread::Builder::new()
                .name("git-search".into())
                .spawn(move || {
                    let batches = tx.clone();
                    let res = search::search(&repo, &scope, kind, &text, &thread_cancel, |b| {
                        let _ = batches.send(Ok(b));
                    });
                    if let Err(e) = res {
                        let _ = tx.send(Err(e.to_string()));
                    }
                });
            if spawned.is_err() {
                return;
            }
            let mut first = true;
            while let Some(msg) = rx.recv().await {
                if cancel.is_cancelled() {
                    return;
                }
                let ok = this.update(cx, |v, cx| {
                    if let Some(s) = &mut v.search {
                        match msg {
                            Ok(batch) => {
                                s.set.extend(batch.iter().copied());
                                s.ids.extend(batch);
                            }
                            Err(e) => s.error = Some(e.into()),
                        }
                    }
                    if first {
                        v.step_match(0, cx);
                    }
                    cx.notify();
                });
                first = false;
                if ok.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |v, cx| {
                if let Some(s) = &mut v.search {
                    s.running = false;
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    /// Select the next (`1`), previous (`-1`) or nearest (`0`: the first at or after the
    /// selection) loaded match.
    fn step_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(s) = &self.search else { return };
        let mut rows = self.match_rows(s);
        if rows.is_empty() {
            return;
        }
        rows.sort_unstable();
        let cur = self.selected;
        let next = match (delta, cur) {
            (0, Some(c)) => rows.iter().find(|&&r| r >= c).or(rows.first()),
            (0, None) => rows.first(),
            (d, Some(c)) if d > 0 => rows.iter().find(|&&r| r > c).or(rows.first()),
            (_, Some(c)) => rows.iter().rev().find(|&&r| r < c).or(rows.last()),
            (d, None) if d > 0 => rows.first(),
            (_, None) => rows.last(),
        };
        if let Some(&row) = next {
            self.selected = Some(row);
            self.scroll.scroll_to_item(row, ScrollStrategy::Center);
            self.sync_details(false, cx);
            cx.notify();
        }
    }

    /// The loaded rows that match.
    fn match_rows(&self, s: &Search) -> Vec<usize> {
        match &s.prefix {
            Some(nib) => self.log.rows_where(|r| has_prefix(&self.log.id(r), nib)),
            None => s.ids.iter().filter_map(|id| self.log.row_of(id)).collect(),
        }
    }

    /// "3 of 17", or what the search is doing.
    fn search_status(&self, s: &Search) -> SharedString {
        if let Some(e) = &s.error {
            return e.clone();
        }
        if s.query.is_empty() {
            return SharedString::default();
        }
        let rows = self.match_rows(s);
        let n = if s.prefix.is_some() {
            rows.len()
        } else {
            s.ids.len()
        };
        let pos = self
            .selected
            .filter(|&r| s.matches(&self.log.id(r)))
            .map(|r| rows.iter().filter(|&&mr| mr <= r).count());
        let more = if s.running { "+" } else { "" };
        match (n, pos) {
            (0, _) if s.running => "Searching…".into(),
            (0, _) => "No matches".into(),
            (n, Some(p)) => format!("{p} of {n}{more}").into(),
            (n, None) => format!("{n}{more} match{}", if n == 1 { "" } else { "es" }).into(),
        }
    }

    pub(super) fn render_search(&self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.search.as_ref()?;
        let kind = s.kind;
        let view = cx.entity().downgrade();
        let kinds = Button::new("search-kind")
            .ghost()
            .xsmall()
            .label(kind.label())
            .dropdown_menu(move |mut menu, _, _| {
                for k in SearchKind::ALL {
                    let view = view.clone();
                    menu = menu.item(PopupMenuItem::new(k.label()).checked(k == kind).on_click(
                        move |_, window, cx| {
                            let _ = view.update(cx, |v, cx| v.set_search_kind(k, window, cx));
                        },
                    ));
                }
                menu
            });
        let status = self.search_status(s);
        let arrow = |id: &'static str, label: &'static str, delta: isize| {
            div()
                .id(id)
                .flex_none()
                .size(rpx(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .text_color(p.fg2)
                .hover(|st| st.bg(p.hover).text_color(p.fg))
                .on_click(cx.listener(move |this, _, _, cx| this.step_match(delta, cx)))
                .child(label)
        };
        Some(
            div()
                .id("search-bar")
                .flex_none()
                .h(rpx(34.))
                .px(rpx(8.))
                .flex()
                .items_center()
                .gap(rpx(6.))
                .border_b_1()
                .border_color(p.bd)
                .bg(p.panel)
                // Escape in the field: the app's Dismiss reaches the bar first.
                .on_action(
                    cx.listener(|this, _: &crate::actions::Dismiss, window, cx| {
                        this.close_search(window, cx)
                    }),
                )
                .child(kinds)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&s.input).small().text_size(ts::BODY)),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .text_size(ts::SMALL)
                        .text_color(if s.error.is_some() { p.deleted } else { p.fg3 })
                        .child(status),
                )
                .child(arrow("search-prev", "↑", -1))
                .child(arrow("search-next", "↓", 1))
                .child(
                    div()
                        .id("search-close")
                        .flex_none()
                        .size(rpx(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .text_color(p.fg2)
                        .hover(|st| st.bg(p.hover).text_color(p.fg))
                        .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx)))
                        .child("×"),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_id_prefixes() {
        let id = Oid::from_hex(b"a1b2c3d4e5f60718293a4b5c6d7e8f9012345678").unwrap();
        assert!(has_prefix(&id, &nibbles("a1b2c").unwrap()));
        assert!(has_prefix(&id, &nibbles("A1B2").unwrap()));
        assert!(!has_prefix(&id, &nibbles("a1b3").unwrap()));
        assert!(nibbles("xyz").is_none());
    }
}
