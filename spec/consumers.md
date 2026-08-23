# Consuming the engine

The core is a **library** (ADR 0014). Consumers — the TUI, the shadow-branch builder, the
forge poster — link `differential-engine` and `differential-schema` directly; the JSON form
of the document is for export and persistence, not inter-process plumbing. The binary
namespace (`dfr`) is reserved for renderers and arrives with the TUI crate.

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
- Language plugins (ADR 0015) and, later, LLM backends (`differential-llm`, ADR 0016) are
  injected by the consumer; `LanguageRegistry::builtin()` and
  `CommandBackend::claude_cli()` are the defaults.

## Dev/CI entry point

The invariant runner is an example, not a product:

```sh
cargo run -p differential-engine --example check -- [--repo <path>] [--config <path>] <base>..<head>
```

Exit codes: 0 all invariants pass, 1 violation or error, 2 usage/config error.

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

Sections reserved for later milestones (documented so the file format is stable):
`[grouping]` (backend command, cache), `[ordering]`, `[stack]` (ref namespace).
