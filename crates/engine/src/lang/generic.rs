//! The generic line normaliser — deliberately identical to the validated
//! prototype (same regexes, same order), so shape-class populations stay
//! byte-comparable with its recorded outputs. Do not "improve" this in place:
//! behaviour changes belong in a language plugin with its own id (ADR 0015).
//!
//! Normalisation ONLY. Symbol extraction used to live here too; it is a
//! separate use case with its own port (`artefact::symbols`) and its readers
//! live in an adapter crate. Sharing a module made a symbol change look like a
//! normalisation change, which is the one thing this file must never allow.

use std::sync::LazyLock;

use regex::bytes::Regex;

// (?-u): byte-level ASCII classes, matching Python bytes-pattern semantics.
static STR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?-u)"[^"]*"|'[^']*'|`[^`]*`"#).unwrap());
static NUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)\b\d+\b").unwrap());
static IDENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)[A-Za-z_][A-Za-z0-9_\-]{3,}").unwrap());
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)\s+").unwrap());

/// Strings → `"S"`, numbers → `N`, identifiers (length ≥ 4) → `I`, whitespace
/// collapsed to single spaces, trimmed.
pub fn normalize_line(line: &[u8]) -> Vec<u8> {
    let s = STR_RE.replace_all(line, b"\"S\"".as_slice());
    let s = NUM_RE.replace_all(&s, b"N".as_slice());
    let s = IDENT_RE.replace_all(&s, b"I".as_slice());
    let s = WS_RE.replace_all(&s, b" ".as_slice());
    trim_ascii(&s).to_vec()
}

fn trim_ascii(s: &[u8]) -> &[u8] {
    let start = s
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |e| e + 1);
    &s[start..end]
}
