//! The prompt: instructions, a path, and the class ids.
//!
//! It used to be the whole context. One block per shape class — a header, four
//! removed and four added lines from the **exemplar hunk only**, six basenames
//! — capped at 90,000 characters, with anything past the cap silently dropped
//! into the back-fill. So a class of nine hunks got rated "read one, trust the
//! rest" on the evidence of one hunk, and a large change lost classes to a
//! character count.
//!
//! Now the model fetches (ADR 0022). The engine writes the pre-group document
//! and the prompt says where it is, how to ask it questions, and how to read
//! the diff text with `git diff`. Nothing is truncated, because nothing is
//! sent.
//!
//! The class id list stays in the prompt: it is about a kilobyte for two
//! hundred classes, and it means a model whose fetches all fail still knows the
//! exact id set. A weak grouping is recoverable; a hallucinated one wastes the
//! audit's time telling us so.
//!
//! **The prompt is prose, so it lives in prose.** `prompt.txt` beside this file
//! is the whole text, read exactly as the model reads it. Editing it is a diff
//! of sentences rather than a diff of Rust string literals, which is what a
//! reviewer of a prompt actually needs to see. Cargo tracks `include_str!`, so
//! an edit still rebuilds, and `src/` ships with the crate.

use super::ClassInfo;

/// Feeds the cache key: bump on ANY change to the prompt text or the shape of
/// what the model can fetch, or cached groupings would silently mix prompt
/// generations.
pub const PROMPT_VERSION: u32 = 6;

/// The prompt, with `{{…}}` placeholders for the five run-specific values.
const PROMPT: &str = include_str!("prompt.txt");

/// Instructions, the commands, and the class id list.
///
/// Takes the executable, the artefact path and the range rather than the
/// document: which binary the model can run, where it should read, and which
/// two revisions `git diff` compares are all the caller's decisions. This
/// function only writes them down — and the backend's tool allowlist must be
/// built from the same `fetch`, or the model is told to run a command it is not
/// permitted to run.
///
/// The range is spelled into a whole `git diff` command rather than described.
/// The reader is an agent with a terminal: a command it can run beats an
/// instruction it has to assemble.
///
/// `base` and `head` are whatever `schema::Source` holds, which for a staged or
/// worktree review is a raw tree oid rather than a commit. `git diff` takes any
/// two tree-ish arguments, so the command is the same one either way — which is
/// the reason to pass the two strings through rather than reconstruct a range
/// spelling here.
///
/// Substitution is five `replace` calls, not a template engine. A dependency
/// for five calls would be a dependency for five calls.
pub fn build_prompt(
    offered: &[&ClassInfo],
    fetch: &str,
    artefact: &str,
    base: &str,
    head: &str,
) -> String {
    let mut order: Vec<&&ClassInfo> = offered.iter().collect();
    order.sort_by_key(|c| (usize::MAX - c.n_hunks, c.exemplar));
    let ids: Vec<&str> = order.iter().map(|c| c.id.as_str()).collect();

    PROMPT
        .replace("{{FETCH}}", fetch)
        .replace("{{DOC}}", artefact)
        .replace("{{BASE}}", base)
        .replace("{{HEAD}}", head)
        .replace("{{CLASS_IDS}}", &ids.join(" "))
}
