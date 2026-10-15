//! Open the repository or one of its files in another application: a terminal, the
//! editor, the file manager, or the external diff tool (see [`crate::external`]).

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::{App, Context};
use switchhead_git::ops::DiffSides;

use super::{OpStatus, RepoView};
use crate::external::{ExternalKind, ExternalTools};

/// How long a terminal or editor gets to fail before it counts as started.
const LAUNCH_WAIT: Duration = Duration::from_millis(1500);

fn tools(cx: &App) -> ExternalTools {
    cx.try_global::<ExternalTools>()
        .cloned()
        .unwrap_or_default()
}

/// What the menu item said, for messages.
fn verb(kind: ExternalKind) -> &'static str {
    match kind {
        ExternalKind::Terminal => "Open in Terminal",
        ExternalKind::Editor => "Open in Editor",
        ExternalKind::FileManager => "Show in File Manager",
        ExternalKind::DiffTool => "Open in External Diff Tool",
    }
}

/// What the file manager does with a path.
enum Show {
    /// Open the folder.
    Folder(PathBuf),
    /// Open its folder with it selected.
    Select(PathBuf),
}

impl RepoView {
    /// Open `path` (relative to the work tree; empty: the repository) with `kind`.
    /// `sides`: what the diff tool compares.
    pub fn open_external(
        &mut self,
        kind: ExternalKind,
        path: PathBuf,
        sides: Option<DiffSides>,
        cx: &mut Context<Self>,
    ) {
        let repo = self.repo.clone();
        let root = repo.root().to_path_buf();
        // `join("")` would add a trailing separator.
        let full = if path.as_os_str().is_empty() {
            root.clone()
        } else {
            root.join(&path)
        };
        let tools = tools(cx);
        let failed = move |v: &mut RepoView, e: String, cx: &mut Context<RepoView>| {
            v.op = Some(OpStatus::Failed(format!("{}: {e}", verb(kind)).into()));
            cx.notify();
        };
        match kind {
            ExternalKind::FileManager => {
                let task = cx.background_executor().spawn(async move {
                    if full.is_dir() {
                        Some(Show::Folder(full))
                    } else if full.exists() {
                        Some(Show::Select(full))
                    } else {
                        // A deleted file: its folder, or the nearest one left.
                        full.ancestors()
                            .find(|p| p.is_dir())
                            .map(|p| Show::Folder(p.to_path_buf()))
                    }
                });
                cx.spawn(async move |this, cx| {
                    let shown = task.await;
                    let _ = this.update(cx, |_, cx| match shown {
                        Some(Show::Folder(p)) => cx.open_with_system(&p),
                        Some(Show::Select(p)) => cx.reveal_path(&p),
                        None => {}
                    });
                })
                .detach();
            }
            ExternalKind::Terminal | ExternalKind::Editor => {
                let terminal = kind == ExternalKind::Terminal;
                let task = cx.background_executor().spawn(async move {
                    let command = if terminal {
                        tools
                            .terminal_command()
                            .ok_or("no terminal found: set one in Settings → External Tools")?
                    } else {
                        tools
                            .editor_command()
                            .ok_or("no editor found: set one in Settings → External Tools")?
                    };
                    let target = if terminal {
                        // A file's folder (the nearest one left for a deleted file).
                        full.ancestors()
                            .find(|p| p.is_dir())
                            .map(|p| p.to_path_buf())
                            .unwrap_or(root.clone())
                    } else if full.exists() {
                        full
                    } else {
                        return Err("the file is not in the work tree".to_owned());
                    };
                    let cwd = if target.is_dir() { &target } else { &root };
                    crate::external::launch(&command, &target, cwd, !terminal, LAUNCH_WAIT)
                });
                cx.spawn(async move |this, cx| {
                    if let Err(e) = task.await {
                        let _ = this.update(cx, |v, cx| failed(v, e, cx));
                    }
                })
                .detach();
            }
            ExternalKind::DiffTool => {
                let sides = sides.unwrap_or(DiffSides::WorkTree);
                let tool = tools.diff_tool().map(str::to_owned);
                // Waits until the tool closes.
                let task = cx
                    .background_executor()
                    .spawn(async move { repo.run_diff_tool(tool.as_deref(), &sides, &path) });
                cx.spawn(async move |this, cx| {
                    if let Err(e) = task.await {
                        let message = match e {
                            switchhead_git::Error::Command { message, .. } => message,
                            e => e.to_string(),
                        };
                        let _ = this.update(cx, |v, cx| failed(v, message, cx));
                    }
                })
                .detach();
            }
        }
    }
}
