//! Filesystem adapters: the mirror of `gitio` for everything that is not git.
//!
//! Implementations of the persistence ports, and the only place in the engine
//! outside `gitio` and `llm` that touches `std::fs` or the process
//! environment (ADR 0020). Path *policy* — where a cache or a review lives —
//! is domain and stays in `plan`; this module only reads and writes there.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::EngineError;
use crate::plan;
use crate::ports;
use crate::review_state::{Finding, ReviewState};

fn io_err(path: &Path, source: std::io::Error) -> EngineError {
    EngineError::Cache {
        path: path.display().to_string(),
        source,
    }
}

// ------------------------------------------------------------ grouping cache

#[derive(Serialize, Deserialize)]
struct Entry {
    response: String,
}

/// The on-disk grouping cache (ADR 0009).
///
/// Disabling is a state of this one type rather than an `Option` in a domain
/// signature: `--no-cache` must not put a branch back into the grouping stage,
/// nor force `None::<&FsGroupingCache>` turbofishes at every call site.
pub struct FsGroupingCache {
    dir: Option<PathBuf>,
}

impl FsGroupingCache {
    /// The repo's conventional cache directory.
    pub fn for_repo<L: ports::RepoLayout>(layout: &L) -> Result<Self, EngineError> {
        Ok(FsGroupingCache {
            dir: Some(plan::grouping_cache_dir(&layout.common_dir()?)),
        })
    }

    pub fn at(dir: PathBuf) -> Self {
        FsGroupingCache { dir: Some(dir) }
    }

    /// `--no-cache`: reads miss, writes are dropped.
    pub fn disabled() -> Self {
        FsGroupingCache { dir: None }
    }

    fn entry_path(dir: &Path, key: &str) -> PathBuf {
        dir.join(format!("{key}.json"))
    }
}

impl ports::GroupingCache for FsGroupingCache {
    fn get(&self, key: &str) -> Result<Option<String>, EngineError> {
        let Some(dir) = &self.dir else {
            return Ok(None);
        };
        let path = Self::entry_path(dir, key);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let entry: Entry = serde_json::from_str(&text)
                    .map_err(|e| EngineError::Cache {
                        path: path.display().to_string(),
                        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                    })
                    .expect("identity serialises");
                Ok(Some(entry.response))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(&path, e)),
        }
    }

    fn put(&self, key: &str, response: &str) -> Result<(), EngineError> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        let body = serde_json::to_string(&Entry {
            response: response.to_string(),
        })
        .expect("string serialises");
        let path = Self::entry_path(dir, key);
        std::fs::write(&path, body).map_err(|e| io_err(&path, e))
    }
}

/// What the regenerable cache currently holds, or what clearing it removed.
pub struct CacheUsage {
    pub groupings: usize,
    pub documents: usize,
    pub bytes: u64,
}

impl CacheUsage {
    pub fn is_empty(&self) -> bool {
        self.groupings == 0 && self.documents == 0
    }
}

/// Measure the regenerable cache without touching it.
pub fn cache_usage<L: ports::RepoLayout>(layout: &L) -> Result<CacheUsage, EngineError> {
    let common = layout.common_dir()?;
    let (groupings, g_bytes) = count(&plan::grouping_cache_dir(&common))?;
    let (documents, d_bytes) = count(&plan::artefact_dir(&common))?;
    Ok(CacheUsage {
        groupings,
        documents,
        bytes: g_bytes + d_bytes,
    })
}

/// Delete the regenerable cache, returning what was removed.
///
/// **It removes `plan::cache_dir` and nothing else.** Reviews live in a sibling
/// tree, so findings are out of reach by construction rather than by this
/// function being careful — see the note on `plan::cache_dir`.
///
/// An absent directory is success with an empty result: clearing a cache that
/// is already clear is not an error.
///
/// The count is taken before the delete, so a grouped run writing an entry in
/// between would have that entry removed without being counted. Deliberately
/// unlocked: the window is one syscall wide, the consequence is a report short
/// by one on a machine running two of its own commands at once, and a lock
/// would be a durable cost for a transient cosmetic one.
pub fn clear_cache<L: ports::RepoLayout>(layout: &L) -> Result<CacheUsage, EngineError> {
    let usage = cache_usage(layout)?;
    let dir = plan::cache_dir(&layout.common_dir()?);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(usage),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(usage),
        Err(e) => Err(io_err(&dir, e)),
    }
}

