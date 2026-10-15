//! Performance check for CI: time opening a repository to its first graph rows, the
//! whole log walk, and the per-frame row reads of scrolling the graph, and fail when a
//! budget is exceeded.
//!
//! `cargo run --release -p switchhead-git --example perf -- <repo> [options]`
//!
//! Options:
//!   --name <text>           label for the report (default: the folder name)
//!   --first-rows-ms <n>     budget for open + refs + first batch (best of the runs)
//!   --frame-us <n>          budget for one frame's row reads (99th percentile)
//!   --runs <n>              open-to-first-rows runs (default 5)
//!
//! Prints one Markdown table row per measure (CI appends them to the job summary) and
//! exits with status 1 when a budget is exceeded.

use std::hint::black_box;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use switchhead_git::cli::Cancel;
use switchhead_git::graph::GraphRows;
use switchhead_git::log::{self, CommitLog, LogOptions};
use switchhead_git::{Repo, refs};

/// Rows a frame shows (a tall window at the default zoom).
const VISIBLE_ROWS: usize = 60;
/// Rows a wheel or trackpad step moves.
const SCROLL_STEP: usize = 3;
/// Random jumps (scrollbar drags) after the full scroll.
const JUMPS: usize = 10_000;

struct Args {
    repo: String,
    name: Option<String>,
    first_rows_ms: Option<f64>,
    frame_us: Option<f64>,
    runs: usize,
}

fn args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        repo: String::new(),
        name: None,
        first_rows_ms: None,
        frame_us: None,
        runs: 5,
    };
    while let Some(arg) = it.next() {
        let mut value = |what: &str| it.next().ok_or(format!("{what} needs a value"));
        match arg.as_str() {
            "--name" => a.name = Some(value("--name")?),
            "--first-rows-ms" => a.first_rows_ms = Some(number(&value("--first-rows-ms")?)?),
            "--frame-us" => a.frame_us = Some(number(&value("--frame-us")?)?),
            "--runs" => a.runs = number(&value("--runs")?)?.max(1.0) as usize,
            _ if a.repo.is_empty() && !arg.starts_with("--") => a.repo = arg,
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if a.repo.is_empty() {
        return Err(
            "usage: perf <repo> [--name N] [--first-rows-ms N] [--frame-us N] [--runs N]".into(),
        );
    }
    Ok(a)
}

fn number(s: &str) -> Result<f64, String> {
    s.parse().map_err(|_| format!("not a number: {s}"))
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Open the repository, load its refs and walk to the first batch, as the app does when
/// a repository tab opens.
fn first_rows(path: &Path) -> Result<f64, switchhead_git::Error> {
    let start = Instant::now();
    let repo = Repo::open(path)?;
    black_box(refs::load(&repo)?);
    let mut rows = 0;
    log::walk(&repo, &LogOptions::default(), &Cancel::default(), |b| {
        rows = b.commits.len();
        false
    })?;
    black_box(rows);
    Ok(ms(start))
}

/// What painting one row reads: its id, summary, author, time and graph segments.
fn read_rows(log: &CommitLog, graph: &GraphRows, top: usize) -> usize {
    let end = (top + VISIBLE_ROWS).min(log.len());
    let mut sum = 0usize;
    for r in top..end {
        sum += log.id(r).as_bytes()[0] as usize;
        sum += log.summary(r).len();
        sum += log.author(r).name.len();
        sum += log.commit_time(r) as usize;
        let row = graph.row(r);
        sum += row.node.column as usize;
        for s in row.segments {
            sum += (s.from + s.to + s.color) as usize;
        }
    }
    sum
}

/// Per-frame row reads over a scroll from top to bottom and random jumps: (99th
/// percentile, worst) in microseconds.
fn scroll(log: &CommitLog, graph: &GraphRows) -> (f64, f64) {
    let last = log.len().saturating_sub(VISIBLE_ROWS);
    let mut tops: Vec<usize> = (0..=last).step_by(SCROLL_STEP).collect();
    // A fixed xorshift sequence: the same jumps on every run.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..JUMPS {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        tops.push((x % (last as u64 + 1)) as usize);
    }
    let mut frames: Vec<f64> = tops
        .iter()
        .map(|&top| {
            let start = Instant::now();
            black_box(read_rows(log, graph, top));
            start.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    frames.sort_by(f64::total_cmp);
    let p99 = frames[(frames.len() * 99 / 100).min(frames.len() - 1)];
    (p99, frames.last().copied().unwrap_or(0.0))
}

/// One report row; `false` when over budget.
fn report(repo: &str, measure: &str, value: f64, unit: &str, budget: Option<f64>) -> bool {
    let ok = budget.is_none_or(|b| value <= b);
    let budget_text = budget.map_or("–".to_owned(), |b| format!("{b} {unit}"));
    let verdict = match budget {
        None => "",
        Some(_) if ok => "✅",
        Some(_) => "❌ over budget",
    };
    println!("| {repo} | {measure} | {value:.1} {unit} | {budget_text} | {verdict} |");
    ok
}

fn main() -> ExitCode {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    match run(&a) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("perf: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(a: &Args) -> Result<bool, switchhead_git::Error> {
    let path = Path::new(&a.repo);
    let name = a.name.clone().unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| a.repo.clone())
    });
    let mut runs = Vec::with_capacity(a.runs);
    for _ in 0..a.runs {
        runs.push(first_rows(path)?);
    }
    runs.sort_by(f64::total_cmp);

    let repo = Repo::open(path)?;
    let start = Instant::now();
    let (log, graph) = log::collect(&repo, &LogOptions::default())?;
    let walk_ms = ms(start);
    let (p99, worst) = scroll(&log, &graph);

    let mut ok = true;
    ok &= report(
        &name,
        "open to first rows (best)",
        runs[0],
        "ms",
        a.first_rows_ms,
    );
    report(
        &name,
        "open to first rows (median)",
        runs[runs.len() / 2],
        "ms",
        None,
    );
    report(
        &name,
        &format!(
            "whole log ({} commits, {} lanes wide)",
            log.len(),
            graph.max_width()
        ),
        walk_ms,
        "ms",
        None,
    );
    ok &= report(
        &name,
        &format!("scroll frame, {VISIBLE_ROWS} rows (p99)"),
        p99,
        "µs",
        a.frame_us,
    );
    report(&name, "scroll frame (worst)", worst, "µs", None);
    Ok(ok)
}
