//! Running custom commands in a repository tab: their variables come from the tab (the
//! repository, the current branch and its remote) and from what the command was run on (a
//! commit, a branch or tag, a file). Output goes to the activity log as it arrives.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{Context, ParentElement as _, Styled as _, WeakEntity, Window, div};

use super::{OpStatus, RepoView};
use crate::appearance::{editor_font_family, ts};
use crate::custom_commands::{self, CommandTarget, CustomCommands, Vars};
use crate::theme::palette;

/// What a custom command runs on, besides the repository.
#[derive(Clone, Debug, Default)]
pub struct CustomOn {
    /// A commit's id.
    pub sha: Option<String>,
    /// A branch or tag.
    pub reference: Option<String>,
    /// A file, relative to the repository.
    pub file: Option<String>,
}

impl RepoView {
    fn custom_vars(&self, on: CustomOn) -> Vars {
        let branch = self.refs.head_branch.clone();
        let remote = branch.as_deref().and_then(|b| {
            let t = self
                .refs
                .branches
                .iter()
                .find(|x| x.name == b)?
                .tracking
                .as_ref()?;
            Some(t.upstream.split_once('/')?.0.to_owned())
        });
        Vars {
            repo: custom_commands::repo_var(self.repo.root()),
            branch,
            sha: on.sha.or_else(|| self.refs.head.map(|h| h.to_string())),
            reference: on.reference,
            file: on.file,
            remote,
        }
    }

    /// Run custom command `index` on `on`, after asking when the command says to.
    pub fn run_custom_command(
        &mut self,
        index: usize,
        on: CustomOn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy() {
            return;
        }
        let Some(cmd) = cx.global::<CustomCommands>().0.get(index).cloned() else {
            return;
        };
        let name = cmd.name.trim().to_owned();
        let script = match custom_commands::expand(&cmd.script, &self.custom_vars(on)) {
            Ok(s) => s,
            Err(e) => {
                self.op = Some(OpStatus::Failed(format!("{name}: {e}").into()));
                cx.notify();
                return;
            }
        };
        if !cmd.confirm {
            self.start_custom(name, script, cx);
            return;
        }
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let p = palette(cx);
            let (view, name, script) = (view.clone(), name.clone(), script.clone());
            alert
                .confirm()
                .title(format!("Run {name}?"))
                .description("Runs this in the repository's folder:")
                .child(
                    div()
                        .p(crate::appearance::rpx(8.))
                        .rounded(gpui_kit::px(6.))
                        .bg(p.bg)
                        .font_family(editor_font_family(cx))
                        .text_size(ts::BODY)
                        .text_color(p.fg2)
                        .child(script.clone()),
                )
                .ok_text("Run")
                .on_ok(move |_, _, cx| {
                    let (name, script) = (name.clone(), script.clone());
                    let _ = view.update(cx, |v, cx| v.start_custom(name, script, cx));
                    true
                })
        });
    }

    fn start_custom(&mut self, name: String, script: String, cx: &mut Context<Self>) {
        self.run_op(
            format!("Running {name}"),
            format!("{name} finished"),
            move |r, cancel, on_line| {
                switchhead_git::cli::run_shell(r.root(), &script, &[], cancel, |l| on_line(l))
                    .map(drop)
            },
            cx,
        );
    }
}

/// The toolbar's Commands menu: the repository's custom commands, Apply Patch and the
/// commands editor.
pub(crate) fn build_commands_menu(
    menu: PopupMenu,
    view: &WeakEntity<RepoView>,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let busy = view.upgrade().is_some_and(|v| v.read(cx).busy());
    let none = cx
        .try_global::<CustomCommands>()
        .is_none_or(|c| c.for_target(CommandTarget::Repository).is_empty());
    let view_r = view.clone();
    let view_p = view.clone();
    let menu = custom_commands::add_to_menu(
        menu,
        CommandTarget::Repository,
        busy,
        move |i, window, cx| {
            let _ = view_r.update(cx, |v, cx| {
                v.run_custom_command(i, CustomOn::default(), window, cx)
            });
        },
        cx,
    );
    let menu = if none {
        menu.item(PopupMenuItem::new("No repository commands yet").disabled(true))
    } else {
        menu
    };
    menu.separator()
        .item(
            PopupMenuItem::new("Apply Patch…")
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    let _ = view_p.update(cx, |v, cx| v.pick_patches(window, cx));
                }),
        )
        .item(
            PopupMenuItem::new("Edit Custom Commands…").on_click(|_, window, cx| {
                window.dispatch_action(Box::new(crate::actions::EditCustomCommands), cx)
            }),
        )
}
