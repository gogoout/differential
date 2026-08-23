# differential

Grouped, ordered reading plans for large diffs.

`differential` turns a big merge request into **one JSON document** describing what a reviewer
should read closely, what can be verified from one exemplar, and what is generated noise —
with 100% hunk coverage guaranteed structurally, never by trusting a model.

The architecture (validated before this implementation; see [`adr/`](adr/)):

1. **Mechanical partition** — every hunk gets a *shape class* (hash of its diff text with
   identifiers and literals normalised away, both sides). Full coverage by construction.
2. **LLM merges and labels class ids, never hunks** — it cannot drop what it never names;
   omissions are detected and back-filled.
3. **Structural audits** — byte-exact reconstruction of every file, a non-tautological tree
   assertion, and an independent recount validate each document before it is emitted.

Three consumers are planned as views over the document: a shadow-branch commit stack (review
natively in your IDE or `tig`), a TUI, and forge review comments (GitLab/GitHub).

## Status

Milestone 1: the frozen JSON contract ([`spec/json-contract.md`](spec/json-contract.md)) and
the core engine — canonical enumeration, dual diff views, shape classes, byte-exact applier,
invariants 1–4. No LLM stage yet: documents carry `groups: null` and
`generator.stages = ["enumerate", "classify"]`.

## Usage

The core is a library; renderers (the TUI, shadow-branch builder, forge poster) link it
directly, and the `dfr` binary arrives with the TUI:

```rust
let repo = Repo::open(path)?;
let (base, head, kind) = resolve_range(&repo, &["main..feature"])?;
let out = run_pipeline(&repo, &base, &head, kind,
                       &Config::load(repo.root(), None)?,
                       &LanguageRegistry::builtin())?;
```

`<a>...<b>` resolves the base via merge-base (what an MR/PR diff is). Full surface and the
per-repo `.differential.toml` config format: [`spec/consumers.md`](spec/consumers.md).

Dev/CI invariant runner:

```sh
cargo run -p differential-engine --example check -- <base>..<head>
```

## Layout

- [`spec/`](spec/) — what the program does (normative).
- [`adr/`](adr/) — why it is this way (decision records).
- `crates/schema` — the frozen JSON contract as serde types. The product boundary.
- `crates/engine` — git io, diff parsing, applier, shape classes, language registry,
  invariants.
- `crates/llm` — the LLM backend abstraction the grouping stage builds on.

## Development

```sh
cargo test                             # unit + synthetic-repo integration tests
cargo test -- --ignored                # real-corpus parity (needs a local fixture
                                       # config; see fixtures.example.toml)
```

Every invariant in [`spec/invariants.md`](spec/invariants.md) caught a real bug during
validation. Keep them all.
