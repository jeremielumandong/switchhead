//! The file lists' context menu and what only it offers: discard changes, delete untracked
//! files, add a pattern to `.gitignore`, and assume unchanged / skip worktree (with the
//! "hidden" menu in the Unstaged header that turns them off again).

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::{App, ClipboardItem, Context, IntoElement, WeakEntity, Window};
use switchhead_git::status::{Change, EntryKind, HiddenFlag};

use super::{ChangesEvent, ChangesView};
use crate::changes_model::{Side, ignore_patterns};

/// What the selection allows, worked out when the menu opens.
#[derive(Default)]
struct Target {
    side: Option<Side>,
    paths: Vec<PathBuf>,
    /// Tracked files with work-tree changes (discard restores them).
    tracked: Vec<PathBuf>,
    /// Untracked files (discard deletes them).
    untracked: Vec<PathBuf>,
    /// Tracked files that can be hidden from status.
    hideable: Vec<PathBuf>,
}

impl ChangesView {
    fn target(&self) -> Target {
        let side = self.selection.side;
        let mut t = Target {
            side: Some(side),
            ..Target::default()
        };
        for ix in self.lists.get(side) {
            let Some(e) = self.status.entries.get(*ix) else {
                continue;
            };
            if !self.selection.paths.contains(&e.path) {
                continue;
            }
            t.paths.push(e.path.clone());
            // A submodule's changes live in its own repository: nothing to discard or hide here.
            if side == Side::Staged || e.conflict().is_some() || e.submodule.is_some() {
                continue;
            }
            match e.kind {
                EntryKind::Untracked => t.untracked.push(e.path.clone()),
                EntryKind::Ordinary | EntryKind::Renamed { .. } => {
                    t.tracked.push(e.path.clone());
                    if e.index != Change::Added {
                        t.hideable.push(e.path.clone());
                    }
                }
                _ => {}
            }
        }
        t
    }

    /// A right-click on row `i` selects it first, unless it is already selected.
    pub(super) fn select_for_menu(&mut self, side: Side, i: usize, cx: &mut Context<Self>) {
        let list = self.paths(side).to_vec();
        if let Some(p) = list.get(i)
            && !self.selection.contains(side, p)
        {
            self.selection.click(side, &list, i);
            self.cursor_changed(cx);
        }
    }

