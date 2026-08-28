//! Ports: the traits the engine's business logic owns and adapters implement.
//!
//! Dependency direction only (ADR 0020). These exist so `pipeline`,
//! `invariants`, `tree`, `worktree` and `crates/stack` never name
//! `gitio::Repo` — each function's bound list is a reviewable statement of
//! exactly how much git it is allowed to touch, and `invariants` can no longer
//! so much as *express* `git log`.
//!
//! **Consumed by static dispatch.** There is exactly one implementation of
//! each git port, `gitio::Repo`, and ADR 0020 forbids a second one — a fake
//! git for tests included. Invariants 1–4 compare the engine's reconstruction
//! against git's own answer; against a fake they would compare the fake with
//! the fake and pass while proving nothing (ADR 0002). Tests use hermetic
//! temporary repositories and real `git`.
//!
//! The two runtime-open abstractions in this crate — `llm::LlmBackend` and
//! `lang::Language` — are deliberately NOT here. They are `dyn` because config
//! and a plugin registry pick them at run time; nothing in this module is
//! chosen at run time.
//!
//! There is no `trait Git: ObjectReader + …` convenience supertrait, and there
//! must not be: the whole value is that a consumer's bounds name what it
//! actually needs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::EngineError;
use crate::review_state::{Finding, ReviewState};

// ---------------------------------------------------------------- objects

/// Reading objects out of the odb.
pub trait ObjectReader {
    /// Blob content at `rev:path`, with `path` kept as raw bytes.
    ///
    /// `Ok(None)` when the path does not exist at that revision. Any other
    /// failure is a real error — an absent file and a broken repository must
    /// never render identically.
    fn blob(&self, rev: &str, path: &[u8]) -> Result<Option<Vec<u8>>, EngineError>;

    /// The same, for several specs at once, answered in the order given.
    ///
    /// Reading a blob costs a process, and a process costs milliseconds — so a
    /// caller that already knows every file it is about to draw should say so
    /// rather than paying that per file. Measured on a 120-file range: 240
    /// one-at-a-time reads took a second, against one call for the lot
    /// (ADR 0021's "what is left is process spawns").
    ///
    /// Bulk is the SAME need as `blob`, not a different one, which is why it
    /// sits on this port rather than earning its own.
    fn blobs(&self, specs: &[(&str, &[u8])]) -> Result<Vec<Option<Vec<u8>>>, EngineError>;

    /// Assert `oid` is present in the odb.
    ///
    /// Invariant 1 verifies binary files this way, since they carry no hunks
    /// to reconstruct from. Deliberately not `-> Result<bool>`: a recorded oid
    /// missing from the odb is a broken repository, not a failed invariant.
    fn require_object(&self, oid: &str) -> Result<(), EngineError>;
}

/// Writing objects into the odb. Loose and unreferenced (so gc-able) by
/// design — the engine creates a ref only where `RefWriter` says so.
pub trait ObjectWriter {
    /// Write `content` as a blob and return its oid.
    fn write_blob(&self, content: &[u8]) -> Result<String, EngineError>;
}

// ------------------------------------------------------------- revisions

/// Turning what the user typed into diff endpoints. Consumed only by
/// `pipeline`: nothing downstream of resolution ever re-resolves.
pub trait RangeResolver {
    fn merge_base(&self, a: &str, b: &str) -> Result<String, EngineError>;

    /// Resolve to a commit sha, or accept a raw tree oid.
    ///
    /// The endpoints of an uncommitted-state review are synthesized trees
    /// (ADR 0017) and every later stage is tree-safe.
    fn resolve_endpoint(&self, rev: &str) -> Result<String, EngineError>;
}

/// Peeling an endpoint to its tree oid — the one thing invariant 3 compares
/// against.
///
/// Kept apart from `RangeResolver` because its consumers (`invariants`,
/// `crates/stack`) must never resolve ranges.
pub trait TreeResolver {
    fn tree_of(&self, rev: &str) -> Result<String, EngineError>;
}

// ----------------------------------------------------------- enumeration

/// Canonical enumeration (ADR 0005: total, no exclusions, ever).
///
/// **FROZEN ARGV.** The byte format each method returns is what `parse.rs`,
/// `rename_view.rs` and ultimately the frozen normaliser were validated
/// against; changing a flag changes shape hashes and breaks real-corpus
/// parity. Add a method, never edit one.
pub trait DiffSource {
    /// `diff-tree -r -z --raw --full-index --no-renames`: authoritative modes,
    /// full oids, dispositions.
    fn raw_records(&self, base: &str, head: &str) -> Result<Vec<u8>, EngineError>;

    /// `diff-tree -r -U0 --no-renames --no-color --no-ext-diff`: the canonical
    /// patch. Every hunk in the system comes from here.
    fn canonical_patch(&self, base: &str, head: &str) -> Result<Vec<u8>, EngineError>;

    /// `diff-tree -r -M -z --name-status`: rename-detected **annotations**
    /// only (ADR 0003). Never affects what exists.
    fn rename_records(&self, base: &str, head: &str) -> Result<Vec<u8>, EngineError>;
}

