//! `dfr agents` — which agents exist, which one you have, and whether it works.
//!
//! Five agents ship (ADR 0032), and not one of their command lines can be
//! proved by a test in this repository. A test asserts the argv this crate
//! writes; whether the CLI on the other end accepts that argv, reads its prompt
//! from stdin, prints plain text and refuses to write is a fact about a program
//! that is not installed here and differs by version. CI cannot answer it and
//! neither can the author, who has one machine.
//!
//! So the answer moves to where the CLI actually is. `dfr agents` lists them and
//! costs nothing. `dfr agents --probe` runs one real call and reports four
//! facts, and it is the only thing in this repository that can tell you an argv
//! is right.
//!
//! The four facts are the four ways this goes wrong, and each one is quiet:
//!
//! - **Spawn.** The executable is missing or renamed. Loud already — this is
//!   here for completeness.
//! - **Stdin.** The agent took the prompt from argv, or printed a JSON envelope,
//!   or wrapped the reply in decoration. The grouping call would then fail to
//!   parse, minutes in, with a 300-character sample.
//! - **Fetch.** The agent could not run `<fetch> agent`. This is the expensive
//!   one and ADR 0022 says so: a model does not give up on a failing tool, it
//!   retries and works around it, and only then falls back to the class ids. The
//!   run succeeds. The grouping is quietly worse.
//! - **Write.** The boundary is gone. Nothing else would ever tell you.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use differential_engine::config::{Agent, ReadOnly};
use differential_engine::llm::{CommandBackend, LlmBackend};
use differential_engine::schema::{
    Audit, ClassEntry, Disposition, FileEntry, ForgePosition, Generator, HunkEntry, PlanDocument,
    SCHEMA_VERSION, Source, SourceKind, Stats,
};

/// What the listing says about one agent.
///
/// A struct rather than a formatted line, so the test can assert the facts
/// without owning the column widths.
pub struct Row {
    pub agent: Agent,
    /// The product name a reviewer sees on the splash.
    pub display: String,
    /// The executable, and whether it is on `PATH`.
    pub program: String,
    pub installed: bool,
    pub read_only: ReadOnly,
    pub configured: bool,
}

/// Every agent, with what this machine has.
pub fn rows(configured: Agent, backend_for: impl Fn(Agent) -> CommandBackend) -> Vec<Row> {
    Agent::ALL
        .iter()
        .map(|&agent| {
            let backend = backend_for(agent);
            let program = backend.program().to_string();
            Row {
                agent,
                display: backend.name().to_string(),
                installed: which::which(&program).is_ok(),
                program,
                read_only: agent.read_only(),
                configured: agent == configured,
            }
        })
        .collect()
}

/// How a `ReadOnly` reads to someone choosing an agent.
///
/// The unenforced one gets a sentence rather than a label. A reader skimming a
/// column of four tidy phrases will skim past a fifth, and this is the one line
/// on the screen that changes what the agent may do to their repository.
fn read_only_words(r: ReadOnly) -> &'static str {
    match r {
        ReadOnly::ToolAllowlist => "read-only: tool allowlist",
        ReadOnly::OsSandbox => "read-only: OS sandbox",
        ReadOnly::AgentDefault => "read-only: the agent's default",
        ReadOnly::NotEnforced => "NOT read-only: it can write, commit and push",
    }
}

/// The listing. Costs nothing and opens no repository.
pub fn list(rows: &[Row]) -> String {
    let mut out = String::new();
    let w_key = rows.iter().map(|r| r.agent.key().len()).max().unwrap_or(0);
    let w_name = rows.iter().map(|r| r.display.len()).max().unwrap_or(0);
    let w_prog = rows.iter().map(|r| r.program.len()).max().unwrap_or(0);
    let w_state = "not found".len();

    out.push_str("Agents. * is the configured one; set it with [grouping].agent\n");
    out.push_str("in ~/.config/differential/config.toml.\n\n");
    for r in rows {
        let mark = if r.configured { '*' } else { ' ' };
        // The executable is its own column because the name a user configures
        // and the binary they have to install are not always the same word:
        // `claude-code` is the agent, `claude` is the command.
        let state = if r.installed { "found" } else { "not found" };
        let _ = writeln!(
            out,
            "{mark} {:w_key$}  {:w_name$}  {:w_prog$}  {:w_state$}  {}",
            r.agent.key(),
            r.display,
            r.program,
            state,
            read_only_words(r.read_only),
        );
    }
    out.push_str(
        "\nAn argv here is written from each agent's documentation, and this\n\
         repository cannot test it against a CLI it does not have. Run\n\
         `dfr agents --probe <name>` on a machine that has one: it makes one\n\
         real model call and reports whether the agent read its prompt, found\n\
         the fetch command, and was refused a write.\n",
    );
    out
}

