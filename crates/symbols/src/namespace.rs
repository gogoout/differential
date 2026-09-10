//! Which names are allowed to match which.
//!
//! A global symbol used to be a bare name, so `FormData` read from a Rust file
//! and `FormData` read from a TypeScript one were one symbol. In a monorepo
//! that is a coincidence far more often than a fact: nothing here parses the
//! build graph, so the tool cannot know whether a Rust crate and a TypeScript
//! package are connected at all, and a shared word is only ever a guess that
//! they are (ADR 0031).
//!
//! So a reader hands the graph a namespace along with the names, and two global
//! symbols match only when their namespaces do. The graph treats the token as
//! opaque — it still cannot tell which reader answered, only whether two
//! answers are comparable.
//!
//! **All three readers use this one table**, and they must: the crude reader is
//! the fallback for a file the tuned reader claimed and failed to parse, so a
//! per-reader answer would split one language across two namespaces the first
//! time a parse failed.
//!
//! Languages share a namespace where they genuinely share names. TypeScript,
//! TSX, JavaScript and the single-file component formats are one `js`; Java,
//! Kotlin and Scala are one `jvm`; C and C++ are one `c`.

/// The namespace for `path`, by extension.
///
/// Falls back to the extension itself, so an unclaimed file cannot land in a
/// shared bucket with an unrelated one. No reader claims such a file today, so
/// the fallback is a guard rather than a path anyone takes.
pub fn of(path: &[u8]) -> Vec<u8> {
    const TABLE: &[(&str, &[&str])] = &[
        ("rust", &[".rs"]),
        ("python", &[".py", ".pyi"]),
        ("go", &[".go"]),
        (
            "js",
            &[
                ".ts", ".mts", ".cts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".vue", ".svelte",
            ],
        ),
        ("jvm", &[".kt", ".kts", ".java", ".scala"]),
        ("c", &[".c", ".h", ".cc", ".cpp", ".cxx", ".hpp", ".hh"]),
        ("csharp", &[".cs"]),
        ("ruby", &[".rb"]),
        ("php", &[".php"]),
        ("swift", &[".swift"]),
        ("shell", &[".sh", ".bash", ".zsh"]),
        ("perl", &[".pl", ".pm"]),
        ("lua", &[".lua"]),
        ("elixir", &[".ex", ".exs"]),
        ("erlang", &[".erl"]),
        ("haskell", &[".hs"]),
        ("ocaml", &[".ml", ".mli"]),
        ("dart", &[".dart"]),
        ("sql", &[".sql"]),
        ("proto", &[".proto"]),
        ("zig", &[".zig"]),
    ];
    for (namespace, extensions) in TABLE {
        if extensions.iter().any(|e| path.ends_with(e.as_bytes())) {
            return namespace.as_bytes().to_vec();
        }
    }
    match path.iter().rposition(|&b| b == b'.') {
        Some(dot) => path[dot..].to_vec(),
        None => path.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ns(path: &str) -> String {
        String::from_utf8(of(path.as_bytes())).unwrap()
    }

    #[test]
    fn one_language_is_one_namespace_however_it_is_spelled() {
        // The case that motivated this: two `FormData`s, one per language.
        assert_ne!(ns("src/model.rs"), ns("tests/api.test.ts"));
        // And the case it must not break: a language reached by more than one
        // extension, or by more than one reader, stays one namespace. The
        // crude reader is the tuned reader's fallback, so a split here would
        // appear the first time a parse failed.
        assert_eq!(ns("a.ts"), ns("b.tsx"));
        assert_eq!(ns("a.ts"), ns("c.js"));
        assert_eq!(ns("A.kt"), ns("B.java"));
        assert_eq!(ns("a.c"), ns("b.hpp"));
        assert_eq!(ns("a.rs"), "rust");
    }

    #[test]
    fn an_unclaimed_file_gets_its_own_extension_not_a_shared_bucket() {
        assert_eq!(ns("Cargo.lock"), ".lock");
        assert_eq!(ns("notes.md"), ".md");
        assert_ne!(ns("notes.md"), ns("Cargo.lock"));
        assert_eq!(ns("CODEOWNERS"), "CODEOWNERS");
    }
}
