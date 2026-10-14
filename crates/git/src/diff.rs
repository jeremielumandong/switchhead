//! Diffs: the files a commit changed, a text diff of one file, and `git diff` output.
//!
//! Commit file lists ([`commit_files`], [`tree_files`]) and blob diffs ([`diff_blobs`]) are
//! read in process with gitoxide: histogram diff with git's line post-processing, plus
//! word-level highlights inside changed hunks. Index and work-tree diffs come from
//! `git diff` (fsmonitor, filters, the user's config) and go through [`parse_unified`].
//! Both produce the same [`TextDiff`], which partial staging ([`crate::stage`]) also reads.
//!
//! A [`TextDiff`] keeps all line text in one string arena; lines, hunks and word
//! highlights are flat vectors of ranges into it.

use std::ops::Range;
use std::path::{Path, PathBuf};

use gix::diff::blob::{Algorithm, Diff, InternedInput};

use crate::{GixExt as _, Oid, Repo, Result, path_from_bytes};

/// Files larger than this are not diffed as text.
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// How a file changed between two trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileStatus {
    /// New file.
    Added,
    /// Removed file.
    Deleted,
    /// Content or mode changed.
    Modified,
    /// Moved (possibly with changes).
    Renamed,
    /// Copied from another file.
    Copied,
    /// Turned into or from a symlink or submodule.
    TypeChanged,
}

impl FileStatus {
    /// One-letter code, as `git diff --name-status` prints it.
    pub fn letter(self) -> char {
        match self {
            FileStatus::Added => 'A',
            FileStatus::Deleted => 'D',
            FileStatus::Modified => 'M',
            FileStatus::Renamed => 'R',
            FileStatus::Copied => 'C',
            FileStatus::TypeChanged => 'T',
        }
    }
}

/// A changed file in a commit or between two trees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    /// Path after the change.
    pub path: PathBuf,
    /// Path before a rename or copy.
    pub old_path: Option<PathBuf>,
    /// What happened.
    pub status: FileStatus,
    /// Blob before (`None` for an added file).
    pub old_id: Option<Oid>,
    /// Blob after (`None` for a deleted file).
    pub new_id: Option<Oid>,
    /// The entry is a submodule commit, not a blob.
    pub submodule: bool,
}

/// The files `commit` changed against its first parent (or everything, for a root commit).
pub fn commit_files(repo: &Repo, commit: Oid) -> Result<Vec<FileChange>> {
    let r = repo.local();
    let c = r.find_commit(commit).gix()?;
    let parent = c.parent_ids().next().map(|p| p.detach());
    tree_files(repo, parent, commit)
}

/// The files that differ between commits `old` (`None`: the empty tree) and `new`, with
/// renames detected like `git diff -M`. Sorted by path.
pub fn tree_files(repo: &Repo, old: Option<Oid>, new: Oid) -> Result<Vec<FileChange>> {
    use gix::object::tree::diff::ChangeDetached as C;

    let started = std::time::Instant::now();
    let r = repo.local();
    let new_tree = r.find_commit(new).gix()?.tree().gix()?;
    let old_tree = match old {
        Some(id) => r.find_commit(id).gix()?.tree().gix()?,
        None => r.empty_tree(),
    };
    let opts = gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default()));
    let changes = r
        .diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(opts))
        .gix()?;
    let mut out = Vec::with_capacity(changes.len());
    for change in changes {
        let fc = match change {
            C::Addition {
                location,
                entry_mode,
                id,
                ..
            } => {
                if entry_mode.is_tree() {
                    continue;
                }
                FileChange {
                    path: path_from_bytes(&location),
                    old_path: None,
                    status: FileStatus::Added,
                    old_id: None,
                    new_id: Some(id),
                    submodule: entry_mode.is_commit(),
                }
            }
            C::Deletion {
                location,
                entry_mode,
                id,
                ..
            } => {
                if entry_mode.is_tree() {
                    continue;
                }
                FileChange {
                    path: path_from_bytes(&location),
                    old_path: None,
                    status: FileStatus::Deleted,
                    old_id: Some(id),
                    new_id: None,
                    submodule: entry_mode.is_commit(),
                }
            }
            C::Modification {
                location,
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
            } => {
                if entry_mode.is_tree() && previous_entry_mode.is_tree() {
                    continue;
                }
                let kind_changed = previous_entry_mode.is_link() != entry_mode.is_link()
                    || previous_entry_mode.is_commit() != entry_mode.is_commit()
                    || previous_entry_mode.is_tree() != entry_mode.is_tree();
                FileChange {
                    path: path_from_bytes(&location),
                    old_path: None,
                    status: if kind_changed {
                        FileStatus::TypeChanged
                    } else {
                        FileStatus::Modified
                    },
                    old_id: Some(previous_id),
                    new_id: Some(id),
                    submodule: entry_mode.is_commit(),
                }
            }
            C::Rewrite {
                source_location,
                source_id,
                entry_mode,
                id,
                location,
                copy,
                ..
            } => FileChange {
                path: path_from_bytes(&location),
                old_path: Some(path_from_bytes(&source_location)),
                status: if copy {
                    FileStatus::Copied
                } else {
                    FileStatus::Renamed
                },
                old_id: Some(source_id),
                new_id: Some(id),
                submodule: entry_mode.is_commit(),
            },
        };
        out.push(fc);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    tracing::debug!(
        repo = %repo.name(),
        files = out.len(),
        ms = started.elapsed().as_millis() as u64,
        "tree diff"
    );
    Ok(out)
}

