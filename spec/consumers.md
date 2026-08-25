# Consuming the engine

The core is a **library** (ADR 0014, 0018). Consumers — the TUI (`differential-tui`), the
shadow-branch builder (`differential-stack`), the forge poster — link `differential-engine`
directly (the frozen contract lives in `engine::schema`); the JSON form of the document is
for export and persistence, not inter-process plumbing. The binaries (`dfr`, also installed
as `differential`) live in the application-layer `crates/cli`, which consumes the renderer
crates.

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
let config = Config::load(repo.root(), None, None)?; // repo + user config, or defaults
let src = resolve_range(&repo, &["main..feature"])?;   // a ReviewSource
let out = run_pipeline(&repo, &src.base, &src.head, src.kind, &config,
                       &LanguageRegistry::builtin())?;
// out.report: InvariantReport — always present
// out.document: Option<PlanDocument> — None iff an invariant failed
```

- `resolve_range` accepts `a..b`, `a...b` (base = merge-base — what an MR/PR diff is), or
  two revs, and returns a `plan::ReviewSource`: the endpoints (`base`, `head`, `kind`) plus
  the review's **identity** (`head_spec`, the head as typed, and `identity_base`). The two
  are separate because reviewing uncommitted work diffs against synthesized trees that churn
  on every edit while the review itself must survive (ADR 0017). `resolve_picked` builds the
  same type from the picker's answer.
- `run_pipeline` runs all invariants before emitting anything; on a violation there is no
  document, only the report saying what failed.
- `run_grouped_pipeline(…, &GroupingOptions { backend, cache_dir })` additionally runs the
  grouping stage ([grouping.md](grouping.md)): `backend: None` builds one from
  `[grouping].command` (default: the tools-denied claude invocation), and the cache
  directory is conventionally `plan::grouping_cache_dir(&repo.common_dir()?)`.
- Language plugins (ADR 0015) and LLM backends (`engine::llm`, ADR 0016/0018) are injected
  by the consumer; `LanguageRegistry::builtin()` and `CommandBackend::claude_cli()` are the
  defaults.

## Dev entry point

One example remains for debugging the grouped document itself (JSON to stdout):

```sh
cargo run -p differential-engine --example group -- [--repo <path>] [--no-cache] [-o <file>] <base>..<head>
```

## Config: two files, split by ownership

**Repo-level** — `.differential.toml` at the target repo's root. Classification hints
only: shared by everyone reviewing the repo. Resolution: `--config` path >
`<repo-root>/.differential.toml` > built-in defaults.

```toml
[classify]
# Additive globs marking files as generated (noise-tier hint).
generated = ["**/__snapshots__/**", "migrations/**"]
# Overrides: never mark these generated, wins over builtins/attributes/globs.
not_generated = ["important.lock"]
# gitattributes attribute names honoured as "generated" declarations.
attributes = ["linguist-generated"]
```

**User-level** — `~/.config/differential/config.toml` (honours `XDG_CONFIG_HOME`).
The agent backend: a per-user choice, never a repo setting — not everyone uses the same
agent. Resolution: `--user-config` path > the XDG location > built-in default.
A `[grouping]` table in the REPO file is a hard error with a migration hint.

```toml
[grouping]
# Backend argv: prompt on stdin, completion on stdout. Default: the validated
# tools-denied claude invocation. Timeout default: 1200s.
command = ["claude", "-p", "--output-format", "text", "--allowed-tools", ""]
timeout_secs = 1200
```

Because the backend command is part of the grouping cache key, users running different
agents get separate cache entries in the clone's shared cache — correct, since a
different model may group differently.

A missing file means defaults; a malformed file is a hard error, never silently ignored.
**The one hard rule: config can never remove a file or hunk from enumeration.**
Enumeration is total, always — every invariant depends on it (ADR 0012). Config tunes
classification hints and tool behaviour only.

Sections reserved for later milestones (documented so the file format is stable):
`[ordering]`, `[stack]` (ref namespace) in the repo file.
