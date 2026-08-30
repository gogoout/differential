# 0025 — Reviewed marks key per hunk, not per class

Status: accepted

Supersedes the reviewed-marks bullet of [ADR 0013](0013-incremental-review-sidecar-state.md).

## Context

ADR 0013 keyed reviewed marks on **class content**: `sha1` over the sorted digests of every
hunk in the class (`plan::class_content_key`). The gate was already "carry it forward only
if the content is untouched", which is right. The **unit** was wrong.

A class holds every hunk of one shape. Change one hunk of five and the class key changes,
so the mark drops for all five — including the four nobody touched. The reviewer reads them
again to learn that nothing happened to them. On a branch that grows a commit at a time,
that is most of what a reopened review asks for.

The blast radius was invisible in the fixtures because the shapes there hold one hunk each.
It is not invisible on a real range, where a class of a dozen call-site edits is normal.

## Decision

**A reviewed mark keys on `hunks[].digest` — the exact, position-free content hash the
schema has carried since v1 and findings already anchor on.**

- `ReviewState.reviewed_classes` becomes `reviewed_hunks`. The old field does not load: its
  keys are class hashes and cannot be read as hunk digests, so marks already on disk are
  lost once. Everything else in `state.json` (cursor, layout) is untouched.
- `space` in the plan pane marks **every hunk** of the selected group or file, in one
  write, with set semantics. `space` in the diff pane marks the hunk under the cursor.
- `plan::class_content_key` is deleted. It had exactly one caller, and this is it.
- `ReviewView` carries the digest per hunk and answers `hunks_marked`. The lookup walks the
  hunks, not the marks: **a digest is content, and content repeats.** Two byte-identical
  hunks carry one key, so a mark on either covers both, and a digest→hunk map could only
  ever name one of them.

## Consequences

- Reviewed progress now survives a partial change to a class, which is the common case on a
  moving branch. Together with [ADR 0026](0026-a-review-adopts-an-ancestor.md), reviewing
  `main..<sha>`, committing and reviewing again keeps what was read.
- `space` no longer means "one exemplar verifies the shape". That reading is a **tier**
  decision and it survives where it belongs: a skim group still shows one exemplar and
  folds the remainder (`spec/tui.md`). What changed is only what a mark records.
- The progress denominator is hunks, not classes, and `reviewed_count` counts marks that
  land on a hunk of the current document. Counting the stored set counted keys from every
  generation of the plan, which could outrun the total drawn beside it.
- Marks are one key per hunk rather than one per class, so `state.json` grows. It holds
  40-character hex strings for hunks a reader has finished; on the corpus's largest range
  that is tens of kilobytes, against a plan document of hundreds.

## Alternatives rejected

**Keying on the class, and re-anchoring dropped marks like findings.** A mark carries no
context lines and no offset, so there is nothing to fuzzy-match with. The re-anchor would
have to compare class membership, which is the class key again with extra steps.

**Migrating the old marks.** For every class in the current document, compute the old class
key and expand a hit into its member digests. It works, and it keeps `class_content_key`
and the dead field alive forever to serve one upgrade. The author chose the clean break.

**Keying on `(file, digest)` or on a digest plus an occurrence index.** The first still
collides for identical hunks in one file. The second is positional, which ADR 0013 rules
out: positional ids do not survive regeneration.
