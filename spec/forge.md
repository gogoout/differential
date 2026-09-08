# The forge consumer

A pull request or merge request, read and written from the reviewer. The forge's review
threads appear under their lines in the diff; the reader's findings are published back as
review comments. Decided in [ADR 0029](../adr/0029-the-forge-consumer.md). This spec is
normative for what the consumer does; the ADR says why.

Vocabulary: **request** is a GitHub pull request or a GitLab merge request when the
distinction does not matter. **Thread** is one forge discussion: a root comment and its
replies. **Finding** is the reader's own note, as in [persistence.md](persistence.md).

## Naming the request

```sh
dfr review --pr 123          # GitHub pull request 123 of this repository's remote
dfr review --mr 123          # GitLab merge request 123
dfr review --pr              # the current branch's pull request, asked of the tool
dfr findings --pr 123 --post # publish open findings without opening the reviewer
```

`--pr` and `--mr` are mutually exclusive with each other and with a range. The flag names
the forge; the tool it runs is `gh` for `--pr` and `glab` for `--mr`, found on the path.

The forge answers with the request's base branch tip, its head commit, and its project.
The review range is `base_tip...head`: the merge-base diff, which is the diff the request
page shows. When either commit is not in the local object database, the tool fetches the
request's refs from `origin` — `git fetch origin main pull/123/head` on GitHub,
`… merge-requests/123/head` on GitLab — and looks again. Only a commit still missing after
that stops the command:

```
pull request 123 needs commits this clone does not have, and fetching did not bring them; try
    git fetch origin main pull/123/head
by hand
```

This is the one place the tool runs `git fetch`, through the `Fetcher` port (ADR 0029,
decision 4 as reversed by the author).

The request is the review's identity. `ReviewIdentity::Remote { forge, project, id }` is
keyed like a name (ADR 0027): neither endpoint is in the key, so a force-push of the head or
a rebase onto a moved base reopens the same review, and it neither adopts nor is adopted.
`identity.json` records the three fields. The document's `source.kind` is `pr` or `mr` and
`source.remote` is `{forge: "github" | "gitlab", project: "<owner>/<repo>", id: "123"}`.

## The trait

```rust
pub trait Forge {
    fn request(&self, id: Option<&str>) -> Result<Request, ForgeError>;
    fn threads(&self, req: &Request) -> Result<Vec<RemoteThread>, ForgeError>;
    fn publish(&self, req: &Request, batch: &Batch) -> Result<Vec<Published>, ForgeError>;
    fn set_resolved(&self, req: &Request, thread: &str, resolved: bool) -> Result<(), ForgeError>;
}
```

`Request` carries `forge`, `project`, `id`, `base_tip`, `head`, `base_ref` (the branch name,
for the fetch hint) and `url`. The trait is domain and lives in `engine::forge`; the
adapters that run `gh` and `glab` live in a module named in the layering test's
`ADAPTER_MODULES`; `crates/cli` composes one from the flag. It is `dyn`: which forge is a
run-time answer (ADR 0020).

## Remote threads

One thread record, as fetched and as cached:

```jsonc
{ "id": "PRRT_…",                       // the forge's thread id, opaque
  "resolved": false, "outdated": false,
  "anchor": { "file": "src/lib.rs", "side": "new", "line": 47, "end_line": 52,
              "offset": 3, "span": 5, "hunk_digest": "…",
              "line_text": "…", "end_line_text": "…" },
  "comments": [
    { "id": "3928619949", "author": "alice", "created": "2026-09-03T20:53:12Z",
      "body": "…", "finding": null },
    { "id": "3928660390", "author": "bob",   "created": "…", "body": "…",
      "finding": null } ] }
```

The anchor is the same type a finding has, computed on fetch from the forge's `path`,
`side`, `line` and `start_line`: the hunk on that side that holds the line gives
`hunk_digest` and `offset`; the line's text comes from the blob at `head` (new side) or at
the merge base (old side). An **outdated** thread — one whose line has left the request's
diff — has no line from the forge; its `line_text` is the last line of the forge's recorded
diff hunk, and `reanchor` places it by content or leaves it orphaned. Re-anchoring on open
is the same call findings get.

The threads live in `comments.jsonl`, one thread per line, beside `findings.jsonl`:

```
reviews/<review-id>/
├── findings.jsonl   # the reader's notes; the forge never writes here
└── comments.jsonl   # a cache of the forge's threads; overwritten on every fetch
```

The fetch starts the moment the reviewer opens, on a worker thread the reviewer polls
between keys; the footer wears `syncing` until it lands. A fetch that fails — tool missing,
not logged in, offline — keeps the previous cache, leaves the review open, and says what
happened in the status line. `R` refetches from inside the reviewer. Publishing refetches
too, so a published finding's twin appears at once.

## What the reviewer shows

Both files render into one diff through the placement findings already use: under the row
whose file and side hold their line, or under the hunk header when no row does.

