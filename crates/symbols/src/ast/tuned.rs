//! The reader for languages with a hand-written query.
//!
//! One `.scm` per language, capturing three things and nothing else:
//!
//! | capture | means |
//! |---|---|
//! | `@def` | a name this file introduces that others can use |
//! | `@call` | a function being called |
//! | `@type` | a type being used |
//! | `@ref` | a name reached by path, without being called |
//! | `@local_def` | a name that reaches only this file |
//! | `@local_ref` | an identifier that might be reading one |
//!
//! The last two are why a `const` inside a function is not thrown away
//! (ADR 0030). They are compared only against the same file's answers, so a
//! common word cannot become a symbol the whole change links to — which is the
//! failure the file-scope rule exists to prevent.
//!
//! Written here rather than vendored from nvim-treesitter: those files use
//! predicates the Rust query engine does not support (`#lua-match?`,
//! `#has-ancestor?`) and are pinned to grammar versions we do not control.
//!
//! **A wrong node name fails when the query compiles**, and the error names the
//! node. That loud failure is the reason for a query file over a hand-written
//! tree walk, which would return zero and say nothing.

use std::collections::HashSet;
use std::ops::Range;

use differential_engine::artefact::symbols::{FileSymbols, Symbol, SymbolSource};
use tree_sitter::{Language, Query, QueryCursor, StreamingIterator};

use super::{is_prose, line_count, line_of, parse, prose_tokens, text_of};

struct Tuned {
    /// Bump the `-vN` when the query changes. It reaches the grouping cache key,
    /// so a stale grouping would otherwise be served for a graph that moved.
    version: &'static str,
    extensions: &'static [&'static [u8]],
    language: fn() -> Language,
    /// Query text, joined in order. TSX is TypeScript's query plus the JSX
    /// patterns, and the plain TypeScript grammar has no nodes for those — a
    /// single shared file would fail to compile against one of the two.
    sources: &'static [&'static str],
}

impl Tuned {
    fn query_text(&self) -> String {
        self.sources.join("\n")
    }
}

static TUNED: &[Tuned] = &[
    Tuned {
        version: "rust-v3",
        extensions: &[b".rs"],
        language: rust,
        sources: &[include_str!("queries/rust.scm")],
    },
    Tuned {
        version: "python-v3",
        extensions: &[b".py", b".pyi"],
        language: python,
        sources: &[include_str!("queries/python.scm")],
    },
    Tuned {
        version: "go-v3",
        extensions: &[b".go"],
        language: go,
        sources: &[include_str!("queries/go.scm")],
    },
    Tuned {
        version: "typescript-v3",
        extensions: &[b".ts", b".mts", b".cts"],
        language: typescript,
        sources: &[include_str!("queries/typescript.scm")],
    },
    Tuned {
        version: "tsx-v3",
        extensions: &[b".tsx"],
        language: tsx,
        sources: &[
            include_str!("queries/typescript.scm"),
            include_str!("queries/tsx.scm"),
        ],
    },
    Tuned {
        version: "kotlin-v3",
        extensions: &[b".kt", b".kts"],
        language: kotlin,
        sources: &[include_str!("queries/kotlin.scm")],
    },
];

fn rust() -> Language {
    tree_sitter_rust::LANGUAGE.into()
}
fn python() -> Language {
    tree_sitter_python::LANGUAGE.into()
}
fn go() -> Language {
    tree_sitter_go::LANGUAGE.into()
}
fn typescript() -> Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}
fn tsx() -> Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}
fn kotlin() -> Language {
    tree_sitter_kotlin_ng::LANGUAGE.into()
}

