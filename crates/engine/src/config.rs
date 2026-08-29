//! Configuration, split by ownership (ADR 0012, amended by ADR 0018-era split):
//!
//! - **Repo-level** `.differential.toml` at the target repo's root —
//!   classification hints only. Shared by everyone reviewing the repo.
//! - **User-level** `~/.config/differential/config.toml` (XDG) — `[grouping]`:
//!   which agent CLI to run and its timeout, and `[review]`: which palette the
//!   reviewer wears, how much context it shows around a hunk, and which diff
//!   layout it opens in. All per-user choices, not properties of the repo, so
//!   none of them lives in it.
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

/// Which agent to run, by name.
///
/// It used to be a free argv, and that was the wrong shape. The grouping stage
/// does not merely spawn a process: it hands the agent a tool allowlist, a
/// fetch command and a prompt written for what that agent can do (ADR 0022).
/// An arbitrary argv gets the prompt and none of the rest, so it was a knob
/// that looked like it worked. A name selects an invocation this crate builds
/// whole, and adding an agent is adding a variant here.
///
/// The name also answers what a reviewer is shown while they wait — the argv
/// never could, at four times the width of the line it had.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    /// Headless `claude`, read-only tools (ADR 0022).
    #[default]
    ClaudeCode,
}

/// `[grouping]` — pure data; the application layer turns it into an LLM
/// backend (ADR 0018, 0020).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupingConfig {
    /// Which agent runs the grouping call. Default: `claude-code`.
    #[serde(default)]
    pub agent: Option<Agent>,
    /// How long to wait for it. Default: 1200 seconds.
    ///
    /// This one stays a number because it tunes the agent rather than replacing
    /// it: a slow machine or a large change may genuinely need longer.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Which palette the terminal reviewer wears, by name.
///
/// A name, for the same reason [`Agent`] is one: a palette is not a colour the
/// caller supplies but a whole coherent set the renderer builds — thirty-one
/// fields plus the syntax theme the code itself is painted with, all derived
/// together so the chrome and the code cannot disagree (ADR 0024). A free-form
/// colour list would be a knob that looked like it worked.
///
/// Adding a theme is adding a variant here and a seed in the renderer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    /// The original palette: a dark slate ground with a cyan accent.
    #[default]
    Dark,
    OneDark,
    OneLight,
    GruvboxDark,
    GruvboxLight,
    SolarizedDark,
    SolarizedLight,
    CatppuccinMocha,
    CatppuccinLatte,
    Dracula,
    Monokai,
}

/// `[review]` — how the terminal reviewer looks, and how much of a file it
/// shows around a hunk.
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
    /// Which diff layout a review opens in, before the reader says otherwise.
    ///
    /// A DEFAULT, not a setting: `s` still toggles, and the toggle is recorded
    /// per review. A review that has recorded a choice keeps it whatever this
    /// says, so changing it never moves a layout under someone mid-read.
    #[serde(default)]
    pub diff: DiffLayout,
    /// Which palette to wear. Default: `dark`.
    #[serde(default)]
    pub theme: ThemeName,
}

/// How the reviewer lays a hunk out.
///
/// An enum rather than a bool because a config key is permanent, and a third
/// layout would otherwise need a second key contradicting the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffLayout {
    /// Old and new side by side.
    #[default]
    Split,
    /// One column, removals above additions.
    Unified,
}

impl DiffLayout {
    pub fn is_split(self) -> bool {
        matches!(self, DiffLayout::Split)
    }
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
            diff: DiffLayout::default(),
            theme: ThemeName::default(),
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
    /// Defaults to [`DEFAULT_ATTRIBUTES`]; setting the key **replaces** the
    /// list rather than adding to it.
    pub attributes: Vec<String>,
    /// From the USER config, never the repo (agents differ per user).
    pub grouping: GroupingConfig,
    /// From the USER config: how much context the reviewer shows.
    pub review: ReviewConfig,
}

/// gitattributes names honoured as a "generated" declaration when
/// `[classify].attributes` is absent.
///
/// Two, because the convention is per-forge and a repository does not choose
/// its forge to suit this tool. `linguist-generated` is GitHub's, via Linguist;
/// `gitlab-generated` is GitLab's, and GitLab already honours it to collapse a
/// file in an MR diff — so a GitLab repository has usually declared its
/// generated files years before it meets this tool, and should not have to
/// declare them again.
///
/// The cost of an extra name is small and one-directional: a file has to carry
/// the attribute to match, and a repository that does not use a forge's
/// convention has nothing to match. A missed declaration is the expensive
/// direction — the file is offered to the model, grouped as real work, and read
/// by the reviewer.
pub const DEFAULT_ATTRIBUTES: &[&str] = &["linguist-generated", "gitlab-generated"];

