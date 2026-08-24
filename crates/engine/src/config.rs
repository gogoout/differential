//! Configuration, split by ownership (ADR 0012, amended by ADR 0018-era split):
//!
//! - **Repo-level** `.differential.toml` at the target repo's root —
//!   classification hints only. Shared by everyone reviewing the repo.
//! - **User-level** `~/.config/differential/config.toml` (XDG) — `[grouping]`:
//!   which agent CLI to run and its timeout. Agents are a per-user choice, so
//!   this never lives in the repo.
//!
//! HARD RULE (ADR 0012): config tunes classification hints and tool behaviour.
//! It can never remove a file or hunk from enumeration — enumeration runs before
//! and independently of anything in this module, and nothing here is consulted
//! by the parser or the invariants.

use std::path::{Path, PathBuf};

use etcetera::BaseStrategy;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::EngineError;

pub const CONFIG_FILE_NAME: &str = ".differential.toml";
pub const USER_CONFIG_DIR: &str = "differential";
pub const USER_CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    classify: RawClassify,
    /// Rejected with a migration hint — [grouping] moved to the user config.
    #[serde(default)]
    grouping: Option<toml::Table>,
    // Reserved for later milestones; accepted so the file format is stable.
    #[serde(default)]
    ordering: toml::Table,
    #[serde(default)]
    stack: toml::Table,
}

/// The user-level file: `[grouping]` only (for now).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUserConfig {
    #[serde(default)]
    grouping: GroupingConfig,
}

/// `[grouping]` — pure data; the pipeline turns it into an LLM backend.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupingConfig {
    /// Backend argv (prompt on stdin, completion on stdout). Default: the
    /// validated tools-denied claude invocation.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
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
    /// From the USER config, never the repo (agents differ per user).
    pub grouping: GroupingConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            generated: GlobSet::empty(),
            not_generated: GlobSet::empty(),
            attributes: vec!["linguist-generated".to_string()],
            grouping: GroupingConfig::default(),
        }
    }
}

/// `~/.config/differential/config.toml` (honours `XDG_CONFIG_HOME`).
/// `None` when no home directory can be determined.
pub fn user_config_path() -> Option<PathBuf> {
    let strategy = etcetera::choose_base_strategy().ok()?;
    Some(
        strategy
            .config_dir()
            .join(USER_CONFIG_DIR)
            .join(USER_CONFIG_FILE_NAME),
    )
}

impl Config {
    /// Resolution, per file: explicit path > default location > defaults.
    /// A missing file means defaults; a malformed file is a hard error, never
    /// silently ignored.
    ///
    /// Repo file: `<repo-root>/.differential.toml` — classification hints.
    /// User file: `~/.config/differential/config.toml` — `[grouping]`.
    pub fn load(
        repo_root: &Path,
        repo_override: Option<&Path>,
        user_override: Option<&Path>,
    ) -> Result<Config, EngineError> {
        let mut config = match resolve(repo_override, || Some(repo_root.join(CONFIG_FILE_NAME)))? {
            Some((text, origin)) => Self::parse(&text, &origin)?,
            None => Config::default(),
        };
        if let Some((text, origin)) = resolve(user_override, user_config_path)? {
            config.grouping = Self::parse_user(&text, &origin)?;
        }
        Ok(config)
    }

    /// Parse the REPO file: classification hints only. A `[grouping]` table
    /// here is a hard error with a pointer to its new home.
    pub fn parse(text: &str, origin: &str) -> Result<Config, EngineError> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| EngineError::Config {
            path: origin.to_string(),
            msg: e.to_string(),
        })?;
        if raw.grouping.is_some() {
            return Err(EngineError::Config {
                path: origin.to_string(),
                msg: "[grouping] moved to the user config \
                      (~/.config/differential/config.toml): the agent command is a \
                      per-user choice, not a repo setting"
                    .to_string(),
            });
        }
        let _ = (&raw.ordering, &raw.stack); // reserved
        Ok(Config {
            generated: build_globs(&raw.classify.generated, origin)?,
            not_generated: build_globs(&raw.classify.not_generated, origin)?,
            attributes: raw
                .classify
                .attributes
                .unwrap_or_else(|| vec!["linguist-generated".to_string()]),
            grouping: GroupingConfig::default(),
        })
    }

    /// Parse the USER file: `[grouping]` only.
    pub fn parse_user(text: &str, origin: &str) -> Result<GroupingConfig, EngineError> {
        let raw: RawUserConfig = toml::from_str(text).map_err(|e| EngineError::Config {
            path: origin.to_string(),
            msg: e.to_string(),
        })?;
        Ok(raw.grouping)
    }
}

/// Read (contents, origin) for `explicit > default_path()`, where a missing
/// default is fine but a missing EXPLICIT path is a hard error.
fn resolve(
    explicit: Option<&Path>,
    default_path: impl FnOnce() -> Option<PathBuf>,
) -> Result<Option<(String, String)>, EngineError> {
    let (path, explicit) = match explicit {
        Some(p) => (p.to_path_buf(), true),
        None => match default_path() {
            Some(p) => (p, false),
            None => return Ok(None),
        },
    };
    if !explicit && !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| EngineError::Config {
        path: path.display().to_string(),
        msg: e.to_string(),
    })?;
    Ok(Some((text, path.display().to_string())))
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
        Config::parse("[ordering]\nfuture = 1\n[stack]\nns = \"y\"", "test").unwrap();
    }

    #[test]
    fn grouping_in_repo_config_errors_with_migration_hint() {
        let err = Config::parse("[grouping]\ncommand = [\"x\"]", "test").unwrap_err();
        assert!(err.to_string().contains("user config"), "{err}");
    }

    #[test]
    fn user_config_parses_grouping_only() {
        let g = Config::parse_user(
            "[grouping]\ncommand = [\"my-llm\", \"--flag\"]\ntimeout_secs = 60",
            "test",
        )
        .unwrap();
        assert_eq!(
            g.command.as_deref(),
            Some(&["my-llm".to_string(), "--flag".to_string()][..])
        );
        assert_eq!(g.timeout_secs, Some(60));
        // Unknown keys and unknown sections stay hard errors.
        assert!(Config::parse_user("[grouping]\nmodel = \"x\"", "test").is_err());
        assert!(Config::parse_user("[classify]\ngenerated = []", "test").is_err());
    }

    #[test]
    fn load_composes_repo_and_user_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_file = tmp.path().join("repo.toml");
        let user_file = tmp.path().join("user.toml");
        std::fs::write(&repo_file, "[classify]\ngenerated = [\"gen/**\"]").unwrap();
        std::fs::write(&user_file, "[grouping]\ncommand = [\"agent\"]").unwrap();
        let c = Config::load(tmp.path(), Some(&repo_file), Some(&user_file)).unwrap();
        assert!(c.generated.is_match("gen/x"));
        assert_eq!(
            c.grouping.command.as_deref(),
            Some(&["agent".to_string()][..])
        );

        // Explicit-but-missing paths are hard errors; absent defaults are not.
        assert!(Config::load(tmp.path(), Some(Path::new("/nope")), Some(&user_file)).is_err());
        assert!(Config::load(tmp.path(), None, Some(&user_file)).is_ok());
    }
}
