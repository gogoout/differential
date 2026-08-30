# Design rules

How to decide what to build, and what to refuse. Linked from
[`AGENTS.md`](../../AGENTS.md), which carries the one-line form of each rule.

1. **Separate essential from accidental complexity — and escalate the essential.**
   Accidental complexity (ceremony, indirection, duplicated state) you remove yourself.
   Essential complexity is a real design decision with real trade-offs: stop and ask the
   human before committing to one. Do not silently pick a side on a decision the author
   would want to make.

2. **Prefer the simple solution. No new abstractions without a demonstrated reason.**
   The abstractions that exist (`Language`, `LlmBackend`, `SymbolSource`, the
   `engine::ports` seams, the `engine::schema` boundary) were author decisions with
   recorded rationale. A new trait,
   layer, or indirection needs the same bar: a concrete second consumer or a recorded
   decision — not "we might need it". If you feel a "manager", "helper", or "context"
   struct forming, stop. A `Context` bundling a git provider with a `Config` is the
   specific one to refuse: it re-opens the exclusion hole ADR 0012 closed.

3. **Information is the most valuable thing analysis produces. Do not discard it to
   keep a diff small.**
   Rule 2 forbids inventing structure. It is not licence to take the cheap answer.
   Before choosing an approach, work out what the mechanism can determine *without* the
   model: what the data already supports, what a finer granularity would reveal, what
   the current code computes and then throws away. Then choose. An approach that answers
   less than the mechanism can answer needs a stated reason, exactly as a new abstraction
   does.

   The tell: you recommended the option with the smallest diff, and the diff was your
   reason. State what each option can determine and let the author weigh it.

4. **Business logic owns the trait; the adapter implements it** (ADR 0020).

   **The domain must never depend on an adapter. The adapter depends on the domain.**
   That arrow is one-way and has no exceptions. Concretely: a domain module may not
   `use crate::gitio`, `std::fs`, `std::process`, `std::env`, `etcetera` or `tempfile` —
   if it needs git, the filesystem, a clock or a terminal, it declares a port next to
   the logic (`engine::ports`) and `gitio`/`store` implement that port. The trait is
   defined by the side that *needs* the capability, never by the side that provides it,
   which is what makes the domain compilable and readable without knowing an adapter
   exists.

   The tell that you have it backwards: the trait's methods read like the tool
   (`run_git`, `write_file`) instead of like the need (`blob`, `save_state`). A port
   named after its implementation is the adapter wearing a trait, and the dependency is
   still pointing the wrong way.

   This is enforced, not merely stated: `crates/engine/tests/layering.rs` fails if a
   domain module names an adapter. Its `NOT_YET_INVERTED` list is **empty** — the
   migration is complete — so the test is now an unconditional statement of the rule.
   Needing to add a line to it is the signal you have the arrow backwards.

   **Generics for inversion, `dyn` for polymorphism.** A trait with one production
   implementation, chosen at compile time, exists to invert a dependency: take it as a
   generic (`fn f<G: ObjectReader>(git: &G)`). A trait whose implementation is genuinely
   chosen at run time is polymorphism: `dyn` is correct. Exactly three seams are the
   latter — `llm::LlmBackend` (config picks the backend), `lang::Language` (an open
   plugin set), and `artefact::symbols::SymbolSource` (each reader ranks itself per
   file, so which one answers is a run-time answer to a run-time question — ADR 0023).
   Reaching for `Box<dyn>` anywhere else means you have mistaken one for the other.

   Three rules that follow, each of which has a way of quietly reversing itself:
   - **Name the port for what the caller needs**, not for the thing that implements it.
     Bound lists are the point: they state a function's budget. Never merge them into a
     `trait Git: A + B + …` supertrait for convenience.
   - **No `Option<&Port>` in a domain signature.** Disabling is a constructor
     (`FsGroupingCache::disabled()`), so the branch lives in the adapter, not the domain.
   - **`gitio::Repo` is the only implementation of the git ports.** A fake git for tests
     is forbidden: invariants 1–4 compare the engine against git's own answer, so a fake
     would make them compare the fake against the fake (ADR 0002). Tests use hermetic
     temp repositories and real `git`.

   Shared domain policy lives in `engine::plan`, not in a renderer. If you find yourself
   parsing an id, indexing classes, or deciding what a tier defers inside `crates/tui`
   or `crates/stack`, it belongs one layer down — that duplication is what let the two
   renderers disagree about the same document.

5. **Don't hand-roll utilities — find an established open-source crate.**
   Before writing a parser, encoder, globber, retry loop, or similar plumbing, look for the
   boring, widely-used crate (as `globset`, `tempfile`, `regex` already are here). A
   hand-rolled utility is only acceptable when the crate genuinely doesn't fit, and then say
   so in a comment. Exception: the deliberately dumb recount in `invariants.rs` must stay
   independent of everything — that separation is its whole point (invariant 4).

6. **Don't artificially minimise blast radius.** If a feature genuinely touches a wide
   area, refactor properly rather than patching around the edges to keep the diff small.
   A narrow patch that leaves the design wrong is more expensive than the wide diff that
   fixes it. (This tool exists precisely because wide, honest diffs are reviewable.)
