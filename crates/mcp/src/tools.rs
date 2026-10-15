//! The git tools. Each runs one read-only `git` command (through `switchhead_git`'s
//! [`Git`](switchhead_git::cli::Git), which never takes optional locks) in the one
//! repository the server was started for. Revisions and paths are checked so they cannot
//! be read as options or leave the work tree, and every output is capped.

use std::path::{Component, Path};

use serde_json::{Value, json};
use switchhead_git::{Repo, RepoState};

use crate::server::{ToolDef, ToolHost, ToolResult};

/// Longest tool output, in bytes; longer text is cut with a note.
pub const MAX_OUTPUT: usize = 60_000;
/// Default and largest `log` lengths.
const LOG_DEFAULT: u64 = 30;
const LOG_MAX: u64 = 500;

/// The tools for one repository.
pub struct GitTools {
    repo: Repo,
}

impl GitTools {
    /// Tools reading `repo`.
    pub fn new(repo: Repo) -> Self {
        Self { repo }
    }

    fn git(&self, args: &[&str]) -> Result<String, String> {
        let git = self.repo.git().map_err(|e| e.to_string())?;
        git.text(args).map_err(|e| e.to_string())
    }

    fn status(&self) -> Result<String, String> {
        let mut out = self.git(&["status", "--branch", "--short", "--untracked-files=all"])?;
        let state = self.repo.state();
        if state != RepoState::Clean {
            out = format!("{} in progress.\n{out}", state.label());
        }
        Ok(out)
    }

    fn log(&self, a: &Value) -> Result<String, String> {
        let max = a["max"].as_u64().unwrap_or(LOG_DEFAULT).clamp(1, LOG_MAX);
        let n = format!("-n{max}");
        let mut args = vec![
            "log",
            "--date=short",
            "--format=%h %ad %an%d%n    %s",
            n.as_str(),
        ];
        let rev = opt_rev(a, "rev")?;
        if a["all"].as_bool() == Some(true) {
            args.push("--all");
        } else if let Some(r) = rev {
            args.push(r);
        }
        let path = opt_path(a)?;
        args.push("--");
        args.extend(path);
        self.git(&args)
    }

    fn show(&self, a: &Value) -> Result<String, String> {
        let rev = req_rev(a, "rev")?;
        let mut args = vec!["show", "--format=fuller", "--stat", "--no-ext-diff"];
        if a["patch"].as_bool() == Some(false) {
            args.push("--no-patch");
        } else {
            args.push("--patch");
        }
        args.push(rev);
        args.push("--");
        args.extend(opt_path(a)?);
        self.git(&args)
    }

    fn diff(&self, a: &Value) -> Result<String, String> {
        let mut args = vec!["diff", "--no-ext-diff", "--find-renames"];
        if a["stat"].as_bool() == Some(true) {
            args.push("--stat");
        }
        let range;
        match a["mode"].as_str().unwrap_or("unstaged") {
            "unstaged" => {}
            "staged" => args.push("--cached"),
            "range" => {
                let from = req_rev(a, "from")?;
                let to = opt_rev(a, "to")?.unwrap_or("HEAD");
                if a["two_dot"].as_bool() == Some(true) {
                    args.extend([from, to]);
                } else {
                    // Three dots: what `to` changed since it left `from`, as a review sees it.
                    range = format!("{from}...{to}");
                    args.push(&range);
                }
            }
            m => return Err(format!("mode must be unstaged, staged or range, not {m}")),
        }
        args.push("--");
        args.extend(opt_path(a)?);
        self.git(&args)
    }

    fn branches(&self) -> Result<String, String> {
        self.git(&[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(HEAD) %(refname:short)  %(objectname:short)  %(committerdate:short)  \
             %(upstream:short) %(upstream:track)  %(subject)",
            "refs/heads",
            "refs/remotes",
        ])
    }

