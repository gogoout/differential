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
//! and the prompt says where it is and how to ask it questions. Nothing is
//! truncated, because nothing is sent.
//!
//! The class id list stays in the prompt: it is about a kilobyte for two
//! hundred classes, and it means a model whose fetches all fail still knows the
//! exact id set. A weak grouping is recoverable; a hallucinated one wastes the
//! audit's time telling us so.

use super::ClassInfo;

/// Feeds the cache key: bump on ANY change to the prompt text or the shape of
/// what the model can fetch, or cached groupings would silently mix prompt
/// generations.
pub const PROMPT_VERSION: u32 = 3;

const PROMPT_HEAD: &str = r#"You are helping a reviewer read a large merge request faster.

A mechanical pass has already split every changed hunk into SHAPE CLASSES: hunks whose
diff text is identical after normalising away identifier names, string and numeric
literals. So a class with count 9 is nine hunks performing the same textual edit.

Your job is NOT to assign hunks - that is already done and must not change. Your job is
to make the result readable:

1. MERGE classes that are the same change in intent even though their text differs.
   This is the part hashing cannot do: `foo(a)` becoming `bar(a)` and `foo(x, y)`
   becoming `bar(x, y)` are one intent in two classes.
2. LABEL each merged group with what a reviewer needs to know.
3. RATE the reading effort each group deserves.

Return ONLY valid JSON, no prose and no code fence.

Schema:
{"groups": [{"label": "short name",
             "description": "one sentence: what changed and why it is safe or not",
             "classes": ["C3", "C17"],
             "effort": "skim" | "focus",
             "reason": "why this effort level"}]}

Rules:
- Every class id must appear in exactly one group. Do not invent class ids.
- Use as many groups as the change genuinely has. Do not force it into a small number;
  a 90-file refactor legitimately has more than five distinct concerns.
- "skim" means a reviewer can verify the whole group by reading one exemplar and
  trusting the rest are the same edit. Mechanical renames, import swaps, dependency
  bumps and refixtured snapshots are "skim".
- "focus" means the group changes behaviour, error handling, control flow, a public
  contract, or a security or correctness boundary. When in doubt use "focus".
- `class C7` notes "renamed from ... N% similar" where git detected a move. Below 95 the
  file was REWRITTEN during the move, not relocated verbatim: that class must be "focus".
- Order groups so "focus" groups come first: the reviewer should meet real work before
  mechanical work.
- Labels describe the PURPOSE, not the mechanism.

HOW TO SEE THE CHANGE

Nothing about the classes is in this prompt. Read what you need instead:

"#;

const PROMPT_TAIL: &str = r#"

TWO CALLS ARE USUALLY ENOUGH. `classes` for the shape of it, then `diff` with no ids
for the entire change. Every command takes as many arguments as you like and none means
all of them, so `diff C7 C8 C9` costs what `diff C7` costs. Asking one id at a time
turns a two-hundred-class change into two hundred round trips, and each one is a whole
turn -- it is by far the slowest thing you can do here.

A change too large for one reply comes back cut, ending with the exact command that
continues it. Run that command. Nothing is ever dropped for length, but you have not
seen the whole change until a reply comes back without one.

Generated files -- lockfiles, snapshots, build artefacts -- are folded away before you
see them, so these replies are smaller than the raw change. Their classes are not in the
id list and are not yours to group. Name one and you will still be shown it.

Rating a class "skim" is a claim that every one of its hunks is the same edit, and the
diff is how you check it. Only a class with MORE THAN ONE HUNK can be wrong about that:
for a `1h` class the exemplar is the whole class. `classes` prints the hunk count.

`uses:` on a class is a definition-to-use edge the mechanical pass found, with the
symbol that produced it. Merging a class that defines something with a class that
consumes a different group leaves no valid reading order, so prefer not to.

CLASS IDS (every one must appear in exactly one group):
"#;

/// Instructions, the fetch commands, and the class id list.
///
/// Takes the executable and the artefact path rather than the document: which
/// binary the model can run, and where it should read, are both the caller's
/// decisions. This function only writes them down — and the backend's tool
/// allowlist must be built from the same `fetch`, or the model is told to run
/// a command it is not permitted to run.
pub fn build_prompt(offered: &[&ClassInfo], fetch: &str, artefact: &str) -> String {
    let mut order: Vec<&&ClassInfo> = offered.iter().collect();
    order.sort_by_key(|c| (usize::MAX - c.n_hunks, c.exemplar));
    let ids: Vec<&str> = order.iter().map(|c| c.id.as_str()).collect();

    format!(
        "{PROMPT_HEAD}{}{PROMPT_TAIL}{}\n",
        commands(fetch, artefact),
        ids.join(" ")
    )
}

/// The five queries, spelled out as the exact commands to run.
fn commands(fetch: &str, artefact: &str) -> String {
    let dfr = format!("{fetch} agent --doc {artefact}");
    [
        format!("  {dfr} classes            every class: size, files, kind, defines, uses"),
        format!("  {dfr} diff               the diff of the WHOLE change, every hunk"),
        format!("  {dfr} diff C7 C8         only those classes"),
        format!("  {dfr} diff h12 h13       only those hunks"),
        format!("  {dfr} diff --after h137  the rest, when a reply came back cut"),
        format!("  {dfr} class C7 C8        those classes in full (no ids: all of them)"),
        format!("  {dfr} file <path>…      the classes touching those files"),
        format!("  {dfr} defines <symbol>… the classes introducing those symbols"),
    ]
    .join("\n")
}
