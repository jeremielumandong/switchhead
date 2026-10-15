//! What switchhead remembers between runs: recent repositories, open tabs, the theme and
//! the appearance settings. One JSON file in the data folder, read once at startup and
//! written whole (to a temporary file, then renamed) on a background thread when it changes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::appearance::AppearanceSettings;

/// Recent repositories kept in the list.
pub const MAX_RECENT: usize = 50;

/// Commit messages kept for the commit box's "Recent" menu.
pub const MAX_MESSAGES: usize = 20;

/// Where switchhead keeps its files.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The data folder (`SWITCHHEAD_DATA`, else the platform's per-user data folder).
    pub data: PathBuf,
}

impl Paths {
    /// Resolve the data folder. `SWITCHHEAD_DATA` (tests, screenshots) wins.
    pub fn resolve() -> Option<Self> {
        if let Some(d) = std::env::var_os("SWITCHHEAD_DATA").filter(|d| !d.is_empty()) {
            return Some(Self { data: d.into() });
        }
        let dirs = directories::ProjectDirs::from("dev", "switchhead", "switchhead")?;
        Some(Self {
            data: dirs.data_dir().to_path_buf(),
        })
    }

    /// Log files.
    pub fn logs_dir(&self) -> PathBuf {
        self.data.join("logs")
    }

    /// The state file.
    pub fn state_file(&self) -> PathBuf {
        self.data.join("state.json")
    }
}

/// A repository opened before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentRepo {
    /// Work tree root (or git dir of a bare repository).
    pub path: PathBuf,
    /// Display name (the folder name).
    pub name: String,
    /// Last opened, seconds since the epoch.
    pub opened: i64,
}

/// Everything saved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Most recently opened first.
    pub recent: Vec<RecentRepo>,
    /// Repositories open as tabs when the app last closed, in tab order.
    pub open_tabs: Vec<PathBuf>,
    /// Theme key (`crate::theme::ThemeId::key`).
    pub theme: Option<String>,
    /// Zoom and editor font.
    pub appearance: AppearanceSettings,
    /// Diff view settings.
    pub diff: DiffPrefs,
    /// Recently committed messages.
    pub messages: RecentMessages,
    /// Saved commit identities.
    pub identities: Identities,
    /// Which coding CLI the assistant uses.
    pub assistant: crate::assistant::AssistantSettings,
    /// The user's custom commands.
    pub custom_commands: crate::custom_commands::CustomCommands,
    /// Hosting accounts (their tokens are in the keychain, not here).
    pub accounts: crate::accounts::Accounts,
}

/// A saved commit identity: name, email and optional signing key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IdentityProfile {
    pub name: String,
    pub email: String,
    pub signing_key: Option<String>,
}

impl IdentityProfile {
    /// The git engine's form.
    pub fn to_git(&self) -> switchhead_git::identity::Identity {
        switchhead_git::identity::Identity {
            name: self.name.clone(),
            email: self.email.clone(),
            signing_key: self.signing_key.clone(),
        }
    }
}

/// Saved commit identities, in the order they were added (every repository; a GPUI
/// global while the app runs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Identities(pub Vec<IdentityProfile>);

impl gpui_kit::Global for Identities {}

impl Identities {
    /// Add `p`, replacing a saved one with the same email.
    pub fn save(&mut self, p: IdentityProfile) {
        match self.0.iter_mut().find(|q| q.email == p.email) {
            Some(q) => *q = p,
            None => self.0.push(p),
        }
    }
}

/// Commit messages used recently, newest first (every repository; a GPUI global while
/// the app runs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecentMessages(pub Vec<String>);

impl gpui_kit::Global for RecentMessages {}

impl RecentMessages {
    /// Remember `message` at the front (once), keeping [`MAX_MESSAGES`].
    pub fn push(&mut self, message: &str) {
        let message = message.trim();
        if message.is_empty() {
            return;
        }
        self.0.retain(|m| m != message);
        self.0.insert(0, message.to_owned());
        self.0.truncate(MAX_MESSAGES);
    }
}

/// How file diffs are shown (shared by every tab; a GPUI global while the app runs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffPrefs {
    /// Old and new side by side instead of one inline column.
    pub side_by_side: bool,
    /// Whitespace handling.
    pub whitespace: WhitespacePref,
    /// Unchanged lines around each change.
    pub context: u32,
    /// Changed files as a folder tree instead of a flat list.
    pub tree: bool,
}

impl Default for DiffPrefs {
    fn default() -> Self {
        Self {
            side_by_side: false,
            whitespace: WhitespacePref::Show,
            context: 3,
            tree: false,
        }
    }
}

