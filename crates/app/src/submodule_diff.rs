//! A submodule's change in a diff pane: its old and new commit, the commits added and
//! removed between them (a virtualized list), and whether its work tree has changes.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement as _, SharedString, Styled as _,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use switchhead_git::submodules::SubmoduleDiff;

use crate::appearance::{editor_font_family, rpx, ts};
use crate::theme::Palette;

/// One line of the list.
enum Row {
    /// The commit move, or a heading.
    Title(SharedString),
    /// A remark (not cloned, uncommitted changes).
    Note(SharedString),
    /// A commit: added (else removed), short id, summary.
    Commit(bool, SharedString, SharedString),
}

/// What the pane lists for `d` at `path`.
fn rows(path: &str, d: &SubmoduleDiff) -> Vec<Row> {
    let id =
        |o: Option<switchhead_git::Oid>| o.map_or("none".into(), |o| switchhead_git::short(&o));
    let mut out = vec![Row::Title(
        match (d.old, d.new) {
            (None, Some(_)) => format!("Submodule {path} added at {}", id(d.new)),
            (Some(_), None) => format!("Submodule {path} removed (was {})", id(d.old)),
            (o, n) if o == n => format!("Submodule {path} at {}", id(n)),
            (o, n) => format!("Submodule {path} {} → {}", id(o), id(n)),
        }
        .into(),
    )];
    match (d.modified, d.untracked) {
        (true, true) => out.push(Row::Note(
            "Its work tree has uncommitted changes and untracked files. Open the submodule \
             to see them."
                .into(),
        )),
        (true, false) => out.push(Row::Note(
            "Its work tree has uncommitted changes. Open the submodule to see them.".into(),
        )),
        (false, true) => out.push(Row::Note(
            "Its work tree has untracked files. Open the submodule to see them.".into(),
        )),
        (false, false) => {}
    }
    if let Some(note) = &d.note {
        out.push(Row::Note(note.clone().into()));
    }
    for added in [true, false] {
        let n = d.commits.iter().filter(|c| c.added == added).count();
        if n == 0 {
            continue;
        }
        let more = if d.truncated { "+" } else { "" };
        let s = if n == 1 { "" } else { "s" };
        out.push(Row::Title(
            if added {
                format!("{n}{more} commit{s} added")
            } else {
                format!("{n}{more} commit{s} removed")
            }
            .into(),
        ));
        out.extend(d.commits.iter().filter(|c| c.added == added).map(|c| {
            Row::Commit(
                added,
                switchhead_git::short(&c.id).into(),
                c.summary.clone().into(),
            )
        }));
    }
    out
}

/// The pane for the submodule at `path`.
pub(crate) fn render(
    path: &str,
    d: &SubmoduleDiff,
    scroll: &UniformListScrollHandle,
    row_h: f32,
    p: &Palette,
    cx: &App,
) -> AnyElement {
    let rows = Rc::new(rows(path, d));
    let (p, mono) = (*p, editor_font_family(cx));
    uniform_list(
        "submodule-diff",
        rows.len(),
        move |range, _: &mut Window, _: &mut App| {
            range
                .filter_map(|i| rows.get(i))
                .map(|row| {
                    let line = div()
                        .h(px(row_h + 4.))
                        .w_full()
                        .px(rpx(16.))
                        .flex()
                        .items_center()
                        .gap(rpx(8.))
                        .text_size(ts::BODY);
                    match row {
                        Row::Title(t) => line.text_color(p.fg).child(t.clone()),
                        Row::Note(t) => line.text_color(p.fg3).child(t.clone()),
                        Row::Commit(added, id, summary) => line
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(if *added { p.added } else { p.deleted })
                                    .child(if *added { "+" } else { "−" }),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(mono.clone())
                                    .text_color(p.fg3)
                                    .child(id.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(p.fg2)
                                    .child(summary.clone()),
                            ),
                    }
                })
                .collect::<Vec<_>>()
        },
    )
    .track_scroll(scroll)
    .py(rpx(8.))
    .size_full()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchhead_git::Oid;
    use switchhead_git::submodules::SubmoduleCommit;

    fn oid(c: char) -> Oid {
        Oid::from_hex(c.to_string().repeat(40).as_bytes()).unwrap()
    }

    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Title(t) | Row::Note(t) => t.to_string(),
                Row::Commit(a, _, s) => format!("{} {s}", if *a { "+" } else { "-" }),
            })
            .collect()
    }

    #[test]
    fn lists_added_then_removed_commits() {
        let d = SubmoduleDiff {
            old: Some(oid('a')),
            new: Some(oid('b')),
            commits: vec![
                SubmoduleCommit {
                    id: oid('c'),
                    summary: "Old".into(),
                    added: false,
                },
                SubmoduleCommit {
                    id: oid('d'),
                    summary: "New".into(),
                    added: true,
                },
            ],
            modified: true,
            ..SubmoduleDiff::default()
        };
        assert_eq!(
            texts(&rows("libs/x", &d)),
            [
                "Submodule libs/x aaaaaaa → bbbbbbb",
                "Its work tree has uncommitted changes. Open the submodule to see them.",
                "1 commit added",
                "+ New",
                "1 commit removed",
                "- Old",
            ]
        );
    }

    #[test]
    fn names_added_submodules_and_notes() {
        let d = SubmoduleDiff {
            new: Some(oid('b')),
            note: Some("not cloned".into()),
            ..SubmoduleDiff::default()
        };
        assert_eq!(
            texts(&rows("x", &d)),
            ["Submodule x added at bbbbbbb", "not cloned"]
        );
    }
}
