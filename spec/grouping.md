# The grouping stage

Turns the mechanical class partition into labelled, effort-rated groups. The model merges
and labels **class ids, never hunks** (ADR 0001): it cannot drop what it never names, and
anything it omits is detected and back-filled. Runs inside the engine
(`run_grouped_pipeline`); the backend is any `LlmBackend` (ADR 0016), defaulting to a
headless claude invocation with read-only tools (ADR 0022), configurable via
`[grouping].command` in the user-level config (agents are per-user; the backend command is
part of the cache key, so different agents get separate cache entries).

## What the model never sees or cannot override

- **Noise is mechanical** (ADR 0006). A class whose hunks all live in `generated` files is
  pre-assigned to one folded noise group and is never offered. A class spanning generated
  and non-generated files stays with the model.
- **The relocation gate** (ADR 0003). A class touching a file whose `rename_similarity` is
  below 95 is a modification, not a relocation. `dfr agent class <id>` reports the rename
  ("renamed from …, N% similar") so the model can judge correctly — and a deterministic
  post-audit pass extracts any such class out of a skim group into a synthesized focus group
  ("Modified during move") regardless of what the model claimed.

## What the model gets (ADR 0022)

Not a payload. The engine writes the **pre-group document** — the frozen schema with
`groups: null` — to `<git-common-dir>/differential/cache/document/<key>.json`, under the
grouping cache's own key, and the prompt says where it is.

The prompt carries the instructions, the `dfr agent` commands, and the class id list
(about 1KB for two hundred classes). The id list is the floor: if every fetch fails the
model still knows the exact id set, so it returns a weak grouping rather than a
hallucinated one. **There is no size cap**, because nothing about the classes is sent.

Five queries, all against the document (`spec/consumers.md`):

| `dfr agent --doc <path> …` | returns |
|---|---|
| `classes` | one line per class: size, files, kind, exemplar location, `defines`, `uses` |
| `class <id>…` | those classes in full — every member hunk, every file, rename notes |
| `diff [id…] [--after <hunk>]` | diff text for hunk ids, or every member of class ids; **no ids means the whole change** |
| `file <path>` | the classes touching a path |
| `defines <symbol>` | the classes that introduce a symbol |

`diff` is the one that changes what the model can claim. Rating a class `skim` asserts
that every member is the same edit; before this the model had seen one member of any
size of class.

**Every command takes any number of arguments, and none means all of them.** A call is a
round trip through a model turn, and the work behind one is under a tenth of a second — a
`diff` batch re-enumerates the range once however many ids it carries. Measured on a
196-class change: one id per call made 176 fetches, batching brought it to 28, and a bare
`diff` for the whole change brought it to 15.

Round trips were never most of the time, and the measurement says so: 15 calls at about
0.9s is 13 seconds of a 391-second run. The rest is the model reading the change and
reasoning about it. Batching removes waste; it does not move that floor.

**A reply carries at most 256KB of diff text, and says how to get the rest.** Past the cap
it ends with the exact `diff --after <hunk-id>` that continues it. The cursor is a hunk id
because a hunk id already names a position in the list, so nothing has to be encoded.
Nothing is ever dropped for length — that was the failure of the 90,000-character prompt
cap this replaced.

The prompt also says which classes are worth checking at all: only a class with more than
one hunk can be wrong about equivalence, and on that change 162 of the 196 had exactly one.

**Asking without naming anything gets the offered set; naming something reaches
everything.** So `classes` and a bare `diff` leave out generated content, exactly as the
prompt's id list does — handing the model a lockfile would be bytes it must read and a
class id it would be penalised for naming. `class C7`, `file <path>` and `defines <symbol>`
answer for generated content too: the noise tier folds, it never hides. One definition
serves both sides (`plan::class_is_generated`), because two copies of that rule would be
two rules.

The backend runs with a read-only allowlist —
`Bash(dfr agent:*),Read,Grep,Glob,Bash(git log:*),Bash(git show:*)` — which supersedes
ADR 0010's tools-denied contract. `LlmBackend` is unchanged: prompt in, text out, with the
tools running inside the CLI it spawns.

**Available, not advertised.** The prompt names the fetch command and nothing else. A
model that needs the code around a hunk can go and read it; it is not sent looking.
Naming these tools in the prompt would invite a whole-repository read where a class table
and a diff were the answer, and would offer a way around the generated content this stage
deliberately folds away.

## Audit — nothing is ever dropped

Against the offered id set: hallucinated ids are removed (and listed), duplicated ids are
kept by their first group (and listed), missing ids land in a trailing
`effort: focus` back-fill group (invariant 5). `audit.coverage` is the honest pre-back-fill
number: model-assigned hunks / offered hunks. Truncation is no longer one of the ways a
class can go missing, because nothing is truncated (ADR 0022). Unknown effort strings mean `focus`
(when in doubt, focus).

## Assembly

Group order is presentation only (the foundation-first sort is the ordering stage): focus
groups in model order (gate group last), skim groups by descending hunk count, the noise
group, then the back-fill. `role` is `null` except the noise group (`"noise"`);
`depends_on` is empty until the ordering stage exists.

Reading plan: focus → `read`; skim → `exemplars`, plus `skip` only when a remainder exists;
noise → `fold`. `audit.read_hunks` counts focus hunks plus one exemplar per skim class;
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

One caveat the key cannot close (ADR 0022): the model may read `git log`, and no key
can capture history. Two clones whose history differs can group differently under one key.

A cache hit makes the grouped document fully deterministic. No auto-retry on a malformed
response: the error carries a response sample and the caller decides.
