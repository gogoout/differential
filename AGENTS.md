# Agent instructions for `differential`

Read [`README.md`](README.md) for what this is. `spec/` is normative (what the program does);
`adr/` records why (0001–0016). When your change contradicts a spec or ADR, the docs and the
code must change together — or the change is wrong.

## Working rules

1. **Separate essential from accidental complexity — and escalate the essential.**
   Accidental complexity (ceremony, indirection, duplicated state) you remove yourself.
   Essential complexity is a real design decision with real trade-offs: stop and ask the
   human before committing to one. Do not silently pick a side on a decision the author
   would want to make.

2. **Prefer the simple solution. No new abstractions without a demonstrated reason.**
   The abstractions that exist (`Language`, `LlmBackend`, the schema crate boundary) were
   author decisions with recorded rationale. A new trait, layer, or indirection needs the
   same bar: a concrete second consumer or a recorded decision — not "we might need it".
   If you feel a "manager", "helper", or "context" struct forming, stop.

3. **Don't hand-roll utilities — find an established open-source crate.**
   Before writing a parser, encoder, globber, retry loop, or similar plumbing, look for the
   boring, widely-used crate (as `globset`, `tempfile`, `regex` already are here). A
   hand-rolled utility is only acceptable when the crate genuinely doesn't fit, and then say
   so in a comment. Exception: the deliberately dumb recount in `invariants.rs` must stay
   independent of everything — that separation is its whole point (invariant 4).

4. **Don't artificially minimise blast radius.** If a feature genuinely touches a wide
   area, refactor properly rather than patching around the edges to keep the diff small.
   A narrow patch that leaves the design wrong is more expensive than the wide diff that
   fixes it. (This tool exists precisely because wide, honest diffs are reviewable.)

## Non-negotiable project constraints

- **Privacy.** The validation corpus is a private employer repo. Nothing committed — code,
  comments, docs, fixtures, commit messages — may reference its MR numbers, SHAs, company
  name, paths, or change-specific file/crate names. Private test data lives only in
  gitignored `*.local.toml`. Before any push: `git grep` the tracked tree for those markers.
- **Enumeration is total.** No extension filters, no path exclusions, not even via config
  (ADR 0005, 0012). Config and language plugins tune classification only.
- **The invariants stay.** Every one caught a real bug (`spec/invariants.md`). Never weaken,
  skip, or tautologise them; the recount must never share code with the parser.
- **The schema is frozen.** Additive changes only; anything breaking bumps
  `schema_version`. Consumer conveniences must not leak into `crates/schema` (ADR 0008).
- **The generic normaliser is frozen.** `lang/generic.rs` is pinned to the validated
  prototype for hash parity; improvements land as language plugins with their own ids
  (ADR 0015). The real-corpus parity test's exact class count is the guard.
- **Git access shells out to real git, plumbing commands only** (ADR 0002, 0011). Bytes
  in/out; UTF-8 only at display boundaries.
- **The core is a library** (ADR 0014). Binaries belong to renderers: `dfr` carries the
  render surfaces (`stack`, `check`, later the TUI) and stays presentation-only — pipeline
  logic lives in the engine.

## Commands

```sh
cargo test                                  # unit + synthetic-repo integration tests
cargo clippy --all-targets && cargo fmt     # keep both clean
cargo run -q --bin dfr -- check <base>..<head>    # invariant runner
cargo run -q --bin dfr -- stack <base>..<head>    # review stack (needs an LLM CLI on a cache miss)
cargo run -p differential-engine --example group -- <base>..<head>   # grouped document JSON (dev)
DIFFERENTIAL_FIXTURE_CONFIG=$PWD/fixtures.local.toml cargo test -- --ignored  # parity (local)
```

A change is done when: tests pass, clippy/fmt are clean, the parity test still matches
exactly (when the local fixture repo is available), and the privacy sweep finds nothing.
