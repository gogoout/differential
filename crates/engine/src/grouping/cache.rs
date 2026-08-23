//! Grouping cache (ADR 0009): labels are non-deterministic across model calls,
//! coverage is not — so a grouping is pinned by a content hash of everything
//! that determines it. The cached value is the RAW model response; audit and
//! assembly are pure functions replayed on load, so their fixes apply to cached
//! runs too (prompt/payload changes must bump `PROMPT_VERSION` instead).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::EngineError;

use super::{ClassInfo, payload::PROMPT_VERSION};

/// Key over: prompt generation, backend identity, normaliser fingerprint, and
/// the exact class structure (sorted member hunk digests — content-exact, so
/// the key survives positional-id shifts across regenerations).
pub fn cache_key(offered: &[&ClassInfo], backend_name: &str, lang_fingerprint: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(PROMPT_VERSION.to_le_bytes());
    hasher.update([0]);
    hasher.update(backend_name.as_bytes());
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

#[derive(Serialize, Deserialize)]
struct Entry {
    response: String,
}

pub fn load(dir: &Path, key: &str) -> Result<Option<String>, EngineError> {
    let path = entry_path(dir, key);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let entry: Entry = serde_json::from_str(&text).map_err(|e| EngineError::Cache {
                path: path.display().to_string(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            })?;
            Ok(Some(entry.response))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(EngineError::Cache {
            path: path.display().to_string(),
            source: e,
        }),
    }
}

pub fn store(dir: &Path, key: &str, response: &str) -> Result<(), EngineError> {
    let io_err = |source| EngineError::Cache {
        path: dir.display().to_string(),
        source,
    };
    std::fs::create_dir_all(dir).map_err(io_err)?;
    let body = serde_json::to_string(&Entry {
        response: response.to_string(),
    })
    .expect("string serialises");
    std::fs::write(entry_path(dir, key), body).map_err(|source| EngineError::Cache {
        path: entry_path(dir, key).display().to_string(),
        source,
    })
}

fn entry_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}
