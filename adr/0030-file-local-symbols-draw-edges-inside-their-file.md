# 0030 — A file-local name draws edges inside its own file

Status: accepted

Extends [ADR 0023](0023-symbol-extraction-is-a-domain-port.md), which stands.

## Context

A reviewer met this range: one hunk introduces
`const label = lookUpName() ?? fallbackName;` inside a React component, and three later
hunks in the same file render it. The reading plan put the
declaration in one group and the three uses in another, and said **nothing** connected them.
The grouping model, which reads the class graph, wrote "defines the new value every display
site below consumes" in prose — while the graph it had been handed said the class defined
nothing at all.

Three causes, and only the second is a decision:

1. **TypeScript's file-scope value declarations were missing from its query.** No
   `lexical_declaration` pattern, not even under `program`. Rust captures
   `(source_file (const_item …))`, Go `(source_file (const_declaration …))`, Kotlin
   `(source_file (property_declaration …))`. So `export const Panel = () => {}` — the
   dominant declaration form in a TypeScript codebase — defined nothing.
2. **The binding is declared inside a function.** ADR 0023's rule, "a definition is
   a file-scope name others can use", excludes it on purpose.
3. **A bare identifier read is not a reference.** The query captured `@call` and `@type`
   only, and `{label}` is neither. Nor is `<Child …/>`: in the TSX grammar
   a JSX element's name is `identifier`, never `type_identifier`, so **rendering a component
   drew no edge at all** — the main consumption relation in a React codebase was invisible.

Measured on a TypeScript corpus of five ranges: **four produced zero edges and zero
definitions**. The fifth produced thirteen, every one of them from an `interface` or `type`
name. The graph was a *type* graph.

## Decision

**A symbol carries a scope, and a file-local name may only draw an edge between classes in
the same file.**

```rust
pub enum Scope { Global, File }
pub struct Symbol { pub name: Vec<u8>, pub scope: Scope }
```

`artefact::graph` keys a global name by the name alone and a file-local name by
`(file, name)`. Everything else is unchanged — including the single-definer rule, which now
judges ambiguity per file: two files each declaring `label` are not a clash, and one file
declaring it twice still is.

**This does not reopen what ADR 0023 closed.** That ADR's failure was `mod template;` and
`fn from` becoming *globally* unique symbols that every file mentioning the word then linked
to — six words, 64% of one range's edges. A file-local name cannot do that however common it
is. It is compared only against its own file's answers, so the worst a wrong one costs is an
ordering inside one file, and the candidate set it competes in is small enough that the
single-definer rule usually resolves it.

Three consequences for the readers:

- **Tuned queries gain `@local_def` and `@local_ref`.** The locals are exactly the
  declarations the file-scope rule deliberately drops — a binding inside a function, a
  parameter, an import, a method — and `(identifier) @local_ref` is everything that might be
  reading one.
- **TypeScript gains its exported file-scope value declarations, and TSX its JSX names.**
  Both are corrections, not extensions: the first is the convention every other query
  already follows, and the second is a call by another spelling.
- **The field-rule reader calls every other declaration file-local.** It cannot tell which
  of them sits at file scope — a JavaScript `export const Panel = …` looks like any other
  `variable_declarator` from there — so the conservative reading is the honest one. It costs
  cross-file edges those names never drew anyway.

**Exported, and only exported.** A definition is a file-scope name *others can use*, and in
a module system `export` is exactly that predicate. Counting a bare top-level
`const send = vi.fn()` in a test file linked every production file calling `send` to that
test, and closed a two-class cycle with the true edge running the other way — ADR
0023's own failure, reappearing through the new rule. Unexported, the same name is still
read as file-local, so it keeps every edge it can honestly draw.

## Consequences

Measured over five ranges of a TypeScript corpus. `sccs` is the number that matters: a
topological sort works if and only if every strongly connected component has size one.

| range | classes | edges before | edges after | sccs before | sccs after |
| --- | --- | --- | --- | --- | --- |
| 1 | 9 | 0 | 3 | 0 | 0 |
| 2 | 30 | 13 | 33 | 0 | 0 |
| 3 | 27 | 0 | 6 | 0 | 0 |
| 4 | 11 | 0 | 2 | 0 | 0 |
| 5 | 17 | 0 | 6 | 0 | 0 |

Not one new cycle, and the edges that arrived are component composition, util calls and
analytics builders — the structure a reviewer of that change actually needs. Ranges 3 and 5
gained edges from the scoping alone: names that were ambiguous as globals resolve as locals.

- **The readers' fingerprints all change, which colds every cached grouping** by design
  (`grouping/key.rs`): the class graph is part of what the model reads (ADR 0022).
- **`(identifier) @local_ref` turns a handful of captures per file into one per token**, and
  `is_prose` answers each by climbing to the root. That is the quadratic shape
  `deep_nesting_costs_neither_stack_nor_quadratic_time` exists to catch, and it fired: the
  tuned reader now collects prose token ranges in one linear pass, the way the field-rule
  reader already carried its flags down. `is_prose` survives as the rule's definition and as
  the fallback for a capture that is not a token.
- **A query's version is now pinned to its text by a test.** Six versions moved in this
  change, and a version reaches the cache key — a forgotten bump serves a stale grouping for
  a graph that moved.
- **Rust's `(source_file (const_item …)) @def` does not check `pub`**, so it has the same
  latent shape as the TypeScript rule above. Left alone: it was there before this change,
  Rust constants are conventionally `SCREAMING_CASE` rather than common words, and moving it
  belongs to its own measurement.

## Alternatives rejected

**Scoping the crude reader's references too.** Its "every identifier of four characters or
more" is the loosest thing in the system and the obvious next candidate. But it is the only
reader Ruby, PHP, Swift and Elixir have, and file-scoping it would delete every cross-file
edge those languages draw. That is a precision question with its own corpus measurement, and
bundling it here would make one movement in the numbers impossible to attribute.

**Splitting a group when the graph says it holds both a definition and its use.** The merge
is the model's judgement (ADR 0001) and the graph that would undo it is heuristic. A wrong
edge misorders; a wrong cut breaks a coherent group and mislabels both halves.
