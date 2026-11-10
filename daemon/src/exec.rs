//! Running the fixed external tools (`hyprctl`, `grim`, `magick`, `wtype`, `wl-copy`, `wl-paste`) —
//! argv only, never a shell.
//!
//! Every call goes through [`CommandRunner`] so tests can answer without a compositor, and every
//! call is bounded: the child is killed when it overruns its timeout, its output is capped so a
//! hostile or wedged tool cannot exhaust this process, and input handed to a child on stdin is
//! capped and written from its own thread so neither direction can deadlock the other.

use crate::error::{ApiError, ErrorCode};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The most output (stdout or stderr) this daemon will hold for one child.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// The most input this daemon will feed a child on stdin (4 MiB).
pub const MAX_STDIN_BYTES: usize = 4 * 1024 * 1024;
/// How often a running child is polled for exit.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// The longest error text this daemon will put into a message or an audit entry.
pub const MAX_MESSAGE_CHARS: usize = 200;

/// The outcome of one external call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit status, `None` when the child was killed or died without one.
    pub status: Option<i32>,
    /// Standard output.
    pub stdout: Vec<u8>,
    /// Standard error.
    pub stderr: Vec<u8>,
    /// Whether the timeout fired and the child was killed.
    pub timed_out: bool,
}

impl CommandOutput {
    /// Exit status 0 and not timed out.
    pub fn success(&self) -> bool {
        !self.timed_out && self.status == Some(0)
    }

    /// Standard output as text (lossy).
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// A one-line description for an error message: the exit status plus the first of stderr.
    pub fn failure_note(&self) -> String {
        if self.timed_out {
            return "timed out and was killed".to_string();
        }
        let status = match self.status {
            Some(code) => format!("exit status {code}"),
            None => "no exit status".to_string(),
        };
        let stderr = one_line(&self.stderr_text(), MAX_MESSAGE_CHARS);
        if stderr.is_empty() {
            status
        } else {
            format!("{status}: {stderr}")
        }
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Run argv arrays, bounded in time and memory.
///
/// Implemented by [`SystemRunner`] in production and by test doubles in `tests/`.
pub trait CommandRunner: Send + Sync + std::fmt::Debug {
    /// Run `program` with `args`, killing it after `timeout`.
    fn run(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput, ApiError>;

    /// Run `program` with `args`, feeding `input` on stdin and closing it afterwards.
    ///
    /// The child's stdin is a pipe; `input` is written from its own thread, so a tool that is slow
    /// to read cannot stall the watchdogs, and a killed child unblocks the writer with `EPIPE`
    /// instead of leaving it hanging.
    fn run_with_input(
        &self,
        program: &str,
        args: &[&str],
        input: &[u8],
        timeout: Duration,
    ) -> Result<CommandOutput, ApiError>;

    /// Start `program` and do not wait for it.
    ///
    /// For launch actions: a terminal (or browser) is supposed to outlive the call, so waiting for
    /// it would time out on success. A spawn that fails is still an error; an app that starts and
    /// then exits is not the daemon's to judge. The child is reaped by a detached thread so it
    /// cannot become a zombie of this long-lived process.
    fn spawn_detached(&self, program: &str, args: &[&str]) -> Result<(), ApiError>;

    /// Like [`run_with_input`], but the child's stdout and stderr go to `/dev/null`.
    ///
    /// For tools that fork a helper which inherits the pipes: `wl-copy` forks the process that
    /// serves the selection, so waiting for EOF on its stdout would block until the clipboard is
    /// replaced. Only the exit status is meaningful for those calls. The default implementation
    /// captures output, so test doubles that only implement [`run_with_input`] keep working.
    fn run_with_input_quiet(
        &self,
        program: &str,
        args: &[&str],
        input: &[u8],
        timeout: Duration,
    ) -> Result<CommandOutput, ApiError> {
        self.run_with_input(program, args, input, timeout)
    }
}

/// The real thing: `std::process::Command` with a watchdog.
#[derive(Debug, Default)]
pub struct SystemRunner;

impl SystemRunner {
    /// A runner for this process's environment.
    pub fn new() -> Self {
        Self
    }
}

impl SystemRunner {
    /// Spawn, feed stdin (when there is input), and reap against a deadline.
    fn run_inner(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<CommandOutput, ApiError> {
        self.run_inner_with(program, args, input, timeout, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_inner_with(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&[u8]>,
        timeout: Duration,
        quiet: bool,
    ) -> Result<CommandOutput, ApiError> {
        if let Some(input) = input
            && input.len() > MAX_STDIN_BYTES
        {
            return Err(ApiError::new(
                ErrorCode::TooLarge,
                format!(
                    "{} KiB is more than the {} KiB this daemon will write to {program}",
                    input.len().div_ceil(1024),
                    MAX_STDIN_BYTES / 1024
                ),
            ));
        }
        let stdin = if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        };
        let (stdout, stderr) = if quiet {
            (Stdio::null(), Stdio::null())
        } else {
            (Stdio::piped(), Stdio::piped())
        };
        let mut child = Command::new(program)
            .args(args)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|err| {
                if err.kind() == io::ErrorKind::NotFound {
                    ApiError::new(
                        ErrorCode::ToolMissing,
                        format!("{program} is not installed (it is not on PATH)"),
                    )
                } else {
                    ApiError::internal(format!("could not run {program}: {err}"))
                }
            })?;

        // The readers start first: a child that writes before it reads must never find both of its
        // output pipes full while we are still blocked on stdin.
        let stdout_thread = child
            .stdout
            .take()
            .map(|pipe| thread::spawn(move || drain(pipe)));
        let stderr_thread = child
            .stderr
            .take()
            .map(|pipe| thread::spawn(move || drain(pipe)));
        let stdin_thread = match (input, child.stdin.take()) {
            (Some(input), Some(mut pipe)) => {
                let body = input.to_vec();
                Some(thread::spawn(move || {
                    // An error here is expected when the child was killed; dropping `pipe` closes
                    // the pipe either way, which is what most tools wait for.
                    let _ = pipe.write_all(&body);
                    let _ = pipe.flush();
                }))
            }
            _ => None,
        };

        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        timed_out = true;
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    thread::sleep(POLL_INTERVAL);
                }
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    join_writer(stdin_thread);
                    return Err(ApiError::internal(format!(
                        "waiting for {program} failed: {err}"
                    )));
                }
            }
        };
        // The child is gone (reaped above, killed and reaped on timeout), so the pipe is closed and
        // a writer blocked on a full pipe comes back with EPIPE. Joining here cannot deadlock.
        join_writer(stdin_thread);

        Ok(CommandOutput {
            status: status.and_then(|status| status.code()),
            stdout: join_reader(stdout_thread)?,
            stderr: join_reader(stderr_thread)?,
            timed_out,
        })
    }
}