A **remote thread** shows each comment with its author and date in a header row, each
reply indented one step under the root, and a resolved thread in the dimmed style. It is
reply-only: edit and delete do nothing on it and the status line says so. `x` toggles
resolved, on the forge, at once; the local copy follows when the forge has answered.

A **finding** keeps the look it has. A **published** finding is hidden when its fetched
twin is present, matched on `upstream.comment` or the marker; the record stays in
`findings.jsonl` so the summary and a re-post can count it. A finding that is a **reply
draft** renders in the finding's look under the thread it answers, in date order after the
thread's comments.

A comment is **the reader's own** when its author is the login the forge knows the reader
as (`whoami` on the trait, asked once per session with the first fetch and told to the
session), when its body carries this review's marker, or when a finding records its
address. The session decides it (`own_comment`, `own_root`, `own_of_finding`); the
reviewer only maps a row to a thread and a comment. `c` on one of its rows
opens the composer on its text, and saving rewrites it on the forge first; the cached
thread follows when the forge has answered, and the record too when a finding is linked.
`dd` on one asks — `y` deletes it there and here, any other key keeps it — and the thread
goes with it when nothing is left. Both reach the forge through `edit_comment` and
`delete_comment` on the trait, keyed by thread and comment, so a comment written on the
forge's own page is as editable as one published from here. Anyone else's comment stays
reply-only, and the footer says `not your comment`.

## Writing

`c` behaves as [tui.md](tui.md) describes, with one addition: with the cursor on a remote
thread's rows, the composer opens as a reply, titled with the thread's file and lines, and
the saved finding carries `reply_to: "<thread id>"` and the thread's anchor. Nothing here
reaches the forge.

## Publishing

`P` collects every finding with `status: open` and no `upstream`, shows what would go and
what would stay and why, and asks. On `y`, on a worker thread:

1. **The head check.** The forge is asked where the request is now; its head must equal
   the review's head. If it does not — someone pushed since the review opened — nothing is
   sent, and the status line says which commit the request is at now.
2. **The diff check.** A finding whose line the request's diff does not show is excluded
   and reported by file and line. On GitHub a request diff carries three lines of context
   around each hunk and the public API resolves a `line` against exactly that: measured on
   a live request, three lines after a change lands and four is refused with `line could
   not be resolved`, whatever the web page allows after expanding. The check is against the
   plan's hunks on the anchor's side, widened by three. Replies skip it: they need a thread
   id, not a line. GitHub also takes a comment on the **file** (`subject_type: file`, no
   line) and it landed in the same measurement; the author chose not to send excluded
   notes that way, so they stay local and the float says why. **GitLab is not held to
   this rule**: every open note goes, positioned by its own line numbers, and if GitLab
   refuses one its refusal is what the reader sees.
3. **One batch.** New comments and replies go up as described per forge below. Each
   finding that lands records `upstream: { thread, comment }`. A finding that fails keeps
   no `upstream` and is reported.
4. **Refetch.** `comments.jsonl` is rewritten and the published findings hide behind their
   twins.

**A publish is idempotent by marker.** Every body sent ends with an HTML comment neither
forge renders, `<!-- differential:finding <id> -->`. Fetched comments are read for it: the
marker is stripped from what is shown and recorded as the comment's `finding`. That is the
match — not path, line and body, which the forge stores reflowed — and it survives a lost
answer: on every fetch, a finding with no `upstream` whose marker a fetched comment carries
is marked published there and then. A publish whose answer never came back therefore heals
on the refetch it runs anyway, the plan never sends a finding a thread already carries, and
`P` a second time has nothing to send. A comment **by the reader with no marker** — sent
before markers existed, or written on the forge's page — is matched more loosely: an
unpublished note on the same file and line with the same text, or a reply in the same
thread with the same text, is linked to it and marked published. Same author, same place,
same words is enough; an edit from here then adds the marker.

`y` copies only findings with no `upstream`. A published finding is on the request; the
clipboard is for what is not. It stays in `findings.jsonl` with its address, hidden behind
its twin in the diff and listed once, as the thread, in `F`.

`dfr findings --pr 123 --post` runs steps 1 to 3 without the reviewer and prints one line
per finding: published with its URL, excluded with its reason, or failed with the tool's
error.

The finding record gains two optional fields. Both default when absent, so a store written
before them loads unchanged:

```jsonc
{ …, "reply_to": "PRRT_…" | null,
     "upstream": { "thread": "PRRT_…", "comment": "3928619949" } | null }
```

## GitHub, through `gh`

| need | call |
|---|---|
| request | `gh pr view <n> --json number,baseRefName,baseRefOid,headRefOid,url` — the project is read from `url`, since the request lives in the base repository |
| threads | `gh api graphql` — `pullRequest(number).reviewThreads { id isResolved isOutdated path diffSide line startLine comments { databaseId body author createdAt replyTo diffHunk } }`, paginated |
| publish, new | `gh api POST repos/{owner}/{repo}/pulls/<n>/reviews` with `commit_id`, `event: "COMMENT"`, and `comments: [{path, body, line, side, start_line, start_side}]` — one review |
| publish, reply | `gh api POST repos/{owner}/{repo}/pulls/<n>/comments/<root comment id>/replies` with `body`, one per reply |
| resolve | `gh api graphql` — `resolveReviewThread(input: {threadId})` / `unresolveReviewThread` |
| who am I | `gh api user` → `login` |
| edit, delete own | `PATCH` / `DELETE repos/{owner}/{repo}/pulls/comments/<id>` |

