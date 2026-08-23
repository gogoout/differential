# 0004 — The shape hash normalises and hashes both sides

Status: accepted

## Context

An early prototype hashed only the *added* lines of each hunk. Every deletion-only hunk
therefore collapsed into a single class — 50 deletion hunks landed in 2 classes — turning
"same shapes, skippable" into a lie: dozens of hunks deleting *different things* are not one
shape. Hashing both sides spread those deletions over 38 classes and raised the total from
149 to 196 on the reference change.

## Decision

The shape key is built from **both** the removed and the added lines, each normalised
(strings → `"S"`, numbers → `N`, identifiers → `I`, whitespace collapsed), sigil-prefixed
(`-`/`+`), sorted — plus the file disposition.

## Consequences

- Deletion-only hunks classify honestly.
- A skim group's promise ("one exemplar verifies the class") is structurally meaningful.
- Note: content-level invariants cannot catch a bad shape hash — this bug produced correct
  trees. Classification correctness rests on this ADR and on `pure_substitution` being
  computed, not claimed.