    fn blame(&self, a: &Value) -> Result<String, String> {
        let path = req_path(a)?;
        let mut args = vec!["blame", "--date=short"];
        let lines;
        if let Some(start) = a["start"].as_u64() {
            let end = a["end"].as_u64().unwrap_or(start + 99).max(start);
            lines = format!("-L{start},{end}");
            args.push(&lines);
        }
        if let Some(r) = opt_rev(a, "rev")? {
            args.push(r);
        }
        args.push("--");
        args.push(path);
        self.git(&args)
    }

    fn file_at(&self, a: &Value) -> Result<String, String> {
        let path = req_path(a)?;
        let rev = opt_rev(a, "rev")?.unwrap_or("HEAD");
        let spec = format!("{rev}:{path}");
        let git = self.repo.git().map_err(|e| e.to_string())?;
        let out = git
            .run(&["cat-file", "blob", spec.as_str()])
            .map_err(|e| e.to_string())?;
        text_or_binary(&out.stdout)
    }

    fn conflicts(&self, a: &Value) -> Result<String, String> {
        let files = self.git(&["diff", "--name-only", "--diff-filter=U"])?;
        let Some(path) = opt_path(a)? else {
            let state = self.repo.state();
            return Ok(if files.trim().is_empty() {
                "No conflicted files.".to_owned()
            } else {
                format!("{}; conflicted files:\n{files}", state.label())
            });
        };
        if !files.lines().any(|f| f == path) {
            return Err(format!("{path} is not conflicted"));
        }
        let work = std::fs::read(self.repo.root().join(path)).map_err(|e| e.to_string())?;
        let mut out = format!("=== {path} (work tree, with conflict markers)\n");
        out.push_str(&text_or_binary(&work)?);
        let git = self.repo.git().map_err(|e| e.to_string())?;
        for (stage, name) in [(1, "base"), (2, "ours"), (3, "theirs")] {
            let spec = format!(":{stage}:{path}");
            out.push_str(&format!("\n=== {name}\n"));
            match git.run(&["cat-file", "blob", spec.as_str()]) {
                Ok(o) => out.push_str(&text_or_binary(&o.stdout)?),
                Err(_) => out.push_str("(not present on this side)\n"),
            }
        }
        Ok(out)
    }
}

impl ToolHost for GitTools {
    fn tools(&self) -> Vec<ToolDef> {
        let path =
            json!({"type": "string", "description": "A path relative to the repository root"});
        let rev = |d: &str| json!({"type": "string", "description": d});
        vec![
            ToolDef {
                name: "status",
                description: "Current branch and upstream, staged, unstaged and untracked files, and any merge, rebase or cherry-pick in progress.",
                input_schema: json!({"type": "object", "properties": {}}),
            },
            ToolDef {
                name: "log",
                description: "Commits, newest first: short id, date, author, refs and subject.",
                input_schema: json!({"type": "object", "properties": {
                    "rev": rev("Revision or range to list (default HEAD), e.g. main, v1.2..HEAD"),
                    "all": {"type": "boolean", "description": "Every branch and tag instead of rev"},
                    "path": path,
                    "max": {"type": "integer", "description": "How many commits (default 30, at most 500)"}
                }}),
            },
            ToolDef {
                name: "show",
                description: "One commit: author, committer, full message, changed files and (unless patch is false) its patch.",
                input_schema: json!({"type": "object", "required": ["rev"], "properties": {
                    "rev": rev("The commit"),
                    "patch": {"type": "boolean", "description": "Include the patch (default true)"},
                    "path": path
                }}),
            },
            ToolDef {
                name: "diff",
                description: "A diff: unstaged (work tree vs index, the default), staged (index vs HEAD), or range (from...to: what `to` changed since it diverged from `from`; two_dot compares the two trees).",
                input_schema: json!({"type": "object", "properties": {
                    "mode": {"type": "string", "enum": ["unstaged", "staged", "range"]},
                    "from": rev("Range start (mode range)"),
                    "to": rev("Range end (default HEAD)"),
                    "two_dot": {"type": "boolean"},
                    "stat": {"type": "boolean", "description": "Only the changed files and line counts"},
                    "path": path
                }}),
            },
            ToolDef {
                name: "branches",
                description: "Local and remote branches, most recent first: current marker (*), name, commit, date, upstream with ahead/behind, subject.",
                input_schema: json!({"type": "object", "properties": {}}),
            },
            ToolDef {
                name: "blame",
                description: "Who last changed each line of a file, with commit and date.",
                input_schema: json!({"type": "object", "required": ["path"], "properties": {
                    "path": path,
                    "rev": rev("Blame as of this revision (default: the work tree)"),
                    "start": {"type": "integer", "description": "First line (1-based)"},
                    "end": {"type": "integer", "description": "Last line (default start + 99)"}
                }}),
            },
            ToolDef {
                name: "file_at",
                description: "A file's content at a revision.",
                input_schema: json!({"type": "object", "required": ["path"], "properties": {
                    "path": path,
                    "rev": rev("Revision (default HEAD)")
                }}),
            },
            ToolDef {
                name: "conflicts",
                description: "Without path: the conflicted files of a merge, rebase or cherry-pick. With path: that file in the work tree (with conflict markers) and its base, ours and theirs versions.",
                input_schema: json!({"type": "object", "properties": {"path": path}}),
            },
        ]
    }

