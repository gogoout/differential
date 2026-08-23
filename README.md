# differential

**Grouped, ordered reading plans for large diffs.**

A 100-file merge request is not 100 files of work. Most of it is one decision echoing through
the codebase: a signature change cascading through call sites, a rename sweeping across
imports, a lockfile regenerating itself. A reviewer's real job is to find the handful of
changes that deserve close reading — and to *safely* skip the rest.

`differential` does that mechanically. It turns a diff into **one JSON document** describing
what to read closely, what can be verified from a single exemplar, and what is generated
noise — with 100% hunk coverage guaranteed structurally, never by trusting a model.

## How it works

The load-bearing idea: **coverage is structural, judgement is delegated.**

1. **Mechanical partition.** Every hunk is assigned a *shape class* — a hash of its diff text
   with identifiers and literals normalised away, on both the removed and added sides. Hunks
   in one class are the same edit wearing different names. Coverage is 100% by construction:
   no model, no parser of semantics, no file ever excluded.
2. **An LLM merges and labels class ids — never hunks.** Asking a model to partition hunks
   directly fails silently: on large refactors, measured coverage dropped as low as ~27%
   while reporting success, because an omitted hunk index is indistinguishable from one that
   never existed. Here the model can only group *class ids*; anything it omits is detected
   against the known id set and back-filled into a must-read group. It still earns its keep —
   merging twenty textually-different shape classes into one "path and import swaps" group is
   exactly what hashing can't do.
3. **Structural audits.** Before any document is emitted: every changed file must reconstruct
   **byte-exactly** from base + hunks; the final tree is rebuilt *from applied hunks* (never
   copied) and must equal the real head tree; and an independent, deliberately dumb recount
   over git's own output must match. Each audit caught a real bug during validation
   ([`spec/invariants.md`](spec/invariants.md)).

The report is honest about the saving: skim exemplars still get read, so documents track
*read hunks* and *skipped hunks* separately — only the latter is time saved.

Three consumers are planned as views over the document:

- **Shadow branch** — the diff rewritten as a synthetic commit stack, reviewed natively in
  your IDE or `tig`; `git log --oneline` alone shows the shape of the change.
- **TUI** — a dedicated reviewer emitting structured findings keyed by hunk.
- **Forge review** — grouped comments posted to a GitLab MR / GitHub PR.

## Status

| stage | state |
|---|---|
| Frozen JSON contract (`schema_version: 1`) | ✅ [`spec/json-contract.md`](spec/json-contract.md) |
| Core engine: enumeration, shape classes, applier, invariants | ✅ |
| Language abstraction (per-language normalisation, generic default) | ✅ seam in place |
| LLM backend abstraction (`differential-llm`) | ✅ seam in place |
| Grouping stage (LLM merge/label + coverage audit) | ⏳ next |
| Ordering (foundation-first group DAG) | planned |
| Consumers: shadow branch, TUI (`dfr`), forge | planned |

Documents currently carry `groups: null` and `generator.stages = ["enumerate", "classify"]` —
a complete, classified, audited enumeration awaiting the grouping stage.

## Usage

The core is a **library** (ADR 0014): renderers link it directly and receive the document
in-process. The `dfr` binary arrives with the TUI.

```rust
use differential_engine::{gitio::Repo, config::Config, lang::LanguageRegistry,
                          resolve_range, run_pipeline};

let repo = Repo::open(path)?;
let config = Config::load(repo.root(), None)?;          // .differential.toml or defaults
let (base, head, kind) = resolve_range(&repo, &["main..feature"])?;
let out = run_pipeline(&repo, &base, &head, kind, &config, &LanguageRegistry::builtin())?;

// out.report:   InvariantReport — always present
// out.document: Option<PlanDocument> — None iff an invariant failed
```

`a...b` resolves the base via merge-base — which is what an MR/PR diff is. Full library
surface and the per-repo `.differential.toml` config format:
[`spec/consumers.md`](spec/consumers.md).

Dev/CI invariant runner (an example, not a product):

```sh
cargo run -p differential-engine --example check -- [--repo <path>] <base>..<head>
```

## Guarantees

- **Nothing is ever excluded from enumeration.** No extension filters, no path exclusions —
  not even via config. Manifest and lockfile edits are where refactor cascades live; path
  filtering was the single worst coverage bug found during validation (ADR 0005, 0012).
  Config and language plugins tune *classification*, never *what exists*.
- **A low-similarity rename can never masquerade as a verbatim move.** Renames are annotated
  with git's similarity score on both halves; below ~95 it is a modification and never
  skim-eligible (ADR 0003).
- **Documents are pure functions of `base..head`.** Review state (comments, progress) lives
  in a sidecar store and re-anchors across regenerations by exact hunk digest — orphaned,
  listed, never silently dropped ([`spec/persistence.md`](spec/persistence.md)).

## Layout

| path | what |
|---|---|
| [`spec/`](spec/) | what the program does (normative) |
| [`adr/`](adr/) | why it is this way (decision records 0001–0016) |
| `crates/schema` | the frozen JSON contract as serde types — the product boundary |
| `crates/engine` | git io, diff parsing, byte-exact applier, shape classes, language registry, invariants |
| `crates/llm` | the LLM backend abstraction the grouping stage builds on |

Dependency direction is strict: consumers → `engine` → `schema`. The schema crate depends
only on serde, so future consumers take the contract without the git plumbing.

## Development

Requires stable Rust (pinned in `rust-toolchain.toml`) and a `git` binary on PATH — all git
access shells out to real git, because the byte-exactness guarantees were validated against
real git output and nothing else (ADR 0002).

```sh
cargo test                     # unit + synthetic-repo integration tests (hermetic temp repos)
cargo clippy --all-targets
DIFFERENTIAL_FIXTURE_CONFIG=$PWD/fixtures.local.toml \
  cargo test -- --ignored      # parity against a real corpus; see fixtures.example.toml
```

The synthetic suite covers the byte-level traps: files without trailing newlines (all five
permutations — some worth exactly one byte), mode-only chmods, symlinks, binary files,
submodule bumps, typechanges, CRLF round-trips, renames at high and low similarity, and
deleted lines that begin with `--` (a real prefix-sniffing parser bug).

Before touching `crates/schema` or the invariants, read the ADRs. Every invariant caught a
real bug during validation; keep them all.
