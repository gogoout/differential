//! The review-state sidecar store (ADR 0013, spec/persistence.md).
//!
//! Regeneration is total; state is a sidecar. Plan documents are immutable and
//! content-addressed; reviewed marks key on class CONTENT (sorted member
//! digests), and findings anchor on exact hunk digests — so both survive the
//! head moving and positional ids shifting. Re-anchoring never drops anything:
//! exact digest match → reattach; same-file content match → reattach flagged
//! moved; otherwise the finding is orphaned and listed.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::schema;

use crate::EngineError;
use crate::gitio::Repo;
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
    pub fn new(body: String, plan_hash: String, anchor: Anchor) -> Self {
        let created = now_unix();
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

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One review's directory under `<git-common-dir>/differential/reviews/<id>/`.
pub struct ReviewStore {
    dir: PathBuf,
}

impl ReviewStore {
    pub fn open(repo: &Repo, base_sha: &str, head_spec: &str) -> Result<Self, EngineError> {
        let dir = repo
            .common_dir()?
            .join("differential")
            .join("reviews")
            .join(review_id(base_sha, head_spec));
        Self::open_at(dir)
    }

    /// Test/tooling entry: open at an explicit directory.
    pub fn open_at(dir: PathBuf) -> Result<Self, EngineError> {
        std::fs::create_dir_all(dir.join("plans")).map_err(|e| io_err(&dir, e))?;
        Ok(ReviewStore { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Persist a plan document (content-addressed, immutable) and point
    /// `current` at it. Returns the plan hash.
    pub fn save_plan(&self, doc: &schema::PlanDocument) -> Result<String, EngineError> {
        let json = doc.to_json()?;
        let hash = crate::plan::plan_hash(&json);
        let path = self.dir.join("plans").join(format!("{hash}.json"));
        if !path.exists() {
            std::fs::write(&path, &json).map_err(|e| io_err(&path, e))?;
        }
        std::fs::write(self.dir.join("current"), &hash).map_err(|e| io_err(&self.dir, e))?;
        Ok(hash)
    }

    pub fn load_state(&self) -> Result<ReviewState, EngineError> {
        let path = self.dir.join("state.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| EngineError::Cache {
                path: path.display().to_string(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ReviewState::default()),
            Err(e) => Err(io_err(&path, e)),
        }
    }

    pub fn save_state(&self, state: &ReviewState) -> Result<(), EngineError> {
        let path = self.dir.join("state.json");
        let text = serde_json::to_string_pretty(state).expect("state serialises");
        std::fs::write(&path, text).map_err(|e| io_err(&path, e))
    }

    pub fn load_findings(&self) -> Result<Vec<Finding>, EngineError> {
        let path = self.dir.join("findings.jsonl");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(&path, e)),
        };
        let mut out = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let f: Finding = serde_json::from_str(line).map_err(|e| EngineError::Cache {
                path: format!("{}:{}", path.display(), n + 1),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            })?;
            out.push(f);
        }
        Ok(out)
    }

    pub fn append_finding(&self, finding: &Finding) -> Result<(), EngineError> {
        let path = self.dir.join("findings.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| io_err(&path, e))?;
        writeln!(
            file,
            "{}",
            serde_json::to_string(finding).expect("serialises")
        )
        .map_err(|e| io_err(&path, e))
    }

    /// Rewrite the whole findings file (status changes, deletions, re-anchor
    /// results). The set is small; simplicity beats cleverness here.
    pub fn save_findings(&self, findings: &[Finding]) -> Result<(), EngineError> {
        let path = self.dir.join("findings.jsonl");
        let mut text = String::new();
        for f in findings {
            text.push_str(&serde_json::to_string(f).expect("serialises"));
            text.push('\n');
        }
        std::fs::write(&path, text).map_err(|e| io_err(&path, e))
    }
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

fn io_err(path: &Path, source: std::io::Error) -> EngineError {
    EngineError::Cache {
        path: path.display().to_string(),
        source,
    }
}
