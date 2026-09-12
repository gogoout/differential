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
    // `IgnoredAny` says exactly that — the table is parsed and discarded,
    // where a `toml::Table` was allocated in full and then discarded, with a
    // `let _ =` further down whose only job was to quiet the compiler about
    // a field nothing reads.
    //
    // Named with a leading underscore because nothing reads them and nothing
    // should: the `#[serde(rename)]` keeps the file's own spelling.
    #[serde(default, rename = "ordering")]
    _ordering: serde::de::IgnoredAny,
    #[serde(default, rename = "stack")]
    _stack: serde::de::IgnoredAny,
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
///
/// **Four of the five keep the model read-only; `Pi` does not** (ADR 0032).
/// Read [`Agent::read_only_is_enforced`] before choosing one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    /// Headless `claude`, read-only by tool allowlist (ADR 0022).
    #[default]
    ClaudeCode,
    /// Headless `codex exec`, read-only by OS sandbox (Seatbelt, bubblewrap).
    Codex,
    /// Headless `droid exec`, read-only by default — the tier is what we do
    /// not pass.
    Droid,
    /// Headless `copilot`, read-only by tool allowlist and an explicit deny.
    Copilot,
    /// Headless `pi`, **read-only is NOT enforced** (ADR 0032).
    ///
    /// Pi ships no sandbox and no per-command allowlist, and its `-t` flag
    /// toggles whole tools. The model needs `bash` to run the fetch command
    /// and `git diff`, and `bash` also lets it write, commit and push. Nothing
    /// but the prompt stops it. Choose this agent only knowing that.
    Pi,
}

impl Agent {
    /// Every agent, so a lister does not keep its own copy of the list.
    ///
    /// The array is exhaustive by hand, which a `match` would enforce and an
    /// array cannot. `all_agents_are_listed` in this module is that check.
    pub const ALL: [Agent; 5] = [
        Agent::ClaudeCode,
        Agent::Codex,
        Agent::Droid,
        Agent::Copilot,
        Agent::Pi,
    ];

    /// The name this agent answers to in `[grouping].agent`.
    ///
    /// Hand-written rather than derived, because serde renames on the way IN
    /// and there is no way to ask it for the string on the way out without a
    /// second derive. The `match` is the guard: a new variant does not compile
    /// until it has a name here.
    pub fn key(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
            Agent::Droid => "droid",
            Agent::Copilot => "copilot",
            Agent::Pi => "pi",
        }
    }

    /// What stops this agent writing, if anything.
    ///
    /// A caller that shows a user the list of agents MUST show this too. The
    /// person picking a name is the person who needs to know, and exactly one
    /// answer here is [`ReadOnly::NotEnforced`].
    pub fn read_only(self) -> ReadOnly {
        match self {
            Agent::ClaudeCode | Agent::Copilot => ReadOnly::ToolAllowlist,
            Agent::Codex => ReadOnly::OsSandbox,
            Agent::Droid => ReadOnly::AgentDefault,
            Agent::Pi => ReadOnly::NotEnforced,
        }
    }
}

/// What keeps an agent from writing.
///
/// An enum rather than a `bool` plus a sentence, because the three enforcing
/// answers are not interchangeable and a reader deciding whether to trust one
/// needs to know which they have. An OS sandbox holds against a model that
/// tries; an allowlist holds against a model that asks; a default holds only
/// until someone adds a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnly {
    /// The agent may run only the tools it was given, and writing is not one.
    ToolAllowlist,
    /// The agent may run anything and the kernel refuses the writes.
    OsSandbox,
    /// The agent is read-only until told otherwise, and it is not told.
    AgentDefault,
    /// **Nothing stops it.** The agent can write, commit and push, and only the
    /// prompt asks it not to. See [`Agent::Pi`] and ADR 0032 for why one agent
    /// is here and why that was a choice rather than an oversight.
    NotEnforced,
}

