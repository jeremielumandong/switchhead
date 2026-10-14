//! What the diff view lists, computed off the UI thread: rows for the inline and
//! side-by-side layouts, and syntax highlights per diff line.
//!
//! The view itself only paints rows. Rows index into the [`TextDiff`]; nothing here copies
//! line text.

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::HighlightStyle;
use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
use switchhead_git::diff::{LineKind, TextDiff};

/// Files larger than this are shown without syntax colors.
const MAX_HIGHLIGHT_BYTES: usize = 1024 * 1024;

/// One row of the inline layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// A hunk's `@@` header (index into [`TextDiff::hunks`]).
    Hunk(u32),
    /// A diff line (index into [`TextDiff::line`]).
    Line(u32),
}

/// One row of the side-by-side layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SbsRow {
    /// A hunk's `@@` header.
    Hunk(u32),
    /// The old side's line and the new side's line (context lines appear on both).
    Pair(Option<u32>, Option<u32>),
}

/// Inline rows: each hunk's header, then its lines.
pub fn inline_rows(d: &TextDiff) -> Vec<Row> {
    let mut rows = Vec::with_capacity(d.len() + d.hunks().len());
    for (h, hunk) in d.hunks().iter().enumerate() {
        rows.push(Row::Hunk(h as u32));
        rows.extend(hunk.lines.clone().map(|i| Row::Line(i as u32)));
    }
    rows
}

/// Side-by-side rows: context on both sides; a run of removed lines is paired line by
/// line with the added lines that follow it.
pub fn sbs_rows(d: &TextDiff) -> Vec<SbsRow> {
    let mut rows = Vec::with_capacity(d.len() + d.hunks().len());
    for (h, hunk) in d.hunks().iter().enumerate() {
        rows.push(SbsRow::Hunk(h as u32));
        let mut i = hunk.lines.start;
        while i < hunk.lines.end {
            if d.line(i).kind == LineKind::Context {
                rows.push(SbsRow::Pair(Some(i as u32), Some(i as u32)));
                i += 1;
                continue;
            }
            let removed_start = i;
            while i < hunk.lines.end && d.line(i).kind == LineKind::Removed {
                i += 1;
            }
            let added_start = i;
            while i < hunk.lines.end && d.line(i).kind == LineKind::Added {
                i += 1;
            }
            let (removed, added) = (added_start - removed_start, i - added_start);
            for k in 0..removed.max(added) {
                rows.push(SbsRow::Pair(
                    (k < removed).then_some((removed_start + k) as u32),
                    (k < added).then_some((added_start + k) as u32),
                ));
            }
        }
    }
    rows
}

/// The inline row holding the longest line (for sizing a horizontally scrolling list).
pub fn widest_row(d: &TextDiff, rows: &[Row]) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(r, row)| match row {
            Row::Line(i) => Some((r, d.text(*i as usize).chars().count())),
            Row::Hunk(_) => None,
        })
        .max_by_key(|(_, n)| *n)
        .map(|(r, _)| r)
}

/// Syntax highlights for every line of a diff, in one arena.
#[derive(Clone, Debug, Default)]
pub struct Syntax {
    /// `spans[offsets[i]..offsets[i + 1]]` are line `i`'s spans.
    offsets: Vec<u32>,
    /// Byte ranges relative to the line's text, sorted, not overlapping.
    spans: Vec<(Range<usize>, HighlightStyle)>,
}

impl Syntax {
    /// Spans of diff line `i` (empty when the line has none).
    pub fn line(&self, i: usize) -> &[(Range<usize>, HighlightStyle)] {
        match (self.offsets.get(i), self.offsets.get(i + 1)) {
            (Some(&a), Some(&b)) => &self.spans[a as usize..b as usize],
            _ => &[],
        }
    }
}

/// Byte offset where each line of `text` starts, plus the end.
fn line_starts(text: &str) -> Vec<usize> {
    let mut out = vec![0];
    out.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    out.push(text.len());
    out
}

