//! One child process, run to completion under a deadline and a cancel flag.
//!
//! The adapter that `llm` and `forgeio` share. Both spawn a program that
//! carries its own credentials, feed it bytes, and want its bytes back before
//! a deadline or the moment a reviewer gives up — and neither wants to own the
//! watchdog that makes that safe. This is that watchdog, once.
//!
//! Threaded i/o on every pipe: a large stdin must not deadlock against a child
//! that writes before it finishes reading, and a large stdout must not fill
//! the pipe while the watchdog waits for exit.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// What to run and how long to wait for it.
pub struct Run<'a> {
    pub argv: &'a [String],
    pub stdin: Option<&'a [u8]>,
    /// `None` inherits this process's directory.
    pub working_dir: Option<&'a Path>,
    pub timeout: Duration,
    /// Set from another thread to kill the child early.
    pub cancel: Option<&'a Arc<AtomicBool>>,
}

/// The child's whole output. The status is not judged here: a non-zero exit
/// means one thing to a completion and another to `is-ancestor`, and the
/// caller knows which it asked for.
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Why there is no output. The command that failed is the caller's to name,
/// so it is not repeated in here.
#[derive(Debug)]
pub enum Failure {
    Spawn(std::io::Error),
    Io(std::io::Error),
    Timeout,
    Cancelled,
}

/// The argv as a reader debugs it.
pub fn describe(argv: &[String]) -> String {
    argv.join(" ")
}

pub fn run(spec: &Run<'_>) -> Result<Output, Failure> {
    let mut cmd = Command::new(&spec.argv[0]);
    if let Some(dir) = spec.working_dir {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .args(&spec.argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Failure::Spawn)?;

    let mut stdin = child.stdin.take().expect("stdin piped");
    let input = spec.stdin.map(<[u8]>::to_vec);
    let writer = std::thread::spawn(move || {
        if let Some(bytes) = input {
            let _ = stdin.write_all(&bytes);
        }
        // stdin closes on drop, so a child that reads to EOF sees it.
    });
    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        out_pipe.read_to_end(&mut buf).map(|_| buf)
    });
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    // Watchdog: poll for exit until the deadline, then kill. The poll is not
    // just the deadline's — the cancel flag has to be read too, which is why
    // this is a loop and not a `wait` with a timeout.
    let deadline = Instant::now() + spec.timeout;
    let cancelled = || spec.cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    let status = loop {
        // Decide first, tear down once.
        let give_up = match child.try_wait().map_err(Failure::Io)? {
            Some(status) => break status,
            None if cancelled() => Some(Failure::Cancelled),
            None if Instant::now() >= deadline => Some(Failure::Timeout),
            None => None,
        };
        if let Some(err) = give_up {
            let _ = child.kill();
            let _ = child.wait();
            let _ = writer.join();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(err);
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let _ = writer.join();
    let stdout = stdout_reader
        .join()
        .expect("stdout reader panicked")
        .map_err(Failure::Io)?;
    let stderr = stderr_reader.join().expect("stderr reader panicked");
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// The first `limit` bytes of stderr as text, for an error message.
pub fn stderr_excerpt(stderr: &[u8], limit: usize) -> String {
    String::from_utf8_lossy(&stderr[..stderr.len().min(limit)])
        .trim()
        .to_string()
}
