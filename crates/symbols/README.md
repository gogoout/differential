# differential-symbols

The symbol readers for [`differential`](https://crates.io/crates/differential). They answer
one question about a file — **what does each line define, and what does it reference?** —
and the answer becomes the dependency graph that orders a review foundation-first.

Three readers ship. Each ranks itself for a given path, and the best claimant answers. A
file no reader claims contributes no symbols at all.

Project home: <https://github.com/gogoout/differential>

## Language coverage

| language | extensions | reader |
|---|---|---|
| Rust | `.rs` | tuned query |
| Python | `.py` `.pyi` | tuned query |
| Go | `.go` | tuned query |
| TypeScript | `.ts` `.mts` `.cts` | tuned query |
| TSX | `.tsx` | tuned query |
| Kotlin | `.kt` `.kts` | tuned query |
| JavaScript | `.js` `.jsx` `.mjs` `.cjs` | field rules |
| Java | `.java` | field rules |
| C | `.c` `.h` | field rules |
| C++ | `.cc` `.cpp` `.cxx` `.hpp` `.hh` | field rules |
| C# | `.cs` | field rules |
| Ruby, PHP, Swift, Scala, shell, Perl, Lua, Elixir, Erlang, Haskell, OCaml, Dart, Vue, Svelte, SQL, Protobuf, Zig | `.rb` `.php` `.swift` `.scala` `.sh` `.bash` `.zsh` `.pl` `.pm` `.lua` `.ex` `.exs` `.erl` `.hs` `.ml` `.mli` `.dart` `.vue` `.svelte` `.sql` `.proto` `.zig` | regex floor |
| everything else — manifests, lockfiles, prose, data | — | none |

## What each rung costs you

| reader | definitions | references | comments and strings |
|---|---|---|---|
| tuned query | from the tree, per language | calls and types, per language | dropped |
| field rules | from the tree | calls and types, by field name | dropped |
| regex floor | declaration keywords | every identifier of four characters or more | **counted** |
| none | — | — | — |

Three consequences worth stating outright, because each one surprises.

**A file no reader claims still exists.** Its hunks are counted, its classes are formed, it
is read like anything else. It simply draws no dependency edges. Withholding symbols is
classification, never enumeration — nothing here can add or remove a hunk (ADR 0005, 0012).

**Falling a rung costs precision, not coverage.** A missing edge misorders a reading plan.
It can never hide a change.

**What a tuned query buys is knowing what a definition is not.** `mod template;` names a
module, and `fn from` inside an `impl` is reached through its type — neither introduces a
name other files can use. The regex floor cannot tell either from a real definition, and on
one measured range six such words produced 64% of every dependency edge.

Comments and strings are dropped by both AST readers without any query, because every
grammar names those nodes with those words. A token that reaches a string through an
interpolation is still code, so `"${resolve(id)}"` keeps its call.

## How a language moves up

**To the field rules:** one entry in the table in `src/ast/generic.rs`, plus the grammar
crate. The reader probes each grammar when it is built and declines any whose shape its
rules cannot read, so a grammar that does not fit falls to the floor rather than answering
empty.

**To a tuned query:** the same, plus a `.scm` in `src/ast/queries` capturing `@def`,
`@call` and `@type`. Roughly ten patterns. Queries compile when the reader is built, so a
wrong node name fails immediately and names the node.

Kotlin is the worked example of why the top rung exists. Its `call_expression` carries no
`function:` field and its `navigation_expression` names none of its children, so the field
rules found no calls there at all. Nine of the other ten grammars passed those rules on a
probe before any of this was written.

Each query carries a version in the reader's fingerprint, which is part of the grouping
cache key. Editing a query therefore colds the cache by itself, and a test pins each
query's content hash against its version so the bump cannot be forgotten.

## What is proven against what

| reader | evidence |
|---|---|
| tuned query | Rust, Python and TypeScript run against a real multi-language corpus. Go and Kotlin are covered by per-language tests only. |
| field rules | Java runs against the corpus. C, C++, C# and JavaScript are covered by per-language tests only. |
| regex floor | runs against the corpus wherever no grammar claims a file. |

On that corpus the readers took two ranges from 288 and 131 dependency edges down to 63 and
26, and the second range's twelve-class cycle — which left every class behind it impossible
to order — disappeared entirely.

## Known edges

- `.pyi`, `.mts` and `.cts` are claimed by the tuned reader but are absent from the regex
  floor's list. Nothing fails today, so nothing falls through; if a parse ever did fail on
  one, it would get no symbols rather than crude ones.

## Using it

```rust
// The application wires the readers. Registration order does not matter —
// each reader ranks itself for a given path.
let symbols = differential_symbols::readers();

let out = run_pipeline(&repo, &src.base, &src.head, src.kind, &config,
                       &LanguageRegistry::builtin(), &symbols)?;
```

The engine owns the `SymbolSource` port and the rule for choosing between readers
(`engine::artefact::symbols`). This crate owns the readers and depends on the engine, never
the reverse. See
[`adr/0023-symbol-extraction-is-a-domain-port.md`](../../adr/0023-symbol-extraction-is-a-domain-port.md).

## Licence

MIT or Apache-2.0, at your option.