/// Highlight `old` and `new` (the whole files) as `lang`, then cut the result into the
/// diff's lines: removed lines take colors from the old file, the rest from the new one.
/// `None` when there is no grammar for `lang` or a file is too large.
pub fn highlight(
    d: &TextDiff,
    old: &str,
    new: &str,
    lang: &str,
    theme: &Arc<HighlightTheme>,
) -> Option<Syntax> {
    if old.len().max(new.len()) > MAX_HIGHLIGHT_BYTES {
        return None;
    }
    let parse = |text: &str| -> Option<SyntaxHighlighter> {
        let mut h = SyntaxHighlighter::new(lang);
        if h.language().as_ref() == "text" {
            return None;
        }
        h.update(None, &text.into(), None);
        Some(h)
    };
    let needs = |kind: LineKind| (0..d.len()).any(|i| d.line(i).kind == kind);
    let old_hl = if needs(LineKind::Removed) {
        Some(parse(old)?)
    } else {
        None
    };
    let new_hl = parse(new)?;
    let (old_starts, new_starts) = (line_starts(old), line_starts(new));
    let mut syntax = Syntax {
        offsets: Vec::with_capacity(d.len() + 1),
        spans: Vec::new(),
    };
    for i in 0..d.len() {
        syntax.offsets.push(syntax.spans.len() as u32);
        let line = d.line(i);
        let (hl, starts, no) = match line.kind {
            LineKind::Removed => (old_hl.as_ref(), &old_starts, line.old_no),
            _ => (Some(&new_hl), &new_starts, line.new_no),
        };
        let (Some(hl), Some(no)) = (hl, no) else {
            continue;
        };
        let Some(&start) = (no as usize).checked_sub(1).and_then(|n| starts.get(n)) else {
            continue;
        };
        let end = start + d.text(i).len();
        for (r, style) in hl.styles(&(start..end), &**theme) {
            if r.start < r.end && style != HighlightStyle::default() {
                syntax.spans.push((r.start - start..r.end - start, style));
            }
        }
    }
    syntax.offsets.push(syntax.spans.len() as u32);
    Some(syntax)
}

/// Combine a line's syntax spans with its changed-word ranges (`word` style on top) into
/// sorted, non-overlapping spans.
pub fn merge_styles(
    syntax: &[(Range<usize>, HighlightStyle)],
    words: &[Range<usize>],
    word: HighlightStyle,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if words.is_empty() {
        return syntax.to_vec();
    }
    let mut cuts: Vec<usize> = syntax
        .iter()
        .flat_map(|(r, _)| [r.start, r.end])
        .chain(words.iter().flat_map(|r| [r.start, r.end]))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let base = syntax
            .iter()
            .find(|(r, _)| r.start <= a && b <= r.end)
            .map(|(_, s)| *s);
        let in_word = words.iter().any(|r| r.start <= a && b <= r.end);
        let style = match (base, in_word) {
            (None, false) => continue,
            (Some(s), false) => s,
            (None, true) => word,
            (Some(s), true) => s.highlight(word),
        };
        match out.last_mut() {
            Some((r, s)) if r.end == a && *s == style => r.end = b,
            _ => out.push((a..b, style)),
        }
    }
    out
}

/// Columns a tab advances to.
const TAB: usize = 4;

