//! Actions and their default keybindings.

use gpui_kit::{Action, App, KeyBinding, actions};

actions!(
    switchhead,
    [
        OpenRepository,
        CloneRepository,
        ShowManager,
        CloseTab,
        NextTab,
        PreviousTab,
        Refresh,
        Fetch,
        Pull,
        Push,
        Stash,
        PopStash,
        ToggleSidebar,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        SelectUp,
        SelectDown,
        PageUp,
        PageDown,
        SelectFirst,
        SelectLast,
        JumpToHead,
        Dismiss,
        QuickLaunch,
        NewRepository,
        ShowSettings,
        ShowAssistantSettings,
        LfsFetch,
        LfsFetchAll,
        LfsPull,
        LfsPrune,
        LfsRefreshLocks,
        AddSubmodule,
        UpdateSubmodules,
        AddWorktree,
        PruneWorktrees,
        GitFlowSettings,
        StartFeature,
        StartRelease,
        StartHotfix,
        FinishFlowBranch,
        ShowAllBranches,
        ShowCurrentBranch,
        CopyCommitId,
        ToggleDiffLayout,
        ToggleFileTree,
        CycleWhitespace,
        MoreContext,
        LessContext,
        ShowLocalChanges,
        ShowCommits,
        StageAll,
        UnstageAll,
        ToggleStaged,
        SelectAllFiles,
        SwitchFileList,
        WriteCommitMessage,
        DiscardSelected,
        NewBranch,
        NewTag,
        PushAllTags,
        AddRemote,
        FindCommits,
        ShowReflog,
        BrowseFiles,
        Undo,
        ToggleActivity,
        ToggleAssistant,
        ExplainCommit,
        ReviewBranch,
    ]
);

/// A command the palette offers.
pub struct Command {
    /// What it says in the list.
    pub label: &'static str,
    /// Its group (shown on the right).
    pub group: &'static str,
    /// The action it dispatches.
    pub action: Box<dyn Action>,
    /// Only offered while a repository tab is active.
    pub needs_repo: bool,
}

/// Every command, in the palette's order.
pub fn commands() -> Vec<Command> {
    fn c(
        label: &'static str,
        group: &'static str,
        action: impl Action,
        needs_repo: bool,
    ) -> Command {
        Command {
            label,
            group,
            action: Box::new(action),
            needs_repo,
        }
    }
    vec![
        c("Fetch…", "Remote", Fetch, true),
        c("Pull…", "Remote", Pull, true),
        c("Push…", "Remote", Push, true),
        c("Add Remote…", "Remote", AddRemote, true),
        c("Stash Changes…", "Stash", Stash, true),
        c("Pop Stash", "Stash", PopStash, true),
        c("Refresh", "Repository", Refresh, true),
        c("Jump to HEAD", "History", JumpToHead, true),
        c("Find Commits…", "History", FindCommits, true),
        c("Show Reflog", "History", ShowReflog, true),
        c("Fetch LFS Objects", "LFS", LfsFetch, true),
        c("Fetch All LFS Objects", "LFS", LfsFetchAll, true),
        c("Pull LFS Objects", "LFS", LfsPull, true),
        c("Prune LFS Objects", "LFS", LfsPrune, true),
        c("Refresh LFS Locks", "LFS", LfsRefreshLocks, true),
        c("Add Submodule…", "Submodules", AddSubmodule, true),
        c(
            "Update Submodules (Recursive)",
            "Submodules",
            UpdateSubmodules,
            true,
        ),
        c("Add Worktree…", "Worktrees", AddWorktree, true),
        c("Prune Missing Worktrees", "Worktrees", PruneWorktrees, true),
        c("Start Feature…", "Git-flow", StartFeature, true),
        c("Start Release…", "Git-flow", StartRelease, true),
        c("Start Hotfix…", "Git-flow", StartHotfix, true),
        c(
            "Finish Current Git-flow Branch…",
            "Git-flow",
            FinishFlowBranch,
            true,
        ),
        c("Git-flow Settings…", "Git-flow", GitFlowSettings, true),
        c("Browse Files at Commit", "History", BrowseFiles, true),
        c("Undo Last Reset or Discard", "Repository", Undo, true),
        c("Show All Branches", "History", ShowAllBranches, true),
        c(
            "Show Current Branch Only",
            "History",
            ShowCurrentBranch,
            true,
        ),
        c("Copy Commit ID", "History", CopyCommitId, true),
        c("Show Local Changes", "Changes", ShowLocalChanges, true),
        c("Show All Commits", "History", ShowCommits, true),
        c("Stage All Changes", "Changes", StageAll, true),
        c("Unstage All Changes", "Changes", UnstageAll, true),
        c("Commit…", "Changes", WriteCommitMessage, true),
        c("New Branch…", "Branches", NewBranch, true),
        c("New Tag…", "Tags", NewTag, true),
        c("Push All Tags", "Tags", PushAllTags, true),
        c("Show / Hide Activity", "Window", ToggleActivity, true),
        c("Assistant…", "Assistant", ToggleAssistant, true),
        c("Explain Selected Commit", "Assistant", ExplainCommit, true),
        c("Review Current Branch", "Assistant", ReviewBranch, true),
        c(
            "Assistant Settings…",
            "Assistant",
            ShowAssistantSettings,
            false,
        ),
        c(
            "Toggle Inline / Side-by-Side Diff",
            "Diff",
            ToggleDiffLayout,
            false,
        ),
        c("Toggle File List / Tree", "Diff", ToggleFileTree, false),
        c("Change Whitespace Mode", "Diff", CycleWhitespace, false),
        c("More Context Lines", "Diff", MoreContext, false),
        c("Fewer Context Lines", "Diff", LessContext, false),
        c("Open Repository…", "Repositories", OpenRepository, false),
        c("Clone Repository…", "Repositories", CloneRepository, false),
        c("New Repository…", "Repositories", NewRepository, false),
        c("Repository Manager", "Repositories", ShowManager, false),
        c("Close Tab", "Window", CloseTab, true),
        c("Next Tab", "Window", NextTab, false),
        c("Previous Tab", "Window", PreviousTab, false),
        c("Toggle Sidebar", "Window", ToggleSidebar, true),
        c("Appearance…", "Window", ShowSettings, false),
        c("Zoom In", "Window", ZoomIn, false),
        c("Zoom Out", "Window", ZoomOut, false),
        c("Actual Size", "Window", ZoomReset, false),
    ]
}