/// Kind of a diff line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineKind {
    /// Unchanged context.
    Context,
    /// Only in the new side.
    Added,
    /// Only in the old side.
    Removed,
}

impl LineKind {
    /// The unified-diff prefix (` `, `+`, `-`).
    pub fn prefix(self) -> char {
        match self {
            LineKind::Context => ' ',
            LineKind::Added => '+',
            LineKind::Removed => '-',
        }
    }
}

/// One line of a [`TextDiff`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffLine {
    /// Context, added or removed.
    pub kind: LineKind,
    /// 1-based line number on the old side (context and removed lines).
    pub old_no: Option<u32>,
    /// 1-based line number on the new side (context and added lines).
    pub new_no: Option<u32>,
    /// The line has no newline at the end (last line of a file).
    pub no_eol: bool,
    text: (u32, u32),
    words: (u32, u32),
}

/// A hunk: a run of lines with its `@@` header numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunkHeader {
    /// First old line (1-based; 0 when the old side is empty).
    pub old_start: u32,
    /// Old lines covered.
    pub old_lines: u32,
    /// First new line (1-based; 0 when the new side is empty).
    pub new_start: u32,
    /// New lines covered.
    pub new_lines: u32,
    /// Text after the second `@@` (git's function context), if any.
    pub section: String,
    /// The hunk's lines: indices into [`TextDiff::line`].
    pub lines: Range<usize>,
}

/// A text diff of one file, in one arena.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextDiff {
    text: String,
    lines: Vec<DiffLine>,
    hunks: Vec<HunkHeader>,
    /// Byte ranges, relative to their line's text, of changed words.
    words: Vec<(u32, u32)>,
}

impl TextDiff {
    /// Number of lines across all hunks.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether there are no changes.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Line `i`.
    pub fn line(&self, i: usize) -> &DiffLine {
        &self.lines[i]
    }

    /// Text of line `i`, without its newline.
    pub fn text(&self, i: usize) -> &str {
        let (a, b) = self.lines[i].text;
        &self.text[a as usize..b as usize]
    }

    /// Byte ranges of changed words in line `i` (empty for context lines and lines whose
    /// hunk is all additions or all removals).
    pub fn word_ranges(&self, i: usize) -> impl Iterator<Item = Range<usize>> + '_ {
        let (a, b) = self.lines[i].words;
        self.words[a as usize..b as usize]
            .iter()
            .map(|(s, e)| *s as usize..*e as usize)
    }

    /// Hunks, in order.
    pub fn hunks(&self) -> &[HunkHeader] {
        &self.hunks
    }

    /// Lines added and removed.
    pub fn stats(&self) -> (usize, usize) {
        let added = self
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Added)
            .count();
        let removed = self
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Removed)
            .count();
        (added, removed)
    }

    fn push_line(&mut self, kind: LineKind, text: &str, old_no: Option<u32>, new_no: Option<u32>) {
        let (body, no_eol) = match text.strip_suffix('\n') {
            Some(b) => (b.strip_suffix('\r').unwrap_or(b), false),
            None => (text, true),
        };
        let start = self.text.len() as u32;
        self.text.push_str(body);
        let w = self.words.len() as u32;
        self.lines.push(DiffLine {
            kind,
            old_no,
            new_no,
            no_eol,
            text: (start, self.text.len() as u32),
            words: (w, w),
        });
    }

    /// The unified diff text of all hunks (no file header), as `git diff` prints it.
    pub fn to_unified(&self) -> String {
        let mut out = String::new();
        for h in &self.hunks {
            out.push_str(&format!(
                "@@ -{} +{} @@",
                range_spec(h.old_start, h.old_lines),
                range_spec(h.new_start, h.new_lines)
            ));
            if !h.section.is_empty() {
                out.push(' ');
                out.push_str(&h.section);
            }
            out.push('\n');
            for i in h.lines.clone() {
                out.push(self.lines[i].kind.prefix());
                out.push_str(self.text(i));
                out.push('\n');
                if self.lines[i].no_eol {
                    out.push_str("\\ No newline at end of file\n");
                }
            }
        }
        out
    }
}