fn default_attributes() -> Vec<String> {
    DEFAULT_ATTRIBUTES.iter().map(|s| s.to_string()).collect()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            generated: GlobSet::empty(),
            not_generated: GlobSet::empty(),
            attributes: default_attributes(),
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
            attributes: raw.classify.attributes.unwrap_or_else(default_attributes),
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
        assert_eq!(c.attributes, ["linguist-generated", "gitlab-generated"]);
        // Both forge conventions out of the box: a repository does not choose
        // its forge to suit this tool, and a missed declaration is the
        // expensive direction — the file is offered to the model, grouped as
        // real work, and read.
        assert_eq!(c.attributes, DEFAULT_ATTRIBUTES);
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
        // Setting the key REPLACES the default list; it does not extend it.
        // A repo naming only its own convention loses the forge ones, which is
        // the behaviour to know about rather than to discover.
        assert_eq!(c.attributes, ["linguist-generated", "custom-generated"]);
        let only_own =
            Config::parse("[classify]\nattributes = [\"custom-generated\"]", "test").unwrap();
        assert_eq!(only_own.attributes, ["custom-generated"]);
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
        let err = Config::parse("[grouping]\nagent = \"claude-code\"", "test").unwrap_err();
        assert!(err.to_string().contains("user config"), "{err}");
    }

    #[test]
    fn the_diff_layout_defaults_to_split_and_accepts_either_name() {
        // Absent means split. A reader who has never opened the config gets the
        // side-by-side layout.
        let u = Config::parse_user("[review]\ncontext = 3", "test").unwrap();
        assert_eq!(u.review.diff, DiffLayout::Split);
        assert!(u.review.diff.is_split());

        let u = Config::parse_user("[review]\ndiff = \"unified\"", "test").unwrap();
        assert_eq!(u.review.diff, DiffLayout::Unified);
        assert!(!u.review.diff.is_split());
        assert_eq!(u.review.context, 3, "setting one key must not zero another");

        let u = Config::parse_user("[review]\ndiff = \"split\"", "test").unwrap();
        assert_eq!(u.review.diff, DiffLayout::Split);

        // A typo is an error, not a silent fallback to the default.
        assert!(Config::parse_user("[review]\ndiff = \"side\"", "test").is_err());
    }

    #[test]
    fn user_config_parses_grouping_and_review() {
        let u = Config::parse_user(
            "[grouping]\nagent = \"claude-code\"\ntimeout_secs = 60",
            "test",
        )
        .unwrap();
        assert_eq!(u.grouping.agent, Some(Agent::ClaudeCode));
        assert_eq!(u.grouping.timeout_secs, Some(60));
        // An absent [review] means the defaults, not zero context.
        assert_eq!(u.review.context, 3);
        assert_eq!(u.review.context_step, 10);

        let u = Config::parse_user("[review]\ncontext_step = 25", "test").unwrap();
        assert_eq!(u.review.context_step, 25);
        assert_eq!(u.review.context, 3, "one key set must not zero the other");

        // Unknown keys and unknown sections stay hard errors.
        assert!(Config::parse_user("[grouping]\nmodel = \"x\"", "test").is_err());

        // An agent nobody implements is a hard error that says which ones
        // exist. A silent fall back to the default would run a different agent
        // than the one asked for, and the cache key would agree with neither.
        let err = Config::parse_user("[grouping]\nagent = \"gpt\"", "test").unwrap_err();
        assert!(err.to_string().contains("claude-code"), "{err}");

        // And the argv this key used to take is now one of those errors, not a
        // command that gets spawned without its allowlist.
        assert!(Config::parse_user("[grouping]\nagent = [\"my-llm\"]", "test").is_err());
        assert!(Config::parse_user("[review]\nlines = 5", "test").is_err());
        assert!(Config::parse_user("[classify]\ngenerated = []", "test").is_err());
    }

    /// A theme is a per-user choice like the agent, named for the same reason:
    /// serde renders the valid names for free, and adding one is a variant.
    #[test]
    fn user_config_parses_the_theme_and_names_the_valid_ones() {
        let u = Config::parse_user("[review]\ntheme = \"gruvbox-light\"", "test").unwrap();
        assert_eq!(u.review.theme, ThemeName::GruvboxLight);
        // Absent is the default, and does not zero the other keys.
        let u = Config::parse_user("[review]\ncontext = 8", "test").unwrap();
        assert_eq!(u.review.theme, ThemeName::Dark);
        assert_eq!(u.review.context, 8);

        // An unknown name is an error that says which ones exist.
        let err = Config::parse_user("[review]\ntheme = \"nosferatu\"", "test").unwrap_err();
        let msg = err.to_string();
        for name in [
            "dark",
            "light",
            "gruvbox-dark",
            "solarized-light",
            "monokai",
        ] {
            assert!(msg.contains(name), "{name} missing from: {msg}");
        }
    }

    /// The repo file cannot set it: a palette is the reader's, not the
    /// repository's. Nothing enforces this by hand — `RawConfig` has no
    /// `[review]` and denies unknown fields.
    #[test]
    fn a_theme_in_the_repo_config_is_rejected() {
        let err = Config::parse("[review]\ntheme = \"one-light\"", "test").unwrap_err();
        assert!(err.to_string().contains("review"), "{err}");
    }

    #[test]
    fn load_composes_repo_and_user_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_file = tmp.path().join("repo.toml");
        let user_file = tmp.path().join("user.toml");
        std::fs::write(&repo_file, "[classify]\ngenerated = [\"gen/**\"]").unwrap();
        std::fs::write(
            &user_file,
            "[grouping]\nagent = \"claude-code\"\n[review]\ncontext = 8",
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
        assert_eq!(c.grouping.agent, Some(Agent::ClaudeCode));
        assert_eq!(c.review.context, 8);

        // Explicit-but-missing paths are hard errors; absent defaults are not.
        assert!(
            Config::load(&SRC, tmp.path(), Some(Path::new("/nope")), Some(&user_file)).is_err()
        );
        assert!(Config::load(&SRC, tmp.path(), None, Some(&user_file)).is_ok());
    }
}
