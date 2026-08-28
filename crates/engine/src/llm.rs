//! LLM backend abstraction (ADR 0016; an engine module since ADR 0018).
//!
//! Nothing else in the engine may reach into subprocess machinery: grouping
//! and the pipeline consume only `LlmBackend`/`CommandBackend` from here.
//!
//! The grouping stage needs exactly one capability from a model: one-shot text
//! completion — prompt in, raw text out. The contract is deliberately that
//! narrow: no streaming, no chat state, no conversation to manage.
//!
//! It stays that narrow now that the model reads for itself (ADR 0022). Tools
//! run inside the CLI this spawns, so what crosses this seam is still a prompt
//! and a string. What changed is a flag in the argv below, not the trait.

use std::io::Write;
use std::path::{Path, PathBuf};
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
    /// What to call this agent on screen, for a reviewer waiting on it.
    ///
    /// A product name, not a command line: "Claude Code", not `claude -p
    /// --output-format text --allowed-tools Bash(...),...`. The reviewer is
    /// waiting to learn *which agent* is thinking, and the argv answers a
    /// different question at four times the width — it overran the splash line
    /// the moment the allowlist grew.
    ///
    /// The command as it will actually run is still reported where it is the
    /// answer: `LlmError` carries it, because a spawn failure is debugged with
    /// the whole argv and nothing less.
    fn name(&self) -> &str;

    /// Everything about this backend that could change the grouping, and
    /// nothing that could not. The grouping cache key hashes this (ADR 0009).
    ///
    /// Separate from `name` because the two answer different questions. `name`
    /// is what to show a reviewer, so it is a product name. This is what
    /// determines the answer, so it is the argv — minus the parts that say
    /// where this machine keeps things. Hashing a path put the absolute
    /// location of `dfr` into the key, so a debug build, a release build and
    /// two checkouts of one commit each re-ran a four-hundred-second call for
    /// an identical class partition, and the worktree-shared cache
    /// `plan::grouping_cache_dir` promises was defeated.
    ///
    /// Defaults to `name`, which is right for any backend whose identity has no
    /// environment in it.
    fn identity(&self) -> &str {
        self.name()
    }

    fn complete(&self, prompt: &str) -> Result<String, LlmError>;
}

/// A subprocess backend: prompt on stdin, completion on stdout.
pub struct CommandBackend {
    argv: Vec<String>,
    timeout: Duration,
    /// What a reviewer is shown: see [`LlmBackend::name`].
    name: String,
    /// The argv as it will actually run, for error text only. A spawn failure
    /// is debugged with the whole command, and neither `name` nor `identity`
    /// is that: one is a product name, the other stands a placeholder where
    /// the executable's path was.
    command: String,
    /// See [`LlmBackend::identity`].
    identity: String,
    /// Where the child runs.
    ///
    /// The prompt hands the model `git diff <base> <head> -- <path>` with paths
    /// as the document records them, which is relative to the repository root.
    /// Git resolves a bare pathspec against the **current directory**, not the
    /// root, so a child inheriting `dfr`'s cwd matches nothing whenever `dfr`
    /// was run from a subdirectory — and matching nothing is an empty diff and
    /// exit 0, not an error. The model would then rate a class having seen no
    /// diff at all, and nothing anywhere would say so.
    ///
    /// `None` means inherit, which is right for a child that reads no repository
    /// (the tests here, and any future backend that takes its whole input on
    /// stdin).
    working_dir: Option<PathBuf>,
    /// Set from another thread to kill an in-flight child (a reviewer
    /// abandoning the wait). Without this the subprocess would outlive the
    /// process that asked for it, up to the whole timeout.
    cancel: Option<Arc<AtomicBool>>,
}

impl CommandBackend {
    /// A backend named by its own command line.
    ///
    /// The named constructors below are the production path; this is for a
    /// backend with nothing better to call itself, which in practice means a
    /// test double.
    pub fn new(argv: Vec<String>, timeout: Duration) -> Self {
        assert!(!argv.is_empty(), "CommandBackend needs a program to run");
        let command = argv.join(" ");
        CommandBackend {
            argv,
            timeout,
            name: command.clone(),
            identity: command.clone(),
            command,
            working_dir: None,
            cancel: None,
        }
    }

