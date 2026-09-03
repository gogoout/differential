# 0029 — The forge consumer: a tool on the path, and a comment is a finding first

Status: accepted

Shapes the third consumer that [ADR 0008](0008-workspace-and-schema-crate.md),
[ADR 0014](0014-core-is-a-library.md) and [ADR 0018](0018-crate-consolidation-and-renderer-crates.md)
reserved a place for and never described. Specified in [`spec/forge.md`](../spec/forge.md).

## Context

Every document about this tool names three consumers. Two are shipped. The third, "forge
review — grouped comments posted to a GitLab MR / GitHub PR", has been one sentence since
the first spec, and the code around it was built ahead: `source.remote`, `source.kind: pr |
mr` and `hunks[].forge_position` sit in the schema and nothing has ever written to them.

What the sentence left open is everything that costs anything. How the tool reaches the
forge. Which forge. How the reader says which pull request this is. Whether the forge's
existing comments come *down* — the sentence only ever describes posting *up* — and if they
do, where they live next to the reader's own notes. When a note the reader writes becomes a
comment other people can see.

Four facts about what already exists settle most of it.

**The reviewer already has per-line notes that survive a force-push.** A finding is a body
and an `Anchor` — file, side, line, an offset into a content-addressed hunk, and the line's
text — and `reanchor` moves it across regenerations or orphans it
([ADR 0013](0013-incremental-review-sidecar-state.md), [ADR 0025](0025-reviewed-marks-key-per-hunk.md)).
A pull request comment is a body and a file, a side and a line. It is the same thing with
an author on it.

**The tool already shells out to a program that carries its own credentials.** The grouping
backend is the `claude` binary on the path, found the way a shell finds it, logged in the
way its own vendor decided ([ADR 0016](0016-llm-backend-abstraction.md)). Nothing here holds
a token, reads an environment variable for a secret, or knows a hostname. `gh` and `glab`
are the same kind of program: authenticated by their own login, aware of the remote's host
and project from the working directory, and able to emit JSON for any endpoint.

**A pull request is an object, and this tool already knows that.** [ADR 0027](0027-a-named-review-session.md)
cites the forges as the precedent for a review whose identity is not derived from a range,
and `spec/persistence.md` says outright that a pull request is not inferred: you name it.

**GitHub itself does not post a comment when you type it.** A review comment is *pending*
until the review is submitted, and the submission is one event. GitLab has draft notes and a
bulk publish for the same reason. Both forges learned that a reviewer wants to reread and
delete before anyone is notified.

## Decision

**1. The forge is a tool on the path.** The adapter runs `gh` for GitHub and `glab` for
GitLab and parses their JSON. No HTTP client, no token, no hostname, no config table. Both
are subprocesses with a deadline and a cancel flag, on the pattern `llm::CommandBackend`
already validated. The user installs and logs in to the tool; the error when they have not
is the tool's own, passed through.

**2. The trait is domain; the adapter is chosen at run time.** `engine::forge` declares what
the domain needs — the pull request's endpoints, its review threads, a way to publish a
batch, a way to resolve a thread — and the types those speak in. The two adapters live in a
module named in the layering test's `ADAPTER_MODULES`, exactly as `llm.rs` does, and the
application layer composes one. This is the third `dyn` seam after `LlmBackend` and
`Language` ([ADR 0020](0020-ports-and-static-dispatch.md)): which forge a repository is on is
a run-time answer, and nothing else in this design is.

**3. The flag names the forge and the pull request, and the pull request is the identity.**
`dfr review --pr 123` is GitHub; `dfr review --mr 123` is GitLab. Either flag without a
number asks the tool for the current branch's request. There is no range argument: the
forge returns the base tip and the head, and the review range is `base...head`, because a
merge-base diff is what a pull request shows. The review session is keyed on the request —
`ReviewIdentity` gains a variant beside `Named` — so it neither adopts nor is adopted and
survives every force-push, which is the property ADR 0027 borrowed from the forges in the
first place. `source.kind` and `source.remote` are finally written.

**4. The tool does not fetch.** If the request's head or base tip is not in the local object
database, the command prints the `git fetch` line that would bring it and stops. Every git
port stays plumbing and offline ([ADR 0011](0011-plumbing-over-porcelain.md)), and the one
network-touching git command a reader might not expect stays theirs to run.

