//! Tags: create (lightweight, or annotated with a message), delete (here and on the
//! remote), push one or all, from the commit menu, the sidebar's tag menu and the palette.

use gpui_kit::{Context, Window};
use switchhead_git::ops::PushOptions;

use super::RepoView;
use super::form::Form;

impl RepoView {
    /// New tag at the selected commit (HEAD when nothing else is selected).
    pub fn new_tag_at_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.op, Some(super::OpStatus::Running { .. })) {
            return;
        }
        let (rev, what) = match self.selected.map(|i| self.log.id(i)) {
            Some(id) if Some(id) != self.refs.head => {
                let short = switchhead_git::short(&id);
                (id.to_string(), format!("commit {short}"))
            }
            _ => ("HEAD".to_owned(), self.head_label().to_string()),
        };
        self.new_tag_dialog(rev, what, window, cx);
    }

    /// Ask for a name and an optional message, then tag `rev` (`what` names it).
    pub(super) fn new_tag_dialog(
        &mut self,
        rev: String,
        what: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = self.default_remote();
        let form = Form {
            title: "New Tag".into(),
            description: format!(
                "Tag {what}. With a message the tag is annotated (it records who tagged it \
                 and when); without one it is a lightweight tag.{}",
                remote
                    .as_ref()
                    .map(|r| format!(" The remote is {r}."))
                    .unwrap_or_default()
            ),
            ok: "Create Tag",
            danger: false,
            name: Some(String::new()),
            placeholder: "Tag name, e.g. v1.2.0",
            taken: self.refs.tags.iter().map(|t| t.name.clone()).collect(),
            message: Some("Message (optional)"),
            checks: match &remote {
                Some(_) => vec![("Push it to the remote", false)],
                None => Vec::new(),
            },
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let rev = rev.clone();
                let push = remote
                    .clone()
                    .filter(|_| a.checks.first().copied().unwrap_or(false));
                v.run_op(
                    "Creating tag",
                    "Tag created",
                    move |r, cancel, on_line| {
                        let message = (!a.message.is_empty()).then_some(a.message.as_str());
                        r.create_tag(&a.name, &rev, message)?;
                        if let Some(remote) = push {
                            r.push(&tag_push(&remote, &a.name), cancel, on_line)?;
                        }
                        Ok(())
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Ask, then delete tag `name`, and on the remote too when ticked.
    pub(super) fn delete_tag_dialog(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = self.default_remote();
        let form = Form {
            title: format!("Delete tag {name}?"),
            description: match &remote {
                Some(r) => format!("The commit stays; only the tag goes. The remote is {r}."),
                None => "The commit stays; only the tag goes.".into(),
            },
            ok: "Delete",
            danger: true,
            name: None,
            placeholder: "",
            taken: Vec::new(),
            message: None,
            checks: match &remote {
                Some(_) => vec![("Also delete it on the remote", false)],
                None => Vec::new(),
            },
        };
        self.open_form(
            form,
            move |v, a, cx| {
                let name = name.clone();
                let remote = remote
                    .clone()
                    .filter(|_| a.checks.first().copied().unwrap_or(false));
                v.run_op(
                    "Deleting tag",
                    "Tag deleted",
                    move |r, cancel, on_line| {
                        r.delete_tag(&name)?;
                        if let Some(remote) = remote {
                            r.delete_remote_ref(
                                &remote,
                                &format!("refs/tags/{name}"),
                                cancel,
                                on_line,
                            )?;
                        }
                        Ok(())
                    },
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Push one tag to the default remote.
    pub(super) fn push_tag(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(remote) = self.default_remote() else {
            return;
        };
        self.run_op(
            "Pushing tag",
            "Tag pushed",
            move |r, cancel, on_line| r.push(&tag_push(&remote, &name), cancel, on_line),
            cx,
        );
    }

    /// Push every tag to the default remote (`git push <remote> --tags`).
    pub fn push_all_tags(&mut self, cx: &mut Context<Self>) {
        if matches!(self.op, Some(super::OpStatus::Running { .. })) {
            return;
        }
        let Some(remote) = self.default_remote() else {
            return;
        };
        let opts = PushOptions {
            remote,
            tags: true,
            ..Default::default()
        };
        self.run_op(
            "Pushing tags",
            "Tags pushed",
            move |r, cancel, on_line| r.push(&opts, cancel, on_line),
            cx,
        );
    }
}

/// Push options for one tag.
fn tag_push(remote: &str, name: &str) -> PushOptions {
    PushOptions {
        remote: remote.to_owned(),
        refspec: format!("refs/tags/{name}"),
        ..Default::default()
    }
}
