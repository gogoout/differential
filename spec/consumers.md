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
dfr findings [--repo <path>] [--config <path>] [--summary] [--no-cache] <range>
dfr check [--repo <path>] [--config <path>] [--json] <range>
dfr agent --doc <path>
dfr clean [--repo <path>] [--dry-run]
```

- `review` opens the terminal reviewer ([tui.md](tui.md)); `findings` prints the review's
  findings as re-anchored JSON, or as markdown with `--summary` — the open ones as
  `- file:lines: note`, which is the same text the reviewer's `y` copies. Both come from
  `ReviewSession`, so the projection has one owner and the two cannot drift.
- `clean` deletes the regenerable cache ([persistence.md](persistence.md)) and reports
  what went. It takes no range — the cache belongs to the repository, not to a review —
  and it never touches findings, which live in a sibling tree.
- `stack` builds and lands the review commit stack ([stack.md](stack.md)), printing the
  commit list and the `git log` line to review with. The grouping backend comes from
  `[grouping].agent` (default: `claude-code`, a headless invocation with read-only tools); the pinning cache
  lives under `<git-common-dir>/differential/cache/grouping` unless `--no-cache`. The
  document the model reads sits beside it, under `…/cache/document`.
- `check` runs the core pipeline and reports invariants 1–4 — the self-test and CI entry
  point.
- `agent` is the grouping model's read path (ADR 0022), not a human one, and it is the
  whole of that path: one command, one answer, no sub-questions. It prints every class the
  model may group, in full — id, hunk count, file count, disposition, exemplar location,
  then every member hunk with its file and line range, then every file, with `defines:`,
  `uses:` and `used by:` lines. That is 72KB for a 196-class change, beside
  the 322KB of diff the model reads anyway, so slicing it into four queries bought three
  extra model turns and nothing else. The prompt names the running executable, and the
  default backend's allowlist is derived from the same string, so the two cannot disagree
  about what the model may invoke. Generated content is left out, as the prompt's id list
  leaves it out. It takes no `--repo`: every answer comes from the document, diff text is
  `git diff`'s job, and so it opens no repository, never runs the pipeline and never calls
  a model — a grouping run cannot recurse into itself. An empty change prints a plain
  sentence and exits 0 — to an agent a blank reply reads as "the tool is broken".
- Exit codes: 0 success/all pass, 1 invariant or pipeline failure, 2 usage/config error.

## Library surface

```rust
use differential_engine::{gitio::Repo, config::Config, lang::LanguageRegistry,
                          store::OsConfigSource, resolve_range, run_pipeline};

let repo = Repo::open(path)?;                       // any dir inside the repo
let config = Config::load(&OsConfigSource, repo.root(), None, None)?; // or defaults
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
- `run_grouped_pipeline(…, &GroupingOptions { backend, cache, artefacts, progress })`
  additionally runs the grouping stage ([grouping.md](grouping.md)). `artefacts` is where
  the pre-group document is left for the model to read (`store::FsArtefactStore`); like the
  cache, disabling it is a state of the store, not an `Option`. All are **injected**: the engine
  no longer builds a backend from `[grouping].agent` — composition is the application
  layer's job (ADR 0020) — and disabling the cache is
  `store::FsGroupingCache::disabled()` rather than an absent one, so the stage never grows
  a branch for `--no-cache`. Cancellation belongs to the backend
  (`CommandBackend::with_cancel`), since the thing that needs killing is the subprocess.
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
# gitattributes attribute names honoured as "generated" declarations. Shown at
# their default: GitHub's convention and GitLab's, because a repository does not
# choose its forge to suit this tool. Setting the key REPLACES the list.
attributes = ["linguist-generated", "gitlab-generated"]
```

**User-level** — `~/.config/differential/config.toml` (honours `XDG_CONFIG_HOME`).
The agent backend: a per-user choice, never a repo setting — not everyone uses the same
agent. Resolution: `--user-config` path > the XDG location > built-in default.
A `[grouping]` table in the REPO file is a hard error with a migration hint.

```toml
[grouping]
# Which agent runs the grouping call. Supported: "claude-code" (the default) —
# a headless Claude Code invocation allowed to read the change and the
# repository and nothing else. A name nobody implements is a hard error that
# says which ones exist.
agent = "claude-code"
# How long to wait for it. Default 1200s. This one is a number because it tunes
# the agent rather than replacing it.
timeout_secs = 1200
```

`agent` is a **name, not an argv**, and that is the whole point. The grouping stage does
not merely spawn a process: it hands the agent a tool allowlist, a fetch command and a
prompt written for what that agent can do (ADR 0022). An arbitrary argv got the prompt and
none of the rest, so it was a knob that looked like it worked. Adding an agent is adding a
`config::Agent` variant and the arm in `backend_from` the compiler then demands.

Because the backend's **identity** is part of the grouping cache key, users running
different agents get separate cache entries in the clone's shared cache — correct, since a
different model may group differently. Identity is not the command as it runs: the default
backend's argv names the executable the prompt tells the model to fetch with, and where
that binary lives determines nothing, so it is held out of the key
(`LlmBackend::identity`). A cache therefore survives a rebuild, a reinstall and a second
checkout, which is what `plan::grouping_cache_dir` promises by living under the git common
directory.

A missing file means defaults; a malformed file is a hard error, never silently ignored.
**The one hard rule: config can never remove a file or hunk from enumeration.**
Enumeration is total, always — every invariant depends on it (ADR 0012). Config tunes
classification hints and tool behaviour only.

Sections reserved for later milestones (documented so the file format is stable):
`[ordering]`, `[stack]` (ref namespace) in the repo file.
