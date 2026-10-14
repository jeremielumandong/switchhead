//! The commit details pane under the graph: the selected commit's header and message, the
//! files it changed (a list or a folder tree), and the selected file's diff (inline or side
//! by side, with word highlights and syntax colors).
//!
//! Everything is read off the UI thread: the header and file list in one background task
//! when the selection changes, the diff in another when the file (or a diff setting)
//! changes. A newer request replaces the older task, so fast scrolling through the graph
//! only finishes the last one.

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::AppContext as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, HighlightStyle, Hsla, InteractiveElement as _, IntoElement,
    ListHorizontalSizingBehavior, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use switchhead_git::commit::{self, CommitInfo};
use switchhead_git::diff::{self, FileChange, FileDiff, FileStatus, LineKind, TextDiff};
use switchhead_git::refs::Label;
use switchhead_git::{Oid, Repo};

use crate::actions;
use crate::appearance::{rpx, ts};
use crate::diff_model::{self, Row, SbsRow, Syntax};
use crate::file_list::{self, FileRow};
use crate::store::{DiffPrefs, WhitespacePref};
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// Diff and file-list row height at 100 % zoom.
const ROW_H: f32 = 20.0;

/// The user clicked a parent: select that commit in the graph.
pub struct Navigate(pub Oid);

/// What the selected file's diff shows.
enum Content {
    Text {
        diff: TextDiff,
        inline: Vec<Row>,
        sbs: Vec<SbsRow>,
        widest: Option<usize>,
        syntax: Option<Syntax>,
    },
    Binary {
        old: Option<u64>,
        new: Option<u64>,
    },
    TooLarge {
        old: Option<u64>,
        new: Option<u64>,
    },
    Submodule {
        old: Option<Oid>,
        new: Option<Oid>,
    },
    Failed(String),
}

/// A loaded file diff and what it was computed with.
struct Loaded {
    file: usize,
    prefs: DiffPrefs,
    theme: usize,
    content: Content,
}

/// The details pane.
pub struct CommitView {
    repo: Repo,
    commit: Option<Oid>,
    info: Option<Rc<CommitInfo>>,
    labels: Vec<Label>,
    files: Rc<Vec<FileChange>>,
    rows: Vec<FileRow>,
    collapsed: HashSet<PathBuf>,
    selected_file: Option<usize>,
    loaded: Option<Rc<Loaded>>,
    loading: bool,
    diff_loading: bool,
    error: Option<SharedString>,
    files_scroll: UniformListScrollHandle,
    diff_scroll: UniformListScrollHandle,
    split: Entity<ResizableState>,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _diff: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Navigate> for CommitView {}

impl CommitView {
    /// An empty pane for `repo`.
    pub fn new(repo: Repo, cx: &mut Context<Self>) -> Self {
        let subs = vec![
            cx.observe_global::<DiffPrefs>(|this, cx| this.refresh_diff(cx)),
            cx.observe_global::<gpui_kit::component::Theme>(|this, cx| this.refresh_diff(cx)),
        ];
        Self {
            repo,
            commit: None,
            info: None,
            labels: Vec::new(),
            files: Rc::default(),
            rows: Vec::new(),
            collapsed: HashSet::new(),
            selected_file: None,
            loaded: None,
            loading: false,
            diff_loading: false,
            error: None,
            files_scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
            split: cx.new(|_| ResizableState::default()),
            focus: cx.focus_handle(),
            _load: None,
            _diff: None,
            _subs: subs,
        }
    }