    fn call(&mut self, name: &str, args: &Value) -> ToolResult {
        let res = match name {
            "status" => self.status(),
            "log" => self.log(args),
            "show" => self.show(args),
            "diff" => self.diff(args),
            "branches" => self.branches(),
            "blame" => self.blame(args),
            "file_at" => self.file_at(args),
            "conflicts" => self.conflicts(args),
            other => Err(format!("unknown tool: {other}")),
        };
        tracing::debug!(tool = name, ok = res.is_ok(), "mcp tool call");
        match res {
            Ok(t) if t.trim().is_empty() => ToolResult::text("(nothing)"),
            Ok(t) => ToolResult::text(cap(t)),
            Err(e) => ToolResult::error(e),
        }
    }
}

/// `text` cut to [`MAX_OUTPUT`] bytes (on a line break when one is near), with a note.
fn cap(text: String) -> String {
    if text.len() <= MAX_OUTPUT {
        return text;
    }
    let mut end = MAX_OUTPUT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind('\n').map_or(end, |i| i + 1);
    format!(
        "{}\n[cut: {} more bytes; narrow it with path, max or a line range]",
        &text[..end],
        text.len() - end
    )
}

fn text_or_binary(bytes: &[u8]) -> Result<String, String> {
    if switchhead_git::diff::is_binary(bytes) {
        return Ok(format!("(binary file, {} bytes)\n", bytes.len()));
    }
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// A revision argument: not an option, no spaces or control characters.
fn check_rev(r: &str) -> Result<&str, String> {
    let r = r.trim();
    if r.is_empty() || r.starts_with('-') || r.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!("not a revision: {r:?}"));
    }
    Ok(r)
}

fn opt_rev<'a>(a: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    a[key].as_str().map(check_rev).transpose()
}

fn req_rev<'a>(a: &'a Value, key: &str) -> Result<&'a str, String> {
    opt_rev(a, key)?.ok_or_else(|| format!("{key} is required"))
}

/// A path inside the repository: relative, without `..`.
fn check_path(p: &str) -> Result<&str, String> {
    let p = p.trim();
    let path = Path::new(p);
    let inside = !p.is_empty()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !inside {
        return Err(format!(
            "path must be relative to the repository root: {p:?}"
        ));
    }
    Ok(p)
}

fn opt_path(a: &Value) -> Result<Option<&str>, String> {
    a["path"].as_str().map(check_path).transpose()
}

