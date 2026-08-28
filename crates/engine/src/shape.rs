//! Shape classes and hunk digests.
//!
//! A shape class groups hunks whose diff text is identical after normalising
//! away identifiers, string and numeric literals — on BOTH sides (ADR 0004:
//! hashing only added lines collapses every deletion-only hunk into one class,
//! turning "same shapes, skippable" into a lie).
//!
//! Line normalisation is pluggable per language (ADR 0015, `crate::lang`); the
//! framing here — sigil prefixes, sorting, disposition in the key, sha1/12-hex —
//! is language-independent and deliberately identical to the validated
//! prototype, so class populations stay comparable with its recorded outputs.

use std::cmp::Reverse;
use std::collections::HashMap;

use sha1::{Digest, Sha1};

use crate::lang::{Language, LanguageRegistry};
use crate::model::{DiffView, Hunk};

/// Normalise one side's lines: language normalisation, sigil prefixed, sorted.
pub fn norm_lines(lines: &[Vec<u8>], sigil: u8, lang: &dyn Language) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = lines
        .iter()
        .map(|l| {
            let norm = lang.normalize_line(l);
            let mut v = Vec::with_capacity(norm.len() + 1);
            v.push(sigil);
            v.extend_from_slice(&norm);
            v
        })
        .collect();
    out.sort_unstable();
    out
}

/// Shape key: normalised removed + added lines, the file disposition, and
/// whether the file is generated. Returns the 12-hex sha1 used as the class
/// key.
///
/// The disposition is in the key because a whole-file-add hunk and a
/// modification with identical text are different shapes.
///
/// **`generated` is in the key because a class is a unit of routing, not only a
/// unit of text.** A lockfile line and a source line can normalise to the same
/// shape, and without this they became one class — a *mixed* class, neither
/// generated nor not. The noise tier could then only ask "is every member
/// generated?", which such a class answers no, so it went to the model and its
/// lockfile hunks went wherever the model put the class. That was how a
/// generated hunk reached a focus group.
///
/// With the flag in the key, every class is wholly generated or wholly not.
/// `plan::class_is_generated` becomes exact rather than a membership test that
/// fails on the one case that matters.
///
/// The cost is stated plainly: `generated` is a *hint* (built-in list,
/// gitattributes, repo config), so a `[classify]` glob now moves a class
/// boundary rather than only a routing decision. That is config tuning
/// classification, which is what config is for (ADR 0012); it still cannot add
/// or remove a hunk. The gain is that the routing decision is exact.
pub fn shape_hash(
    hunk: &Hunk,
    disposition_letter: u8,
    generated: bool,
    lang: &dyn Language,
) -> String {
    let mut parts = norm_lines(&hunk.removed, b'-', lang);
    parts.extend(norm_lines(&hunk.added, b'+', lang));
    let mut hasher = Sha1::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update(b"\n");
        }
        hasher.update(p);
    }
    hasher.update(b"|");
    hasher.update([disposition_letter]);
    hasher.update(b"|");
    hasher.update([if generated { b'g' } else { b'.' }]);
    hex::encode(hasher.finalize())[..12].to_string()
}

/// Exact content digest — NOT normalised and NOT language-dependent. The stable
/// anchor that lets comments and review state survive regeneration (positional
/// ids do not).
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
    hex::encode(hasher.finalize())
}

/// True iff, after erasing identifiers and literals, removed and added lines
/// match: a structure-free substitution. Insertion-only and deletion-only hunks
/// are never pure. This is a property of the shape, so it is computed once per
/// class from the exemplar. Computed, never claimed — there is no setter.
pub fn pure_substitution(hunk: &Hunk, lang: &dyn Language) -> bool {
    if hunk.removed.is_empty() || hunk.added.is_empty() {
        return false;
    }
    // Sigil-free normalisation, so the two sides are comparable.
    norm_lines(&hunk.removed, b' ', lang) == norm_lines(&hunk.added, b' ', lang)
}

/// The mechanical partition: every hunk assigned to a shape class.
/// Classes are ordered by descending member count (ties broken by first
/// appearance, so the ordering is deterministic); `class_of[i]` is the class
/// index of canonical hunk `i`. Coverage is total by construction.
pub struct Partition {
    /// Hunk indices per class, in canonical order within each class.
    pub classes: Vec<Vec<usize>>,
    /// Canonical hunk index -> class index.
    pub class_of: Vec<usize>,
    /// Per class: is the exemplar a pure substitution?
    pub pure: Vec<bool>,
}