    /// Run the child in `dir`.
    ///
    /// The repository root, for any backend whose prompt names repo-relative
    /// paths — which the default one does. See the field for what goes wrong
    /// without it, and why it goes wrong silently.
    pub fn with_working_dir(mut self, dir: &Path) -> Self {
        self.working_dir = Some(dir.to_path_buf());
        self
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

    /// The default: headless, text output, and read-only tools (ADR 0022).
    ///
    /// ADR 0010 denied tools outright, because the evaluated grouping tool kept
    /// exiting 1 on `stop_reason: "tool_use"`. Denying them cured it by sending
    /// no tool definitions at all, so the model could not ask. An allowlist is
    /// the other cure: it can ask, and the answer is yes.
    ///
    /// `fetch` is the executable the prompt tells the model to run — normally
    /// this process. The allowlist is derived from it, so the two cannot
    /// disagree about what the model is allowed to invoke.
    ///
    /// Nothing here can write. The fetch command reads the document the engine
    /// just wrote; the rest read the repository. `git log` and `git show` are
    /// what reach the *reason* a change was made, which no prompt can carry.
    ///
    /// **`git diff` is advertised; the rest are not.** The prompt names the
    /// fetch command and `git diff`, and nothing else.
    ///
    /// That is a change of rule, and it is worth saying why. `git diff` is
    /// advertised because it is now the only way to see what a hunk says: the
    /// fetch command's `diff` query is gone, having duplicated `class` except
    /// for the text. A tool the model must use and is not told about is a tool
    /// it will not use.
    ///
    /// It costs an invitation to read the whole repository, and the prompt is
    /// what pays for that: it says to read what decides a label and then stop.
    ///
    /// It no longer costs a route around the generated content this stage folds
    /// away, though it did when it was written. `generated` is part of the
    /// shape-class key now (ADR 0004), so no class the model is given contains
    /// a generated file and there is nothing folded left for it to ask
    /// `git diff` about by accident. The prompt still says not to go looking.
    ///
    /// `Read`, `Grep`, `Glob`, `git log` and `git show` stay unadvertised for
    /// the original reason: a model that needs the code around a hunk can go
    /// and read it, but it is not sent looking. If you add a tool here, do not
    /// add a line about it to the prompt.
    ///
    /// The allowlist is this function's business, not the user's, and there is
    /// no config that replaces it. `[grouping].agent` picks between agents by
    /// name; it used to take a free argv, which handed a stranger's process the
    /// prompt and none of the allowlist, fetch command or read path the prompt
    /// is written for.
    ///
    /// `fetch` is where a binary lives, so it is the one part of this argv that
    /// says nothing about what the model will do. The cache identity stands a
    /// placeholder in its place: change the allowlist and every cached grouping
    /// is rightly invalidated, move the binary and none of them are.
    pub fn claude_cli(fetch: &str) -> Self {
        let mut b = Self::new(Self::claude_argv(fetch), Duration::from_secs(1200));
        b.name = "Claude Code".to_string();
        b.identity = Self::claude_argv("<fetch>").join(" ");
        b
    }

    fn claude_argv(fetch: &str) -> Vec<String> {
        vec![
            "claude".to_string(),
            "-p".to_string(),
            "--output-format".to_string(),
            "text".to_string(),
            "--allowed-tools".to_string(),
            format!(
                "Bash({fetch} agent:*),Bash(git diff:*),Read,Grep,Glob,\
                 Bash(git log:*),Bash(git show:*)"
            ),
        ]
    }
}

impl LlmBackend for CommandBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn identity(&self) -> &str {
        &self.identity
    }

