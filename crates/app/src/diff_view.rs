//! The diff body shared by the commit details pane and Local Changes: the loaded
//! [`Content`] of one file, its virtualized line list (inline or side by side, with word
//! highlights and syntax colors), and the diff settings controls.

use std::collections::HashSet;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, FontWeight, HighlightStyle, Hsla, InteractiveElement as _,
    IntoElement, ListHorizontalSizingBehavior, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, UniformListScrollHandle, Window, div,
    px, uniform_list,
};
use switchhead_git::Oid;
use switchhead_git::diff::{FileDiff, FileStatus, LineKind, TextDiff};

use crate::appearance::{rpx, ts};
use crate::diff_model::{self, Mark, Row, SbsRow, Syntax};
use crate::store::{DiffPrefs, WhitespacePref};
use crate::theme::{MONO, Palette, palette};
use crate::ui;

/// What the selected file's diff shows.
pub(crate) enum Content {
    Text {
        diff: TextDiff,
        inline: Vec<Row>,
        sbs: Vec<SbsRow>,
        widest: Option<usize>,
        syntax: Option<Syntax>,
        /// Minimap marks for the inline and side-by-side rows.
        marks: Arc<Vec<Mark>>,
        sbs_marks: Arc<Vec<Mark>>,
    },
    Binary {
        old: Option<u64>,
        new: Option<u64>,
        /// What the file is, from its first bytes.
        kind: Option<&'static str>,
    },
    Image {
        old: Option<crate::image_diff::ImageSide>,
        new: Option<crate::image_diff::ImageSide>,
    },
    TooLarge {
        old: Option<u64>,
        new: Option<u64>,
    },
    Submodule {
        old: Option<Oid>,
        new: Option<Oid>,
    },
    /// An LFS file whose object (on at least one side) is not downloaded: each side's
    /// size, and whether it is in the local store.
    Lfs {
        old: Option<(u64, bool)>,
        new: Option<(u64, bool)>,
    },
    Failed(String),
}

/// A loaded file diff and what it was computed with.
pub(crate) struct Loaded {
    /// Which file (the owner's own key).
    pub file: usize,
    pub prefs: DiffPrefs,
    pub theme: usize,
    pub content: Content,
}

impl Content {
    /// Content for a text diff: rows for both layouts, and syntax colors when the
    /// language is known and both sides' text is given.
    pub(crate) fn text(
        d: TextDiff,
        path: &Path,
        sides: Option<(&[u8], &[u8])>,
        theme: &Arc<HighlightTheme>,
    ) -> Self {
        let inline = diff_model::inline_rows(&d);
        let sbs = diff_model::sbs_rows(&d);
        let widest = diff_model::widest_row(&d, &inline);
        let marks = Arc::new(diff_model::inline_marks(&d, &inline));
        let sbs_marks = Arc::new(diff_model::sbs_marks(&d, &sbs));
        let syntax = sides.and_then(|(old, new)| {
            let lang = diff_model::language_for(path)?;
            diff_model::highlight(
                &d,
                &String::from_utf8_lossy(old),
                &String::from_utf8_lossy(new),
                lang,
                theme,
            )
        });
        Content::Text {
            diff: d,
            inline,
            sbs,
            widest,
            syntax,
            marks,
            sbs_marks,
        }
    }

    /// Content for a diff of two blobs (either side missing for an add or delete).
    pub(crate) fn from_bytes(
        path: &Path,
        old: Option<&[u8]>,
        new: Option<&[u8]>,
        prefs: &DiffPrefs,
        theme: &Arc<HighlightTheme>,
    ) -> Self {
        let d = switchhead_git::diff::diff_bytes(old, new, &prefs.options());
        if matches!(d, FileDiff::Binary { .. } | FileDiff::TooLarge { .. })
            && let Some((old, new)) = crate::image_diff::sides(path, old, new)
        {
            return Content::Image { old, new };
        }
        match d {
            FileDiff::Binary { old_size, new_size } => Content::Binary {
                old: old_size,
                new: new_size,
                kind: new
                    .or(old)
                    .and_then(|b| crate::image_diff::kind_of(&b[..b.len().min(64)])),
            },
            FileDiff::TooLarge { old_size, new_size } => Content::TooLarge {
                old: old_size,
                new: new_size,
            },
            FileDiff::Text(d) => Content::text(
                d,
                path,
                Some((old.unwrap_or_default(), new.unwrap_or_default())),
                theme,
            ),
        }
    }

