# Overview

`differential` turns a large diff into a grouped, ordered reading plan, so a reviewer reads
what deserves reading and skips what has already been verified by shape.

## The product

The product is **one JSON document** (see [json-contract.md](json-contract.md)) describing:

- every changed file and hunk (canonical, complete — nothing is ever filtered out),
- **shape classes**: hunks that are the same textual edit after normalising away identifiers
  and literals,
- **groups**: shape classes merged and labelled by intent, each rated `close`, `skim`, or
  `noise`,
- a **reading plan**: groups ordered foundation-first, with dependency edges.

Consumers are views over this document and must not influence its shape:

1. **Shadow branch** — the diff rewritten as a synthetic commit stack, reviewed natively in an
   IDE or `tig`. `git log --oneline` alone shows the shape of the change.
2. **TUI** — a dedicated reviewer emitting structured findings keyed by hunk.
3. **Forge review** — grouped comments posted to a GitLab MR / GitHub PR.

## The two-layer architecture

Coverage is structural; judgement is delegated.

1. **Mechanical partition (no LLM).** Every hunk is assigned a shape class by hashing its
   normalised diff text — both removed and added sides. Coverage is 100% by construction.
2. **LLM merges and labels class ids, never hunks.** The model cannot drop a hunk because it
   never names one. An omitted class id is detected against the known id set and back-filled
   into a trailing group that must be read.
3. **Audits.** Byte-exact reconstruction, hunk accounting, a non-tautological tree assertion,
   and an independent recount validate every document before it is emitted.

The alternative — asking a model to assign hunks to groups — was measured and rejected: on
large refactors it silently dropped up to ~73% of hunks while reporting success. See
[adr/0001](../adr/0001-llm-merges-class-ids-never-hunks.md).

## Pipeline stages

`generator.stages` in the document records which of these actually ran:

| stage | what it does | milestone |
|---|---|---|
| `enumerate` | canonical hunk enumeration from `git diff -U0 --no-renames`; rename-detected view (`-M`) merged in as annotations | 1 |
| `classify` | shape classes, `pure_substitution`, generated-file hints | 1 |
| `group` | LLM merge/label of class ids; coverage audit; back-fill | 2 |
| `order` | group dependency DAG, foundation-first topological sort, roles | 2 |

## Honest reporting

`skim` totals overstate the saving: exemplars still get read. Documents report both
`read_hunks` (close + exemplars) and `skipped_hunks` (skim remainders + folded noise); only
the latter is the genuine saving. Consumers must not present skim totals as time saved.
