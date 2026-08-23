//! Per-repo configuration: `.differential.toml` at the target repo's root.
//!
//! HARD RULE (ADR 0012): config tunes classification hints and tool behaviour.
//! It can never remove a file or hunk from enumeration — enumeration runs before
//! and independently of anything in this module, and nothing here is consulted
//! by the parser or the invariants.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::EngineError;

pub const CONFIG_FILE_NAME: &str = ".differential.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    classify: RawClassify,
    // Reserved for later milestones; accepted so the file format is stable.
    #[serde(default)]
    grouping: toml::Table,
    #[serde(default)]
    ordering: toml::Table,
    #[serde(default)]
    stack: toml::Table,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClassify {
    #[serde(default)]
    generated: Vec<String>,
    #[serde(default)]
    not_generated: Vec<String>,
    #[serde(default)]
    attributes: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct Config {
    /// Additive globs marking files as generated (noise-tier hint).
    pub generated: GlobSet,
    /// Overrides: never mark these generated. Wins over everything.
    pub not_generated: GlobSet,
    /// gitattributes attribute names honoured as "generated" declarations.
    pub attributes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            generated: GlobSet::empty(),
            not_generated: GlobSet::empty(),
            attributes: vec!["linguist-generated".to_string()],
        }
    }
}

impl Config {
    /// Resolution: explicit path > `<repo-root>/.differential.toml` > defaults.
    /// A missing file means defaults; a malformed file is a hard error, never
    /// silently ignored.
    pub fn load(repo_root: &Path, explicit: Option<&Path>) -> Result<Config, EngineError> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => {
                let p = repo_root.join(CONFIG_FILE_NAME);
                if !p.exists() {
                    return Ok(Config::default());
                }
                p
            }
        };
        let text = std::fs::read_to_string(&path).map_err(|e| EngineError::Config {
            path: path.display().to_string(),
            msg: e.to_string(),
        })?;
        Self::parse(&text, &path.display().to_string())
    }

    pub fn parse(text: &str, origin: &str) -> Result<Config, EngineError> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| EngineError::Config {
            path: origin.to_string(),
            msg: e.to_string(),
        })?;
        let _ = (&raw.grouping, &raw.ordering, &raw.stack); // reserved
        Ok(Config {
            generated: build_globs(&raw.classify.generated, origin)?,
            not_generated: build_globs(&raw.classify.not_generated, origin)?,
            attributes: raw
                .classify
                .attributes
                .unwrap_or_else(|| vec!["linguist-generated".to_string()]),
        })
    }
}

fn build_globs(patterns: &[String], origin: &str) -> Result<GlobSet, EngineError> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let glob = Glob::new(p).map_err(|e| EngineError::Config {
            path: origin.to_string(),
            msg: format!("bad glob {p:?}: {e}"),
        })?;
        b.add(glob);
    }
    b.build().map_err(|e| EngineError::Config {
        path: origin.to_string(),
        msg: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        let c = Config::parse("", "test").unwrap();
        assert_eq!(c.attributes, vec!["linguist-generated"]);
        assert!(!c.generated.is_match("anything"));
    }

    #[test]
    fn globs_and_overrides() {
        let c = Config::parse(
            r#"
[classify]
generated = ["**/__snapshots__/**", "migrations/**"]
not_generated = ["important.lock"]
attributes = ["linguist-generated", "custom-generated"]
"#,
            "test",
        )
        .unwrap();
        assert!(c.generated.is_match("ui/__snapshots__/x.snap"));
        assert!(c.generated.is_match("migrations/0001_init.sql"));
        assert!(!c.generated.is_match("src/main.rs"));
        assert!(c.not_generated.is_match("important.lock"));
        assert_eq!(c.attributes.len(), 2);
    }

    #[test]
    fn malformed_config_is_a_hard_error() {
        assert!(Config::parse("classify = 5", "test").is_err());
        assert!(Config::parse("[classify]\nnope = true", "test").is_err());
    }

    #[test]
    fn reserved_sections_are_accepted() {
        Config::parse("[grouping]\nmodel = \"x\"\n[stack]\nns = \"y\"", "test").unwrap();
    }
}
