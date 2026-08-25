//! Configuration, split by ownership (ADR 0012, amended by ADR 0018-era split):
//!
//! - **Repo-level** `.differential.toml` at the target repo's root —
//!   classification hints only. Shared by everyone reviewing the repo.
//! - **User-level** `~/.config/differential/config.toml` (XDG) — `[grouping]`:
//!   which agent CLI to run and its timeout, and `[review]`: how much context
//!   the reviewer shows around a hunk. Both are per-user choices, not
//!   properties of the repo, so neither lives in it.
//!
//! HARD RULE (ADR 0012): config tunes classification hints and tool behaviour.
//! It can never remove a file or hunk from enumeration — enumeration runs before
//! and independently of anything in this module, and nothing here is consulted
//! by the parser or the invariants.

use std::path::{Path, PathBuf};

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

/// The user-level file: `[grouping]` and `[review]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUserConfig {
    #[serde(default)]
    grouping: GroupingConfig,
    #[serde(default)]
    review: ReviewConfig,
}

/// Everything `parse_user` reads, so `load` assigns one value rather than
/// growing a second assignment every time the user file gains a table.
#[derive(Debug, Default)]
pub struct UserConfig {
    pub grouping: GroupingConfig,
    pub review: ReviewConfig,
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

/// `[review]` — how much of a file the terminal reviewer shows around a hunk.
///
/// Presentation only: it can widen what is *displayed* around a hunk and can
/// never change which hunks exist. Enumeration is total and runs before any of
/// this (ADR 0005, 0012).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    /// Context lines shown either side of a hunk before any expansion.
    #[serde(default = "default_context")]
    pub context: usize,
    /// Lines one `z` at a context boundary row pulls in.
    #[serde(default = "default_context_step")]
    pub context_step: usize,
}

const fn default_context() -> usize {
    3
}

const fn default_context_step() -> usize {
    10
}

impl Default for ReviewConfig {
    fn default() -> Self {
        ReviewConfig {
            context: default_context(),
            context_step: default_context_step(),
        }
    }
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
    /// From the USER config: how much context the reviewer shows.
    pub review: ReviewConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            generated: GlobSet::empty(),
            not_generated: GlobSet::empty(),
            attributes: vec!["linguist-generated".to_string()],
            grouping: GroupingConfig::default(),
            review: ReviewConfig::default(),
        }
    }
}

/// `<user config dir>/differential/config.toml`.
///
/// The directory comes from `ConfigSource`; the two path components are
/// contract, not adapter, so they stay here.
pub fn user_config_path<S: crate::ports::ConfigSource>(src: &S) -> Option<PathBuf> {
    Some(
        src.user_config_dir()?
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
    pub fn load<S: crate::ports::ConfigSource>(
        src: &S,
        repo_root: &Path,
        repo_override: Option<&Path>,
        user_override: Option<&Path>,
    ) -> Result<Config, EngineError> {
        let repo_default = Some(repo_root.join(CONFIG_FILE_NAME));
        let mut config = match resolve(src, repo_override, repo_default)? {
            Some((text, origin)) => Self::parse(&text, &origin)?,
            None => Config::default(),
        };
        let user_default = src
            .user_config_dir()
            .map(|d| d.join(USER_CONFIG_DIR).join(USER_CONFIG_FILE_NAME));
        if let Some((text, origin)) = resolve(src, user_override, user_default)? {
            let user = Self::parse_user(&text, &origin)?;
            config.grouping = user.grouping;
            config.review = user.review;
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
            review: ReviewConfig::default(),
        })
    }

    /// Parse the USER file: `[grouping]` and `[review]`.
    pub fn parse_user(text: &str, origin: &str) -> Result<UserConfig, EngineError> {
        let raw: RawUserConfig = toml::from_str(text).map_err(|e| EngineError::Config {
            path: origin.to_string(),
            msg: e.to_string(),
        })?;
        Ok(UserConfig {
            grouping: raw.grouping,
            review: raw.review,
        })
    }
}

/// Read (contents, origin) for `explicit > default`, where a missing default
/// is fine but a missing EXPLICIT path is a hard error.
///
/// The policy — which file, what precedence, what absence means — is here; the
/// port only hands back bytes. The two read methods exist so that an
/// explicit-but-missing path reports the same message it always did.
fn resolve<S: crate::ports::ConfigSource>(
    src: &S,
    explicit: Option<&Path>,
    default: Option<PathBuf>,
) -> Result<Option<(String, String)>, EngineError> {
    match explicit {
        Some(p) => Ok(Some((src.read_required(p)?, p.display().to_string()))),
        None => {
            let Some(p) = default else {
                return Ok(None);
            };
            Ok(src.read(&p)?.map(|text| (text, p.display().to_string())))
        }
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
    /// The real filesystem: these assertions are about resolution policy
    /// (precedence, what absence means), which is what `load` owns.
    const SRC: crate::store::OsConfigSource = crate::store::OsConfigSource;

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
    fn user_config_parses_grouping_and_review() {
        let u = Config::parse_user(
            "[grouping]\ncommand = [\"my-llm\", \"--flag\"]\ntimeout_secs = 60",
            "test",
        )
        .unwrap();
        assert_eq!(
            u.grouping.command.as_deref(),
            Some(&["my-llm".to_string(), "--flag".to_string()][..])
        );
        assert_eq!(u.grouping.timeout_secs, Some(60));
        // An absent [review] means the defaults, not zero context.
        assert_eq!(u.review.context, 3);
        assert_eq!(u.review.context_step, 10);

        let u = Config::parse_user("[review]\ncontext_step = 25", "test").unwrap();
        assert_eq!(u.review.context_step, 25);
        assert_eq!(u.review.context, 3, "one key set must not zero the other");

        // Unknown keys and unknown sections stay hard errors.
        assert!(Config::parse_user("[grouping]\nmodel = \"x\"", "test").is_err());
        assert!(Config::parse_user("[review]\nlines = 5", "test").is_err());
        assert!(Config::parse_user("[classify]\ngenerated = []", "test").is_err());
    }

    #[test]
    fn load_composes_repo_and_user_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_file = tmp.path().join("repo.toml");
        let user_file = tmp.path().join("user.toml");
        std::fs::write(&repo_file, "[classify]\ngenerated = [\"gen/**\"]").unwrap();
        std::fs::write(
            &user_file,
            "[grouping]\ncommand = [\"agent\"]\n[review]\ncontext = 8",
        )
        .unwrap();
        let c = Config::load(
            &crate::store::OsConfigSource,
            tmp.path(),
            Some(&repo_file),
            Some(&user_file),
        )
        .unwrap();
        assert!(c.generated.is_match("gen/x"));
        assert_eq!(
            c.grouping.command.as_deref(),
            Some(&["agent".to_string()][..])
        );
        assert_eq!(c.review.context, 8);

        // Explicit-but-missing paths are hard errors; absent defaults are not.
        assert!(
            Config::load(&SRC, tmp.path(), Some(Path::new("/nope")), Some(&user_file)).is_err()
        );
        assert!(Config::load(&SRC, tmp.path(), None, Some(&user_file)).is_ok());
    }
}