    /// Show commit `id` (`None`: nothing selected). `labels`: its refs.
    pub fn show(&mut self, id: Option<Oid>, labels: Vec<Label>, cx: &mut Context<Self>) {
        self.labels = labels;
        if id == self.commit {
            cx.notify();
            return;
        }
        self.commit = id;
        self.error = None;
        let Some(id) = id else {
            self.info = None;
            self.files = Rc::default();
            self.rows.clear();
            self.selected_file = None;
            self.loaded = None;
            self.loading = false;
            self._load = None;
            self._diff = None;
            cx.notify();
            return;
        };
        self.loading = true;
        let repo = self.repo.clone();
        let task = cx.background_executor().spawn(async move {
            let info = commit::info(&repo, id)?;
            let files = diff::tree_files(&repo, info.diff_base, id)?;
            Ok::<_, switchhead_git::Error>((info, files))
        });
        self._load = Some(cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.commit != Some(id) {
                    return;
                }
                v.loading = false;
                match res {
                    Ok((info, files)) => v.set_files(info, files, cx),
                    Err(e) => {
                        v.error = Some(e.to_string().into());
                        v.info = None;
                        v.files = Rc::default();
                        v.rows.clear();
                        v.selected_file = None;
                        v.loaded = None;
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn set_files(&mut self, info: CommitInfo, files: Vec<FileChange>, cx: &mut Context<Self>) {
        // Keep the same file selected when the next commit changed it too.
        let previous = self
            .selected_file
            .and_then(|i| self.files.get(i))
            .map(|f| f.path.clone());
        self.info = Some(Rc::new(info));
        self.files = Rc::new(files);
        self.rebuild_rows(cx);
        let keep = previous.and_then(|p| self.files.iter().position(|f| f.path == p));
        let first = self.rows.iter().find_map(|r| match r {
            FileRow::File { index, .. } => Some(*index),
            FileRow::Dir { .. } => None,
        });
        self.loaded = None;
        self.select_file(keep.or(first), cx);
    }

    fn rebuild_rows(&mut self, cx: &App) {
        self.rows = if cx.global::<DiffPrefs>().tree {
            file_list::tree(&self.files, &self.collapsed)
        } else {
            file_list::flat(&self.files)
        };
    }

    fn select_file(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        self.selected_file = index;
        if let Some(row) = self
            .rows
            .iter()
            .position(|r| matches!(r, FileRow::File { index: i, .. } if Some(*i) == index))
        {
            self.files_scroll
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        self.load_diff(cx);
        cx.notify();
    }

    /// Move the file selection by `delta` files, in list order.
    fn move_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        let order: Vec<usize> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                FileRow::File { index, .. } => Some(*index),
                FileRow::Dir { .. } => None,
            })
            .collect();
        if order.is_empty() {
            return;
        }
        let cur = self
            .selected_file
            .and_then(|s| order.iter().position(|i| *i == s))
            .map_or(-1, |p| p as isize);
        let next = (cur + delta).clamp(0, order.len() as isize - 1) as usize;
        if Some(order[next]) != self.selected_file {
            self.select_file(Some(order[next]), cx);
        }
    }

    /// Reload the diff when a setting or the theme changed.
    fn refresh_diff(&mut self, cx: &mut Context<Self>) {
        let tree = cx.global::<DiffPrefs>().tree;
        let showing_tree = self.rows.iter().any(|r| matches!(r, FileRow::Dir { .. }))
            || (tree && self.rows.is_empty());
        if tree != showing_tree {
            self.rebuild_rows(cx);
        }
        let stale = self.loaded.as_ref().is_some_and(|l| {
            l.prefs != *cx.global::<DiffPrefs>() || l.theme != theme_key(&highlight_theme(cx))
        });
        if stale {
            self.load_diff(cx);
        }
        cx.notify();
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_file else {
            self.loaded = None;
            self._diff = None;
            self.diff_loading = false;
            return;
        };
        let Some(change) = self.files.get(index).cloned() else {
            return;
        };
        let prefs = *cx.global::<DiffPrefs>();
        let theme = highlight_theme(cx);
        let key = theme_key(&theme);
        let repo = self.repo.clone();
        self.diff_loading = true;
        let task = cx
            .background_executor()
            .spawn(async move { compute(&repo, &change, &prefs, &theme) });
        self._diff = Some(cx.spawn(async move |this, cx| {
            let content = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.selected_file != Some(index) {
                    return;
                }
                let same_file = v.loaded.as_ref().is_some_and(|l| l.file == index);
                v.diff_loading = false;
                v.loaded = Some(Rc::new(Loaded {
                    file: index,
                    prefs,
                    theme: key,
                    content,
                }));
                if !same_file {
                    v.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                cx.notify();
            });
        }));
    }

    fn toggle_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
        self.rebuild_rows(cx);
        cx.notify();
    }

    // ---------------------------------------------------------------- header

