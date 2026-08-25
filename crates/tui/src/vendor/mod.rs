//! Code adapted from other MIT-licensed projects, with attribution headers in
//! each file (see LICENSE-MIT and the README credits).
//!
//! Trimmed to what this crate actually calls: the module is private, so the
//! compiler can see what is reachable, and everything it reported unreachable
//! is gone (a search feature that was never ported, and the suspend/resume
//! terminal machinery). Keep it that way — do not re-add a helper "for later",
//! and do not make the module public, which would make every `pub fn` in it
//! exported surface and silence the dead-code lint entirely.
//!
//! Adapted from:
//!
//! - `agavra/tuicr` — syntax highlighting, terminal lifecycle, and one span
//!   truncation helper. Its span-wrapping and search utilities were taken too
//!   and are the bulk of what the trim removed.
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
