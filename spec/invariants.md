# Invariants

Every one of these caught a real bug during validation of the prototype.

They fall into two halves, and the split is the write boundary (ADR 0028).

**Invariants 1b, 1 and 2 are read-only and core.** They run inside the pipeline, before any
document is emitted; a failure means no output and a non-zero exit. They are what protect a
renderer from a bad parse, a dropped file, or broken accounting.

**Invariants 3 and 4 build a tree, so they write.** They run in the `verify` stage, which a
caller invokes. Only a consumer that reconstructs a tree is protected by them, which is the
shadow-branch builder alone. `dfr check` and `dfr stack` run them; `dfr review` and
`dfr findings` do not. `generator.stages` records whether they ran, and their absence there
must never be read as a pass.

## 1. Applier fidelity

Every changed file must reconstruct **byte-exactly** from its base content plus all of its
hunks, before anything else is built. Binary files are checked by object id instead (they
carry no hunks); submodules are excluded from byte reconstruction.

Known 1-byte traps this must survive: an absent file's base is one empty line, not an empty
list (the trailing empty element encodes "ends with a newline"), and the
`\ No newline at end of file` marker is honoured per side.

## 1b. No enumeration hole

The file set parsed from the canonical patch must equal the file set in the independent
`--raw` listing, on both membership and count. A mismatch is an error, not a report.

This is the check invariant 1 cannot make. Invariant 1 iterates the parser's *own* file
list, so a file the parser never saw — a malformed `diff --git` header — is skipped in
silence rather than failed. Comparing against a second git call closes that directly, for
the cost of one set comparison.

Implemented in `rename_view::merge_raw`, which is where the two views already meet. It
raises `EngineError::Invariant` with "enumeration hole" rather than filling a report field,
because there is no document to attach a report to yet.

## 2. Hunk accounting

Hunk ids are unique; hunks summed across any partition (files now, groups later) equal the
canonical count; no hunk appears twice.

## 3. Tree assertion — and it must not be tautological

The final tree is computed by **applying hunks**, never by copying head blobs. Then
`built_tree == head^{tree}` proves every hunk was carried. With the copy shortcut, equality
holds by construction and proves nothing.

One documented exception: binary files are staged from the head object id — for them alone
the assertion is tautological, and the invariant report says so.

What this invariant does NOT catch: classification bugs. A wrong shape hash can produce
correct content in the correct order and still pass. Classification is validated separately
(`pure_substitution` is computed, rename similarity gates skim-eligibility).

## 4. Independent recount

A deliberately dumb counter — lines starting `@@ -` in `git diff-tree -r -U0 --no-renames`
output over the built tree — compared against the canonical hunk count. It is computed from
git, not from the builder's own bookkeeping, and its implementation must not share code with
the diff parser.

## 5. Nothing unassigned is dropped (grouping stage)

Any class id the model omits is back-filled into a trailing group marked `focus`. Applies
from milestone 2 onward; the schema reserves the shape for it.
