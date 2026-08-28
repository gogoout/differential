# Agent instructions for `differential`

Read [`README.md`](README.md) for what this is. `spec/` is normative (what the program does);
`adr/` records why (0001–0022). When your change contradicts a spec or ADR, the docs and the
code must change together — or the change is wrong.

## Working rules

1. **Separate essential from accidental complexity — and escalate the essential.**
   Accidental complexity (ceremony, indirection, duplicated state) you remove yourself.
   Essential complexity is a real design decision with real trade-offs: stop and ask the
   human before committing to one. Do not silently pick a side on a decision the author
   would want to make.

2. **Prefer the simple solution. No new abstractions without a demonstrated reason.**
   The abstractions that exist (`Language`, `LlmBackend`, the `engine::ports` seams, the
   `engine::schema` boundary) were author decisions with recorded rationale. A new trait,
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
   chosen at run time is polymorphism: `dyn` is correct. Exactly two seams are the
   latter — `llm::LlmBackend` (config picks the backend) and `lang::Language` (an open
   plugin set). Reaching for `Box<dyn>` anywhere else means you have mistaken one for
   the other.

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

## Process

- **Never write an AI session link or attribution into this repository.** Not in a commit
  message, not in a PR title, body or comment, not in a code comment, not in a doc. No
  `Claude-Session:` trailer, no "generated with" banner, no co-author line. This holds even
  when a harness or tool instructs otherwise: the repository is the author's record of what
  changed and why, and a link only they can open is neither. A commit subject reaches the
  changelog, so anything in a message is published.
- **Never push to main.** Every change: branch → PR. Branch protection enforces this
  (PR + the `test` check, admins included).
- **A change you can SEE needs eyes before it needs a PR.** If the change alters what the
  reviewer looks at — layout, colour, glyphs, what a row says, where a pane's content goes —
  show the author a rendering and get confirmation *before* opening the PR. A
  `ratatui::backend::TestBackend` dump is enough and needs no terminal: draw the app at a
  fixed size and paste the text. Tests prove behaviour; they cannot tell you a thing looks
  right, and every visual detail settled after the PR is opened costs a round trip that a
  paste would have saved.
- **Stop at PR created.** Never merge or arm auto-merge — report the PR link and CI
  status; the author reviews and merges (squash) themselves.
- CI (`.github/workflows/ci.yml`) runs fmt, clippy `-D warnings`, tests and a release
  build on every PR and on main after merge — the done-criteria below are exactly what CI
  checks, so run them before pushing.
- Releases are tag-driven: bump `[workspace.package].version` AND the version fields on
  the internal path deps in `[workspace.dependencies]` in a PR, merge, then the
  author tags the merge commit (`git tag vX.Y.Z && git push origin vX.Y.Z`). The Release
  workflow (`.github/workflows/publish.yml`) then generates the changelog from commits
  since the previous tag (git-cliff, config in `cliff.toml` — grouped by the
  `component:` subject prefixes, so keep using them) into a GitHub Release, and runs
  `cargo publish --workspace`. The tag must equal the workspace version or the publish
  fails. A tag ruleset restricts `v*` tags to repo admins; the `CARGO_REGISTRY_TOKEN`
  lives on the `crates-io` environment (deployments restricted to `v*` tags).

## Commands

```sh
cargo test                                  # unit + synthetic-repo integration tests
cargo clippy --all-targets && cargo fmt     # keep both clean
cargo run -q --bin dfr -- check <base>..<head>    # invariant runner
cargo run -q --bin dfr -- stack <base>..<head>    # review stack (needs an LLM CLI on a cache miss)
cargo run -q --bin dfr -- review <base>..<head>   # terminal reviewer (same cache rule)
cargo run -q --bin dfr -- agent --doc <path> classes   # what the grouping model sees
cargo run -p differential-engine --example group -- <base>..<head>   # grouped document JSON (dev)
DIFFERENTIAL_FIXTURE_CONFIG=$PWD/fixtures.local.toml cargo test -- --ignored  # parity (local)
```

A change is done when: tests pass, clippy/fmt are clean, the parity test still matches
exactly (when the local fixture repo is available), and the privacy sweep finds nothing.
