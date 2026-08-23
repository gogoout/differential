//! Shape classes and hunk digests.
//!
//! A shape class groups hunks whose diff text is identical after normalising
//! away identifiers, string and numeric literals — on BOTH sides (ADR 0004:
//! hashing only added lines collapses every deletion-only hunk into one class,
//! turning "same shapes, skippable" into a lie).
//!
//! The normalisation and hash are deliberately identical to the validated
//! prototype (sha1, 12 hex chars, same regexes, same ordering), so class
//! populations can be compared against its recorded outputs when validating the
//! port.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::bytes::Regex;
use sha1::{Digest, Sha1};

use crate::model::{DiffView, Hunk};

// (?-u): byte-level ASCII classes, matching Python bytes-pattern semantics.
static STR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?-u)"[^"]*"|'[^']*'|`[^`]*`"#).unwrap());
static NUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)\b\d+\b").unwrap());
static IDENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)[A-Za-z_][A-Za-z0-9_\-]{3,}").unwrap());
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)\s+").unwrap());

/// Normalise one side's lines: literals → placeholders, whitespace collapsed,
/// sigil prefixed, sorted.
pub fn norm_lines(lines: &[Vec<u8>], sigil: u8) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = lines
        .iter()
        .map(|l| {
            let s = STR_RE.replace_all(l, b"\"S\"".as_slice());
            let s = NUM_RE.replace_all(&s, b"N".as_slice());
            let s = IDENT_RE.replace_all(&s, b"I".as_slice());
            let s = WS_RE.replace_all(&s, b" ".as_slice());
            let trimmed = trim_ascii(&s);
            let mut v = Vec::with_capacity(trimmed.len() + 1);
            v.push(sigil);
            v.extend_from_slice(trimmed);
            v
        })
        .collect();
    out.sort_unstable();
    out
}

/// Shape key: normalised removed + added lines, plus the file disposition — a
/// whole-file-add hunk and a modification with identical text are different
/// shapes. Returns the 12-hex sha1 used as the class key.
pub fn shape_hash(hunk: &Hunk, disposition_letter: u8) -> String {
    let mut parts = norm_lines(&hunk.removed, b'-');
    parts.extend(norm_lines(&hunk.added, b'+'));
    let mut hasher = Sha1::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update(b"\n");
        }
        hasher.update(p);
    }
    hasher.update(b"|");
    hasher.update([disposition_letter]);
    let hex = hex_string(&hasher.finalize());
    hex[..12].to_string()
}

/// Exact content digest — NOT normalised. The stable anchor that lets comments
/// and review state survive regeneration (positional ids do not).
pub fn hunk_digest(hunk: &Hunk) -> String {
    let mut hasher = Sha1::new();
    for l in &hunk.removed {
        hasher.update(b"-");
        hasher.update(l);
        hasher.update(b"\n");
    }
    for l in &hunk.added {
        hasher.update(b"+");
        hasher.update(l);
        hasher.update(b"\n");
    }
    hasher.update([hunk.nonl_old as u8, hunk.nonl_new as u8]);
    hex_string(&hasher.finalize())
}

/// True iff, after erasing identifiers and literals, removed and added lines
/// match: a structure-free substitution. Insertion-only and deletion-only hunks
/// are never pure. This is a property of the shape, so it is computed once per
/// class from the exemplar. Computed, never claimed — there is no setter.
pub fn pure_substitution(hunk: &Hunk) -> bool {
    if hunk.removed.is_empty() || hunk.added.is_empty() {
        return false;
    }
    // Same normalisation, sigil-free, so the two sides are comparable.
    norm_lines(&hunk.removed, b' ') == norm_lines(&hunk.added, b' ')
}

/// The mechanical partition: every hunk assigned to a shape class.
/// Returns classes as (members) lists ordered by descending size (ties broken by
/// first appearance, so the ordering is deterministic); `class_of[i]` is the
/// class index of canonical hunk `i`. Coverage is total by construction.
pub struct Partition {
    /// Hunk indices per class, in canonical order within each class.
    pub classes: Vec<Vec<usize>>,
    /// Canonical hunk index -> class index.
    pub class_of: Vec<usize>,
}

