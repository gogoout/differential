# The grouping stage

Turns the mechanical class partition into labelled, effort-rated groups. The model merges
and labels **class ids, never hunks** (ADR 0001): it cannot drop what it never names, and
anything it omits is detected and back-filled. Runs inside the engine
(`run_grouped_pipeline`); the backend is any `LlmBackend` (ADR 0016), selected by name via
`[grouping].agent` in the user-level config — `claude-code` is the default and so far the
only one, a headless invocation with read-only tools (ADR 0022). Agents are per-user, and
the backend's identity is part of the cache key, so different agents get separate cache
entries.

The key is a **name, not an argv**. The stage hands its agent a tool allowlist, a fetch
command and a prompt written for what that agent can do; an arbitrary argv got the prompt
and none of the rest.

## What the model never sees or cannot override

- **Noise is mechanical** (ADR 0006). A class whose hunks live in `generated` files is
  pre-assigned to one folded noise group and is never offered. **`generated` is part of the
  shape-class key** (`shape::shape_hash`), so no class spans both kinds: one shaped edit made
  in a lockfile and in a source file is two classes, folded and offered respectively.

  It used to be one class, and that was a hole. `class_is_generated` could only ask "is every
  member generated?", which such a class answers no, so it went to the model and its lockfile
  hunk went into whatever group the model chose — a generated hunk in a focus group. The
  price of closing it is that a `[classify]` glob now moves a class boundary rather than only
  a routing decision. That is config tuning classification, which is what config is for
  (ADR 0012); it still cannot add or remove a hunk.
- **The relocation gate** (ADR 0003). A class touching a file whose `rename_similarity` is
  below 95 is a modification, not a relocation. `dfr agent` reports the rename
  ("renamed from …, N% similar") so the model can judge correctly — and a deterministic
  post-audit pass extracts any such class out of a skim group into a synthesized focus group
  ("Modified during move") regardless of what the model claimed.

## What the model gets (ADR 0022)

Not a payload. The engine writes the **pre-group document** — the frozen schema with
`groups: null` — to `<git-common-dir>/differential/cache/document/<key>.json`, under the
grouping cache's own key, and the prompt says where it is.

The prompt carries the instructions, the `dfr agent` command, the `git diff` command with
the range already in it, and the class id list (about 1KB for two hundred classes). The id
list is the floor: if every fetch fails the model still knows the exact id set, so it
returns a weak grouping rather than a hallucinated one. **There is no size cap**, because
nothing about the classes is sent.

**One command, one answer.** `dfr agent --doc <path>` prints every class the model may
group, in full (`spec/consumers.md`): id, hunk count, file count, disposition, exemplar
location, then every member hunk with its file and line range, then every file it touches,
with `defines:`, `uses:` and `used by:` lines.

There were five queries — `classes`, `class`, `diff`, `file`, `defines`. Two measurements
collapsed them. `diff` re-enumerated the range to print hunk text, which `git diff` does;
it went, and with it the only reason `dfr agent` ever opened a repository. What remained is
**72KB for a 196-class change**, beside the 322KB of diff the model reads anyway — not
worth four commands and three extra model turns to slice. `file` and `defines` were lookups
into a list the reader now has in front of it.

**Diff text comes from `git diff`, not from the engine.** `dfr agent` says where every hunk
is; `git diff <base> <head> -- <path>` says what it holds. The prompt spells that command
out with the range already in it, because the reader is an agent with a terminal and a
command it can run beats an instruction it has to assemble.

That is what lets the model check the claim a `skim` rating makes. Rating a class `skim`
asserts every member is the same edit; a 9h class is a claim about nine hunks, and its
entry gives all nine locations. Only a class with more than one hunk can be wrong about
equivalence, and on that change 162 of the 196 had exactly one.

**The lever is reading, not round trips.** Measured on a 196-class change, fetch calls fell
176 → 28 → 15 → 5 while the wall clock went 450s → 391s → 401s: at about 0.9s a call, round
trips were never more than about 13 seconds of it. The rest is the model reading the change
and reasoning about it. So the prompt asks for **selective** reading — read what decides a
label and then stop, judge a file by its path before opening it, and check a multi-hunk
class rather than walking every class in turn.

**Generated content is left out, exactly as the prompt's id list leaves it out** — handing
the model a lockfile would be bytes it must read and a class id it would be penalised for
naming. One definition serves both sides (`plan::class_is_generated`), because two copies
of that rule would be two rules — and with `generated` in the class key, that definition
cannot come back half true.

**No generated path reaches the model at all**, because there is no mixed class to leak
one. The prompt says so, and says not to `git diff` a path that looks generated — `git diff`
honours no tier and will show a lockfile to anyone who asks. The noise tier folds and never
hides: `git diff` reaches any path at all, for a reviewer who wants it.

The backend runs with a read-only allowlist —
`Bash(dfr agent:*),Bash(git diff:*),Read,Grep,Glob,Bash(git log:*),Bash(git show:*)` —
which supersedes ADR 0010's tools-denied contract. `LlmBackend` is unchanged: prompt in,
text out, with the tools running inside the CLI it spawns.

**`git diff` is advertised; the rest are not.** The prompt names the fetch command and
`git diff`, because `git diff` is now the only way to see what a hunk says, and a tool the
model must use and is not told about is a tool it will not use. It costs what advertising
always cost — an invitation to read the whole repository, and a route around the generated
content this stage folds away. Two things pay for it, and both are above: the prompt's
instruction to read selectively, and the class key that keeps generated hunks out of every
offered class. `Read`, `Grep`, `Glob`,
`git log` and `git show` stay unadvertised: a model that needs the code around a hunk can go
and read it; it is not sent looking.

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
`PROMPT_VERSION`, the backend identity, the language-registry fingerprint, and each offered
class's sorted member hunk digests (content-exact, so keys survive positional-id shifts
across regenerations). The cached value is the **raw model response**; parsing, audit, the
gate and assembly are pure functions replayed on every load — their fixes apply to cached
runs, while prompt or payload changes must bump `PROMPT_VERSION`.

One caveat the key cannot close (ADR 0022): the model may read `git log`, and no key
can capture history. Two clones whose history differs can group differently under one key.

A cache hit makes the grouped document fully deterministic. No auto-retry on a malformed
response: the error carries a response sample and the caller decides.

The prompt itself is a text file, `crates/engine/src/grouping/prompt.txt`, pulled in with
`include_str!` and substituted at five `{{…}}` placeholders. A prompt is prose, so editing
one should read as a diff of sentences. `crates/engine/tests/golden.rs` pins a second,
separate copy under `tests/fixtures/prompt-v<N>.txt`: one shared file would move both sides
together and catch nothing.
