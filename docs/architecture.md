# Architecture

How `differential` turns a large diff into a reading plan, and why it is built this way.

The normative behaviour lives in [`spec/`](../spec/). The decision records, with their
evidence, live in [`adr/`](../adr/). This page explains how the pieces fit together.

## The problem

Most of a large merge request is one decision repeated. A signature change reaches every
call site. A rename reaches every import. A lockfile is regenerated whole.

The changes that need careful reading are a small part of it. The rest has to be skippable
without the reviewer guessing which part is which.

## Coverage and judgement

Coverage is structural. Judgement is delegated.

### 1. Mechanical partition

Every hunk gets a **shape class**. Two hunks in one class are textually identical after
normalisation.

A shape class is nothing more than normalised text, hashed. There is no parser, no AST, no
index and no model — four regex substitutions over the raw bytes of the diff.

The generic normaliser is
[`crates/engine/src/lang/generic.rs`](../crates/engine/src/lang/generic.rs), in
`normalize_line`. It does exactly this, in this order:

| step | rule |
|---|---|
| 1 | Strings become `"S"`. |
| 2 | Numbers become `N`. |
| 3 | Identifiers of four characters or more become `I`. |
| 4 | Runs of whitespace collapse to one space, then the line is trimmed. |

So this hunk:

```diff
-    let timeout = Duration::from_secs(30);
+    let timeout = Duration::from_secs(config.timeout);
```

normalises to:

```
- let I = I::I(N);
+ let I = I::I(I.I);
```

`let` survives because it is three characters, below the identifier threshold. Any other
hunk that reduces to those same two lines is the same shape.

The framing around the normaliser is
[`crates/engine/src/shape.rs`](../crates/engine/src/shape.rs), in `shape_hash`. It prefixes
each removed line with `-` and each added line with `+`, **sorts each side**, joins them
with newlines, appends the file's disposition letter, takes a sha1, and keeps the first 12
hex characters. That string is the class key.

Two details in there carry weight. Both sides are hashed, not just the added side — hashing
added lines alone collapses every deletion-only hunk into one class, which would make
"same shapes, skippable" false (ADR 0004). And the disposition is part of the key, so a
whole-file addition and a modification with identical text are different shapes.

Sorting each side means a hunk is compared as a multiset of lines rather than a sequence.
The framing is inherited unchanged from the validated prototype.

`normalize_line` is pluggable per language ([ADR 0015](../adr/0015-language-abstraction.md)).
The framing is not. The generic normaliser is frozen against the validated prototype so
class populations stay comparable with its recorded outputs; improvements land as language
plugins with their own ids.

Coverage is 100% by construction. No model is involved. No file is excluded.

### 2. An LLM merges and labels class ids, never hunks

Asking a model to partition hunks directly fails silently. We measured it. On large
refactors, coverage dropped as low as 27% while the model reported success. An omitted hunk
index is indistinguishable from one that never existed.

Here the model can only group **class ids**. Anything it omits is detected against the known
id set and back-filled into a must-read group.

The model contributes what hashing cannot: merging twenty textually different shape classes
into one "path and import swaps" group. See
[ADR 0001](../adr/0001-llm-merges-class-ids-never-hunks.md).

### 3. Deterministic ordering

Edges run from a symbol's definition to its uses. Those edges build a dependency graph
between groups. The focus section is sorted foundation-first, so a definition is ordered
before its references. See [spec/ordering.md](../spec/ordering.md).

### 4. Structural audits

These run before any document is emitted.

- Every changed file must reconstruct **byte-exactly** from its base plus its hunks.
- The final tree is rebuilt **from applied hunks**, never copied, and must equal the real
  head tree.
- An independent, deliberately dumb recount over git's own output must match.

Each audit caught a real bug during validation. See
[`spec/invariants.md`](../spec/invariants.md).

### Read and skipped hunks

Skim exemplars still get read. So documents track **read hunks** and **skipped hunks**
separately. Only the second number is time saved.

## The pipeline

`enumerate → classify → group → order`. The document records which stages ran, in
`generator.stages`. See [spec/overview.md](../spec/overview.md).