/// `start,count`, or just `start` when the count is 1 (git's form).
pub(crate) fn range_spec(start: u32, count: u32) -> String {
    if count == 1 {
        start.to_string()
    } else {
        format!("{start},{count}")
    }
}

/// Whitespace handling for [`diff_text`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Whitespace {
    /// Compare lines exactly.
    #[default]
    Exact,
    /// Ignore changes in the amount of whitespace and at line ends (`-b`).
    IgnoreChanges,
    /// Ignore all whitespace (`-w`).
    IgnoreAll,
}

/// Options for text diffs.
#[derive(Clone, Copy, Debug)]
pub struct DiffOptions {
    /// Unchanged lines shown around each change.
    pub context: u32,
    /// Whitespace handling.
    pub whitespace: Whitespace,
    /// Compute word highlights.
    pub words: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            context: 3,
            whitespace: Whitespace::Exact,
            words: true,
        }
    }
}

fn normalize(line: &str, ws: Whitespace) -> String {
    match ws {
        Whitespace::Exact => line.to_owned(),
        Whitespace::IgnoreAll => line.chars().filter(|c| !c.is_whitespace()).collect(),
        Whitespace::IgnoreChanges => {
            let mut out = String::with_capacity(line.len());
            let mut space = false;
            for c in line.trim_end().chars() {
                if c.is_whitespace() {
                    space = true;
                } else {
                    if space {
                        out.push(' ');
                    }
                    space = false;
                    out.push(c);
                }
            }
            out
        }
    }
}

/// Split into lines, each keeping its newline.
fn split_lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

/// Diff two texts line by line.
pub fn diff_text(old: &str, new: &str, opts: &DiffOptions) -> TextDiff {
    let a = split_lines(old);
    let b = split_lines(new);
    let mut diff = if opts.whitespace == Whitespace::Exact {
        let input = InternedInput::new(old, new);
        let mut d = Diff::compute(Algorithm::Histogram, &input);
        d.postprocess_lines(&input);
        d
    } else {
        let na: Vec<String> = a.iter().map(|l| normalize(l, opts.whitespace)).collect();
        let nb: Vec<String> = b.iter().map(|l| normalize(l, opts.whitespace)).collect();
        let mut input = InternedInput::default();
        input.update_before(na.iter().map(String::as_str));
        input.update_after(nb.iter().map(String::as_str));
        let mut d = Diff::compute(Algorithm::Histogram, &input);
        d.postprocess_no_heuristic(&input);
        d
    };
    // Hunks as (before, after) ranges of line indices.
    let changes: Vec<(Range<u32>, Range<u32>)> =
        diff.hunks().map(|h| (h.before, h.after)).collect();
    let mut out = TextDiff::default();
    if changes.is_empty() {
        return out;
    }
    let ctx = opts.context;
    let mut word_input: InternedInput<&str> = InternedInput::default();
    let mut i = 0;
    while i < changes.len() {
        // Group changes whose context overlaps into one hunk.
        let mut j = i;
        while j + 1 < changes.len()
            && changes[j + 1].0.start.saturating_sub(changes[j].0.end) <= 2 * ctx
        {
            j += 1;
        }
        let old_from = changes[i].0.start.saturating_sub(ctx);
        let old_to = (changes[j].0.end + ctx).min(a.len() as u32);
        // The new side is offset by the same amount as the first change.
        let new_from = changes[i].1.start - (changes[i].0.start - old_from);
        let first_line = out.lines.len();
        let (mut o, mut n) = (old_from, new_from);
        for (before, after) in &changes[i..=j] {
            while o < before.start {
                out.push_line(LineKind::Context, a[o as usize], Some(o + 1), Some(n + 1));
                o += 1;
                n += 1;
            }
            let removed_at = out.lines.len();
            for x in before.clone() {
                out.push_line(LineKind::Removed, a[x as usize], Some(x + 1), None);
            }
            let added_at = out.lines.len();
            for y in after.clone() {
                out.push_line(LineKind::Added, b[y as usize], None, Some(y + 1));
            }
            if opts.words && !before.is_empty() && !after.is_empty() {
                word_highlights(
                    &mut out,
                    &mut word_input,
                    &mut diff,
                    (&a[before.start as usize..before.end as usize], removed_at),
                    (&b[after.start as usize..after.end as usize], added_at),
                );
            }
            o = before.end;
            n = after.end;
        }
        while o < old_to {
            out.push_line(LineKind::Context, a[o as usize], Some(o + 1), Some(n + 1));
            o += 1;
            n += 1;
        }
        let old_lines = o - old_from;
        let new_lines = n - new_from;
        out.hunks.push(HunkHeader {
            old_start: if old_lines == 0 {
                old_from
            } else {
                old_from + 1
            },
            old_lines,
            new_start: if new_lines == 0 {
                new_from
            } else {
                new_from + 1
            },
            new_lines,
            section: String::new(),
            lines: first_line..out.lines.len(),
        });
        i = j + 1;
    }
    out
}

