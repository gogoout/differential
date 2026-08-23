# The grouping stage

Turns the mechanical class partition into labelled, effort-rated groups. The model merges
and labels **class ids, never hunks** (ADR 0001): it cannot drop what it never names, and
anything it omits is detected and back-filled. Runs inside the engine
(`run_grouped_pipeline`); the backend is any `LlmBackend` (ADR 0016), defaulting to the
tools-denied claude invocation (ADR 0010), configurable via `[grouping].command`.

## What the model never sees or cannot override

- **Noise is mechanical** (ADR 0006). A class whose hunks all live in `generated` files is
  pre-assigned to one folded noise group and never appears in the payload. A class spanning
  generated and non-generated files stays with the model.
- **The relocation gate** (ADR 0003). A class touching a file whose `rename_similarity` is
  below 95 is a modification, not a relocation. The payload announces the rename
  ("renamed from …, N% similar") so the model can judge correctly — and a deterministic
  post-audit pass extracts any such class out of a skim group into a synthesized close group
  ("Modified during move") regardless of what the model claimed.

## Payload

One block per offered class, largest first: `[Cn] count= files= kind= e.g. path:line`
header (plus the rename note), up to four removed and four added exemplar lines (both sides
— a deletion-only hunk is otherwise invisible), a basename list for multi-file classes.
Capped at 90k chars; classes cut by the cap become audit-missing and are back-filled into a
must-read group, so truncation can never lose a hunk.

## Audit — nothing is ever dropped

Against the offered id set: hallucinated ids are removed (and listed), duplicated ids are
kept by their first group (and listed), missing ids land in a trailing
`effort: close` back-fill group (invariant 5). `audit.coverage` is the honest pre-back-fill
number: model-assigned hunks / offered hunks. Unknown effort strings mean `close`
(when in doubt, close).

## Assembly

Group order is presentation only (the foundation-first DAG is the ordering stage): close
groups in model order (gate group last), skim groups by descending hunk count, the noise
group, then the back-fill. `role` is `null` except the noise group (`"noise"`);
`depends_on` is empty until the ordering stage exists.

Reading plan: close → `read`; skim → `exemplars`, plus `skip` only when a remainder exists;
noise → `fold`. `audit.read_hunks` counts close hunks plus one exemplar per skim class;
`audit.skipped_hunks` counts skim remainders plus folded noise — only the latter is the
genuine saving (ADR 0006).

An empty diff produces `groups: []` — the one case where an empty list is valid: the stage
ran and there was nothing to group.

## Cache (ADR 0009)

Labels are non-deterministic across model calls; coverage is structural. Groupings are
pinned under `<git-common-dir>/differential/cache/grouping/<key>.json`, keyed by sha1 over:
`PROMPT_VERSION`, the backend name, the language-registry fingerprint, and each offered
class's sorted member hunk digests (content-exact, so keys survive positional-id shifts
across regenerations). The cached value is the **raw model response**; parsing, audit, the
gate and assembly are pure functions replayed on every load — their fixes apply to cached
runs, while prompt or payload changes must bump `PROMPT_VERSION`.

A cache hit makes the grouped document fully deterministic. No auto-retry on a malformed
response: the error carries a response sample and the caller decides.
