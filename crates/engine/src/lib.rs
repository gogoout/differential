//! Core engine: git io, diff parsing, byte-exact apply, shape classes,
//! invariants, and plan-document assembly.
//!
//! The canonical enumeration (`git diff -U0 --no-renames`) processes every file
//! with no exclusions (ADR 0005); the rename-detected view (`-M`) only annotates
//! it (ADR 0003). Repo config tunes classification hints and can never remove
//! anything from enumeration (ADR 0012).

pub mod apply;
pub mod artefact;
pub mod config;
pub mod document;
pub mod gitio;
pub mod grouping;
pub mod invariants;
pub mod lang;
pub mod llm;
pub mod model;
pub mod ordering;
pub mod parse;
pub mod paths;
pub mod pipeline;
pub mod plan;
pub mod ports;
pub mod rename_view;
pub mod review_identity;
pub mod review_session;
pub mod review_state;
pub mod schema;
pub mod shape;
pub mod store;
pub mod tree;
pub mod worktree;

pub use grouping::GroupingOptions;
pub use pipeline::{PipelineOutput, resolve_range, run_grouped_pipeline, run_pipeline, verify};
pub use review_session::ReviewSession;

/// The production session: a review over the on-disk sidecar. Renderers name
/// this one concrete type rather than carrying a generic parameter for a
/// choice they never make.
pub type FsReviewSession = ReviewSession<store::FsReviewStore>;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("failed to spawn git: {source}")]
    GitSpawn {
        #[source]
        source: std::io::Error,
    },

    #[error("{command} exited with {code:?}: {stderr}")]
    GitCommand {
        command: String,
        code: Option<i32>,
        stderr: String,
    },

    #[error("diff parse error at line {line}: {msg}")]
    Parse { line: usize, msg: String },

    #[error(
        "path is not valid UTF-8 and cannot be serialised to JSON yet: {lossy:?} \
         (non-UTF-8 path support is deferred)"
    )]
    NonUtf8Path { lossy: String },

    #[error("invariant violated: {0}")]
    Invariant(String),

    #[error("plan document is inconsistent: {0}")]
    PlanIntegrity(String),

    #[error("config error in {path}: {msg}")]
    Config { path: String, msg: String },

    #[error("bad revision range: {0}")]
    Range(String),

    #[error("grouping backend failed: {0}")]
    Llm(#[from] crate::llm::LlmError),

    #[error("grouping response unusable: {msg}; response sample: {sample}")]
    GroupingParse { msg: String, sample: String },

    #[error("grouping cache error at {path}: {source}")]
    Cache {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("schema error: {0}")]
    Schema(#[from] crate::schema::SchemaError),
}