    /// [`Content::from_bytes`] looking through LFS pointers: a side whose object is in
    /// the local store is diffed as its real content; when one is not downloaded, the
    /// file shows as [`Content::Lfs`].
    pub(crate) fn from_sides(
        repo: &switchhead_git::Repo,
        path: &Path,
        old: Option<Vec<u8>>,
        new: Option<Vec<u8>>,
        prefs: &DiffPrefs,
        theme: &Arc<HighlightTheme>,
    ) -> Self {
        use switchhead_git::lfs::Side;
        let old = old.map(|b| repo.lfs_side(b));
        let new = new.map(|b| repo.lfs_side(b));
        let missing = |s: &Option<Side>| matches!(s, Some(Side::Missing(_)));
        if missing(&old) || missing(&new) {
            let info = |s: Option<Side>| {
                s.map(|s| match s {
                    Side::Missing(p) => (p.size, false),
                    Side::Content(b) => (b.len() as u64, true),
                })
            };
            return Content::Lfs {
                old: info(old),
                new: info(new),
            };
        }
        fn bytes(s: &Option<Side>) -> Option<&[u8]> {
            match s {
                Some(Side::Content(b)) => Some(b.as_slice()),
                _ => None,
            }
        }
        Self::from_bytes(path, bytes(&old), bytes(&new), prefs, theme)
    }

    /// Added and removed line counts of a text diff.
    pub(crate) fn stats(&self) -> Option<(usize, usize)> {
        match self {
            Content::Text { diff, .. } => Some(diff.stats()),
            _ => None,
        }
    }
}

/// Click handler for a diff line (its index in the [`TextDiff`]).
pub(crate) type OnLine = Rc<dyn Fn(usize, &ClickEvent, &mut Window, &mut App)>;
/// Buttons for a hunk header (the hunk's index).
pub(crate) type HunkButtons = Rc<dyn Fn(usize, &Palette) -> Vec<AnyElement>>;

/// Makes a diff interactive (Local Changes): changed lines can be clicked and show as
/// selected, and hunk headers carry buttons.
pub(crate) struct Interact {
    /// Selected lines (indices into the [`TextDiff`]).
    pub selected: HashSet<usize>,
    /// A changed line was clicked.
    pub on_line: OnLine,
    /// The buttons at the start of each hunk header.
    pub hunk_buttons: HunkButtons,
}

/// A centered note in the diff area.
pub(crate) fn message(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p(rpx(16.))
        .text_color(color)
        .child(text.into())
        .into_any_element()
}

/// The diff of `loaded`: a virtualized line list, or a note for binary, large or
/// unchanged files.
pub(crate) fn body(
    loaded: &Rc<Loaded>,
    scroll: &UniformListScrollHandle,
    row_h: f32,
    p: &Palette,
    interact: Option<Rc<Interact>>,
    cx: &App,
) -> AnyElement {
    let side_by_side = loaded.prefs.side_by_side;
    match &loaded.content {
        Content::Binary { old, new, kind } => message(
            format!(
                "{} ({}{})",
                kind.unwrap_or("Binary file"),
                sizes(*old, *new),
                growth(*old, *new)
            ),
            p.fg3,
        ),
        Content::Image { old, new } => crate::image_diff::render(old.as_ref(), new.as_ref(), p, cx),
        Content::TooLarge { old, new } => message(
            format!("File too large to show ({})", sizes(*old, *new)),
            p.fg3,
        ),
        Content::Lfs { old, new } => message(lfs_note(*old, *new), p.fg3),
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
                "No text changes (mode or rename only)."
            } else {
                "Only whitespace changes."
            },
            p.fg3,
        ),
        Content::Text {
            inline,
            sbs,
            widest,
            marks,
            sbs_marks,
            ..
        } => {
            let (count, marks) = if side_by_side {
                (sbs.len(), sbs_marks.clone())
            } else {
                (inline.len(), marks.clone())
            };
            let map = crate::minimap::minimap(marks, count, row_h, scroll, p);
            let widest = *widest;
            let loaded = loaded.clone();
            let list = uniform_list("diff", count, move |range: Range<usize>, _w, cx| {
                let p = palette(cx);
                range
                    .map(|i| render_row(&loaded, i, &p, row_h, interact.as_deref()))
                    .collect::<Vec<_>>()
            })
            .track_scroll(scroll)
            .flex_1()
            .min_w_0()
            .h_full()
            .font_family(crate::appearance::editor_font_family(cx))
            .text_size(crate::appearance::editor_font_size(cx));
            let list = if side_by_side {
                list.into_any_element()
            } else {
                list.with_width_from_item(widest)
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .into_any_element()
            };
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(list)
                .child(map)
                .into_any_element()
        }
    }
}

