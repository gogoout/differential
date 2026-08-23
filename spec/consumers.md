# Consuming the engine

The core is a **library** (ADR 0014). Consumers — the TUI, the shadow-branch builder, the
forge poster — link `differential-engine` and `differential-schema` directly; the JSON form
of the document is for export and persistence, not inter-process plumbing. The binary
namespace (`dfr`, also installed as `differential`) belongs to renderers; the shadow-branch
renderer is its first occupant, and the TUI joins it later.

## The renderer binary

```sh
dfr review [--repo <path>] [--config <path>] [--no-cache] <range>
dfr stack [--repo <path>] [--config <path>] [--ref <name>] [--no-cache] <range>
dfr findings [--repo <path>] [--config <path>] [--no-cache] <range>
dfr check [--repo <path>] [--config <path>] [--json] <range>
```

- `review` opens the terminal reviewer ([tui.md](tui.md)); `findings` prints the review's
  findings as re-anchored JSON.
- `stack` builds and lands the review commit stack ([stack.md](stack.md)), printing the
  commit list and the `git log` line to review with. The grouping backend comes from
  `[grouping].command` (default: the tools-denied claude invocation); the pinning cache
  lives under `<git-common-dir>/differential/cache/grouping` unless `--no-cache`.
- `check` runs the core pipeline and reports invariants 1–4 — the self-test and CI entry
  point.
- Exit codes: 0 success/all pass, 1 invariant or pipeline failure, 2 usage/config error.

## Library surface

```rust
use differential_engine::{gitio::Repo, config::Config, lang::LanguageRegistry,
                          resolve_range, run_pipeline};

let repo = Repo::open(path)?;                       // any dir inside the repo
let config = Config::load(repo.root(), None)?;      // .differential.toml or defaults
let (base, head, kind) = resolve_range(&repo, &["main..feature"])?;
let out = run_pipeline(&repo, &base, &head, kind, &config, &LanguageRegistry::builtin())?;
// out.report: InvariantReport — always present
// out.document: Option<PlanDocument> — None iff an invariant failed
```

- `resolve_range` accepts `a..b`, `a...b` (base = merge-base — what an MR/PR diff is), or
  two revs.
- `run_pipeline` runs all invariants before emitting anything; on a violation there is no
  document, only the report saying what failed.
- `run_grouped_pipeline(…, &GroupingOptions { backend, cache_dir })` additionally runs the
  grouping stage ([grouping.md](grouping.md)): `backend: None` builds one from
  `[grouping].command` (default: the tools-denied claude invocation), and the cache
  directory is conventionally `repo.common_dir()?/differential/cache/grouping`.
- Language plugins (ADR 0015) and LLM backends (`differential-llm`, ADR 0016) are injected
  by the consumer; `LanguageRegistry::builtin()` and `CommandBackend::claude_cli()` are the
  defaults.

## Dev entry point

One example remains for debugging the grouped document itself (JSON to stdout):

```sh
cargo run -p differential-engine --example group -- [--repo <path>] [--no-cache] [-o <file>] <base>..<head>
```

## Config: `.differential.toml`

Resolution: explicit path > `<repo-root>/.differential.toml` > built-in defaults.
A missing file means defaults; a malformed file is a hard error, never silently ignored.

```toml
[classify]
# Additive globs marking files as generated (noise-tier hint).
generated = ["**/__snapshots__/**", "migrations/**"]
# Overrides: never mark these generated, wins over builtins/attributes/globs.
not_generated = ["important.lock"]
# gitattributes attribute names honoured as "generated" declarations.
attributes = ["linguist-generated"]
```

**The one hard rule: config can never remove a file or hunk from enumeration.** Enumeration
is total, always — every invariant depends on it (ADR 0012). Config tunes classification
hints and tool behaviour only.

```toml
[grouping]
# Backend argv: prompt on stdin, completion on stdout. Default: the validated
# tools-denied claude invocation. Timeout default: 1200s.
command = ["claude", "-p", "--output-format", "text", "--allowed-tools", ""]
timeout_secs = 1200
```

Sections reserved for later milestones (documented so the file format is stable):
`[ordering]`, `[stack]` (ref namespace).