/// Replace tabs with spaces up to the next tab stop, moving `spans` along.
pub fn expand_tabs(
    text: &str,
    spans: Vec<(Range<usize>, HighlightStyle)>,
) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    if !text.contains('\t') {
        return (text.to_owned(), spans);
    }
    let mut out = String::with_capacity(text.len() + 16);
    // New offset of every old byte offset (and the end).
    let mut map = Vec::with_capacity(text.len() + 1);
    let mut col = 0;
    for (i, c) in text.char_indices() {
        map.resize(i + 1, out.len());
        if c == '\t' {
            let n = TAB - col % TAB;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    map.resize(text.len() + 1, out.len());
    let spans = spans
        .into_iter()
        .map(|(r, s)| (map[r.start.min(text.len())]..map[r.end.min(text.len())], s))
        .collect();
    (out, spans)
}

/// The grammar for a path, by file name or extension.
pub fn language_for(path: &std::path::Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let by_name = match name.as_str() {
        "makefile" | "gnumakefile" => Some("make"),
        "cmakelists.txt" => Some("cmake"),
        "cargo.lock" => Some("toml"),
        ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" => Some("bash"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    let ext = name.rsplit_once('.')?.1;
    Some(match ext {
        "rs" => "rust",
        "go" => "go",
        "py" | "pyi" => "python",
        "rb" => "ruby",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" | "mdx" => "markdown",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "swift" => "swift",
        "lua" => "lua",
        "php" => "php",
        "sh" | "bash" | "zsh" => "bash",
        "sql" => "sql",
        "zig" => "zig",
        "ex" | "exs" => "elixir",
        "graphql" | "gql" => "graphql",
        "proto" => "proto",
        "svelte" => "svelte",
        "astro" => "astro",
        "erb" => "erb",
        "ejs" => "ejs",
        "cmake" => "cmake",
        "mk" => "make",
        "diff" | "patch" => "diff",
        _ => return None,
    })
}

/// What a minimap mark stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkKind {
    Added,
    Removed,
    /// Removed lines replaced by added ones.
    Changed,
}

/// A run of rows `start..end` of one kind, for the diff minimap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub start: u32,
    pub end: u32,
    pub kind: MarkKind,
}

/// Minimap marks for the inline layout: a removed run directly followed by an added
/// run is one changed mark.
pub fn inline_marks(d: &TextDiff, rows: &[Row]) -> Vec<Mark> {
    let kinds = rows.iter().map(|r| match *r {
        Row::Line(i) => match d.line(i as usize).kind {
            LineKind::Added => Some(MarkKind::Added),
            LineKind::Removed => Some(MarkKind::Removed),
            LineKind::Context => None,
        },
        Row::Hunk(_) => None,
    });
    let mut out: Vec<Mark> = Vec::new();
    for m in runs(kinds) {
        match out.last_mut() {
            Some(prev)
                if prev.kind == MarkKind::Removed
                    && m.kind == MarkKind::Added
                    && prev.end == m.start =>
            {
                prev.end = m.end;
                prev.kind = MarkKind::Changed;
            }
            _ => out.push(m),
        }
    }
    out
}

/// Minimap marks for the side-by-side layout: a row with a line on both sides that
/// differ is changed.
pub fn sbs_marks(d: &TextDiff, rows: &[SbsRow]) -> Vec<Mark> {
    let changed = |i: u32| d.line(i as usize).kind != LineKind::Context;
    runs(rows.iter().map(|r| match *r {
        SbsRow::Pair(Some(o), Some(n)) if changed(o) || changed(n) => Some(MarkKind::Changed),
        SbsRow::Pair(Some(o), None) if changed(o) => Some(MarkKind::Removed),
        SbsRow::Pair(None, Some(n)) if changed(n) => Some(MarkKind::Added),
        _ => None,
    }))
}