/// One check inside a probe.
pub struct Check {
    pub what: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// What a probe found.
pub struct Report {
    pub agent: Agent,
    pub command: String,
    pub elapsed: Duration,
    pub checks: Vec<Check>,
    /// The agent's whole reply, shown only when it failed a check — that is
    /// when it is the thing a reader needs and never otherwise.
    ///
    /// Empty when the agent never replied at all. A spawn failure already puts
    /// its whole message in the check beside it, and printing the same sentence
    /// twice under a heading promising the agent's words is worse than not
    /// printing it.
    pub reply: String,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
}

/// Render a report.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} — {}", report.agent.key(), report.command);
    let w = report
        .checks
        .iter()
        .map(|c| c.what.len())
        .max()
        .unwrap_or(0);
    for c in &report.checks {
        let mark = if c.ok { "ok  " } else { "FAIL" };
        let _ = writeln!(out, "  {:w$}  {mark}  {}", c.what, c.detail);
    }
    let _ = writeln!(out, "\n  {:.1}s", report.elapsed.as_secs_f64());
    if !report.passed() && !report.reply.trim().is_empty() {
        let _ = writeln!(
            out,
            "\nWhat the agent replied:\n{}",
            report.reply.trim_end()
        );
    }
    out
}

/// The prompt. Short on purpose: this is a self-test, and every sentence is a
/// sentence the agent reads before it can start.
///
/// Step 2 is phrased to stop the agent helping. An agent told to write a file
/// and refused will, left to itself, try another path, ask why, or report a
/// problem — all of which cost a turn and none of which is the answer. Saying
/// the refusal is the correct result is what keeps the probe to one round.
const PROBE_PROMPT: &str = "\
This is a self-test of your tool access, not a coding task. Do two things.

1. Run this command and read what it prints:

     {{FETCH}} agent --doc {{DOC}}

   It describes one shape class, which touches exactly one file. Note that
   file's path.

2. Try to create a file at this exact path, containing any text:

     {{WRITE}}

   You are expected to be refused. A refusal is the correct result: do not
   retry, do not work around it, and do not report it as a problem.

Then reply with ONLY this JSON object. No prose and no code fence.

{\"nonce\": \"{{NONCE}}\", \"file\": \"<the path from step 1, or none>\"}
";

/// Run one probe. Makes one real model call.
///
/// `fetch` is the executable the prompt names, the same one the grouping stage
/// would name. `backend` is the agent's invocation, already built by the same
/// function the pipeline uses — a probe against a command the pipeline would not
/// run tells you nothing.
pub fn probe(
    agent: Agent,
    backend: CommandBackend,
    fetch: &str,
    timeout: Duration,
) -> anyhow::Result<Report> {
    // PATH first, because it is free and the answer is different. A missing
    // executable is "you have not installed this agent", which no amount of
    // argv work fixes, and it should not read like a failing command line.
    if which::which(backend.program()).is_err() {
        return Ok(Report {
            agent,
            command: backend.command().to_string(),
            elapsed: Duration::ZERO,
            checks: vec![Check {
                what: "install",
                ok: false,
                detail: format!(
                    "{} is not on PATH, so there is nothing to probe",
                    backend.program()
                ),
            }],
            reply: String::new(),
        });
    }

    // The temp directory is the source of the nonce as well as the place the
    // probe writes. `tempfile` names it with a proper random suffix, so the
    // nonce is unguessable without hand-rolling randomness for it (design rule
    // 5), and it is gone when this function returns.
    let dir = tempfile::TempDir::new()?;
    // `tempfile` names the directory `.tmpXXXXXX`; the random tail is the part
    // worth keeping, and it makes a tidier marker filename than the whole thing.
    let nonce = dir
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().trim_start_matches(".tmp").to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "probe".to_string());

    // The marker file's name is the thing only a reader of the document can
    // know. A class id would not do: they are `C0..Cn` and a model that never
    // ran the command can guess `C0` and pass a check it failed.
    let marker = format!("probe-{nonce}.rs");
    let doc_path = dir.path().join("document.json");
    std::fs::write(&doc_path, probe_document(&marker).to_json_pretty()?)?;
    let write_path = dir.path().join("the-agent-should-not-write-this");

    let prompt = PROBE_PROMPT
        .replace("{{FETCH}}", fetch)
        .replace("{{DOC}}", &doc_path.display().to_string())
        .replace("{{WRITE}}", &write_path.display().to_string())
        .replace("{{NONCE}}", &nonce);

    let command = backend.command().to_string();
    let started = Instant::now();
    let outcome = backend.with_timeout(timeout).complete(&prompt);
    let elapsed = started.elapsed();

    let reply = outcome.as_deref().unwrap_or_default().to_string();
    let checks = judge(agent, &outcome, &nonce, &marker, &write_path);

    Ok(Report {
        agent,
        command,
        elapsed,
        checks,
        reply,
    })
}

