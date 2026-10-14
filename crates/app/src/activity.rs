//! The activity log: each git operation a repository tab ran, with its progress and
//! output, newest first. Progress lines that only update a counter ("Receiving objects:
//! 45%") replace the previous line instead of piling up.

use std::collections::VecDeque;
use std::time::Instant;

/// Operations kept per tab.
pub const MAX_ENTRIES: usize = 50;
/// Output lines kept per operation (the first ones and the error are what matter).
pub const MAX_LINES: usize = 400;

/// How an operation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Still running.
    Running,
    /// Succeeded.
    Done,
    /// git failed.
    Failed,
    /// Stopped by the user.
    Cancelled,
}

/// One operation.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Serial number within the tab.
    pub id: u64,
    /// What ran ("Fetching").
    pub label: String,
    /// Local wall-clock time it started (`14:02:31`).
    pub at: String,
    started: Instant,
    /// How long it took, once finished.
    pub ms: Option<u64>,
    /// How it ended.
    pub outcome: Outcome,
    /// Progress and output lines.
    pub lines: Vec<String>,
}

/// A tab's log.
#[derive(Debug, Default)]
pub struct Activity {
    entries: VecDeque<Entry>,
    next: u64,
}

impl Activity {
    /// Start an entry; returns its id.
    pub fn start(&mut self, label: &str) -> u64 {
        let id = self.next;
        self.next += 1;
        self.entries.push_front(Entry {
            id,
            label: label.to_owned(),
            at: chrono::Local::now().format("%H:%M:%S").to_string(),
            started: Instant::now(),
            ms: None,
            outcome: Outcome::Running,
            lines: Vec::new(),
        });
        self.entries.truncate(MAX_ENTRIES);
        id
    }

    /// Add a progress or output line to entry `id`.
    pub fn line(&mut self, id: u64, line: &str) {
        if let Some(e) = self.get_mut(id) {
            push_line(&mut e.lines, line);
        }
    }

    /// End entry `id`; `message` (an error) is appended line by line.
    pub fn finish(&mut self, id: u64, outcome: Outcome, message: &str) {
        if let Some(e) = self.get_mut(id) {
            e.outcome = outcome;
            e.ms = Some(e.started.elapsed().as_millis() as u64);
            for l in message.lines().filter(|l| !l.trim().is_empty()) {
                if e.lines.last().map(String::as_str) != Some(l) {
                    push_line(&mut e.lines, l);
                }
            }
        }
    }

    /// Entries, newest first.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// No operation ran yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }
}

/// Append `line`, replacing the last line when both are updates of the same progress
/// counter (same text before the first digit, and the last one has a `%` or `/`).
pub fn push_line(lines: &mut Vec<String>, line: &str) {
    let line = line.trim_end();
    if line.is_empty() {
        return;
    }
    if let Some(last) = lines.last_mut()
        && is_progress(last)
        && progress_key(last) == progress_key(line)
    {
        *last = line.to_owned();
        return;
    }
    if lines.len() < MAX_LINES {
        lines.push(line.to_owned());
    } else if lines.len() == MAX_LINES {
        lines.push("…".to_owned());
    }
}

/// The line of a git error worth showing in the status bar: a rejected ref, then the
/// first `fatal:` or `error:` line, else the first line; runs of spaces collapsed.
pub fn summary(message: &str) -> String {
    let lines: Vec<&str> = message
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let pick = lines
        .iter()
        .find(|l| l.contains("[rejected]") || l.contains("[remote rejected]"))
        .or_else(|| {
            lines
                .iter()
                .find(|l| l.starts_with("fatal:") || l.starts_with("error:"))
        })
        .or(lines.first())
        .copied()
        .unwrap_or("failed");
    pick.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_progress(line: &str) -> bool {
    line.contains('%') || line.contains('/')
}
/// The part before the first digit ("Receiving objects:"), without the padding git puts
/// before short numbers.
/// The part before the first digit ("Receiving objects: ").
fn progress_key(line: &str) -> &str {
    let end = line
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(line.len());
    line[..end].trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_updates_replace_the_last_line() {
        let mut lines = Vec::new();
        for l in [
            "remote: Enumerating objects: 5, done.",
            "Receiving objects:  10% (1/10)",
            "Receiving objects:  50% (5/10)",
            "Receiving objects: 100% (10/10), done.",
            "Resolving deltas: 100% (2/2), done.",
            "From ../up",
            " * [new branch]      x -> origin/x",
        ] {
            push_line(&mut lines, l);
        }
        assert_eq!(
            lines,
            [
                "remote: Enumerating objects: 5, done.",
                "Receiving objects: 100% (10/10), done.",
                "Resolving deltas: 100% (2/2), done.",
                "From ../up",
                " * [new branch]      x -> origin/x",
            ]
        );
    }

    #[test]
    fn keeps_the_newest_entries_and_the_error() {
        let mut a = Activity::default();
        let first = a.start("Fetching");
        for _ in 0..MAX_ENTRIES {
            a.start("Pulling");
        }
        assert_eq!(a.entries().count(), MAX_ENTRIES);
        assert!(a.entries().all(|e| e.id != first));
        let id = a.start("Pushing");
        a.line(id, "To ../up");
        a.finish(
            id,
            Outcome::Failed,
            "To ../up\n ! [rejected] main -> main (fetch first)\n",
        );
        let e = a.entries().next().unwrap();
        assert_eq!(e.outcome, Outcome::Failed);
        assert!(e.ms.is_some());
        assert_eq!(
            e.lines,
            ["To ../up", " ! [rejected] main -> main (fetch first)"]
        );
    }

    #[test]
    fn summarizes_errors() {
        let push = "git push --progress origin main failed: To ../up\n ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs to '../up'\nhint: Updates were rejected";
        assert_eq!(summary(push), "! [rejected] main -> main (fetch first)");
        let fetch = "git fetch failed: \nfatal: 'nope' does not appear to be a git repository\nfatal: Could not read";
        assert_eq!(
            summary(fetch),
            "fatal: 'nope' does not appear to be a git repository"
        );
        assert_eq!(summary("something odd\nmore"), "something odd");
        assert_eq!(summary(""), "failed");
    }

    #[test]
    fn caps_the_output() {
        let mut lines = Vec::new();
        for i in 0..MAX_LINES + 10 {
            push_line(&mut lines, &format!("line {i} done"));
        }
        assert_eq!(lines.len(), MAX_LINES + 1);
        assert_eq!(lines.last().map(String::as_str), Some("…"));
    }
}
