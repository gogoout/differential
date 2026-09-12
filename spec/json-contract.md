# The JSON contract (schema v3)

The document the engine produces. Types live in `engine::schema`
(`crates/engine/src/schema.rs`); this file is the prose
contract. The schema is frozen: breaking changes bump `schema_version`, additive changes do
not (readers tolerate unknown fields, and must reject versions they do not know).

## Conventions

- Optional fields serialise as explicit `null`, never omitted.
- All ids (`h0…`, `C0…`, `g0…`) are document-local. Hunk and class ids are **positional** and
  do not survive regeneration; `hunks[].digest` is the stable anchor.
- `h<N>` is the wire form of a hunk id and is frozen. The engine parses it into a `HunkId`
  (`engine::plan`) for its own use, but that type never crosses a serde boundary: every id
  written to or read from a document is the `h<N>` string described here (ADR 0020).
- `base`/`head` are fully resolved commit shas — except for `source.kind` of `staged` or
  `worktree` (ADR 0017), where they are the synthesized snapshot **tree** oids.
- `source.kind` is `range`, `commit`, `staged`, `worktree`, `pr` or `mr`. For `pr` and `mr`
  the range came from a forge and `source.remote` is `{forge, project, id}` — `github` or
  `gitlab`, the project path (`owner/repo`), and the request number as a string
  ([forge.md](forge.md), ADR 0029). For every other kind `source.remote` is `null`.

## `generator`

`{tool, version, stages}` — `stages` lists exactly the pipeline stages that ran
(`enumerate`, `classify`, `group`, `order`). A consumer must consult this rather than
guessing from field presence.

## `groups: null` vs `[]`

`null` means the grouping stage has not run: the document is a complete, classified
enumeration and a consumer should treat every hunk as `focus` in canonical order.
`[]` means the stage ran and there was nothing to group — valid **only** for an empty diff
(zero hunks); on a non-empty diff an empty list is a bug, because the audit back-fills
everything the model drops. The same rule applies to `reading_plan`.

## `files[]`

Canonical (`--no-renames`) view: a rename appears as a `D` entry plus an `A` entry. The
rename-detected (`-M`) view annotates both sides:

- the `A` side carries `old_path` and `rename_similarity` (0–100),
- the `D` side carries `new_path` and the same `rename_similarity`.

This makes "moved and modified" addressable from both ends. **A similarity below ~95 is a
modification, not a relocation, and must never be treated as skim-eligible** — the grouping
layer enforces this; the core only records the number.

`generated` + `generated_by` (`builtin | attr | config`) are computed hints for the `noise`
tier — from a built-in artefact list, a gitattributes attribute, or the repo's
`.differential.toml`. They are never claimed by a model, and they never affect enumeration.

Zero-hunk files are real entries: empty-file add/delete, mode-only changes, binary files.

`submodule` entries carry `{old, new}` commit ids; their pseudo-hunk ("Subproject commit"
lines) is kept in the canonical hunk count but excluded from byte reconstruction.

## `hunks[]`

Canonical enumeration from `git diff -U0 --no-renames`, every file, no exclusions.

- `digest` — exact content hash of the hunk's removed ++ added bytes (un-normalised). Stable
  across regenerations; comments and review state anchor to it (see
  [persistence.md](persistence.md)).
- `nonl_old` / `nonl_new` — the `\ No newline at end of file` marker, per side. Worth exactly
  one byte each in reconstruction.
- `forge_position` — `{new_line, old_line}`: the hunk's first line on each side, in the
  canonical `--no-renames` view. `new_line` is null for deletion-only hunks; `old_line` for
  insertion-only. **Not a posting anchor.** A renamed-and-edited file is a whole-file delete
  plus add in this view, so both read `1` there; the forge consumer positions a comment by a
  finding's anchor and the file entry's `old_path` instead ([forge.md](forge.md)). The field
  stays because the schema is frozen (ADR 0022).

## `classes[]`

The mechanical partition. Every hunk appears in exactly one class; ids `C0…Cn` numbered by
descending member count.

`defines` and `depends_on` are the class dependency graph (ADR 0022), computed from classes
before the grouping stage runs — so it is a fact about the diff and never changes with how
the model grouped. `defines` lists the symbols the class introduces. Each `depends_on` entry
is `{on, via}`: the class consumed, and the symbols that produced the edge. Only a symbol
defined by exactly one class creates an edge. Extraction is heuristic and its precision is
low (ADR 0015), which is why `via` exists — an edge can be judged by its cause.

`pure_substitution` is **computed, never claimed**: after erasing identifiers and literals
from both sides, the removed and added lines match. A group that is not mostly
pure-substitution must not promise "read one exemplar, trust the rest". Insertion-only and
deletion-only hunks are never pure.

