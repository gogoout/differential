//! Language abstraction (ADR 0015).
//!
//! The tool must eventually support every language. This module is the seam:
//! a [`Language`] plugin can override how line content is normalised for shape
//! classification, and — in a later milestone — how symbol definitions and
//! references are extracted for the ordering stage. Everything defaults to the
//! generic behaviour, so with no plugins registered the engine behaves exactly
//! like the validated milestone-1 normaliser (the parity test enforces this).
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

    /// Symbol names this line DEFINES (declaration heuristics). Feeds the
    /// ordering stage's definition → use edges; precision is allowed to be low
    /// (ADR 0007: a wrong edge misorders, it never hides content).
    fn symbol_definitions(&self, line: &[u8]) -> Vec<Vec<u8>> {
        generic::symbol_definitions(line)
    }

    /// Identifiers this line REFERENCES.
    fn symbol_references(&self, line: &[u8]) -> Vec<Vec<u8>> {
        generic::symbol_references(line)
    }
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
