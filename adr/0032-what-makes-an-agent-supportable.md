# 0032 — What makes an agent supportable, and the one that is not read-only

Status: accepted. Extends [0022](0022-the-model-fetches-its-own-context.md).

## Context

ADR 0022 settled that the agent is chosen **by name**, not by argv, and said why: the
grouping stage does not merely spawn a process. It hands its agent a tool allowlist, a
fetch command and a prompt written for what that agent can do, and an arbitrary argv got
the prompt and none of the rest. It also said what follows — *"adding an agent is adding a
variant here"*.

One agent shipped under that rule. Issue #108 asks for the rest, and "the rest" turns out
to be a much smaller set than the market suggests.

## The contract

`CommandBackend::complete` pipes the prompt to stdin and reads stdout. The response parser
takes the span between the first `{` and the last `}`. ADR 0022 requires the tool
permission to come from the argv this crate writes, because the moment it comes from a file
the user maintains, the knob is back and it looks like it works.

Five things follow, and an agent must do all five:

1. Take the whole prompt on **stdin**, with nothing in argv.
2. Print the **final assistant message only**, as plain text, with no ANSI.
3. Be restricted to **read-only by flags alone**.
4. Be able to run an **arbitrary named binary** — the fetch command — and `git diff`.
5. Not stall on an approval prompt when there is no TTY.

Point 5 is not hypothetical. Several of these CLIs default to asking a human per tool call,
and a headless call that asks sits until the 1200-second deadline kills it.

## Eleven candidates, five survivors

| CLI | stdin | plain text | read-only from argv | |
|---|---|---|---|---|
| Claude Code `claude -p` | yes | yes | `--allowed-tools` | ships since 0022 |
| Codex `codex exec` | yes, `-` | yes, default | `--sandbox read-only --ask-for-approval never` | **added** |
| Droid `droid exec` | yes, `-` | yes, `-o text` | read-only is the default | **added** |
| Copilot `copilot` | yes, when no `-p` | `-s` | `--allow-tool` / `--deny-tool` / `--no-ask-user` | **added** |
| Pi `pi -p` | yes | yes | **no** | **added anyway — see below** |
| Gemini `gemini` | yes | `-o text` | plan mode denies `run_shell_command` headless | no |
| Cursor `agent -p` | undocumented | yes | needs `.cursor/cli.json` | no |
| Amp `amp -x` | yes | yes | needs `~/.config/amp/settings.json` | no |
| opencode `run` | **no** | unclear | needs `opencode.json` | no |
| Crush `crush run` | yes | `-q` | `bash` is all or nothing | no |
| Goose `goose run -i -` | yes | `-q` | all or nothing | no |
| Aider | no | flag combination | no model-driven shell tool | no |

This table is the decision, not the evidence for it. Four of the six refusals are the same
refusal: the agent's read-only policy lives in a config file, so supporting it means either
writing that file on the user's behalf or telling the user to write it. The second is the
knob ADR 0022 removed. The first would make this tool an editor of other tools' settings,
for four agents, on three platforms — and it does not even work for Amp, which documents no
flag for loading a settings file from a path.

opencode fails point 1 outright: it has no stdin, and the request for one was closed as not
planned. Aider fails point 4 by being a different kind of program — it edits files at a
user's direction and has no shell tool a model can choose to call.

## Two mechanisms make an agent read-only, and ADR 0022 assumed one

ADR 0022 said the tool allowlist is derived from the fetch command, so the prompt can never
name a command the model is not permitted to run. That is true of Claude Code and Copilot,
and meaningless for Codex and Droid, which have no allowlist at all: their boundary is an OS
sandbox — Seatbelt on macOS, bubblewrap on Linux — under which the model may run anything
and the kernel refuses the writes.

So the rule widens rather than breaks. **The model must be able to run the fetch command,
however that agent expresses it.** For an allowlisted agent that is still a derivation, and
`fetch` still appears in the argv. For a sandboxed one there is nothing to derive and
`fetch` does not appear at all.

The sandbox is the stronger of the two. An allowlist holds against a model that asks; a
sandbox holds against a model that tries.

Droid is a third case and worth naming separately, because it is weaker than either: its
read-only state is a **default**, so its boundary is the flags this crate does not pass
(`--auto`, `--skip-permissions-unsafe`). A default holds only until someone adds a flag, and
the test for it is written as an absence for exactly that reason.

## Pi can write, and that was a decision

Pi fails point 3 and ships anyway. This is the part of this ADR worth reading twice.

Pi has no sandbox and no per-command allowlist. Its own documentation says so: *"Pi does not
include a built-in sandbox. Built-in tools can read files, write files, edit files, and run
shell commands with the permissions of the pi process."* It ships no approval prompts either,
deliberately. Its `-t` flag toggles whole tools, and `bash` is one tool — the same tool the
model needs to run the fetch command and `git diff` is the one that lets it write, commit
and push.

The alternative was `-t read,grep,find,ls`: a real boundary, and no fetch command. That
leaves the model grouping from the class ids alone, which is precisely the truncated payload
ADR 0022 was written to end — a worse grouping every single time, bought with a risk the
prompt never asks anyone to take.

So Pi ships with the boundary absent and the absence stated. `config::ReadOnly::NotEnforced`
is the type that carries it, `Agent::read_only` is how any caller asks, and a caller that
offers a user the list of agents must show it. `dfr agents` prints *"NOT read-only: it can
write, commit and push"* on Pi's row, in a sentence rather than a tidy label, because a
reader skimming four tidy phrases will skim past a fifth.

