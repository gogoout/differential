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

   **The domain must never depend on an adapter. The adapter depends on the domain.** The
   arrow is one-way and has no exceptions. A domain module may not `use crate::gitio`,
   `std::fs`, `std::process`, `std::env`, `etcetera` or `tempfile`; if it needs git, the
   filesystem, a clock or a terminal, it declares a port next to the logic
   (`engine::ports`) and the adapter implements it.

   Four tells that you have it backwards, which is all you need at the keyboard:

   - The port's methods read like the tool (`run_git`, `write_file`) instead of like the
     need (`blob`, `save_state`). That is the adapter wearing a trait.
   - A `Box<dyn>` outside the three seams whose implementation is a genuine run-time
     answer — `llm::LlmBackend`, `lang::Language`, `artefact::symbols::SymbolSource`
     (ADR 0023). Everywhere else a port is a generic: `fn f<G: ObjectReader>(git: &G)`.
   - An `Option<&Port>` in a domain signature. Disabling is a constructor
     (`FsGroupingCache::disabled()`), so the branch lives in the adapter.
   - A bound list merged into a `trait Git: A + B + …` supertrait. The list IS the point:
     it states a function's budget.

   Enforced, not merely stated: `crates/engine/tests/layering.rs` fails if a domain module
   names an adapter, and its `NOT_YET_INVERTED` list is **empty**. Needing to add a line
   to it is the signal, not the fix.

   Shared domain policy lives in `engine::plan`, not in a renderer. Parsing an id or
   deciding what a tier defers inside `crates/tui` or `crates/stack` belongs one layer
   down — that duplication is what let the two renderers disagree about one document.

   [ADR 0020](../../adr/0020-ports-and-static-dispatch.md) argues all of this at length,
   and says why `gitio::Repo` must stay the only implementation of the git ports. Read it
   before arguing with any line above; do not restate it here.

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

7. **Name the contradiction; do not hide behind it.** When a request contradicts a spec, an
   ADR, or a decision recorded as frozen, say so plainly, say what that decision was
   protecting, and say what reversing it would cost. Then evaluate the request on its
   merits.

   "The spec says otherwise" is a fact to put on the table. It is never an argument on its
   own. Everything in `spec/` and `adr/` is a record of past reasoning by the author, and
   the author is entitled to overturn any of it. Refusing on the strength of the record
   alone hands them back their own words instead of an answer.

   What a flag owes the reader: which document, what it decided, what it was guarding
   against, and what would have to change alongside. A reversal lands with the docs in the
   same change (the line at the top of [`AGENTS.md`](../../AGENTS.md)), and a superseded ADR
   gets a successor that says why it was wrong.

   The tell that you got this wrong: your objection restates the rule and adds no fact to
   it.

   The worked example is the request to make the pipeline read-only. `spec/invariants.md`
   said all four invariants run before any document is emitted, and the schema is frozen at
   version 3. Both were true and neither was a reason. The real question was which consumers
   invariants 3 and 4 actually protect — and answering it made the split correct, while
   turning up an invariant (1b) that had been implemented and never written down. An
   objection that stopped at "the spec says all four run" would have cost the change and
   found nothing.
