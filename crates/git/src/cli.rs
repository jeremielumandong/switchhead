//! The system `git`, for everything that changes a repository.
//!
//! Each call is one short-lived process in the repository's work tree. Output is read as
//! bytes (paths are not always UTF-8). Prompts are turned off so a missing credential
//! fails instead of hanging; credential helpers and SSH agents still work.
//! Long operations (fetch, pull, push, clone) report progress lines through a callback
//! and can be cancelled.

use std::ffi::OsStr;
use std::io::{BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::{Error, Result};

/// A `git` bound to one repository.
#[derive(Clone, Debug)]
pub struct Git {
    program: PathBuf,
    /// Where commands run (the work tree, or the git dir of a bare repository).
    cwd: PathBuf,
}

/// What a command printed.
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// Standard output.
    pub stdout: Vec<u8>,
    /// Standard error.
    pub stderr: Vec<u8>,
}

impl Output {
    /// Standard output as (lossy) text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Standard error as (lossy) text.
    pub fn err_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Cancels a running long operation.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// Request cancellation; the process is killed at its next progress line or poll.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Environment every command gets.
fn base_env(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        // Stable, parseable output whatever the user's locale.
        .env("LC_ALL", "C")
        .env("LANG", "C")
        // Never open an editor or pager from a background command.
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null());
}

fn join_args<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Find `git` on PATH (or `SWITCHHEAD_GIT`).
pub fn find_git() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SWITCHHEAD_GIT").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    let exe = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
}

impl Git {
    /// A `git` that runs in `cwd`.
    pub fn new(cwd: impl Into<PathBuf>) -> Result<Self> {
        let program = find_git().ok_or(Error::GitMissing)?;
        Ok(Self {
            program,
            cwd: cwd.into(),
        })
    }

    /// The directory commands run in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn command<S: AsRef<OsStr>>(&self, args: &[S]) -> Command {
        let mut cmd = Command::new(&self.program);
        // Keep git's output free of color and decoration regardless of config.
        cmd.args(["-c", "color.ui=false", "-c", "core.quotepath=false"])
            .args(args)
            .current_dir(&self.cwd);
        base_env(&mut cmd);
        cmd
    }

    /// Run and wait. A non-zero exit is an [`Error::Command`].
    pub fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Output> {
        self.run_with(args, None, &[])
    }

    /// Run with `stdin` written to the process and extra environment.
    pub fn run_with<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        stdin: Option<&[u8]>,
        env: &[(&str, &OsStr)],
    ) -> Result<Output> {
        let started = Instant::now();
        let mut cmd = self.command(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::GitMissing,
            _ => Error::Io(e),
        })?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            let input = input.to_vec();
            // Write on a thread so a large input cannot deadlock against a full stdout pipe.
            std::thread::spawn(move || {
                let _ = pipe.write_all(&input);
            });
        }
        let out = child.wait_with_output()?;
        tracing::debug!(
            args = %join_args(args),
            ms = started.elapsed().as_millis() as u64,
            code = out.status.code(),
            "git"
        );
        let output = Output {
            stdout: out.stdout,
            stderr: out.stderr,
        };
        if out.status.success() {
            Ok(output)
        } else {
            Err(command_error(args, out.status.code(), &output))
        }
    }

    /// Run and return stdout as text.
    pub fn text<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<String> {
        Ok(self.run(args)?.text())
    }

    /// Run a long operation with `--progress`, sending each progress or output line to
    /// `on_line` as it arrives. `cancel` kills the process.
    pub fn run_streaming<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        cancel: &Cancel,
        mut on_line: impl FnMut(&str),
    ) -> Result<Output> {
        let mut cmd = self.command(args);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::GitMissing,
            _ => Error::Io(e),
        })?;
        let mut stdout = child.stdout.take();
        let out_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let mut stderr_all = Vec::new();
        if let Some(stderr) = child.stderr.take() {
            // git ends progress updates with '\r'; split on both.
            let mut r = BufReader::new(stderr);
            let mut chunk = Vec::new();
            loop {
                if cancel.is_cancelled() {
                    let _ = child.kill();
                    break;
                }
                chunk.clear();
                let n = read_until_any(&mut r, &mut chunk)?;
                if n == 0 {
                    break;
                }
                stderr_all.extend_from_slice(&chunk);
                let line = String::from_utf8_lossy(&chunk);
                let line = line.trim_end_matches(['\r', '\n']);
                if !line.is_empty() {
                    on_line(line);
                }
            }
        }
        let status = child.wait()?;
        let stdout = out_thread.join().unwrap_or_default();
        let output = Output {
            stdout,
            stderr: stderr_all,
        };
        if cancel.is_cancelled() {
            return Err(Error::Command {
                args: join_args(args),
                code: status.code(),
                message: "cancelled".into(),
            });
        }
        if status.success() {
            Ok(output)
        } else {
            Err(command_error(args, status.code(), &output))
        }
    }
}

/// Read up to and including the next `\n` or `\r`.
fn read_until_any(r: &mut impl std::io::BufRead, out: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut total = 0;
    loop {
        let (done, used) = {
            let buf = r.fill_buf()?;
            if buf.is_empty() {
                return Ok(total);
            }
            match buf.iter().position(|b| *b == b'\n' || *b == b'\r') {
                Some(i) => {
                    out.extend_from_slice(&buf[..=i]);
                    (true, i + 1)
                }
                None => {
                    out.extend_from_slice(buf);
                    (false, buf.len())
                }
            }
        };
        r.consume(used);
        total += used;
        if done {
            return Ok(total);
        }
    }
}

fn command_error<S: AsRef<OsStr>>(args: &[S], code: Option<i32>, out: &Output) -> Error {
    let mut message = out.err_text().trim().to_owned();
    if message.is_empty() {
        message = out.text().trim().to_owned();
    }
    if message.is_empty() {
        message = format!("exit code {code:?}");
    }
    Error::Command {
        args: join_args(args),
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn runs_and_reports_failures() {
        let r = TestRepo::new();
        r.commit_file("a.txt", "hello\n", "first");
        let git = Git::new(r.path()).unwrap();
        let log = git.text(&["log", "--format=%s"]).unwrap();
        assert_eq!(log.trim(), "first");
        let err = git.run(&["checkout", "no-such-branch"]).unwrap_err();
        match err {
            Error::Command { code, message, .. } => {
                assert_eq!(code, Some(1));
                assert!(message.contains("no-such-branch"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stdin_reaches_git() {
        let r = TestRepo::new();
        r.write("a.txt", "x\n");
        let git = Git::new(r.path()).unwrap();
        git.run(&["add", "a.txt"]).unwrap();
        git.run_with(
            &["commit", "-q", "-F", "-"],
            Some(b"from stdin\n\nbody"),
            &[],
        )
        .unwrap();
        let msg = git.text(&["log", "-1", "--format=%B"]).unwrap();
        assert_eq!(msg.trim(), "from stdin\n\nbody");
    }

    #[test]
    fn splits_progress_on_carriage_returns() {
        let mut r = BufReader::new(&b"Counting 1%\rCounting 50%\rdone\n"[..]);
        let mut lines = Vec::new();
        let mut buf = Vec::new();
        while read_until_any(&mut r, &mut buf).unwrap() > 0 {
            lines.push(String::from_utf8_lossy(&buf).trim_end().to_owned());
            buf.clear();
        }
        assert_eq!(lines, ["Counting 1%", "Counting 50%", "done"]);
    }
}
