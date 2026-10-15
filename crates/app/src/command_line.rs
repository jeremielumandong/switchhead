//! The commands in an assistant's answer: the answer split into prose and code blocks,
//! and each line of a shell block split into words the way a shell would, so a plain
//! `git ...` line can run without a shell. Lines with shell syntax (pipes, redirects,
//! variables, placeholders like `<branch>`) are shown but not runnable.

/// A piece of an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Prose, as written.
    Text(String),
    /// A code block in another language, shown as is.
    Code(String),
    /// The commands of a shell block.
    Commands(Vec<CommandLine>),
}

/// One line of a shell block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandLine {
    /// The line as shown (a leading `$ ` prompt removed).
    pub line: String,
    /// The arguments after `git`, when the line can run as it is.
    pub args: Option<Vec<String>>,
    /// Why it cannot run here, when it cannot.
    pub why_not: Option<&'static str>,
}

impl CommandLine {
    /// Parse one shell line.
    pub fn parse(line: &str) -> Self {
        let line = line.trim();
        let line = line.strip_prefix("$ ").unwrap_or(line).trim().to_owned();
        let (args, why_not) = match split(&line) {
            Err(why) => (None, Some(why)),
            Ok(words) => match words.split_first() {
                Some((git, rest)) if git == "git" && !rest.is_empty() => {
                    (Some(rest.to_vec()), None)
                }
                _ => (None, Some("Not a git command")),
            },
        };
        Self {
            line,
            args,
            why_not,
        }
    }

    /// Whether running it can lose work: asks for a confirmation first.
    pub fn is_destructive(&self) -> bool {
        let Some(args) = &self.args else {
            return false;
        };
        let has = |a: &str| args.iter().any(|x| x == a);
        let short = |c: char| {
            args.iter()
                .any(|x| x.starts_with('-') && !x.starts_with("--") && x.contains(c))
        };
        match args.first().map(String::as_str) {
            Some("reset") => has("--hard") || has("--merge") || has("--keep"),
            Some("clean") => short('f') || has("--force"),
            Some("push") => {
                short('f')
                    || has("--delete")
                    || short('d')
                    || args.iter().any(|a| {
                        a.starts_with("--force") || a.starts_with('+') || a.starts_with(':')
                    })
            }
            Some("branch") => has("-D") || (short('d') && short('f')) || has("--force"),
            Some("checkout") | Some("switch") => {
                has("-f") || has("--force") || has("--discard-changes") || has("--")
            }
            Some("restore") => !has("--staged") || has("--worktree") || short('W'),
            Some("stash") => matches!(args.get(1).map(String::as_str), Some("drop" | "clear")),
            Some("tag") => short('d') || short('f'),
            Some("rebase") | Some("filter-branch") | Some("filter-repo") | Some("update-ref") => {
                true
            }
            Some("reflog") => has("expire") || has("delete"),
            Some("gc") => args.iter().any(|a| a.starts_with("--prune")),
            Some("worktree") => has("remove") || has("prune"),
            Some("remote") => has("remove") || has("rm") || has("prune"),
            _ => false,
        }
    }
}

/// Split an answer into prose, code and command blocks.
pub fn parts(answer: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut lines = answer.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let fence = trimmed.chars().take_while(|c| *c == '`').count();
        if fence < 3 {
            text.push_str(line);
            text.push('\n');
            continue;
        }
        let lang = trimmed[fence..].trim().to_ascii_lowercase();
        let mut body = Vec::new();
        for l in lines.by_ref() {
            let t = l.trim();
            if t.len() >= fence && t.chars().all(|c| c == '`') {
                break;
            }
            body.push(l);
        }
        let prose = text.trim();
        if !prose.is_empty() {
            parts.push(Part::Text(prose.to_owned()));
        }
        text.clear();
        if matches!(
            lang.as_str(),
            "" | "bash" | "sh" | "shell" | "console" | "zsh"
        ) {
            let cmds: Vec<CommandLine> = body
                .iter()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(CommandLine::parse)
                .collect();
            if !cmds.is_empty() {
                parts.push(Part::Commands(cmds));
            }
        } else {
            parts.push(Part::Code(body.join("\n")));
        }
    }
    let prose = text.trim();
    if !prose.is_empty() {
        parts.push(Part::Text(prose.to_owned()));
    }
    parts
}

