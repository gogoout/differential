# The ordering stage

Reorders the grouped document foundation-first, so the reviewer meets the abstraction
before its consumers. The measured failure this fixes: in model order, the group
introducing the trait everything else consumed landed 9th of 13.

Deterministic and model-free; runs unconditionally after grouping inside
`run_grouped_pipeline`. Appends `"order"` to `generator.stages`.

## Edges

Per non-noise group, over the **added** lines of its member hunks, the `Language` hooks
(ADR 0015) extract:

- `defs` — names introduced by declaration keywords (generic heuristic:
  `fn/struct/enum/trait/class/interface/type/def/func/impl/const/static/mod/module/package/protocol`
  + identifier),
- `refs` — identifiers referenced.

Only symbols defined by **exactly one** group create edges (multi-definer symbols are
noise). `B depends_on A` when B references a symbol only A defines. Precision is allowed to
be low (ADR 0007): a wrong edge misorders; it can never hide content. No indexer — see the
no-SCIP note in ADR 0015.

## Reorder

Only the **contiguous focus prefix** is reordered (skim/noise/back-fill placement is fixed
by the grouping stage; the audit back-fill group always stays trailing). Kahn's topological
sort, foundation-first; among ready groups the tie-break is descending hunk count, then
original model order. A dependency cycle (heuristic noise) is broken deterministically on
the largest remaining group — the recorded `depends_on` edges keep the truth.

## Roles

- focus group that at least one other group depends on → `foundation`
- focus group with only outgoing dependencies → `consumer`
- skim → `mechanical`; the noise group keeps `noise`; isolated focus groups and the
  back-fill stay `null`.

`rank` is rewritten to the final order; group ids are stable; the reading plan is
re-grouped to follow (per-group step sequences unchanged).

## Consumers

`depends_on` is emitted so a renderer can show the *chain*, not just the sequence — "this
group exists because of that one" — which is the legibility gap the validation session
called out. The ordering does not affect the grouping cache key: cached groupings are
re-ordered on every load by the same deterministic pass.
