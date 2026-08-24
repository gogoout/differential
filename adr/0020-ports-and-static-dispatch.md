# 0020 — Domain-owned ports, consumed by static dispatch

Status: accepted (extends 0018; constrains 0002, 0011, 0012)

## Context

The pure kernels are clean: `apply`, `parse`, `shape`, `ordering`, `document`, `paths`
and `rename_view` are pure functions over owned data with dense unit tests. Everything
around them is not, and in three distinct ways.

**Orchestration fuses policy with I/O.** `run_core_with_progress` interleaves six git
invocations with parsing, classification and assembly across 95 lines. `check_all` mixes
invariant policy with `repo.blob` calls. `build_tree` decides *what* to stage and *writes
it* in the same loop. None of these can be exercised without a real repository, and — the
part that matters — none of them state their dependencies. A function taking `&Repo` can
run `git push`; nothing but review says otherwise.

**Domain policy leaked into the renderers.** The effort-tier rule of ADR 0006 (a skim
group shows exemplars and defers the remainder) is implemented twice, nearly
character-for-character, in `tui::rows` and `crates/stack`. `h<N>` parsing has seven
copies with four different panic messages. The `class_by_id` index has four. The
duplication is not hypothetical harm: the two renderers have already diverged, and
`crates/stack` renders back-filled groups as `[unclassified]` while the TUI shows them as
ordinary focus groups — the same document, two different answers.

**Consumers re-derive what the engine already knows.** `crates/cli` holds the ADR-0017
review-source state machine inside a clap closure, and a second parser for range syntax
the engine already parses. Its only protection against divergence is that the second
parser's extra arms are currently unreachable.

## Decision

**Business logic owns the trait; the adapter implements it.** Dependency direction is
inverted so implementation depends on domain and never the reverse.

**Ports are consumed by static dispatch — generics, not `dyn`.** The distinction is the
load-bearing part of this record:

- **A generic means dependency inversion.** The trait exists so the implementation
  depends on the domain. There is exactly one production implementation, chosen at
  compile time. Monomorphised: no vtable, no runtime choice. This is the default.
- **`dyn` means polymorphism.** The implementation set is genuinely open and selected at
  run time.

Two seams are polymorphism and **stay `dyn`**, both by earlier recorded decision:
`llm::LlmBackend` (config picks the backend command — ADR 0016) and `lang::Language` /
`LanguageRegistry` (a heterogeneous `Vec<Box<dyn Language>>` plugin set — ADR 0015).
Everything else in this refactor is inversion and takes a generic.

**One type parameter per provider, with a bound list that is the function's budget.**

```rust
pub fn build_tree<G>(git: &G, base: &str, view: &DiffView) -> Result<String, EngineError>
where G: ports::ObjectReader + ports::ObjectWriter + ports::TreeBuilder
```

Not one parameter per trait, and not one god trait. `G` is `gitio::Repo` at every call
site. The value bought is not substitutability — it is that `invariants` can no longer
*express* `git log`, and that the bound list at the top of each function is a reviewable
statement of exactly how much git that function is allowed to touch.

**`gitio::Repo` is the only implementation of the git ports, and a fake git is
forbidden.** ADR 0002 pins the byte-exactness guarantees to real git output "and nothing
else". Invariants 1–4 compare the engine's reconstruction against git's own answer; a
fake git would make them compare the fake against the fake — they would pass while
proving nothing. The ports exist for dependency direction, not for test doubles. Tests
keep using hermetic temporary repositories and real `git`.

**A god port may not grow back.** No `trait Git: ObjectReader + ObjectWriter + …`
convenience supertrait. Seven-bound where-clauses on the spine are the documentation, not
a smell. `TreeBuilder` carries an associated type and is therefore not object-safe, which
makes `Box<dyn TreeBuilder>` impossible by construction.

**Disabling is a constructor, not an `Option`.** No `Option<&Port>` in a domain
signature: it would force `None::<&FsGroupingCache>` turbofishes at every call site and
put a runtime branch back into domain code. `FsGroupingCache::disabled()` is one type
that misses on read and drops on write.

**Two new engine modules, no new crate.** `engine::ports` holds the traits and the small
data shapes they carry; `engine::plan` holds the shared domain policy; `engine::store` is
the filesystem mirror of `gitio`. `engine::schema` is untouched and stays serde-only.

## Why this does not re-open 0018

ADR 0018 is a recorded reversal of boundary-hardening, and this record has to answer it
rather than quietly contradict it. It collapsed `differential-schema` and
`differential-llm` back into engine modules because the isolation "had zero takers, while
the extra crate cost a publish, a version bump line, and cross-crate import ceremony on
every contract change."

Every cost 0018 names is a **crate** cost: a publish, a version bump line, cross-crate
imports. This decision adds no crate. It is method-level dependency inversion inside the
engine, and the dependency direction 0018 fixed — `cli → {tui, stack} → engine` — is
unchanged.

The benefit is also different in kind. 0008's schema crate was speculative: a consumer
who wanted the contract without the plumbing, who never appeared. The problem here is not
speculative — it is four copies of an index, seven of a parse, two of a tier rule, and
two renderers that already disagree about the same document. 0018's own standard was
"arguments that materialise in practice"; these did.

What 0018 does bind is the ambition. `ports` and `plan` are modules under the same
reviewed-discipline rule that `schema` and `llm` already live under. If they ever justify
a crate, that is a later record with evidence, not this one.

## Consequences

- `Repo::run` and `Repo::run_env` become **private to `gitio`**. The raw-git escape hatch
  is sealed by the compiler rather than by review, and that is the migration's completion
  test: `rg '\.run(_env)?\(' crates --glob '!**/gitio.rs'` returns nothing.
- Invariant 4 keeps its independence structurally. `RecountSource` is a **separate trait**
  from `DiffSource` — not one trait with two methods, not one method with a flag — so a
  change to enumeration's argv cannot move both sides of the comparison. Its
  implementation must call git directly, and its return type stays `Vec<u8>` forever: the
  moment a port hands invariant 4 a parsed structure, the counter is no longer
  independent. The argv of the two genuinely differs today; that duplication is deliberate
  and must not be tidied away.
- ADR 0012 is **strengthened**, not merely preserved. `plan::build_view` takes an
  enumeration and returns a view, with no `Config` and no `LanguageRegistry` parameter, so
  "enumeration runs before and independently of config" stops being a property of
  statement order inside a long function and becomes a property of a parameter list. No
  port method takes a `Config`, and no struct bundles a git provider with one.
- The frozen surfaces are unmoved: `schema` is untouched, `lang/generic.rs` is untouched,
  the grouping cache key and prompt bytes are pinned by goldens, and `h<N>` remains the
  wire form for hunk ids while `HunkId` is only ever the in-memory form.
- Renderers are adapters, not domain: `crates/tui` keeps naming `gitio::Repo` concretely.
  `crates/stack` is domain — `build_stack` carries invariants 2, 3 and 4 — so it takes
  bounds like any engine consumer.
- Composition moves to the application layer. The engine stops constructing LLM backends
  from config; `crates/cli` builds one and injects it, which also removes cancellation
  from the pipeline's signature (it was only ever a property of the subprocess).