`side` is `RIGHT` for the anchor's `new` and `LEFT` for `old`. `path` is the file's path in
the request, which for a renamed file is the new path on either side. A multi-line finding
sends `start_line` and `start_side`. The review's `body` is empty; a verdict is later work,
and until then `event` is always `COMMENT`.

## GitLab, through `glab`

| need | call |
|---|---|
| request | `glab mr view [<iid>] --output json` — `iid`, `target_branch`, `web_url`, `diff_refs.{base_sha, start_sha, head_sha}` |
| threads | `glab api --paginate projects/:id/merge_requests/<iid>/discussions` — a discussion is a thread when its first non-system note is a `DiffNote` with a `text` position; `notes[]` give `author.username`, `created_at`, `resolved` |
| publish, new | one `POST …/merge_requests/<iid>/draft_notes` per finding, then one `POST …/draft_notes/bulk_publish`; the discussions are fetched again to learn each note's id. The `note` is a `-f` body field. The **position rides in the query string** as `position[position_type]=text` and `position[base_sha]`, `position[start_sha]`, `position[head_sha]`, `position[old_path]`, `position[new_path]`, and `position[old_line]` and/or `position[new_line]` — one number for a changed line, both for an unchanged one. A `position` object in the JSON body is **not read** by this endpoint: it returns `position[base_sha] is missing` for every key. GitLab reads the hash only from bracket-encoded params, so they go in the query, percent-encoded |
| publish, reply | `POST …/merge_requests/<iid>/discussions/<id>/notes -f body=…`, one per reply, **before** the draft notes; the note comes back with its id, so each reply is on record as it lands, and a reply that fails stops the batch before any new comment has gone up. A draft note with `in_reply_to_discussion_id` came out as a new discussion on the first live run |
| resolve | `PUT …/merge_requests/<iid>/discussions/<id> -F resolved=true|false` |
| who am I | `glab api user` → `username` |
| edit, delete own | `PUT …/discussions/<id>/notes/<note id> -f body=…` / `DELETE` the same path |

`:id` is the tool's placeholder for the current directory's project. Every write goes as
`glab`'s field flags, never as a raw body on stdin: the tool sends fields as JSON with the
content type set, and a raw body without one drew `HTTP 415` from GitLab on the first live
write. `gh` labels a raw body as JSON, so GitHub keeps its bodies. When a call fails, the
error carries what the tool printed on stderr **and** stdout: both tools put only the status
on stderr — `glab: HTTP 400` — and the forge's own answer, which is the reason, on stdout.
`start_sha` is the
target branch's tip when the diff was computed, `base_sha` the merge base; both come from
the request and travel in every position. `old_path` is the file entry's `old_path` when
it has one, else the path. Two limits, until the adapter has met a live instance: a
**multi-line finding is positioned at its last line**, opens its note with `(lines a-b)`,
and carries a `position[line_range]`: a `[start]` and an `[end]`, each a `line_code` (the
path's sha1, then the old and new line numbers, `<sha>_<old>_<new>`) and a `type`. An added
or deleted line has a real number on its own side and, on the other, the position it sits at.
GitLab rejects `old_line`/`new_line` fields inside `line_range`, so each end carries only its
`line_code` and `type`. This is unconfirmed against a live instance; a **position recorded against another head is outdated** and counted
rather than drawn, because the REST answer carries no diff text to place it by; and a
**publish that fails part-way** may leave draft notes on the request, because GitLab has
no batch create — the next publish does not know them, so they are cleared by hand. The
**three-line diff check is GitHub's** and is not applied here: GitLab positions a note by
`old_line` / `new_line` against the request's diff refs, an unchanged line carries both
numbers (the other side's computed from the hunks before it), and any line of the file is
sent. Whether GitLab accepts every such line is unmeasured; a refusal comes back as the
tool's own error. The GitHub
table above is verified against a live request; this one is written from the API
reference and pinned by tests on the shapes it expects.

## Later

In the order they are likely to be wanted: a verdict on `P` (GitHub `event: APPROVE |
REQUEST_CHANGES`; GitLab `POST …/approve`); a request-level comment with no line, which is
where a findings summary could go; and a `[forge]` config table, should a tool ever need a
flag the defaults do not give. Reactions are not planned.

## Status

Implemented: the engine types and trait, both adapters, `--pr` and `--mr`, threads in the
reviewer, replies, resolve, publish, edit and delete of the reader's own comments, and the
findings list with threads. Verified live on GitHub: reading a request and its threads, and
one publish. Not verified live: editing and deleting on GitHub, `gh api user`, and anything
GitLab.