/// Invariant 4's independent patch source, and nothing else.
///
/// A **separate trait** from `DiffSource` on purpose. Invariant 4 recounts
/// `@@` headers over a patch of the tree the engine built, using a counter
/// that is deliberately not the parser. Sharing an accessor with enumeration
/// would mean one edit to one flag silently moving both sides of the
/// comparison together.
///
/// An implementation MUST call git directly and MUST NOT delegate to
/// `DiffSource::canonical_patch` — note the argv genuinely differs today, and
/// that duplication is the point rather than an oversight.
///
/// The return type is `Vec<u8>` and must stay `Vec<u8>`: the moment this port
/// hands invariant 4 anything structured, the counter stops being independent.
pub trait RecountSource {
    fn recount_patch(&self, from: &str, to: &str) -> Result<Vec<u8>, EngineError>;
}

/// One `check-attr` answer for one path.
pub struct AttrValue {
    pub path: Vec<u8>,
    /// git's raw answer: a value, or `unspecified` / `unset` / `true` /
    /// `false`. What those *mean* is domain policy
    /// (`plan::attr_marks_generated`).
    pub value: Vec<u8>,
}

/// gitattributes lookup, for the generated-file **hint** — never enumeration.
///
/// Takes an attribute name the caller chose; it does not take a `Config` and
/// iterate one itself, because a port that reads config is a port that could
/// filter (ADR 0012).
pub trait AttributeSource {
    /// Note, unchanged from before this trait existed: `check-attr` consults
    /// the worktree/index `.gitattributes`, not the reviewed revisions —
    /// acceptable for a hint that can never remove a file from enumeration.
    fn check_attr(&self, attr: &str, paths: &[&[u8]]) -> Result<Vec<AttrValue>, EngineError>;
}

// -------------------------------------------------------- scratch index

/// One record to feed a scratch index.
///
/// Owned rather than borrowed: a text file's oid is produced by
/// `ObjectWriter::write_blob` inside the staging loop and would not outlive a
/// borrow. One allocation per changed file is free next to the subprocess it
/// is about to be piped into.
pub enum IndexEntry {
    Set {
        mode: String,
        oid: String,
        path: Vec<u8>,
    },
    Remove {
        path: Vec<u8>,
    },
}

/// Opening a scratch index. Never the user's index, never a checkout
/// (ADR 0011).
pub trait TreeBuilder {
    /// Not object-safe, by design: an associated type makes
    /// `Box<dyn TreeBuilder>` impossible, so runtime dispatch cannot creep
    /// back in behind this seam.
    type Session: IndexSession;

    /// A scratch index seeded from `tree_ish`.
    fn begin_from_tree(&self, tree_ish: &str) -> Result<Self::Session, EngineError>;

    /// A scratch index seeded from the repository's CURRENT index, for the
    /// ADR-0017 uncommitted-state snapshots.
    ///
    /// Errors if the index has unmerged entries — a conflicted index has no
    /// single tree.
    fn begin_from_current_index(&self) -> Result<Self::Session, EngineError>;
}

/// A scratch index, alive as long as the value. Dropping it removes the
/// temporary index file; blobs it wrote stay in the odb, unreferenced.
pub trait IndexSession {
    /// Stage a batch in one feed: quoting-proof, and one subprocess instead
    /// of one per file.
    fn stage(&mut self, entries: &[IndexEntry]) -> Result<(), EngineError>;

    /// Hash each path's CURRENT WORKTREE content into the odb and stage it,
    /// admitting new files and dropping ones deleted from the worktree.
    ///
    /// The worktree-snapshot primitive; nothing else may call it.
    fn stage_from_worktree(&mut self, nul_paths: &[u8]) -> Result<(), EngineError>;

    /// The tree oid of the currently staged state.
    fn write_tree(&self) -> Result<String, EngineError>;
}

/// Reading the working copy. Only the ADR-0017 snapshots use this.
pub trait WorkingCopy {
    /// NUL-terminated tracked paths.
    fn tracked_paths(&self) -> Result<Vec<u8>, EngineError>;
    /// NUL-terminated untracked-but-not-ignored paths.
    fn untracked_paths(&self) -> Result<Vec<u8>, EngineError>;

    /// Whether any tracked file differs from `HEAD`, staged or unstaged.
    ///
    /// Untracked files are a separate question — `untracked_paths` answers
    /// that — because a snapshot admits them via `--add`, and the two are
    /// detected by different plumbing.
    ///
    /// May answer `true` for a merely stat-dirty index, where a content
    /// comparison would say otherwise. That is the safe direction: a spurious
    /// `true` costs a no-op checkbox, a spurious `false` would hide an option
    /// the reviewer needs.
    fn has_tracked_changes(&self) -> Result<bool, EngineError>;
}

// ------------------------------------------------------ writes that publish

/// Author/committer identity for a synthetic commit. Domain data: a renderer
/// decides who its commits belong to.
pub struct CommitIdentity<'a> {
    pub name: &'a str,
    pub email: &'a str,
}

