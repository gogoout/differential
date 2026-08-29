# The ordering stage

Reorders the grouped document foundation-first, so the reviewer meets the abstraction
before its consumers. The measured failure this fixes: in model order, the group
introducing the trait everything else consumed landed 9th of 13.

Deterministic and model-free; runs unconditionally after grouping inside
`run_grouped_pipeline`. Appends `"order"` to `generator.stages`.

## It does not build the graph

`artefact::graph` does, from **classes**, before the model runs (ADR 0022) — so the model
reads the same edges the ordering acts on, and grouping cannot change what depends on what.
This stage contracts that graph onto groups.

`Language::file_symbols` (ADR 0015) is asked once per changed file, and is handed the
**whole file** from the head tree — not a hunk and not a line. A line inside a block comment
or a multi-line string is indistinguishable from code on its own, so the two cuts worth
making (drop comment and string tokens; keep only call and type positions) are not
decidable per line. It answers per new-side line number, and each class reads the lines its
member hunks added.

The generic default is still a per-line pass, byte-identical to the validated prototype:

- `defines` — names introduced by declaration keywords (generic heuristic:
  `fn/struct/enum/trait/class/interface/type/def/func/impl/const/static/mod/module/package/protocol`
  + identifier),
- references — identifiers used.

Two categories contribute **no symbols at all**: generated content (a lockfile would
otherwise appear to define half the dependency tree) and gitlinks, whose only added line is
`Subproject commit <oid>` — diff prose about a commit this repository does not have, whose
words are plausible identifiers. Both are skipped where the classes are read, not only
where the blobs are, because a category excluded from the read still reaches the fallback.

Beyond those, a file is not read when it cannot contribute: binaries carry no lines, and a
file whose every hunk is a pure deletion has no added line to attribute. When a head blob
is absent, or disagrees with the diff about how long the file is, the class falls back to
the generic per-line pass over the hunk's own added lines. Fewer symbols than a parse would
find, never none.

Only symbols defined by **exactly one class** create edges; a symbol two classes define is
ambiguous and is dropped. `B depends_on A` when B references a symbol only A defines, and
the edge records the symbols that produced it. Precision is allowed to be low (ADR 0007): a
wrong edge misorders; it can never hide content. No indexer — see the no-SCIP note in
ADR 0015.

## Reorder

Only the **contiguous focus prefix** is reordered (skim/noise/back-fill placement is fixed
by the grouping stage; the audit back-fill group always stays trailing). Kahn's topological
sort, foundation-first; among ready groups the tie-break is descending hunk count, then
original model order.

Each group's `class_ids` is sorted foundation-first too, by the same rule. Those are the
intra-group edges the old group-level union discarded, and they are the only thing that can
order a group's members.

## Cycles

A cycle means no reading order satisfies every edge. The stage says which kind it is
rather than picking on size and staying silent.

- **`artefact`** — the class graph is acyclic here. Contracting classes into groups made
  the cycle: one group both defines and consumes, against the same other group. The class
  order decides which group is emitted first.
- **`mutual`** — the classes deadlock too. The mutual dependency is in the change, and the
  deterministic fallback (largest remaining group, ties by original position) is as good an
  answer as there is.

The verdict lands on `Edge.cycle`, and only on an edge the sort could not honour. *Whether*
it could is derivable from `rank`, so it is not recorded twice.

`pivot` counts the leading `class_ids` that depend on nothing ranked later — where the group
stops being a foundation and starts being a consumer.

**Nothing splits a group.** The merge is the model's judgement (ADR 0001), and the graph
that would undo it is heuristic. A wrong edge misorders; a wrong cut would break a coherent
group and mislabel both halves. The impossibility is information the reviewer wants.

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
called out. `via` says which symbol produced the edge, so a reader can judge it rather than
trust it. The ordering does not affect the grouping cache key: cached groupings are
re-ordered on every load by the same deterministic pass.