/// Word-diff one change: removed lines `old` (pushed at line `old_at`) against added lines
/// `new` (pushed at `new_at`), recording changed words on each line.
fn word_highlights<'a>(
    out: &mut TextDiff,
    input: &mut InternedInput<&'a str>,
    diff: &mut Diff,
    (old, old_at): (&[&'a str], usize),
    (new, new_at): (&[&'a str], usize),
) {
    use gix::diff::blob::sources::words;

    input.clear();
    input.update_before(old.iter().flat_map(|l| words(l)));
    input.update_after(new.iter().flat_map(|l| words(l)));
    diff.compute_with(
        Algorithm::Myers,
        &input.before,
        &input.after,
        input.interner.num_tokens(),
    );
    diff.postprocess_no_heuristic(input);
    let mut ranges: Vec<(usize, Range<usize>)> = Vec::new();
    for (side, lines, at) in [(0u8, old, old_at), (1u8, new, new_at)] {
        let mut token = 0u32;
        for (li, line) in lines.iter().enumerate() {
            let body_len = line.trim_end_matches(['\n', '\r']).len();
            let base = line.as_ptr() as usize;
            for w in words(line) {
                let changed = if side == 0 {
                    diff.is_removed(token)
                } else {
                    diff.is_added(token)
                };
                token += 1;
                if !changed {
                    continue;
                }
                let start = w.as_ptr() as usize - base;
                let end = (start + w.len()).min(body_len);
                if start >= end {
                    continue;
                }
                // Merge with the previous range when adjacent.
                match ranges.last_mut() {
                    Some((l, r)) if *l == at + li && r.end == start => r.end = end,
                    _ => ranges.push((at + li, start..end)),
                }
            }
        }
    }
    // Each line's ranges must be contiguous in the arena: attach them in line order.
    ranges.sort_by_key(|(l, r)| (*l, r.start));
    for (line, range) in ranges {
        let w = out.words.len() as u32;
        out.words.push((range.start as u32, range.end as u32));
        let l = &mut out.lines[line];
        if l.words.0 == l.words.1 {
            l.words = (w, w + 1);
        } else {
            l.words.1 = w + 1;
        }
    }
}

/// What a file's content is, for choosing a view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileDiff {
    /// A text diff.
    Text(TextDiff),
    /// Binary content (a NUL byte in the first 8,000 bytes, like git).
    Binary {
        /// Old size in bytes (`None`: no old side).
        old_size: Option<u64>,
        /// New size in bytes.
        new_size: Option<u64>,
    },
    /// Too large to diff as text.
    TooLarge {
        /// Old size in bytes.
        old_size: Option<u64>,
        /// New size in bytes.
        new_size: Option<u64>,
    },
}

/// Whether `data` looks binary to git (a NUL in its first 8,000 bytes).
pub fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(8000)].contains(&0)
}

/// Image formats the image diff view can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF.
    Gif,
    /// WebP.
    Webp,
    /// BMP.
    Bmp,
    /// ICO.
    Ico,
    /// SVG.
    Svg,
    /// TIFF.
    Tiff,
}

/// The image kind of a path, by extension.
pub fn image_kind(path: &Path) -> Option<ImageKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => ImageKind::Png,
        "jpg" | "jpeg" => ImageKind::Jpeg,
        "gif" => ImageKind::Gif,
        "webp" => ImageKind::Webp,
        "bmp" => ImageKind::Bmp,
        "ico" => ImageKind::Ico,
        "svg" => ImageKind::Svg,
        "tif" | "tiff" => ImageKind::Tiff,
        _ => return None,
    })
}

/// Diff two byte contents: binary and size checks, then a text diff.
pub fn diff_bytes(old: Option<&[u8]>, new: Option<&[u8]>, opts: &DiffOptions) -> FileDiff {
    let size = |d: Option<&[u8]>| d.map(|d| d.len() as u64);
    let (old_size, new_size) = (size(old), size(new));
    let (o, n) = (old.unwrap_or_default(), new.unwrap_or_default());
    if is_binary(o) || is_binary(n) {
        return FileDiff::Binary { old_size, new_size };
    }
    if o.len() > MAX_TEXT_BYTES || n.len() > MAX_TEXT_BYTES {
        return FileDiff::TooLarge { old_size, new_size };
    }
    let (o, n) = (String::from_utf8_lossy(o), String::from_utf8_lossy(n));
    FileDiff::Text(diff_text(&o, &n, opts))
}

