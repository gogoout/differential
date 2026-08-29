```
       ___ ________                     __  _       __
  ____/ (_) __/ __/__  ________  ____  / /_(_)___ _/ /
 / __  / / /_/ /_/ _ \/ ___/ _ \/ __ \/ __/ / __ `/ /
/ /_/ / / __/ __/  __/ /  /  __/ / / / /_/ / /_/ / /
\__,_/_/_/ /_/  \___/_/   \___/_/ /_/\__/_/\__,_/_/
```

# differential

`differential` groups the hunks of a large diff by textual shape, labels the groups with
an LLM, and orders them so that definitions precede their references. It renders the
result as a terminal reviewer or as a stack of synthetic git commits.

Enumeration is total: every hunk in the range is assigned to exactly one group, and the
partition is checked by four structural invariants before any output is produced.

https://github.com/user-attachments/assets/0d8dccb7-ddb7-4c71-b6a5-582e9e2a00a7

## Requirements

- `git` on your PATH. All repository access shells out to real git.
- An LLM CLI for the grouping stage. The default is
  [`claude`](https://claude.com/claude-code), run headless with tools denied. Any command
  that takes a prompt on stdin and writes text on stdout works. See [Config](#config).
- Rust stable to build from source. The version is pinned in `rust-toolchain.toml`.

## Install

```sh
cargo install differential
```

That installs two binaries, `dfr` and `differential`. They are the same program.

## Quick start

```sh
cd your-repo
dfr review main..feature
```

That opens the terminal reviewer. Two panes: the reading plan on the left, the diff on the
right. Groups are rated `focus`, `skim` or `noise`; see [How it works](#how-it-works).

The first run on a range calls the LLM once. On a big merge request that takes a minute or
two. A splash screen shows the stages while it runs. The result is cached, so a later run
on the same range does not call the LLM again.

Work down the plan from the top. `tab` switches panes. `j` and `k` move. `space` marks a
hunk's shape class reviewed, so one exemplar clears the whole class. `c` writes a finding
against the line under the cursor, and `F` lists every finding you have written. `y` copies
them all to the clipboard as markdown; over SSH it sends them to your own terminal instead,
and names the command that prints them. `?` shows every key. `q` quits; state is written on
every change.

Run it with no range at all:

```sh
dfr review
```

That opens a picker. Choose the base commit, and tick the box to include uncommitted
work.

Full detail, and every key: [`crates/tui/README.md`](crates/tui/README.md).

### As a commit stack

To read the same plan in an IDE, in `tig`, or with plain `git log`, render it as a stack of
synthetic commits:

```sh
dfr stack main..feature
```

```
refs/review/1a2b3c4-5d6e7f8/stack  (14 commits, 187 hunks, recount 187)
  ...
