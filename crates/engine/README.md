# differential-engine

The core library behind [`differential`](https://crates.io/crates/differential). It turns
a git revision range into one JSON document: every hunk, every shape class, the labelled
groups, and the order to read them in.

The engine is a library. Renderers link it and receive the document in-process. The
JSON form is for export and for storage on disk, not for talking between processes.

Project home: <https://github.com/gogoout/differential>

## What it produces

One `PlanDocument`. It describes:

- every changed file and every hunk — complete, with nothing filtered out;
- **shape classes**: hunks that are the same edit after identifiers and literals are
  normalised away;
- **groups**: shape classes merged and labelled, each rated `focus`, `skim` or `noise`;
- a **reading plan**: the groups ordered foundation-first, with dependency edges.

## The pipeline

Four stages. The document records which ones ran, in `generator.stages`.

| stage | what it does |
|---|---|
| `enumerate` | Read every hunk from `git diff -U0 --no-renames`. Merge in the rename-detected view (`-M`) as annotations. |
| `classify` | Assign shape classes. Compute `pure_substitution`. Compute generated-file hints. |
| `group` | Ask an LLM to merge and label class ids. Audit the answer. Back-fill anything missing. |
| `order` | Contract the class dependency graph onto groups. Topologically sort the focus groups. |

### Shape classes

A shape class is **normalised text, then a hash of it** — four regex substitutions over the
raw bytes.

Regex rather than a parser because enumeration is total: every hunk in every file needs a
class, including files no grammar covers. Symbol extraction does parse, and it is a
separate job behind a separate port (`artefact::symbols`, ADR 0023).

`lang::generic::normalize_line` does the substitutions: strings become `"S"`, numbers become
`N`, identifiers of four characters or more become `I`, then whitespace collapses and the
line is trimmed. So both of these lines

```
    let timeout = Duration::from_secs(30);
    let timeout = Duration::from_secs(config.timeout);
```

normalise to `let I = I::I(N);` and `let I = I::I(I.I);`.

`shape::shape_hash` does the framing: prefix removed lines with `-` and added lines with
`+`, sort each side, join with newlines, append the file's disposition letter, sha1, keep
12 hex characters.

Both sides are hashed, not just the added side. Hashing added lines alone would collapse
every deletion-only hunk into one class. The disposition is in the key too, so a whole-file
addition and a modification with identical text are different shapes.

Classes are named `C0`, `C1`, and so on, largest first.

`normalize_line` is pluggable per language. The framing is not, and the generic normaliser
is frozen against the validated prototype for hash parity.

`pure_substitution` is true when the removed and added lines match after that erasure. It
is **computed, never claimed by a model**.

Separately, every hunk carries a `digest`. That is the exact, un-normalised content hash.
It is the stable anchor that review findings key on.

### Why the model never names a hunk

Asking a model to assign hunks to groups fails silently. We measured it: on large
refactors, coverage dropped as low as 27% while the model reported success. An omitted
hunk index looks exactly like one that never existed.

So the model only ever sees and names **class ids**. Anything it omits is detected against
the known id set and back-filled into a trailing must-read group. The model still earns
its keep: merging twenty textually different classes into one "path and import swaps"
group is exactly what hashing cannot do.

Two things the model cannot override:

- **Noise is mechanical.** A class whose hunks all live in generated files is pre-assigned
  to a folded noise group. It never reaches the model at all.
- **The relocation gate.** A class touching a file whose rename similarity is below 95 is a
  modification, not a move. A deterministic pass after the model runs pulls any such class
  out of a skim group and into a synthesized focus group.

### The three tiers

| tier | what a reader does |
|---|---|
| `focus` | Read every hunk, line by line. |
| `skim` | Read one exemplar per shape class. Trust the rest. |
| `noise` | Generated content. Folded entirely. No exemplars. |

The reading rule lives in exactly one place, `engine::plan::tiers`. Renderers read it from
there so they cannot disagree.

`audit.read_hunks` counts focus hunks plus one exemplar per skim class.
`audit.skipped_hunks` counts skim remainders plus folded noise. **Only the second number
is the saving.** Consumers must never present a skim total as time saved.

## The invariants

All five run before any document is emitted. A failure means no document and a non-zero
exit. Every one caught a real bug during validation.

1. **Applier fidelity.** Every changed file must reconstruct byte-exactly from its base
   content plus all of its hunks. Binary files are checked by object id instead.
2. **Hunk accounting.** Hunk ids are unique. Hunks summed across any partition equal the
   canonical count. No hunk appears twice.
3. **Tree assertion.** The final tree is computed by applying hunks, never by copying head
   blobs. Then `built_tree == head^{tree}` proves every hunk was carried. Copying would
   make the equality hold by construction and prove nothing.
4. **Independent recount.** A deliberately dumb counter over git's own `diff-tree` output
   is compared against the canonical count. Its implementation must never share code with
   the diff parser. That separation is the whole point.
5. **Nothing unassigned is dropped.** Any class id the model omits is back-filled into a
   trailing `focus` group.

## Using it

```rust
use differential_engine::{gitio::Repo, config::Config, lang::LanguageRegistry,
                          store::OsConfigSource, resolve_range, run_pipeline};

let repo = Repo::open(path)?;                                          // any dir inside the repo
let config = Config::load(&OsConfigSource, repo.root(), None, None)?;   // or Config::default()
let src = resolve_range(&repo, &["main..feature"])?;                    // a plan::ReviewSource
let out = run_pipeline(&repo, &src, &config,
                       &LanguageRegistry::builtin(),
                       &differential_symbols::readers())?;   // the symbol readers

// out.report   — an InvariantReport. Always present.
// out.document — Some(PlanDocument), or None if an invariant failed.
// out.view     — the canonical diff view, carrying the hunk bytes.
```

`resolve_range` accepts `a..b`, `a...b` (base is the merge-base — what a merge request
diff shows), or two separate revisions. It returns a `plan::ReviewSource`. That carries the
endpoints (`base`, `head`, `kind`, and `remote` when a forge named them) **and** the
review's identity (`head_spec`, the head as typed, plus `identity_base`).

The two are separate on purpose. Reviewing uncommitted work diffs against synthesized trees
that change on every edit, while the review itself must survive. `resolve_picked` builds the
same type from the picker's answer.

### Adding the grouping stage

`run_pipeline` runs the core stages. `run_grouped_pipeline` also runs grouping:

```rust
use differential_engine::{GroupingOptions, run_grouped_pipeline};
use differential_engine::llm::CommandBackend;
use differential_engine::store::{FsArtefactStore, FsGroupingCache};

let backend = CommandBackend::claude_cli();
let cache = FsGroupingCache::for_repo(&repo)?;       // or FsGroupingCache::disabled()
let artefacts = FsArtefactStore::for_repo(&repo)?;   // where the model reads from
let opts = GroupingOptions { backend: &backend, cache: &cache,
                             artefacts: &artefacts, progress: None };

let out = run_grouped_pipeline(&repo, &src, &config,
                               &LanguageRegistry::builtin(),
                               &differential_symbols::readers(), &opts)?;
```

The symbol readers are injected the same way, and `differential_symbols::readers()` is
the whole of it — the readers rank themselves, so there is no order to get wrong (ADR 0023).

The backend, the cache and the artefact store are all **injected**. The engine does not
build a backend from config: composing one is the application's job. Disabling either store
is a constructor — `FsGroupingCache::disabled()`, `FsArtefactStore::disabled()` — not an
absent one, so the stage never grows a branch for a `--no-cache` flag.

The artefact store is where the pre-group document is left for the model to read (ADR 0022).
The prompt names that path instead of describing the classes, and the model fetches what it
needs with `dfr agent`.

Cancellation belongs to the backend, through `CommandBackend::with_cancel`. The thing that
needs killing is a subprocess.

`progress` takes an optional channel. The TUI uses it to drive its splash screen.

## The module map

| module | what it holds |
|---|---|
| `schema` | The frozen JSON contract. Serde types only, no engine internals. `SCHEMA_VERSION` is `3`. |
| `plan` | Shared domain policy over the schema: tiers, reading splits, review identity, the plan index, range parsing. |
| `ports` | The traits the domain owns. Eighteen of them, each named for a need. |
| `gitio` | `Repo` — the only implementation of the git ports. |
| `store` | Filesystem adapters: `FsGroupingCache`, `FsReviewStore`, `OsConfigSource`. |
| `llm` | `LlmBackend` and `CommandBackend`. Prompt in, text out. |
| `forge` | The forge consumer's domain: `Forge`, `Request`, `RemoteThread`, where a thread lands, what a publish sends (ADR 0029). |
| `forgeio` | `GhForge` and `GlabForge`: `gh` and `glab` on the path. |
| `subprocess` | One child process under a deadline and a cancel flag, shared by `llm` and `forgeio`. |
| `lang` | The `Language` trait and `LanguageRegistry`. Shape normalisation only — symbol extraction is `artefact::symbols` (ADR 0023). |
| `pipeline` | `run_pipeline`, `run_grouped_pipeline`, `resolve_range`, `resolve_picked`. |
| `grouping` | The grouping stage: prompt, parse, audit, gate, assembly, cache. |
| `artefact` | The class dependency graph, the `SymbolSource` port and the rule for choosing between readers, the document the model reads, and the queries behind `dfr agent`. |
| `ordering` | The foundation-first sort. It does NOT build the graph — `artefact::graph` does, from classes, before the model runs (ADR 0022). |
| `invariants` | All five checks. |
| `review_session` | `ReviewSession` — the facade over review state on disk. |
| `apply`, `parse`, `shape`, `tree`, `document`, `model` | The mechanical core. |

### The dependency arrow points one way

The domain never depends on an adapter. The adapter depends on the domain.

A domain module may not name `gitio`, `std::fs`, `std::process` or `std::env`. If it needs
git, a filesystem or a clock, it declares a port next to the logic, and an adapter
implements that port. This is enforced by a test, not merely stated.

Ports are named for what the caller needs (`ObjectReader`, `CommitWriter`, `ReviewStore`),
never for the tool that implements them. A function's bound list is an accurate statement
of how much git it can touch. There is no `trait Git: A + B + …` supertrait, on purpose.

**`gitio::Repo` is the only implementation of the git ports.** A fake git for tests is
forbidden. Invariants 1 to 4 compare the engine against git's own answer, so a fake would
make them compare a fake against itself. Tests use hermetic temporary repositories and real
`git`.

## Review state

`ReviewSession` owns everything that persists. The TUI is a stateless frontend over it:
every mutation is on disk before the call returns.

```
<git-common-dir>/differential/
├── reviews/<review-id>/
│   ├── plans/<content-hash>.json   every generated document, immutable
│   ├── current                     the active plan's content hash
│   ├── findings.jsonl              the findings store
│   ├── identity.json               a name, or the endpoints it was opened as
│   └── state.json                  progress and view preferences
└── cache/grouping/<classes-hash>.json
```

The review id is `sha1(base_sha ‖ NUL ‖ head_spec)` truncated to 16 characters. The head is
the string **as typed**, so reviewing `main..feature` keeps one review while `feature` moves.

Because the spelling is part of the name, `review_identity::resolve` decides which review a
range opens. A spelling with no review of its own adopts one filed on the same base whose
head is reachable from this one — the two spellings of a commit, and the commits added
since. Two branches off one base adopt nothing from each other. The join is recorded as an
`alias` file, so it is permanent and costs one file read (ADR 0026).

Adoption rests on ancestry, and a rebase rewrites both endpoints, so nothing is adoptable
after one. `dfr review --name <name>` files the session under `review_id_named(name)`
instead: no endpoint is in the key, it works from the picker, and it neither adopts nor is
adopted (ADR 0027).

Documents are pure functions of `base..head`. Review state lives in the sidecar and
re-anchors on every regeneration: exact hunk digest first, then a content match flagged
*moved*, then orphaned. An orphan is listed, never silently dropped. It revives when its
content comes back.

Reviewed marks key on the exact hunk digest, not on position and not on the class (ADR
0025). Marking a group marks every hunk in it; changing one hunk leaves the rest read.

## The grouping cache

Labels are not deterministic across model calls. A content-hash cache keeps a review from
reshuffling under the reviewer.

The key is a sha1 over the prompt version, the backend name, the language-registry
fingerprint, and each offered class's sorted member hunk digests. Digests are content-exact,
so a key survives an id shift across regenerations.

The **raw model response** is what gets cached. Parsing, the audit, the relocation gate and
assembly are pure functions replayed on every load. So a fix to any of them applies to
cached runs too. A change to the prompt, or to what the model can fetch, must bump the
prompt version.

## Config

```rust
let config = Config::load(&OsConfigSource, repo.root(), repo_path, user_path)?;
```

Two files. The repo file (`.differential.toml`) holds classification hints. The user file
(`~/.config/differential/config.toml`) holds the grouping backend and the reviewer's context
settings. Full key tables:
<https://github.com/gogoout/differential/blob/main/crates/cli/README.md#config>

**Config can never remove a file or a hunk from enumeration.** Enumeration is total,
always. Every invariant depends on that. Path filtering was the single worst coverage bug
found during validation.

## Dev entry point

```sh
cargo run -p differential-engine --example group -- [--repo <path>] [--no-cache] [-o <file>] <base>..<head>
```

That prints the grouped document as JSON.

## Learn more

- The normative behaviour:
  <https://github.com/gogoout/differential/tree/main/spec>
- The decision records, with the measurements behind them:
  <https://github.com/gogoout/differential/tree/main/adr>

## Licence

MIT or Apache-2.0, at your option.