/// `+12 −3` in the toolbar.
pub(crate) fn stats_chips(added: usize, removed: usize, p: &Palette) -> [AnyElement; 2] {
    [
        div()
            .flex_none()
            .font_family(MONO)
            .text_size(ts::SMALL)
            .text_color(p.added)
            .child(format!("+{added}"))
            .into_any_element(),
        div()
            .flex_none()
            .font_family(MONO)
            .text_size(ts::SMALL)
            .text_color(p.deleted)
            .child(format!("−{removed}"))
            .into_any_element(),
    ]
}

/// The diff settings at the right of a diff toolbar: context lines, whitespace (when
/// `whitespace` is offered) and the inline / side-by-side switch.
pub(crate) fn settings(p: &Palette, whitespace: bool, cx: &App) -> Div {
    let prefs = *cx.global::<DiffPrefs>();
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
        .flex_none()
        .flex()
        .items_center()
        .gap(rpx(8.))
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
        .when(whitespace, |d| d.child(ws_menu))
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
}

/// Diff row height for the current zoom and editor font.
pub(crate) fn row_height(cx: &App) -> f32 {
    let zoom = crate::appearance::zoom(cx);
    let font: f32 = crate::appearance::editor_font_size(cx).into();
    (ROW_H * zoom).max(font * 1.45).round()
}

/// Diff row height at 100 % zoom.
const ROW_H: f32 = 20.0;

