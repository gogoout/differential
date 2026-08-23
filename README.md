# differential

**Review large diffs as an ordered, honest reading plan.**

`differential` looks at a big merge request, works out which changes are the same edit
repeated (a rename sweeping through imports, a signature change echoing through call
sites), which are generated noise, and which few genuinely need close reading — then
renders the result as a **review commit stack** you read natively in your IDE, `tig`, or
plain `git log`. Coverage is guaranteed structurally: every hunk is accounted for, audited,
and byte-exactly reconstructible, so skipping what it says to skip is safe.

## Requirements

- `git` on PATH (all repository access shells out to real git)
- Rust (stable, pinned in `rust-toolchain.toml`) to build
- an LLM CLI for the grouping step — by default [`claude`](https://claude.com/claude-code),
  invoked headless with tools denied; any prompt-in/text-out command works (see
  Configuration)

## Install

```sh
cargo install --path crates/cli
```

This installs `dfr` (and `differential`, the long name).

## Quick start

```sh
cd your-repo
dfr stack main..feature
```

First run on a range calls the LLM once (a minute or two on a big MR); the grouping is then
cached, so re-runs are instant and stable. Output:

```
refs/review/1a2b3c4-5d6e7f8/stack  (14 commits, 187 hunks, recount 187)
  ...
review with: git log --oneline 1a2b3c4d5e6f..refs/review/1a2b3c4-5d6e7f8/stack
```

Then review the stack like any branch:

```
$ git log --oneline 1a2b3c4d5e6f..refs/review/1a2b3c4-5d6e7f8/stack
f00dfee  [unclassified] 1 hunks carried by no group
0ddba11  [noise] Lockfiles and generated artefacts — folded, 21 hunks
cafe007  [skim 2/2] Import swaps for the renamed module — 38 further hunks, same shapes
beefed5  [skim 1/2] Import swaps for the renamed module — 28 exemplars
add1c7e  [close] Rework retry handling in the client
decade0  [close] Introduce the storage backend trait and its implementations
```

Read bottom-up: `[close]` commits first (ordered so definitions precede their consumers),
then one exemplar per shape in `[skim 1/2]`, and skip `[skim 2/2]` and `[noise]` on their
subject lines alone — every hunk in them is a repeat of a shape you already verified.

### Commands

```sh
dfr stack [--ref <name>] [--no-cache] <range>   # build + land the review stack
dfr check [--json] <range>                      # run the structural invariants (CI-friendly)
```

- `<range>`: `base..head`, `a...b` (base = merge-base, i.e. what an MR/PR diff is), or two
  revs.
- `--repo <path>` / `--config <path>` work on every command; the repo defaults to the one
  containing your cwd.
- `dfr stack --ref refs/review/my-review/stack` picks the ref; default is
  `refs/review/<base7>-<head7>/stack`. Re-running moves the ref.
- `--no-cache` forces a fresh LLM grouping.
- Exit codes: 0 success, 1 invariant/pipeline failure, 2 usage or config error.

The stack never touches your worktree, index, or branches — it is built entirely with git
plumbing and only lands a ref.

## Configuration (optional)

Drop a `.differential.toml` at the repo root:

```toml
[classify]
# Extra globs to mark as generated (folded as noise in the reading plan).
generated = ["**/__snapshots__/**", "migrations/**"]
# Never mark these generated, even if a builtin rule says so.
not_generated = ["important.lock"]
# gitattributes names honoured as "generated" declarations.
attributes = ["linguist-generated"]

[grouping]
# Any prompt-on-stdin / text-on-stdout command. Default shown.
command = ["claude", "-p", "--output-format", "text", "--allowed-tools", ""]
timeout_secs = 1200
```

Config can tune classification and the backend — it can never exclude files from analysis.

## Using it as a library

The engine is a library first; the JSON plan document it produces is the contract every
renderer consumes:

```rust
use differential_engine::{gitio::Repo, config::Config, lang::LanguageRegistry,
                          resolve_range, run_pipeline};

let repo = Repo::open(path)?;
let config = Config::load(repo.root(), None)?;
let (base, head, kind) = resolve_range(&repo, &["main..feature"])?;
let out = run_pipeline(&repo, &base, &head, kind, &config, &LanguageRegistry::builtin())?;
```

Full surface: [`spec/consumers.md`](spec/consumers.md).

## Status

Shipped: the full pipeline (enumeration → shape classes → LLM grouping → foundation-first
ordering) and the shadow-branch renderer (`dfr stack`). Planned: a review TUI (joining the
`dfr` binary) and posting grouped review comments to GitLab/GitHub.

## Learn more

- [`docs/architecture.md`](docs/architecture.md) — how it works and why it's built this way
- [`spec/`](spec/) — normative behaviour (JSON contract, invariants, each pipeline stage)
- [`adr/`](adr/) — decision records with the measurements behind them

## Development

```sh
cargo test                                # unit + hermetic synthetic-repo tests
cargo clippy --all-targets && cargo fmt
```

Changes land via pull request; CI runs format, clippy, tests and a release build on every
PR, and main is protected. Releases are cut manually: bump the workspace version, merge,
and dispatch the Publish workflow, which runs `cargo publish --workspace`.

See [`AGENTS.md`](AGENTS.md) for working rules and
[`docs/architecture.md`](docs/architecture.md) for the testing philosophy. Before touching
`crates/schema` or the invariants, read the ADRs — every invariant caught a real bug.
