//! A commit's whole file tree, shown in place of the graph: folders and files as they were
//! at that commit, with the selected file's content beside them. A file can be opened in
//! the system's default app (from a temporary copy), saved elsewhere, or shown in File
//! History and Blame.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::AppContext as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ListHorizontalSizingBehavior, ParentElement as _, Render,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use switchhead_git::tree::{self, EntryKind, TreeEntry};
use switchhead_git::{Oid, Repo, diff};

use crate::actions;
use crate::appearance::{rpx, ts};
use crate::file_list::{self, FileRow};
use crate::file_view::Lines;
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// Files above this size are not previewed (they can still be opened or saved).
const PREVIEW_LIMIT: usize = 4 * 1024 * 1024;

/// What the tree view asks of the repository view.
pub enum TreeViewEvent {
    /// Close the view.
    Close,
    /// Show a file's history (`blame: false`) or blame at this view's commit.
    OpenFile {
        /// The file.
        path: PathBuf,
        /// Blame rather than history.
        blame: bool,
    },
}

/// The selected file's content.
enum Preview {
    None,
    Loading,
    Text(Arc<Lines>),
    Binary(usize),
    TooLarge(usize),
    Submodule(Oid),
    Failed(SharedString),
}

/// The files of one commit.
pub struct TreeView {
    repo: Repo,
    commit: Oid,
    /// "a1b2c3d Summary".
    label: SharedString,
    files: Rc<Vec<TreeEntry>>,
    /// Indices into `files` that match the filter.
    shown: Vec<usize>,
    rows: Vec<FileRow>,
    /// Folders the user opened (all open while filtering).
    expanded: HashSet<PathBuf>,
    filter: Entity<InputState>,
    filter_text: String,
    /// Index into `files`.
    selected: Option<usize>,
    preview: Preview,
    loading: bool,
    error: Option<SharedString>,
    /// The last save or open, for the footer (text, failed).
    notice: Option<(SharedString, bool)>,
    scroll: UniformListScrollHandle,
    preview_scroll: UniformListScrollHandle,
    split: Entity<ResizableState>,
    focus: FocusHandle,
    focus_pending: bool,
    _load: Task<()>,
    _preview: Task<()>,
    _sub: Subscription,
}

impl EventEmitter<TreeViewEvent> for TreeView {}

