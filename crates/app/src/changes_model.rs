//! The data behind Local Changes, kept apart from the view so it can be unit-tested:
//! which status entries go in the unstaged and staged lists, and the multi-selection
//! (click, Ctrl/Cmd-click, Shift-click, arrow keys) that survives a status reload.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use switchhead_git::status::{Change, Entry, EntryKind, Status};

/// One of the two file lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// Work tree changes, untracked and conflicted files.
    Unstaged,
    /// Index changes (what the next commit takes).
    Staged,
}

impl Side {
    /// The other list.
    pub fn other(self) -> Side {
        match self {
            Side::Unstaged => Side::Staged,
            Side::Staged => Side::Unstaged,
        }
    }
}

/// The two lists, as indices into `Status::entries`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lists {
    /// Unstaged: changed, untracked and conflicted entries.
    pub unstaged: Vec<usize>,
    /// Staged entries.
    pub staged: Vec<usize>,
}

impl Lists {
    /// Split `status` into the two lists (an entry with both staged and unstaged
    /// changes is in both). Conflicts come first in the unstaged list.
    pub fn new(status: &Status) -> Self {
        let mut lists = Lists::default();
        let mut conflicts = Vec::new();
        for (i, e) in status.entries.iter().enumerate() {
            if e.conflict().is_some() {
                conflicts.push(i);
            } else if e.is_unstaged() {
                lists.unstaged.push(i);
            }
            if e.is_staged() {
                lists.staged.push(i);
            }
        }
        conflicts.append(&mut lists.unstaged);
        lists.unstaged = conflicts;
        lists
    }

    /// One list.
    pub fn get(&self, side: Side) -> &[usize] {
        match side {
            Side::Unstaged => &self.unstaged,
            Side::Staged => &self.staged,
        }
    }
}

/// How an entry shows in one list: a one-letter badge and its tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// New file (staged add, or untracked).
    Added,
    /// Untracked (`?`).
    Untracked,
    /// Modified or type changed.
    Modified,
    /// Deleted.
    Deleted,
    /// Renamed or copied.
    Renamed,
    /// In conflict.
    Conflict,
}

impl Badge {
    /// The entry's badge in `side`'s list.
    pub fn of(e: &Entry, side: Side) -> Badge {
        match e.kind {
            EntryKind::Untracked => return Badge::Untracked,
            EntryKind::Unmerged(_) => return Badge::Conflict,
            _ => {}
        }
        let change = match side {
            Side::Unstaged => e.worktree,
            Side::Staged => e.index,
        };
        match change {
            Change::Added => Badge::Added,
            Change::Deleted => Badge::Deleted,
            Change::Renamed | Change::Copied => Badge::Renamed,
            _ => Badge::Modified,
        }
    }

    /// The letter shown.
    pub fn letter(self) -> &'static str {
        match self {
            Badge::Added => "A",
            Badge::Untracked => "?",
            Badge::Modified => "M",
            Badge::Deleted => "D",
            Badge::Renamed => "R",
            Badge::Conflict => "!",
        }
    }
}

/// The selected files: all in one list, with an anchor for Shift-click and a cursor
/// (the file whose diff shows, moved by the arrow keys).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The list the selection is in.
    pub side: Side,
    /// Selected paths.
    pub paths: HashSet<PathBuf>,
    /// Where Shift-click ranges start.
    pub anchor: Option<PathBuf>,
    /// The focused file, and its position when it was focused (to land nearby when
    /// it leaves the list).
    pub cursor: Option<(PathBuf, usize)>,
}

impl Default for Selection {
    fn default() -> Self {
        Selection {
            side: Side::Unstaged,
            paths: HashSet::new(),
            anchor: None,
            cursor: None,
        }
    }
}

impl Selection {
    /// Is `path` in `side` selected?
    pub fn contains(&self, side: Side, path: &Path) -> bool {
        self.side == side && self.paths.contains(path)
    }