impl CommandRunner for SystemRunner {
    fn spawn_detached(&self, program: &str, args: &[&str]) -> Result<(), ApiError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| {
                if err.kind() == io::ErrorKind::NotFound {
                    ApiError::new(
                        ErrorCode::ToolMissing,
                        format!("{program} is not installed (it is not on PATH)"),
                    )
                } else {
                    ApiError::internal(format!("could not start {program}: {err}"))
                }
            })?;
        // Reap on exit (a terminal lives as long as its window).
        thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    fn run(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput, ApiError> {
        self.run_inner(program, args, None, timeout)
    }

    fn run_with_input(
        &self,
        program: &str,
        args: &[&str],
        input: &[u8],
        timeout: Duration,
    ) -> Result<CommandOutput, ApiError> {
        self.run_inner(program, args, Some(input), timeout)
    }

    fn run_with_input_quiet(
        &self,
        program: &str,
        args: &[&str],
        input: &[u8],
        timeout: Duration,
    ) -> Result<CommandOutput, ApiError> {
        self.run_inner_with(program, args, Some(input), timeout, true)
    }
}

/// Collect at most [`MAX_OUTPUT_BYTES`], then keep draining so the child never blocks on a full pipe.
fn drain(mut pipe: impl Read) -> Vec<u8> {
    let mut collected = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let room = MAX_OUTPUT_BYTES.saturating_sub(collected.len());
                let keep = room.min(read);
                collected.extend_from_slice(&buffer[..keep]);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    collected
}

fn join_reader(handle: Option<thread::JoinHandle<Vec<u8>>>) -> Result<Vec<u8>, ApiError> {
    match handle {
        None => Ok(Vec::new()),
        Some(handle) => handle
            .join()
            .map_err(|_| ApiError::internal("a pipe reader thread failed")),
    }
}

/// Wait for the stdin writer. Its result is deliberately ignored: a broken pipe means the child
/// stopped reading, which the exit status already reports.
///
/// This is called only after the child has been reaped (or killed and reaped), so the read end of
/// the pipe is already closed and a writer blocked on a full pipe comes back with `EPIPE` instead of
/// keeping the daemon waiting. The fixed tools this feeds (`wl-copy`) read stdin to EOF, so the
/// writer is not left behind in normal use either.
fn join_writer(handle: Option<thread::JoinHandle<()>>) {
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

/// Find an executable on `PATH` without invoking a shell.
pub fn which(program: &str) -> Option<PathBuf> {
    if program.is_empty() || program.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(program);
        let Ok(meta) = fs::metadata(&candidate) else {
            continue;
        };
        if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
            return Some(candidate);
        }
    }
    None
}