## `symbols` (ADR 0032)

The same extraction as the class graph, one class apart: `classes[].depends_on` says *which
class* depends on which, and this says *which token on which line* resolves to which
declaration. Produced by `classify`, so it needs no entry of its own in `generator.stages`.
It never feeds the ordering.

`null` on a document written before the field existed — it is additive, so `schema_version`
stays 3, and a consumer must tolerate its absence. Stored documents are re-read
(`dfr agent --doc`, the grouping cache), so this is a real case and not a theoretical one.

**`file` is an INDEX into `files[]`, not a path.** On a change of any size this section
holds thousands of rows, and repeating the path was 56% of its bytes on the validation
corpus; the document already lists every file exactly once. It therefore differs from
`hunks[].file`, which is a path string and frozen that way — the inconsistency is
unavoidable, and this is the side where the repetition is large enough to matter.

- `definitions[]` — `{id, name, file, line, through, start, end, class}`. Ids are `s0…sn`,
  document-local and positional like `h<N>` and `C<N>`, and do not survive regeneration.
  `line` is the new-side line of the declaring token; `through` is the last line of what the
  name declares, and **equals `line` where the reader could not see an extent** — a regex
  has no tree to ask. A declaration may sit on **any** line of a file the change touches, not
  only one the change wrote — a new call to an existing helper is the commonest thing a
  reviewer wants resolved. `class` is the shape class that wrote the declaring line, and
  **`null` where the change did not write it**: the declaration is real and simply not part
  of the change. Only names with exactly ONE definer appear, the same rule that draws an
  edge. A definition **nothing in `uses` points at is absent** — it could never be shown.
- `uses[]` — `{on, file, line, start, end}`, where `on` is a `definitions[].id`. Recorded
  **only on a line the change wrote**. Those are the lines being reviewed, and they bound the
  index: with any declaration resolvable, every mention in every parsed file would grow this
  with the size of the files. An older call site therefore has no entry. **A declaration is
  never a use of itself** — matched on position, so the same name genuinely used again on its
  own declaring line still counts.

**`start` and `end` are byte offsets into the RAW line**, before any tab expansion. A
renderer that expands tabs — the TUI does — must translate them against its own expansion
rather than index its display text with them. This is the one place the two coordinate
systems meet, and getting it wrong mis-highlights every tab-indented file without erroring.

Only files the change touches are parsed, so a name declared in an untouched file resolves
to nothing — the honest limit of reading a diff rather than a repository.

The index is the largest thing this document carries on a change of any size. Measured on
the validation corpus it takes a 199-class document from 146KB to 238KB (+63%), and a second
range from 180KB to 210KB (+17%). `dfr agent` does not print it, so the grouping prompt is
unaffected.

## `groups[]` and `reading_plan[]` (grouping stage)

- `effort`: `focus` (read every hunk) | `skim` (one exemplar per shape class) | `noise`
  (generated content, folded entirely — no exemplars).
- `role`: `foundation | consumer | mechanical | noise` — filled by the ordering stage
  ([ordering.md](ordering.md)); isolated focus groups and the back-fill stay `null`.
- `class_ids` is ordered foundation-first within the group, by the same stage.
- `depends_on` is the class graph contracted onto groups, which may contain cycles — a
  consumer walking them must not assume otherwise. Each entry is `{on, via, cycle}`.
  `cycle` is `null` except on an edge the sort could not honour: `artefact` when the class
  graph is acyclic and the cycle came from contracting classes into groups, `mutual` when
  the classes deadlock too. *Whether* an edge was honoured is derivable from `rank`, so it
  is not recorded twice.
- `pivot` counts the leading `class_ids` that depend on nothing ranked later — where the
  group stops being a foundation and starts being a consumer. `null` unless the sort broke
  a cycle on this group. Nothing splits the group; the number says where it cannot be read
  as one thing.
- `rank` is the final reading-order position, and it is always a total order.
- `reading_plan` actions: `read`, `exemplars`, `skip`, `fold`.
- Any class the model omitted lands in a trailing back-filled group with `effort: focus`.
  Nothing is ever dropped. Full stage semantics: [grouping.md](grouping.md).

## `audit`

Structural fields exist on every document: `applier_exact` ("n/n"), `tree_assertion`
("pass"), `hunks_carried`, `recount` (independently computed from git output). The
LLM-coverage fields (`coverage`, `classes_missing`, `classes_duplicated`,
`classes_hallucinated`, `read_hunks`, `skipped_hunks`) are null until the grouping stage
runs.