    /// Discard the selection's changes: tracked files go back to the index, untracked
    /// files are deleted. Asks first.
    pub(super) fn discard_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let t = self.target();
        if t.side != Some(Side::Unstaged) || self.busy {
            return;
        }
        let (tracked, untracked) = (t.tracked, t.untracked);
        let (title, ok) = discard_wording(tracked.len(), untracked.len());
        if title.is_empty() {
            return;
        }
        let description = if untracked.is_empty() {
            "The files go back to their staged or committed content. Undo (Ctrl/Cmd+Z) brings \
             the changes back."
        } else {
            "Untracked files are deleted from disk, not moved to the trash. Undo (Ctrl/Cmd+Z) \
             brings them back."
        };
        let label = title.trim_end_matches('?').to_owned();
        let this = cx.entity().downgrade();
        let paths = Rc::new((tracked, untracked));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let paths = paths.clone();
            let label = label.clone();
            alert
                .confirm()
                .title(title.clone())
                .description(description)
                .ok_text(ok)
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let (tracked, untracked) = (*paths).clone();
                    let label = label.clone();
                    let _ = this.update(cx, |v, cx| {
                        v.run(
                            move |r| {
                                let all: Vec<_> = tracked.iter().chain(&untracked).collect();
                                let all: Vec<&std::path::Path> =
                                    all.iter().map(|p| p.as_path()).collect();
                                switchhead_git::undo::guard(r, &label, &all);
                                if !tracked.is_empty() {
                                    r.discard(&refs(&tracked))?;
                                }
                                if !untracked.is_empty() {
                                    r.delete_untracked(&refs(&untracked))?;
                                }
                                Ok(())
                            },
                            cx,
                        )
                    });
                    true
                })
        });
    }

    fn ignore(&mut self, pattern: String, cx: &mut Context<Self>) {
        self.run(move |r| r.ignore(&pattern), cx);
    }

    fn set_hidden(
        &mut self,
        paths: Vec<PathBuf>,
        flag: HiddenFlag,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.run(
            move |r| {
                let refs: Vec<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
                match flag {
                    HiddenFlag::AssumeUnchanged => r.set_assume_unchanged(&refs, on),
                    HiddenFlag::SkipWorktree => r.set_skip_worktree(&refs, on),
                }
            },
            cx,
        );
    }

    /// The Unstaged header's "N hidden" menu, when files are hidden from status.
    pub(super) fn render_hidden_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let n = self.hidden.len();
        if n == 0 {
            return None;
        }
        let view = cx.entity().downgrade();
        let hidden = self.hidden.clone();
        Some(
            Button::new("hidden-files")
                .ghost()
                .xsmall()
                .label(format!("{n} hidden"))
                .dropdown_menu(move |mut menu, _, _| {
                    menu = menu.label("Not checked by status; click to show again");
                    for h in hidden.iter().take(MAX_HIDDEN_ITEMS) {
                        let (path, flag) = (h.path.clone(), h.flag);
                        let what = match flag {
                            HiddenFlag::AssumeUnchanged => "assume unchanged",
                            HiddenFlag::SkipWorktree => "skip worktree",
                        };
                        let view = view.clone();
                        menu = menu.item(
                            PopupMenuItem::new(format!("{}  ({what})", path.display())).on_click(
                                move |_, _, cx| {
                                    let path = path.clone();
                                    let _ = view.update(cx, |v, cx| {
                                        v.set_hidden(vec![path], flag, false, cx)
                                    });
                                },
                            ),
                        );
                    }
                    if hidden.len() > MAX_HIDDEN_ITEMS {
                        menu = menu.label(format!("and {} more", hidden.len() - MAX_HIDDEN_ITEMS));
                    }
                    let view = view.clone();
                    let all = hidden.clone();
                    menu.separator()
                        .item(
                            PopupMenuItem::new("Show All Again").on_click(move |_, _, cx| {
                                let by = |f: HiddenFlag| -> Vec<PathBuf> {
                                    all.iter()
                                        .filter(|h| h.flag == f)
                                        .map(|h| h.path.clone())
                                        .collect()
                                };
                                let (assumed, skipped) = (
                                    by(HiddenFlag::AssumeUnchanged),
                                    by(HiddenFlag::SkipWorktree),
                                );
                                let _ = view.update(cx, |v, cx| {
                                    // Both run in one background op so the second is not dropped
                                    // while the first is busy.
                                    v.run(
                                        move |r| {
                                            if !assumed.is_empty() {
                                                r.set_assume_unchanged(&refs(&assumed), false)?;
                                            }
                                            if !skipped.is_empty() {
                                                r.set_skip_worktree(&refs(&skipped), false)?;
                                            }
                                            Ok(())
                                        },
                                        cx,
                                    )
                                });
                            }),
                        )
                }),
        )
    }
}

fn refs(v: &[PathBuf]) -> Vec<&std::path::Path> {
    v.iter().map(PathBuf::as_path).collect()
}

/// Hidden files listed in the header menu.
const MAX_HIDDEN_ITEMS: usize = 50;

/// The confirmation's title and button for discarding `tracked` changed and deleting
/// `untracked` files ("" when there is nothing to do).
fn discard_wording(tracked: usize, untracked: usize) -> (String, &'static str) {
    let files = |n: usize| {
        if n == 1 {
            "1 file".to_owned()
        } else {
            format!("{n} files")
        }
    };
    match (tracked, untracked) {
        (0, 0) => (String::new(), ""),
        (t, 0) => (format!("Discard changes to {}?", files(t)), "Discard"),
        (0, u) => (format!("Delete {} (untracked)?", files(u)), "Delete"),
        (t, u) => (
            format!(
                "Discard changes to {} and delete {} (untracked)?",
                files(t),
                files(u)
            ),
            "Discard",
        ),
    }
}