/// Read a blob's bytes.
pub fn read_blob(repo: &Repo, id: Oid) -> Result<Vec<u8>> {
    let r = repo.local();
    let blob = r.find_blob(id).gix()?;
    Ok(blob.data.clone())
}

/// Where [`read_at`] reads a file from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The commit HEAD points at.
    Head,
    /// The index (staged content).
    Index,
}

/// The bytes of `path` in HEAD or the index (`None` when it is not there, or HEAD is
/// unborn). Work-tree content is read from disk by the caller.
pub fn read_at(repo: &Repo, path: &Path, source: Source) -> Result<Option<Vec<u8>>> {
    let r = repo.local();
    let rela = gix::path::to_unix_separators_on_windows(gix::path::into_bstr(path).gix()?);
    let id = match source {
        Source::Head => {
            let Ok(commit) = r.head_commit() else {
                return Ok(None);
            };
            let tree = commit.tree().gix()?;
            match tree.lookup_entry_by_path(path).gix()? {
                Some(e) if e.mode().is_blob() => e.object_id(),
                _ => return Ok(None),
            }
        }
        Source::Index => {
            let index = r.index_or_empty().gix()?;
            match index.entry_by_path(rela.as_ref()) {
                Some(e) => e.id,
                None => return Ok(None),
            }
        }
    };
    read_blob(repo, id).map(Some)
}

/// Diff two blobs (either may be absent, for added and deleted files).
pub fn diff_blobs(
    repo: &Repo,
    old: Option<Oid>,
    new: Option<Oid>,
    opts: &DiffOptions,
) -> Result<FileDiff> {
    let started = std::time::Instant::now();
    let old = old.map(|id| read_blob(repo, id)).transpose()?;
    let new = new.map(|id| read_blob(repo, id)).transpose()?;
    let d = diff_bytes(old.as_deref(), new.as_deref(), opts);
    tracing::debug!(ms = started.elapsed().as_millis() as u64, "blob diff");
    Ok(d)
}

/// One file of `git diff` output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePatch {
    /// Path before (`None` for a new file).
    pub old_path: Option<PathBuf>,
    /// Path after (`None` for a deleted file).
    pub new_path: Option<PathBuf>,
    /// Git said "Binary files ... differ".
    pub binary: bool,
    /// Mode lines (`old mode`, `new file mode`, ...), as printed.
    pub mode_lines: Vec<String>,
    /// The hunks.
    pub diff: TextDiff,
}

impl FilePatch {
    /// The path to show: new, or old for a deletion.
    pub fn path(&self) -> Option<&Path> {
        self.new_path.as_deref().or(self.old_path.as_deref())
    }
}

/// Run `git diff` (work tree against the index) for `paths` and parse it.
pub fn worktree_diff(repo: &Repo, paths: &[&Path], context: u32) -> Result<Vec<FilePatch>> {
    git_diff(repo, false, paths, context)
}

/// Run `git diff --cached` (index against HEAD) for `paths` and parse it.
pub fn index_diff(repo: &Repo, paths: &[&Path], context: u32) -> Result<Vec<FilePatch>> {
    git_diff(repo, true, paths, context)
}

fn git_diff(repo: &Repo, cached: bool, paths: &[&Path], context: u32) -> Result<Vec<FilePatch>> {
    let unified = format!("-U{context}");
    let mut args: Vec<std::ffi::OsString> = vec![
        "diff".into(),
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "-M".into(),
        unified.into(),
    ];
    if cached {
        args.push("--cached".into());
    }
    args.push("--".into());
    args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
    let out = repo.git()?.run(&args)?;
    Ok(parse_unified(&out.stdout))
}

/// Parse `@@ -a,b +c,d @@ section`.
fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32, String)> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, section) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let parse = |s: &str| -> Option<(u32, u32)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (os, ol) = parse(old)?;
    let (ns, nl) = parse(new)?;
    Some((os, ol, ns, nl, section.trim_start().to_owned()))
}

/// Strip `a/` or `b/` and git's C-style quoting from a header path.
fn header_path(s: &str) -> Option<PathBuf> {
    let s = s.trim_end_matches('\t');
    if s == "/dev/null" {
        return None;
    }
    let unquoted = if s.starts_with('"') {
        unquote(s)
    } else {
        s.as_bytes().to_vec()
    };
    let bytes = unquoted
        .strip_prefix(b"a/")
        .or_else(|| unquoted.strip_prefix(b"b/"))
        .unwrap_or(&unquoted);
    Some(path_from_bytes(bytes))
}

