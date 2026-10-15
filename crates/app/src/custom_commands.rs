//! Custom commands: shell commands the user defines once and runs from the app, on the
//! repository, a commit, a branch or tag, or a file. `${sha}`, `${ref}`, `${file}` and the
//! other variables are filled in from what the command runs on, quoted for the shell when
//! they need it, so a branch or file name cannot break out of the command.

use std::path::Path;
use std::rc::Rc;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, Window};
use serde::{Deserialize, Serialize};

/// What a command runs on, and so where it is offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandTarget {
    /// The repository (the toolbar's Commands menu).
    #[default]
    Repository,
    /// A commit (the graph's menu).
    Commit,
    /// A branch or tag (the sidebar's menu).
    Ref,
    /// A file (the file lists' menus).
    File,
}

impl CommandTarget {
    /// Every target, in the editor's order.
    pub const ALL: [CommandTarget; 4] = [
        CommandTarget::Repository,
        CommandTarget::Commit,
        CommandTarget::Ref,
        CommandTarget::File,
    ];

    /// "Repository", "Commit", "Branch or tag", "File".
    pub fn label(self) -> &'static str {
        match self {
            CommandTarget::Repository => "Repository",
            CommandTarget::Commit => "Commit",
            CommandTarget::Ref => "Branch or tag",
            CommandTarget::File => "File",
        }
    }
}

/// A user-defined command.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomCommand {
    /// Its name in menus.
    pub name: String,
    /// What it runs on.
    pub target: CommandTarget,
    /// The shell command, with `${...}` variables.
    pub script: String,
    /// Ask before running it.
    pub confirm: bool,
}

/// Every custom command, in the user's order (a GPUI global while the app runs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CustomCommands(pub Vec<CustomCommand>);

impl gpui_kit::Global for CustomCommands {}

impl CustomCommands {
    /// The commands for `target`, with their indices.
    pub fn for_target(&self, target: CommandTarget) -> Vec<(usize, CustomCommand)> {
        self.0
            .iter()
            .enumerate()
            .filter(|(_, c)| c.target == target && !c.name.trim().is_empty())
            .map(|(i, c)| (i, c.clone()))
            .collect()
    }
}

/// Add `target`'s commands to `menu` after a separator (nothing when there are none);
/// `run` gets the clicked command's index.
pub fn add_to_menu(
    mut menu: PopupMenu,
    target: CommandTarget,
    disabled: bool,
    run: impl Fn(usize, &mut Window, &mut App) + 'static,
    cx: &App,
) -> PopupMenu {
    let commands = cx
        .try_global::<CustomCommands>()
        .map(|c| c.for_target(target))
        .unwrap_or_default();
    if commands.is_empty() {
        return menu;
    }
    let run = Rc::new(run);
    menu = menu.separator();
    for (i, c) in commands {
        let run = run.clone();
        let label = if c.confirm {
            format!("{}…", c.name.trim())
        } else {
            c.name.trim().to_owned()
        };
        menu = menu.item(
            PopupMenuItem::new(label)
                .disabled(disabled)
                .on_click(move |_, window, cx| run(i, window, cx)),
        );
    }
    menu
}

/// The variables, with what they hold, for the editor's help.
pub const VARIABLES: [(&str, &str); 6] = [
    ("${repo}", "the repository's folder"),
    ("${branch}", "the current branch"),
    ("${sha}", "the commit's full id"),
    ("${ref}", "the branch or tag (a commit's id for a commit)"),
    ("${file}", "the file's path in the repository"),
    ("${remote}", "the current branch's remote"),
];

/// The values a command's variables take.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    /// The repository's folder.
    pub repo: String,
    /// The current branch.
    pub branch: Option<String>,
    /// The commit id.
    pub sha: Option<String>,
    /// The branch or tag.
    pub reference: Option<String>,
    /// The file, relative to the repository.
    pub file: Option<String>,
    /// The current branch's remote.
    pub remote: Option<String>,
}

