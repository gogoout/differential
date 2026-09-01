# 0028 — The verify stage is separate, and it is the write boundary

Status: accepted

Amends [ADR 0020](0020-ports-and-static-dispatch.md) by making a bound list tell the truth it
was designed to tell. Amends `spec/invariants.md`, which said all four invariants run before
any document is emitted.

## Context

`run_pipeline` wrote to the object database. One `git hash-object -w --stdin` subprocess per
text file with hunks, then one `write-tree`. Every one of those writes came from invariant 3;
invariant 4 ran `diff-tree` over the tree invariant 3 built, so it rode on the same work.

That made the function's bound list misleading. ADR 0020's whole argument is that a
function's trait bounds are an accurate statement of how much git it can touch. A caller
reading `RangeResolver + DiffSource + AttributeSource + ObjectReader + ObjectWriter +
TreeResolver + TreeBuilder + RecountSource` could not tell that four of those eight existed
for one audit, in a function whose name says it runs a pipeline.

So: who do the tree invariants serve? Invariant 3 asserts that a tree built by applying
hunks equals the head tree. Invariant 4 recounts `@@` headers over that same built tree.
Both are statements about a tree that was built.

Exactly one consumer builds one. The shadow-branch renderer writes a tree per synthetic
commit, which is the path invariant 3 describes. The reviewer renders windows over blobs it
reads, and the forge poster comments against line positions; neither reconstructs anything,
so neither can be protected by an assertion about a reconstruction.

Two of the three consumers were paying for a guarantee that cannot reach them.

The objection to splitting was that invariant 3 is the only check that catches a file
enumeration never saw: invariant 1 iterates the parser's own file list, so it passes over
such a file vacuously. That objection does not hold. **Invariant 1b already closes it** —
`rename_view::merge_raw` compares the parsed file set against the independent `--raw` listing
on membership and count, and hard-errors with "enumeration hole" on a mismatch. It is
read-only and costs one set comparison. It was implemented and never written down, which is
why it kept being missed.

## Decision

**Split the pipeline at the write boundary.**

`run_pipeline` and `run_grouped_pipeline` are read-only. Their bound list is
`RangeResolver + DiffSource + AttributeSource + ObjectReader`. They run invariants 1 and 2,
after invariant 1b has already run during enumeration, and emit no document when either
fails.

`pipeline::verify` runs invariants 3 and 4. Its bound list is
`ObjectReader + ObjectWriter + TreeResolver + TreeBuilder + RecountSource` — every write port
in one place, in a function whose name is what it does.

**The command decides, and there is no flag.** `dfr check` runs verify; running invariants is
its entire job. `dfr stack` runs verify; its commits are trees built from exactly these
hunks, so invariant 3 is about the path it is taking. `dfr review` and `dfr findings` do not.

**`generator.stages` carries the answer.** `verify` appends `"verify"`. Without it,
`audit.tree_assertion` reads `skipped` and `audit.recount` is `0`. The schema does not
change: `spec/overview.md` already told consumers to consult `generator.stages` rather than
infer from field presence, exactly as `groups: null` already means the grouping stage did not
run. Schema version stays 3.

`InvariantReport.tree` is `Option<TreeReport>`, and `all_ok()` is false while it is `None`.
A caller wanting the weaker claim asks `fidelity_ok()`. Absence must never read as a pass,
and the type is what enforces that.

## Consequences

The structural guarantee survives where it matters. `run_pipeline` still returns
`document: None` when invariant 1 or 2 fails, and `merge_raw` still errors on an enumeration
hole. A renderer cannot draw a plan with a bad parse, a dropped file, or broken accounting —
the three failures that would make it show the wrong thing. Only invariants 3 and 4 become
caller-chosen, and they protect the builder alone.

The saving is subprocesses, not repository growth, and that surprised us. **A passing verify
adds no new object.** Every blob it writes already exists, because the reconstruction
equalling head is precisely what invariant 3 asserts. New objects appear only when the
assertion is about to fail. `crates/engine/tests/verify_stage.rs` pins this, because the
opposite is the obvious assumption.

The shadow-branch builder keeps its own series assertions. They check the cumulative path —
index operations, deletions, modes, partial subsets in sequence — which the engine's
all-at-once `build_tree` does not exercise. The two are complementary.

Invariant 1b is now documented as `spec/invariants.md` §1b. It was load-bearing for this
decision and invisible, which is its own lesson: an implemented invariant that no spec names
cannot be reasoned with.