impl ReadOnly {
    pub fn is_enforced(self) -> bool {
        !matches!(self, ReadOnly::NotEnforced)
    }
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
        let user = Self::load_user(src, user_override)?;
        config.grouping = user.grouping;
        config.review = user.review;
        Ok(config)
    }

    /// The USER file alone: `[grouping]` and `[review]`, and no repository.
    ///
    /// [`load`](Self::load) needs a repository root to find the repo file.
    /// `dfr agents` has none — which agent you would run is a per-user choice
    /// and the question is answerable from anywhere. Rather than hand it a
    /// directory it has no use for, the user half is its own call, and `load`
    /// goes through it so there is one answer to "where does the user file
    /// live".
    ///
    /// A missing file means defaults; a malformed one is a hard error.
    pub fn load_user<S: crate::ports::ConfigSource>(
        src: &S,
        user_override: Option<&Path>,
    ) -> Result<UserConfig, EngineError> {
        match resolve(src, user_override, user_config_path(src))? {
            Some((text, origin)) => Self::parse_user(&text, &origin),
            None => Ok(UserConfig::default()),
        }
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

        // An agent nobody implements is a hard error that names every one that
        // exists. A silent fall back to the default would run a different agent
        // than the one asked for, and the cache key would agree with neither.
        let err = Config::parse_user("[grouping]\nagent = \"gpt\"", "test").unwrap_err();
        let text = err.to_string();
        for agent in Agent::ALL {
            assert!(
                text.contains(agent.key()),
                "the error must name {}: {text}",
                agent.key()
            );
        }

        // And the argv this key used to take is now one of those errors, not a
        // command that gets spawned without its allowlist.
        assert!(Config::parse_user("[grouping]\nagent = [\"my-llm\"]", "test").is_err());
        assert!(Config::parse_user("[review]\nlines = 5", "test").is_err());
        assert!(Config::parse_user("[classify]\ngenerated = []", "test").is_err());
    }

    #[test]
    fn every_agent_name_round_trips() {
        // `key` is hand-written and serde renames on the way in, so the two
        // can drift. They may not: `key` is what the docs print, what
        // `dfr agents` lists and what an error message offers, and a name a
        // user copies from any of those must parse.
        for agent in Agent::ALL {
            let toml = format!("[grouping]\nagent = \"{}\"", agent.key());
            let u = Config::parse_user(&toml, "test")
                .unwrap_or_else(|e| panic!("{} must parse: {e}", agent.key()));
            assert_eq!(u.grouping.agent, Some(agent), "{}", agent.key());
        }
    }

    #[test]
    fn all_agents_are_listed() {
        // `Agent::ALL` is an array, so nothing makes it exhaustive but this.
        // A variant missing from it is an agent nobody can find: it would not
        // appear in `dfr agents`, and the "valid names" error would not offer
        // it.
        fn covered(agent: Agent) -> bool {
            Agent::ALL.contains(&agent)
        }
        // The `match` is the point. Adding a variant breaks this line, and the
        // fix is to add it to `ALL` as well.
        for agent in Agent::ALL {
            match agent {
                Agent::ClaudeCode | Agent::Codex | Agent::Droid | Agent::Copilot | Agent::Pi => {
                    assert!(covered(agent))
                }
            }
        }
        assert_eq!(Agent::ALL.len(), 5, "a new agent belongs in ALL");

        // No two agents share a name.
        let mut keys: Vec<&str> = Agent::ALL.iter().map(|a| a.key()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "two agents share a name: {keys:?}");
    }

    #[test]
    fn exactly_one_agent_does_not_enforce_read_only() {
        // The tier is a fact a user must be shown, so it is pinned here rather
        // than left to a doc comment. Pi ships no sandbox and no per-command
        // allowlist (ADR 0032); the other four refuse a write.
        let unenforced: Vec<&str> = Agent::ALL
            .iter()
            .filter(|a| !a.read_only().is_enforced())
            .map(|a| a.key())
            .collect();
        assert_eq!(unenforced, vec!["pi"], "{unenforced:?}");
        assert!(
            Agent::default().read_only().is_enforced(),
            "the default must be enforced"
        );
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