fn render_row(
    loaded: &Loaded,
    i: usize,
    p: &Palette,
    row_h: f32,
    interact: Option<&Interact>,
) -> AnyElement {
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
    let hunk_row = |hunk: u32| {
        let Some(h) = diff.hunks().get(hunk as usize) else {
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
        let buttons = interact
            .map(|x| (x.hunk_buttons)(hunk as usize, p))
            .unwrap_or_default();
        div()
            .id(("diff", i))
            .w_full()
            .h(px(row_h))
            .flex()
            .items_center()
            .gap(rpx(6.))
            .px(rpx(8.))
            .bg(p.panel)
            .text_color(p.fg3)
            .whitespace_nowrap()
            .children(buttons)
            .child(SharedString::from(text))
            .into_any_element()
    };
    if loaded.prefs.side_by_side {
        match sbs.get(i) {
            Some(SbsRow::Hunk(h)) => hunk_row(*h),
            Some(SbsRow::Pair(old, new)) => row
                .child(side_cell(diff, syntax.as_ref(), *old, true, p, interact))
                .child(div().w(px(1.)).h_full().flex_none().bg(p.bd))
                .child(side_cell(diff, syntax.as_ref(), *new, false, p, interact))
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
                let row = clickable(row, line.kind, l, interact, p);
                row.when_some(bg, |d, bg| d.bg(bg))
                    .when(selected(interact, l), |d| d.bg(sign.opacity(0.34)))
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
    interact: Option<&Interact>,
) -> AnyElement {
    let cell = div()
        .id(("cell", usize::from(old) | (line.unwrap_or(0) as usize) << 1))
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
    let cell = clickable(cell, d.kind, l, interact, p);
    cell.when_some(bg, |c, bg| c.bg(bg))
        .when(selected(interact, l), |c| c.bg(sign.opacity(0.34)))
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

/// Hook a changed line's clicks up to [`Interact::on_line`].
fn clickable(
    el: gpui_kit::Stateful<Div>,
    kind: LineKind,
    l: usize,
    interact: Option<&Interact>,
    p: &Palette,
) -> gpui_kit::Stateful<Div> {
    match interact {
        Some(x) if kind != LineKind::Context => {
            let on_line = x.on_line.clone();
            let hover = p.hover;
            el.cursor_pointer()
                .hover(move |s| s.border_color(hover))
                .on_click(move |ev, window, cx| on_line(l, ev, window, cx))
        }
        _ => el,
    }
}

fn selected(interact: Option<&Interact>, l: usize) -> bool {
    interact.is_some_and(|x| x.selected.contains(&l))
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
pub(crate) fn status_badge(s: FileStatus, p: &Palette) -> AnyElement {
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
pub(crate) fn set_prefs(cx: &mut App, f: impl FnOnce(&mut DiffPrefs)) {
    let mut prefs = *cx.global::<DiffPrefs>();
    f(&mut prefs);
    if prefs != *cx.global::<DiffPrefs>() {
        cx.set_global(prefs);
    }
}

pub(crate) fn highlight_theme(cx: &App) -> Arc<HighlightTheme> {
    cx.theme().highlight_theme.clone()
}

/// Identity of a highlight theme (a new theme is a new `Arc`).
pub(crate) fn theme_key(t: &Arc<HighlightTheme>) -> usize {
    Arc::as_ptr(t) as usize
}

/// "1.2 KB → 3.4 KB", or one size for an added or deleted file.
pub(crate) fn sizes(old: Option<u64>, new: Option<u64>) -> String {
    match (old, new) {
        (Some(o), Some(n)) => format!("{} → {}", ui::bytes(o), ui::bytes(n)),
        (Some(s), None) | (None, Some(s)) => ui::bytes(s),
        (None, None) => String::new(),
    }
}

/// The note for an LFS file that cannot be compared yet.
fn lfs_note(old: Option<(u64, bool)>, new: Option<(u64, bool)>) -> String {
    let sizes = sizes(old.map(|s| s.0), new.map(|s| s.0));
    let which = match (old, new) {
        (Some((_, false)), Some((_, false))) | (None, _) | (_, None) => "is",
        (Some((_, false)), _) => "before this change is",
        _ => "after this change is",
    };
    format!(
        "LFS file ({sizes}). Its content {which} not downloaded: run Fetch LFS Objects \
         (this branch) or Fetch All LFS Objects (every commit), then select the file again."
    )
}

/// How much a file grew or shrank (" , +1.2 KB"), when both sizes are known and differ.
pub(crate) fn growth(old: Option<u64>, new: Option<u64>) -> String {
    match (old, new) {
        (Some(o), Some(n)) if n > o => format!(", +{}", ui::bytes(n - o)),
        (Some(o), Some(n)) if o > n => format!(", −{}", ui::bytes(o - n)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_missing_lfs_content() {
        assert!(
            lfs_note(None, Some((2048, false)))
                .starts_with("LFS file (2.0 KB). Its content is not")
        );
        assert!(lfs_note(Some((1, false)), Some((2, true))).contains("before this change is not"));
        assert!(lfs_note(Some((1, true)), Some((2, false))).contains("after this change is not"));
    }

    #[test]
    fn formats_growth() {
        assert_eq!(growth(Some(1), Some(1)), "");
        assert_eq!(growth(None, Some(5)), "");
        assert_eq!(growth(Some(1), Some(3)), format!(", +{}", ui::bytes(2)));
        assert_eq!(growth(Some(3), Some(1)), format!(", −{}", ui::bytes(2)));
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(sizes(None, Some(2048)), ui::bytes(2048));
        assert!(sizes(Some(1), Some(2)).contains('→'));
        assert_eq!(sizes(None, None), "");
    }
}
