//! TEMPORARY: probes the app's branch-delete cycle on each CI OS. Not for merge.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::testutil::TestRepo;
use crate::{Repo, log, refs, status, watch};

fn timed<T: Send + 'static>(
    report: &mut Vec<String>,
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(v) => {
            report.push(format!("{name}: {} ms", started.elapsed().as_millis()));
            Some(v)
        }
        Err(_) => {
            report.push(format!("{name}: HUNG (>20 s)"));
            None
        }
    }
}

#[test]
fn branch_delete_cycle() {
    let t = TestRepo::new();
    let head = t.commit_file("a.txt", "a\n", "one");
    let head = head.trim().to_owned();
    // Enough packed refs that gix memory-maps packed-refs (> 32 KiB).
    let mut packed = String::from("# pack-refs with: peeled fully-peeled sorted \n");
    let mut names: Vec<String> = (0..1500).map(|i| format!("b{i:04}")).collect();
    names.sort();
    for n in &names {
        packed.push_str(&format!("{head} refs/heads/{n}\n"));
    }
    t.write(".git/packed-refs", &packed);
    let repo = Repo::open(t.path()).unwrap();
    let mut report = Vec::new();
    let (wtx, wrx) = mpsc::channel();
    let _watcher = watch::Watcher::start(&repo, move |c| {
        let _ = wtx.send(c);
    });
    let mut failed = false;
    for n in names.iter().take(6) {
        let r = repo.clone();
        if let Some(Ok(refs)) = timed(&mut report, "refs::load", move || refs::load(&r)) {
            report.push(format!("  branches: {}", refs.branches.len()));
        }
        let (r, n2) = (repo.clone(), n.clone());
        match timed(&mut report, &format!("delete {n}"), move || {
            r.delete_branch(&n2, false)
        }) {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                report.push(format!("  error: {e}"));
                failed = true;
            }
            None => failed = true,
        }
        let r = repo.clone();
        if timed(&mut report, "status", move || {
            status::status(&r, status::StatusOptions::default()).map(|_| ())
        })
        .is_none()
        {
            failed = true;
        }
        let r = repo.clone();
        timed(&mut report, "fingerprint+state", move || {
            (watch::fingerprint(&r), r.state())
        });
        let r = repo.clone();
        if timed(&mut report, "log", move || {
            log::collect(&r, &log::LogOptions::default()).map(|_| ())
        })
        .is_none()
        {
            failed = true;
        }
        let events = wrx.try_iter().count();
        report.push(format!("  watcher bursts: {events}"));
    }
    let text = report.join("\n");
    eprintln!("{text}");
    // Fail on Windows too, so CI shows the timings.
    assert!(!failed && !cfg!(windows), "branch delete cycle:\n{text}");
}
