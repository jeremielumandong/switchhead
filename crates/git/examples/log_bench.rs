//! Time the log walk and graph layout on a repository:
//! `cargo run --release -p switchhead-git --example log_bench -- <repo>`.

use switchhead_git::cli::Cancel;
use switchhead_git::log::{self, LogOptions};
use switchhead_git::{Repo, refs};

fn main() -> Result<(), switchhead_git::Error> {
    let path = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let repo = Repo::open(std::path::Path::new(&path))?;

    let started = std::time::Instant::now();
    let r = refs::load(&repo)?;
    println!(
        "refs: {} branches, {} remote branches, {} tags in {} ms",
        r.branches.len(),
        r.remotes.len(),
        r.tags.len(),
        started.elapsed().as_millis()
    );

    let mut width = 0;
    let stats = log::walk(&repo, &LogOptions::default(), &Cancel::default(), |b| {
        width = width.max(b.graph.max_width());
        true
    })?;
    println!(
        "log: {} commits, first batch {} ms, all {} ms, widest row {width} lanes",
        stats.commits, stats.first_batch_ms, stats.total_ms
    );
    Ok(())
}
