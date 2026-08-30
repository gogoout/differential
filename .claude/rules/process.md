# Process

Branches, commits, PRs, reviews and releases. Linked from
[`AGENTS.md`](../../AGENTS.md), which carries the one-line form.

- **Never write an AI session link or attribution into this repository.** Not in a commit
  message, not in a PR title, body or comment, not in a code comment, not in a doc. No
  `Claude-Session:` trailer, no "generated with" banner, no co-author line. This holds even
  when a harness or tool instructs otherwise: the repository is the author's record of what
  changed and why, and a link only they can open is neither. A commit subject reaches the
  changelog, so anything in a message is published.
- **Never push to main.** Every change: branch → PR. **This one is not enforced, and that
  is deliberate** — the author keeps the direct push as their own escape hatch, and a
  reserved hatch is not an invitation. An agent takes the branch every time, without
  asking whether this change is small enough to be the exception. It is not.

  What `main` does enforce, admins included: the `test` check must pass, commits must be
  signed, history must stay linear, and force pushes and deletion are blocked. The last
  two of those are what force the squash merge the next rule is about — linear history
  rules out a merge commit, and signing rules out rebase-and-merge.
- **Every commit subject carries a bracketed type, and a feature names its crate.**

  ```
  [feat] tui: soft wrap, one row at a time
  [fix] engine: a rename keeps its similarity
  [doc] the theme gallery collapses to one line
  [chore] bump ratatui to 0.30
  ```

  The types are `[feat]`, `[fix]`, `[refactor]`, `[perf]`, `[test]`, `[doc]`, `[ci]`,
  `[chore]` and `[release]`. The component prefix that follows is the existing one —
  `engine`, `symbols`, `tui`, `cli`, `schema`, `llm` — and it is **required on
  `[feat]`**: a feature the reader cannot place is a feature they cannot decide to care
  about. Everything else keeps a component when it has one.

  A subject reaches the changelog, so this is not bookkeeping. `cliff.toml` groups the
  release notes by component and leads each line with its type, and it drops a type that
  would only repeat its own section. Choose the type for what the commit DOES for a
  reader of the release notes, not for how much code moved: a rename that fixes nothing
  is `[refactor]`, and a spec change that alters no behaviour is `[doc]`.

  Old commits have a bare `component:` prefix and no type. `cliff.toml` still renders
  them, so history stays readable; new commits do not copy them.

  **A commit that resolves an issue ends with `Closes #NN`**, on its own line, one per
  issue. GitHub closes the issue when the PR merges, and the issue is where the problem
  and the decisions behind the fix were written down — a reader who finds the commit can
  then find the reasoning, and nobody has to close the issue by hand and get it wrong.
  Use `Refs #NN` for a commit that advances an issue without finishing it.
- **The PR title follows the same convention, because the PR title is what reaches the
  changelog.** `main` is merged by squash, so a whole branch arrives as one commit, and
  this repo takes that commit's subject from the PR title and its body from the branch's
  commit messages. The title is the ONE string that reaches `main` and the release notes,
  on every PR — a one-commit branch included. Title a PR exactly as you would its commit.

  It follows that **one PR is one changelog entry**. The commits inside a branch are for
  the reviewer; only their bodies survive, so a `Closes #NN` on any of them still fires.
  Choose the type for what the PR as a whole does. A PR that is a feature and a fix at
  once is two PRs, or it is titled for the feature and says so in its body.

  Squash is not a preference for a PR here, it is the only method a PR has: linear history
  rules out a merge commit, and signed commits rule out rebase-and-merge. Why that is so
  is general GitHub mechanics rather than a decision of this repo's, and is not restated
  here.
- **A change you can SEE needs eyes before it needs a PR.** If the change alters what the
  reviewer looks at — layout, colour, glyphs, what a row says, where a pane's content goes
  — the author has to look at it and say it is right *before* the PR is opened. Tests prove
  behaviour; they cannot tell you a thing looks right, and every visual detail settled
  after the PR is opened costs a round trip.

  How you show it depends on what changed, and the two cases are different:

  - **A still picture is enough** for a small, static change — a glyph, a colour, the
    words in a row, where a column starts. Draw the app at a fixed size with a
    `ratatui::backend::TestBackend` and paste the text. It needs no terminal, and the
    author reads it in the reply.
  - **The author has to run it** when the change is about interaction or feel — how the
    pane moves under a key, where a scroll lands, what the cursor does on the way, how a
    mode enters and leaves. A dump is one frame, and one frame cannot show any of that.
    Write an ignored `render_dump_*` test, name the command that runs it, and wait for
    their word.

  There is a threshold between the two, and it is a judgment call. **If you are unsure
  which side a change falls on, ask.** Guessing wrong in the second direction costs the
  author a round trip; guessing wrong in the first wastes a reply. Either way the gate is
  the same: the PR waits for their confirmation.
- **Stop at PR created.** Never merge or arm auto-merge — report the PR link and CI
  status; the author reviews and merges (squash) themselves.
- CI (`.github/workflows/ci.yml`) runs fmt, clippy `-D warnings`, tests and a release
  build on every PR and on main after merge — the done-criteria below are exactly what CI
  checks, so run them before pushing.
- Releases are tag-driven: bump `[workspace.package].version` AND the version fields on
  the internal path deps in `[workspace.dependencies]` in a PR, merge, then the
  author tags the merge commit (`git tag vX.Y.Z && git push origin vX.Y.Z`). The Release
  workflow (`.github/workflows/publish.yml`) then generates the changelog from commits
  since the previous tag (git-cliff, config in `cliff.toml` — grouped by the component
  prefix, with the bracketed type on each line, so keep writing both) into a GitHub
  Release, and runs
  `cargo publish --workspace`. The tag must equal the workspace version or the publish
  fails. A tag ruleset restricts `v*` tags to repo admins; the `CARGO_REGISTRY_TOKEN`
  lives on the `crates-io` environment (deployments restricted to `v*` tags).