impl Focusable for TreeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TreeView {
    /// The files of `commit` (described by `summary`).
    pub fn new(
        repo: Repo,
        commit: Oid,
        summary: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
        let sub = cx.subscribe_in(&filter, window, |this, input, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                this.filter_text = input.read(cx).value().to_string();
                this.rebuild(cx);
            }
        });
        let mut v = Self {
            repo,
            commit,
            label: format!("{}  {summary}", switchhead_git::short(&commit)).into(),
            files: Rc::default(),
            shown: Vec::new(),
            rows: Vec::new(),
            expanded: HashSet::new(),
            filter,
            filter_text: String::new(),
            selected: None,
            preview: Preview::None,
            loading: true,
            error: None,
            notice: None,
            scroll: UniformListScrollHandle::new(),
            preview_scroll: UniformListScrollHandle::new(),
            split: cx.new(|_| ResizableState::default()),
            focus: cx.focus_handle(),
            focus_pending: true,
            _load: Task::ready(()),
            _preview: Task::ready(()),
            _sub: sub,
        };
        v.load(cx);
        v
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (repo, commit) = (self.repo.clone(), self.commit);
        let task = cx
            .background_executor()
            .spawn(async move { tree::list(&repo, commit) });
        self._load = cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                v.loading = false;
                match res {
                    Ok(files) => {
                        v.files = Rc::new(files);
                        v.rebuild(cx);
                    }
                    Err(e) => v.error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        });
    }

    /// Recompute the matching files and the visible rows.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let needle = self.filter_text.trim().to_lowercase();
        self.shown = (0..self.files.len())
            .filter(|&i| {
                needle.is_empty()
                    || self.files[i]
                        .path
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&needle)
            })
            .collect();
        let shown: Vec<&TreeEntry> = self.shown.iter().map(|&i| &self.files[i]).collect();
        let filtering = !needle.is_empty();
        let expanded = &self.expanded;
        self.rows = file_list::tree_with(&shown, &|p: &Path| filtering || expanded.contains(p));
        cx.notify();
    }

    fn toggle_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.rebuild(cx);
    }

    /// Select `files[index]` and read its content.
    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.files.get(index).cloned() else {
            return;
        };
        if self.selected == Some(index) {
            return;
        }
        self.selected = Some(index);
        self.notice = None;
        if let Some(row) = self.row_of(index) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        if entry.kind == EntryKind::Submodule {
            self.preview = Preview::Submodule(entry.id);
            self._preview = Task::ready(());
            cx.notify();
            return;
        }
        self.preview = Preview::Loading;
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move {
            let data = diff::read_blob(&repo, entry.id)?;
            Ok::<_, switchhead_git::Error>(if data.len() > PREVIEW_LIMIT {
                Preview::TooLarge(data.len())
            } else if diff::is_binary(&data) {
                Preview::Binary(data.len())
            } else {
                Preview::Text(Arc::new(Lines::new(
                    String::from_utf8_lossy(&data).into_owned(),
                )))
            })
        });
        self._preview = cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                v.preview = res.unwrap_or_else(|e| Preview::Failed(e.to_string().into()));
                v.preview_scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            });
        });
        cx.notify();
    }

    /// The row showing `files[index]`, if its folders are open.
    fn row_of(&self, index: usize) -> Option<usize> {
        self.rows.iter().position(
            |r| matches!(r, FileRow::File { index: i, .. } if self.shown.get(*i) == Some(&index)),
        )
    }

    /// Move the selection by `delta` files among the visible rows.
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let files: Vec<usize> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                FileRow::File { index, .. } => self.shown.get(*index).copied(),
                FileRow::Dir { .. } => None,
            })
            .collect();
        if files.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| files.iter().position(|&f| f == s));
        let next = match at {
            Some(at) => (at as isize)
                .saturating_add(delta)
                .clamp(0, files.len() as isize - 1) as usize,
            None => 0,
        };
        self.select(files[next], cx);
    }

    fn selected_entry(&self) -> Option<&TreeEntry> {
        self.selected.and_then(|i| self.files.get(i))
    }

    /// Write the selected file to a temporary folder and open it with the system's app.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        if entry.kind == EntryKind::Submodule {
            return;
        }
        let dir = std::env::temp_dir()
            .join("switchhead")
            .join(switchhead_git::short(&self.commit));
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move {
            let name = entry
                .path
                .file_name()
                .map(|n| n.to_owned())
                .unwrap_or_default();
            let dest = dir.join(name);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&dest, diff::read_blob(&repo, entry.id)?)?;
            Ok::<_, switchhead_git::Error>(dest)
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                match res {
                    Ok(dest) => {
                        cx.open_with_system(&dest);
                        v.notice = Some((format!("Opened {}", dest.display()).into(), false));
                    }
                    Err(e) => v.notice = Some((format!("Could not open: {e}").into(), true)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Ask where to save the selected file as it was at this commit, and write it there.
    fn save_selected(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        if entry.kind == EntryKind::Submodule {
            return;
        }
        let name = entry
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let chosen = cx.prompt_for_new_path(self.repo.root(), Some(&name));
        let repo = self.repo.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(dest))) = chosen.await else {
                return;
            };
            let task = cx.background_executor().spawn(async move {
                std::fs::write(&dest, diff::read_blob(&repo, entry.id)?)?;
                Ok::<_, switchhead_git::Error>(dest)
            });
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                v.notice = Some(match res {
                    Ok(dest) => (format!("Saved {}", dest.display()).into(), false),
                    Err(e) => (format!("Could not save: {e}").into(), true),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn render_header(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let file = self
            .selected_entry()
            .filter(|e| e.kind != EntryKind::Submodule);
        let has = file.is_some();
        let path = file.map(|e| e.path.clone());
        let emit = move |blame: bool| {
            let path = path.clone();
            move |_: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                if let Some(path) = path.clone() {
                    cx.emit(TreeViewEvent::OpenFile { path, blame });
                }
            }
        };
        div()
            .flex_none()
            .h(rpx(40.))
            .px(rpx(12.))
            .flex()
            .items_center()
            .gap(rpx(8.))
            .border_b_1()
            .border_color(p.bd)
            .child(
                div()
                    .flex_none()
                    .text_size(ts::TITLE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.fg)
                    .child("Files"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ts::SMALL)
                    .text_color(p.fg3)
                    .child(format!("at {}", self.label)),
            )
            .child(
                ui::button("tree-open", "Open", ui::Kind::Secondary, p)
                    .when(!has, |d| d.opacity(0.5))
                    .on_click(cx.listener(|this, _, _, cx| this.open_selected(cx))),
            )
            .child(
                ui::button("tree-save", "Save As…", ui::Kind::Secondary, p)
                    .when(!has, |d| d.opacity(0.5))
                    .on_click(cx.listener(|this, _, _, cx| this.save_selected(cx))),
            )
            .child(
                ui::button("tree-history", "File History", ui::Kind::Secondary, p)
                    .when(!has, |d| d.opacity(0.5))
                    .on_click(cx.listener(emit(false))),
            )
            .child(
                ui::button("tree-blame", "Blame", ui::Kind::Secondary, p)
                    .when(!has, |d| d.opacity(0.5))
                    .on_click(cx.listener(emit(true))),
            )
            .child(
                ui::button("tree-close", "Close", ui::Kind::Secondary, p)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TreeViewEvent::Close))),
            )
            .into_any_element()
    }

    fn render_list(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let body = if let Some(e) = &self.error {
            message(e.clone(), p.deleted)
        } else if self.rows.is_empty() {
            message(
                match (self.loading, self.filter_text.trim().is_empty()) {
                    (true, _) => "",
                    (false, true) => "This commit has no files.",
                    (false, false) => "No file matches the filter.",
                },
                p.fg3,
            )
        } else {
            let row_h = crate::diff_view::row_height(cx);
            uniform_list(
                "tree-files",
                self.rows.len(),
                cx.processor(move |this, range: Range<usize>, _, cx| {
                    let p = palette(cx);
                    range
                        .filter_map(|i| this.rows.get(i).cloned().map(|r| (i, r)))
                        .map(|(i, r)| this.render_row(i, r, row_h, &p, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let count = match (self.shown.len(), self.files.len()) {
            (s, t) if s == t => format!("{t} files"),
            (s, t) => format!("{s} of {t} files"),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .p(rpx(8.))
                    .flex()
                    .items_center()
                    .gap(rpx(8.))
                    .border_b_1()
                    .border_color(p.bd)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.filter).small().text_size(ts::BODY)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ts::SMALL)
                            .text_color(p.fg3)
                            .child(count),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_row(
        &self,
        i: usize,
        row: FileRow,
        row_h: f32,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let base = div()
            .id(("tree-row", i))
            .w_full()
            .h(px(row_h))
            .flex()
            .items_center()
            .gap(rpx(6.))
            .pr(rpx(10.))
            .text_size(ts::BODY)
            .cursor_pointer();
        match row {
            FileRow::Dir {
                path,
                name,
                depth,
                open,
                files,
            } => base
                .pl(rpx(10. + 14. * depth as f32))
                .text_color(p.fg2)
                .hover(|s| s.bg(p.hover))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_dir(path.clone(), cx)))
                .child(
                    div()
                        .w(rpx(10.))
                        .flex_none()
                        .text_size(ts::CAPTION)
                        .text_color(p.fg3)
                        .child(if open { "▾" } else { "▸" }),
                )
                .child(div().flex_1().min_w_0().truncate().child(name))
                .child(
                    div()
                        .flex_none()
                        .font_family(MONO)
                        .text_size(ts::CAPTION)
                        .text_color(p.fg3)
                        .child(files.to_string()),
                )
                .into_any_element(),
            FileRow::File { index, depth } => {
                let Some(&file) = self.shown.get(index) else {
                    return base.into_any_element();
                };
                let Some(e) = self.files.get(file) else {
                    return base.into_any_element();
                };
                let selected = self.selected == Some(file);
                let name = e
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let tag = match e.kind {
                    EntryKind::File => None,
                    EntryKind::Executable => Some("exec"),
                    EntryKind::Link => Some("link"),
                    EntryKind::Submodule => Some("submodule"),
                };
                base.pl(rpx(24. + 14. * depth as f32))
                    .when(selected, |d| d.bg(p.sel))
                    .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        this.select(file, cx);
                        if ev.click_count() > 1 {
                            this.open_selected(cx);
                        }
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(p.fg)
                            .child(SharedString::from(name)),
                    )
                    .when_some(tag, |d, tag| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(ts::CAPTION)
                                .text_color(p.fg3)
                                .child(tag),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    fn render_preview(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let title = self
            .selected_entry()
            .map(|e| e.path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let body = match &self.preview {
            Preview::None => message("Select a file to see its content.", p.fg3),
            Preview::Loading => message("", p.fg3),
            Preview::Binary(n) => message(format!("Binary file, {}.", size(*n)), p.fg3),
            Preview::TooLarge(n) => message(
                format!(
                    "{} is too large to preview. Open or save it instead.",
                    size(*n)
                ),
                p.fg3,
            ),
            Preview::Submodule(id) => message(
                format!("Submodule at commit {}.", switchhead_git::short(id)),
                p.fg3,
            ),
            Preview::Failed(e) => message(e.clone(), p.deleted),
            Preview::Text(lines) if lines.len() == 0 => message("The file is empty.", p.fg3),
            Preview::Text(lines) => {
                let row_h = crate::diff_view::row_height(cx);
                let digits = lines.len().to_string().len() as f32;
                let lines2 = lines.clone();
                uniform_list(
                    "tree-preview",
                    lines.len(),
                    cx.processor(move |_, range: Range<usize>, _, cx| {
                        let p = palette(cx);
                        range
                            .map(|n| {
                                div()
                                    .id(("tree-line", n))
                                    .h(px(row_h))
                                    .flex()
                                    .items_center()
                                    .whitespace_nowrap()
                                    .child(
                                        div()
                                            .flex_none()
                                            .w(rpx(10. + 8. * digits))
                                            .pr(rpx(10.))
                                            .flex()
                                            .justify_end()
                                            .text_color(p.fg3)
                                            .child((n + 1).to_string()),
                                    )
                                    .child(
                                        div()
                                            .text_color(p.fg)
                                            .child(lines2.line(n).replace('\t', "    ")),
                                    )
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.preview_scroll)
                .flex_1()
                .min_h_0()
                .font_family(crate::appearance::editor_font_family(cx))
                .text_size(crate::appearance::editor_font_size(cx))
                .with_width_from_item(Some(lines.widest))
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(rpx(30.))
                    .px(rpx(12.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(p.bd)
                    .font_family(MONO)
                    .text_size(ts::SMALL)
                    .text_color(p.fg2)
                    .child(div().min_w_0().truncate().child(title)),
            )
            .child(body)
            .into_any_element()
    }
}

impl Render for TreeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_pending) {
            window.focus(&self.focus, cx);
        }
        let p = palette(cx);
        let rem = window.rem_size();
        let list = self.render_list(&p, cx);
        let preview = self.render_preview(&p, cx);
        div()
            .id("tree-view")
            .key_context("Files")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.surface)
            .on_action(
                cx.listener(|this, _: &actions::SelectUp, _, cx| this.move_selection(-1, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::SelectDown, _, cx| this.move_selection(1, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::SelectFirst, _, cx| {
                this.move_selection(isize::MIN / 2, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::SelectLast, _, cx| {
                this.move_selection(isize::MAX / 2, cx)
            }))
            .on_action(cx.listener(|_, _: &actions::Dismiss, _, cx| cx.emit(TreeViewEvent::Close)))
            .child(self.render_header(&p, cx))
            .when(self.loading, |d| {
                d.child(ui::loading_bar("tree-loading", &p))
            })
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("tree-split")
                        .with_state(&self.split)
                        .child(
                            resizable_panel()
                                .size(rem * (340. / 13.))
                                .size_range(rem * (200. / 13.)..rem * (900. / 13.))
                                .child(list),
                        )
                        .child(resizable_panel().child(preview)),
                ),
            )
            .when_some(self.notice.clone(), |d, (text, failed)| {
                d.child(
                    div()
                        .flex_none()
                        .h(rpx(28.))
                        .px(rpx(12.))
                        .flex()
                        .items_center()
                        .border_t_1()
                        .border_color(p.bd)
                        .bg(p.panel)
                        .text_size(ts::SMALL)
                        .text_color(if failed { p.deleted } else { p.fg2 })
                        .child(div().min_w_0().truncate().child(text)),
                )
            })
    }
}

fn message(text: impl Into<SharedString>, color: gpui_kit::Hsla) -> AnyElement {
    div()
        .flex_1()
        .p(rpx(16.))
        .text_size(ts::BODY)
        .text_color(color)
        .child(text.into())
        .into_any_element()
}

/// "812 bytes", "4.2 KB", "13.0 MB".
fn size(n: usize) -> String {
    match n {
        n if n < 1024 => format!("{n} bytes"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.),
        n => format!("{:.1} MB", n as f64 / (1024. * 1024.)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(size(812), "812 bytes");
        assert_eq!(size(4300), "4.2 KB");
        assert_eq!(size(13 * 1024 * 1024), "13.0 MB");
    }
}
