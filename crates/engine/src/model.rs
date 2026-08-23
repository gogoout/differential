//! Data model for the canonical diff view.
//!
//! Paths are raw bytes throughout the engine; they become UTF-8 strings only at
//! JSON serialisation time, where a non-UTF-8 path is a hard error naming the
//! file (deferred support, never silent).

/// One line's content, without its newline. `Vec<Vec<u8>>` splits preserve the
/// trailing empty element that encodes "ends with a newline".
pub type Line = Vec<u8>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Added,
    Deleted,
    Modified,
}

impl Disposition {
    pub fn letter(self) -> u8 {
        match self {
            Disposition::Added => b'A',
            Disposition::Deleted => b'D',
            Disposition::Modified => b'M',
        }
    }
}

/// One changed file in the canonical (`--no-renames`) view. Exists independently
/// of hunks: empty-file adds/deletes, mode-only changes and binary files carry
/// zero hunks but are still real changes.
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: Vec<u8>,
    pub disposition: Disposition,
    /// "100644" etc. New-side mode; `None` for deletions.
    pub new_mode: Option<String>,
    /// Old-side mode when it differs, and for deletions.
    pub old_mode: Option<String>,
    pub binary: bool,
    /// `Some((old, new))` commit ids for gitlink (mode 160000) changes.
    pub submodule: Option<(Option<String>, Option<String>)>,
    /// Full blob oids from the `--raw --full-index` record. `new_oid` is used to
    /// stage BINARY files only — for text files the tree is always built from
    /// applied hunks, or the tree assertion would be tautological.
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
    /// Indices into the canonical hunk vector, in file order.
    pub hunks: Vec<usize>,
    // Annotations merged in from the rename-detected view:
    pub rename_similarity: Option<u8>,
    /// A side of a rename: where the content came from.
    pub rename_from: Option<Vec<u8>>,
    /// D side of a rename: where the content went.
    pub rename_to: Option<Vec<u8>>,
    /// Generated-file hint (never affects enumeration).
    pub generated: Option<GeneratedBy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedBy {
    Builtin,
    Attr,
    Config,
}

/// One canonical hunk from `git diff -U0 --no-renames`.
#[derive(Debug, Clone)]
pub struct Hunk {
    /// Index into the canonical file vector.
    pub file: usize,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Removed lines, without prefix or newline.
    pub removed: Vec<Line>,
    /// Added lines, without prefix or newline.
    pub added: Vec<Line>,
    /// `\ No newline at end of file` on the old side.
    pub nonl_old: bool,
    /// `\ No newline at end of file` on the new side.
    pub nonl_new: bool,
}

/// The canonical enumeration: every file, every hunk, no exclusions.
#[derive(Debug, Clone, Default)]
pub struct DiffView {
    pub files: Vec<FileChange>,
    /// Canonical order: file order in the diff, hunk order within each file.
    /// A hunk's id `hN` is its index here.
    pub hunks: Vec<Hunk>,
}

impl DiffView {
    pub fn file_of(&self, hunk: &Hunk) -> &FileChange {
        &self.files[hunk.file]
    }
}
