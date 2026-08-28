//! The grouping cache KEY (ADR 0009): labels are non-deterministic across model calls,
//! coverage is not — so a grouping is pinned by a content hash of everything
//! that determines it. The cached value is the RAW model response; audit and
//! assembly are pure functions replayed on load, so their fixes apply to cached
//! runs too (prompt/payload changes must bump `PROMPT_VERSION` instead).
//!
//! Reading and writing entries is `store::FsGroupingCache`; this file is only
//! the key, whose composition pins every existing entry in every checkout and
//! must not change by one byte.

use sha1::{Digest, Sha1};

use super::{ClassInfo, payload::PROMPT_VERSION};

/// Key over: prompt generation, backend identity, normaliser fingerprint, and
/// the exact class structure (sorted member hunk digests — content-exact, so
/// the key survives positional-id shifts across regenerations).
///
/// **Backend IDENTITY, never its display name** (`LlmBackend::identity`). The
/// name is the command as it will run, absolute paths and all; where a binary
/// lives does not determine a grouping, and hashing it made a debug build, a
/// release build and two checkouts of one commit key differently for the same
/// class partition.
pub fn cache_key(offered: &[&ClassInfo], backend_identity: &str, lang_fingerprint: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(PROMPT_VERSION.to_le_bytes());
    hasher.update([0]);
    hasher.update(backend_identity.as_bytes());
    hasher.update([0]);
    hasher.update(lang_fingerprint.as_bytes());
    let mut classes: Vec<&&ClassInfo> = offered.iter().collect();
    classes.sort_by(|a, b| a.id.cmp(&b.id));
    for c in classes {
        hasher.update([0]);
        hasher.update(c.id.as_bytes());
        for d in &c.digests {
            hasher.update([1]);
            hasher.update(d.as_bytes());
        }
    }
    hex::encode(hasher.finalize())
}
