# 0011 — Plumbing commands over porcelain

Status: accepted

## Context

`git diff` is porcelain: its output is affected by user configuration — external diff
drivers, color, pager settings, mnemonic prefixes — any of which breaks a parser that
targets the default format.

## Decision

Read diffs via plumbing: `git diff-tree -r -U0 --no-renames` (canonical) and
`git diff-tree -r -M --name-status -z` (rename view). Belt and braces: also pass
`--no-color --no-ext-diff -c core.quotepath=false`. Write objects via plumbing only:
`hash-object`, `update-index` against a temporary `GIT_INDEX_FILE`, `write-tree`,
`commit-tree` — no checkout, no branch switching.

## Consequences

- Parser input is stable across machines and user configs.
- Tree building never touches the user's worktree or index.
