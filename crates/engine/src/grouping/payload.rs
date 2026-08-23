//! Prompt and payload construction, ported from the validated prototype: one
//! compact block per shape class, largest first, both diff sides shown (a
//! deletion-only hunk is otherwise invisible to the model), plus rename
//! annotations so a heavily-edited move can never masquerade as a plain add.

use crate::model::DiffView;

use super::ClassInfo;

/// Feeds the cache key: bump on ANY change to the prompt text or payload
/// format, or cached groupings would silently mix prompt generations.
pub const PROMPT_VERSION: u32 = 1;

const MAX_SIDE_LINES: usize = 4;
const MAX_LINE_CHARS: usize = 150;
const MAX_FILES_SHOWN: usize = 6;
const MAX_PAYLOAD_CHARS: usize = 90_000;

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
             "effort": "skim" | "close",
             "reason": "why this effort level"}]}

Rules:
- Every class id must appear in exactly one group. Do not invent class ids.
- Use as many groups as the change genuinely has. Do not force it into a small number;
  a 90-file refactor legitimately has more than five distinct concerns.
- "skim" means a reviewer can verify the whole group by reading one exemplar and
  trusting the rest are the same edit. Mechanical renames, import swaps, dependency
  bumps and refixtured snapshots are "skim".
- "close" means the group changes behaviour, error handling, control flow, a public
  contract, or a security or correctness boundary. When in doubt use "close".
- A block noting "renamed from ... N% similar" with N below 95 was REWRITTEN during the
  move, not relocated verbatim: it must be "close".
- Order groups so "close" groups come first: the reviewer should meet real work before
  mechanical work.
- Labels describe the PURPOSE, not the mechanism.

SHAPE CLASSES:
"#;

pub fn build_prompt(offered: &[&ClassInfo], view: &DiffView) -> String {
    let mut blocks: Vec<String> = Vec::with_capacity(offered.len());
    let mut order: Vec<&&ClassInfo> = offered.iter().collect();
    order.sort_by_key(|c| (usize::MAX - c.n_hunks, c.exemplar));

    for c in order {
        blocks.push(class_block(c, view));
    }

    let mut payload = String::with_capacity(MAX_PAYLOAD_CHARS.min(blocks.len() * 200));
    for b in blocks {
        if payload.len() + b.len() > MAX_PAYLOAD_CHARS {
            // Classes cut here become audit-missing and are back-filled into a
            // must-read group — truncation can never lose a hunk.
            payload.push_str("\n[remaining classes omitted for length]");
            break;
        }
        payload.push_str(&b);
        payload.push_str("\n\n");
    }

    format!("{PROMPT_HEAD}{payload}")
}

fn class_block(c: &ClassInfo, view: &DiffView) -> String {
    let ex = &view.hunks[c.exemplar];
    let loc = format!("{}:{}", c.files[0], ex.new_start.max(1));
    let rename = c
        .rename_note
        .as_deref()
        .map(|n| format!(" ({n})"))
        .unwrap_or_default();

    let mut out = format!(
        "[{}] count={} files={} kind={} e.g. {}{}",
        c.id,
        c.n_hunks,
        c.files.len(),
        c.kind,
        loc,
        rename
    );

    // Both sides: a deletion-only hunk is otherwise invisible.
    let mut shown = 0usize;
    for (sigil, side) in [("-", &ex.removed), ("+", &ex.added)] {
        for line in side
            .iter()
            .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
            .take(MAX_SIDE_LINES)
        {
            let text = String::from_utf8_lossy(line);
            let clipped: String = text.chars().take(MAX_LINE_CHARS).collect();
            out.push_str("\n    ");
            out.push_str(sigil);
            out.push_str(&clipped);
            shown += 1;
        }
    }
    if shown == 0 && c.kind == 'D' {
        out.push_str("\n    (whole file deleted)");
    }

    if c.files.len() > 1 {
        let names: Vec<&str> = c
            .files
            .iter()
            .take(MAX_FILES_SHOWN)
            .map(|p| p.rsplit('/').next().unwrap_or(p))
            .collect();
        let more = if c.files.len() > MAX_FILES_SHOWN {
            format!(" +{} more", c.files.len() - MAX_FILES_SHOWN)
        } else {
            String::new()
        };
        out.push_str(&format!("\n    (in: {}{more})", names.join(", ")));
    }
    out
}
