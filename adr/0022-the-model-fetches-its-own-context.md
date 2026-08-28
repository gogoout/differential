# 0022 — The model fetches its own context

Status: accepted. Supersedes [0010](0010-llm-invocation-tools-denied.md).

## Context

The grouping stage handed the model one fixed string. Per shape class it carried a header,
up to four removed and four added lines from the **exemplar hunk only**, and up to six
basenames — the whole payload capped at 90,000 characters.

Three things followed from that, and the first is a correctness problem.

1. **The model could not check the claim it makes.** Rating a class `skim` asserts that
   every hunk in it is the same edit, and "read one exemplar, trust the rest" is the whole
   saving (ADR 0006). It had seen one hunk of nine. Nothing else checks within-class
   equivalence either: invariants 1–4 guarantee coverage, not equivalence
   (`spec/invariants.md`).
2. **It merged blind to structure.** The payload carried no dependency information, so it
   could not know that putting a class which defines a trait in the same group as a class
   which consumes another group leaves no valid reading order.
3. **It truncated.** At roughly 400 to 900 characters per class, a change with two hundred
   classes straddles the cap. Classes past it became audit-missing and were back-filled
   into a must-read group — safe, but the opposite of the tool's purpose.

The dependency graph had a second, separate problem. `ordering.rs` built it from **groups**,
unioning every symbol in a group before computing one edge. So a symbol two classes defined
produced an edge only when the model happened to merge those two classes, and contracting
the finer graph onto groups manufactured cycles the change did not contain.

## Decision

**The engine writes what it knows; the model reads what it needs.**

- The pre-group document — `PlanDocument` with `groups: null` — is written to
  `<git-common-dir>/differential/cache/document/<key>.json`, under the grouping cache's own
  key. One grouping, one document.
- The prompt carries instructions, that path, the `dfr agent` commands, and the class id
  list. Nothing else. The 90,000-character cap is gone, and with it the truncation
  back-fill.
- `dfr agent` answers five questions against the document: `classes`, `class`, `diff`,
  `file`, `defines`. `diff` is the one that matters: it is the first time the model can look
  at a non-exemplar member before rating a class.
- **Every command takes any number of arguments, and none means all of them.** A call is a
  round trip through a model turn, and the work behind one is negligible beside it: `dfr
  agent` answers in under a tenth of a second even on a 283-hunk range in a debug build,
  and a `diff` batch re-enumerates the range once however many ids it carries. Measured on
  a 196-class change: one id per call made 176 fetches, batching brought it to 28, a bare
  `diff` for the whole change brought it to 15, and leaving generated content out brought
  it to 5.
- **Asking without naming anything gets the offered set; naming something reaches
  everything.** `classes` and a bare `diff` leave out generated content, matching what the
  prompt's id list offers (ADR 0006). Handing the model a lockfile would be bytes it must
  read and a class id the audit would then reject as a hallucination. `class`, `file` and
  `defines` answer for generated content when asked by name — the noise tier folds, it
  never hides. `plan::class_is_generated` is the single definition both sides use.
- **A `diff` reply carries at most 256KB, and says how to continue.** Past the cap it ends
  with the exact `diff --after <hunk-id>` to run next. The cursor is a hunk id because a
  hunk id already names a position in the list — nothing has to be encoded, and the reader
  gets a command rather than an instruction to assemble one. **Nothing is ever dropped for
  length**, which is the whole difference from the cap this replaced.
- **The dependency graph moves to `artefact::graph`, built from classes before the model
  runs.** It lands on `ClassEntry.defines` and `ClassEntry.depends_on`; the ordering stage
  contracts it onto groups. Every edge carries the symbols that produced it.
- The default backend gains a read-only allowlist:
  `Bash(dfr agent:*),Read,Grep,Glob,Bash(git log:*),Bash(git show:*)`. **Available, not
  advertised**: the prompt names the fetch command and nothing else. A model that needs the
  code around a hunk can go and read it, but it is not sent looking — naming these would
  invite a whole-repository read where a class table and a diff were the answer, and would
  offer a way around the generated content the stage deliberately folds away.
- **`schema_version` is 3.** `Group.depends_on` becomes a list of `Edge { on, via, cycle }`,
  and `Group` gains `pivot`.

The model's job is unchanged. It merges class ids, labels and rates, and it never touches
hunks (ADR 0001). Only its context changed.

## Why ADR 0010 is superseded, not contradicted

ADR 0010 denied tools because every observed hard failure was the model CLI exiting 1 with
`stop_reason: "tool_use"`. `--allowed-tools ""` sends **no tool definitions**, so the model
cannot emit a tool-use block and cannot fail that way. That was a cure, not a diagnosis: the
failure is a model asking for a tool the harness will not run. A correct allowlist is the
other cure — it asks, and the answer is yes.

