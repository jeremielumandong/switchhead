//! Actions and their default keybindings.

use gpui_kit::{App, KeyBinding, actions};

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
    ]
);

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
    ]);
}
