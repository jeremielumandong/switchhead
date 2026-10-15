//! Running a coding CLI for the app's assistant features: which CLI and how (settings),
//! and one run with switchhead's read-only git MCP server for the repository
//! (`switchhead mcp --repo <path>`). The CLI runs in `switchhead-agents`' private temp
//! directory, never in the repository.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use switchhead_agents::{
    AgentAdapter, AgentError, AgentEvent, AgentKind, AgentRun, CancelHandle, ClaudeCode, CustomCli,
    McpServer, RunRequest,
};

/// One built-in CLI's settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CliSettings {
    /// Executable; empty looks the CLI up on PATH.
    pub path: String,
    /// Model; empty keeps the CLI's default.
    pub model: String,
    /// Extra arguments.
    pub extra_args: Vec<String>,
}

/// Which coding CLI the assistant uses and how (a GPUI global while the app runs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AssistantSettings {
    /// The CLI to use; `None` takes the first one installed.
    pub agent: Option<AgentKind>,
    pub claude_code: CliSettings,
    pub codex: CliSettings,
    pub gemini: CliSettings,
    pub custom_settings: CliSettings,
    /// The user-defined CLI.
    pub custom: CustomCli,
    /// Generated commit messages follow Conventional Commits (`feat: …`).
    pub conventional_commits: bool,
}

impl gpui_kit::Global for AssistantSettings {}

impl AssistantSettings {
    /// One CLI's settings.
    pub fn cli(&self, kind: AgentKind) -> &CliSettings {
        match kind {
            AgentKind::ClaudeCode => &self.claude_code,
            AgentKind::Codex => &self.codex,
            AgentKind::Gemini => &self.gemini,
            AgentKind::Custom => &self.custom_settings,
        }
    }

    /// The CLI runs use: the chosen one, else the first built-in one found.
    pub fn agent(&self) -> Option<AgentKind> {
        self.agent.or_else(|| {
            [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Gemini]
                .into_iter()
                .find(|k| self.program(*k).is_some())
        })
    }

    /// Where `kind`'s executable is, if it is installed.
    pub fn program(&self, kind: AgentKind) -> Option<PathBuf> {
        let explicit = self.cli(kind).path.trim();
        let explicit = (!explicit.is_empty()).then(|| PathBuf::from(explicit));
        let name = match kind {
            AgentKind::Custom => self.custom.program.trim().to_owned(),
            k => adapter_for(k, None)?.program().to_owned(),
        };
        if name.is_empty() && explicit.is_none() {
            return None;
        }
        switchhead_agents::find_program(&name, explicit.as_deref())
    }
}

fn adapter_for(kind: AgentKind, custom: Option<&CustomCli>) -> Option<Arc<dyn AgentAdapter>> {
    match kind {
        AgentKind::ClaudeCode => Some(Arc::new(ClaudeCode)),
        AgentKind::Codex => Some(Arc::new(switchhead_agents::Codex)),
        AgentKind::Gemini => Some(Arc::new(switchhead_agents::Gemini)),
        AgentKind::Custom => custom
            .filter(|c| !c.program.trim().is_empty())
            .map(|c| Arc::new(switchhead_agents::Custom(c.clone())) as Arc<dyn AgentAdapter>),
    }
}

