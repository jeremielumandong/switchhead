//! The commands in an assistant's answer: the answer split into prose and code blocks,
//! and each line of a shell block split into words the way a shell would, so a plain
//! `git ...` line can run without a shell. Lines with shell syntax (pipes, redirects,
//! variables, placeholders like `<branch>`) are shown but not runnable. Prose keeps its
//! lists and headings, with inline Markdown (code, bold, italic, links) as styles.

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

/// How a span of prose is styled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    /// `**strong**`.
    pub bold: bool,
    /// `*emphasis*`.
    pub italic: bool,
    /// `` `code` ``.
    pub code: bool,
    /// A link's URL (http and https only; other links show as text).
    pub link: Option<String>,
}

/// Text without its Markdown markers, with styled byte ranges (sorted, not overlapping).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inline {
    /// The text as shown.
    pub text: String,
    /// Styled ranges of `text`.
    pub spans: Vec<(std::ops::Range<usize>, Marks)>,
}

impl Inline {
    fn push(&mut self, s: &str, marks: &Marks) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(s);
        if *marks == Marks::default() {
            return;
        }
        match self.spans.last_mut() {
            Some((r, m)) if r.end == start && m == marks => r.end = self.text.len(),
            _ => self.spans.push((start..self.text.len(), marks.clone())),
        }
    }

    /// Parse one paragraph's inline Markdown: `code`, **bold**, *italic*, [links](url).
    /// Underscores stay as they are (they are common in names).
    pub fn parse(src: &str) -> Self {
        let mut out = Inline::default();
        let mut marks = Marks::default();
        let mut rest = src;
        while let Some(c) = rest.chars().next() {
            let after = &rest[c.len_utf8()..];
            match c {
                '`' => {
                    if let Some(end) = after.find('`') {
                        let code = Marks {
                            code: true,
                            ..marks.clone()
                        };
                        out.push(&after[..end], &code);
                        rest = &after[end + 1..];
                        continue;
                    }
                }
                '*' if after.starts_with('*') => {
                    marks.bold = !marks.bold;
                    rest = &after[1..];
                    continue;
                }
                '*' if marks.italic || after.starts_with(|c: char| !c.is_whitespace()) => {
                    marks.italic = !marks.italic;
                    rest = after;
                    continue;
                }
                '[' => {
                    let link = after.find("](").and_then(|close| {
                        let url_end = after[close + 2..].find(')')?;
                        let url = &after[close + 2..close + 2 + url_end];
                        (url.starts_with("https://") || url.starts_with("http://"))
                            .then(|| (&after[..close], url, close + 2 + url_end + 1))
                    });
                    if let Some((label, url, len)) = link {
                        let m = Marks {
                            link: Some(url.to_owned()),
                            ..marks.clone()
                        };
                        out.push(label, &m);
                        rest = &after[len..];
                        continue;
                    }
                }
                _ => {}
            }
            out.push(&rest[..c.len_utf8()], &marks);
            rest = after;
        }
        out
    }
}

/// A block of prose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prose {
    /// A paragraph, its lines joined.
    Paragraph(Inline),
    /// A heading.
    Heading(Inline),
    /// A list item: its marker (`•`, `1.`) and nesting depth.
    Item {
        /// `•` or the number.
        marker: String,
        /// 0 for a top-level item.
        depth: usize,
        /// The item's text.
        text: Inline,
    },
}

/// Prose as display blocks: paragraphs with their lines joined, headings, and each list
/// item on its own, with inline Markdown turned into styles.
pub fn prose_blocks(text: &str) -> Vec<Prose> {
    enum Raw {
        Paragraph(String),
        Heading(String),
        Item(String, usize, String),
    }
    let mut blocks: Vec<Raw> = Vec::new();
    let mut open = false;
    for raw in text.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if line.is_empty() {
            open = false;
            continue;
        }
        if let Some(h) = line.strip_prefix('#') {
            blocks.push(Raw::Heading(h.trim_start_matches('#').trim().to_owned()));
            open = false;
            continue;
        }
        let item = match line.split_once(' ') {
            Some(("-" | "*" | "+", rest)) => Some(("•".to_owned(), rest)),
            Some((n, rest))
                if n.len() >= 2
                    && n.len() <= 4
                    && (n.ends_with('.') || n.ends_with(')'))
                    && n[..n.len() - 1].chars().all(|c| c.is_ascii_digit()) =>
            {
                Some((n.to_owned(), rest))
            }
            _ => None,
        };
        match (item, blocks.last_mut()) {
            (Some((marker, rest)), _) => {
                blocks.push(Raw::Item(marker, indent / 2, rest.trim().to_owned()));
                open = true;
            }
            (None, Some(Raw::Paragraph(t) | Raw::Item(_, _, t))) if open => {
                t.push(' ');
                t.push_str(line);
            }
            (None, _) => {
                blocks.push(Raw::Paragraph(line.to_owned()));
                open = true;
            }
        }
    }
    blocks
        .into_iter()
        .map(|b| match b {
            Raw::Paragraph(t) => Prose::Paragraph(Inline::parse(&t)),
            Raw::Heading(t) => Prose::Heading(Inline::parse(&t)),
            Raw::Item(marker, depth, t) => Prose::Item {
                marker,
                depth,
                text: Inline::parse(&t),
            },
        })
        .collect()
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
        let t = "## Summary\nTwo steps:\nfirst this.\n\n- **Safe:** keeps\n  your files\n  - Local only\n1. One";
        let plain: Vec<(String, String)> = prose_blocks(t)
            .into_iter()
            .map(|b| match b {
                Prose::Paragraph(t) => ("p".into(), t.text),
                Prose::Heading(t) => ("h".into(), t.text),
                Prose::Item {
                    marker,
                    depth,
                    text,
                } => (format!("{marker}{depth}"), text.text),
            })
            .collect();
        let want = [
            ("h", "Summary"),
            ("p", "Two steps: first this."),
            ("•0", "Safe: keeps your files"),
            ("•1", "Local only"),
            ("1.0", "One"),
        ];
        assert_eq!(
            plain,
            want.map(|(a, b)| (a.to_owned(), b.to_owned())).to_vec()
        );
    }

    #[test]
    fn styles_inline_markdown() {
        let i = Inline::parse(
            "Run `git reset --soft`, **not** *hard*: see [docs](https://git-scm.com) or my_branch_name.",
        );
        assert_eq!(
            i.text,
            "Run git reset --soft, not hard: see docs or my_branch_name."
        );
        let marked: Vec<(&str, &Marks)> = i
            .spans
            .iter()
            .map(|(r, m)| (&i.text[r.clone()], m))
            .collect();
        assert_eq!(marked.len(), 4);
        assert_eq!(marked[0].0, "git reset --soft");
        assert!(marked[0].1.code);
        assert!(marked[1].1.bold && marked[1].0 == "not");
        assert!(marked[2].1.italic && marked[2].0 == "hard");
        assert_eq!(marked[3].1.link.as_deref(), Some("https://git-scm.com"));
        // A lone star is text.
        assert_eq!(Inline::parse("a * b").text, "a * b");
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