/// Entries and total bytes in one cache directory. An absent directory is zero.
fn count(dir: &Path) -> Result<(usize, u64), EngineError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(e) => return Err(io_err(dir, e)),
    };
    let mut n = 0usize;
    let mut bytes = 0u64;
    for entry in entries {
        let entry = entry.map_err(|e| io_err(dir, e))?;
        let meta = entry.metadata().map_err(|e| io_err(&entry.path(), e))?;
        if meta.is_file() {
            n += 1;
            bytes += meta.len();
        }
    }
    Ok((n, bytes))
}

// ---------------------------------------------------------------- artefact

/// Where the pre-group document is left for the model to read (ADR 0022).
///
/// Disabling mirrors `FsGroupingCache`: `--no-cache` must not put a branch back
/// into the grouping stage. With no directory the document goes to a temporary
/// file instead of being skipped — the model needs a path either way, and only
/// the survival of that path across runs is what caching buys.
pub struct FsArtefactStore {
    dir: Option<PathBuf>,
}

impl FsArtefactStore {
    pub fn for_repo<L: ports::RepoLayout>(layout: &L) -> Result<Self, EngineError> {
        Ok(FsArtefactStore {
            dir: Some(plan::artefact_dir(&layout.common_dir()?)),
        })
    }

    /// `--no-cache`: written under the temporary directory, not kept.
    pub fn disabled() -> Self {
        FsArtefactStore { dir: None }
    }
}

impl ports::ArtefactStore for FsArtefactStore {
    fn make_readable(&self, key: &str, json: &str) -> Result<PathBuf, EngineError> {
        let dir = self
            .dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("differential"));
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        let path = dir.join(format!("{key}.json"));
        std::fs::write(&path, json).map_err(|e| io_err(&path, e))?;
        Ok(path)
    }
}

// ------------------------------------------------------------- review store

/// One review's sidecar directory (ADR 0013).
pub struct FsReviewStore {
    dir: PathBuf,
}

impl FsReviewStore {
    /// Open the review with this id.
    ///
    /// The id is `review_identity::resolve`'s answer, not this adapter's: which
    /// review a spelling opens is domain policy, and it can be another
    /// spelling's review.
    pub fn for_review<L: ports::RepoLayout>(layout: &L, id: &str) -> Result<Self, EngineError> {
        Self::at(plan::review_dir(&layout.common_dir()?, id))
    }