    /// Plain click: select only `list[i]`.
    pub fn click(&mut self, side: Side, list: &[PathBuf], i: usize) {
        let Some(p) = list.get(i) else {
            return;
        };
        self.side = side;
        self.paths = HashSet::from([p.clone()]);
        self.anchor = Some(p.clone());
        self.cursor = Some((p.clone(), i));
    }

    /// Ctrl/Cmd-click: add or remove `list[i]` (a click in the other list starts over).
    pub fn toggle(&mut self, side: Side, list: &[PathBuf], i: usize) {
        let Some(p) = list.get(i) else {
            return;
        };
        if self.side != side {
            return self.click(side, list, i);
        }
        if !self.paths.remove(p) {
            self.paths.insert(p.clone());
        }
        self.anchor = Some(p.clone());
        self.cursor = Some((p.clone(), i));
    }

    /// Shift-click: select from the anchor to `list[i]`.
    pub fn extend(&mut self, side: Side, list: &[PathBuf], i: usize) {
        let from = self
            .anchor
            .as_ref()
            .filter(|_| self.side == side)
            .and_then(|a| list.iter().position(|p| p == a));
        let Some(from) = from else {
            return self.click(side, list, i);
        };
        let Some(p) = list.get(i) else {
            return;
        };
        let (a, b) = (from.min(i), from.max(i));
        self.paths = list[a..=b].iter().cloned().collect();
        self.cursor = Some((p.clone(), i));
    }

    /// Select every file of `side`.
    pub fn all(&mut self, side: Side, list: &[PathBuf]) {
        self.side = side;
        self.paths = list.iter().cloned().collect();
        if self.cursor.is_none()
            && let Some(p) = list.first()
        {
            self.cursor = Some((p.clone(), 0));
        }
    }

    /// Arrow keys: move the cursor by `delta` within the current list and select it alone.
    pub fn step(&mut self, list: &[PathBuf], delta: isize) {
        if list.is_empty() {
            return;
        }
        let cur = self
            .cursor
            .as_ref()
            .and_then(|(p, _)| list.iter().position(|q| q == p))
            .map_or(-1, |i| i as isize);
        let next = (cur + delta).clamp(0, list.len() as isize - 1) as usize;
        self.click(self.side, list, next);
    }

    /// After a reload: drop paths that left the list. When the cursor left, select the
    /// file now at its position (or the last one), so staging one file after another
    /// walks down the list like Fork. Returns the cursor's path.
    pub fn retain(&mut self, list: &[PathBuf]) -> Option<PathBuf> {
        self.paths.retain(|p| list.contains(p));
        if self.anchor.as_ref().is_some_and(|a| !list.contains(a)) {
            self.anchor = None;
        }
        match self.cursor.clone() {
            Some((p, _)) if list.contains(&p) => {
                let i = list.iter().position(|q| *q == p).unwrap_or(0);
                self.cursor = Some((p.clone(), i));
                Some(p)
            }
            Some((_, i)) if self.paths.is_empty() && !list.is_empty() => {
                self.click(self.side, list, i.min(list.len() - 1));
                self.cursor.as_ref().map(|(p, _)| p.clone())
            }
            _ => {
                self.cursor = None;
                None
            }
        }
    }
}

/// Lines picked in the focused file's diff for line staging. Only changed lines can
/// be picked; `changed` is their sorted indices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineSelection {
    /// Picked line indices.
    pub lines: HashSet<usize>,
    anchor: Option<usize>,
}

impl LineSelection {
    /// Plain click: pick only `l` (clicking the only picked line clears it).
    pub fn click(&mut self, l: usize) {
        if self.lines.len() == 1 && self.lines.contains(&l) {
            self.lines.clear();
        } else {
            self.lines = HashSet::from([l]);
        }
        self.anchor = Some(l);
    }

