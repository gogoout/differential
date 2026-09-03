# Overview

`differential` turns a large diff into a grouped, ordered reading plan, so a reviewer reads
what deserves reading and skips what has already been verified by shape.

## The product

The product is **one JSON document** (see [json-contract.md](json-contract.md)) describing:

- every changed file and hunk (canonical, complete — nothing is ever filtered out),
- **shape classes**: hunks that are the same textual edit after normalising away identifiers
  and literals,
- **groups**: shape classes merged and labelled by intent, each rated `focus`, `skim`, or
  `noise`,
- a **reading plan**: groups ordered foundation-first, with dependency edges.

The core ships as a library (ADR 0014): consumers link the engine and receive the document
in-process — see [consumers.md](consumers.md). Consumers are views over this document and
must not influence its shape:

1. **Shadow branch** ([stack.md](stack.md)) — the diff rewritten as a synthetic commit stack,
   reviewed natively in an IDE or `tig`. `git log --oneline` alone shows the shape of the change.
2. **TUI** — a dedicated reviewer emitting structured findings keyed by hunk.
3. **Forge review** ([forge.md](forge.md)) — the request's review threads shown under their
   lines, and findings published back as review comments.

## The two-layer architecture

Coverage is structural; judgement is delegated.

1. **Mechanical partition (no LLM).** Every hunk is assigned a shape class by hashing its
   normalised diff text — both removed and added sides. Coverage is 100% by construction.
2. **LLM merges and labels class ids, never hunks.** The model cannot drop a hunk because it
   never names one. An omitted class id is detected against the known id set and back-filled
   into a trailing group that must be read.
3. **Audits.** Byte-exact reconstruction and hunk accounting validate every document before
   it is emitted. A non-tautological tree assertion and an independent recount run in a
   separate `verify` stage, because they build a tree and only a tree-building consumer is
   protected by them (ADR 0028).

The alternative — asking a model to assign hunks to groups — was measured and rejected: on
large refactors it silently dropped up to ~73% of hunks while reporting success. See
[adr/0001](../adr/0001-llm-merges-class-ids-never-hunks.md).

## Pipeline stages

`generator.stages` in the document records which of these actually ran:

| stage | what it does | milestone |
|---|---|---|
| `enumerate` | canonical hunk enumeration from `git diff -U0 --no-renames`; rename-detected view (`-M`) merged in as annotations | 1 |
| `classify` | shape classes, `pure_substitution`, generated-file hints, the class dependency graph (ADR 0022) | 1 |
| `group` | LLM merge/label of class ids; coverage audit; back-fill ([grouping.md](grouping.md)) | 2 |
| `order` | contract the class dependency graph onto groups, foundation-first topological sort, roles ([ordering.md](ordering.md)) | 3 |
| `verify` | invariants 3 and 4: the tree assertion and the independent recount ([invariants.md](invariants.md)) | 1 |

`verify` is the one stage a caller opts into. It writes — building a tree needs the blobs in
the object database — and only a consumer that reconstructs a tree is protected by it. The
shadow branch runs it; `dfr check` exists to run it. The reviewer does not. Read its absence
from `generator.stages` as "did not run", never as a pass: `audit.tree_assertion` then reads
`skipped` and `audit.recount` is `0`.

## Honest reporting

`skim` totals overstate the saving: exemplars still get read. Documents report both
`read_hunks` (focus + exemplars) and `skipped_hunks` (skim remainders + folded noise); only
the latter is the genuine saving. Consumers must not present skim totals as time saved.
