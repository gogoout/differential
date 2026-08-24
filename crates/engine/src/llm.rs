//! LLM backend abstraction (ADR 0016; an engine module since ADR 0018).
//!
//! Nothing else in the engine may reach into subprocess machinery: grouping
//! and the pipeline consume only `LlmBackend`/`CommandBackend` from here.
//!
//! The grouping stage needs exactly one capability from a model: one-shot text
//! completion — prompt in, raw text out. The contract is deliberately that
//! narrow: no tools, no streaming, no chat state. Every observed hard failure
//! of the evaluated grouping tools was a model CLI stopping to call a tool
//! (ADR 0010); a backend that cannot express tools cannot fail that way.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("failed to spawn {command}: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{command} exited with {code:?}: {stderr}")]
    Failed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },

    #[error("{command} produced no output")]
    Empty { command: String },

    #[error("{command} exceeded the {timeout:?} deadline and was killed")]
    Timeout { command: String, timeout: Duration },

    #[error("{command} was cancelled and killed")]
    Cancelled { command: String },

    #[error("io error talking to {command}: {source}")]
    Io {
        command: String,
        #[source]
        source: std::io::Error,
    },
}

/// One-shot completion: prompt in, raw text out.
pub trait LlmBackend: Send + Sync {
    /// Human-readable backend name, for logs and audit output.
    fn name(&self) -> &str;
    fn complete(&self, prompt: &str) -> Result<String, LlmError>;
}

/// A subprocess backend: prompt on stdin, completion on stdout.
pub struct CommandBackend {
    argv: Vec<String>,
    timeout: Duration,
    name: String,
    /// Set from another thread to kill an in-flight child (a reviewer
    /// abandoning the wait). Without this the subprocess would outlive the
    /// process that asked for it, up to the whole timeout.
    cancel: Option<Arc<AtomicBool>>,
}

impl CommandBackend {
    pub fn new(argv: Vec<String>, timeout: Duration) -> Self {
        assert!(!argv.is_empty(), "CommandBackend needs a program to run");
        let name = argv.join(" ");
        CommandBackend {
            argv,
            timeout,
            name,
            cancel: None,
        }
    }

    /// Kill the child as soon as `flag` is set.
    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// The validated default (ADR 0010): headless, text output, tools denied.
    pub fn claude_cli() -> Self {
        Self::new(
            [
                "claude",
                "-p",
                "--output-format",
                "text",
                "--allowed-tools",
                "",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            Duration::from_secs(1200),
        )
    }
}

impl LlmBackend for CommandBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn complete(&self, prompt: &str) -> Result<String, LlmError> {
        let mut child = Command::new(&self.argv[0])
            .args(&self.argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| LlmError::Spawn {
                command: self.name.clone(),
                source,
            })?;

        // Prompt in and output out run on their own threads: a large prompt
        // must not deadlock against a child that writes before it finishes
        // reading, and a large completion must not fill the pipe while the
        // watchdog waits for exit.
        let mut stdin = child.stdin.take().expect("stdin piped");
        let prompt_owned = prompt.as_bytes().to_vec();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&prompt_owned);
            // stdin closes on drop
        });
        use std::io::Read;
        let mut out_pipe = child.stdout.take().expect("stdout piped");
        let stdout_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let res = out_pipe.read_to_end(&mut buf);
            res.map(|_| buf)
        });
        let mut err_pipe = child.stderr.take().expect("stderr piped");
        let stderr_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err_pipe.read_to_end(&mut buf);
            buf
        });

        // Watchdog: poll for exit until the deadline, then kill.
        let deadline = std::time::Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait().map_err(|source| LlmError::Io {
                command: self.name.clone(),
                source,
            })? {
                Some(status) => break status,
                None if self.cancelled() => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = writer.join();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(LlmError::Cancelled {
                        command: self.name.clone(),
                    });
                }
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = writer.join();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(LlmError::Timeout {
                        command: self.name.clone(),
                        timeout: self.timeout,
                    });
                }
                None => std::thread::sleep(Duration::from_millis(25)),
            }
        };
        let _ = writer.join();

        let stdout = stdout_reader
            .join()
            .expect("stdout reader panicked")
            .map_err(|source| LlmError::Io {
                command: self.name.clone(),
                source,
            })?;
        let stderr = stderr_reader.join().expect("stderr reader panicked");

        if !status.success() {
            return Err(LlmError::Failed {
                command: self.name.clone(),
                code: status.code(),
                stderr: String::from_utf8_lossy(&stderr[..stderr.len().min(600)]).into_owned(),
            });
        }
        let text = String::from_utf8_lossy(&stdout).into_owned();
        if text.trim().is_empty() {
            return Err(LlmError::Empty {
                command: self.name.clone(),
            });
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cat_echoes_the_prompt() {
        let b = CommandBackend::new(vec!["cat".into()], Duration::from_secs(10));
        let out = b.complete("hello prompt\n").unwrap();
        assert_eq!(out, "hello prompt\n");
    }

    #[test]
    fn nonzero_exit_is_failed() {
        let b = CommandBackend::new(vec!["false".into()], Duration::from_secs(10));
        match b.complete("x") {
            Err(LlmError::Failed { code, .. }) => assert_eq!(code, Some(1)),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn empty_output_is_an_error() {
        let b = CommandBackend::new(vec!["true".into()], Duration::from_secs(10));
        match b.complete("x") {
            Err(LlmError::Empty { .. }) => {}
            other => panic!("expected Empty, got {other:?}"),
        }
    }

    #[test]
    fn cancel_kills_the_child() {
        // A long sleep with a generous deadline: only the cancel flag can end
        // this, and it must do so promptly rather than leaving the child to
        // outlive the caller.
        let flag = Arc::new(AtomicBool::new(false));
        let backend =
            CommandBackend::new(vec!["sleep".into(), "600".into()], Duration::from_secs(600))
                .with_cancel(Arc::clone(&flag));
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        });
        let err = backend.complete("hello").unwrap_err();
        assert!(
            matches!(err, LlmError::Cancelled { .. }),
            "expected cancellation, got {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "child was not killed promptly"
        );
    }

    #[test]
    fn deadline_kills_the_child() {
        let b = CommandBackend::new(
            vec!["sleep".into(), "30".into()],
            Duration::from_millis(200),
        );
        let started = std::time::Instant::now();
        match b.complete("x") {
            Err(LlmError::Timeout { .. }) => {}
            other => panic!("expected Timeout, got {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "child was not killed promptly"
        );
    }

    #[test]
    fn large_prompt_does_not_deadlock() {
        // A prompt bigger than the pipe buffer, against a child that echoes
        // while reading: the writer thread prevents the classic deadlock.
        let b = CommandBackend::new(vec!["cat".into()], Duration::from_secs(30));
        let big = "line of prompt text\n".repeat(60_000); // ~1.2 MB
        let out = b.complete(&big).unwrap();
        assert_eq!(out.len(), big.len());
    }

    #[test]
    fn claude_cli_default_denies_tools() {
        let b = CommandBackend::claude_cli();
        assert!(b.name().contains("--allowed-tools"));
    }
}
