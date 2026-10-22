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
    no_window(cmd);
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

/// On Windows, start a console program (git, `cmd`) without a console window. The app is
/// a GUI-subsystem binary with no console of its own, so each child would otherwise open
/// one that flashes on screen until it exits.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
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
        on_line: impl FnMut(&str),
    ) -> Result<Output> {
        self.run_streaming_with(args, &[], false, cancel, on_line)
    }

    /// [`Git::run_streaming`] with extra environment; `stdout_lines` also sends stdout's
    /// lines to `on_line` (git-lfs writes its progress there).
    pub fn run_streaming_with<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        env: &[(&str, &OsStr)],
        stdout_lines: bool,
        cancel: &Cancel,
        mut on_line: impl FnMut(&str),
    ) -> Result<Output> {
        let mut cmd = self.command(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        // Its own process group, so cancelling also stops what git started (the per-remote
        // fetches of `fetch --all`, ssh, transport helpers).
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::GitMissing,
            _ => Error::Io(e),
        })?;
        // stderr is read on its own thread so a silent git (a stalled connection) can
        // still be cancelled: this loop wakes every 50 ms to check.
        // Chunks are tagged `true` when they come from stdout.
        let (tx, rx) = std::sync::mpsc::channel::<(bool, Vec<u8>)>();
        let stdout = child.stdout.take();
        let out_tx = stdout_lines.then(|| tx.clone());
        let out_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            match (stdout, out_tx) {
                (Some(s), Some(tx)) => {
                    let mut r = BufReader::new(s);
                    loop {
                        let mut chunk = Vec::new();
                        match read_until_any(&mut r, &mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                buf.extend_from_slice(&chunk);
                                let _ = tx.send((true, chunk));
                            }
                        }
                    }
                }
                (Some(mut s), None) => {
                    let _ = s.read_to_end(&mut buf);
                }
                (None, _) => {}
            }
            buf
        });
        let stderr = child.stderr.take();
        let err_thread = std::thread::spawn(move || {
            let Some(stderr) = stderr else { return };
            // git ends progress updates with '\r'; split on both.
            let mut r = BufReader::new(stderr);
            loop {
                let mut chunk = Vec::new();
                match read_until_any(&mut r, &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send((false, chunk)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut stderr_all = Vec::new();
        let mut killed = false;
        // When git has exited, a process it started (ssh, a transport helper) can keep
        // stderr open: give the pipe a moment after the exit, then stop reading.
        let mut exited: Option<Instant> = None;
        loop {
            if cancel.is_cancelled() && !killed {
                kill_tree(&mut child);
                killed = true;
            }
            if exited.is_none() && matches!(child.try_wait(), Ok(Some(_))) {
                exited = Some(Instant::now());
            }
            if exited.is_some_and(|t| t.elapsed() > std::time::Duration::from_millis(500)) {
                break;
            }
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok((from_stdout, chunk)) => {
                    if !from_stdout {
                        stderr_all.extend_from_slice(&chunk);
                    }
                    let line = String::from_utf8_lossy(&chunk);
                    let line = line.trim_end_matches(['\r', '\n']);
                    if !line.is_empty() && !killed {
                        on_line(line);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        // The reader ends with the pipe; only wait for it when the pipe is already closed.
        if err_thread.is_finished() {
            let _ = err_thread.join();
        }
        let status = child.wait()?;
        // Like stderr, stdout can be held open by a process git left behind.
        let stdout = join_within(out_thread, std::time::Duration::from_millis(500));
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

impl Git {
    /// Run and hand each stdout line (without its newline) to `on_line` as it arrives, for
    /// output that is useful before the command ends (`blame --incremental`). `on_line`
    /// returning `false`, or `cancel`, kills the process; that is not an error.
    pub fn run_lines<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        cancel: &Cancel,
        mut on_line: impl FnMut(&[u8]) -> bool,
    ) -> Result<()> {
        use std::io::BufRead as _;

        let started = Instant::now();
        let mut cmd = self.command(args);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::GitMissing,
            _ => Error::Io(e),
        })?;
        let mut stderr = child.stderr.take();
        let err_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let mut stopped = false;
        if let Some(stdout) = child.stdout.take() {
            let mut r = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                if cancel.is_cancelled() {
                    stopped = true;
                    break;
                }
                line.clear();
                if r.read_until(b'\n', &mut line)? == 0 {
                    break;
                }
                let l = line.strip_suffix(b"\n").unwrap_or(&line);
                if !on_line(l) {
                    stopped = true;
                    break;
                }
            }
        }
        if stopped {
            let _ = child.kill();
        }
        let status = child.wait()?;
        let stderr = err_thread.join().unwrap_or_default();
        tracing::debug!(
            args = %join_args(args),
            ms = started.elapsed().as_millis() as u64,
            code = status.code(),
            "git (streamed)"
        );
        if stopped || status.success() {
            Ok(())
        } else {
            Err(command_error(
                args,
                status.code(),
                &Output {
                    stdout: Vec::new(),
                    stderr,
                },
            ))
        }
    }
}

/// Run a user's own shell command in `cwd` (custom commands): `sh -c` on Unix, `cmd /C`
/// on Windows, with `env` added. Its output (stdout and stderr together) goes to `on_line`
/// line by line as it arrives and is returned at the end. Prompts and editors are off as
/// for git. `cancel` kills it and, on Unix, what it started. A non-zero exit is an
/// [`Error::Command`] whose message is the end of the output.
pub fn run_shell(
    cwd: &Path,
    script: &str,
    env: &[(&str, &str)],
    cancel: &Cancel,
    mut on_line: impl FnMut(&str),
) -> Result<String> {
    let started = Instant::now();
    let (reader, writer) = std::io::pipe()?;
    let mut cmd = shell_command(script);
    cmd.current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let spawned = cmd.spawn();
    // The command holds the pipe's write ends; drop them so the pipe ends with the child.
    drop(cmd);
    let mut child = spawned?;
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut r = BufReader::new(reader);
        loop {
            let mut chunk = Vec::new();
            match read_until_any(&mut r, &mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(chunk).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut all = Vec::new();
    let mut killed = false;
    // A process the command left running in the background can keep the pipe open.
    let mut exited: Option<Instant> = None;
    loop {
        if cancel.is_cancelled() && !killed {
            kill_tree(&mut child);
            killed = true;
        }
        if exited.is_none() && matches!(child.try_wait(), Ok(Some(_))) {
            exited = Some(Instant::now());
        }
        if exited.is_some_and(|t| t.elapsed() > std::time::Duration::from_millis(500)) {
            break;
        }
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(chunk) => {
                all.extend_from_slice(&chunk);
                let line = String::from_utf8_lossy(&chunk);
                let line = line.trim_end_matches(['\r', '\n']);
                if !line.is_empty() && !killed {
                    on_line(line);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait()?;
    let text = String::from_utf8_lossy(&all).into_owned();
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        code = status.code(),
        "custom command"
    );
    if cancel.is_cancelled() {
        return Err(Error::Command {
            args: "custom command".into(),
            code: status.code(),
            message: "cancelled".into(),
        });
    }
    if status.success() {
        return Ok(text);
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(20)..].join("\n");
    Err(Error::Command {
        args: "custom command".into(),
        code: status.code(),
        message: if tail.is_empty() {
            format!("exit code {:?}", status.code())
        } else {
            tail
        },
    })
}

#[cfg(unix)]
fn shell_command(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script);
    cmd
}

#[cfg(windows)]
fn shell_command(script: &str) -> Command {
    use std::os::windows::process::CommandExt as _;
    let mut cmd = Command::new("cmd");
    // `/S /C "..."`: cmd strips the outer quotes and runs the rest as typed.
    cmd.raw_arg(format!("/S /C \"{script}\""));
    no_window(&mut cmd);
    cmd
}

/// Read up to and including the next `\n` or `\r`.
/// Kill git and, on Unix, its process group (see [`Git::run_streaming`]).
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // procps `kill` needs `--` before a negative pid (a process group).
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// The thread's result if it ends within `limit`, else empty (the thread is left to end
/// on its own).
fn join_within(t: std::thread::JoinHandle<Vec<u8>>, limit: std::time::Duration) -> Vec<u8> {
    let started = Instant::now();
    while !t.is_finished() && started.elapsed() < limit {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if t.is_finished() {
        t.join().unwrap_or_default()
    } else {
        Vec::new()
    }
}

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

    #[cfg(unix)]
    #[test]
    fn cancels_a_silent_command() {
        let r = TestRepo::new();
        let git = Git::new(r.path()).unwrap();
        let cancel = Cancel::default();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            c.cancel();
        });
        let started = Instant::now();
        // A transport that prints nothing for 20 s.
        let err = git
            .run_streaming(
                &[
                    "-c",
                    "protocol.ext.allow=always",
                    "fetch",
                    "ext::sh -c sleep% 20",
                ],
                &cancel,
                |_| {},
            )
            .unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(err.to_string().contains("cancelled"), "{err}");
        // The transport's own process is gone too.
        let left = Command::new("pgrep")
            .args(["-f", "sleep 20"])
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default();
        assert!(left.is_empty(), "{}", String::from_utf8_lossy(&left));
    }

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

    #[cfg(unix)]
    #[test]
    fn runs_shell_commands() {
        let r = TestRepo::new();
        let mut lines = Vec::new();
        let out = run_shell(
            r.path(),
            "echo one; echo two >&2; echo \"$X\"",
            &[("X", "three")],
            &Cancel::default(),
            |l| lines.push(l.to_owned()),
        )
        .unwrap();
        assert_eq!(lines, ["one", "two", "three"]);
        assert_eq!(out, "one\ntwo\nthree\n");
        let err = run_shell(
            r.path(),
            "echo bad; exit 3",
            &[],
            &Cancel::default(),
            |_| {},
        )
        .unwrap_err();
        match err {
            Error::Command { code, message, .. } => {
                assert_eq!((code, message.as_str()), (Some(3), "bad"));
            }
            other => panic!("{other:?}"),
        }
        let cancel = Cancel::default();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            c.cancel();
        });
        let started = Instant::now();
        assert!(run_shell(r.path(), "sleep 21", &[], &cancel, |_| {}).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
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