/// Turn one reply into four verdicts.
///
/// Split out because this is the part worth testing, and it needs no
/// subprocess: the tests below feed it replies a real agent might produce.
fn judge(
    agent: Agent,
    outcome: &Result<String, differential_engine::llm::LlmError>,
    nonce: &str,
    marker: &str,
    write_path: &Path,
) -> Vec<Check> {
    let text = match outcome {
        Ok(t) => t.as_str(),
        Err(e) => {
            // A spawn failure ends the probe: the other three checks have
            // nothing to look at, and reporting them as failures would blame
            // the argv for a missing binary.
            return vec![Check {
                what: "spawn",
                ok: false,
                detail: e.to_string(),
            }];
        }
    };

    let mut checks = vec![Check {
        what: "spawn",
        ok: true,
        detail: "the agent ran".to_string(),
    }];

    // The reply is parsed exactly as a grouping reply is: first `{` to last
    // `}`. That is deliberate. A probe that parsed more leniently than the
    // pipeline would pass an agent the pipeline then rejects.
    let json = extract_json(text);
    let parsed: Option<serde_json::Value> = json.and_then(|s| serde_json::from_str(s).ok());

    let got_nonce = parsed
        .as_ref()
        .and_then(|v| v.get("nonce"))
        .and_then(|v| v.as_str());
    checks.push(match got_nonce {
        Some(n) if n == nonce => Check {
            what: "stdin",
            ok: true,
            detail: "the reply carried the nonce from the prompt".to_string(),
        },
        Some(n) => Check {
            what: "stdin",
            ok: false,
            detail: format!("the reply carried {n:?}, not the nonce from the prompt"),
        },
        None => Check {
            what: "stdin",
            ok: false,
            detail: "no JSON object with a nonce: the agent may not have read \
                     stdin, or may not print plain text"
                .to_string(),
        },
    });

    let got_file = parsed
        .as_ref()
        .and_then(|v| v.get("file"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    checks.push(if got_file.contains(marker) {
        Check {
            what: "fetch",
            ok: true,
            detail: format!("it read the document and named {marker}"),
        }
    } else {
        Check {
            what: "fetch",
            ok: false,
            detail: format!(
                "it did not name {marker}, so it could not run the fetch \
                 command. A grouping run would not fail — it would group from \
                 the class ids alone and say nothing"
            ),
        }
    });

    // The filesystem decides this one, never the agent's account of it. An
    // agent that believes it was refused and wrote the file anyway is exactly
    // the failure worth catching.
    let wrote = write_path.exists();
    checks.push(match (wrote, agent.read_only().is_enforced()) {
        (false, true) => Check {
            what: "write",
            ok: true,
            detail: "the write was refused".to_string(),
        },
        (true, true) => Check {
            what: "write",
            ok: false,
            detail: "THE AGENT WROTE THE FILE. This agent is supposed to be \
                     read-only and is not. Do not use it until the argv is \
                     fixed"
                .to_string(),
        },
        (true, false) => Check {
            what: "write",
            ok: true,
            detail: "the agent wrote the file, as documented: this agent is \
                     not read-only (ADR 0032)"
                .to_string(),
        },
        (false, false) => Check {
            what: "write",
            ok: true,
            detail: "no file was written this time, but nothing stops it: \
                     this agent is not read-only (ADR 0032)"
                .to_string(),
        },
    });

    checks
}

/// First `{` to last `}`, the same span the grouping parser takes.
fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then(|| &text[start..=end])
}

/// A one-class, one-hunk document for the probe to fetch.
///
/// Built from the schema types rather than written as JSON, so a schema change
/// fails to compile here instead of failing to parse on a user's machine
/// halfway through a paid call.
fn probe_document(marker: &str) -> PlanDocument {
    PlanDocument {
        schema_version: SCHEMA_VERSION,
        generator: Generator {
            tool: "differential".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            stages: vec!["enumerate".to_string(), "classify".to_string()],
        },
        source: Source {
            kind: SourceKind::Range,
            base: "0".repeat(40),
            head: "1".repeat(40),
            remote: None,
        },
        stats: Stats {
            files: 1,
            hunks: 1,
            classes: 1,
            binary_files: 0,
            submodules: 0,
        },
        files: vec![FileEntry {
            path: marker.to_string(),
            disposition: Disposition::M,
            mode: Some("100644".to_string()),
            old_mode: None,
            old_path: None,
            new_path: None,
            rename_similarity: None,
            binary: false,
            submodule: None,
            generated: false,
            generated_by: None,
            hunk_ids: vec!["h0".to_string()],
        }],
        hunks: vec![HunkEntry {
            id: "h0".to_string(),
            file: marker.to_string(),
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            class: "C0".to_string(),
            digest: "0".repeat(40),
            nonl_old: false,
            nonl_new: false,
            forge_position: ForgePosition {
                new_line: Some(1),
                old_line: Some(1),
            },
        }],
        classes: vec![ClassEntry {
            id: "C0".to_string(),
            hunk_ids: vec!["h0".to_string()],
            exemplar: "h0".to_string(),
            pure_substitution: false,
            defines: Vec::new(),
            depends_on: Vec::new(),
        }],
        groups: None,
        reading_plan: None,
        audit: Audit {
            applier_exact: "1/1".to_string(),
            tree_assertion: "pass".to_string(),
            hunks_carried: 1,
            recount: 1,
            coverage: None,
            classes_missing: None,
            classes_duplicated: None,
            classes_hallucinated: None,
            read_hunks: None,
            skipped_hunks: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Result<String, differential_engine::llm::LlmError> {
        Ok(text.to_string())
    }

    #[test]
    fn a_good_reply_passes_every_check() {
        let dir = tempfile::TempDir::new().unwrap();
        let unwritten = dir.path().join("nope");
        let reply = r#"{"nonce": "abc", "file": "probe-abc.rs"}"#;
        let checks = judge(
            Agent::ClaudeCode,
            &ok(reply),
            "abc",
            "probe-abc.rs",
            &unwritten,
        );
        assert!(checks.iter().all(|c| c.ok), "{:?}", failures(&checks));
        assert_eq!(checks.len(), 4);
    }

    #[test]
    fn a_reply_without_the_nonce_fails_stdin() {
        // The agent that takes its prompt from argv rather than stdin replies
        // to nothing, or to a stale instruction. Either way the nonce is the
        // tell, because only a reader of THIS prompt has it.
        let dir = tempfile::TempDir::new().unwrap();
        let checks = judge(
            Agent::Codex,
            &ok(r#"{"nonce": "guessed", "file": "probe-abc.rs"}"#),
            "abc",
            "probe-abc.rs",
            &dir.path().join("nope"),
        );
        assert!(!checks.iter().find(|c| c.what == "stdin").unwrap().ok);
    }

    #[test]
    fn prose_around_the_json_is_tolerated_exactly_as_the_pipeline_tolerates_it() {
        // The probe must not be stricter OR looser than the grouping parser.
        // Looser passes an agent the pipeline will reject; stricter fails one
        // it would accept, and sends someone chasing a working argv.
        let dir = tempfile::TempDir::new().unwrap();
        let reply =
            "Sure! Here you go:\n```json\n{\"nonce\": \"abc\", \"file\": \"probe-abc.rs\"}\n```\n";
        let checks = judge(
            Agent::Droid,
            &ok(reply),
            "abc",
            "probe-abc.rs",
            &dir.path().join("nope"),
        );
        assert!(checks.iter().all(|c| c.ok), "{:?}", failures(&checks));
    }

    #[test]
    fn a_reply_that_never_ran_the_fetch_command_fails_fetch_and_nothing_else() {
        // This is the quiet one. The agent answered, the JSON is well formed,
        // the nonce is right — and it never saw the document. A grouping run
        // shaped like this succeeds and groups worse, which is why the probe
        // has to say so.
        let dir = tempfile::TempDir::new().unwrap();
        let checks = judge(
            Agent::ClaudeCode,
            &ok(r#"{"nonce": "abc", "file": "none"}"#),
            "abc",
            "probe-abc.rs",
            &dir.path().join("nope"),
        );
        assert!(checks.iter().find(|c| c.what == "stdin").unwrap().ok);
        assert!(!checks.iter().find(|c| c.what == "fetch").unwrap().ok);
        assert!(checks.iter().find(|c| c.what == "write").unwrap().ok);
    }

    #[test]
    fn a_write_by_an_enforced_agent_fails_and_the_same_write_by_pi_does_not() {
        // The one check the agent's own account never decides. Both runs below
        // produce the same file on disk; what differs is what the agent
        // promised, and the verdict follows the promise.
        let dir = tempfile::TempDir::new().unwrap();
        let written = dir.path().join("written");
        std::fs::write(&written, "the agent got through").unwrap();
        let reply = r#"{"nonce": "abc", "file": "probe-abc.rs"}"#;

        let enforced = judge(Agent::Codex, &ok(reply), "abc", "probe-abc.rs", &written);
        let check = enforced.iter().find(|c| c.what == "write").unwrap();
        assert!(!check.ok, "a sandboxed agent that writes is a failure");
        assert!(check.detail.contains("WROTE"), "{}", check.detail);

        let pi = judge(Agent::Pi, &ok(reply), "abc", "probe-abc.rs", &written);
        let check = pi.iter().find(|c| c.what == "write").unwrap();
        assert!(check.ok, "pi writing is documented, not a regression");
        assert!(check.detail.contains("not read-only"), "{}", check.detail);
    }

    #[test]
    fn a_spawn_failure_reports_one_check_and_blames_nothing_else() {
        let dir = tempfile::TempDir::new().unwrap();
        let err = Err(differential_engine::llm::LlmError::Spawn {
            command: "codex".to_string(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        });
        let checks = judge(
            Agent::Codex,
            &err,
            "abc",
            "probe-abc.rs",
            &dir.path().join("x"),
        );
        assert_eq!(checks.len(), 1, "the other three have nothing to look at");
        assert_eq!(checks[0].what, "spawn");
        assert!(!checks[0].ok);
    }

    #[test]
    fn the_probe_document_is_readable_by_the_fetch_command() {
        // The probe pays for a model call before anything reads this document.
        // A document `dfr agent` cannot parse would spend that call to learn
        // nothing, so it is parsed here for free.
        let doc = probe_document("probe-abc.rs");
        let json = doc.to_json_pretty().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("document.json");
        std::fs::write(&path, &json).unwrap();

        let rendered = crate::agent::run(&path).unwrap();
        assert!(rendered.contains("C0"), "{rendered}");
        assert!(
            rendered.contains("probe-abc.rs"),
            "the marker is what the agent is asked to report back: {rendered}"
        );
    }

    #[test]
    fn the_listing_marks_the_configured_agent_and_names_the_unenforced_one() {
        let rows = rows(Agent::Codex, |a| match a {
            Agent::ClaudeCode => CommandBackend::claude_cli("/opt/bin/dfr"),
            Agent::Codex => CommandBackend::codex_cli(),
            Agent::Droid => CommandBackend::droid_cli(),
            Agent::Copilot => CommandBackend::copilot_cli("/opt/bin/dfr"),
            Agent::Pi => CommandBackend::pi_cli(),
        });
        assert_eq!(rows.len(), Agent::ALL.len());
        let configured: Vec<&str> = rows
            .iter()
            .filter(|r| r.configured)
            .map(|r| r.agent.key())
            .collect();
        assert_eq!(configured, vec!["codex"]);

        let text = list(&rows);
        for agent in Agent::ALL {
            assert!(
                text.contains(agent.key()),
                "{} missing: {text}",
                agent.key()
            );
        }
        // The whole reason the listing exists rather than a docs paragraph.
        assert!(
            text.contains("NOT read-only: it can write, commit and push"),
            "{text}"
        );
        assert!(text.contains("--probe"), "{text}");
    }

    fn failures(checks: &[Check]) -> Vec<&str> {
        checks.iter().filter(|c| !c.ok).map(|c| c.what).collect()
    }
}
