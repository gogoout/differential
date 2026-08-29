# 0023 — Symbol extraction is a domain port, with tree-sitter readers behind it

Status: accepted

## Context

The class dependency graph decides reading order. On the validation corpus it was mostly
wrong, and tracing one group showed why.

The extractor was a pair of regexes hanging off `Language` (ADR 0015). `DEF_RE` listed
`mod` among its declaration keywords, and `fn` caught trait methods:

```rust
mod template;                        → defined the global symbol "template"
impl From<X> for Y { fn from(..) }   → defined "from"
```

Only one class defined each, so the ambiguity guard never fired. `template` became a
globally unique symbol, and then every file containing the word linked to it. Six such
words — `template`, `from`, `error`, `into`, `generated`, `sqlx` — produced **64% of one
range's 131 edges**. A further **32% came from classes made entirely of `.toml`, `.md`,
`.sql` and `.json` files**: a manifest "depended on" an error module via the word `from`,
a README via prose.

The cost was not cosmetic. A topological sort works if and only if every strongly connected
component has size one. Both ranges held **one knot** — 16 classes and 12 classes — and in
the smaller one that knot left **every class with an out-edge unorderable**. The ordering
stage fell back to sorting by size, which is the failure it exists to fix, on 23 of 51
group edges.

Three problems, and only the third is about parsing:

1. **The domain picked an implementation.** `artefact::graph` called the generic regexes by
   hand as a fallback.
2. **The port shipped an answer.** `Language::file_symbols` had a default body, so the crude
   reader was the trait's opinion rather than one implementation among peers.
3. **A regex cannot tell `mod template;` from a definition worth having.**

## Decision

**Symbol extraction is a domain use case with its own port.** The graph needs to know what
each line defines and references. Whether a regex or a parser answered is not a distinction
it can see or act on — those are the same capability at different effort and precision.

```rust
pub trait SymbolSource {
    fn priority(&self, path: &[u8]) -> Option<u8>;
    fn file_symbols(&self, path: &[u8], content: &[u8]) -> Option<FileSymbols>;
    fn fingerprint(&self) -> String;
}
```

- **A reader ranks itself.** `priority` returns how good this reader's answer would be for
  this path, or `None` if it does not read the file. Nothing outside the reader knows why
  one beats another, and no registration order can get the ranking wrong.
- **`SymbolReaders` holds the rule**, and the rule is the whole policy: ask the best
  claimant; fall to the next best if it fails; **if nobody claims the file, take no symbols
  from it.**
- **`dyn`, and correctly so.** Which reader answers is chosen at run time, per file — the
  case the layering rules reserve `dyn` for.
- **Readers live in `crates/symbols`, which depends on the engine and never the reverse.**
  They are not optional: a build wiring none produces no edges at all. The crate is separate
  so the engine's own test builds do not compile eleven grammars, and so the mechanism can
  leave the engine later with the graph it serves.
- **`Language` keeps normalisation and nothing else** (ADR 0015). One trait serving two
  unrelated needs is the merged supertrait the layering rules forbid.

Three readers ship, and the graph cannot tell which answered:

| reader | reads | how |
| --- | --- | --- |
| tuned | Rust, TypeScript (+TSX), Python, Go, Kotlin | a hand-written `.scm` per language |
| field-rule | JavaScript, Java, C, C++, C# | tree-sitter with no query, using field names |
| crude | any other source extension | the moved regexes, at the floor |

**A definition is a file-scope name others can use.** Not `mod template;`, which names a
module. Not `fn from` inside an `impl`, which is reached through its type.

**A file no reader claims contributes nothing.** Manifests, lockfiles, prose. This is
classification, never enumeration — the file, its hunks and its classes all still exist
(ADR 0005, 0012).

## Consequences

Measured on the corpus, class dependency edges and classes inside a cycle:

| range | before | readers, no parser | with the AST readers |
| --- | --- | --- | --- |
| first | 288 edges, 16 in a cycle | 219, 15 | **63, 2** |
| second | 131 edges, 12 in a cycle | 95, 11 | **26, 0** |

The middle column is worth reading. Declining manifests and prose removed a quarter of the
edges and **barely touched the knot** — because the knot was tied by symbols in code. Edge
count was never the target; the cycle was.

- The grouping cache key hashes the readers' fingerprint. The class graph is part of what
  the model reads (ADR 0022), so readers that answer differently must cold the cache.
- A tuned query is compiled when the reader is built, so a wrong node name fails loudly and
  names the node. The field-rule reader has no query to fail, so it probes its grammars
  instead and declines any that cannot support the rule — a silent zero is the failure
  nobody sees.
- Adding a language is one table entry. Adding a query moves a language up a rung and
  changes the fingerprint, which colds the cache by itself.
- Engine tests use a stub reader rather than dev-depending on the adapter crate. The real
  readers are measured where they live, against the corpus.

## Alternatives rejected

**SCIP or another indexer.** ADR 0015 already rejected it and nothing here changes that: an
indexer wants a working build per language against a checkout of the right revision, to
feed a stage that tolerates noise. Tree-sitter needs only bytes.

**stack-graphs.** Archived 2025-09-09, last published 2024-12-13, four languages, and it
pins `tree-sitter ^0.24` against grammars now at 0.25 and 0.26.

**Vendoring nvim-treesitter's `highlights.scm`.** Wide — around 151 grammars — but those
files use predicates the Rust query engine does not support (`#lua-match?`,
`#has-ancestor?`) and pin to grammar versions we do not control. Our own queries are ten
patterns each and fail loudly against the grammar we actually ship.

**Keeping the regex for definitions in the field-rule reader.** An earlier draft did, on the
grounds that a false definition *deletes* an edge by making a symbol ambiguous. The corpus
showed the opposite failure is far larger: a false definition on a common word manufactures
dozens.
