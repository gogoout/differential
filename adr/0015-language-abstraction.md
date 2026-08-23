# 0015 — Language abstraction: a registry of plugins over a generic default

Status: accepted

## Context

The tool must eventually support every language. Two stages are language-shaped: shape
normalisation today (what counts as "the same edit"), and symbol extraction for the ordering
stage later (definition → use edges between groups). The validated generic byte normaliser
works across languages but is a floor, not a ceiling.

## Decision

A `Language` trait (`engine::lang`) with a `LanguageRegistry`:

- Every method has a working generic default; a plugin implements only what it improves.
  Today that is `normalize_line`; ordering hooks are added later with defaults.
- The registry falls back to `Generic` for unclaimed files. With no plugins registered,
  behaviour is byte-identical to the validated milestone-1 normaliser — the real-corpus
  parity test (exact class count) enforces this.
- Languages influence **classification only**. They never see enumeration (ADR 0005/0012),
  and they never touch `hunk_digest`, the exact-content persistence anchor (ADR 0013).
- `LanguageRegistry::fingerprint()` identifies the normalisation behaviour; anything pinned
  to a partition (the grouping cache, ADR 0009) must include it in its key, so groupings
  from different normaliser versions never mix silently.

## Consequences

- Adding a language is additive: implement `claims` + overrides, register, done.
- The generic normaliser in `lang/generic.rs` is frozen; improvements land as plugins with
  their own ids, never as in-place edits.
