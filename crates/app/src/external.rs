//! Other applications: a terminal or editor opened on the repository or a file, and the
//! external diff and merge tools git runs. The user picks each in Settings → External
//! Tools; an empty setting means the first one found (terminal, editor) or git's own
//! `diff.tool` / `merge.tool`.
//!
//! A terminal or editor setting is a shell command line (`sh -c`, or `cmd /C` on
//! Windows). `{path}` in it stands for the folder or file, quoted; without it an
//! editor gets the path at the end, and a terminal starts in the folder.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// What to open a path with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalKind {
    /// A terminal in the folder (a file's folder).
    Terminal,
    /// The editor.
    Editor,
    /// The system file manager, showing the file (or the folder).
    FileManager,
    /// The external diff tool, on the file's changes.
    DiffTool,
}

/// The user's external tools (empty: automatic).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalTools {
    /// Command that opens a terminal.
    pub terminal: String,
    /// Command that opens a file or folder in an editor.
    pub editor: String,
    /// `git difftool --tool` name.
    pub diff_tool: String,
    /// `git mergetool --tool` name.
    pub merge_tool: String,
}

impl gpui_kit::Global for ExternalTools {}

impl ExternalTools {
    /// The terminal command to use, if any.
    pub fn terminal_command(&self) -> Option<String> {
        some(&self.terminal).or_else(default_terminal)
    }

    /// The editor command to use, if any.
    pub fn editor_command(&self) -> Option<String> {
        some(&self.editor).or_else(default_editor)
    }

    /// The diff tool name to pass to git (`None`: git's `diff.tool`).
    pub fn diff_tool(&self) -> Option<&str> {
        Some(self.diff_tool.trim()).filter(|t| !t.is_empty())
    }

    /// The merge tool name to pass to git (`None`: git's `merge.tool`).
    pub fn merge_tool(&self) -> Option<&str> {
        Some(self.merge_tool.trim()).filter(|t| !t.is_empty())
    }
}