review with: git log --oneline 1a2b3c4d5e6f..refs/review/1a2b3c4-5d6e7f8/stack
```

Now read the stack like any branch:

```
$ git log --oneline 1a2b3c4d5e6f..refs/review/1a2b3c4-5d6e7f8/stack
f00dfee  [unclassified] 1 hunks carried by no group
0ddba11  [noise] Lockfiles and generated artefacts — folded, 21 hunks
cafe007  [skim 2/2] Import swaps for the renamed module — 38 further hunks, same shapes
beefed5  [skim 1/2] Import swaps for the renamed module — 28 exemplars
add1c7e  [focus] Rework retry handling in the client
decade0  [focus] Introduce the storage backend trait and its implementations
```

Read from the bottom up. Read the `[focus]` commits first. Definitions come before their
callers. Then read one exemplar per shape in `[skim 1/2]`. Then skip `[skim 2/2]` and
`[noise]` on their subject lines alone. Every hunk in them repeats a shape you already
checked.

The stack never touches your worktree, your index, or your branches. It is built with git
plumbing and lands one ref.

Full detail: [`crates/stack/README.md`](crates/stack/README.md).

## Commands

| command | what it does |
|---|---|
| `dfr review [<range>]` | Open the terminal reviewer. With no range it opens a picker. |
| `dfr stack <range>` | Build the review commit stack and land it on a ref. |
| `dfr check <range>` | Run the structural invariants. Use this in CI. |
| `dfr findings <range>` | Print the review's findings as JSON. |
| `dfr clean [--dry-run]` | Delete the regenerable cache. Never touches findings. |

Every command takes `--repo`; all but `dfr clean` also take `--config` and `--user-config`. Exit codes: `0` success,
`1` invariant or pipeline failure, `2` usage or config error.

Full reference, including every flag and every key in the reviewer:
[`crates/cli/README.md`](crates/cli/README.md).

## How it works

The pipeline has four stages.

1. **Enumerate.** Read every hunk from `git diff -U0 --no-renames`. No file is skipped.
   No extension is filtered. Config cannot change this.
2. **Classify.** Give each hunk a **shape class**. A shape class is a hash of the hunk's
   diff text after identifiers and literals are normalised away, on both the removed and
   the added side. Two hunks in one class are textually identical after normalisation.
   Classes are named `C0`, `C1`, and so on, largest first.
3. **Group.** An LLM merges and labels **class ids**. It never sees or names a hunk. So
   it cannot drop one. If it omits a class id, an audit catches that and back-fills the
   class into a must-read group.
4. **Order.** Build a dependency graph from symbol definitions to symbol uses. Sort the
   focus groups foundation-first, so a definition is ordered before its references.

### The three tiers

Every group gets one tier.

| tier | what is read |
|---|---|
| `focus` | Read every hunk, line by line. |
| `skim` | Read one exemplar per shape class. The remainder is deferred. |
| `noise` | Generated content. Folded. No exemplars. |

A fourth label appears in the output: `unclassified`. That is the back-fill group: the
model never named those classes, so nothing rated them. They are read in full.

### Read and skipped hunks

Skim exemplars are read, so a skim total is not a count of what was skipped. Every
document reports the two separately:

- `read_hunks` — focus hunks, plus one exemplar per skim class.
- `skipped_hunks` — skim remainders, plus folded noise.

`skipped_hunks` is the saving. `read_hunks` is not.

## Using it as a library

The engine is a library. The JSON plan document it produces is the contract that every
renderer reads.

```rust
use differential_engine::{gitio::Repo, config::Config, lang::LanguageRegistry,
                          store::OsConfigSource, resolve_range, run_pipeline};

let repo = Repo::open(path)?;
let config = Config::load(&OsConfigSource, repo.root(), None, None)?;
let src = resolve_range(&repo, &["main..feature"])?;   // a ReviewSource
let out = run_pipeline(&repo, &src.base, &src.head, src.kind, &config,
                       &LanguageRegistry::builtin(),
                       &differential_symbols::readers())?;   // the symbol readers
// out.report   — the invariant report, always present
// out.document — Some(PlanDocument), or None if an invariant failed
```

Full surface: [`crates/engine/README.md`](crates/engine/README.md) and
[`spec/consumers.md`](spec/consumers.md).

## Config

Config is optional. Two files exist, split by who owns the setting.

**Repo file** — `.differential.toml` at the repository root. Classification hints only.
Everyone reviewing the repo shares them.

```toml
[classify]
# Extra globs to mark as generated. Generated files fold as noise.
generated = ["**/__snapshots__/**", "migrations/**"]
# Never mark these generated. This wins over everything else.
not_generated = ["important.lock"]
# gitattributes names honoured as a "generated" declaration. This is the
# default: GitHub's convention and GitLab's, because a repo does not choose its
# forge to suit us. Setting the key REPLACES the list, it does not extend it.
attributes = ["linguist-generated", "gitlab-generated"]
```

| key | default | meaning |
|---|---|---|
| `classify.generated` | `[]` | Globs that mark a file as generated. |
| `classify.not_generated` | `[]` | Globs that never mark a file as generated. |
| `classify.attributes` | `["linguist-generated", "gitlab-generated"]` | gitattributes names read as "generated". Setting it **replaces** the list. |

**User file** — `~/.config/differential/config.toml`. It honours `XDG_CONFIG_HOME`. Which
agent to run is your choice, not the repo's. So is how much of a file the reviewer shows.

This whole file is optional. The default agent is Claude Code, headless, allowed to read
the change and the repository — nothing else. `agent` picks between the agents we support
by name; it is not a command line, because the grouping call hands its agent a tool
allowlist and a prompt written for what that agent can do.

```toml
[grouping]
agent = "claude-code"   # the only one so far, and the default
timeout_secs = 1200