Nothing in `constraints.md` is contradicted by this. It is not a constraint that was
loosened; it is a property four agents have and a fifth does not.

## An argv in this repository is unverified until someone runs it

This is the honest consequence and it has no clean fix.

Every command line here is written from its agent's documentation. No test in this
repository can check one, because the CLI on the other end is not installed here, and its
flags change by version. CI cannot answer it either. The tests in `llm.rs` assert the string
this crate builds — that a flag is present, that a boundary-removing flag never is, that no
two agents share a cache identity — and every one of them would pass against an argv the
real CLI rejects.

So the check moves to where the CLI actually is. `dfr agents --probe <name>` makes one real
call and reports four facts, and each is a way this fails quietly:

- **spawn** — the executable is missing or renamed. Loud already.
- **stdin** — the reply carries a nonce only a reader of that prompt has. Without it the
  agent took its prompt from argv, or printed an envelope, or decorated the reply, and the
  grouping call would fail to parse minutes in.
- **fetch** — the reply names a marker file only a reader of the probe document can know. A
  class id would not do: ids are `C0..Cn` and a model that ran nothing can guess `C0`. This
  is the expensive one, and ADR 0022 says why: a model does not give up on a failing tool, it
  works around it, and the run then succeeds and groups worse with nothing to show for it.
- **write** — the probe asks for a file and the **filesystem** answers, never the agent's
  account of itself. An agent that reports a refusal and wrote the file anyway is the exact
  failure worth catching. For the four enforced agents a write is a failure; for Pi it is
  documented behaviour and the probe says so rather than pretending.

### The probe found the default agent was not read-only

This is not a hypothetical, and it is the reason this section is not an apology for a
missing test.

The first `dfr agents --probe` ever run was against `claude-code` — the agent that had
shipped since ADR 0022 as the only option — and it wrote the file. ADR 0022 says *"Nothing
here can write"*, and that had not been true for anybody whose own Claude Code settings set
`defaultMode` to `auto`, `acceptEdits` or `bypassPermissions`.

The cause is a precedence nobody had reason to check: **`--allowed-tools` adds permissions,
it does not cap them.** A setting in the user's own `~/.claude/settings.json` outranks it. So
the argv was asking for a boundary rather than imposing one, and the difference is invisible
from inside this repository — the flag is present, the tests pass, the tool list is right.

`--permission-mode default` is the fix, confirmed the same way it was found: the write comes
back refused and the file is absent. `default` means ask, and a headless call has nobody to
ask.

**Codex is sealed for the same reason**, though nobody here can probe it: its sandbox is
derived from a permission profile in `$CODEX_HOME/config.toml`, so a user's file can widen
what the flag asked for. `--ignore-user-config --ignore-rules` closes it. Pi was already
sealed, by `-na --no-extensions --no-skills`, which were passed for determinism and turn out
to buy this too. Copilot's precedence is undocumented and remains a question the probe will
answer on the first machine that has it.

The general rule, and the one an author adding a sixth agent should carry: **an argv a
user's own config file can widen is not a boundary.** Find that agent's flag for ignoring
its user config, and pass it.

The cost was one round of invalidation. `claude_argv` feeds the backend identity, which feeds
the cache key, so every cached grouping in every checkout was dropped once. That is automatic
rather than a migration, and it is the cheaper half of the trade by a wide margin.

Three details are documented but unproven, and the probe is how each gets settled: Copilot's
`--allow-tool` filter grammar, which appears in examples and is never specified; Droid's risk
classifier, which is not documented to say whether an unknown third-party binary passes at
the read-only tier; and Copilot's exit code, which an open bug reports as 0 after producing
no output — which is why the probe judges on stdout and not on status.

## Consequences

- **Every cached grouping is invalidated once, and it is not the new agents that did it.**
  The prompt does not change, so `PROMPT_VERSION` stays 6. But `claude_argv` gained
  `--permission-mode default`, and the identity it feeds is part of the cache key — so every
  checkout re-runs its next grouping. The alternative was leaving the default agent
  write-capable to keep a cache warm.
- **Each agent gets its own cache entries**, because identity is the argv and the argvs
  differ. A test asserts no two share one: an agent serving another's grouping under a name
  it never ran would be undetectable from the outside.
- **The hermetic flags are load-bearing, not tidiness.** Pi's `-nc -na --no-extensions
  --no-skills` each drop a file outside the cache key that could otherwise change a grouping.
  ADR 0022 names that hole — a model reading `git log` reads history no key can capture — and
  these flags close the part of it that is closable.
- **`Config::load_user` exists** so `dfr agents` can answer without a repository. Which agent
  you run is a per-user choice and the question is answerable from anywhere; handing the
  command a repository root it has no use for would have been the alternative.
- **`ReadOnly` is a four-variant enum, not a bool.** The three enforcing answers are not
  interchangeable to someone deciding whether to trust one, and the fourth needs a sentence
  rather than a label.
- **Adding a sixth agent is still three edits and a compiler error.** A variant in
  `config::Agent`, a constructor in `llm.rs`, an arm in `backend_for`. What the compiler
  cannot ask for is the boundary, which is why it is written down here and in the module
  header rather than left to the next reader to infer from four examples.
