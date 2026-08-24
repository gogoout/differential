# Review instructions

This file guides automated code review only (CLAUDE.md/AGENTS.md govern all other
agent work; violations of those are findings here too).

Reserve **Important** for: correctness bugs, weakening or skipping a structural
invariant (`spec/invariants.md` — never tautologise them; the recount must stay
independent of the parser), breaking the frozen schema contract (`engine::schema`
is additive-only; breaking changes need a `schema_version` bump and an ADR),
excluding anything from enumeration (enumeration is total — no extension filters
or path exclusions, ADR 0005/0012), touching the frozen generic normaliser
(`lang/generic.rs` is pinned for hash parity), or porcelain git usage (plumbing
only, ADR 0002/0011).

**Privacy is Important, always**: nothing may reference the private validation
corpus — MR numbers, SHAs, company name, or repo-specific file/crate names from
it. Flag anything that looks like leaked private-repo detail.

Style, naming and refactoring suggestions are **Nit** at most; report at most 5
Nits per review. Skip `crates/tui/src/vendor/**` for style (vendored MIT code,
kept close to upstream deliberately).

When a change contradicts `spec/` or `adr/`, the docs and code must change
together — flag a mismatch as Important.
