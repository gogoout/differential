//! The review-state sidecar store (ADR 0013, spec/persistence.md).
//!
//! Regeneration is total; state is a sidecar. Plan documents are immutable and
//! content-addressed; reviewed marks key on class CONTENT (sorted member
//! digests), and findings anchor on exact hunk digests — so both survive the
//! head moving and positional ids shifting. Re-anchoring never drops anything:
//! exact digest match → reattach; same-file content match → reattach flagged
//! moved; otherwise the finding is orphaned and listed.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::schema;

use crate::model::DiffView;

// Review identity and class keys are domain policy and live in `plan`; they
// are re-exported here because this is where consumers of the store expect to
// find them, and moving the names would break them for no gain.
pub use crate::plan::{class_content_key, review_id};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewState {
    /// Class content keys marked reviewed.
    #[serde(default)]
    pub reviewed_classes: BTreeSet<String>,
    /// Resume position: (group id or file path, row offset) in the last-open
    /// plan — a group id in the semantic view, a file path in the file view.
    #[serde(default)]
    pub cursor: Option<(String, usize)>,
    /// Side-by-side diff layout (default: unified).
    #[serde(default)]
    pub split_diff: bool,
    /// Flattened per-file view instead of semantic groups (default: groups).
    #[serde(default)]
    pub file_view: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingStatus {
    Open,
    Resolved,
    Orphaned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    pub file: String,
    /// "old" | "new"
    pub side: String,
    pub line: u32,
    pub hunk_digest: String,
    /// The anchored line's text — the fuzzy re-anchor key.
    #[serde(default)]
    pub line_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    /// Unix seconds.
    pub created: u64,
    pub body: String,
    pub status: FindingStatus,
    /// Reattached by content match rather than exact digest.
    #[serde(default)]
    pub moved: bool,
    pub plan_hash: String,
    pub anchor: Anchor,
}

impl Finding {
    pub fn new(created: u64, body: String, plan_hash: String, anchor: Anchor) -> Self {
        let mut h = Sha1::new();
        h.update(anchor.hunk_digest.as_bytes());
        h.update(created.to_le_bytes());
        h.update(body.as_bytes());
        Finding {
            id: hex::encode(h.finalize())[..12].to_string(),
            created,
            body,
            status: FindingStatus::Open,
            moved: false,
            plan_hash,
            anchor,
        }
    }
}

/// Wall-clock seconds. The one reader is `ReviewSession::add_finding`, which
/// passes the value into `Finding::new` — so the constructor stays a pure
/// function of its arguments and the ids it produces are reproducible.
///
/// Not a `Clock` port: one reader, `SystemTime` is std rather than a project
/// adapter, and nothing tests timestamps. That fails the bar for a new
/// abstraction, and would cost `ReviewSession` a second type parameter.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Re-anchor findings onto a (possibly regenerated) plan. Never drops:
/// exact digest → reattach (position refreshed); same-file content match →
/// reattach flagged `moved`; otherwise `orphaned` (revived automatically if a
/// later plan matches again).
pub fn reanchor(
    findings: &mut [Finding],
    doc: &schema::PlanDocument,
    view: &DiffView,
    plan_hash: &str,
) {
    for f in findings.iter_mut() {
        if f.plan_hash == plan_hash {
            continue; // written against this exact plan
        }
        // 1. Exact digest.
        if let Some((idx, h)) = doc
            .hunks
            .iter()
            .enumerate()
            .find(|(_, h)| h.digest == f.anchor.hunk_digest)
        {
            let _ = idx;
            f.anchor.file = h.file.clone();
            f.anchor.line = if f.anchor.side == "old" {
                h.old_start.max(1)
            } else {
                h.new_start.max(1)
            };
            f.plan_hash = plan_hash.to_string();
            f.moved = false;
            if f.status == FindingStatus::Orphaned {
                f.status = FindingStatus::Open;
            }
            continue;
        }
        // 2. Same-file content match on the anchored line text.
        let text = f.anchor.line_text.as_bytes();
        let matched = (!text.is_empty())
            .then(|| {
                view.hunks.iter().enumerate().find(|(_, h)| {
                    let file = view.file_of(h);
                    file.path == f.anchor.file.as_bytes()
                        && (h.added.iter().any(|l| l == text)
                            || h.removed.iter().any(|l| l == text))
                })
            })
            .flatten();
        if let Some((hi, h)) = matched {
            f.anchor.hunk_digest = doc.hunks[hi].digest.clone();
            f.anchor.line = if f.anchor.side == "old" {
                h.old_start.max(1)
            } else {
                h.new_start.max(1)
            };
            f.plan_hash = plan_hash.to_string();
            f.moved = true;
            if f.status == FindingStatus::Orphaned {
                f.status = FindingStatus::Open;
            }
        } else {
            f.status = FindingStatus::Orphaned;
        }
    }
}