/// Runs of equal, present kinds.
fn runs(kinds: impl Iterator<Item = Option<MarkKind>>) -> Vec<Mark> {
    let mut out: Vec<Mark> = Vec::new();
    for (i, k) in kinds.enumerate() {
        let Some(kind) = k else { continue };
        let i = i as u32;
        match out.last_mut() {
            Some(m) if m.kind == kind && m.end == i => m.end = i + 1,
            _ => out.push(Mark {
                start: i,
                end: i + 1,
                kind,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchhead_git::diff::{DiffOptions, diff_text};

    fn diff(old: &str, new: &str) -> TextDiff {
        diff_text(old, new, &DiffOptions::default())
    }

    #[test]
    fn minimap_marks_runs_of_changes() {
        let d = diff("a\nb\nc\nd\ne\n", "a\nB\nB2\nc\ne\nf\n");
        // Lines: a, -b, +B, +B2, c, -d, e, +f
        let mark = |start, end, kind| Mark { start, end, kind };
        assert_eq!(
            inline_marks(&d, &inline_rows(&d)),
            vec![
                mark(2, 5, MarkKind::Changed),
                mark(6, 7, MarkKind::Removed),
                mark(8, 9, MarkKind::Added),
            ]
        );
        // Rows: @@, a, b|B, -|B2, c, d|-, e, -|f
        assert_eq!(
            sbs_marks(&d, &sbs_rows(&d)),
            vec![
                mark(2, 3, MarkKind::Changed),
                mark(3, 4, MarkKind::Added),
                mark(5, 6, MarkKind::Removed),
                mark(7, 8, MarkKind::Added),
            ]
        );
    }

    #[test]
    fn side_by_side_pairs_removals_with_additions() {
        let d = diff("a\nb\nc\nd\n", "a\nB\nB2\nc\n");
        // Lines: a, -b, +B, +B2, c, -d
        let rows = sbs_rows(&d);
        assert_eq!(rows[0], SbsRow::Hunk(0));
        assert_eq!(rows[1], SbsRow::Pair(Some(0), Some(0)));
        assert_eq!(rows[2], SbsRow::Pair(Some(1), Some(2)));
        assert_eq!(rows[3], SbsRow::Pair(None, Some(3)));
        assert_eq!(rows[4], SbsRow::Pair(Some(4), Some(4)));
        assert_eq!(rows[5], SbsRow::Pair(Some(5), None));
        assert_eq!(rows.len(), 6);
        let inline = inline_rows(&d);
        assert_eq!(inline.len(), d.len() + 1);
        assert_eq!(inline[0], Row::Hunk(0));
        assert_eq!(widest_row(&d, &inline), Some(4), "+B2 is the longest");
    }

    #[test]
    fn merges_word_highlights_over_syntax() {
        let red = HighlightStyle {
            color: Some(gpui_kit::red()),
            ..Default::default()
        };
        let bg = HighlightStyle {
            background_color: Some(gpui_kit::blue()),
            ..Default::default()
        };
        let out = merge_styles(&[(0..4, red)], &[2..6, 6..6], bg);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], (0..2, red));
        assert_eq!(out[1].0, 2..4);
        assert_eq!(out[1].1.color, red.color);
        assert_eq!(out[1].1.background_color, bg.background_color);
        assert_eq!(out[2], (4..6, bg));
        assert_eq!(merge_styles(&[(0..4, red)], &[], bg), vec![(0..4, red)]);
    }

    #[test]
    fn highlights_lines_from_their_side() {
        let old = "fn main() {\n    let a = 1;\n}\n";
        let new = "fn main() {\n    let b = \"x\";\n}\n";
        let d = diff(old, new);
        let theme = HighlightTheme::default_dark();
        let s = highlight(&d, old, new, "rust", &theme).expect("rust grammar");
        for i in 0..d.len() {
            let text = d.text(i);
            for (r, _) in s.line(i) {
                assert!(r.end <= text.len(), "span inside line {i}");
            }
        }
        // `fn` on the first (context) line is a keyword.
        assert!(s.line(0).iter().any(|(r, _)| *r == (0..2)));
        assert!(highlight(&d, old, new, "no-such-language", &theme).is_none());
    }

    #[test]
    fn expands_tabs_and_moves_spans() {
        let s = HighlightStyle::default();
        let (t, spans) = expand_tabs("\tab\tc", vec![(1..3, s), (4..5, s)]);
        assert_eq!(t, "    ab  c");
        assert_eq!(spans[0].0, 4..6);
        assert_eq!(spans[1].0, 8..9);
        let (t, spans) = expand_tabs("plain", vec![(0..5, s)]);
        assert_eq!((t.as_str(), spans[0].0.clone()), ("plain", 0..5));
    }

    #[test]
    fn picks_languages() {
        use std::path::Path;
        assert_eq!(language_for(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(language_for(Path::new("Makefile")), Some("make"));
        assert_eq!(language_for(Path::new("a/B.TSX")), Some("tsx"));
        assert_eq!(language_for(Path::new("README")), None);
        assert_eq!(language_for(Path::new("x.unknown")), None);
    }
}
