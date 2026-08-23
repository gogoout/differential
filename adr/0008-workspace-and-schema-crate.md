# 0008 — Multi-crate workspace; the schema is its own crate

Status: accepted

## Context

The JSON document is the product; three consumers (shadow branch, TUI, forge) are views over
it. The main design risk named in the project brief is a consumer's convenience leaking into
the schema.

## Decision

Cargo workspace with strict one-way dependencies: `cli → engine → schema`.
`differential-schema` contains only the serde types of the frozen contract and depends only
on serde/serde_json. Future crates (`grouping`, `ordering`, `stack`, `tui`, `forge`) slot in
beside them; consumers depend on `schema` without pulling git plumbing.

## Consequences

- Changing the contract requires touching the schema crate — visible in review.
- Placeholder crates are not created ahead of need.