impl gpui_kit::Global for DiffPrefs {}

impl DiffPrefs {
    /// Most context lines offered.
    pub const MAX_CONTEXT: u32 = 25;

    /// The git engine's options for these settings.
    pub fn options(&self) -> switchhead_git::diff::DiffOptions {
        use switchhead_git::diff::Whitespace;
        switchhead_git::diff::DiffOptions {
            context: self.context.min(Self::MAX_CONTEXT),
            whitespace: match self.whitespace {
                WhitespacePref::Show => Whitespace::Exact,
                WhitespacePref::IgnoreChanges => Whitespace::IgnoreChanges,
                WhitespacePref::IgnoreAll => Whitespace::IgnoreAll,
            },
            words: true,
        }
    }
}

impl WhitespacePref {
    /// The next mode (show → ignore amount → ignore all → show).
    pub fn next(self) -> Self {
        match self {
            WhitespacePref::Show => WhitespacePref::IgnoreChanges,
            WhitespacePref::IgnoreChanges => WhitespacePref::IgnoreAll,
            WhitespacePref::IgnoreAll => WhitespacePref::Show,
        }
    }
}

/// Whitespace handling in diffs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhitespacePref {
    /// Show every change.
    #[default]
    Show,
    /// Ignore changes in the amount of whitespace (`-b`).
    IgnoreChanges,
    /// Ignore all whitespace (`-w`).
    IgnoreAll,
}

impl State {
    /// Read the state file. A missing or unreadable file gives the defaults (logged).
    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, path = %path.display(), "state file unreadable; starting fresh");
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "could not read the state file");
                Self::default()
            }
        }
    }

    /// Write the state file atomically.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// Record that `path` was opened now: it moves to the front of the recent list.
    pub fn touch(&mut self, path: &Path, name: &str, now: i64) {
        self.recent.retain(|r| r.path != path);
        self.recent.insert(
            0,
            RecentRepo {
                path: path.to_path_buf(),
                name: name.to_owned(),
                opened: now,
            },
        );
        self.recent.truncate(MAX_RECENT);
    }

    /// Forget a recent repository.
    pub fn forget(&mut self, path: &Path) {
        self.recent.retain(|r| r.path != path);
    }
}

/// Seconds since the epoch, now.
pub fn now() -> i64 {
    chrono::Local::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_replace_by_email() {
        let mut ids = Identities::default();
        let p = |name: &str, email: &str| IdentityProfile {
            name: name.into(),
            email: email.into(),
            signing_key: None,
        };
        ids.save(p("Jane", "jane@work"));
        ids.save(p("Jane", "jane@home"));
        ids.save(p("Jane D.", "jane@work"));
        assert_eq!(ids.0, [p("Jane D.", "jane@work"), p("Jane", "jane@home")]);
    }

    #[test]
    fn recent_messages_move_to_front() {
        let mut m = RecentMessages::default();
        m.push("one");
        m.push("  ");
        m.push("two\n");
        m.push("one");
        assert_eq!(m.0, ["one", "two"]);
        for i in 0..30 {
            m.push(&i.to_string());
        }
        assert_eq!(m.0.len(), MAX_MESSAGES);
        assert_eq!(m.0[0], "29");
    }

    #[test]
    fn round_trips_and_tolerates_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub/state.json");
        assert_eq!(State::load(&file), State::default());
        let mut s = State::default();
        s.touch(Path::new("/a"), "a", 1);
        s.theme = Some("nord".into());
        s.open_tabs = vec!["/a".into()];
        s.diff.side_by_side = true;
        s.diff.whitespace = WhitespacePref::IgnoreAll;
        s.save(&file).unwrap();
        assert_eq!(State::load(&file), s);
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(State::load(&file), State::default());
        // Fields added later default when an older file lacks them.
        std::fs::write(&file, r#"{"theme":"light"}"#).unwrap();
        assert_eq!(State::load(&file).theme.as_deref(), Some("light"));
    }

    #[test]
    fn recent_moves_to_front_dedups_and_caps() {
        let mut s = State::default();
        for i in 0..60 {
            s.touch(Path::new(&format!("/r{i}")), "r", i);
        }
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0].path, Path::new("/r59"));
        s.touch(Path::new("/r30"), "r", 100);
        assert_eq!(s.recent[0].path, Path::new("/r30"));
        assert_eq!(
            s.recent
                .iter()
                .filter(|r| r.path == Path::new("/r30"))
                .count(),
            1
        );
        s.forget(Path::new("/r30"));
        assert!(s.recent.iter().all(|r| r.path != Path::new("/r30")));
    }
}
