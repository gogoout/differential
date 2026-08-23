# 0016 — LLM backend abstraction: a narrow one-shot completion contract

Status: accepted

## Context

The grouping stage must not be welded to one model CLI. At the same time, the contract must
not grow surface that reintroduces the failure ADR 0010 eliminated: every observed hard
failure of the evaluated grouping tools was a model CLI stopping to call a tool.

## Decision

A separate crate, `differential-llm`, so the deterministic engine never depends on model
concerns. Its whole contract is:

```rust
trait LlmBackend { fn name(&self) -> &str; fn complete(&self, prompt: &str) -> Result<String, LlmError>; }
```

One-shot: prompt in, raw text out. No tools, no streaming, no chat state — a backend that
cannot express tools cannot stop to call one.

The default implementation is `CommandBackend`, a subprocess with the prompt on stdin and the
completion on stdout, with a kill-on-deadline watchdog and threaded i/o (no pipe deadlocks in
either direction). `CommandBackend::claude_cli()` constructs the validated invocation
(headless, text output, tools denied).

## Consequences

- The grouping crate (next milestone) consumes the trait; swapping providers is a
  constructor, and its `[grouping]` config selects the command.
- Response parsing (fence stripping, JSON extraction, coverage audit) is grouping-layer
  logic, deliberately outside this crate.
