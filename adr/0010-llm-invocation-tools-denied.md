# 0010 — The LLM is invoked headless with tools denied

Status: accepted

## Context

Every observed hard failure of the evaluated LLM-grouping tool was the model CLI exiting 1
with `stop_reason: "tool_use"`: the model stops to call a tool and the harness treats that
as fatal.

## Decision

The grouping stage invokes `claude -p --output-format text --allowed-tools ""` — plain
prompt in, text out, no tools. Across all validation runs this configuration never exhibited
the failure. The command is configurable (`[grouping]` in the user-level `~/.config/differential/config.toml`) but the
tools-denied contract is required of any configured command.

## Consequences

- The model cannot wander; latency and cost are bounded by one call per document.
- Output parsing must still tolerate code fences and surrounding prose defensively.