/// Squash text onto one line and cut it to `max_chars` characters.
///
/// Tool output and error text end up in messages and audit entries, so it is flattened first.
pub fn one_line(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_control() || ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
        if out.chars().count() >= max_chars {
            break;
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn one_line_flattens_and_truncates() {
        assert_eq!(one_line("a\nb\tc", 32), "a b c");
        assert_eq!(one_line("  lots   of\nspace  ", 32), "lots of space");
        assert_eq!(one_line("abcdef", 3), "abc");
        assert_eq!(one_line("", 10), "");
        // Control characters (a colour code, a NUL) do not survive into the log.
        assert_eq!(one_line("a\u{1b}[31mb", 32), "a [31mb");
        assert_eq!(one_line("a\u{0}b", 32), "a b");
    }

    #[test]
    fn which_finds_a_real_tool_and_rejects_paths() {
        // `sh` exists in /bin and /usr/bin on every Linux this runs on.
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-tool-xyzzy").is_none());
        assert!(
            which("/bin/sh").is_none(),
            "a path is not looked up on PATH"
        );
        assert!(which("").is_none());
    }

    #[test]
    fn system_runner_reports_a_missing_tool() {
        let err = SystemRunner::new()
            .run(
                "omassistant-not-a-real-tool-xyzzy",
                &[],
                Duration::from_secs(5),
            )
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::ToolMissing);
        assert!(err.message().contains("not installed"), "{}", err.message());
    }

    #[test]
    fn system_runner_captures_output_and_status() {
        let output = SystemRunner::new()
            .run(
                "sh",
                &["-c", "printf hello; printf oops >&2; exit 3"],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(output.status, Some(3));
        assert!(!output.success());
        assert_eq!(output.stdout_text(), "hello");
        assert_eq!(output.stderr_text(), "oops");
        assert!(!output.timed_out);
        assert!(output.failure_note().contains("exit status 3"));

        let output = SystemRunner::new()
            .run("sh", &["-c", "printf done"], Duration::from_secs(5))
            .unwrap();
        assert!(output.success());
    }

    #[test]
    fn a_wedged_child_is_killed_at_the_timeout() {
        let started = Instant::now();
        let output = SystemRunner::new()
            .run("sh", &["-c", "sleep 30"], Duration::from_millis(150))
            .unwrap();
        assert!(output.timed_out, "the timeout should have fired");
        assert!(!output.success());
        assert!(output.failure_note().contains("timed out"));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the watchdog must not wait for the child"
        );
    }

    #[test]
    fn stdin_is_fed_and_closed() {
        let output = SystemRunner::new()
            .run_with_input("cat", &[], b"hello stdin", Duration::from_secs(5))
            .unwrap();
        assert!(output.success());
        assert_eq!(output.stdout_text(), "hello stdin");

        // Empty input is still a pipe that gets closed, which is how a tool learns to stop.
        let output = SystemRunner::new()
            .run_with_input("cat", &[], b"", Duration::from_secs(5))
            .unwrap();
        assert!(output.success());
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn a_large_stdin_does_not_deadlock_against_a_large_stdout() {
        // 1 MiB in and 1 MiB out through two pipes at once: the classic two-way deadlock, made
        // impossible by writing stdin from its own thread.
        let body = vec![b'x'; 1024 * 1024];
        let output = SystemRunner::new()
            .run_with_input("cat", &[], &body, Duration::from_secs(20))
            .unwrap();
        assert!(output.success());
        assert_eq!(output.stdout.len(), body.len());
    }

    #[test]
    fn a_child_that_never_reads_its_stdin_is_still_killed_at_the_timeout() {
        let body = vec![b'x'; 1024 * 1024];
        let started = Instant::now();
        let output = SystemRunner::new()
            .run_with_input("sh", &["-c", "sleep 30"], &body, Duration::from_millis(150))
            .unwrap();
        assert!(output.timed_out, "the timeout should have fired");
        assert!(!output.success());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the blocked writer must not keep the watchdog waiting"
        );
    }

    #[test]
    fn a_child_that_exits_without_reading_releases_the_writer() {
        // 200 KiB is more than a pipe buffer, so the writer really does block — until the child
        // exits and the read end closes under it with EPIPE.
        let body = vec![b'x'; 200 * 1024];
        let started = Instant::now();
        let output = SystemRunner::new()
            .run_with_input("sh", &["-c", "exit 0"], &body, Duration::from_secs(10))
            .unwrap();
        assert!(output.success());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the writer must be released when the reader goes away"
        );
    }

    #[test]
    fn oversized_stdin_is_refused_before_a_child_runs() {
        let body = vec![b'x'; MAX_STDIN_BYTES + 1];
        let err = SystemRunner::new()
            .run_with_input("cat", &[], &body, Duration::from_secs(5))
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::TooLarge);
        assert!(err.message().contains("KiB"), "{}", err.message());
    }

    #[test]
    fn output_is_capped() {
        // 200 KiB through a 8 MiB cap: all of it arrives, nothing is lost.
        let output = SystemRunner::new()
            .run(
                "sh",
                &["-c", "head -c 200000 /dev/zero | tr '\\0' 'x'"],
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(output.stdout.len(), 200_000);
    }
}
