//! The reader for languages with a hand-written query.
//!
//! One `.scm` per language, capturing three things and nothing else:
//!
//! | capture | means |
//! |---|---|
//! | `@def` | a name this file introduces that others can use |
//! | `@call` | a function being called |
//! | `@type` | a type being used |
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

use differential_engine::artefact::symbols::{FileSymbols, SymbolSource};
use tree_sitter::{Language, Query, QueryCursor, StreamingIterator};

use super::{is_prose, line_count, line_of, parse, text_of};

struct Tuned {
    /// Bump the `-vN` when the query changes. It reaches the grouping cache key,
    /// so a stale grouping would otherwise be served for a graph that moved.
    version: &'static str,
    extensions: &'static [&'static [u8]],
    language: fn() -> Language,
    source: &'static str,
}

static TUNED: &[Tuned] = &[
    Tuned {
        version: "rust-v1",
        extensions: &[b".rs"],
        language: rust,
        source: include_str!("queries/rust.scm"),
    },
    Tuned {
        version: "python-v1",
        extensions: &[b".py", b".pyi"],
        language: python,
        source: include_str!("queries/python.scm"),
    },
    Tuned {
        version: "go-v1",
        extensions: &[b".go"],
        language: go,
        source: include_str!("queries/go.scm"),
    },
    Tuned {
        version: "typescript-v1",
        extensions: &[b".ts", b".mts", b".cts"],
        language: typescript,
        source: include_str!("queries/typescript.scm"),
    },
    Tuned {
        version: "tsx-v1",
        extensions: &[b".tsx"],
        language: tsx,
        source: include_str!("queries/typescript.scm"),
    },
    Tuned {
        version: "kotlin-v1",
        extensions: &[b".kt", b".kts"],
        language: kotlin,
        source: include_str!("queries/kotlin.scm"),
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
            match Query::new(&language, tuned.source) {
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
            defines: vec![Vec::new(); lines],
            references: vec![Vec::new(); lines],
        };

        // Collect first, decide after. A definition site is also a type
        // mention — `struct Widget` matches both `@def` and `@type` — and query
        // matches arrive in no particular order, so the veto needs every
        // capture in hand.
        let names = query.capture_names();
        let mut captured: Vec<(&str, usize, Range<usize>, Vec<u8>)> = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), content);
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node;
                if is_prose(node) {
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

        let defined: HashSet<Range<usize>> = captured
            .iter()
            .filter(|(name, ..)| *name == "def")
            .map(|(_, _, range, _)| range.clone())
            .collect();

        for (name, line, range, text) in captured {
            match name {
                "def" => out.defines[line].push(text),
                // Definitions win: a class must never appear to consume the
                // thing it introduces.
                "call" | "type" if !defined.contains(&range) => out.references[line].push(text),
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