The product is **one JSON document** ([spec/json-contract.md](../spec/json-contract.md)).
Renderers are views over it:

- **Shadow branch** ([spec/stack.md](../spec/stack.md)) — the diff rewritten as a synthetic
  commit stack. `git log --oneline` over it is the reading plan. Shipped, as `dfr stack`.
- **TUI** ([spec/tui.md](../spec/tui.md)) — a reviewer that emits structured
  findings keyed by hunk. Shipped, as `dfr review`. `dfr findings` prints those findings as
  JSON.
- **Forge review** — grouped comments posted to a GitLab merge request or a GitHub pull
  request. Planned.

## Guarantees

**No file is excluded from enumeration.** No extension filters. No path exclusions. Not
even through config. Manifest and lockfile edits are where repeated edits appear, and
path filtering was the single worst coverage bug found during validation (ADR 0005, 0012).
Config and language plugins tune *classification*, never *what exists*.

**A low-similarity rename is not treated as a verbatim move.** Renames carry git's
similarity score on both halves. Below 95, the change is a modification and is never
skim-eligible. A deterministic gate enforces this after the model runs (ADR 0003).

**Groupings are pinned.** Labels are not deterministic across model calls. A content-hash
cache keeps a review from reshuffling under the reviewer (ADR 0009).

**Documents are pure functions of `base..head`.** Review state — findings and progress —
lives in a sidecar store. It re-anchors across regenerations by exact hunk digest. An
orphan is listed, never silently dropped. See
[spec/persistence.md](../spec/persistence.md).

## Crate layout

| crate | what it holds |
|---|---|
| [`crates/engine`](../crates/engine/README.md) | The backend: git io, diff parsing, the byte-exact applier, shape classes, the language registry, grouping, ordering, invariants and review sessions. |
| [`crates/stack`](../crates/stack/README.md) | The shadow-branch renderer. The diff as a synthetic commit stack. |
| [`crates/tui`](../crates/tui/README.md) | The terminal reviewer. Vendored `tuicr` and `lumen` pieces live here. |
| [`crates/cli`](../crates/cli/README.md) | The application layer: the `dfr` and `differential` binaries. Argument parsing and dispatch only. |
| `crates/testutil` | Shared test fixtures. `publish = false`. |

Inside the engine, four module boundaries carry weight:

| module | what it is |
|---|---|
| `engine::schema` | The frozen JSON contract. Serde types only, no engine internals. |
| `engine::plan` | Shared domain policy over the schema. Both renderers read tiers and reading splits from here, so they cannot disagree. |
| `engine::ports` | The traits the domain owns. |
| `engine::store` and `engine::gitio` | The adapters that implement those ports. |

Dependency direction is strict: `cli → {tui, stack} → engine`.

`engine::schema` stays the product boundary. It is a reviewed module discipline rather than
a crate boundary (ADR 0008, 0018).

All git access shells out to real git, plumbing commands only. The byte-exactness guarantees
were validated against real git output and nothing else (ADR 0002, 0011).

Domain code reaches git through `engine::ports`, and `gitio::Repo` is their only
implementation. `Repo::run` is private, so a function's trait bounds are an accurate
statement of how much git it can touch. A fake git would make invariants 1 to 4 compare a fake with
itself, so it is impossible to introduce by accident (ADR 0020, enforced by
`engine/tests/layering.rs`).

## Testing

**Unit tests cover the byte-level traps.** No-trailing-newline permutations worth exactly
one byte. Deleted lines starting with `--`. CRLF round-trips.

**Integration tests run the full pipeline** against hermetic temporary repositories, for
every edge case: modes, symlinks, binary files, submodules, typechanges, and renames at
high and low similarity.

**Grouping and stack tests drive a programmable fake LLM backend.** The LLM is the one seam
where a fake is correct: it is chosen at run time.

**A parity test asserts exact class-count agreement** with the validated prototype, on a
real private corpus. It runs from an environment variable, so it is local-only. Drift is a
port bug. It is never tolerated by adjusting the number.