[review]
# Context lines shown either side of a hunk in `dfr review`.
context = 3
# Lines that one `z` pulls in at a context boundary.
context_step = 10
```

| key | default | meaning |
|---|---|---|
| `grouping.agent` | `claude-code` | Which agent runs the grouping call, by name. |
| `grouping.timeout_secs` | `1200` | How long to wait for the backend. |
| `review.context` | `3` | Context lines around a hunk before any expansion. |
| `review.context_step` | `10` | Lines one `z` pulls in at a boundary row. |

A missing file means defaults. A malformed file is a hard error. An unknown key is a hard
error too.

Which agent you run is part of the grouping cache key, so two agents never share an entry —
a different model may group differently. Where its binary happens to live is not, so a
cache survives a rebuild, a reinstall and a second checkout.

Config never removes a file or a hunk from analysis. It tunes classification hints and
tool behaviour only. Every invariant depends on that.

## The crates

| crate | what it is |
|---|---|
| [`differential`](crates/cli/README.md) | The application. It owns the `dfr` and `differential` binaries. |
| [`differential-engine`](crates/engine/README.md) | The core library: git io, diff parsing, shape classes, grouping, ordering, invariants. |
| [`differential-stack`](crates/stack/README.md) | The shadow-branch renderer. The diff as a synthetic commit stack. |
| [`differential-symbols`](crates/symbols) | Symbol readers: tree-sitter, and a crude fallback. |
| [`differential-tui`](crates/tui/README.md) | The terminal reviewer behind `dfr review`. |

Dependency direction is strict: `cli → {tui, stack} → engine`.

## Status

Shipped: the full pipeline, the review TUI (`dfr review`) with findings that survive
regeneration, and the shadow-branch renderer (`dfr stack`).

Planned: posting grouped review comments to a GitLab merge request or a GitHub pull
request.

## Name

> A differential is a gear system in vehicles that lets driven wheels rotate at different
> speeds while still receiving power from the engine.

The word already contains "diff", and the gear is the arrangement: each group is read at
its own speed, and every hunk is still carried.

## Learn more

- [`docs/architecture.md`](docs/architecture.md) — how it works, and why it is built this
  way.
- [`spec/`](spec/) — the normative behaviour: the JSON contract, the invariants, each
  pipeline stage.
- [`adr/`](adr/) — decision records, with the measurements behind them.
- [`CREDITS.md`](CREDITS.md) — third-party code and prior art.

## Development

Install the binaries from your working tree:

```sh
cargo install --path crates/cli
```

Then:

```sh
cargo test                                # unit tests and hermetic repo tests
cargo clippy --all-targets && cargo fmt
```

Changes land by pull request. CI runs format, clippy, tests and a release build on every
pull request. `main` is protected.

Releases are tag-driven. Bump the workspace version in a pull request, merge it, then push
a `vX.Y.Z` tag. The Release workflow writes the changelog into a GitHub Release and runs
`cargo publish --workspace`.

See [`AGENTS.md`](AGENTS.md) for the working rules.

## Licence

MIT or Apache-2.0, at your option. See [`LICENSE-MIT`](LICENSE-MIT) and
[`LICENSE-APACHE`](LICENSE-APACHE).