/// `commit-tree`. Separate from `IndexSession` because it does not touch an
/// index — it takes a tree oid already written.
pub trait CommitWriter {
    fn commit_tree(
        &self,
        tree: &str,
        parent: &str,
        message: &[u8],
        identity: CommitIdentity<'_>,
    ) -> Result<String, EngineError>;
}

/// `update-ref`. The only port in the engine that mutates repository state a
/// user can see, with exactly one consumer: the shadow-branch renderer.
pub trait RefWriter {
    fn update_ref(&self, name: &str, target: &str) -> Result<(), EngineError>;
}

// -------------------------------------------------------------- browsing

pub struct CommitSummary {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub author: String,
}

/// History browsing for the review-source picker.
pub trait CommitHistory {
    /// False on an unborn HEAD — there is nothing to diff against.
    fn has_commits(&self) -> bool;

    /// The most recent `max` commits reachable from `from`, newest first.
    fn recent_commits(&self, from: &str, max: usize) -> Result<Vec<CommitSummary>, EngineError>;

    /// Branch/tag/remote names by the COMMIT sha they point at, annotated tags
    /// peeled.
    ///
    /// Decoration only, so an unreadable ref list costs decoration and never
    /// the picker: the adapter returns an empty map rather than an error.
    fn refs_by_commit(&self) -> HashMap<String, Vec<String>>;
}

/// Where this repository's differential state lives. Path *policy* is domain
/// (`plan::grouping_cache_dir`, `plan::review_dir`); this only says where the
/// repository keeps its shared git directory.
pub trait RepoLayout {
    /// The shared git directory, absolutised (worktree-safe).
    fn common_dir(&self) -> Result<PathBuf, EngineError>;
    fn work_root(&self) -> &Path;
}

// ----------------------------------------------------------- persistence

/// The grouping cache (ADR 0009).
///
/// The stored value is the RAW model response, so audit and assembly stay pure
/// functions replayed on load and their fixes apply to cached runs too.
///
/// Keys are opaque hex from `grouping::cache_key`. An implementation MUST
/// treat them as opaque and MUST NOT derive, namespace or truncate them: the
/// key composition pins every existing cache entry in every checkout.
pub trait GroupingCache {
    fn get(&self, key: &str) -> Result<Option<String>, EngineError>;
    fn put(&self, key: &str, response: &str) -> Result<(), EngineError>;
}

/// Somewhere the model can read the pre-group document from (ADR 0022).
///
/// The grouping stage hands the model a path, not a payload, so the need is
/// "make this readable and tell me where" — one call, because a path the
/// caller composed itself would be a path the adapter never agreed to.
///
/// Keys are the grouping cache's keys. An implementation MUST treat them as
/// opaque, exactly as `GroupingCache` must.
pub trait ArtefactStore {
    fn make_readable(&self, key: &str, json: &str) -> Result<PathBuf, EngineError>;
}

/// One review's sidecar (ADR 0013).
///
/// Every read is total: a store that has never been written yields defaults,
/// never an error. `ReviewSession` is write-through — every mutator saves
/// before returning — so an implementation must be cheap enough for that, and
/// crash-safe in the sense that matters here: a torn write loses at most the
/// last action.
pub trait ReviewStore {
    /// Persist a plan document under its content hash and point `current` at
    /// it. Idempotent: re-saving the same hash must not rewrite the body.
    ///
    /// Takes serialised JSON and the hash rather than a `PlanDocument`,
    /// which keeps `schema` out of this module entirely — the frozen contract
    /// stays frozen (ADR 0008, 0018).
    fn save_plan(&self, hash: &str, json: &str) -> Result<(), EngineError>;

    fn load_state(&self) -> Result<ReviewState, EngineError>;
    fn save_state(&self, state: &ReviewState) -> Result<(), EngineError>;

    fn load_findings(&self) -> Result<Vec<Finding>, EngineError>;
    /// Rewrites the whole set (status changes, deletions, re-anchor results).
    /// The set is small; simplicity beats cleverness.
    fn save_findings(&self, findings: &[Finding]) -> Result<(), EngineError>;
}

/// Where configuration comes from.
///
/// The engine decides WHICH files to look for, what precedence they have and
/// what their absence means; this port only says where the user's config
/// directory is and hands back file contents.
pub trait ConfigSource {
    /// The user config directory. `None` when no home directory can be
    /// determined — then the user file simply does not exist.
    fn user_config_dir(&self) -> Option<PathBuf>;

    /// Contents, or `None` when the file does not exist. Any other failure
    /// (permissions, non-UTF-8) is an error.
    fn read(&self, path: &Path) -> Result<Option<String>, EngineError>;

    /// Contents of a file the caller named explicitly, where absence is a hard
    /// error rather than a default.
    ///
    /// A separate method rather than the domain synthesising the message from
    /// `read`'s `None`, so the error text comes from the same `std::fs` call
    /// it always did and cannot drift.
    fn read_required(&self, path: &Path) -> Result<String, EngineError>;
}
