# 0019 — The `focus` tier and schema v2

Status: accepted (amends 0006's vocabulary; the three-tier rationale is unchanged)

## Context

The top effort tier was named `close` ("read closely"). As a bare adjective on group
headers, commit subjects (`[close]`) and the JSON wire (`effort: "close"`) it read
ambiguously — "close" the verb (close the group? closed?) rather than the intended
reading depth. The author picked `focus` from candidates: `focus | skim | noise` reads
as an attention gradient.

## Decision

Rename the tier everywhere, including the wire value:

- `engine::schema`: `Effort::Focus`, wire string `"focus"`, **`schema_version` = 2**.
  Renaming a wire value is breaking, and the frozen-schema rule is additive-or-bump;
  readers already reject unknown versions, and nothing external consumed v1 (published
  for days). No compatibility shim — v1 documents are simply regenerated.
- Grouping prompt: the model is asked for `"skim" | "focus"`; **`PROMPT_VERSION` = 2**,
  which invalidates every grouping cache entry by design (cached responses from the v1
  prompt use the old vocabulary).
- Renderers: `[focus]` commit subjects, `F` tier letter and `[focus]` headers in the
  TUI.

The response mapping is unchanged in shape: anything that is not literally `"skim"` is
treated as focus ("when in doubt, focus"), so even a model that answers with stale
vocabulary degrades safely to the careful tier.

## Consequences

- Old plan documents (v1) in review stores are unreadable by v2 readers; they are
  regenerated on the next open — reviewed marks and findings are unaffected (they key
  on class content and hunk digests, not on plan documents).
- One-time grouping cache miss per review after upgrading.