/// Build the file lists' context menu for the current selection (see
/// [`ChangesView::select_for_menu`], which runs first).
pub(super) fn build(
    mut menu: PopupMenu,
    view: &WeakEntity<ChangesView>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(v) = view.upgrade() else {
        return menu;
    };
    let t = v.read(cx).target();
    let Some(side) = t.side else {
        return menu;
    };
    if t.paths.is_empty() {
        return menu;
    }
    let n = t.paths.len();
    let count = |verb: &str| {
        if n == 1 {
            verb.to_owned()
        } else {
            format!("{verb} {n} Files")
        }
    };
    let act = |f: fn(&mut ChangesView, &mut Window, &mut Context<ChangesView>)| {
        let view = view.clone();
        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| f(v, window, cx));
        }
    };
    menu = menu.item(
        PopupMenuItem::new(count(match side {
            Side::Unstaged => "Stage",
            Side::Staged => "Unstage",
        }))
        .on_click(act(|v, _, cx| v.toggle_selected(cx))),
    );
    if side == Side::Unstaged {
        let (title, _) = discard_wording(t.tracked.len(), t.untracked.len());
        let label = if t.tracked.is_empty() && !t.untracked.is_empty() {
            count("Delete")
        } else {
            count("Discard Changes")
        };
        menu = menu.item(
            PopupMenuItem::new(format!("{label}…"))
                .disabled(title.is_empty())
                .on_click(act(|v, window, cx| v.discard_selected(window, cx))),
        );
        if let [path] = t.untracked.as_slice()
            && n == 1
        {
            let patterns = ignore_patterns(path);
            let view = view.clone();
            menu = menu
                .separator()
                .submenu("Ignore", window, cx, move |mut m, _, _| {
                    for pat in &patterns {
                        let view = view.clone();
                        let pat = pat.clone();
                        m = m.item(PopupMenuItem::new(pat.clone()).on_click(move |_, _, cx| {
                            let pat = pat.clone();
                            let _ = view.update(cx, |v, cx| v.ignore(pat, cx));
                        }));
                    }
                    m
                });
        }
        if !t.hideable.is_empty() {
            let hide = |label: &str, flag: HiddenFlag| {
                let view = view.clone();
                let paths = t.hideable.clone();
                PopupMenuItem::new(label.to_owned()).on_click(move |_, _, cx| {
                    let paths = paths.clone();
                    let _ = view.update(cx, |v, cx| v.set_hidden(paths, flag, true, cx));
                })
            };
            menu = menu
                .separator()
                .item(hide("Assume Unchanged", HiddenFlag::AssumeUnchanged))
                .item(hide("Skip Worktree", HiddenFlag::SkipWorktree));
        }
    }
    if let [path] = t.paths.as_slice()
        && t.untracked.is_empty()
    {
        let open = |blame: bool| {
            let (view, path) = (view.clone(), path.clone());
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                let path = path.clone();
                let _ = view.update(cx, |_, cx| cx.emit(ChangesEvent::OpenFile { path, blame }));
            }
        };
        let submodule = v
            .read(cx)
            .status
            .entries
            .iter()
            .any(|e| &e.path == path && e.submodule.is_some());
        menu = menu
            .separator()
            .item(PopupMenuItem::new("File History").on_click(open(false)));
        if !submodule {
            menu = menu.item(PopupMenuItem::new("Blame").on_click(open(true)));
        }
        if submodule {
            let emit = |ev: fn(PathBuf) -> ChangesEvent| {
                let (view, path) = (view.clone(), path.clone());
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    let path = path.clone();
                    let _ = view.update(cx, |_, cx| cx.emit(ev(path)));
                }
            };
            menu = menu
                .separator()
                .item(
                    PopupMenuItem::new("Open Submodule")
                        .on_click(emit(ChangesEvent::OpenSubmodule)),
                )
                .item(
                    PopupMenuItem::new("Update Submodule")
                        .on_click(emit(ChangesEvent::UpdateSubmodule)),
                );
        }
        if let Some(locked) = &v.read(cx).lfs {
            let lock = !locked.contains(path);
            let (view, path) = (view.clone(), path.clone());
            menu = menu.separator().item(
                PopupMenuItem::new(if lock {
                    "Lock File (LFS)"
                } else {
                    "Unlock File (LFS)"
                })
                .on_click(move |_, _, cx| {
                    let path = path.clone();
                    let _ = view.update(cx, |_, cx| cx.emit(ChangesEvent::LfsLock { path, lock }));
                }),
            );
        }
    }
    let text = t
        .paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    let menu = menu.separator().item(
        PopupMenuItem::new(if n == 1 { "Copy Path" } else { "Copy Paths" }).on_click(
            move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone())),
        ),
    );
    match t.paths.as_slice() {
        [path] => {
            let (view, path) = (view.clone(), path.clone());
            crate::custom_commands::add_to_menu(
                menu,
                crate::custom_commands::CommandTarget::File,
                false,
                move |index, _, cx| {
                    let path = path.clone();
                    let _ =
                        view.update(cx, |_, cx| cx.emit(ChangesEvent::RunCustom { index, path }));
                },
                cx,
            )
        }
        _ => menu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_the_discard_question() {
        assert_eq!(discard_wording(0, 0).0, "");
        assert_eq!(
            discard_wording(1, 0),
            ("Discard changes to 1 file?".into(), "Discard")
        );
        assert_eq!(
            discard_wording(0, 3),
            ("Delete 3 files (untracked)?".into(), "Delete")
        );
        assert_eq!(
            discard_wording(2, 1).0,
            "Discard changes to 2 files and delete 1 file (untracked)?"
        );
    }
}
