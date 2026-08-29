//! The generic line normaliser — deliberately identical to the validated
//! prototype (same regexes, same order), so shape-class populations stay
//! byte-comparable with its recorded outputs. Do not "improve" this in place:
//! behaviour changes belong in a language plugin with its own id (ADR 0015).

use std::sync::LazyLock;

use regex::bytes::Regex;

use super::FileSymbols;

// (?-u): byte-level ASCII classes, matching Python bytes-pattern semantics.
static STR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?-u)"[^"]*"|'[^']*'|`[^`]*`"#).unwrap());
static NUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)\b\d+\b").unwrap());
static IDENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)[A-Za-z_][A-Za-z0-9_\-]{3,}").unwrap());
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)\s+").unwrap());

static DEF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?-u)\b(?:fn|struct|enum|trait|class|interface|type|def|func|impl|const|static|mod|module|package|protocol)\s+([A-Za-z_][A-Za-z0-9_]{2,})",
    )
    .unwrap()
});
static REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)[A-Za-z_][A-Za-z0-9_]{3,}").unwrap());

/// Symbol names introduced by common declaration keywords. Deliberately crude:
/// ordering tolerates low precision — a wrong edge misorders, it can never hide
/// content (ADR 0007/0015).
pub fn symbol_definitions(line: &[u8]) -> Vec<Vec<u8>> {
    DEF_RE.captures_iter(line).map(|c| c[1].to_vec()).collect()
}

/// Identifiers referenced in the line (superset of definitions; the ordering
/// stage intersects against other groups' definitions, so noise cancels out).
pub fn symbol_references(line: &[u8]) -> Vec<Vec<u8>> {
    REF_RE
        .find_iter(line)
        .map(|m| m.as_bytes().to_vec())
        .collect()
}

/// Every line's definitions and references, from the two heuristics above.
///
/// The validated prototype worked one line at a time, and this is that same
/// loop lifted to a file — so the generic tier stays byte-identical while the
/// trait gains the whole-file view a parser needs.
///
/// Split on `\n` only. A `\r` survives into the line, where the identifier
/// patterns cannot match it, exactly as it could not in a diff line.
pub fn file_symbols(content: &[u8]) -> FileSymbols {
    let lines: Vec<&[u8]> = content.split(|&b| b == b'\n').collect();
    FileSymbols {
        defines: lines.iter().map(|l| symbol_definitions(l)).collect(),
        references: lines.iter().map(|l| symbol_references(l)).collect(),
    }
}

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