fn some(s: &str) -> Option<String> {
    Some(s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Look `name` up on PATH (on Windows also as `.exe`, `.cmd` and `.bat`: a bare name
/// there can be an extensionless shell script next to the real one).
pub fn find_program(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        exts.iter()
            .map(|e| dir.join(format!("{name}{e}")))
            .find(|p| p.is_file())
    })
}

/// Editors with a command line launcher, in the order they are looked for.
const EDITORS: [&str; 9] = [
    "code", "cursor", "zed", "zeditor", "subl", "codium", "windsurf", "idea", "fleet",
];

/// Linux terminals that start in the current folder, in the order they are looked for.
const TERMINALS: [&str; 12] = [
    "x-terminal-emulator",
    "gnome-terminal",
    "ptyxis",
    "kgx",
    "konsole",
    "xfce4-terminal",
    "ghostty",
    "kitty",
    "alacritty",
    "wezterm",
    "foot",
    "xterm",
];

/// The first editor launcher on PATH.
pub fn default_editor() -> Option<String> {
    EDITORS
        .into_iter()
        .find(|e| find_program(e).is_some())
        .map(str::to_owned)
}

/// The system's terminal: Terminal on macOS, Windows Terminal (else PowerShell) on
/// Windows, `$TERMINAL` or the first one found elsewhere.
pub fn default_terminal() -> Option<String> {
    if cfg!(target_os = "macos") {
        return Some("open -a Terminal {path}".to_owned());
    }
    if cfg!(windows) {
        return Some(if find_program("wt").is_some() {
            "wt -d {path}".to_owned()
        } else {
            "start \"\" powershell".to_owned()
        });
    }
    std::env::var("TERMINAL")
        .ok()
        .filter(|t| !t.trim().is_empty() && find_program(t.trim()).is_some())
        .or_else(|| {
            TERMINALS
                .into_iter()
                .find(|t| find_program(t).is_some())
                .map(str::to_owned)
        })
}

/// The script that runs `command` on `path`: `{path}` becomes the path, quoted; without
/// it the path is appended when `append` is set.
fn script(command: &str, path: &Path, append: bool) -> String {
    let quoted = if cfg!(windows) {
        windows_quoted(&path.to_string_lossy())
    } else {
        // The path is the script's `$1`, so nothing in it is interpreted.
        "\"$1\"".to_owned()
    };
    let command = command.trim();
    if command.contains("{path}") {
        command.replace("{path}", &quoted)
    } else if append {
        format!("{command} {quoted}")
    } else {
        command.to_owned()
    }
}

/// `path` in double quotes for a Windows command line. Windows paths cannot contain `"`,
/// but a trailing `\` (a drive root, a folder) would escape the closing quote for
/// programs that split their command line the C runtime's way (`wt -d "C:\"` gets
/// `C:"`), so trailing backslashes are doubled.
fn windows_quoted(path: &str) -> String {
    let trailing = path.len() - path.trim_end_matches('\\').len();
    format!("\"{path}{}\"", "\\".repeat(trailing))
}

/// Start `command` on `path` in `cwd` and leave it running. Waits up to `wait` for it to
/// fail (a missing program, a bad option) and returns what it said; a command still
/// running after that, or one that exited cleanly, counts as started.
pub fn launch(
    command: &str,
    path: &Path,
    cwd: &Path,
    append: bool,
    wait: Duration,
) -> Result<(), String> {
    // Terminals and shells refuse a verbatim `\\?\` path as their folder.
    let path = &switchhead_git::paths::simplified(path);
    let cwd = &switchhead_git::paths::simplified(cwd);
    let script = script(command, path, append);
    #[cfg(unix)]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&script).arg("switchhead").arg(path);
        // Its own process group: a Ctrl+C meant for whoever started switchhead does not
        // close the user's terminal or editor.
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        cmd
    };
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new("cmd");
        cmd.raw_arg(format!("/S /C \"{script}\""))
            .creation_flags(CREATE_NO_WINDOW);
        cmd
    };
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    tracing::debug!(command = %script, "launch");
    let mut child = cmd.spawn().map_err(|e| format!("could not start: {e}"))?;
    let mut stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel();
    // The thread keeps waiting for a long-lived program, so it never becomes a zombie.
    std::thread::spawn(move || {
        let status = child.wait();
        let mut said = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.take(4096).read_to_string(&mut said);
        }
        let _ = tx.send((status, said));
    });
    match rx.recv_timeout(wait) {
        Ok((Ok(status), _)) if status.success() => Ok(()),
        Ok((Ok(status), said)) => Err(match said.trim() {
            "" => format!("{} ({status})", command.trim()),
            said => said.lines().last().unwrap_or(said).trim().to_owned(),
        }),
        Ok((Err(e), _)) => Err(e.to_string()),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn builds_scripts() {
        let p = Path::new("/a b/c");
        assert_eq!(script("code", p, true), "code \"$1\"");
        assert_eq!(
            script("kitty --directory {path}", p, false),
            "kitty --directory \"$1\""
        );
        assert_eq!(script("gnome-terminal", p, false), "gnome-terminal");
    }

    #[cfg(unix)]
    #[test]
    fn launches_with_the_path_as_one_argument() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a b$(echo x)'c.txt");
        std::fs::write(&file, "hi").unwrap();
        launch(
            "cp {path} copied.txt",
            &file,
            dir.path(),
            false,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("copied.txt")).unwrap(),
            "hi"
        );
        let err = launch(
            "echo 'no such editor' >&2; exit 3",
            &file,
            dir.path(),
            false,
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(err, "no such editor");
        // Still running when the wait ends: started.
        launch(
            "sleep 2",
            &file,
            dir.path(),
            false,
            Duration::from_millis(100),
        )
        .unwrap();
    }

    #[test]
    fn quotes_windows_paths() {
        assert_eq!(windows_quoted(r"C:\a b\c"), r#""C:\a b\c""#);
        assert_eq!(windows_quoted(r"C:\repo\"), r#""C:\repo\\""#);
        assert_eq!(windows_quoted(r"D:\"), r#""D:\\""#);
    }

    #[test]
    fn empty_settings_mean_automatic() {
        let t = ExternalTools {
            diff_tool: " meld ".into(),
            ..Default::default()
        };
        assert_eq!(t.diff_tool(), Some("meld"));
        assert_eq!(t.merge_tool(), None);
    }
}
