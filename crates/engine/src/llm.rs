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
//!
//! There are five agents, one constructor each, and the trait did not move to
//! make room for them (ADR 0032). What differs between them is the argv, and
//! the part of the argv that matters is how each one is stopped from writing:
//!
//! - **An allowlist** — `claude_cli`, `copilot_cli`. The agent may run the
//!   named tools and nothing else, and the fetch command is one of them.
//! - **An OS sandbox** — `codex_cli`, `droid_cli`. The agent may run anything
//!   and the kernel refuses the writes, so there is no allowlist to derive and
//!   `fetch` does not appear in the argv.
//! - **Nothing** — `pi_cli`. Pi ships no sandbox and no per-command allowlist,
//!   and the shell tool it needs to fetch is the one that also lets it write.
//!   That is a decision with a reason, recorded in ADR 0032 and in the
//!   constructor, and `config::Agent::read_only` is how a caller
//!   tells a user about it.
//!
//! Adding a sixth means a constructor here, a variant in `config::Agent`, and
//! an arm in the application layer's `backend_from`. The compiler asks for the
//! last two; this comment is the only thing that asks for the boundary.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::subprocess;

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
    ///
    /// **`--permission-mode default` is what makes the allowlist mean anything,
    /// and it was missing for two releases** (ADR 0032). `--allowed-tools` ADDS
    /// permissions; it does not cap them. A user whose own settings set
    /// `defaultMode` to `auto`, `acceptEdits` or `bypassPermissions` was
    /// handing this call an agent that could write, commit and push, and
    /// nothing anywhere said so. `default` means ask, and a headless call has
    /// nobody to ask, so the answer is no.
    ///
    /// It was found by `dfr agents --probe`, on the first run, against the
    /// agent that had shipped as the only option. That is the whole argument
    /// for the probe existing.
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
            "--permission-mode".to_string(),
            "default".to_string(),
            "--allowed-tools".to_string(),
            format!(
                "Bash({fetch} agent:*),Bash(git diff:*),Read,Grep,Glob,\
                 Bash(git log:*),Bash(git show:*)"
            ),
        ]
    }

    /// Headless `codex exec`, read-only by OS sandbox (ADR 0032).
    ///
    /// Codex has no tool allowlist and needs none: `--sandbox read-only` is
    /// enforced by the kernel — Seatbelt on macOS, bubblewrap on Linux — so the
    /// model may run any command it likes and the writes are refused beneath
    /// it. That is a different boundary from Claude Code's and an equally real
    /// one, which is why `fetch` does not appear in this argv at all. The
    /// prompt still names the fetch command; nothing has to permit it.
    ///
    /// `--ask-for-approval never` is the headless half. Without it a command
    /// the sandbox refuses escalates to a human who is not there, and the call
    /// sits until the deadline kills it. With it the refusal returns to the
    /// model as a tool failure, which is what we want it to see.
    ///
    /// `--color never` keeps stdout clean. The response parser takes the text
    /// between the first `{` and the last `}`, and an escape sequence inside
    /// that span is a parse error with a sample nobody can read.
    ///
    /// The trailing `-` makes stdin the whole prompt. Codex will otherwise
    /// treat stdin as context for an argv instruction, and there is no argv
    /// instruction here.
    ///
    /// Never pass `--full-auto`, `--yolo` or
    /// `--dangerously-bypass-approvals-and-sandbox`: each removes the boundary.
    ///
    /// `--ignore-user-config` and `--ignore-rules` are the same lesson Claude
    /// Code taught (ADR 0032): the sandbox a flag asks for is not the sandbox
    /// that runs if the user's own `config.toml` or execpolicy rules say
    /// otherwise. An argv that can be widened by a file this crate never reads
    /// is not a boundary, it is a request.
    pub fn codex_cli() -> Self {
        let mut b = Self::new(Self::codex_argv(), Duration::from_secs(1200));
        b.name = "Codex".to_string();
        b.identity = Self::codex_argv().join(" ");
        b
    }

    fn codex_argv() -> Vec<String> {
        vec![
            "codex".to_string(),
            "exec".to_string(),
            "--ignore-user-config".to_string(),
            "--ignore-rules".to_string(),
            "--sandbox".to_string(),
            "read-only".to_string(),
            "--ask-for-approval".to_string(),
            "never".to_string(),
            "--color".to_string(),
            "never".to_string(),
            "-".to_string(),
        ]
    }

    /// Headless `droid exec`, read-only by default (ADR 0032).
    ///
    /// Droid is the one agent whose boundary is what this function does NOT
    /// pass. Its documented default is read-only file inspection plus git read
    /// operations, with file edits, package installs and git writes blocked,
    /// and a blocked action fails rather than asking — so a bare `droid exec`
    /// neither writes nor stalls.
    ///
    /// Never pass `--auto` at any level, and never
    /// `--skip-permissions-unsafe`. Each is the whole boundary, given away.
    ///
    /// `-o text` prints the final message only. `-` makes stdin the prompt.
    pub fn droid_cli() -> Self {
        let mut b = Self::new(Self::droid_argv(), Duration::from_secs(1200));
        b.name = "Droid".to_string();
        b.identity = Self::droid_argv().join(" ");
        b
    }

    fn droid_argv() -> Vec<String> {
        vec![
            "droid".to_string(),
            "exec".to_string(),
            "-o".to_string(),
            "text".to_string(),
            "-".to_string(),
        ]
    }

    /// Headless `copilot`, read-only by allowlist and an explicit deny
    /// (ADR 0032).
    ///
    /// The closest of the five to Claude Code: an allowlist derived from
    /// `fetch`, so the prompt can never name a command the model may not run.
    ///
    /// **There is deliberately no `-p`.** Copilot reads the prompt from stdin,
    /// and its own documentation says piped input is ignored when `-p` is
    /// given. Passing both would send an empty prompt and waste a call.
    ///
    /// `-s` suppresses the session decoration around the reply, for the same
    /// reason Codex gets `--color never`. `--no-ask-user` stops the agent
    /// pausing for a human who is not there.
    ///
    /// `--deny-tool write` is belt and braces: `write` is already absent from
    /// the allowlist, and a deny takes precedence over any allow, so the two
    /// cannot be talked out of agreeing.
    ///
    /// Never pass `--allow-all-tools` or `--allow-all-paths`.
    pub fn copilot_cli(fetch: &str) -> Self {
        let mut b = Self::new(Self::copilot_argv(fetch), Duration::from_secs(1200));
        b.name = "GitHub Copilot".to_string();
        b.identity = Self::copilot_argv("<fetch>").join(" ");
        b
    }

    fn copilot_argv(fetch: &str) -> Vec<String> {
        vec![
            "copilot".to_string(),
            "-s".to_string(),
            "--no-ask-user".to_string(),
            "--deny-tool".to_string(),
            "write".to_string(),
            "--allow-tool".to_string(),
            format!("read,shell(git:*),shell({fetch}:*)"),
        ]
    }

    /// Headless `pi`. **Read-only is NOT enforced here** (ADR 0032).
    ///
    /// Every other constructor in this file hands the model a boundary. This
    /// one cannot, and the reason is Pi's design rather than an oversight in
    /// this argv.
    ///
    /// Pi ships no sandbox, no per-command allowlist and no approval prompts.
    /// Its `-t` flag toggles whole tools, and `bash` is one tool: the model
    /// needs it to run the fetch command and `git diff`, and the same tool lets
    /// it write a file, commit or push. Nothing but the prompt asks it not to.
    ///
    /// Dropping `bash` would restore the boundary and take the change with it.
    /// The model would be back to grouping from class ids alone, which is the
    /// truncated payload ADR 0022 was written to end — a worse grouping, every
    /// time, in exchange for a risk the prompt never asks anyone to take.
    ///
    /// So the author chose this knowingly, and the duty that comes with it is
    /// disclosure: `Agent::read_only` answers `NotEnforced` for Pi, and
    /// every place that offers the name says so.
    ///
    /// The rest of the argv is hermetic sealing, and it is not decoration.
    /// `-nc` drops `AGENTS.md` and `CLAUDE.md`, `-na` drops the repository's
    /// own `.pi/` config, and `--no-extensions --no-skills` drop the user's.
    /// Each is a file outside the cache key that could otherwise change a
    /// grouping, which is the hole ADR 0022 names and cannot close.
    /// `--no-session` stops Pi writing a session file for a call nobody
    /// resumes.
    pub fn pi_cli() -> Self {
        let mut b = Self::new(Self::pi_argv(), Duration::from_secs(1200));
        b.name = "Pi".to_string();
        b.identity = Self::pi_argv().join(" ");
        b
    }

    fn pi_argv() -> Vec<String> {
        vec![
            "pi".to_string(),
            "-p".to_string(),
            "--mode".to_string(),
            "text".to_string(),
            "--no-session".to_string(),
            "-nc".to_string(),
            "-na".to_string(),
            "--no-extensions".to_string(),
            "--no-skills".to_string(),
            "-t".to_string(),
            "read,grep,find,ls,bash".to_string(),
        ]
    }

    /// The program this backend spawns, for a caller checking `PATH`.
    ///
    /// `dfr agents` says whether each agent is installed, and the answer has to
    /// come from the argv that will actually run rather than from a second list
    /// of executable names that could disagree with it.
    pub fn program(&self) -> &str {
        &self.argv[0]
    }

    /// The whole command line, for a caller showing what will run.
    ///
    /// Not [`LlmBackend::name`], which is a product name, and not
    /// [`LlmBackend::identity`], which stands a placeholder where the binary
    /// path is. This is the argv itself, and the two callers that want it are a
    /// spawn failure and `dfr agents`.
    pub fn command(&self) -> &str {
        &self.command
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
        let command = || self.command.clone();
        let out = subprocess::run(&subprocess::Run {
            argv: &self.argv,
            stdin: Some(prompt.as_bytes()),
            working_dir: self.working_dir.as_deref(),
            timeout: self.timeout,
            cancel: self.cancel.as_ref(),
        })
        .map_err(|f| match f {
            subprocess::Failure::Spawn(source) => LlmError::Spawn {
                command: command(),
                source,
            },
            subprocess::Failure::Io(source) => LlmError::Io {
                command: command(),
                source,
            },
            subprocess::Failure::Timeout => LlmError::Timeout {
                command: command(),
                timeout: self.timeout,
            },
            subprocess::Failure::Cancelled => LlmError::Cancelled { command: command() },
        })?;

        if !out.status.success() {
            return Err(LlmError::Failed {
                command: command(),
                code: out.status.code(),
                stderr: subprocess::stderr_excerpt(&out.stderr, 600),
            });
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if text.trim().is_empty() {
            return Err(LlmError::Empty { command: command() });
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

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
        // Without this the allowlist is advisory: `--allowed-tools` adds
        // permissions and does not cap them, so a user whose settings set
        // `defaultMode` to `auto` got an agent that could write. `dfr agents
        // --probe` caught it; this line is what stops it coming back.
        assert!(
            argv.contains("--permission-mode default"),
            "the allowlist only binds under the default permission mode: {argv}"
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

    /// Every backend this crate builds, for the tests that must hold across all
    /// of them. A new agent belongs here, and two of the tests below fail until
    /// it is.
    fn every_backend() -> Vec<(&'static str, CommandBackend)> {
        vec![
            ("claude-code", CommandBackend::claude_cli("/opt/bin/dfr")),
            ("codex", CommandBackend::codex_cli()),
            ("droid", CommandBackend::droid_cli()),
            ("copilot", CommandBackend::copilot_cli("/opt/bin/dfr")),
            ("pi", CommandBackend::pi_cli()),
        ]
    }

    #[test]
    fn codex_runs_sandboxed_and_never_stops_to_ask() {
        let b = CommandBackend::codex_cli();
        assert_eq!(b.name(), "Codex");
        assert_eq!(b.program(), "codex");
        // The whole argv, exactly. Codex has no allowlist to get wrong, so the
        // boundary IS these two flag pairs and nothing else says so.
        assert_eq!(
            b.command(),
            "codex exec --ignore-user-config --ignore-rules --sandbox read-only \
             --ask-for-approval never --color never -"
        );
        // A sandbox the user's own config can widen is not a sandbox. Same
        // lesson as `--permission-mode default` on Claude Code (ADR 0032).
        assert!(
            b.command().contains("--ignore-user-config"),
            "{}",
            b.command()
        );
        // `-` is what makes stdin the whole prompt rather than context for an
        // argv instruction that does not exist here. Without it Codex waits for
        // an instruction and the call is wasted.
        assert!(b.command().ends_with(" -"), "{}", b.command());
    }

    #[test]
    fn droid_is_read_only_because_of_what_it_does_not_pass() {
        let b = CommandBackend::droid_cli();
        assert_eq!(b.name(), "Droid");
        assert_eq!(b.program(), "droid");
        assert_eq!(b.command(), "droid exec -o text -");
        // Droid's default is read-only, so its boundary is an absence. A test
        // on presence would pass while the boundary was being given away, which
        // is why this one is written the other way round.
        assert!(!b.command().contains("--auto"), "{}", b.command());
    }

    #[test]
    fn copilot_allows_reading_and_the_fetch_command_and_nothing_else() {
        let b = CommandBackend::copilot_cli("/opt/bin/dfr");
        assert_eq!(b.name(), "GitHub Copilot");
        assert_eq!(b.program(), "copilot");
        assert!(
            b.command().contains("shell(/opt/bin/dfr:*)"),
            "the allowlist names the same executable the prompt does: {}",
            b.command()
        );
        assert!(
            b.command().contains("shell(git:*)"),
            "the prompt tells the model to run git diff, so it must be permitted"
        );
        // The whole list, exactly, for the reason the Claude one is pinned: a
        // stray character inside it produces an allowlist that parses as
        // something else, and it fails here loudly rather than there silently.
        assert!(
            b.command().ends_with(
                "--deny-tool write --allow-tool read,shell(git:*),shell(/opt/bin/dfr:*)"
            ),
            "{}",
            b.command()
        );
        // Copilot ignores piped input when `-p` is given, and the prompt only
        // ever arrives on stdin. A `-p` here would send an empty prompt.
        assert!(
            !b.command().contains(" -p"),
            "the prompt comes from stdin: {}",
            b.command()
        );
        assert!(b.command().contains("--no-ask-user"), "{}", b.command());
    }

    #[test]
    fn pi_is_the_one_agent_that_can_write_and_says_so() {
        // This test states an exception, not a requirement. Pi ships no sandbox
        // and no per-command allowlist, so the shell tool it needs to run the
        // fetch command is the same tool that lets it write (ADR 0032).
        //
        // `bash` being present is therefore the decision, and pinning it here is
        // what stops a later reader "fixing" it and silently taking the fetch
        // command away — which does not fail, it just groups worse.
        let b = CommandBackend::pi_cli();
        assert_eq!(b.name(), "Pi");
        assert_eq!(b.program(), "pi");
        assert!(
            b.command().contains("-t read,grep,find,ls,bash"),
            "pi needs bash to fetch; removing it removes the change, not the risk: {}",
            b.command()
        );
        assert!(
            !b.command().contains("edit") && !b.command().contains("write"),
            "the write tools stay off even though bash makes that a courtesy: {}",
            b.command()
        );
        // The hermetic flags are not decoration. Each one is a file outside the
        // cache key that could otherwise change a grouping.
        for flag in [
            "-nc",
            "-na",
            "--no-extensions",
            "--no-skills",
            "--no-session",
        ] {
            assert!(
                b.command().contains(flag),
                "{flag} missing: {}",
                b.command()
            );
        }
    }

    #[test]
    fn no_agent_is_given_a_flag_that_removes_its_boundary() {
        // One list, every agent. These are the flags each CLI offers for
        // turning its own protection off, and none of them may ever appear in
        // an argv this crate writes. A new agent is covered the moment it joins
        // `every_backend`.
        const FORBIDDEN: [&str; 8] = [
            "--yolo",
            "--full-auto",
            "--dangerously-bypass-approvals-and-sandbox",
            "--dangerously-allow-all",
            "--skip-permissions-unsafe",
            "--allow-all-tools",
            "--allow-all-paths",
            "--auto",
        ];
        for (key, b) in every_backend() {
            for flag in FORBIDDEN {
                assert!(
                    !b.command().contains(flag),
                    "{key} must never be given {flag}: {}",
                    b.command()
                );
            }
        }
    }

    #[test]
    fn no_agent_takes_its_prompt_or_its_working_directory_in_the_argv() {
        // Two rules that hold across all five.
        //
        // The prompt goes on stdin, always: `complete` writes it there and
        // passes no argument. An agent given an argv prompt would read an empty
        // one.
        //
        // The working directory comes from `with_working_dir`, never from a
        // flag. A path in the argv lands in the cache identity, and then a
        // debug build, a release build and a second checkout each re-run a
        // four-hundred-second call over an identical class partition.
        for (key, b) in every_backend() {
            for flag in ["--cwd", "--workspace", "--dir", "-C "] {
                assert!(
                    !b.command().contains(flag),
                    "{key} must take its directory from with_working_dir, not {flag}"
                );
            }
            assert!(
                !b.identity().contains("/opt/bin"),
                "{key} put a binary path in its cache identity: {}",
                b.identity()
            );
        }
    }

    #[test]
    fn no_two_agents_share_a_cache_identity() {
        // Two agents sharing an identity share a cache entry, so one would
        // serve the other's grouping under a name it never ran.
        let all = every_backend();
        for (i, (key_a, a)) in all.iter().enumerate() {
            for (key_b, b) in all.iter().skip(i + 1) {
                assert_ne!(
                    a.identity(),
                    b.identity(),
                    "{key_a} and {key_b} share a cache identity"
                );
                assert_ne!(a.name(), b.name(), "{key_a} and {key_b} share a name");
            }
        }
    }
}
