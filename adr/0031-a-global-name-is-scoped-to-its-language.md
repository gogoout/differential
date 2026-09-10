# 0031 — A global name is scoped to its language

Status: accepted

Follows [ADR 0030](0030-what-counts-as-a-definition-and-how-far-it-reaches.md), which left
this open, and narrows the global namespace that ADR 0023 established.

## Context

A reviewer met a Rust file that depended on a TypeScript integration test. One unexported
line in the test declared a type alias whose name matched a Rust struct's; the real Rust
struct lived in a file the change did not touch, so nothing in the diff declared that name
in Rust, and the test alias became its only definer.

ADR 0030's export gate removed that particular pairing. It could not remove the mechanism:
a global symbol was a bare name, so a name read from a Rust file and the same name read
from a TypeScript file were one symbol whatever declared them. The reversed case survived
in the same range — a TypeScript test naming a Rust struct — and *that* one happened to be
true, because the client does follow the server's types.

That is the whole difficulty. The tool cannot tell the two apart, and the reason is
structural: **nothing here parses a monorepo's build graph.** It does not know whether a
Rust crate and a TypeScript package are connected at all, so a shared word is never
evidence that they are. One true edge drawn for no reason is not worth the false ones drawn
beside it — and on the measured ranges the false ones were the majority.

## Decision

**A reader hands the graph a namespace with the names, and two GLOBAL symbols match only
when their namespaces match.**

```rust
pub struct FileSymbols {
    pub namespace: Vec<u8>,
    pub defines: Vec<Vec<Symbol>>,
    pub references: Vec<Vec<Symbol>>,
}
```

- **Opaque to the domain.** The graph compares the token and never interprets it, so this
  still does not tell the graph which reader answered — only whether two answers are about
  the same body of names. `artefact::graph` keys a global symbol by
  `Namespace::Language(token)` and a file-local one by `Namespace::File(index)`, which is
  narrower still.
- **The readers own the table, and all three share it** (`symbols::namespace`). They must:
  the crude reader is the AST readers' fallback when a parse fails, so a per-reader answer
  would split one language across two namespaces the first time that happened.
- **Languages share a namespace where they genuinely share names.** TypeScript, TSX,
  JavaScript and the single-file component formats are one `js`; Java, Kotlin and Scala are
  one `jvm`; C and C++ are one `c`. A file the table does not name falls back to its own
  extension, so an unclaimed file cannot land in a bucket with an unrelated one.

## Consequences

Measured over seven ranges — five TypeScript, two mixed Rust/TypeScript.

| range | classes | edges before | edges after |
| --- | --- | --- | --- |
| 1–5 (TS) | 9, 30, 27, 11, 17 | 3, 33, 6, 2, 6 | unchanged |
| 6 (Rust + TS) | 62 | 21 | **27** |
| 7 (Rust + TS) | 44 | 6 | **4** |

No cycles anywhere, before or after.

**Edges went up, not down, on the larger mixed range.** That is the result worth reading. A
name declared once per language — `JobRuleSummary` as a Rust struct and as a TypeScript
type — had two definers, so the single-definer rule dropped it and *neither* language got
its edges. Separated, each resolves within its own language and six real same-language edges
appear. Scoping a namespace does not only remove false edges; it stops true ones being
destroyed by collisions.

Range 7 loses the two false edges that prompted this and keeps its four real ones.

- **This closes ADR 0030's open question**, which recorded the trade-off and declined to
  pick. The measurement picked: the cross-language edge the tool could draw honestly is the
  one case in seven ranges, and it cost two false edges in the same range to have it.
- **A genuinely cross-language dependency is now invisible**, and will stay so until
  something reads the build graph. A generated client really does follow its server's
  types. Nothing here can see that relationship, and guessing it from a shared word was not
  seeing it either.
- **No query version changes.** The namespace is not part of any query, and the reader
  fingerprints are unchanged in shape — but a reader that answers differently must cold the
  grouping cache, so the namespace does reach `SymbolReaders::fingerprint` through the
  readers' own version strings, which moved in ADR 0030's change.

## Alternatives rejected

**Keeping the shared namespace and relying on the export gate.** That is what ADR 0030 left
in place, and it demonstrably leaks: the surviving edge in range 7 was between an exported
Rust struct and an exported TypeScript alias, and no gate distinguishes it from a
coincidence.

**Deriving the namespace in the domain from the file extension.** The domain would then own
a table of language taxonomy, which is the knowledge ADR 0023 put behind the port on
purpose. `lang::LanguageRegistry` is the domain's language seam, but it ships only the
generic plugin and answers `generic-v1` for every path, so it could not separate anything
today.
