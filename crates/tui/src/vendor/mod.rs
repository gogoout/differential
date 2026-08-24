//! Code adapted from other MIT-licensed projects, with attribution headers in
//! each file (see LICENSE-MIT and the README credits):
//!
//! - `agavra/tuicr` — span wrapping/search utilities, syntax highlighting,
//!   terminal lifecycle.
//! - `jnsahaj/lumen` — the blob-to-rows diff engine with word-level emphasis.

pub mod diff_algo;
pub mod diff_types;
pub mod syntax;
pub mod terminal;
pub mod text_utils;

/// Which side of a diff a rendered line belongs to (tuicr's `LineOrigin`,
/// hosted here so the vendored syntax module stays self-contained).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOrigin {
    Context,
    Addition,
    Deletion,
}
