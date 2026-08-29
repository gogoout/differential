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

Symbol extraction is a **domain use case with pluggable readers** (ADR 0023). The graph asks
`SymbolReaders::of_file` once per changed file, handing it the **whole file** from the head
tree — not a hunk and not a line. A line inside a block comment or a multi-line string is
indistinguishable from code on its own, so the two cuts worth making are not decidable per
line. The answer is per new-side line number, and each class reads the lines its member
hunks added.

Each reader answers `priority(path)` with how good its answer would be, or nothing if it
does not read that file. The rule is: **ask the best claimant, fall to the next best if it
fails, and if nobody claims the file, take no symbols from it.** A reader ranks itself, so
no wiring order can get the ranking wrong.

Three readers ship. Which one answered is not a distinction this stage can see:

| reader | reads | definitions | references |
| --- | --- | --- | --- |
| tuned | Rust, TypeScript (+TSX), Python, Go, Kotlin | from the tree, per query | calls and types, per query |
| field-rule | JavaScript, Java, C, C++, C# | from the tree | calls and types, from field names |
| crude | any other source extension | declaration keywords | every identifier ≥ 4 chars |

**A definition is a file-scope name others can use.** `mod template;` is not one — it names
a module. `fn from` inside an `impl` is not one — it is reached through its type. Counting
those made a single common word into a globally unique symbol that every file mentioning it
then linked to; six such words produced 64% of one corpus range's edges.

**Comments and strings contribute nothing**, which needs no query: every grammar names its
comment and string nodes with those words. A token reaching a string through an
interpolation is still code, so `"${resolve(id)}"` keeps its call.

Two categories contribute **no symbols at all** whatever the readers say: generated content
(a lockfile would otherwise appear to define half the dependency tree) and gitlinks, whose
only added line is `Subproject commit <oid>` — diff prose about a commit this repository
does not have, whose words are plausible identifiers. Both are skipped where the classes are
read, not only where the blobs are.

Beyond those, a file is not read when it cannot contribute: binaries carry no lines, and a
file whose every hunk is a pure deletion has no added line to attribute.

Withholding symbols is **classification, never enumeration**. The file, its hunks and its
classes all still exist (ADR 0005, 0012).

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
