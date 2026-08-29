# differential (`dfr`)

`differential` groups the hunks of a large diff by textual shape, labels the groups with
an LLM, and orders them so that definitions precede their references. It renders the
result as a terminal reviewer or as a stack of synthetic git commits.

Enumeration is total: every hunk in the range is assigned to exactly one group, and the
partition is checked by four structural invariants before any output is produced.

This crate is the application layer. It owns the `dfr` and `differential` binaries. It
parses arguments and dispatches. All the work happens in
[`differential-engine`](https://crates.io/crates/differential-engine).

Project home: <https://github.com/gogoout/differential>

![The dfr review reviewer, mid-review](https://raw.githubusercontent.com/gogoout/differential/main/assets/screenshot.png)

## Install

```sh
cargo install differential
```

That installs two binaries, `dfr` and `differential`. They are the same program.

You also need:

- `git` on your PATH. All repository access shells out to real git.
- An LLM CLI for the grouping stage. The default is `claude`, run headless with tools
  denied. Any command that takes a prompt on stdin and writes text on stdout works.

## Quick start

```sh
cd your-repo
dfr review main..feature
```

That opens the terminal reviewer. Run `dfr review` with no range and it opens a picker
instead.

To read the same plan in your IDE, in `tig`, or in plain `git log`, render it as a stack of
synthetic commits:

```sh
dfr stack main..feature
```

The first run on a range calls the LLM once. The result is cached, so a later run on the
same range does not call it again.

## The five commands

### `dfr review [<range>]`

Open the terminal reviewer over the grouped reading plan. Two panes: the plan on the left,
the diff on the right. You mark hunks reviewed and write findings against lines. Everything
is saved as you go.

With no range it opens a picker instead of failing. See [The picker](#the-picker).

| flag | meaning |
|---|---|
| `--no-cache` | Bypass the grouping cache. This forces a fresh LLM call. |

Full key list and behaviour:
<https://github.com/gogoout/differential/blob/main/crates/tui/README.md>

### `dfr stack <range>`

Build the review commit stack and land it on a ref. Then read it in your IDE, in `tig`, or
with plain `git log`.

| flag | meaning |
|---|---|
| `--ref <name>` | The ref to land on. Default: `refs/review/<base7>-<head7>/stack`. |
| `--no-cache` | Bypass the grouping cache. |

Output:

```
refs/review/1a2b3c4-5d6e7f8/stack  (14 commits, 187 hunks, recount 187)
  decade0    31h  [focus] Introduce the storage backend trait and its implementations
  ...
review with: git log --oneline 1a2b3c4d5e6f..refs/review/1a2b3c4-5d6e7f8/stack
```

The stack never touches your worktree, your index, or your branches. It is built with git
plumbing and lands one ref. Re-running moves the ref.

Full detail:
<https://github.com/gogoout/differential/blob/main/crates/stack/README.md>

### `dfr check <range>`

Run the pipeline and report the structural invariants. This is the self-test and the CI
entry point. It writes nothing.

| flag | meaning |
|---|---|
| `--json` | Print a machine-readable report instead of the text one. |

### `dfr findings <range>`

Print the review's findings as JSON, re-anchored to the current plan. Use this to feed a
review into other tooling.

| flag | meaning |
|---|---|
| `--no-cache` | Bypass the grouping cache. |

Each record carries `{id, created, body, status, moved, plan_hash, anchor}`. The anchor
carries `{file, side, line, end_line, offset, span, hunk_digest, line_text,
end_line_text}`. `hunk_digest` keys back into the plan document's `hunks[].digest`.

### `dfr clean [--dry-run]`

Delete the regenerable cache and report what went. Takes **no range** — the cache belongs
to the repository, not to a review.

| flag | meaning |
|---|---|
| `--dry-run` | Report what would be removed, and remove nothing. |

```
$ dfr clean --dry-run
would remove 93 grouping responses, 2 pre-group documents (1.0 MiB)
```

Two things go: the grouping responses, which cost a model call to rebuild, and the
pre-group documents, which rebuild for free. **Findings never go.** They are not cache and
they live in a sibling tree — see [Where state lives](#where-state-lives).

Clear the cache when a grouping is stale for a reason the cache key cannot see, or to
reclaim the space. A changed normaliser or language plugin does **not** need this: the key
includes the language fingerprint, so those entries go cold on their own.

## Flags every command takes

| flag | default | meaning |
|---|---|---|
| `--repo <path>` | the repo containing your cwd | Which repository to work on. |
| `--config <path>` | `<repo-root>/.differential.toml` | The repo config file. |
| `--user-config <path>` | `~/.config/differential/config.toml` | The user config file. |
| `<range>` | required, except for `review` and `cache` | See below. |

`dfr clean` takes only `--repo`: it reads no config and resolves no range.

A path you pass explicitly must exist. A missing default file just means defaults.

## Range forms

| form | meaning |
|---|---|
| `base..head` | Two endpoints. |
| `a...b` | Base is the merge-base of `a` and `b`. This is what a merge request diff shows. |
| `<rev> <rev>` | Two revisions as two arguments. |

Only `dfr review` may omit the range. Every other command exits `2` without one.

## The picker

`dfr review` with no range opens a picker.

- A list of recent commits. Pick one as the **base**. Branch and tag names are shown. A
  bar marks every commit inside the range as you move.
- A checkbox, **include uncommitted changes (worktree)**, ticked by default. It appears
  only when the worktree is dirty. On a clean tree it could not change anything.

So "everything since `main`, including my uncommitted work" is one choice.

The range is `base..head`. It excludes the base commit's own changes.

| key | action |
|---|---|
| `j` / `↓` | Next commit. |
| `k` / `↑` | Previous commit. |
| `space` | Toggle "include uncommitted changes". Dirty worktree only. |
| `enter` | Use this commit as the base. Start the review. |
| `esc` / `q` | Cancel. |

## Exit codes

| code | meaning |
|---|---|
| `0` | Success. All invariants passed. |
| `1` | An invariant failed, or the pipeline failed. |
| `2` | A usage error or a config error. |

## Config

Two files, split by who owns the setting.

**Repo file** — `.differential.toml` at the repository root. Classification hints only.
Everyone reviewing the repo shares them.

```toml
[classify]
generated = ["**/__snapshots__/**", "migrations/**"]
not_generated = ["important.lock"]
# GitHub's convention and GitLab's, the default. Setting it REPLACES the list.
attributes = ["linguist-generated", "gitlab-generated"]
```

| key | default | meaning |
|---|---|---|
| `classify.generated` | `[]` | Globs that mark a file as generated. Generated files fold as noise. |
| `classify.not_generated` | `[]` | Globs that never mark a file as generated. This wins over everything. |
| `classify.attributes` | `["linguist-generated", "gitlab-generated"]` | gitattributes names read as a "generated" declaration. Setting it **replaces** the list. |

`[ordering]` and `[stack]` are reserved for later work. They are accepted and ignored. A
`[grouping]` table here is a hard error, with a hint telling you to move it.

**User file** — `~/.config/differential/config.toml`. It honours `XDG_CONFIG_HOME`.

Optional. The default agent is Claude Code, headless, allowed to read the change and the
repository and nothing else. `agent` picks by name between the agents we support, not by
command line: the grouping call hands its agent a tool allowlist and a prompt written for
what that agent can do.

```toml
[grouping]
agent = "claude-code"   # the only one so far, and the default
timeout_secs = 1200

[review]
context = 3
context_step = 10
```

| key | default | meaning |
|---|---|---|
| `grouping.agent` | `claude-code` | Which agent runs the grouping call, by name. |
| `grouping.timeout_secs` | `1200` | How long to wait for the backend. |
| `review.context` | `3` | Context lines shown around a hunk before any expansion. |
| `review.context_step` | `10` | Lines one `z` pulls in at a context boundary row. |

Resolution order for each file: the explicit flag, then the default path, then the built-in
defaults. A missing file means defaults. A malformed file is a hard error. An unknown key
is a hard error too.

**Config never removes a file or a hunk from analysis.** It tunes classification hints and
tool behaviour only.

The backend command is part of the grouping cache key. So two people running different
agents get separate cache entries. That is correct: a different model may group
differently.

## Where state lives

Everything lives under the repository's git common directory. Nothing is written to your
worktree.

```
<git-common-dir>/differential/
├── reviews/<review-id>/
│   ├── plans/<content-hash>.json   every generated document, immutable
│   ├── current                     the active plan's content hash
│   ├── findings.jsonl              the findings store
│   └── state.json                  progress and view preferences
└── cache/
    ├── grouping/<classes-hash>.json   the raw model response
    └── document/<content-hash>.json   the pre-group document
```

`cache/` and `reviews/` are siblings on purpose. Everything under `cache/` can be
recomputed; findings cannot. `dfr clean` removes `cache/` and nothing else, so it has no
path by which to reach your findings.

The review id derives from the resolved base sha plus the head **as typed**. So reviewing
`main..feature` keeps one review while `feature` moves.

## Licence

MIT or Apache-2.0, at your option.
