# The JSON contract (schema v2)

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
- `forge_position` — `{new_line, old_line}` for posting comments against a forge's
  rename-detected diff. `new_line` is null for deletion-only hunks; `old_line` for
  insertion-only.

## `classes[]`

The mechanical partition. Every hunk appears in exactly one class; ids `C0…Cn` numbered by
descending member count.

`pure_substitution` is **computed, never claimed**: after erasing identifiers and literals
from both sides, the removed and added lines match. A group that is not mostly
pure-substitution must not promise "read one exemplar, trust the rest". Insertion-only and
deletion-only hunks are never pure.

## `groups[]` and `reading_plan[]` (grouping stage)

- `effort`: `focus` (read every hunk) | `skim` (one exemplar per shape class) | `noise`
  (generated content, folded entirely — no exemplars).
- `role`: `foundation | consumer | mechanical | noise` — filled by the ordering stage
  ([ordering.md](ordering.md)); isolated focus groups and the back-fill stay `null`.
  `depends_on` edges form the group DAG; `rank` is the final reading-order position.
- `reading_plan` actions: `read`, `exemplars`, `skip`, `fold`.
- Any class the model omitted lands in a trailing back-filled group with `effort: focus`.
  Nothing is ever dropped. Full stage semantics: [grouping.md](grouping.md).

## `audit`

Structural fields exist on every document: `applier_exact` ("n/n"), `tree_assertion`
("pass"), `hunks_carried`, `recount` (independently computed from git output). The
LLM-coverage fields (`coverage`, `classes_missing`, `classes_duplicated`,
`classes_hallucinated`, `read_hunks`, `skipped_hunks`) are null until the grouping stage
runs.