/// Prose as display blocks: paragraphs with their lines joined, and each list item on
/// its own, without Markdown's bold markers.
pub fn prose_blocks(text: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut open = false;
    for line in text.lines() {
        let line = line.trim().replace("**", "");
        // A Markdown heading reads as its own line.
        let heading = line.starts_with('#');
        let line = line.trim_start_matches('#').trim_start().to_owned();
        let item = heading
            || line.starts_with("- ")
            || line.starts_with("* ")
            || line.split_once(". ").is_some_and(|(n, _)| {
                !n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit())
            });
        match blocks.last_mut() {
            _ if line.is_empty() => open = false,
            Some(last) if open && !item => {
                last.push(' ');
                last.push_str(&line);
            }
            _ => {
                let line = match line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                    Some(rest) => format!("• {rest}"),
                    None => line,
                };
                blocks.push(line);
                open = !heading;
            }
        }
    }
    blocks
}

/// Split a line into words like a POSIX shell: quotes, backslashes and a trailing
/// `# comment`. Anything that needs a real shell is an error.
pub fn split(line: &str) -> Result<Vec<String>, &'static str> {
    const SHELL: &str = "Uses shell syntax; run it in a terminal";
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '#' if !in_word => break,
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("Unclosed quote"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("Unclosed quote"),
                        },
                        Some('$' | '`') => return Err(SHELL),
                        Some(c) => word.push(c),
                        None => return Err("Unclosed quote"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => word.push(c),
                    None => return Err(SHELL),
                }
            }
            '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' => return Err(SHELL),
            '*' | '?' | '[' if !in_word || !word.contains('=') => return Err(SHELL),
            '~' if !in_word => return Err(SHELL),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Option<Vec<String>> {
        CommandLine::parse(line).args
    }

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(
            split(r#"git commit -m "fix: it's \"done\"" # why"#).unwrap(),
            ["git", "commit", "-m", r#"fix: it's "done""#]
        );
        assert_eq!(
            split("git log --format='%h %s' a\\ b").unwrap(),
            ["git", "log", "--format=%h %s", "a b"]
        );
        assert_eq!(split("git show HEAD~2").unwrap(), ["git", "show", "HEAD~2"]);
        assert_eq!(
            split("git log --grep=fix*").unwrap(),
            ["git", "log", "--grep=fix*"]
        );
        for bad in [
            "git log | head",
            "git add . && git commit",
            "git checkout <branch>",
            "git branch -D $NAME",
            "git commit -m \"$(date)\"",
            "git add *.rs",
            "git log > out.txt",
            "git commit -m 'open",
        ] {
            assert!(split(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_git_lines_run() {
        assert_eq!(args("$ git status"), Some(vec!["status".to_owned()]));
        assert_eq!(args("ls -la"), None);
        assert_eq!(args("git"), None);
        assert_eq!(
            CommandLine::parse("git switch <name>").why_not,
            Some("Uses shell syntax; run it in a terminal")
        );
    }

    #[test]
    fn flags_commands_that_lose_work() {
        let d = |l: &str| CommandLine::parse(l).is_destructive();
        assert!(d("git reset --hard HEAD~1"));
        assert!(d("git push --force-with-lease origin main"));
        assert!(d("git push origin +main"));
        assert!(d("git clean -fd"));
        assert!(d("git branch -D old"));
        assert!(d("git stash drop"));
        assert!(d("git restore src/a.rs"));
        assert!(d("git checkout -- src/a.rs"));
        assert!(!d("git reset --soft HEAD~1"));
        assert!(!d("git restore --staged src/a.rs"));
        assert!(!d("git push -u origin feature"));
        assert!(!d("git branch -d merged"));
        assert!(!d("git switch -c new"));
        assert!(!d("git status"));
    }

    #[test]
    fn keeps_list_items_apart() {
        let t = "## Summary\nTwo steps:\nfirst this.\n\n- **Safe:** keeps\n  your files\n- Local only\n1. One";
        assert_eq!(
            prose_blocks(t),
            [
                "Summary",
                "Two steps: first this.",
                "• Safe: keeps your files",
                "• Local only",
                "1. One"
            ]
        );
    }

    #[test]
    fn splits_an_answer() {
        let a = "Undo it but keep the changes:\n\n```bash\n# soft reset\n$ git reset --soft HEAD~1\ngit status\n```\n\nThen commit again.\n```rust\nfn a() {}\n```";
        let p = parts(a);
        assert_eq!(p.len(), 4);
        assert_eq!(p[0], Part::Text("Undo it but keep the changes:".into()));
        let Part::Commands(c) = &p[1] else {
            panic!("{p:?}")
        };
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].line, "git reset --soft HEAD~1");
        assert_eq!(p[2], Part::Text("Then commit again.".into()));
        assert_eq!(p[3], Part::Code("fn a() {}".into()));
    }
}