**5. Remote threads are a cache in the sidecar; local notes are the reader's.** The forge's
review threads are fetched when the review opens, anchored into the diff with the same
`Anchor` a finding has, and written to `comments.jsonl` beside `findings.jsonl` under the
review directory. Every open refetches and overwrites that file; a fetch that fails leaves
the previous one in place and says so. The forge never writes into `findings.jsonl`. The two
files render into one diff, under their lines, told apart by look: a remote thread shows its
author and age, and it cannot be edited or deleted from here.

**6. Every comment starts as a finding. Posting is a separate, batched, confirmed act.** The
composer does not change. `c` writes a local note, or — with the cursor on a remote thread —
a local reply that carries the thread's id. Nothing leaves the machine on save. `P` shows
how many open findings are not yet on the request, asks, and publishes them as one review
(GitHub) or one draft-note publish (GitLab). Each published finding records its upstream id,
so a second `P` sends only what is new, and `y` — the clipboard summary — now means "not yet
on the request". A published finding and its fetched twin match on that id and render once,
as the thread.

**7. Resolve is immediate.** With the cursor on a remote thread, one key toggles resolved on
the forge at once. It is reversible and it is one short call, so a pending state and a
confirmation would cost more than they protect.

**8. GitHub first, both directions, then GitLab.** The first release pulls threads down and
posts findings up, on GitHub. GitLab is the second adapter behind the same trait. A review
verdict (approve / request changes), editing or deleting a published comment, and comments
with no line are later work; reactions are not work at all.

## Consequences

- `hunks[].forge_position` is not the posting anchor and never was one. It carries a hunk's
  first line per side from the canonical `--no-renames -U0` view, so for a renamed and edited
  file it reads `1`. Posting uses a finding's `Anchor` and the file entry's `old_path`. The
  schema is frozen, so the field stays; `spec/json-contract.md` says what it actually is.
- A finding gains two optional fields, `reply_to` and `upstream`. Both are additive with
  defaults, like every field before them; an older `findings.jsonl` loads unchanged.
- A note on a line the forge's diff does not show cannot be published. GitHub's request diff
  carries three lines of context per hunk; a note further out fails the whole review. The
  batch therefore excludes such findings and reports each by file and line, and the
  reviewer shows which they were.
- The local head must equal the request's head at post time. Both forges reject a comment
  against a commit that is not the request's. The check is the first thing `P` does.
- Whether `gh` or `glab` is present, logged in, and online is checked when it is used, by
  using it. There is no probe, and a review of a request opens without its threads rather
  than not at all.
- The reviewer's loop stays synchronous. The fetch rides the splash as one more stage; a
  post blocks the loop with a status line for the second or two it takes. If that second
  grows, the splash's worker-and-channel shape is the one to reach for, and the trait does
  not change.
- `crates/stack` is untouched. It emits commits, and a commit has no line cursor to comment
  under.

## Alternatives rejected

**An HTTP client and a token.** It removes the install step and adds a dependency tree, a
credential store to design, a hostname to configure for self-hosted GitLab, and a second
thing to keep secret. The tools already solve every one of those, for every forge they
support, and are maintained by the forges. If a reader has neither, the install step is one
line and the tool's own login flow is better than any this project would write.

**Post on save.** One notification per note, and no chance to reread. Both forges moved
away from it for their own reviewers.

**Choose local or remote at write time**, with a second key or a toggle in the composer.
It is a decision per note about something the reader decides once per review, and it puts
the irreversible act one keystroke from the reversible one.

**Infer the forge from the origin URL.** `github.com` is unambiguous; a self-hosted GitLab
is any hostname at all. Both tools already know which host they are logged in to, and the
flag costs the reader two characters they had to type anyway to give the number.

**Import remote threads into `findings.jsonl`** as findings with an author. One file, one
type, one render path — and every refetch becomes a merge into the reader's own data, which
is the file that cannot be recomputed. Two files means nothing that overwrites one can
reach the other, the same argument `persistence.md` makes for `cache/` and `reviews/`.

**Batch the resolves with `P`.** It needs a pending state, a visual for it, and a way to
undo a pending resolve that was not meant. A resolve is already undoable on the forge.