    fn render_header(&self, info: &CommitInfo, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let field = |label: &'static str| {
            div()
                .w(rpx(66.))
                .flex_none()
                .text_size(ts::SMALL)
                .text_color(p.fg3)
                .child(label)
        };
        let row = || div().flex().items_start().gap(rpx(8.)).min_w_0();
        let person = |s: &commit::Signature| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_x(rpx(6.))
                .child(
                    div()
                        .text_color(p.fg)
                        .child(SharedString::from(s.name.clone())),
                )
                .child(
                    div()
                        .text_color(p.fg3)
                        .child(SharedString::from(format!("<{}>", s.email))),
                )
                .child(div().text_color(p.fg2).child(full_date(s.time)))
        };
        let id = info.id;
        let body = info.body();
        div()
            .id("commit-header")
            .flex_none()
            .max_h(rpx(170.))
            .overflow_y_scroll()
            .px(rpx(12.))
            .py(rpx(10.))
            .flex()
            .flex_col()
            .gap(rpx(6.))
            .border_b_1()
            .border_color(p.bd)
            .text_size(ts::BODY)
            .child(
                div()
                    .text_size(ts::TITLE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.fg)
                    .child(SharedString::from(info.summary().to_owned())),
            )
            .when(!body.is_empty(), |d| {
                d.child(
                    div()
                        .text_color(p.fg2)
                        .child(SharedString::from(reflow(body))),
                )
            })
            .when(!self.labels.is_empty(), |d| {
                d.child(
                    div().flex().flex_wrap().gap(rpx(4.)).children(
                        self.labels
                            .iter()
                            .map(|l| crate::repo_view::label_chip(l, p)),
                    ),
                )
            })
            .child(row().child(field("Author")).child(person(&info.author)))
            .when(info.committed_by_other(), |d| {
                d.child(
                    row()
                        .child(field("Committer"))
                        .child(person(&info.committer)),
                )
            })
            .child(
                row()
                    .child(field("Commit"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(ts::SMALL)
                            .text_color(p.fg2)
                            .child(SharedString::from(id.to_string())),
                    )
                    .child(
                        div()
                            .id("copy-id")
                            .flex_none()
                            .px(rpx(5.))
                            .rounded(px(4.))
                            .text_size(ts::SMALL)
                            .text_color(p.acc)
                            .cursor_pointer()
                            .hover(|s| s.bg(p.hover))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(id.to_string()))
                            })
                            .child("Copy"),
                    )
                    .when(info.signed, |d| d.child(ui::chip("signed", p.added))),
            )
            .when(!info.parents.is_empty(), |d| {
                d.child(
                    row()
                        .child(field(if info.parents.len() > 1 {
                            "Parents"
                        } else {
                            "Parent"
                        }))
                        .child(div().flex().flex_wrap().gap(rpx(6.)).children(
                            info.parents.iter().enumerate().map(|(i, parent)| {
                                let parent = *parent;
                                div()
                                    .id(("parent", i))
                                    .font_family(MONO)
                                    .text_size(ts::SMALL)
                                    .text_color(p.acc)
                                    .cursor_pointer()
                                    .hover(|s| s.underline())
                                    .on_click(
                                        cx.listener(move |_, _, _, cx| cx.emit(Navigate(parent))),
                                    )
                                    .child(switchhead_git::short(&parent))
                            }),
                        ))
                        .when(info.diff_base.is_none(), |d| {
                            d.child(
                                div()
                                    .text_size(ts::SMALL)
                                    .text_color(p.fg3)
                                    .child("(not in this shallow clone)"),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    // ---------------------------------------------------------------- files

    fn render_files(&self, p: &Palette, row_h: f32, cx: &mut Context<Self>) -> AnyElement {
        let prefs = *cx.global::<DiffPrefs>();
        let n = self.files.len();
        let merge = self.info.as_ref().is_some_and(|i| i.parents.len() > 1);
        let toolbar = div()
            .h(rpx(30.))
            .flex_none()
            .flex()
            .items_center()
            .gap(rpx(8.))
            .px(rpx(12.))
            .border_b_1()
            .border_color(p.bd)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ts::SMALL)
                    .text_color(p.fg2)
                    .child(SharedString::from(match (n, merge) {
                        (1, false) => "1 file changed".to_owned(),
                        (n, false) => format!("{n} files changed"),
                        (n, true) => format!("{n} files changed (first parent)"),
                    })),
            )
            .child(ui::segmented(
                "file-mode",
                vec![
                    (
                        "List".into(),
                        !prefs.tree,
                        Box::new(|_, _, cx: &mut App| set_prefs(cx, |p| p.tree = false)),
                    ),
                    (
                        "Tree".into(),
                        prefs.tree,
                        Box::new(|_, _, cx: &mut App| set_prefs(cx, |p| p.tree = true)),
                    ),
                ],
                18.,
                p,
            ));
        let list = uniform_list(
            "files",
            self.rows.len(),
            cx.processor(move |this, range: Range<usize>, _w, cx| {
                let p = palette(cx);
                range
                    .filter_map(|i| this.rows.get(i).cloned().map(|r| (i, r)))
                    .map(|(i, r)| this.render_file_row(i, r, &p, row_h, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.files_scroll)
        .flex_1()
        .min_h_0();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(list)
            .into_any_element()
    }

    fn render_file_row(
        &self,
        i: usize,
        row: FileRow,
        p: &Palette,
        row_h: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let base = div()
            .id(("file", i))
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
                let Some(f) = self.files.get(index) else {
                    return base.into_any_element();
                };
                let selected = self.selected_file == Some(index);
                let tree = matches!(self.rows.first(), Some(FileRow::Dir { .. })) || depth > 0;
                let name: SharedString = if tree {
                    f.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into()
                } else {
                    f.path.to_string_lossy().into_owned().into()
                };
                let (dir, file) = if tree {
                    (None, name)
                } else {
                    split_path(&name)
                };
                base.pl(rpx(10. + 14. * depth as f32 + if tree { 14. } else { 0. }))
                    .when(selected, |d| d.bg(p.sel))
                    .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        this.select_file(Some(index), cx);
                    }))
                    .child(status_badge(f.status, p))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(div().flex_none().text_color(p.fg).child(file))
                            .when_some(dir, |d, dir| {
                                d.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .pl(rpx(6.))
                                        .text_color(p.fg3)
                                        .child(dir),
                                )
                            }),
                    )
                    .when_some(f.old_path.as_ref(), |d, old| {
                        d.child(
                            div()
                                .flex_none()
                                .max_w(rpx(160.))
                                .truncate()
                                .text_size(ts::SMALL)
                                .text_color(p.fg3)
                                .child(SharedString::from(format!(
                                    "from {}",
                                    old.to_string_lossy()
                                ))),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    // ---------------------------------------------------------------- diff

    fn render_diff_toolbar(&self, p: &Palette, cx: &App) -> AnyElement {
        let prefs = *cx.global::<DiffPrefs>();
        let file = self.selected_file.and_then(|i| self.files.get(i));
        let stats = self.loaded.as_ref().and_then(|l| match &l.content {
            Content::Text { diff, .. } => Some(diff.stats()),
            _ => None,
        });
        let ws_label = match prefs.whitespace {
            WhitespacePref::Show => "Whitespace",
            WhitespacePref::IgnoreChanges => "Ignore amount",
            WhitespacePref::IgnoreAll => "Ignore all",
        };
        let ws_menu = Button::new("whitespace")
            .ghost()
            .xsmall()
            .label(ws_label)
            .dropdown_menu(move |mut menu, _, cx| {
                let cur = cx.global::<DiffPrefs>().whitespace;
                for (label, w) in [
                    ("Show whitespace changes", WhitespacePref::Show),
                    (
                        "Ignore changes in amount (-b)",
                        WhitespacePref::IgnoreChanges,
                    ),
                    ("Ignore all whitespace (-w)", WhitespacePref::IgnoreAll),
                ] {
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(cur == w)
                            .on_click(move |_, _, cx| set_prefs(cx, |p| p.whitespace = w)),
                    );
                }
                menu
            });
        let context = prefs.context;
        let small_btn = |id: &'static str, label: &'static str, enabled: bool| {
            div()
                .id(id)
                .flex_none()
                .size(rpx(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .text_size(ts::BODY)
                .text_color(if enabled { p.fg2 } else { p.fg3.opacity(0.5) })
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(p.hover)))
                .child(label)
        };
        div()
            .h(rpx(30.))
            .flex_none()
            .flex()
            .items_center()
            .gap(rpx(8.))
            .px(rpx(12.))
            .bg(p.panel)
            .border_b_1()
            .border_color(p.bd)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(rpx(6.))
                    .when_some(file, |d, f| {
                        let path = match &f.old_path {
                            Some(old) => {
                                format!("{} → {}", old.to_string_lossy(), f.path.to_string_lossy())
                            }
                            None => f.path.to_string_lossy().into_owned(),
                        };
                        d.child(status_badge(f.status, p)).child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family(MONO)
                                .text_size(ts::SMALL)
                                .text_color(p.fg)
                                .child(SharedString::from(path)),
                        )
                    })
                    .when_some(stats, |d, (added, removed)| {
                        d.child(
                            div()
                                .flex_none()
                                .font_family(MONO)
                                .text_size(ts::SMALL)
                                .text_color(p.added)
                                .child(format!("+{added}")),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(MONO)
                                .text_size(ts::SMALL)
                                .text_color(p.deleted)
                                .child(format!("−{removed}")),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(rpx(2.))
                    .text_size(ts::SMALL)
                    .text_color(p.fg2)
                    .child(
                        small_btn("ctx-less", "−", context > 0).on_click(|_, _, cx| {
                            set_prefs(cx, |p| p.context = p.context.saturating_sub(1))
                        }),
                    )
                    .child(
                        div()
                            .min_w(rpx(64.))
                            .flex()
                            .justify_center()
                            .child(SharedString::from(format!("{context} lines"))),
                    )
                    .child(
                        small_btn("ctx-more", "+", context < DiffPrefs::MAX_CONTEXT).on_click(
                            |_, _, cx| {
                                set_prefs(cx, |p| {
                                    p.context = (p.context + 1).min(DiffPrefs::MAX_CONTEXT)
                                })
                            },
                        ),
                    ),
            )
            .child(ws_menu)
            .child(ui::segmented(
                "diff-layout",
                vec![
                    (
                        "Inline".into(),
                        !prefs.side_by_side,
                        Box::new(|_, _, cx: &mut App| set_prefs(cx, |p| p.side_by_side = false)),
                    ),
                    (
                        "Side by side".into(),
                        prefs.side_by_side,
                        Box::new(|_, _, cx: &mut App| set_prefs(cx, |p| p.side_by_side = true)),
                    ),
                ],
                18.,
                p,
            ))
            .into_any_element()
    }

    fn render_diff(&self, p: &Palette, row_h: f32, cx: &mut Context<Self>) -> AnyElement {
        let message = |text: String, color: Hsla| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(rpx(16.))
                .text_color(color)
                .child(SharedString::from(text))
                .into_any_element()
        };
        let Some(loaded) = self.loaded.clone() else {
            return if self.diff_loading || self.loading {
                div().flex_1().into_any_element()
            } else if self.files.is_empty() && self.info.is_some() {
                message("This commit changes no files.".into(), p.fg3)
            } else {
                message("Select a file to see its changes.".into(), p.fg3)
            };
        };
        let side_by_side = loaded.prefs.side_by_side;
        match &loaded.content {
            Content::Binary { old, new } => {
                message(format!("Binary file ({})", sizes(*old, *new)), p.fg3)
            }
            Content::TooLarge { old, new } => message(
                format!("File too large to show ({})", sizes(*old, *new)),
                p.fg3,
            ),
            Content::Submodule { old, new } => message(
                format!(
                    "Submodule {} → {}",
                    old.map_or("none".into(), |o| switchhead_git::short(&o)),
                    new.map_or("none".into(), |o| switchhead_git::short(&o)),
                ),
                p.fg2,
            ),
            Content::Failed(e) => message(e.clone(), p.deleted),
            Content::Text { diff, .. } if diff.is_empty() => message(
                if loaded.prefs.whitespace == WhitespacePref::Show {
                    "No text changes (mode or rename only).".into()
                } else {
                    "Only whitespace changes.".into()
                },
                p.fg3,
            ),
            Content::Text {
                inline,
                sbs,
                widest,
                ..
            } => {
                let count = if side_by_side {
                    sbs.len()
                } else {
                    inline.len()
                };
                let widest = *widest;
                let list = uniform_list(
                    "diff",
                    count,
                    cx.processor(move |this, range: Range<usize>, _w, cx| {
                        let p = palette(cx);
                        let Some(loaded) = this.loaded.clone() else {
                            return Vec::new();
                        };
                        range
                            .map(|i| render_diff_row(&loaded, i, &p, row_h))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.diff_scroll)
                .flex_1()
                .min_h_0()
                .font_family(crate::appearance::editor_font_family(cx))
                .text_size(crate::appearance::editor_font_size(cx));
                if side_by_side {
                    list.into_any_element()
                } else {
                    list.with_width_from_item(widest)
                        .with_horizontal_sizing_behavior(
                            ListHorizontalSizingBehavior::Unconstrained,
                        )
                        .into_any_element()
                }
            }
        }
    }
}

/// Read and diff one file (on a background thread).
fn compute(repo: &Repo, f: &FileChange, prefs: &DiffPrefs, theme: &Arc<HighlightTheme>) -> Content {
    if f.submodule {
        return Content::Submodule {
            old: f.old_id,
            new: f.new_id,
        };
    }
    let read = |id: Option<Oid>| id.map(|id| diff::read_blob(repo, id)).transpose();
    let (old, new) = match (read(f.old_id), read(f.new_id)) {
        (Ok(o), Ok(n)) => (o, n),
        (Err(e), _) | (_, Err(e)) => return Content::Failed(e.to_string()),
    };
    let started = std::time::Instant::now();
    let content = match diff::diff_bytes(old.as_deref(), new.as_deref(), &prefs.options()) {
        FileDiff::Binary { old_size, new_size } => Content::Binary {
            old: old_size,
            new: new_size,
        },
        FileDiff::TooLarge { old_size, new_size } => Content::TooLarge {
            old: old_size,
            new: new_size,
        },
        FileDiff::Text(d) => {
            let inline = diff_model::inline_rows(&d);
            let sbs = diff_model::sbs_rows(&d);
            let widest = diff_model::widest_row(&d, &inline);
            let syntax = diff_model::language_for(&f.path).and_then(|lang| {
                let text = |b: &Option<Vec<u8>>| {
                    String::from_utf8_lossy(b.as_deref().unwrap_or_default()).into_owned()
                };
                diff_model::highlight(&d, &text(&old), &text(&new), lang, theme)
            });
            Content::Text {
                diff: d,
                inline,
                sbs,
                widest,
                syntax,
            }
        }
    };
    tracing::debug!(
        path = %f.path.display(),
        ms = started.elapsed().as_millis() as u64,
        "file diff"
    );
    content
}

fn render_diff_row(loaded: &Loaded, i: usize, p: &Palette, row_h: f32) -> AnyElement {
    let Content::Text {
        diff,
        inline,
        sbs,
        syntax,
        ..
    } = &loaded.content
    else {
        return div().into_any_element();
    };
    let row = div()
        .id(("diff", i))
        .w_full()
        .h(px(row_h))
        .flex()
        .items_center();
    let hunk_row = |h: u32| {
        let Some(h) = diff.hunks().get(h as usize) else {
            return div().into_any_element();
        };
        let mut text = format!(
            "@@ -{},{} +{},{} @@",
            h.old_start, h.old_lines, h.new_start, h.new_lines
        );
        if !h.section.is_empty() {
            text.push(' ');
            text.push_str(&h.section);
        }
        div()
            .id(("diff", i))
            .w_full()
            .h(px(row_h))
            .flex()
            .items_center()
            .px(rpx(8.))
            .bg(p.panel)
            .text_color(p.fg3)
            .whitespace_nowrap()
            .child(SharedString::from(text))
            .into_any_element()
    };
    if loaded.prefs.side_by_side {
        match sbs.get(i) {
            Some(SbsRow::Hunk(h)) => hunk_row(*h),
            Some(SbsRow::Pair(old, new)) => row
                .child(side_cell(diff, syntax.as_ref(), *old, true, p))
                .child(div().w(px(1.)).h_full().flex_none().bg(p.bd))
                .child(side_cell(diff, syntax.as_ref(), *new, false, p))
                .into_any_element(),
            None => row.into_any_element(),
        }
    } else {
        match inline.get(i) {
            Some(Row::Hunk(h)) => hunk_row(*h),
            Some(Row::Line(l)) => {
                let l = *l as usize;
                let line = diff.line(l);
                let (bg, sign) = line_colors(line.kind, p);
                row.when_some(bg, |d, bg| d.bg(bg))
                    .child(gutter(line.old_no, p))
                    .child(gutter(line.new_no, p))
                    .child(
                        div()
                            .w(rpx(16.))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .text_color(sign)
                            .child(line.kind.prefix().to_string()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .pr(rpx(16.))
                            .whitespace_nowrap()
                            .text_color(p.fg)
                            .child(line_text(diff, syntax.as_ref(), l, p)),
                    )
                    .into_any_element()
            }
            None => row.into_any_element(),
        }
    }
}

/// One half of a side-by-side row.
fn side_cell(
    diff: &TextDiff,
    syntax: Option<&Syntax>,
    line: Option<u32>,
    old: bool,
    p: &Palette,
) -> AnyElement {
    let cell = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .overflow_hidden();
    let Some(l) = line else {
        return cell.bg(p.panel.opacity(0.6)).into_any_element();
    };
    let l = l as usize;
    let d = diff.line(l);
    let (bg, sign) = line_colors(d.kind, p);
    cell.when_some(bg, |c, bg| c.bg(bg))
        .child(gutter(if old { d.old_no } else { d.new_no }, p))
        .child(
            div()
                .w(rpx(14.))
                .flex_none()
                .flex()
                .justify_center()
                .text_color(sign)
                .child(if d.kind == LineKind::Context {
                    String::new()
                } else {
                    d.kind.prefix().to_string()
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(p.fg)
                .child(line_text(diff, syntax, l, p)),
        )
        .into_any_element()
}

/// A line's text with syntax colors and changed-word backgrounds.
fn line_text(diff: &TextDiff, syntax: Option<&Syntax>, l: usize, p: &Palette) -> StyledText {
    let kind = diff.line(l).kind;
    let words: Vec<Range<usize>> = diff.word_ranges(l).collect();
    let word = HighlightStyle {
        background_color: Some(match kind {
            LineKind::Added => p.added.opacity(0.24),
            _ => p.deleted.opacity(0.24),
        }),
        ..Default::default()
    };
    let spans =
        diff_model::merge_styles(syntax.map(|s| s.line(l)).unwrap_or_default(), &words, word);
    let (text, spans) = diff_model::expand_tabs(diff.text(l), spans);
    StyledText::new(text).with_highlights(spans)
}

/// Line-number cell.
fn gutter(no: Option<u32>, p: &Palette) -> AnyElement {
    div()
        .w(rpx(44.))
        .flex_none()
        .pr(rpx(8.))
        .flex()
        .justify_end()
        .text_color(p.fg3)
        .child(no.map(|n| n.to_string()).unwrap_or_default())
        .into_any_element()
}

/// Row background and the `+`/`-` color for a line kind.
fn line_colors(kind: LineKind, p: &Palette) -> (Option<Hsla>, Hsla) {
    match kind {
        LineKind::Context => (None, p.fg3),
        LineKind::Added => (Some(p.added_bg), p.added),
        LineKind::Removed => (Some(p.deleted_bg), p.deleted),
    }
}

/// The one-letter status badge of a changed file.
fn status_badge(s: FileStatus, p: &Palette) -> AnyElement {
    let color = match s {
        FileStatus::Added | FileStatus::Copied => p.added,
        FileStatus::Deleted => p.deleted,
        FileStatus::Renamed => p.acc,
        _ => p.modified,
    };
    div()
        .flex_none()
        .size(rpx(15.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .bg(color.opacity(0.16))
        .text_color(color)
        .font_family(MONO)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(ts::CAPTION)
        .child(s.letter().to_string())
        .into_any_element()
}

/// Change the diff settings (every tab follows; the workspace saves them).
fn set_prefs(cx: &mut App, f: impl FnOnce(&mut DiffPrefs)) {
    let mut prefs = *cx.global::<DiffPrefs>();
    f(&mut prefs);
    if prefs != *cx.global::<DiffPrefs>() {
        cx.set_global(prefs);
    }
}

fn highlight_theme(cx: &App) -> Arc<HighlightTheme> {
    cx.theme().highlight_theme.clone()
}

/// Identity of a highlight theme (a new theme is a new `Arc`).
fn theme_key(t: &Arc<HighlightTheme>) -> usize {
    Arc::as_ptr(t) as usize
}

/// `src/app/main.rs` → (`Some("src/app")`, `main.rs`).
fn split_path(path: &str) -> (Option<SharedString>, SharedString) {
    match path.rsplit_once('/') {
        Some((dir, file)) => (Some(dir.to_owned().into()), file.to_owned().into()),
        None => (None, path.to_owned().into()),
    }
}

/// "1.2 KB → 3.4 KB", or one size for an added or deleted file.
fn sizes(old: Option<u64>, new: Option<u64>) -> String {
    match (old, new) {
        (Some(o), Some(n)) => format!("{} → {}", ui::bytes(o), ui::bytes(n)),
        (Some(s), None) | (None, Some(s)) => ui::bytes(s),
        (None, None) => String::new(),
    }
}

/// Join the hard-wrapped lines of each paragraph so the text wraps to the pane. Lines
/// that start a list item or are indented keep their own line.
fn reflow(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut prev_blank = true;
    for line in body.lines() {
        let blank = line.trim().is_empty();
        let keeps_line = line.starts_with([' ', '\t'])
            || ["- ", "* ", "• ", "> "].iter().any(|m| line.starts_with(m))
            || line
                .split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            || is_trailer(line);
        if !out.is_empty() {
            out.push(if blank || prev_blank || keeps_line {
                '\n'
            } else {
                ' '
            });
        }
        if !blank {
            out.push_str(line.trim_end());
        }
        prev_blank = blank;
    }
    out
}

/// `Signed-off-by: …`, `Co-Authored-By: …`: git trailers keep their own lines.
fn is_trailer(line: &str) -> bool {
    line.split_once(": ").is_some_and(|(key, _)| {
        !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// A full local date: "Tue, Oct 14 2026 15:02".
fn full_date(secs: i64) -> SharedString {
    use chrono::{Local, TimeZone as _};
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|t| t.format("%a, %b %-d %Y %H:%M").to_string())
        .unwrap_or_default()
        .into()
}

impl Focusable for CommitView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CommitView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zoom = crate::appearance::zoom(cx);
        let font: f32 = crate::appearance::editor_font_size(cx).into();
        let row_h = (ROW_H * zoom).max(font * 1.45).round();
        let file_row_h = (22. * zoom).round();
        let root = div()
            .id("commit-view")
            .key_context("Files")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(p.surface)
            .border_t_1()
            .border_color(p.bd)
            .on_action(cx.listener(|this, _: &actions::SelectUp, _, cx| this.move_file(-1, cx)))
            .on_action(cx.listener(|this, _: &actions::SelectDown, _, cx| this.move_file(1, cx)))
            .on_action(cx.listener(|this, _: &actions::SelectFirst, _, cx| {
                this.move_file(isize::MIN / 2, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::SelectLast, _, cx| {
                this.move_file(isize::MAX / 2, cx)
            }));
        let Some(info) = self.info.clone() else {
            let text: SharedString = match (&self.error, self.commit, self.loading) {
                (Some(e), _, _) => e.clone(),
                (None, None, _) => "Select a commit to see its details.".into(),
                (None, Some(_), _) => SharedString::default(),
            };
            return root
                .items_center()
                .justify_center()
                .text_color(if self.error.is_some() {
                    p.deleted
                } else {
                    p.fg3
                })
                .child(text)
                .into_any_element();
        };
        let rem = window.rem_size();
        let left = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(self.render_header(&info, &p, cx))
            .child(self.render_files(&p, file_row_h, cx));
        let right = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(self.render_diff_toolbar(&p, cx))
            .when(self.diff_loading && self.loaded.is_some(), |d| {
                d.child(ui::loading_bar("diff-loading", &p))
            })
            .child(self.render_diff(&p, row_h, cx));
        root.child(
            h_resizable("commit-details")
                .with_state(&self.split)
                .child(
                    resizable_panel()
                        .size(rem * (340. / 13.))
                        .size_range(rem * (200. / 13.)..rem * (900. / 13.))
                        .child(left),
                )
                .child(resizable_panel().child(right)),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_paths_and_sizes() {
        let (dir, file) = split_path("src/app/main.rs");
        assert_eq!(dir.as_deref(), Some("src/app"));
        assert_eq!(file.as_ref(), "main.rs");
        assert_eq!(split_path("README").0, None);
        assert_eq!(sizes(None, Some(2048)), ui::bytes(2048));
        assert!(sizes(Some(1), Some(2)).contains('→'));
    }

    #[test]
    fn reflows_paragraphs_but_keeps_lists() {
        let body =
            "One line\nwrapped here.\n\n- item a\n- item b\n  more b\n1. first\nSigned-off-by: X";
        assert_eq!(
            reflow(body),
            "One line wrapped here.\n\n- item a\n- item b\n  more b\n1. first\nSigned-off-by: X"
        );
    }
}
