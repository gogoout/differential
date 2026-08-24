# 0012 — Repo config tunes classification, never enumeration

Status: accepted (amended: `[grouping]` moved to the user-level
`~/.config/differential/config.toml` — agents are a per-user choice; the repo file
keeps classification hints only)

## Context

Repos legitimately differ in what counts as generated (snapshot dirs, migration files,
custom gitattributes names). Early prototypes hardcoded such lists — and separately proved
(0005) that excluding paths from enumeration destroys coverage silently.

## Decision

A per-repo `.differential.toml` (repo root of the *target* repo; `--config` overrides)
configures classification hints and tool behaviour: `generated` globs, `not_generated`
overrides, which gitattributes names to honour, and later grouping/ordering/stack options.

**Config can never remove a file or hunk from enumeration.** The engine's API enforces this
structurally: enumeration runs before and independently of config; config feeds only the
hint computation.

## Consequences

- Users get per-repo tuning without a mechanism that can silently hide changes.
- A malformed config file is a hard error, never silently ignored.