`LlmBackend` does not change. It is still `complete(prompt) -> String`; tools run inside the
CLI this spawns. Two failure modes replace the old one, and both are the caller's to see:
the 1200-second deadline, and prose instead of JSON after a long agentic run.

## The mechanism computes dependencies; the model never states them

The model can resolve a symbol better than a regex. It is still not asked to.

Dependency is computable, so it is deterministic, exhaustive over the whole change, and
free. A model asked for edges on top of an already large task would sample rather than
cover, and nothing could check what it returned. The graph is a fact about the diff; the
grouping is a judgement about the diff. Keeping them apart is what lets a cached grouping be
re-ordered on every load without another model call.

## Consequences

- **`depends_on` loses the edges grouping used to create.** A symbol two classes define now
  has no unique definer and produces no edge, where before an edge survived if the model
  merged both defining classes. That is the right loss: what depends on what cannot turn on
  how a label was drawn. A precise `Language` (ADR 0015) would resolve the ambiguity rather
  than drop it.
- **Classes inside a group are now ordered.** The old `def_gi != gi` guard discarded every
  intra-group edge; those edges are the only thing that can order a group's members, so
  `class_ids` is sorted foundation-first.
- **A cycle now says why it exists.** `Edge.cycle` is `artefact` when the class graph is
  acyclic — the deadlock came from contracting classes into groups — and `mutual` when the
  classes deadlock too. On an artefact cycle the class order decides which group is emitted,
  instead of group size.
- **Nothing splits a group.** `pivot` records where a group stops being a foundation and
  starts being a consumer, and stops there. The merge is the model's judgement (ADR 0001),
  the graph that would undo it is heuristic (ADR 0015), and ADR 0007's tolerance — a wrong
  edge only misorders — does not extend to a wrong cut, which would break a coherent group
  and mislabel both halves. The impossibility is information the reviewer wants, not a
  problem to hide behind two rows.
- **`PROMPT_VERSION` is 3 and the backend's identity changed**, so every cached grouping in
  every checkout is invalidated once. Both feed the cache key, so this is automatic, not a
  migration.
- **The cache key hashes the backend's identity, never its display name.** The default
  argv names the executable the prompt tells the model to fetch with, which on an installed
  binary is an absolute path. Where a binary lives determines nothing about a grouping, so
  `LlmBackend::identity` stands a placeholder in its place. Hashing the name instead would
  have made a debug build, a release build and two checkouts of one commit each re-run a
  four-hundred-second call over an identical class partition, and would have defeated the
  worktree-shared cache `plan::grouping_cache_dir` exists to provide (ADR 0009). The
  allowlist itself stays in the key: it shapes what the model can see.
- **A hole the cache key cannot close.** The key covers the prompt version, the backend
  name, the language fingerprint and the class content digests (ADR 0009). A model reading
  `git log` reads history no key can capture, so two clones with different history can group
  differently under one key. That is the price of reaching the *reason* a change was made,
  and no key design fixes it.
- **`dfr agent` never runs the pipeline and never calls a model.** It reads the document and
  re-enumerates the range the document names, so a grouping run cannot recurse into itself.
- An unknown id prints a plain sentence and exits 0. To an agent a non-zero exit reads as
  "the tool is broken" and stops it asking, which is worse than a clear "no".
- **Grouping is now minutes, not seconds, and round trips are not why.** Measured on the
  validation corpus: 176 fetch calls, then 28, then 15, then 5 — and the wall clock went
  450s, 391s, 401s. Round trips fell thirty-five-fold and the time did not move, because
  at roughly 0.9s a call they were never more than about 13 seconds of it. The rest is the
  model reading 322KB of diff and reasoning about 196 classes. Batching, the bare form and
  the cursor remove waste and remove the size cliff; none of them moves that floor. The
  only lever on it is asking the model to read less, and what it does not read is what it
  cannot label from. The grouping cache means the cost is once per change, not once per
  run.
- **The artefact-against-mutual verdict did not discriminate on the validation corpus.**
  Every one of 45 broken edges came back `mutual`: the class graph was cyclic wherever the
  group graph was. On 196 classes and 290 edges at roughly 30% precision, spurious edges
  make a tangle at every granularity, so the finer graph has nothing cleaner to say. The
  distinction is honest and cheap to compute, and it should start firing when extraction
  gets better — that is what makes precision worth raising, not a reason to drop it.
- **A fetch command that does not work is expensive, and silently so.** The model does not
  give up on a failing tool; it retries and works around it, and only then falls back to the
  class ids. So the executable the prompt names must be one that has `agent`.
  `std::env::current_exe()` is right for the shipped binary and wrong for anything else —
  the dev example resolves its sibling `dfr` and refuses to run without it. The tool
  allowlist is derived from the same string, so the two cannot disagree about it.
