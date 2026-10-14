//! Stashes: create with options, show one in the details pane, apply, pop, drop and
//! branch from one (the sidebar's stash menu, the toolbar and the palette).

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, ClickEvent, ClipboardItem, Context, WeakEntity, Window};
use switchhead_git::ops::StashOptions;
use switchhead_git::{Error, Oid, Repo};

use super::RepoView;
use super::branch_ops::ViewAction;
use super::form::Form;

/// Fail when `stash@{index}` is no longer `id` (the list changed since it was shown).
fn check_stash(r: &Repo, index: usize, id: Oid) -> switchhead_git::Result<()> {
    match r.resolve(&format!("stash@{{{index}}}")) {
        Ok(found) if found == id => Ok(()),
        _ => Err(Error::Unsupported(
            "the stash list changed: refresh and try again".into(),
        )),
    }
}

impl RepoView {
    /// Ask for a message and options, then stash local changes.
    pub fn stash_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = Form {
            title: "Stash Changes".into(),
            description: "Put local changes aside and clean the work tree. Bring them back \
                          with Apply or Pop from the Stashes list."
                .into(),
            ok: "Stash",
            danger: false,
            name: None,
            placeholder: "",
            taken: Vec::new(),
            message: Some("Message (optional)"),
            url: None,
            checks: vec![
                ("Include untracked files", false),
                ("Only staged changes", false),
                ("Keep staged changes in the index", false),
            ],
        };
        self.open_form(
            form,
            |v, a, cx| {
                let opts = StashOptions {
                    message: (!a.message.is_empty()).then(|| a.message.clone()),
                    untracked: a.checks[0],
                    staged_only: a.checks[1],
                    keep_index: a.checks[2],
                };
                v.run_op(
                    "Stashing",
                    "Stashed local changes",
                    move |r, _, _| r.stash_push(&opts),
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Show stash `id` in the details pane (stashes are not in the graph).
    pub(super) fn show_stash(&mut self, id: Oid, cx: &mut Context<Self>) {
        self.selected = None;
        self.shown = Some(id);
        let labels = self.decorations.get(&id).cloned().unwrap_or_default();
        self.details
            .update(cx, |d, cx| d.show(Some(id), labels, cx));
        cx.notify();
    }

    /// Apply stash `index` (`pop`: and drop it when it applied cleanly).
    pub(super) fn apply_stash(&mut self, index: usize, id: Oid, pop: bool, cx: &mut Context<Self>) {
        let (label, done) = if pop {
            ("Popping stash", "Stash applied and dropped")
        } else {
            ("Applying stash", "Stash applied")
        };
        self.run_op(
            label,
            done,
            move |r, _, _| {
                check_stash(r, index, id)?;
                r.stash_apply(index, pop)
            },
            cx,
        );
    }

    /// Ask, then drop stash `index`.
    pub(super) fn drop_stash_dialog(
        &mut self,
        index: usize,
        id: Oid,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = Form {
            title: format!("Drop stash@{{{index}}}?"),
            description: format!(
                "\"{message}\" is deleted. Until git cleans up, its commit ({}) can still \
                 bring the changes back.",
                switchhead_git::short(&id)
            ),
            ok: "Drop",
            danger: true,
            name: None,
            placeholder: "",
            taken: Vec::new(),
            message: None,
            url: None,
            checks: Vec::new(),
        };
        self.open_form(
            form,
            move |v, _, cx| {
                v.run_op(
                    "Dropping stash",
                    "Stash dropped",
                    move |r, _, _| {
                        check_stash(r, index, id)?;
                        r.stash_drop(index)
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Ask for a name, then check out a new branch at the stash's base with its changes
    /// (`git stash branch`, which drops the stash).
    pub(super) fn stash_branch_dialog(
        &mut self,
        index: usize,
        id: Oid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = Form {
            title: "New Branch from Stash".into(),
            description: format!(
                "A new branch starts at the commit stash@{{{index}}} was made on, with the \
                 stash's changes applied; the stash is then dropped."
            ),
            ok: "Create Branch",
            danger: false,
            name: Some(String::new()),
            placeholder: "Branch name",
            taken: self.branch_names(),
            message: None,
            url: None,
            checks: Vec::new(),
        };
        self.open_form(
            form,
            move |v, a, cx| {
                v.run_op(
                    "Creating branch",
                    "Branch created from the stash",
                    move |r, _, _| {
                        check_stash(r, index, id)?;
                        r.stash_branch(&a.name, index)
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }
}

/// The sidebar menu for `stash@{index}`.
pub(super) fn build_stash_menu<F, A>(
    menu: PopupMenu,
    index: usize,
    id: Oid,
    act: &F,
    view: &WeakEntity<RepoView>,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu
where
    F: Fn(Box<ViewAction>) -> A,
    A: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    let Some(v) = view.upgrade() else {
        return menu;
    };
    let message = v
        .read(cx)
        .refs
        .stashes
        .iter()
        .find(|s| s.index == index)
        .map(|s| s.message.clone())
        .unwrap_or_default();
    let copy = message.clone();
    menu.item(
        PopupMenuItem::new("Apply").on_click(act(Box::new(move |v, _, cx| {
            v.apply_stash(index, id, false, cx)
        }))),
    )
    .item(
        PopupMenuItem::new("Pop (apply and drop)").on_click(act(Box::new(move |v, _, cx| {
            v.apply_stash(index, id, true, cx)
        }))),
    )
    .item(
        PopupMenuItem::new("New Branch from Stash…").on_click(act(Box::new(
            move |v, window, cx| v.stash_branch_dialog(index, id, window, cx),
        ))),
    )
    .separator()
    .item(
        PopupMenuItem::new("Drop…").on_click(act(Box::new(move |v, window, cx| {
            v.drop_stash_dialog(index, id, message.clone(), window, cx)
        }))),
    )
    .separator()
    .item(
        PopupMenuItem::new("Copy Message").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
        }),
    )
}
