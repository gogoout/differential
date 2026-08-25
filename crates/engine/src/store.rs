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
                let entry: Entry = serde_json::from_str(&text).map_err(|e| EngineError::Cache {
                    path: path.display().to_string(),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                })?;
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

// ------------------------------------------------------------- review store

/// One review's sidecar directory (ADR 0013).
pub struct FsReviewStore {
    dir: PathBuf,
}

impl FsReviewStore {
    pub fn for_review<L: ports::RepoLayout>(
        layout: &L,
        base_sha: &str,
        head_spec: &str,
    ) -> Result<Self, EngineError> {
        let dir = plan::review_dir(&layout.common_dir()?, &plan::review_id(base_sha, head_spec));
        Self::at(dir)
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
            let f: Finding = serde_json::from_str(line).map_err(|e| EngineError::Cache {
                path: format!("{}:{}", path.display(), n + 1),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            })?;
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
