//! The frozen JSON contract for differential reading plans.
//!
//! This module is the product boundary (ADR 0008, superseded-in-form by ADR
//! 0018): every consumer (shadow-branch stack, TUI, forge review) depends on
//! these types and nothing else. It stays serde-only — consumer conveniences
//! and engine internals must not leak in here; that discipline is enforced in
//! review now that the crate boundary is gone.
//!
//! Contract rules:
//! - `schema_version` is 1. Readers must reject versions they do not know.
//! - Deserialisation tolerates unknown fields, so additive changes are non-breaking.
//! - `groups`/`reading_plan` are `null` when the grouping stage has not run. That is
//!   distinct from `[]`, which would mean "grouping ran and produced nothing" and is
//!   always a bug. `generator.stages` states exactly which stages produced the document.
//! - Optional fields serialise as explicit `null`, never omitted.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// The one JSON document: a grouped, ordered reading plan for a diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanDocument {
    pub schema_version: u32,
    pub generator: Generator,
    pub source: Source,
    pub stats: Stats,
    pub files: Vec<FileEntry>,
    pub hunks: Vec<HunkEntry>,
    pub classes: Vec<ClassEntry>,
    /// `None` until the grouping stage runs. `Some(vec![])` is a bug, not a state.
    pub groups: Option<Vec<Group>>,
    /// `None` until the grouping stage runs; ordered foundation-first once present.
    pub reading_plan: Option<Vec<ReadingStep>>,
    pub audit: Audit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Generator {
    pub tool: String,
    pub version: String,
    /// Pipeline stages that actually ran, in order: "enumerate", "classify",
    /// "group", "order".
    pub stages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    /// Fully resolved commit sha — a raw tree oid for `staged`/`worktree`
    /// sources, whose endpoints are synthesized snapshots of uncommitted
    /// state.
    pub base: String,
    /// Fully resolved commit sha — a raw tree oid for `staged`/`worktree`
    /// sources (see `base`).
    pub head: String,
    pub remote: Option<Remote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Commit,
    Range,
    Mr,
    Pr,
    /// HEAD vs the index (additive in schema v1).
    Staged,
    /// The index vs the worktree (additive in schema v1).
    Worktree,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Remote {
    pub forge: String,
    pub project: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub files: u32,
    pub hunks: u32,
    pub classes: u32,
    pub binary_files: u32,
    pub submodules: u32,
}

/// One changed file in the canonical (`--no-renames`) view. A rename therefore
/// appears as a D entry plus an A entry; the rename-detected view annotates both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub disposition: Disposition,
    /// New-side mode ("100644", "100755", "120000", "160000"); `None` on deletion.
    pub mode: Option<String>,
    /// Old-side mode when it differs from `mode`, and on deletion.
    pub old_mode: Option<String>,
    /// On the A side of a detected rename: where the content came from.
    pub old_path: Option<String>,
    /// On the D side of a detected rename: where the content went. Together with
    /// `old_path` this makes "moved and modified" addressable from both ends.
    pub new_path: Option<String>,
    /// Similarity score 0-100 from git's rename detection. Present on both sides of
    /// a detected rename. Below ~95 the change is a modification, not a relocation,
    /// and must never be treated as skim-eligible.
    pub rename_similarity: Option<u8>,
    /// Binary files carry zero hunks; content is tracked by object id only.
    pub binary: bool,
    pub submodule: Option<SubmoduleChange>,
    /// Hint for the noise tier. Computed (builtin list, gitattributes, repo config),
    /// never claimed by a model.
    pub generated: bool,
    pub generated_by: Option<GeneratedBy>,
    /// Ids into `hunks`, in file order.
    pub hunk_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    A,
    D,
    M,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmoduleChange {
    pub old: Option<String>,
    pub new: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GeneratedBy {
    /// Matched the built-in lockfile/artefact list.
    Builtin,
    /// Declared by the repo via a gitattributes attribute (e.g. linguist-generated).
    Attr,
    /// Matched a glob in the repo's `.differential.toml`.
    Config,
}

/// One canonical hunk from `git diff -U0 --no-renames`. Ids are positional
/// (`h0..hN` in enumeration order) and do NOT survive regeneration; `digest` does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HunkEntry {
    pub id: String,
    pub file: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Shape class id into `classes`.
    pub class: String,
    /// Exact content hash of the hunk (removed ++ added bytes, un-normalised).
    /// The stable anchor for comments and review state across regenerations.
    pub digest: String,
    /// `\ No newline at end of file` on the old side.
    pub nonl_old: bool,
    /// `\ No newline at end of file` on the new side.
    pub nonl_new: bool,
    /// Position in the forge's rename-detected diff, for posting comments.
    pub forge_position: ForgePosition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgePosition {
    /// Line in the new file; `None` for deletion-only hunks.
    pub new_line: Option<u32>,
    /// Line in the old file; `None` for insertion-only hunks.
    pub old_line: Option<u32>,
}

/// A shape class: hunks whose diff text is identical after normalising away
/// identifiers and literals on BOTH sides. Ids `C0..Cn`, numbered by descending
/// member count. 100% hunk coverage is by construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassEntry {
    pub id: String,
    pub hunk_ids: Vec<String>,
    /// The member a reviewer reads to verify the whole class.
    pub exemplar: String,
    /// True iff, after erasing identifiers and literals, the removed and added
    /// lines match — a structure-free substitution. Computed, never claimed.
    pub pure_substitution: bool,
}

/// A merged, labelled group of shape classes. Produced by the grouping stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub label: String,
    pub description: String,
    pub reason: String,
    pub effort: Effort,
    /// `None` until the ordering stage runs — role is an ordering-stage output.
    pub role: Option<Role>,
    pub class_ids: Vec<String>,
    /// Group ids this group depends on (it consumes what they define).
    pub depends_on: Vec<String>,
    /// Position in the foundation-first ordering.
    pub rank: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Read every hunk.
    Close,
    /// Read one exemplar per shape class; trust the rest.
    Skim,
    /// Generated content: folded entirely, no exemplars to read.
    Noise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Foundation,
    Consumer,
    Mechanical,
    Noise,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadingStep {
    pub group: String,
    pub action: ReadAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadAction {
    /// Read every hunk in the group.
    Read,
    /// Read one hunk per shape class.
    Exemplars,
    /// Remaining members of already-verified shapes.
    Skip,
    /// Noise group: collapsed entirely.
    Fold,
}

/// Structural audit. The first four fields exist for every document; the rest are
/// `null` until the grouping stage runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Audit {
    /// "n/n" — files reconstructed byte-exactly from base + hunks.
    pub applier_exact: String,
    /// "pass" — built-from-hunks tree equals the head tree.
    pub tree_assertion: String,
    pub hunks_carried: u32,
    /// Independent `@@` recount computed from git output, not from bookkeeping.
    pub recount: u32,
    pub coverage: Option<f64>,
    pub classes_missing: Option<u32>,
    pub classes_duplicated: Option<Vec<String>>,
    pub classes_hallucinated: Option<Vec<String>>,
    /// Hunks a reviewer actually reads (close + exemplars). The honest number.
    pub read_hunks: Option<u32>,
    /// Hunks never opened (skim remainders + folded noise). The genuine saving.
    pub skipped_hunks: Option<u32>,
}

#[derive(Debug)]
pub enum SchemaError {
    UnsupportedVersion { found: u32 },
    Json(serde_json::Error),
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::UnsupportedVersion { found } => write!(
                f,
                "unsupported schema_version {found} (this reader understands {SCHEMA_VERSION})"
            ),
            SchemaError::Json(e) => write!(f, "invalid plan document: {e}"),
        }
    }
}

impl std::error::Error for SchemaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SchemaError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for SchemaError {
    fn from(e: serde_json::Error) -> Self {
        SchemaError::Json(e)
    }
}

impl PlanDocument {
    /// Parse and enforce the version gate. Use this instead of raw serde_json.
    pub fn from_json(s: &str) -> Result<Self, SchemaError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            schema_version: u32,
        }
        let probe: VersionProbe = serde_json::from_str(s)?;
        if probe.schema_version != SCHEMA_VERSION {
            return Err(SchemaError::UnsupportedVersion {
                found: probe.schema_version,
            });
        }
        Ok(serde_json::from_str(s)?)
    }

    pub fn to_json_pretty(&self) -> Result<String, SchemaError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn to_json(&self) -> Result<String, SchemaError> {
        Ok(serde_json::to_string(self)?)
    }
}
