//! Crash logs: a panic writes a report (version, platform, thread, message, location and
//! backtrace) to `<data>/crashes/`, and the next start offers to show it. Reports stay on
//! this computer; nothing is sent anywhere.

use std::backtrace::Backtrace;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Reports kept; older ones are removed when a new one is written.
const KEEP: usize = 10;

/// File name prefix of a report (`crash-20261015-134501.txt`).
const PREFIX: &str = "crash-";

/// Write a report to `dir` on every panic, then run the hook that was installed before
/// (logging and the default message).
pub fn install(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let report = report(
            thread.name().unwrap_or("unnamed"),
            info.payload_as_str().unwrap_or("(no message)"),
            &info
                .location()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "unknown".into()),
            &Backtrace::force_capture().to_string(),
        );
        match write(&dir, &report) {
            Ok(path) => eprintln!("switchhead: crash report written to {}", path.display()),
            Err(e) => eprintln!("switchhead: could not write a crash report: {e}"),
        }
        previous(info);
    }));
}

/// The report's text.
fn report(thread: &str, message: &str, location: &str, backtrace: &str) -> String {
    format!(
        "switchhead {version} crashed\n\
         time: {time}\n\
         os: {os} ({arch})\n\
         thread: {thread}\n\
         at: {location}\n\
         \n{message}\n\nbacktrace:\n{backtrace}\n",
        version = env!("CARGO_PKG_VERSION"),
        time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
    )
}

/// Write `report` as a new file in `dir` and remove all but the newest [`KEEP`].
fn write(dir: &Path, report: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut path = dir.join(format!("{PREFIX}{stamp}.txt"));
    // Two panics in one second (two threads): number the second.
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = dir.join(format!("{PREFIX}{stamp}-{n}.txt"));
    }
    std::fs::File::create_new(&path)?.write_all(report.as_bytes())?;
    let mut reports = list(dir);
    while reports.len() > KEEP {
        let _ = std::fs::remove_file(reports.remove(0));
    }
    Ok(path)
}

/// The reports in `dir`, oldest first (their names sort by time).
fn list(dir: &Path) -> Vec<PathBuf> {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(".txt"))
        })
        .collect();
    reports.sort();
    reports
}

/// The newest report, unless its name is `seen` (already offered).
pub fn unseen(dir: &Path, seen: Option<&str>) -> Option<PathBuf> {
    let newest = list(dir).pop()?;
    let name = newest.file_name()?.to_str()?;
    (Some(name) != seen).then_some(newest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_reports_and_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let text = report("main", "index out of bounds", "src/x.rs:3:5", "0: frame");
        assert!(text.starts_with(concat!("switchhead ", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("thread: main\nat: src/x.rs:3:5\n\nindex out of bounds"));

        assert_eq!(unseen(dir.path(), None), None);
        for _ in 0..KEEP + 2 {
            write(dir.path(), &text).unwrap();
        }
        let reports = list(dir.path());
        assert_eq!(reports.len(), KEEP);
        assert_eq!(std::fs::read_to_string(&reports[0]).unwrap(), text);

        let newest = unseen(dir.path(), None).unwrap();
        assert_eq!(&newest, reports.last().unwrap());
        let name = newest.file_name().unwrap().to_str().unwrap();
        assert_eq!(unseen(dir.path(), Some(name)), None);
    }
}