/// Register the default keybindings (Fork's where it has one).
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-o", OpenRepository, None),
        KeyBinding::new("secondary-n", CloneRepository, None),
        KeyBinding::new("secondary-shift-m", ShowManager, None),
        KeyBinding::new("secondary-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new("secondary-r", Refresh, None),
        KeyBinding::new("secondary-shift-f", Fetch, None),
        KeyBinding::new("secondary-shift-l", Pull, None),
        KeyBinding::new("secondary-shift-p", Push, None),
        KeyBinding::new("secondary-shift-s", Stash, None),
        KeyBinding::new("secondary-b", ToggleSidebar, None),
        KeyBinding::new("secondary-=", ZoomIn, None),
        KeyBinding::new("secondary-+", ZoomIn, None),
        KeyBinding::new("secondary--", ZoomOut, None),
        KeyBinding::new("secondary-0", ZoomReset, None),
        KeyBinding::new("escape", Dismiss, None),
        KeyBinding::new("up", SelectUp, Some("Graph")),
        KeyBinding::new("down", SelectDown, Some("Graph")),
        KeyBinding::new("pageup", PageUp, Some("Graph")),
        KeyBinding::new("pagedown", PageDown, Some("Graph")),
        KeyBinding::new("home", SelectFirst, Some("Graph")),
        KeyBinding::new("end", SelectLast, Some("Graph")),
        KeyBinding::new("up", SelectUp, Some("Files")),
        KeyBinding::new("down", SelectDown, Some("Files")),
        KeyBinding::new("home", SelectFirst, Some("Files")),
        KeyBinding::new("end", SelectLast, Some("Files")),
        KeyBinding::new("secondary-shift-h", JumpToHead, None),
        KeyBinding::new("secondary-p", QuickLaunch, None),
        KeyBinding::new("secondary-,", ShowSettings, None),
        KeyBinding::new("secondary-shift-c", CopyCommitId, Some("Graph")),
        KeyBinding::new("secondary-f", FindCommits, Some("Graph")),
        KeyBinding::new("secondary-z", Undo, Some("Graph && !Input")),
        KeyBinding::new("secondary-z", Undo, Some("Changes && !Input")),
        KeyBinding::new("secondary-z", Undo, Some("Files && !Input")),
        KeyBinding::new("up", SelectUp, Some("Changes && !Input")),
        KeyBinding::new("down", SelectDown, Some("Changes && !Input")),
        KeyBinding::new("home", SelectFirst, Some("Changes && !Input")),
        KeyBinding::new("end", SelectLast, Some("Changes && !Input")),
        KeyBinding::new("space", ToggleStaged, Some("Changes && !Input")),
        KeyBinding::new("enter", ToggleStaged, Some("Changes && !Input")),
        KeyBinding::new("secondary-a", SelectAllFiles, Some("Changes && !Input")),
        KeyBinding::new("tab", SwitchFileList, Some("Changes && !Input")),
        KeyBinding::new("secondary-shift-enter", WriteCommitMessage, None),
        KeyBinding::new("secondary-shift-b", NewBranch, None),
        KeyBinding::new("secondary-shift-g", ToggleAssistant, None),
        KeyBinding::new(
            "secondary-backspace",
            DiscardSelected,
            Some("Changes && !Input"),
        ),
        KeyBinding::new("delete", DiscardSelected, Some("Changes && !Input")),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_listed_once() {
        let cmds = commands();
        for (i, a) in cmds.iter().enumerate() {
            assert!(!a.label.is_empty());
            for b in &cmds[i + 1..] {
                assert!(a.label != b.label, "{} is listed twice", a.label);
                assert!(
                    !a.action.partial_eq(&*b.action),
                    "{} and {} run the same action",
                    a.label,
                    b.label
                );
            }
        }
    }
}
