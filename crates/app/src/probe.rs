//! TEMPORARY (not for merge): with `SWITCHHEAD_PROBE=<file>`, delete the `zN` branches
//! one by one through `run_op` and record UI-thread stalls in `<file>`, then quit.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::{AsyncApp, Context, WeakEntity};

use crate::repo_view::RepoView;

fn log(line: &str) {
    if let Some(p) = std::env::var_os("SWITCHHEAD_PROBE")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Start the probe for `view` when `SWITCHHEAD_PROBE` is set.
pub fn start(cx: &mut Context<RepoView>) {
    if std::env::var_os("SWITCHHEAD_PROBE").is_none() {
        return;
    }
    let started = Instant::now();
    let beat = Arc::new(AtomicU64::new(0));
    let step = Arc::new(AtomicU64::new(0));
    {
        let (beat, step) = (beat.clone(), step.clone());
        std::thread::spawn(move || {
            let mut reported = 0;
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let now = started.elapsed().as_millis() as u64;
                let last = beat.load(Ordering::SeqCst);
                if now.saturating_sub(last) > 1000 && last != reported {
                    reported = last;
                    log(&format!(
                        "UI STALL: no heartbeat since {last} ms (now {now} ms), step {}",
                        step.load(Ordering::SeqCst)
                    ));
                }
                if now > 240_000 {
                    log("PROBE TIMEOUT");
                    std::process::exit(3);
                }
            }
        });
    }
    {
        let beat = beat.clone();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            loop {
                beat.store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
        })
        .detach();
    }
    cx.spawn(async move |this: WeakEntity<RepoView>, cx: &mut AsyncApp| {
        log("probe started");
        for i in 1..=30u64 {
            // Wait for the view to settle.
            let t = Instant::now();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let idle = this.update(cx, |v, _| v.probe_idle()).unwrap_or(true);
                if idle {
                    break;
                }
                if t.elapsed() > Duration::from_secs(30) {
                    log(&format!("step {i}: view never settled"));
                    break;
                }
            }
            step.store(i, Ordering::SeqCst);
            let name = format!("z{i}");
            log(&format!(
                "step {i}: delete {name} at {} ms",
                started.elapsed().as_millis()
            ));
            let _ = this.update(cx, |v, cx| v.probe_delete(name, cx));
        }
        cx.background_executor().timer(Duration::from_secs(3)).await;
        log(&format!(
            "probe done at {} ms",
            started.elapsed().as_millis()
        ));
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
}
