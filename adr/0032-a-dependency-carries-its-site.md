# 0032 — A dependency carries its site

Status: accepted

Follows [ADR 0023](0023-symbol-extraction-is-a-domain-port.md),
[ADR 0030](0030-what-counts-as-a-definition-and-how-far-it-reaches.md) and
[ADR 0031](0031-a-global-name-is-scoped-to-its-language.md), all of which stand. Nothing
here changes which edges the graph draws.

## Context

The graph answers one question: *class C2 depends on class C7, via `lookUpName`.* That is
the question the ordering stage asks, and answering it is why the extraction exists.

It is not the question a reviewer asks. Reading a hunk, the question is *what is this thing
on the line in front of me* — and the tool had the answer and threw it away, inside a
forty-line window:

1. **`FileSymbols` is already per-line** (`artefact::symbols`). Whole files are parsed, so
   every line of every changed file has its symbols, not only the added ones.
2. **The tuned reader already computes each capture's byte range** (`ast/tuned.rs`). It uses
   it as an identity key to veto a definition also matching `@type`, and drops it.
3. **`graph::build` is the one place `(class, hunk, line, symbol)` coexist.** It folds them
   into per-class `BTreeSet<Key>`s, and by the time the edges are emitted the file, the
   line, the scope and the namespace are all gone, leaving bare `String` names.

So a reviewer who wanted to see what a name was declared as had to go and find it by hand,
in a tool whose entire subject is what depends on what.

## Decision

**A symbol carries where it is, and a definition carries how far it reaches.**

```rust
pub struct Site {
    /// Byte offsets within the token's own RAW line.
    pub start: u32,
    pub end: u32,
    /// Last line of what this name declares. Zero when unknown.
    pub through: u32,
}

pub struct Symbol {
    pub name: Vec<u8>,
    pub scope: Scope,
    pub site: Site,
}
```

- **The graph cannot see it.** `graph::key` builds its key from the namespace and the name
  alone, exactly as before. Edge counts, the single-definer rule and the corpus parity
  figures do not move, and the parity test is what says so.
- **`Symbol::global(name)` and `Symbol::local(name)` keep their signatures**, leaving the
  site at its default; readers add precision with `.at(start, end)` and `.through(line)`.
  A reader that cannot answer says so by not answering, and the domain's own tests and the
  stub reader are untouched.
- **Raw-line offsets, not display columns.** The TUI expands tabs and right-trims every
  line before drawing it, so a consumer must translate rather than index its display text
  with these. That is stated on the field, in the schema and in the contract, because it is
  the one place the two coordinate systems meet and a silent mismatch mis-highlights every
  tab-indented file.

**The extent is the captured node's PARENT, and no query changed to get it.** In every one
of the six tuned queries the `@def` capture is the NAME identifier and its parent is the
declaration that name introduces — `(function_item name: (identifier) @def)`,
`(function_definition name: (identifier) @def)`, `(method_declaration name:
(field_identifier) @def)`, `(variable_declarator name: (identifier) @def)`. The field-rule
reader reaches a name through the same relation.

An explicit `@def.extent` capture would be more precise and was rejected: it edits six
`.scm` files to buy a difference that appears only where the parent is narrower than the
declaration — a Rust `fn` under a multi-line attribute, say. **That error is in the safe
direction.** A parent contains its child, so the extent can stop SHORT and show fewer lines;
it can never run past the declaration into the next one. A cheap rule whose failure mode is
"showed less" did not justify moving every query version.

**A use is recorded on any line of a parsed file, not only an added one.** The graph reads
added lines because it is asking what the CHANGE introduces and consumes. A reviewer opens
context with `z` and lands on unchanged lines, and a token that resolves there resolves for
them too. The cost is bounded by dependencies rather than by file size: an occurrence is
recorded only when its name resolves to a definition the change makes.

**The index reuses the graph's own single-definer verdict.** A name two classes declare is
absent from the index for exactly the reason it draws no edge. Computing a second opinion
would be a bug waiting for a corpus to find it.

**A declaration is not a use of itself.** The crude reader has no veto — its reference regex
takes every identifier on a line, the name just declared included — so `fn helper()` reports
`helper` as reading `helper`, and a reader following it would be sent to the line they are
already standing on. The test is POSITION, not name, so the same name genuinely used again on
its own declaring line — a default argument, a one-line recursive call — is still a use.

**It reaches the document as an additive field, and the schema stays at 3.** `symbols` is
`Option<SymbolIndex>` with `#[serde(default)]`, so every artefact written before this
deserialises unchanged — which matters, because stored documents are re-read by
`dfr agent --doc` and replayed against the grouping cache. The freeze
([`constraints.md`](../.claude/rules/constraints.md), ADR 0022) allows exactly this shape,
and `generator.stages` needs no new entry: the index is produced in `classify`, beside the
class graph that is already listed there.

## Consequences

- **Every grouping cache entry in every checkout goes cold, once.** All three readers answer
  differently now, and the port's contract is that a reader which answers differently colds
  the cache (`artefact::symbols`). ADR 0031 paid this knowingly and so does this.
- **The tuned reader gains a version of its own**, `ast-tuned-v2[…]`. Its fingerprint was
  built purely from query versions, so a change to its Rust that edits no `.scm` could not
  move it. Every earlier change to this reader happened to touch a query as well, so the
  hole never fired; adding `Site` is the first Rust-only change to walk into it. This is the
  same hole ADR 0031 found and closed for the crude reader, one rung up. `ast-fields` and
  `naive` bump for the ordinary reason.
- **The crude reader reports no extent.** A regex has no tree to ask how far a declaration
  runs, and `through: 0` says that rather than guessing at the next blank line. A consumer
  reads zero as "the declaring line is all there is". It does report columns, which its
  matches always had.
- **No invariant covers any of this**, and that is correct (`spec/invariants.md`): a wrong
  entry shows a reader the wrong snippet, which they can see is wrong. It cannot hide a hunk
  and it cannot move a group. Correctness here is a test and corpus question.
- **A cross-file dependency into an untouched file is still invisible.** Only files with
  hunks are parsed, so a call into a helper the change does not touch resolves to nothing.
  That is the honest limit of a tool that reads a diff rather than a repository, and it is
  the same limit [ADR 0031](0031-a-global-name-is-scoped-to-its-language.md) recorded for
  cross-language edges.

## Alternatives rejected

**Letting the renderer find the token by searching the line for the name.** It needs no
engine change at all, and it is wrong whenever a name appears twice on a line or as a
substring of a longer identifier — which is most lines that are worth asking about. The
mechanism already knows the exact range; searching for it again is discarding information
and then approximating it (design rule 3).

**Widening `ClassEdge.via` to carry sites.** `via` is a frozen field and its element type
is `String`; changing that breaks the schema. An additive sibling would work, but the edge
is the wrong home: an edge is per class PAIR, and the same name used on four lines produces
one edge. The reviewer's question is per token.

**Indexing the whole repository so every name resolves.** It answers more, and it is a
different tool. Every stage here reads a diff and the blobs its paths name; a repo-wide
index wants a traversal, a cache keyed on something other than the range, and an answer to
what happens when it is stale. That is its own ADR, and this one does not foreclose it.