/// Start `prompt` on the configured CLI, with the git tools for the repository at `repo`.
pub fn start(
    settings: &AssistantSettings,
    repo: &Path,
    prompt: String,
) -> Result<AgentRun, String> {
    let kind = settings.agent().ok_or_else(|| {
        "No coding CLI found. Install Claude Code, Codex CLI or Gemini CLI, or set one up in \
         Settings → Assistant."
            .to_owned()
    })?;
    let adapter = adapter_for(kind, Some(&settings.custom))
        .ok_or_else(|| format!("{} is not set up", kind.display_name()))?;
    let exe = std::env::current_exe().map_err(|e| format!("could not find switchhead: {e}"))?;
    let cli = settings.cli(kind);
    let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_owned());
    let req = RunRequest {
        program: settings.program(kind),
        prompt,
        resume: None,
        model: some(&cli.model),
        mcp: McpServer {
            command: exe,
            args: vec![
                "mcp".into(),
                "--repo".into(),
                repo.to_string_lossy().into_owned(),
            ],
            env: Vec::new(),
        },
        temp_root: None,
        extra_args: cli.extra_args.clone(),
        extra_env: Vec::new(),
        guards: Vec::new(),
    };
    switchhead_agents::runner::start(adapter, req).map_err(|e| match e {
        AgentError::NotFound(n) => format!("{n} is not installed or not on PATH"),
        e => e.to_string(),
    })
}

/// Run to the end on this (non-async) thread and return the answer text.
pub fn answer(mut run: AgentRun) -> Result<String, String> {
    let mut text = String::new();
    let mut error = None;
    while let Some(ev) = run.blocking_next() {
        match ev {
            AgentEvent::Text(t) => text.push_str(&t),
            AgentEvent::Done(s) if !s.text.trim().is_empty() => text = s.text,
            AgentEvent::ToolCall { name, .. } => {
                tracing::debug!(tool = %name, "assistant tool call")
            }
            AgentEvent::Error(e) => error = Some(e),
            AgentEvent::Exited(_) => break,
            _ => {}
        }
    }
    match error {
        Some(e) => Err(e),
        None if text.trim().is_empty() => Err("The assistant gave no answer.".into()),
        None => Ok(text),
    }
}

/// A run started on its own thread; the answer arrives on the returned receiver.
pub fn spawn(
    settings: &AssistantSettings,
    repo: &Path,
    prompt: String,
) -> Result<
    (
        CancelHandle,
        tokio::sync::oneshot::Receiver<Result<String, String>>,
    ),
    String,
> {
    let run = start(settings, repo, prompt)?;
    let cancel = run.cancel_handle();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("assistant-run".into())
        .spawn(move || {
            let _ = tx.send(answer(run));
        })
        .map_err(|e| e.to_string())?;
    Ok((cancel, rx))
}

/// The answer without a wrapping code fence or surrounding quotes, as a commit message.
pub fn clean_message(answer: &str) -> String {
    let t = answer.trim();
    let t = match t.strip_prefix("```") {
        Some(rest) => {
            let rest = rest.split_once('\n').map_or("", |(_, r)| r);
            rest.trim_end().strip_suffix("```").unwrap_or(rest)
        }
        None => t,
    };
    t.trim().to_owned()
}

/// The prompt asking for a commit message for the staged changes.
pub fn commit_message_prompt(conventional: bool, amend: bool) -> String {
    let what = if amend {
        "the last commit as it will be amended: the changes of `show HEAD` plus the staged \
         changes"
    } else {
        "the staged changes"
    };
    let style = if conventional {
        "Use Conventional Commits: `type(scope): summary` with a type such as feat, fix, docs, \
         refactor, test or chore."
    } else {
        "Follow the style of the repository's recent commit subjects (capitalization, \
         prefixes, tense)."
    };
    format!(
        "Write a git commit message for {what} in this repository.\n\n\
         First read the staged diff with the `diff` tool (mode staged), and the last 15 commits \
         with `log` (max 15) to see how this repository writes messages. {style}\n\n\
         The subject says what the change does in about 50 characters (72 at most), with no \
         period at the end. If the reason or the effect is not obvious from the subject, add a \
         blank line and a short body saying why, wrapped at 72 columns.\n\n\
         Reply with the commit message only: no code fence, no quotes, no commentary, and no \
         trailers (Co-Authored-By, Signed-off-by, session links): the user adds those."
    )
}

