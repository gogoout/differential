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
(`-`/`+`), sorted — plus the file disposition and whether the file is **generated**.

## The generated flag is in the key, and it is the one non-textual part

A class is a unit of routing, not only a unit of text. The noise tier assigns whole classes
(ADR 0006), so a class that is generated only in places has no honest destination.

That case was real. A lockfile line and a source line can normalise to the same shape, and
without this component they became one class. `plan::class_is_generated` could then only ask
"is every member generated?" — which such a class answers no. So it was offered to the model,
and its lockfile hunk went into whatever group the model chose. A generated hunk in a focus
group was the symptom, and no amount of prompting could fix it: every offered id must appear
in exactly one group, and a class the model declines to name is back-filled into a *focus*
group by invariant 5.

The cost is stated rather than hidden. `generated` is a **hint** — built-in list,
gitattributes, repo config — so a `[classify]` glob now moves a class boundary, not only a
routing decision. Two things bound that. It is classification, which is exactly what config
may tune, and it still cannot add or remove a hunk (ADR 0012). And the hint already decided
what the model was offered, so it already shaped the partition's consequences; this makes it
shape the partition itself, where the answer is exact.

The order is load-bearing: `plan::classify` applies the hints and *then* partitions. Reversed,
nothing would fail — every file would read as not generated and the mixed classes would
quietly return.

## Consequences

- Deletion-only hunks classify honestly.
- A skim group's promise ("one exemplar verifies the class") is structurally meaningful.
- **Every class is wholly generated or wholly not.** `plan::class_is_generated` cannot come
  back half true, and no offered class contains a generated hunk.
- A shape that appears in both a generated and a source file counts twice. On the reference
  change that is three classes becoming six.
- Note: content-level invariants cannot catch a bad shape hash — this bug produced correct
  trees. Classification correctness rests on this ADR and on `pure_substitution` being
  computed, not claimed.
