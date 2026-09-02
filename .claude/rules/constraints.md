# Non-negotiable constraints

Each of these was a decision, and each erodes in practice unless it is re-read — not
because anyone overturns it, but because the shortcut past it always looks local and small.
That is what this file is for. **A line here yields only to the author saying so, in the
conversation where it comes up** (design rule 7); an argument earns that conversation, never
the change. Privacy and enumeration's totality yield to nothing.
Linked from [`AGENTS.md`](../../AGENTS.md), which carries the one-line form.

- **Privacy.** The validation corpus is a private employer repo. Nothing committed — code,
  comments, docs, fixtures, commit messages — may reference its MR numbers, SHAs, company
  name, paths, or change-specific file/crate names. Private test data lives only in
  gitignored `*.local.toml`. Before any push: `git grep` the tracked tree for those markers.
- **Enumeration is total.** No extension filters, no path exclusions, not even via config
  (ADR 0005, 0012). Config and language plugins tune classification only.
- **The invariants stay.** Every one caught a real bug (`spec/invariants.md`). Never weaken,
  skip, or tautologise them; the recount must never share code with the parser.
- **The schema is frozen.** Additive changes only; anything breaking bumps
  `schema_version` (it is 3, ADR 0022). Consumer conveniences must not leak into
  `engine::schema` (ADR 0008, 0018).
- **The generic normaliser is frozen.** `lang/generic.rs` is pinned to the validated
  prototype for hash parity; improvements land as language plugins with their own ids
  (ADR 0015). The real-corpus parity test's exact class count is the guard.
- **Git access shells out to real git, plumbing commands only** (ADR 0002, 0011). Bytes
  in/out; UTF-8 only at display boundaries. Domain code reaches git through the
  `engine::ports` traits, whose only implementation is `gitio::Repo`; never add a second
  one, a fake git included (ADR 0020). The migration completes when `Repo::run` is
  private to `gitio` — until then, don't add call sites outside it.
- **The core is a library** (ADR 0014, 0018). Renderers are library crates
  (`crates/stack`, `crates/tui`); `crates/cli` is the application layer owning the
  `dfr`/`differential` binaries — presentation and dispatch only, pipeline logic lives
  in the engine.
