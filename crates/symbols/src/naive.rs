//! The crude reader: one regex pass per line, no parser.
//!
//! Moved verbatim from `engine::lang::generic`, where it was the `Language`
//! trait's default answer. A port must not ship an answer, so it is a named
//! reader now, ranked below anything that actually parses.
//!
//! It reads only files whose extension names a language. A `Cargo.toml`, a
//! `README.md` or a lockfile is claimed by nobody and contributes nothing: on
//! the validation corpus, 32% of every dependency edge came from classes made
//! entirely of such files, and every one of those edges was false.

use std::sync::LazyLock;

use differential_engine::artefact::symbols::{FileSymbols, SymbolSource};
use regex::bytes::Regex;

// (?-u): byte-level ASCII classes, matching the validated prototype.
static DEF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?-u)\b(?:fn|struct|enum|trait|class|interface|type|def|func|impl|const|static|mod|module|package|protocol)\s+([A-Za-z_][A-Za-z0-9_]{2,})",
    )
    .unwrap()
});
static REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)[A-Za-z_][A-Za-z0-9_]{3,}").unwrap());

/// Extensions this reader will attempt. A whitelist, never a blacklist: a file
/// type nobody thought about gets silence, which is the safe answer.
const CODE: &[&[u8]] = &[
    b".rs", b".py", b".go", b".ts", b".tsx", b".js", b".jsx", b".mjs", b".cjs", b".java", b".kt",
    b".kts", b".c", b".h", b".cc", b".cpp", b".cxx", b".hpp", b".hh", b".cs", b".rb", b".php",
    b".swift", b".scala", b".sh", b".bash", b".zsh", b".pl", b".pm", b".lua", b".ex", b".exs",
    b".erl", b".hs", b".ml", b".mli", b".dart", b".vue", b".svelte", b".sql", b".proto", b".zig",
];

pub struct NaiveSymbols;

impl SymbolSource for NaiveSymbols {
    /// The floor. Anything that parses outranks it.
    fn priority(&self, path: &[u8]) -> Option<u8> {
        CODE.iter().any(|e| path.ends_with(e)).then_some(1)
    }

    /// Split on `\n` only. A `\r` survives into the line, where the identifier
    /// patterns cannot match it — exactly as it could not in a diff line.
    ///
    /// Never fails: a regex has nothing to choke on.
    fn file_symbols(&self, _path: &[u8], content: &[u8]) -> Option<FileSymbols> {
        let lines: Vec<&[u8]> = content.split(|&b| b == b'\n').collect();
        Some(FileSymbols {
            defines: lines.iter().map(|l| definitions(l)).collect(),
            references: lines.iter().map(|l| references(l)).collect(),
        })
    }

    fn fingerprint(&self) -> String {
        "naive-v1".to_string()
    }
}

/// Symbol names introduced by common declaration keywords. Deliberately crude:
/// ordering tolerates low precision — a wrong edge misorders, it can never hide
/// content (ADR 0007).
fn definitions(line: &[u8]) -> Vec<Vec<u8>> {
    DEF_RE.captures_iter(line).map(|c| c[1].to_vec()).collect()
}

/// Identifiers used in the line. A superset of definitions; the graph
/// intersects against what other classes define, so most noise cancels out.
fn references(line: &[u8]) -> Vec<Vec<u8>> {
    REF_RE
        .find_iter(line)
        .map(|m| m.as_bytes().to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rows: &[Vec<Vec<u8>>], line: usize) -> Vec<String> {
        rows[line - 1]
            .iter()
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    }

    #[test]
    fn it_reads_source_and_declines_data_and_prose() {
        assert_eq!(NaiveSymbols.priority(b"src/lib.rs"), Some(1));
        assert_eq!(NaiveSymbols.priority(b"queries/get.sql"), Some(1));
        // The four the corpus indicted, plus a file with no extension at all.
        assert_eq!(NaiveSymbols.priority(b"Cargo.toml"), None);
        assert_eq!(NaiveSymbols.priority(b"README.md"), None);
        assert_eq!(NaiveSymbols.priority(b"pnpm-lock.yaml"), None);
        assert_eq!(NaiveSymbols.priority(b"package.json"), None);
        assert_eq!(NaiveSymbols.priority(b"CODEOWNERS"), None);
    }

    #[test]
    fn symbols_land_on_the_line_that_carries_them() {
        let s = NaiveSymbols
            .file_symbols(
                b"a.rs",
                b"// nothing\nstruct WidgetCore {}\nlet c = WidgetCore;\n",
            )
            .unwrap();
        assert_eq!(text(&s.defines, 2), ["WidgetCore"]);
        assert!(s.defines[2].is_empty(), "line 3 defines nothing");
        assert_eq!(text(&s.references, 3), ["WidgetCore"]);
    }
}