impl Vars {
    fn get(&self, name: &str) -> Result<Option<&str>, String> {
        Ok(match name {
            "repo" => Some(self.repo.as_str()),
            "branch" => self.branch.as_deref(),
            "sha" => self.sha.as_deref(),
            "ref" => self.reference.as_deref(),
            "file" => self.file.as_deref(),
            "remote" => self.remote.as_deref(),
            _ => return Err(format!("Unknown variable ${{{name}}}")),
        })
    }
}

/// `script` with its variables replaced by their (quoted) values. Fails on an unknown
/// variable or one with no value here (`${file}` in a commit's command).
pub fn expand(script: &str, vars: &Vars) -> Result<String, String> {
    let mut out = String::with_capacity(script.len());
    let mut rest = script;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err("A ${ has no closing }".into());
        };
        let name = after[..end].trim();
        let value = vars
            .get(name)?
            .ok_or_else(|| format!("${{{name}}} has no value here"))?;
        out.push_str(&quote(value)?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Characters a value can hold and still go into a command as it is.
fn is_plain(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@+=,%".contains(c))
}

/// `value` as one shell word: as it is when plain, else in single quotes (`'` written as
/// `'\''`).
#[cfg(not(windows))]
fn quote(value: &str) -> Result<String, String> {
    if is_plain(value) {
        return Ok(value.to_owned());
    }
    Ok(format!("'{}'", value.replace('\'', r"'\''")))
}

/// `value` as one `cmd` word: as it is when plain, else in double quotes. cmd has no
/// escape for `"` and expands `%` inside quotes, so values with them are refused.
#[cfg(windows)]
fn quote(value: &str) -> Result<String, String> {
    if is_plain(value) && !value.contains('%') {
        return Ok(value.to_owned());
    }
    if value.contains(['"', '%', '\n', '\r']) {
        return Err(format!("{value} cannot be passed to cmd safely"));
    }
    Ok(format!("\"{value}\""))
}

/// The repository folder as the variable's text.
pub fn repo_var(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        Vars {
            repo: "/home/me/app".into(),
            branch: Some("feature/login".into()),
            sha: Some("0123abcd".into()),
            reference: Some("v1.0".into()),
            file: None,
            remote: Some("origin".into()),
        }
    }

    #[test]
    fn fills_in_variables() {
        assert_eq!(
            expand("git push ${remote} ${branch} && echo ${sha}", &vars()).unwrap(),
            "git push origin feature/login && echo 0123abcd"
        );
        assert_eq!(expand("cd ${ repo }", &vars()).unwrap(), "cd /home/me/app");
        assert_eq!(expand("no variables", &vars()).unwrap(), "no variables");
    }

    #[test]
    fn refuses_unknown_and_missing_variables() {
        assert_eq!(
            expand("cat ${file}", &vars()).unwrap_err(),
            "${file} has no value here"
        );
        assert_eq!(
            expand("echo ${nope}", &vars()).unwrap_err(),
            "Unknown variable ${nope}"
        );
        assert!(expand("echo ${sha", &vars()).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn quotes_values_that_need_it() {
        let mut v = vars();
        v.file = Some("docs/it's here; rm -rf x.txt".into());
        assert_eq!(
            expand("wc -l ${file}", &v).unwrap(),
            r"wc -l 'docs/it'\''s here; rm -rf x.txt'"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn quoted_values_reach_the_shell_intact() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = vars();
        v.file = Some("a b'c$(echo hi)".into());
        let script = expand("printf '%s' ${file}", &v).unwrap();
        let out = switchhead_git::cli::run_shell(
            dir.path(),
            &script,
            &[],
            &switchhead_git::cli::Cancel::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(out, "a b'c$(echo hi)");
    }

    #[test]
    fn lists_commands_by_target() {
        let c = CustomCommands(vec![
            CustomCommand {
                name: "Open".into(),
                target: CommandTarget::File,
                ..Default::default()
            },
            CustomCommand {
                name: "Deploy".into(),
                ..Default::default()
            },
            CustomCommand::default(),
        ]);
        let repo: Vec<usize> = c
            .for_target(CommandTarget::Repository)
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        assert_eq!(repo, [1]);
    }
}
