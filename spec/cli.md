# CLI

Two binaries from one entry point: `dfr` (short, used in all docs) and `differential`.

## Commands (milestone 1)

```
dfr plan  [--repo <path>] [--config <path>] [--pretty] [-o <file>] <range>
dfr check [--repo <path>] [--config <path>] [--json] <range>
```

- `<range>`: `<base>..<head>` (exact endpoints), `<a>...<b>` (base = merge-base — what an
  MR/PR diff is), or two positional revs. Revs are resolved via `git rev-parse`.
- `--repo` defaults to the repository containing the current directory.
- `plan` runs the full pipeline **including all invariants**, then writes the JSON document
  to stdout (or `-o`). If any invariant fails: no JSON, non-zero exit, details on stderr.
- `check` runs the same pipeline and prints a human-readable invariant report (counts,
  applier n/n, tree shas, recount) — the self-test and CI entry point. `--json` for machine
  output. Exit 0 only if every invariant passes.

## Exit codes

| code | meaning |
|---|---|
| 0 | success / all invariants pass |
| 1 | invariant failure or unexpected error |
| 2 | usage error (bad range, missing repo, malformed config) |

## Config: `.differential.toml`

Resolution: `--config <path>` > `<repo-root>/.differential.toml` > built-in defaults.
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
is total, always — every invariant depends on it, and path filtering was the single worst
coverage bug found during validation. Config tunes classification hints and tool behaviour
only.

Sections reserved for later milestones (documented so the file format is stable):
`[grouping]` (model command, cache), `[ordering]`, `[stack]` (ref namespace).