    fn complete(&self, prompt: &str) -> Result<String, LlmError> {
        let mut cmd = Command::new(&self.argv[0]);
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }
        let mut child = cmd
            .args(&self.argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| LlmError::Spawn {
                command: self.command.clone(),
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
                command: self.command.clone(),
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
                        command: self.command.clone(),
                    });
                }
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = writer.join();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(LlmError::Timeout {
                        command: self.command.clone(),
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
                command: self.command.clone(),
                source,
            })?;
        let stderr = stderr_reader.join().expect("stderr reader panicked");

        if !status.success() {
            return Err(LlmError::Failed {
                command: self.command.clone(),
                code: status.code(),
                stderr: String::from_utf8_lossy(&stderr[..stderr.len().min(600)]).into_owned(),
            });
        }
        let text = String::from_utf8_lossy(&stdout).into_owned();
        if text.trim().is_empty() {
            return Err(LlmError::Empty {
                command: self.command.clone(),
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
    fn where_the_binary_lives_is_not_part_of_the_cache_identity() {
        // The grouping cache key hashes `identity`. If it hashed the argv the
        // absolute path would be in the key, and a debug build, a release build
        // and a second checkout of the same commit would each re-run a
        // four-hundred-second call over an identical class partition.
        let a = CommandBackend::claude_cli("/Users/someone/.cargo/bin/dfr");
        let b = CommandBackend::claude_cli("/srv/ci/target/release/dfr");
        assert_eq!(a.identity(), b.identity());
        assert!(!a.identity().contains(".cargo"), "{}", a.identity());

        // A backend with nothing better to call itself is its own identity, and
        // two different agents must never share a cache entry.
        let one = CommandBackend::new(vec!["agent-one".into()], Duration::from_secs(1));
        let two = CommandBackend::new(vec!["agent-two".into()], Duration::from_secs(1));
        assert_eq!(one.identity(), one.name());
        assert_ne!(one.identity(), two.identity());
    }

    #[test]
    fn the_child_runs_where_it_was_told_to() {
        // The prompt hands the model repo-root-relative paths for `git diff`.
        // Git resolves a bare pathspec against the CURRENT DIRECTORY, so a child
        // inheriting this process's cwd matches nothing whenever `dfr` ran from
        // a subdirectory — and matching nothing is an empty diff and exit 0, not
        // an error. The model would rate a class having seen no diff, and
        // nothing would say so. Hence a test on the cwd itself.
        let dir = tempfile::TempDir::new().unwrap();
        // The temp dir may be a symlink (/var -> /private/var on macOS), so
        // compare what the child reports against the canonical form.
        let want = dir.path().canonicalize().unwrap();
        let b = CommandBackend::new(vec!["pwd".into()], Duration::from_secs(10))
            .with_working_dir(dir.path());
        let got = b.complete("x").unwrap();
        assert_eq!(
            std::path::Path::new(got.trim()).canonicalize().unwrap(),
            want,
            "the child must run in the directory it was given"
        );

        // Without it, the child inherits — which is right for a backend that
        // reads no repository, and wrong for one whose prompt names paths.
        let inherit = CommandBackend::new(vec!["pwd".into()], Duration::from_secs(10));
        assert_ne!(
            std::path::Path::new(inherit.complete("x").unwrap().trim())
                .canonicalize()
                .unwrap(),
            want
        );
    }

    #[test]
    fn the_reviewer_sees_a_product_name_and_an_error_sees_the_command() {
        // The splash prints `name` on one line. The argv is four times the
        // width and answers a different question, so it lives where it is the
        // answer: a spawn failure.
        let b = CommandBackend::claude_cli("/opt/bin/dfr");
        assert_eq!(b.name(), "Claude Code");

        let missing = CommandBackend::new(
            vec!["definitely-not-a-real-program".into()],
            Duration::from_secs(1),
        );
        match missing.complete("x") {
            Err(LlmError::Spawn { command, .. }) => {
                assert_eq!(command, "definitely-not-a-real-program");
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[test]
    fn changing_the_allowlist_does_change_the_cache_identity() {
        // The other half of the rule: the allowlist shapes what the model can
        // see, so it must stay in the key even though the path does not.
        let b = CommandBackend::claude_cli("/opt/bin/dfr");
        assert!(b.identity().contains("Read,Grep,Glob"), "{}", b.identity());
        assert!(!b.identity().contains("/opt/bin"), "{}", b.identity());
    }

    #[test]
    fn claude_cli_default_allows_reading_and_nothing_else() {
        let b = CommandBackend::claude_cli("/opt/bin/dfr");
        let argv = &b.command;
        assert!(
            argv.contains("Bash(/opt/bin/dfr agent:*)"),
            "the allowlist names the same executable the prompt does"
        );
        assert!(
            argv.contains("Bash(git diff:*)"),
            "the prompt tells the model to run git diff, so it must be permitted"
        );
        // The whole list, exactly. The argv is built with a line continuation,
        // and a stray space inside one would produce an allowlist that parses
        // as something else. This is the security boundary, and a broken fetch
        // costs minutes of a model working around it, so it fails here loudly
        // rather than there silently.
        assert!(
            argv.ends_with(
                "--allowed-tools Bash(/opt/bin/dfr agent:*),Bash(git diff:*),Read,Grep,Glob,Bash(git log:*),Bash(git show:*)"
            ),
            "{argv}"
        );
        // The allowlist is the security boundary, so the test states what must
        // stay OUT of it, not merely what is in it.
        for forbidden in [
            "Write",
            "Edit",
            "Bash(git commit",
            "Bash(git push",
            "WebFetch",
        ] {
            assert!(!argv.contains(forbidden), "{forbidden} must not be allowed");
        }
    }
}