/// Undo git's C-style path quoting (`"a\tb\303\251"`).
fn unquote(s: &str) -> Vec<u8> {
    let inner = s.trim_matches('"').as_bytes();
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] != b'\\' || i + 1 >= inner.len() {
            out.push(inner[i]);
            i += 1;
            continue;
        }
        let c = inner[i + 1];
        i += 2;
        match c {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = u32::from(c - b'0');
                let mut n = 1;
                while n < 3 && i < inner.len() && (b'0'..=b'7').contains(&inner[i]) {
                    v = v * 8 + u32::from(inner[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push(v as u8);
            }
            other => out.push(other),
        }
    }
    out
}

/// Parse unified diff output (`git diff`, `git diff --cached`, `git show`) into files.
pub fn parse_unified(out: &[u8]) -> Vec<FilePatch> {
    let text = String::from_utf8_lossy(out);
    let mut files: Vec<FilePatch> = Vec::new();
    // Remaining old / new lines in the current hunk, and the next line numbers.
    let (mut old_left, mut new_left, mut old_no, mut new_no) = (0u32, 0u32, 0u32, 0u32);
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches('\n');
        if bare.starts_with("\\ ") {
            if let Some(l) = files.last_mut().and_then(|f| f.diff.lines.last_mut()) {
                l.no_eol = true;
            }
            continue;
        }
        let in_hunk = old_left > 0 || new_left > 0;
        if in_hunk && let Some(f) = files.last_mut() {
            let (kind, body) = match bare.as_bytes().first() {
                Some(b'+') => (LineKind::Added, &line[1..]),
                Some(b'-') => (LineKind::Removed, &line[1..]),
                Some(b' ') => (LineKind::Context, &line[1..]),
                // Some tools drop the space of empty context lines.
                None => (LineKind::Context, line),
                _ => {
                    old_left = 0;
                    new_left = 0;
                    continue;
                }
            };
            let (o, n) = match kind {
                LineKind::Context => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                    old_no += 1;
                    new_no += 1;
                    (Some(old_no - 1), Some(new_no - 1))
                }
                LineKind::Removed => {
                    old_left = old_left.saturating_sub(1);
                    old_no += 1;
                    (Some(old_no - 1), None)
                }
                LineKind::Added => {
                    new_left = new_left.saturating_sub(1);
                    new_no += 1;
                    (None, Some(new_no - 1))
                }
            };
            f.diff.push_line(kind, body, o, n);
            // The line keeps its newline until a "\ No newline" marker says otherwise.
            if let Some(l) = f.diff.lines.last_mut() {
                l.no_eol = false;
            }
            if let Some(h) = f.diff.hunks.last_mut() {
                h.lines.end = f.diff.lines.len();
            }
            continue;
        }
        if let Some(rest) = bare.strip_prefix("diff --git ") {
            // Paths come from the ---/+++ or rename lines; this is a fallback for
            // mode-only and binary changes.
            let mut f = FilePatch::default();
            if let Some((a, b)) = rest.split_once(" b/") {
                f.old_path = header_path(a);
                f.new_path = header_path(&format!("b/{b}"));
            }
            files.push(f);
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if let Some(p) = bare.strip_prefix("--- ") {
            f.old_path = header_path(p);
        } else if let Some(p) = bare.strip_prefix("+++ ") {
            f.new_path = header_path(p);
        } else if let Some(p) = bare.strip_prefix("rename from ") {
            f.old_path = header_path(p);
        } else if let Some(p) = bare.strip_prefix("rename to ") {
            f.new_path = header_path(p);
        } else if bare.starts_with("Binary files ") || bare == "GIT binary patch" {
            f.binary = true;
        } else if bare.starts_with("new file mode") {
            f.old_path = None;
            f.mode_lines.push(bare.to_owned());
        } else if bare.starts_with("deleted file mode") {
            f.new_path = None;
            f.mode_lines.push(bare.to_owned());
        } else if bare.starts_with("old mode") || bare.starts_with("new mode") {
            f.mode_lines.push(bare.to_owned());
        } else if let Some((os, ol, ns, nl, section)) = parse_hunk_header(bare) {
            let at = f.diff.lines.len();
            f.diff.hunks.push(HunkHeader {
                old_start: os,
                old_lines: ol,
                new_start: ns,
                new_lines: nl,
                section,
                lines: at..at,
            });
            old_left = ol;
            new_left = nl;
            old_no = os.max(1);
            new_no = ns.max(1);
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    fn render(d: &TextDiff) -> Vec<String> {
        (0..d.len())
            .map(|i| format!("{}{}", d.line(i).kind.prefix(), d.text(i)))
            .collect()
    }

    #[test]
    fn diffs_text_with_context_and_numbers() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\n";
        let d = diff_text(old, new, &DiffOptions::default());
        assert_eq!(d.hunks().len(), 2);
        assert_eq!(
            render(&d)[..6],
            [" a", "-b", "+B", " c", " d", " e"].map(String::from)
        );
        let h = &d.hunks()[0];
        assert_eq!(
            (h.old_start, h.old_lines, h.new_start, h.new_lines),
            (1, 5, 1, 5)
        );
        let h = &d.hunks()[1];
        assert_eq!(
            (h.old_start, h.old_lines, h.new_start, h.new_lines),
            (10, 3, 10, 4)
        );
        assert_eq!(d.stats(), (2, 1));
        // Matches git's own output.
        assert_eq!(
            d.to_unified(),
            "@@ -1,5 +1,5 @@\n a\n-b\n+B\n c\n d\n e\n@@ -10,3 +10,4 @@\n j\n k\n l\n+m\n"
        );
        assert_eq!(d.line(0).old_no, Some(1));
        assert_eq!(d.line(2).new_no, Some(2));
    }

    #[test]
    fn close_changes_share_a_hunk() {
        let d = diff_text(
            "1\n2\n3\n4\n5\n",
            "x\n2\n3\n4\ny\n",
            &DiffOptions::default(),
        );
        assert_eq!(d.hunks().len(), 1);
        assert_eq!(
            render(&d),
            ["-1", "+x", " 2", " 3", " 4", "-5", "+y"].map(String::from)
        );
    }

    #[test]
    fn new_deleted_and_no_newline() {
        let d = diff_text("", "a\nb", &DiffOptions::default());
        let h = &d.hunks()[0];
        assert_eq!(
            (h.old_start, h.old_lines, h.new_start, h.new_lines),
            (0, 0, 1, 2)
        );
        assert!(d.line(1).no_eol);
        assert_eq!(
            d.to_unified(),
            "@@ -0,0 +1,2 @@\n+a\n+b\n\\ No newline at end of file\n"
        );
        let d = diff_text("a\n", "", &DiffOptions::default());
        assert_eq!(d.to_unified(), "@@ -1 +0,0 @@\n-a\n");
        assert!(diff_text("same\n", "same\n", &DiffOptions::default()).is_empty());
    }

    #[test]
    fn word_highlights_mark_changed_words() {
        let d = diff_text(
            "let x = foo(1);\n",
            "let x = bar(1);\n",
            &DiffOptions::default(),
        );
        let words = |i: usize| -> Vec<&str> { d.word_ranges(i).map(|r| &d.text(i)[r]).collect() };
        assert_eq!(words(0), ["foo"]);
        assert_eq!(words(1), ["bar"]);
        let off = diff_text(
            "a b\n",
            "a c\n",
            &DiffOptions {
                words: false,
                ..Default::default()
            },
        );
        assert_eq!(off.word_ranges(0).count(), 0);
    }

    #[test]
    fn ignores_whitespace_on_request() {
        let old = "fn a() {\n  x;\n}\n";
        let new = "fn a() {\n    x;  \n}\n";
        assert_eq!(diff_text(old, new, &DiffOptions::default()).stats(), (1, 1));
        for ws in [Whitespace::IgnoreAll, Whitespace::IgnoreChanges] {
            let d = diff_text(
                old,
                new,
                &DiffOptions {
                    whitespace: ws,
                    ..Default::default()
                },
            );
            assert!(d.is_empty(), "{ws:?}");
        }
        let d = diff_text(
            "a b\n",
            "ab\n",
            &DiffOptions {
                whitespace: Whitespace::IgnoreChanges,
                ..Default::default()
            },
        );
        assert_eq!(d.stats(), (1, 1));
    }

    #[test]
    fn binary_large_and_images() {
        assert!(matches!(
            diff_bytes(Some(b"a\0b"), Some(b"c"), &DiffOptions::default()),
            FileDiff::Binary {
                old_size: Some(3),
                new_size: Some(1)
            }
        ));
        let big = vec![b'a'; MAX_TEXT_BYTES + 1];
        assert!(matches!(
            diff_bytes(None, Some(&big), &DiffOptions::default()),
            FileDiff::TooLarge { old_size: None, .. }
        ));
        assert_eq!(image_kind(Path::new("x/Logo.PNG")), Some(ImageKind::Png));
        assert_eq!(image_kind(Path::new("a.rs")), None);
    }

    #[test]
    fn commit_files_with_renames() {
        let t = TestRepo::new();
        let body = "line one\nline two\nline three\nline four\nline five\n";
        t.commit_file("keep.txt", "k\n", "one");
        t.commit_file("dir/old.txt", body, "two");
        t.commit_file("gone.txt", "g\n", "three");
        t.git(&["mv", "dir/old.txt", "dir/new.txt"]);
        t.git(&["rm", "-q", "gone.txt"]);
        t.write("keep.txt", "k2\n");
        t.write("added.txt", "new\n");
        t.git(&["add", "-A"]);
        t.git(&["commit", "-q", "-m", "changes"]);
        let repo = Repo::open(t.path()).unwrap();
        let head = repo.resolve("HEAD").unwrap();
        let files = commit_files(&repo, head).unwrap();
        let got: Vec<(String, FileStatus, Option<String>)> = files
            .iter()
            .map(|f| {
                (
                    f.path.to_string_lossy().replace('\\', "/"),
                    f.status,
                    f.old_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().replace('\\', "/")),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("added.txt".into(), FileStatus::Added, None),
                (
                    "dir/new.txt".into(),
                    FileStatus::Renamed,
                    Some("dir/old.txt".into())
                ),
                ("gone.txt".into(), FileStatus::Deleted, None),
                ("keep.txt".into(), FileStatus::Modified, None),
            ]
        );
        let keep = files.iter().find(|f| f.path.ends_with("keep.txt")).unwrap();
        let FileDiff::Text(d) =
            diff_blobs(&repo, keep.old_id, keep.new_id, &DiffOptions::default()).unwrap()
        else {
            panic!("not text")
        };
        assert_eq!(render(&d), ["-k", "+k2"].map(String::from));

        // Root commit: everything added.
        let root = repo.resolve("HEAD~3").unwrap();
        let files = commit_files(&repo, root).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Added);
    }

    #[test]
    fn parses_git_diff_output() {
        let t = TestRepo::new();
        t.commit_file("a.txt", "1\n2\n3\n", "one");
        t.commit_file("sp ace.txt", "x\n", "two");
        t.commit_file("no-eol.txt", "end", "three");
        t.write("a.txt", "1\nTWO\n3\n4\n");
        t.write("sp ace.txt", "y\n");
        t.write("no-eol.txt", "end2");
        let repo = Repo::open(t.path()).unwrap();
        let files = worktree_diff(&repo, &[], 3).unwrap();
        assert_eq!(files.len(), 3);
        let a = &files[0];
        assert_eq!(a.path(), Some(Path::new("a.txt")));
        assert_eq!(
            render(&a.diff),
            [" 1", "-2", "+TWO", " 3", "+4"].map(String::from)
        );
        let h = &a.diff.hunks()[0];
        assert_eq!(
            (h.old_start, h.old_lines, h.new_start, h.new_lines),
            (1, 3, 1, 4)
        );
        assert_eq!(a.diff.line(4).new_no, Some(4));
        let eol = &files[1];
        assert_eq!(eol.path(), Some(Path::new("no-eol.txt")));
        assert!(eol.diff.line(0).no_eol && eol.diff.line(1).no_eol);
        assert_eq!(files[2].path(), Some(Path::new("sp ace.txt")));
        // Round trip: our unified text equals git's hunks.
        let git = t.git(&["diff", "--", "a.txt"]);
        let hunks = &git[git.find("@@").unwrap()..];
        assert_eq!(a.diff.to_unified(), hunks);

        t.git(&["add", "a.txt"]);
        assert_eq!(
            index_diff(&repo, &[Path::new("a.txt")], 3).unwrap().len(),
            1
        );
    }

    #[test]
    fn parses_headers_new_deleted_binary_and_quoted() {
        let out = b"diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..1\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hi\n\
diff --git a/old.txt b/old.txt\ndeleted file mode 100644\n--- a/old.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n\
diff --git a/img.png b/img.png\nindex 1..2 100644\nBinary files a/img.png and b/img.png differ\n\
diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"\n--- \"a/caf\\303\\251.txt\"\n+++ \"b/caf\\303\\251.txt\"\n@@ -1 +1 @@ fn main\n-a\n+b\n";
        let files = parse_unified(out);
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].old_path, None);
        assert_eq!(files[0].new_path, Some(PathBuf::from("new.txt")));
        assert_eq!(files[0].mode_lines, ["new file mode 100644"]);
        assert_eq!(files[1].new_path, None);
        assert_eq!(files[1].diff.stats(), (0, 1));
        assert!(files[2].binary && files[2].diff.is_empty());
        assert_eq!(files[3].path(), Some(Path::new("café.txt")));
        assert_eq!(files[3].diff.hunks()[0].section, "fn main");
    }
}