    /// Ctrl/Cmd-click: add or remove `l`.
    pub fn toggle(&mut self, l: usize) {
        if !self.lines.remove(&l) {
            self.lines.insert(l);
        }
        self.anchor = Some(l);
    }

    /// Shift-click: pick the changed lines from the anchor to `l`.
    pub fn extend(&mut self, l: usize, changed: &[usize]) {
        let Some(a) = self.anchor else {
            return self.click(l);
        };
        let (lo, hi) = (a.min(l), a.max(l));
        self.lines = changed
            .iter()
            .copied()
            .filter(|i| (lo..=hi).contains(i))
            .collect();
    }

    /// Forget the picks (another file, or the diff changed).
    pub fn clear(&mut self) {
        self.lines.clear();
        self.anchor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchhead_git::status;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn splits_status_into_lists() {
        // Staged+unstaged `both`, unstaged `mod`, staged-only `new`, untracked `u`,
        // conflicted `c`.
        let out = b"# branch.oid (initial)\0# branch.head main\0\
1 MM N... 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa both\0\
1 .M N... 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa mod\0\
1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa new\0\
u UU N... 100644 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa c\0\
? u\0";
        let st = status::parse(out).unwrap();
        let lists = Lists::new(&st);
        let names = |ix: &[usize]| -> Vec<String> {
            ix.iter()
                .map(|i| st.entries[*i].path.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(names(&lists.unstaged), ["c", "both", "mod", "u"]);
        assert_eq!(names(&lists.staged), ["both", "new"]);
        let badge = |name: &str, side| {
            let e = st
                .entries
                .iter()
                .find(|e| e.path == Path::new(name))
                .unwrap();
            Badge::of(e, side)
        };
        assert_eq!(badge("new", Side::Staged), Badge::Added);
        assert_eq!(badge("u", Side::Unstaged), Badge::Untracked);
        assert_eq!(badge("c", Side::Unstaged), Badge::Conflict);
        assert_eq!(badge("both", Side::Unstaged), Badge::Modified);
    }

    #[test]
    fn click_toggle_and_shift_extend() {
        let list = paths(&["a", "b", "c", "d"]);
        let mut s = Selection::default();
        s.click(Side::Unstaged, &list, 1);
        s.extend(Side::Unstaged, &list, 3);
        assert_eq!(s.paths, paths(&["b", "c", "d"]).into_iter().collect());
        s.toggle(Side::Unstaged, &list, 2);
        assert_eq!(s.paths, paths(&["b", "d"]).into_iter().collect());
        // A click in the other list starts over there.
        s.toggle(Side::Staged, &list, 0);
        assert_eq!(s.side, Side::Staged);
        assert_eq!(s.paths, paths(&["a"]).into_iter().collect());
        s.step(&list, 1);
        assert_eq!(s.cursor, Some((PathBuf::from("b"), 1)));
        s.step(&list, 10);
        assert_eq!(s.cursor, Some((PathBuf::from("d"), 3)));
    }

    #[test]
    fn staged_file_hands_selection_to_the_next() {
        let mut s = Selection::default();
        let list = paths(&["a", "b", "c"]);
        s.click(Side::Unstaged, &list, 1);
        // `b` was staged: `c` now sits at its position.
        assert_eq!(s.retain(&paths(&["a", "c"])), Some(PathBuf::from("c")));
        // The last one goes: select the new last.
        assert_eq!(s.retain(&paths(&["a"])), Some(PathBuf::from("a")));
        assert_eq!(s.retain(&[]), None);
        assert!(s.paths.is_empty());
    }

    #[test]
    fn picks_changed_lines() {
        let changed = [2, 3, 4, 8, 9];
        let mut s = LineSelection::default();
        s.click(3);
        s.extend(8, &changed);
        assert_eq!(s.lines, HashSet::from([3, 4, 8]));
        s.toggle(4);
        assert_eq!(s.lines, HashSet::from([3, 8]));
        s.click(9);
        s.click(9);
        assert!(s.lines.is_empty());
    }
}
