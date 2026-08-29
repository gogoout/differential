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
    /// The reader's diff-layout choice, or `None` if they have not made one.
    ///
    /// `None` means "use the configured default" — which is why this is an
    /// option and not a bool. A state file written before this field existed
    /// records `false`, and that deserialises to `Some(false)`: a review
    /// already on disk keeps the layout it had, whatever the config now says.
    #[serde(default)]
    pub split_diff: Option<bool>,
    /// The reader's soft-wrap choice, or `None` if they have not pressed `w`.
    ///
    /// An option for the same reason `split_diff` is one: absent means the
    /// reader has never chosen, and the renderer's own default stands.
    #[serde(default)]
    pub wrap: Option<bool>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Anchor {
    pub file: String,
    /// "old" | "new"
    pub side: String,
    /// First anchored line, in file coordinates. DERIVED — `offset` is what
    /// survives a regeneration, and this is recomputed from it.
    pub line: u32,
    /// Last anchored line. Equal to `line` for a single-line anchor; `0` on a
    /// record written before ranges existed, which reads as "just `line`".
    #[serde(default)]
    pub end_line: u32,
    /// Lines from the hunk's start to `line`. **Signed**: a reader can
    /// annotate a context line, and context sits on both sides of a hunk.
    ///
    /// This, not `line`, is what the anchor is really made of. The digest
    /// fixes the hunk's CONTENT, so a hunk that moved in the file still holds
    /// the same line at the same offset — while its absolute line number did
    /// not survive the move. A record written before offsets existed has `0`,
    /// which lands it on the hunk's first line: exactly where it used to.
    #[serde(default)]
    pub offset: i32,
    /// Lines the anchor spans past `offset`. `0` is a single line.
    #[serde(default)]
    pub span: u32,
    pub hunk_digest: String,
    /// The anchored line's text — the fuzzy re-anchor key.
    #[serde(default)]
    pub line_text: String,
    /// The last anchored line's text, for the same job at the range's far end.
    #[serde(default)]
    pub end_line_text: String,
}

impl Anchor {
    /// Where the anchor's side of `hunk` begins in the file.
    fn hunk_start(&self, old_start: u32, new_start: u32) -> u32 {
        if self.side == "old" {
            old_start
        } else {
            new_start
        }
        .max(1)
    }

    /// The lines this annotates, as a reader writes them: `47`, or `47-52`.
    ///
    /// One place decides it, because `end_line` is `0` on a record written
    /// before ranges existed and every consumer would otherwise have to know
    /// that.
    pub fn line_span(&self) -> String {
        if self.end_line > self.line {
            format!("{}-{}", self.line, self.end_line)
        } else {
            self.line.to_string()
        }
    }

    /// Recompute the line numbers from the offset the anchor really carries.
    ///
    /// Clamped at 1, never at 0: a line number is 1-based, and an offset that
    /// would put one above the top of the file is a broken anchor, not line
    /// zero.
    fn resolve(&mut self, start: u32) {
        let at = i64::from(start) + i64::from(self.offset);
        self.line = at.max(1).min(i64::from(u32::MAX)) as u32;
        self.end_line = self.line.saturating_add(self.span);
    }
}

/// The lines a reviewer pointed at, in file coordinates.
///
/// An observation, not a decision: a renderer reports what its cursor was on,
/// and the engine turns it into an anchor — which side, how far into the hunk,
/// how many lines, and what text to re-find it by. `None` at the call site
/// means the whole hunk, which is what a finding filed from its header
/// annotates.
#[derive(Debug, Clone)]
pub struct Lines {
    /// "old" | "new"
    pub side: String,
    pub start: u32,
    pub end: u32,
    pub start_text: String,
    pub end_text: String,
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
        // 1. Exact digest. The content is identical, so the offset holds and
        //    only the hunk's position in the file has to be re-read.
        if let Some(h) = doc.hunks.iter().find(|h| h.digest == f.anchor.hunk_digest) {
            f.anchor.file = h.file.clone();
            let start = f.anchor.hunk_start(h.old_start, h.new_start);
            f.anchor.resolve(start);
            f.plan_hash = plan_hash.to_string();
            f.moved = false;
            if f.status == FindingStatus::Orphaned {
                f.status = FindingStatus::Open;
            }
            continue;
        }
        // 2. Same-file content match on the anchored line text. The hunk is
        //    not the one this was written against, so the offset is re-found
        //    from where the text now sits inside it — the anchor's own side
        //    first, since a line can appear on both.
        let text = f.anchor.line_text.as_bytes();
        let at = |lines: &[Vec<u8>]| lines.iter().position(|l| l == text);
        let matched = (!text.is_empty())
            .then(|| {
                view.hunks.iter().enumerate().find(|(_, h)| {
                    let file = view.file_of(h);
                    file.path == f.anchor.file.as_bytes()
                        && (at(&h.added).is_some() || at(&h.removed).is_some())
                })
            })
            .flatten();
        if let Some((hi, h)) = matched {
            f.anchor.hunk_digest = doc.hunks[hi].digest.clone();
            // An offset is a position in ONE side's numbering, so the side it
            // was found on is the side it now belongs to. Keeping the old side
            // while taking the fallback's index paired one side's offset with
            // the other side's start, and the note landed on an unrelated line
            // wherever `old_start` and `new_start` had diverged — silently,
            // and reported as a clean re-anchor.
            let own = if f.anchor.side == "old" { "old" } else { "new" };
            let lines_of = |side: &str| if side == "old" { &h.removed } else { &h.added };
            let found = at(lines_of(own))
                .map(|p| (own, p))
                .or_else(|| at(&h.added).map(|p| ("new", p)))
                .or_else(|| at(&h.removed).map(|p| ("old", p)));
            let (side, offset) = found.unwrap_or((own, 0));
            f.anchor.side = side.to_string();
            f.anchor.offset = offset as i32;
            let start = f.anchor.hunk_start(h.old_start, h.new_start);
            f.anchor.resolve(start);
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