pub struct AstSymbols {
    ready: Vec<(&'static Tuned, Language, Query)>,
    /// Languages whose query would not compile, and why.
    ///
    /// **A test asserts this is empty.** A query and its grammar are both ours
    /// and both pinned, so a mismatch is a bug to fix, never a state to ship.
    failures: Vec<(&'static str, String)>,
}

impl Default for AstSymbols {
    fn default() -> Self {
        Self::new()
    }
}

impl AstSymbols {
    pub fn new() -> Self {
        let mut ready = Vec::new();
        let mut failures = Vec::new();
        for tuned in TUNED {
            let language = (tuned.language)();
            match Query::new(&language, &tuned.query_text()) {
                Ok(query) => ready.push((tuned, language, query)),
                Err(e) => failures.push((tuned.version, e.to_string())),
            }
        }
        AstSymbols { ready, failures }
    }

    /// Every query that would not compile against its pinned grammar.
    pub fn failures(&self) -> &[(&'static str, String)] {
        &self.failures
    }

    /// Every query, as `(version, text)`.
    ///
    /// Exists for the pin test: the version reaches the grouping cache key, so
    /// editing a query without bumping it serves a stale grouping for a graph
    /// that moved. The test hashes the patterns against the version, which is
    /// the only way the bump cannot be forgotten.
    pub fn queries() -> Vec<(&'static str, String)> {
        TUNED.iter().map(|t| (t.version, t.query_text())).collect()
    }

    fn entry(&self, path: &[u8]) -> Option<&(&'static Tuned, Language, Query)> {
        self.ready
            .iter()
            .find(|(t, _, _)| t.extensions.iter().any(|e| path.ends_with(e)))
    }
}

impl SymbolSource for AstSymbols {
    /// The top rung: a query knows this language's shape, so nothing outranks it.
    fn priority(&self, path: &[u8]) -> Option<u8> {
        self.entry(path).map(|_| 9)
    }

    fn file_symbols(&self, path: &[u8], content: &[u8]) -> Option<FileSymbols> {
        let (_, language, query) = self.entry(path)?;
        let tree = parse(language, content)?;
        let lines = line_count(content);
        let mut out = FileSymbols {
            namespace: crate::namespace::of(path),
            defines: vec![Vec::new(); lines],
            references: vec![Vec::new(); lines],
        };

        // Collect first, decide after. A definition site is also a type
        // mention — `struct Widget` matches both `@def` and `@type` — and query
        // matches arrive in no particular order, so the veto needs every
        // capture in hand.
        let prose = prose_tokens(&tree);
        let names = query.capture_names();
        let mut captured: Vec<(&str, usize, Range<usize>, Vec<u8>)> = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), content);
        while let Some(m) = matches.next() {
            for capture in m.captures() {
                let node = capture.node;
                // A token answers from the set; anything else — no query here
                // captures one — pays the ancestor walk.
                let prosaic = if node.child_count() == 0 {
                    prose.contains(&node.byte_range())
                } else {
                    is_prose(node)
                };
                if prosaic {
                    continue;
                }
                let (Some(text), Some(line)) =
                    (text_of(node, content), line_of(node).checked_sub(1))
                else {
                    continue;
                };
                if line >= lines {
                    continue;
                }
                captured.push((
                    names[capture.index as usize],
                    line,
                    node.byte_range(),
                    text.to_vec(),
                ));
            }
        }

        let ranges = |wanted: &str| -> HashSet<Range<usize>> {
            captured
                .iter()
                .filter(|(name, ..)| *name == wanted)
                .map(|(_, _, range, _)| range.clone())
                .collect()
        };
        let defined = ranges("def");
        let locally_defined = ranges("local_def");

        for (name, line, range, text) in captured {
            // Definitions win: a class must never appear to consume the thing
            // it introduces. A file-scope definition also wins over the
            // file-local capture of the same token, which is how
            // `(variable_declarator …) @local_def` and its `(program …) @def`
            // sibling both stay in the query without fighting.
            let is_a_definition = defined.contains(&range) || locally_defined.contains(&range);
            match name {
                "def" => out.defines[line].push(Symbol::global(text)),
                "local_def" if !defined.contains(&range) => {
                    out.defines[line].push(Symbol::local(text));
                }
                // Three spellings of one thing: the file consumes a name
                // that came from somewhere else. `@ref` is the one that is
                // neither a call nor a type — a function handed to a router
                // rather than invoked, an enum variant, a constant by path.
                "call" | "type" | "ref" if !is_a_definition => {
                    out.references[line].push(Symbol::global(text));
                }
                // A call may be resolving a file-local binding or a global
                // one, and nothing here can say which. Both are recorded; the
                // graph keeps whichever finds a definer.
                "local_ref" if !is_a_definition => out.references[line].push(Symbol::local(text)),
                _ => {}
            }
        }
        Some(out)
    }

    fn fingerprint(&self) -> String {
        let mut parts: Vec<&str> = self.ready.iter().map(|(t, _, _)| t.version).collect();
        parts.sort_unstable();
        format!("ast-tuned[{}]", parts.join(","))
    }
}
