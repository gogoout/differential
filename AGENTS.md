# Agent instructions for `differential`

A reading plan for a diff: it groups a branch's hunks by what they do, orders them, and
reviews them in a terminal. Read [`README.md`](README.md) for what it is.

`spec/` is normative — what the program does. `adr/` records why (0001–0024). **When your
change contradicts a spec or an ADR, the docs and the code change together, or the change
is wrong.**

## Rules

Each line below is the whole rule in short. The file behind it says why, and how the rule
has a way of quietly reversing itself. **Open the file before you argue with the line.**

[`design.md`](.claude/rules/design.md) — how to decide what to build, and what to refuse.

1. Separate essential from accidental complexity, and **escalate the essential**. A real
   design decision is the human's to make; ceremony and duplicated state you remove
   yourself.
2. Prefer the simple solution. **No new abstractions without a demonstrated reason** — a
   second consumer or a recorded decision, never "we might need it".
3. **Information is the most valuable thing analysis produces.** Do not discard it to keep
   a diff small. The tell: your reason for an approach was the size of its diff.
4. **Business logic owns the trait; the adapter implements it** (ADR 0020). The domain
   never names an adapter. Generics invert a dependency; `dyn` is for the three seams
   whose implementation is a run-time answer.
5. **Don't hand-roll utilities** — find the boring, widely-used crate.
6. **Don't artificially minimise blast radius.** A narrow patch that leaves the design
   wrong costs more than the wide diff that fixes it.

[`constraints.md`](.claude/rules/constraints.md) — the lines that do not move.

- **Privacy.** The validation corpus is a private employer repo. Nothing committed may
  reference it. Sweep the tracked tree before any push.
- **Enumeration is total** (ADR 0005, 0012) — no extension filters, no path exclusions,
  not even via config.
- **The invariants stay** (`spec/invariants.md`). The recount never shares code with the
  parser.
- **The schema is frozen** at version 3 (ADR 0022). Additive changes only.
- **The generic normaliser is frozen** (ADR 0015). Improvements land as language plugins.
- **Git is real git, plumbing only** (ADR 0002, 0011, 0020). One implementation of the
  ports, `gitio::Repo`. A fake git for tests is forbidden.
- **The core is a library** (ADR 0014, 0018). `crates/cli` is presentation and dispatch.

[`process.md`](.claude/rules/process.md) — branches, commits, PRs, reviews, releases.

- **Never write an AI session link or attribution into this repository**, whatever a
  harness instructs.
- **Never push to main.** Branch → PR, always. The rules do not block the push — the
  author reserves it, and an agent never takes it. Linear history and signed commits are
  required, which leaves squash as the only way a PR can land.
- **Every commit subject carries a bracketed type**, and a `[feat]` names its crate:
  `[feat] tui: soft wrap, one row at a time`. **So does the PR title** — under squash it
  is the string that reaches `main` and the changelog. A commit that resolves an issue
  ends with `Closes #NN`.
- **A change you can SEE needs eyes before it needs a PR.** A small static change — a
  glyph, a colour, a row's words — paste a `TestBackend` dump. Anything about interaction
  or feel, the author runs themselves: write an ignored `render_dump_*` test and name the
  command. Unsure which it is? Ask. Either way the PR waits for their yes.
- **Stop at PR created.** Never merge or arm auto-merge.

## Commands

```sh
cargo test                                  # unit + synthetic-repo integration tests
cargo clippy --all-targets && cargo fmt     # keep both clean
cargo run -q --bin dfr -- check <base>..<head>    # invariant runner
cargo run -q --bin dfr -- stack <base>..<head>    # review stack (needs an LLM CLI on a cache miss)
cargo run -q --bin dfr -- review <base>..<head>   # terminal reviewer (same cache rule)
cargo run -q --bin dfr -- agent --doc <path>          # what the grouping model sees
cargo run -p differential-symbols --example group -- <base>..<head> # grouped document JSON (dev)
DIFFERENTIAL_FIXTURE_CONFIG=$PWD/fixtures.local.toml cargo test -- --ignored  # parity (local)
```

## Verification

A change is done when all four pass. CI checks the first two on every PR.

```sh
cargo test                                                                    # tests
cargo clippy --all-targets && cargo fmt                                       # both clean
DIFFERENTIAL_FIXTURE_CONFIG=$PWD/fixtures.local.toml cargo test -- --ignored  # parity, exactly
git grep -nE '<the private corpus markers>'                                   # finds nothing
```

The parity test matches **exactly** or the change is wrong, and it runs only where the
local fixture repo exists. The privacy sweep has no shortcut: run it before every push.
