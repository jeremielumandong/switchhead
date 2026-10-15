//! The sidebar menus of section headers (New Branch…, Add Remote…, New Tag…, Stash…)
//! and folders: a remote's folder manages the remote (fetch, prune, edit, remove).

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, ClickEvent, ClipboardItem, Context, WeakEntity, Window};

use super::RepoView;
use super::branch_ops::ViewAction;
use super::sidebar::{Section, SideRow};

/// The menu for a header or folder row.
pub(super) fn build_section_menu(
    mut menu: PopupMenu,
    row: SideRow,
    view: &WeakEntity<RepoView>,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(v) = view.upgrade() else {
        return menu;
    };
    let (has_remote, busy) = {
        let v = v.read(cx);
        (!v.refs.remote_names().is_empty(), v.busy())
    };
    let act = |f: Box<ViewAction>| {
        let view = view.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| f(v, window, cx));
        }
    };
    let item = |label: &str, f: Box<ViewAction>| {
        PopupMenuItem::new(label.to_owned())
            .disabled(busy)
            .on_click(act(f))
    };
    match row {
        SideRow::Header(section, _) => {
            menu = match section {
                Section::Branches => menu.item(item(
                    "New Branch…",
                    Box::new(|v, w, cx| v.new_branch_at_selection(w, cx)),
                )),
                Section::Remotes => {
                    let m = menu.item(item(
                        "Add Remote…",
                        Box::new(|v, w, cx| v.add_remote_dialog(w, cx)),
                    ));
                    if has_remote {
                        m.item(item(
                            "Fetch All Remotes…",
                            Box::new(|v, w, cx| v.fetch_dialog(w, cx)),
                        ))
                    } else {
                        m
                    }
                }
                Section::Tags => {
                    let m = menu.item(item(
                        "New Tag…",
                        Box::new(|v, w, cx| v.new_tag_at_selection(w, cx)),
                    ));
                    if has_remote {
                        m.item(item(
                            "Push All Tags",
                            Box::new(|v, _, cx| v.push_all_tags(cx)),
                        ))
                    } else {
                        m
                    }
                }
                Section::Stashes => menu.item(item(
                    "Stash Changes…",
                    Box::new(|v, w, cx| v.stash_dialog(w, cx)),
                )),
                Section::Submodules | Section::Worktrees => menu,
                Section::LfsLocks => menu.item(
                    PopupMenuItem::new("Refresh Locks")
                        .on_click(act(Box::new(|v, _, cx| v.load_lfs_locks(cx)))),
                ),
            };
            let open = !v.read(cx).collapsed.contains(&section);
            menu.separator().item(
                PopupMenuItem::new(if open { "Collapse" } else { "Expand" })
                    .on_click(act(Box::new(move |v, _, cx| v.toggle_section(section, cx)))),
            )
        }
        SideRow::Folder {
            key, depth, open, ..
        } => {
            if depth == 0
                && let Some(remote) = key.strip_prefix("r/").map(str::to_owned)
            {
                let url = v.read(cx).refs.remote_url(&remote).map(str::to_owned);
                let (r1, r2, r3, r4) = (
                    remote.clone(),
                    remote.clone(),
                    remote.clone(),
                    remote.clone(),
                );
                menu = menu
                    .item(item(
                        &format!("Fetch {remote}"),
                        Box::new(move |v, _, cx| v.fetch_remote(r1.clone(), cx)),
                    ))
                    .item(item(
                        "Prune Deleted Branches",
                        Box::new(move |v, _, cx| v.prune_remote(r2.clone(), cx)),
                    ))
                    .separator()
                    .item(item(
                        "Edit…",
                        Box::new(move |v, w, cx| v.edit_remote_dialog(r3.clone(), w, cx)),
                    ))
                    .item(item(
                        "Remove…",
                        Box::new(move |v, w, cx| v.remove_remote_dialog(r4.clone(), w, cx)),
                    ));
                if let Some(url) = url {
                    menu = menu
                        .separator()
                        .item(PopupMenuItem::new("Copy URL").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))
                        }));
                }
                menu = menu.separator();
            }
            menu.item(
                PopupMenuItem::new(if open { "Collapse" } else { "Expand" })
                    .on_click(act(Box::new(move |v, _, cx| v.toggle_folder(&key, cx)))),
            )
        }
        SideRow::Ref { .. } => menu,
    }
}