/// The prompt asking for the resolved content of the conflicted file `path`.
pub fn conflict_prompt(path: &str) -> String {
    format!(
        "Resolve the merge conflicts in `{path}` in this repository.\n\n\
         Read the file with the `conflicts` tool (path `{path}`): it gives the work-tree file \
         with conflict markers and the base, ours and theirs versions. Use `status`, `log` and \
         `show` to learn what each side meant to change. Keep both sides' intent where they \
         are compatible; where they are not, prefer the change that keeps the code working, \
         and say so.\n\n\
         Reply with two or three sentences on how you resolved each conflict, then the whole \
         resolved file in one fenced code block, with no conflict markers left. If the file \
         itself contains ``` fences, fence it with more backticks. Nothing after the block."
    )
}

/// The prompt asking how to do `task` with git in this repository.
pub fn command_prompt(task: &str) -> String {
    format!(
        "The user of a git client asks how to do this in this repository:\n\n{task}\n\n\
         Look at the repository first (`status`, `branches`, `log`, and `show` or `diff` as \
         needed) so the commands use its real branch, remote and file names. Then answer \
         in a few short sentences: what the commands do, and anything that can lose work \
         or rewrite published history, with a safer alternative when there is one.\n\n\
         Put the commands in one ```bash block, one plain `git` command per line, in the \
         order to run them, with `#` comments where a step needs explaining. No pipes, \
         `&&`, variables, globs or placeholders like `<branch>`: the app runs each line by \
         itself without a shell and without a terminal, so avoid commands that open an \
         editor or prompt (use `-m`, `--no-edit`). If the request is not about git or \
         cannot be done with git, say so and give no block. You cannot run anything: \
         the user reviews each command and runs it from the app."
    )
}

/// Split an answer into the text before its last fenced code block and the block's
/// content (with a final newline). `None` when it has no closed block.
pub fn fenced_block(answer: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = answer.lines().collect();
    let ticks = |l: &str| l.trim_start().chars().take_while(|c| *c == '`').count();
    // The outermost block: the first opening fence and the last line closing it.
    let open = lines.iter().position(|l| ticks(l) >= 3)?;
    let fence = ticks(lines[open]);
    let close = (open + 1..lines.len())
        .rev()
        .find(|&j| ticks(lines[j]) >= fence && lines[j].trim().chars().all(|c| c == '`'))?;
    let note = lines[..open].join("\n").trim().to_owned();
    let mut body = lines[open + 1..close].join("\n");
    body.push('\n');
    Some((note, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_fenced_and_plain_answers() {
        assert_eq!(
            clean_message("```\nFix it\n\nWhy.\n```\n"),
            "Fix it\n\nWhy."
        );
        assert_eq!(clean_message("```text\nAdd x\n```"), "Add x");
        assert_eq!(clean_message("  Add x \n"), "Add x");
    }

    #[test]
    fn finds_the_resolved_file() {
        let a = "Kept both.\n\n```rust\nfn a() {}\n```\n";
        assert_eq!(
            fenced_block(a),
            Some(("Kept both.".into(), "fn a() {}\n".into()))
        );
        // A Markdown file with its own fences inside a longer one.
        let md = "Merged.\n````markdown\n# T\n```sh\nls\n```\n````";
        assert_eq!(
            fenced_block(md).map(|(_, b)| b),
            Some("# T\n```sh\nls\n```\n".into())
        );
        assert_eq!(fenced_block("no block"), None);
        assert_eq!(fenced_block("```\nunclosed"), None);
    }

    #[test]
    fn prompts_name_the_style() {
        assert!(commit_message_prompt(true, false).contains("Conventional Commits"));
        assert!(commit_message_prompt(false, false).contains("recent commit subjects"));
        assert!(commit_message_prompt(false, true).contains("amended"));
    }

    #[test]
    fn settings_round_trip() {
        let s = AssistantSettings {
            agent: Some(AgentKind::Codex),
            conventional_commits: true,
            ..Default::default()
        };
        let back: AssistantSettings =
            serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.agent(), Some(AgentKind::Codex));
    }
}