    /// Test/tooling entry: open at an explicit directory.
    pub fn at(dir: PathBuf) -> Result<Self, EngineError> {
        std::fs::create_dir_all(dir.join("plans")).map_err(|e| io_err(&dir, e))?;
        Ok(FsReviewStore { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl ports::ReviewStore for FsReviewStore {
    fn save_plan(&self, hash: &str, json: &str) -> Result<(), EngineError> {
        let path = self.dir.join("plans").join(format!("{hash}.json"));
        // Content-addressed and immutable: re-saving the same hash is a no-op.
        if !path.exists() {
            std::fs::write(&path, json).map_err(|e| io_err(&path, e))?;
        }
        let current = self.dir.join("current");
        std::fs::write(&current, hash).map_err(|e| io_err(&current, e))
    }

    fn load_state(&self) -> Result<ReviewState, EngineError> {
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

    fn save_state(&self, state: &ReviewState) -> Result<(), EngineError> {
        let path = self.dir.join("state.json");
        let text = serde_json::to_string_pretty(state).expect("state serialises");
        std::fs::write(&path, text).map_err(|e| io_err(&path, e))
    }

    fn load_findings(&self) -> Result<Vec<Finding>, EngineError> {
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
            let f: Finding = serde_json::from_str(line)
                .map_err(|e| EngineError::Cache {
                    path: format!("{}:{}", path.display(), n + 1),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                })
                .expect("identity serialises");
            out.push(f);
        }
        Ok(out)
    }

    fn save_findings(&self, findings: &[Finding]) -> Result<(), EngineError> {
        let path = self.dir.join("findings.jsonl");
        let mut text = String::new();
        for f in findings {
            text.push_str(&serde_json::to_string(f).expect("serialises"));
            text.push('\n');
        }
        std::fs::write(&path, text).map_err(|e| io_err(&path, e))
    }
}

// ------------------------------------------------------------ config source

/// Config files from the real filesystem, with the user directory resolved by
/// platform convention.
pub struct OsConfigSource;

impl ports::ConfigSource for OsConfigSource {
    fn user_config_dir(&self) -> Option<PathBuf> {
        use etcetera::BaseStrategy;
        let strategy = etcetera::choose_base_strategy().ok()?;
        Some(strategy.config_dir())
    }

    fn read(&self, path: &Path) -> Result<Option<String>, EngineError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(EngineError::Config {
                path: path.display().to_string(),
                msg: e.to_string(),
            }),
        }
    }

    fn read_required(&self, path: &Path) -> Result<String, EngineError> {
        // Deliberately its own `std::fs` call rather than unwrapping `read`'s
        // `None`: the error text for an explicit-but-missing path is part of
        // the CLI contract and must come from where it always came from.
        std::fs::read_to_string(path).map_err(|e| EngineError::Config {
            path: path.display().to_string(),
            msg: e.to_string(),
        })
    }
}

// --------------------------------------------------------- review catalogue

/// The reviews directory, read as a catalogue.
///
/// Holds the common dir rather than a `RepoLayout`, so the one call that can
/// fail happens at construction and every later read is infallible policy.
pub struct FsReviewCatalogue {
    common_dir: PathBuf,
}

/// The on-disk form of `ports::ReviewIdentity`. Additive by the same rule as
/// the rest of the sidecar: every field defaults, so a file written by a later
/// version still loads.
#[derive(Serialize, Deserialize)]
struct IdentityFile {
    #[serde(default)]
    base: String,
    #[serde(default)]
    head_spec: String,
}

impl FsReviewCatalogue {
    pub fn new<L: ports::RepoLayout>(layout: &L) -> Result<Self, EngineError> {
        Ok(FsReviewCatalogue {
            common_dir: layout.common_dir()?,
        })
    }

    /// Test/tooling entry: the git common dir directly.
    pub fn at(common_dir: PathBuf) -> Self {
        FsReviewCatalogue { common_dir }
    }
}

impl ports::ReviewCatalogue for FsReviewCatalogue {
    fn filed_reviews(&self) -> Result<Vec<ports::FiledReview>, EngineError> {
        let root = plan::reviews_dir(&self.common_dir);
        // Never reviewed anything here yet. Not an error: an empty catalogue
        // is the correct answer, and the directory appears on the first open.
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_err(&root, e))?;
            if !entry.path().is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // A redirect is a pointer, not a review: listing it would let a
            // third spelling adopt the pointer instead of its target.
            if plan::alias_path(&self.common_dir, &id).exists() {
                continue;
            }
            let opened_as = match std::fs::read(plan::identity_path(&self.common_dir, &id)) {
                Ok(bytes) => serde_json::from_slice::<IdentityFile>(&bytes)
                    .ok()
                    .map(|f| ports::ReviewIdentity {
                        base: f.base,
                        head_spec: f.head_spec,
                    }),
                Err(_) => None,
            };
            out.push(ports::FiledReview { id, opened_as });
        }
        // Stable order, so a scan that finds two equally good candidates
        // cannot answer differently on two machines.
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn alias_of(&self, id: &str) -> Result<Option<String>, EngineError> {
        let path = plan::alias_path(&self.common_dir, id);
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(&path, e)),
        }
    }

    fn file_alias(&self, from: &str, to: &str) -> Result<(), EngineError> {
        let dir = plan::review_dir(&self.common_dir, from);
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        let path = plan::alias_path(&self.common_dir, from);
        std::fs::write(&path, to).map_err(|e| io_err(&path, e))
    }

    fn file_identity(
        &self,
        id: &str,
        opened_as: &ports::ReviewIdentity,
    ) -> Result<(), EngineError> {
        let dir = plan::review_dir(&self.common_dir, id);
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        let path = plan::identity_path(&self.common_dir, id);
        let body = serde_json::to_string_pretty(&IdentityFile {
            base: opened_as.base.clone(),
            head_spec: opened_as.head_spec.clone(),
        })
        .expect("identity serialises");
        std::fs::write(&path, body).map_err(|e| io_err(&path, e))
    }
}
