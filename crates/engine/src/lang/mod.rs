//! Language abstraction (ADR 0015).
//!
//! The tool must eventually support every language. This module is the seam:
//! a [`Language`] plugin can override how line content is normalised for shape
//! classification, and how a file's symbol definitions and references are
//! extracted for the ordering stage. Everything defaults to the generic
//! behaviour, so with no plugins registered the engine behaves exactly like the
//! validated milestone-1 normaliser (the parity test enforces this).
//!
//! Normalisation is per line; symbol extraction is per FILE. A line inside a
//! block comment or a multi-line string cannot be told from code on its own, so
//! that decision is made where the context to make it exists.
//!
//! Languages never see enumeration: which files and hunks exist is decided
//! before this module is consulted (ADR 0005/0012). They only influence
//! *classification*.

pub mod generic;

/// A language plugin. Every method has a working generic default, so a new
/// language implements only what it improves on.
pub trait Language: Send + Sync {
    /// Stable identifier, e.g. "generic", "rust". Part of the registry
    /// fingerprint, so bump-worthy behaviour changes need a new id or version.
    fn id(&self) -> &'static str;

    /// Whether this plugin claims the file (typically by extension/basename).
    /// The registry's generic fallback claims everything.
    fn claims(&self, path: &[u8]) -> bool;

    /// Normalise one line's content for shape classification: erase what varies
    /// between instances of the same edit (identifiers, literals, spacing),
    /// keep what distinguishes different edits.
    ///
    /// This feeds the shape hash ONLY — never `hunk_digest`, which is the
    /// exact-content persistence anchor.
    fn normalize_line(&self, line: &[u8]) -> Vec<u8> {
        generic::normalize_line(line)
    }

    /// Every symbol the file DEFINES and REFERENCES, per line. Feeds the
    /// ordering stage's definition → use edges; precision is allowed to be low
    /// (ADR 0007: a wrong edge misorders, it never hides content).
    ///
    /// **Whole file, not a line and not a hunk.** A line inside a block comment
    /// or a multi-line string is indistinguishable from code on its own, so the
    /// two cuts worth making — drop comment and string tokens, keep only call
    /// and type positions — are not decidable per line. The generic default is
    /// still a per-line loop, and stays byte-identical to what it replaced.
    fn file_symbols(&self, path: &[u8], content: &[u8]) -> FileSymbols {
        let _ = path;
        generic::file_symbols(content)
    }
}

/// A file's symbols, indexed by NEW-SIDE line number.
///
/// Both vectors are parallel and one entry per line, so an entry is addressed
/// by the line number a hunk already carries. Lines with no symbols hold an
/// empty `Vec`, which does not allocate — a 100k-line file costs pointers.
#[derive(Debug, Default, Clone)]
pub struct FileSymbols {
    pub defines: Vec<Vec<Vec<u8>>>,
    pub references: Vec<Vec<Vec<u8>>>,
}

impl FileSymbols {
    /// How many lines this covers. A caller comparing it against a hunk's
    /// new-side range is checking that the blob and the diff agree.
    pub fn lines(&self) -> usize {
        self.defines.len().min(self.references.len())
    }

    /// Symbols defined on `line`, counting from 1. Empty when out of range —
    /// the range check is `lines()`, so a caller that wants to know asks.
    pub fn defines_at(&self, line: u32) -> &[Vec<u8>] {
        at(&self.defines, line)
    }

    /// Symbols referenced on `line`, counting from 1.
    pub fn references_at(&self, line: u32) -> &[Vec<u8>] {
        at(&self.references, line)
    }
}

fn at(rows: &[Vec<Vec<u8>>], line: u32) -> &[Vec<u8>] {
    line.checked_sub(1)
        .and_then(|i| rows.get(i as usize))
        .map_or(&[], |v| v.as_slice())
}

/// The generic fallback: claims every file, uses the default normaliser.
pub struct Generic;

impl Language for Generic {
    fn id(&self) -> &'static str {
        "generic-v1"
    }
    fn claims(&self, _path: &[u8]) -> bool {
        true
    }
}

/// Ordered set of language plugins with a guaranteed generic fallback.
pub struct LanguageRegistry {
    langs: Vec<Box<dyn Language>>,
    fallback: Generic,
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl LanguageRegistry {
    /// The built-in registry: generic fallback only (for now).
    pub fn builtin() -> Self {
        LanguageRegistry {
            langs: Vec::new(),
            fallback: Generic,
        }
    }

    /// Register a plugin. First registered, first asked; the generic fallback
    /// always answers last.
    pub fn register(&mut self, lang: Box<dyn Language>) {
        self.langs.push(lang);
    }

    pub fn detect(&self, path: &[u8]) -> &dyn Language {
        for l in &self.langs {
            if l.claims(path) {
                return l.as_ref();
            }
        }
        &self.fallback
    }

    /// Cache-key component: shape hashes depend on normalisation, so anything
    /// pinned to a partition (e.g. the future grouping cache) must include this.
    /// Two registries with the same fingerprint classify identically.
    pub fn fingerprint(&self) -> String {
        let mut parts: Vec<&str> = self.langs.iter().map(|l| l.id()).collect();
        parts.push(self.fallback.id());
        parts.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeToml;
    impl Language for FakeToml {
        fn id(&self) -> &'static str {
            "fake-toml-v1"
        }
        fn claims(&self, path: &[u8]) -> bool {
            path.ends_with(b".toml")
        }
        fn normalize_line(&self, _line: &[u8]) -> Vec<u8> {
            b"T".to_vec()
        }
    }

    #[test]
    fn fallback_claims_everything() {
        let reg = LanguageRegistry::builtin();
        assert_eq!(reg.detect(b"whatever.xyz").id(), "generic-v1");
        assert_eq!(reg.fingerprint(), "generic-v1");
    }

    #[test]
    fn registered_language_wins_for_its_files_only() {
        let mut reg = LanguageRegistry::builtin();
        reg.register(Box::new(FakeToml));
        assert_eq!(reg.detect(b"Cargo.toml").id(), "fake-toml-v1");
        assert_eq!(reg.detect(b"src/main.rs").id(), "generic-v1");
        assert_eq!(reg.fingerprint(), "fake-toml-v1+generic-v1");
    }
}
