//! Raw git output in, a classified diff view out. No I/O.

use std::collections::HashSet;

use crate::EngineError;
use crate::config::Config;
use crate::document::mark_generated;
use crate::lang::LanguageRegistry;
use crate::model::DiffView;
use crate::parse::parse_canonical;
use crate::rename_view::{merge_raw, merge_renames, parse_raw_z, parse_renames_z};
use crate::shape::{Partition, partition};

/// The three git outputs enumeration is built from.
///
/// Bytes, not paths or a repository: which commands produced these is the
/// adapter's business, and the parsers were validated against real git output
/// (ADR 0002).
pub struct Enumeration<'a> {
    /// `diff-tree -r -z --raw --full-index --no-renames`: authoritative modes,
    /// full oids, dispositions.
    pub raw_records: &'a [u8],
    /// `diff-tree -r -U0 --no-renames --no-color --no-ext-diff`: the canonical
    /// patch. Every hunk in the system comes from here.
    pub canonical_patch: &'a [u8],
    /// `diff-tree -r -M -z --name-status`: rename-detected annotations only
    /// (ADR 0003). Never affects what exists.
    pub rename_records: &'a [u8],
}

/// Build the canonical view.
///
/// Takes **no `Config` and no `LanguageRegistry`**, and that is the point:
/// ADR 0012's "enumeration runs before and independently of config" used to be
/// a property of statement order inside a 95-line function, and is now a
/// property of this parameter list. Nothing reachable from here can remove a
/// file or a hunk.
pub fn build_view(e: &Enumeration<'_>) -> Result<DiffView, EngineError> {
    let records = parse_raw_z(e.raw_records)?;
    let dispositions = records
        .iter()
        .map(|r| (r.path.clone(), r.disposition()))
        .collect();

    // Canonical enumeration: every file, no exclusions (ADR 0005).
    let mut view = parse_canonical(e.canonical_patch, &dispositions)?;
    // Authoritative modes and oids overlay the patch; a count mismatch here is
    // an enumeration hole and errors rather than being papered over.
    merge_raw(&mut view, &records)?;
    merge_renames(&mut view, &parse_renames_z(e.rename_records)?);
    Ok(view)
}

/// Apply classification hints and compute the mechanical partition.
///
/// The only place config and languages enter the pipeline. Both tune how hunks
/// are *described*, never which ones exist (ADR 0012, ADR 0015).
pub fn classify(
    view: &mut DiffView,
    config: &Config,
    attr_marked: &HashSet<Vec<u8>>,
    langs: &LanguageRegistry,
) -> Partition {
    mark_generated(view, config, attr_marked);
    partition(view, langs)
}

/// Does a `check-attr` value declare a path generated?
///
/// git answers `unspecified` / `unset` / `false` for "no"; anything else — a
/// bare `true`, or a value — is a declaration. Which attribute names to ask
/// about is config's business; what the answers *mean* is this.
pub fn attr_marks_generated(value: &[u8]) -> bool {
    value != b"unspecified" && value != b"unset" && value != b"false"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_positive_attribute_value_declares_generated() {
        for no in [&b"unspecified"[..], b"unset", b"false"] {
            assert!(
                !attr_marks_generated(no),
                "{:?}",
                String::from_utf8_lossy(no)
            );
        }
        for yes in [&b"true"[..], b"linguist-generated", b"1", b""] {
            assert!(
                attr_marks_generated(yes),
                "{:?}",
                String::from_utf8_lossy(yes)
            );
        }
    }
}