fn req_path(a: &Value) -> Result<&str, String> {
    opt_path(a)?.ok_or_else(|| "path is required".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn repo() -> (tempfile::TempDir, GitTools) {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        git(d, &["init", "-q", "-b", "main"]);
        for (k, v) in [
            ("user.name", "Test"),
            ("user.email", "t@example.com"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            git(d, &["config", k, v]);
        }
        std::fs::write(d.join("a.txt"), "one\n").unwrap();
        git(d, &["add", "a.txt"]);
        git(d, &["commit", "-qm", "First"]);
        git(d, &["switch", "-qc", "side"]);
        std::fs::write(d.join("a.txt"), "side\n").unwrap();
        git(d, &["commit", "-qam", "Side change"]);
        git(d, &["switch", "-q", "main"]);
        std::fs::write(d.join("a.txt"), "main\n").unwrap();
        git(d, &["commit", "-qam", "Main change"]);
        let tools = GitTools::new(Repo::open(d).unwrap());
        (dir, tools)
    }

    fn call(t: &mut GitTools, name: &str, args: Value) -> (String, bool) {
        let r = t.call(name, &args);
        (
            r.content[0]["text"].as_str().unwrap().to_owned(),
            r.is_error,
        )
    }

    #[test]
    fn reads_history_and_files() {
        let (_d, mut t) = repo();
        let (log, err) = call(&mut t, "log", json!({"all": true}));
        assert!(!err, "{log}");
        assert!(log.contains("Main change") && log.contains("Side change"));
        let (show, _) = call(&mut t, "show", json!({"rev": "side"}));
        assert!(show.contains("+side"), "{show}");
        let (file, _) = call(&mut t, "file_at", json!({"path": "a.txt", "rev": "side"}));
        assert_eq!(file, "side\n");
        let (diff, _) = call(
            &mut t,
            "diff",
            json!({"mode": "range", "from": "main", "to": "side"}),
        );
        assert!(diff.contains("+side"), "{diff}");
        let (branches, _) = call(&mut t, "branches", json!({}));
        assert!(branches.contains("* main"), "{branches}");
        let (blame, _) = call(&mut t, "blame", json!({"path": "a.txt"}));
        assert!(blame.contains("main"), "{blame}");
    }

    #[test]
    fn shows_conflicts_with_all_sides() {
        let (d, mut t) = repo();
        let out = Command::new("git")
            .args(["merge", "-q", "side"])
            .current_dir(d.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(!out.status.success(), "the merge conflicts");
        let (status, _) = call(&mut t, "status", json!({}));
        assert!(status.starts_with("Merging"), "{status}");
        let (list, _) = call(&mut t, "conflicts", json!({}));
        assert!(list.contains("a.txt"), "{list}");
        let (file, err) = call(&mut t, "conflicts", json!({"path": "a.txt"}));
        assert!(!err, "{file}");
        assert!(file.contains("<<<<<<<") && file.contains("=== base\none\n"));
        assert!(file.contains("=== ours\nmain\n") && file.contains("=== theirs\nside\n"));
    }

    #[test]
    fn refuses_options_and_paths_outside() {
        let (_d, mut t) = repo();
        for (name, args) in [
            ("show", json!({"rev": "--output=/tmp/x"})),
            ("log", json!({"rev": "-p"})),
            ("file_at", json!({"path": "../secret"})),
            ("file_at", json!({"path": "/etc/passwd"})),
            ("blame", json!({"path": "a.txt", "rev": "HEAD --"})),
            ("diff", json!({"mode": "worktree"})),
        ] {
            let (msg, err) = call(&mut t, name, args);
            assert!(err, "{name}: {msg}");
        }
    }

    #[test]
    fn caps_long_output() {
        let long = "line\n".repeat(MAX_OUTPUT / 4);
        let out = cap(long);
        assert!(out.len() < MAX_OUTPUT + 100);
        assert!(out.ends_with("narrow it with path, max or a line range]"));
    }
}