pub fn partition(view: &DiffView, langs: &LanguageRegistry) -> Partition {
    let mut by_hash: HashMap<String, Vec<usize>> = HashMap::new();
    let mut first_seen: HashMap<String, usize> = HashMap::new();
    for (i, h) in view.hunks.iter().enumerate() {
        let file = view.file_of(h);
        let lang = langs.detect(&file.path);
        let key = shape_hash(h, file.disposition.letter(), file.generated.is_some(), lang);
        first_seen.entry(key.clone()).or_insert(i);
        by_hash.entry(key).or_default().push(i);
    }

    let mut keys: Vec<&String> = by_hash.keys().collect();
    keys.sort_by_key(|k| (Reverse(by_hash[*k].len()), first_seen[*k]));

    let mut classes = Vec::with_capacity(keys.len());
    let mut class_of = vec![0usize; view.hunks.len()];
    let mut pure = Vec::with_capacity(keys.len());
    for (ci, k) in keys.iter().enumerate() {
        let members = by_hash[*k].clone();
        for &hi in &members {
            class_of[hi] = ci;
        }
        let exemplar = &view.hunks[members[0]];
        let lang = langs.detect(&view.file_of(exemplar).path);
        pure.push(pure_substitution(exemplar, lang));
        classes.push(members);
    }
    Partition {
        classes,
        class_of,
        pure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Generic;
    use crate::model::Hunk;

    const G: &Generic = &Generic;

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
        assert_eq!(
            shape_hash(&a, b'M', false, G),
            shape_hash(&b, b'M', false, G)
        );
    }

    #[test]
    fn literals_are_normalised() {
        let a = hunk(
            &[br#"    retry(5, "backoff")"#],
            &[br#"    retry(9, "linear")"#],
        );
        let b = hunk(&[br#"  retry(12, "other")"#], &[br#"  retry(3, "words")"#]);
        assert_eq!(
            shape_hash(&a, b'M', false, G),
            shape_hash(&b, b'M', false, G)
        );
    }

    #[test]
    fn deletion_only_hunks_with_different_content_differ() {
        // ADR 0004: both sides contribute; different deletions are not one shape.
        let a = hunk(&[b"fn compute_interest(rate: f64) -> f64 {"], &[]);
        let b = hunk(&[b"const RETRY_LIMIT: usize = 5;"], &[]);
        assert_ne!(
            shape_hash(&a, b'M', false, G),
            shape_hash(&b, b'M', false, G)
        );
    }

    #[test]
    fn disposition_is_part_of_the_key() {
        let a = hunk(&[], &[b"content line here"]);
        assert_ne!(
            shape_hash(&a, b'A', false, G),
            shape_hash(&a, b'M', false, G)
        );
    }

    #[test]
    fn a_generated_file_never_shares_a_class_with_a_source_file() {
        // The one case this component exists for: identical text, one side
        // generated. Sharing a class made it neither generated nor not, and the
        // noise tier could only route a class that was wholly one.
        let a = hunk(&[b"old = 1"], &[b"new = 2"]);
        assert_ne!(
            shape_hash(&a, b'M', true, G),
            shape_hash(&a, b'M', false, G)
        );
    }

    #[test]
    fn crlf_agnostic_normalisation() {
        let unix = hunk(&[b"old_value_name = 1"], &[b"new_value_name = 1"]);
        let dos = hunk(&[b"old_value_name = 1\r"], &[b"new_value_name = 1\r"]);
        assert_eq!(
            shape_hash(&unix, b'M', false, G),
            shape_hash(&dos, b'M', false, G)
        );
    }

    #[test]
    fn short_identifiers_survive_normalisation() {
        // The identifier regex needs length >= 4; `x` and `y` stay distinct.
        let a = hunk(&[b"x = 1"], &[b"y = 1"]);
        let b = hunk(&[b"y = 1"], &[b"x = 1"]);
        assert_ne!(
            shape_hash(&a, b'M', false, G),
            shape_hash(&b, b'M', false, G)
        );
    }

    #[test]
    fn pure_substitution_detects_rename() {
        let h = hunk(
            &[b"if !self.mail_service.is_enabled() {"],
            &[b"if !self.system_notifier.is_enabled() {"],
        );
        assert!(pure_substitution(&h, G));
    }

    #[test]
    fn structural_change_is_not_pure() {
        let h = hunk(
            &[b"send(user_address)"],
            &[b"send(user_address, RetryPolicy::default())"],
        );
        assert!(!pure_substitution(&h, G));
    }

    #[test]
    fn insertion_only_is_never_pure() {
        let h = hunk(&[], &[b"brand_new_line()"]);
        assert!(!pure_substitution(&h, G));
    }

    #[test]
    fn digest_is_exact_not_normalised() {
        let a = hunk(&[b"alpha_name = 1"], &[b"beta_name = 1"]);
        let b = hunk(&[b"gamma_name = 1"], &[b"delta_name = 1"]);
        // Same shape, different digests.
        assert_eq!(
            shape_hash(&a, b'M', false, G),
            shape_hash(&b, b'M', false, G)
        );
        assert_ne!(hunk_digest(&a), hunk_digest(&b));
    }

    #[test]
    fn digest_covers_nonl_flags() {
        let a = hunk(&[b"line"], &[b"line"]);
        let mut b = a.clone();
        b.nonl_new = true;
        assert_ne!(hunk_digest(&a), hunk_digest(&b));
    }

    #[test]
    fn language_override_changes_classification_but_not_digest() {
        use crate::lang::Language;
        struct Flattener;
        impl Language for Flattener {
            fn id(&self) -> &'static str {
                "flatten-v1"
            }
            fn claims(&self, _p: &[u8]) -> bool {
                true
            }
            fn normalize_line(&self, _l: &[u8]) -> Vec<u8> {
                b"X".to_vec()
            }
        }
        let a = hunk(&[b"completely unlike"], &[b"anything else at all"]);
        let b = hunk(&[b"nothing shared here"], &[b"with the other hunk"]);
        // Generic: different shapes. Flattener: same shape. Digests: unmoved.
        assert_ne!(
            shape_hash(&a, b'M', false, G),
            shape_hash(&b, b'M', false, G)
        );
        assert_eq!(
            shape_hash(&a, b'M', false, &Flattener),
            shape_hash(&b, b'M', false, &Flattener)
        );
        assert_ne!(hunk_digest(&a), hunk_digest(&b));
    }
}
