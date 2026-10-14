//! The interactive rebase editor's plan, kept in graph order (newest first, like the
//! commit list) and turned into git's oldest-first todo list when the rebase starts.
//! A squash or fixup row melds into the row below it.

use switchhead_git::rebase::{Action, TodoItem};

/// The rows being edited.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    rows: Vec<TodoItem>,
    /// The ids in their original order, to tell whether anything changed.
    original: Vec<switchhead_git::Oid>,
}

impl Plan {
    /// A plan from [`switchhead_git::rebase::plan`]'s oldest-first list.
    pub fn new(mut oldest_first: Vec<TodoItem>) -> Self {
        oldest_first.reverse();
        let original = oldest_first.iter().map(|i| i.id).collect();
        Self {
            rows: oldest_first,
            original,
        }
    }

    /// The rows, newest first.
    pub fn rows(&self) -> &[TodoItem] {
        &self.rows
    }

    /// Row count.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// No commits to rebase.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Set row `i`'s action.
    pub fn set_action(&mut self, i: usize, action: Action) {
        if let Some(r) = self.rows.get_mut(i) {
            r.action = action;
        }
    }

    /// Set the new message of row `i` (empty clears it).
    pub fn set_message(&mut self, i: usize, message: &str) {
        if let Some(r) = self.rows.get_mut(i) {
            r.message = (!message.trim().is_empty()).then(|| message.to_owned());
        }
    }

    /// Move row `from` so it ends up at index `to`.
    pub fn move_row(&mut self, from: usize, to: usize) {
        if from >= self.rows.len() || to >= self.rows.len() || from == to {
            return;
        }
        let row = self.rows.remove(from);
        self.rows.insert(to, row);
    }

    /// Anything differs from a plain replay (order, an action, a message).
    pub fn is_changed(&self) -> bool {
        self.rows
            .iter()
            .zip(&self.original)
            .any(|(r, id)| r.id != *id || r.action != Action::Pick || r.message.is_some())
    }

    /// The row a squash or fixup at `i` melds into: the nearest kept row below it that
    /// is not itself squashed (`None` when there is none, or `i` is not melded).
    pub fn meld_target(&self, i: usize) -> Option<usize> {
        if !matches!(self.rows.get(i)?.action, Action::Squash | Action::Fixup) {
            return None;
        }
        (i + 1..self.rows.len()).find(|&j| {
            !matches!(
                self.rows[j].action,
                Action::Drop | Action::Squash | Action::Fixup
            )
        })
    }

    /// Why the plan cannot run, if it can't.
    pub fn problem(&self) -> Option<String> {
        if let Some(oldest) = self.rows.iter().rev().find(|r| r.action != Action::Drop)
            && matches!(oldest.action, Action::Squash | Action::Fixup)
        {
            return Some(format!(
                "\"{}\" can't be squashed: there is no commit below it to meld into.",
                oldest.summary
            ));
        }
        self.rows
            .iter()
            .find(|r| r.action == Action::Reword && r.message.is_none())
            .map(|r| format!("Write the new message for \"{}\".", r.summary))
    }

    /// Commits left after the rebase.
    pub fn result_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r.action, Action::Pick | Action::Reword | Action::Edit))
            .count()
    }

    /// git's todo list, oldest first.
    pub fn todo(&self) -> Vec<TodoItem> {
        self.rows.iter().rev().cloned().collect()
    }
}

/// The editor's name for an action, and its one-key shortcut.
pub fn action_label(a: Action) -> (&'static str, &'static str) {
    match a {
        Action::Pick => ("Pick", "P"),
        Action::Reword => ("Reword", "R"),
        Action::Edit => ("Edit", "E"),
        Action::Squash => ("Squash", "S"),
        Action::Fixup => ("Fixup", "F"),
        Action::Drop => ("Drop", "D"),
    }
}

/// Every action, in menu order.
pub const ACTIONS: [Action; 6] = [
    Action::Pick,
    Action::Reword,
    Action::Edit,
    Action::Squash,
    Action::Fixup,
    Action::Drop,
];

#[cfg(test)]
mod tests {
    use super::*;
    use switchhead_git::Oid;

    fn item(n: u8, summary: &str) -> TodoItem {
        TodoItem {
            action: Action::Pick,
            id: Oid::from_bytes_or_panic(&[n; 20]),
            summary: summary.into(),
            message: None,
        }
    }

    fn plan() -> Plan {
        // Oldest first, as git lists them.
        Plan::new(vec![item(1, "a"), item(2, "b"), item(3, "c")])
    }

    fn summaries(items: &[TodoItem]) -> Vec<&str> {
        items.iter().map(|i| i.summary.as_str()).collect()
    }

    #[test]
    fn shows_newest_first_and_runs_oldest_first() {
        let p = plan();
        assert_eq!(summaries(p.rows()), ["c", "b", "a"]);
        assert_eq!(summaries(&p.todo()), ["a", "b", "c"]);
        assert!(!p.is_changed());
    }

    #[test]
    fn moves_rows() {
        let mut p = plan();
        p.move_row(0, 2);
        assert_eq!(summaries(p.rows()), ["b", "a", "c"]);
        p.move_row(2, 0);
        assert_eq!(summaries(p.rows()), ["c", "b", "a"]);
        assert!(!p.is_changed());
        p.move_row(1, 0);
        assert!(p.is_changed());
        p.move_row(0, 9);
        assert_eq!(summaries(p.rows()), ["b", "c", "a"]);
    }

    #[test]
    fn finds_what_a_squash_melds_into() {
        let mut p = plan();
        p.set_action(0, Action::Squash);
        p.set_action(1, Action::Fixup);
        assert_eq!(p.meld_target(0), Some(2));
        assert_eq!(p.meld_target(1), Some(2));
        assert_eq!(p.meld_target(2), None);
        assert_eq!(p.result_count(), 1);
        p.set_action(2, Action::Drop);
        assert_eq!(p.meld_target(0), None);
        assert!(p.problem().is_some());
    }

    #[test]
    fn reports_problems() {
        let mut p = plan();
        assert_eq!(p.problem(), None);
        p.set_action(2, Action::Fixup);
        assert!(p.problem().unwrap().contains("\"a\""));
        p.set_action(2, Action::Pick);
        p.set_action(1, Action::Reword);
        assert!(p.problem().unwrap().contains("\"b\""));
        p.set_message(1, "  ");
        assert!(p.problem().is_some());
        p.set_message(1, "B");
        assert_eq!(p.problem(), None);
        assert!(p.is_changed());
    }
}