pub fn partition(view: &DiffView) -> Partition {
    let mut by_hash: HashMap<String, Vec<usize>> = HashMap::new();
    let mut first_seen: HashMap<String, usize> = HashMap::new();
    for (i, h) in view.hunks.iter().enumerate() {
        let letter = view.file_of(h).disposition.letter();
        let key = shape_hash(h, letter);
        first_seen.entry(key.clone()).or_insert(i);
        by_hash.entry(key).or_default().push(i);
    }

    let mut keys: Vec<&String> = by_hash.keys().collect();
    keys.sort_by_key(|k| (usize::MAX - by_hash[*k].len(), first_seen[*k]));

    let mut classes = Vec::with_capacity(keys.len());
    let mut class_of = vec![0usize; view.hunks.len()];
    for (ci, k) in keys.iter().enumerate() {
        let members = by_hash[*k].clone();
        for &hi in &members {
            class_of[hi] = ci;
        }
        classes.push(members);
    }
    Partition { classes, class_of }
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

fn hex_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Hunk;

    fn hunk(removed: &[&[u8]], added: &[&[u8]]) -> Hunk {
        Hunk {
            file: 0,
            old_start: 1,
            old_count: removed.len() as u32,
            new_start: 1,
            new_count: added.len() as u32,
            removed: removed.iter().map(|l| l.to_vec()).collect(),
            added: added.iter().map(|l| l.to_vec()).collect(),
            nonl_old: false,
            nonl_new: false,
        }
    }

    #[test]
    fn identifier_renames_share_a_shape() {
        let a = hunk(
            &[b"    let total_count = compute_total(items);"],
            &[b"    let total_count = compute_sum(items);"],
        );
        let b = hunk(
            &[b"  let grand_total = derive_total(rows);"],
            &[b"  let grand_total = derive_result(rows);"],
        );
        assert_eq!(shape_hash(&a, b'M'), shape_hash(&b, b'M'));
    }

    #[test]
    fn literals_are_normalised() {
        let a = hunk(
            &[br#"    retry(5, "backoff")"#],
            &[br#"    retry(9, "linear")"#],
        );
        let b = hunk(&[br#"  retry(12, "other")"#], &[br#"  retry(3, "words")"#]);
        assert_eq!(shape_hash(&a, b'M'), shape_hash(&b, b'M'));
    }

    #[test]
    fn deletion_only_hunks_with_different_content_differ() {
        // ADR 0004: both sides contribute; different deletions are not one shape.
        let a = hunk(&[b"fn compute_interest(rate: f64) -> f64 {"], &[]);
        let b = hunk(&[b"const RETRY_LIMIT: usize = 5;"], &[]);
        assert_ne!(shape_hash(&a, b'M'), shape_hash(&b, b'M'));
    }

    #[test]
    fn disposition_is_part_of_the_key() {
        let a = hunk(&[], &[b"content line here"]);
        assert_ne!(shape_hash(&a, b'A'), shape_hash(&a, b'M'));
    }

    #[test]
    fn crlf_agnostic_normalisation() {
        let unix = hunk(&[b"old_value_name = 1"], &[b"new_value_name = 1"]);
        let dos = hunk(&[b"old_value_name = 1\r"], &[b"new_value_name = 1\r"]);
        assert_eq!(shape_hash(&unix, b'M'), shape_hash(&dos, b'M'));
    }

    #[test]
    fn short_identifiers_survive_normalisation() {
        // The identifier regex needs length >= 4; `x` and `y` stay distinct.
        let a = hunk(&[b"x = 1"], &[b"y = 1"]);
        let b = hunk(&[b"y = 1"], &[b"x = 1"]);
        assert_ne!(shape_hash(&a, b'M'), shape_hash(&b, b'M'));
    }

    #[test]
    fn pure_substitution_detects_rename() {
        let h = hunk(
            &[b"if !self.mail_service.is_enabled() {"],
            &[b"if !self.system_notifier.is_enabled() {"],
        );
        assert!(pure_substitution(&h));
    }

    #[test]
    fn structural_change_is_not_pure() {
        let h = hunk(
            &[b"send(user_address)"],
            &[b"send(user_address, RetryPolicy::default())"],
        );
        assert!(!pure_substitution(&h));
    }

    #[test]
    fn insertion_only_is_never_pure() {
        let h = hunk(&[], &[b"brand_new_line()"]);
        assert!(!pure_substitution(&h));
    }

    #[test]
    fn digest_is_exact_not_normalised() {
        let a = hunk(&[b"alpha_name = 1"], &[b"beta_name = 1"]);
        let b = hunk(&[b"gamma_name = 1"], &[b"delta_name = 1"]);
        // Same shape, different digests.
        assert_eq!(shape_hash(&a, b'M'), shape_hash(&b, b'M'));
        assert_ne!(hunk_digest(&a), hunk_digest(&b));
    }

    #[test]
    fn digest_covers_nonl_flags() {
        let a = hunk(&[b"line"], &[b"line"]);
        let mut b = a.clone();
        b.nonl_new = true;
        assert_ne!(hunk_digest(&a), hunk_digest(&b));
    }
}
