//! The rename-detected classification view (ADR 0003).
//!
//! Two inputs, both plumbing:
//! - `diff-tree -r -z --raw --full-index --no-renames`: authoritative modes,
//!   full oids and status per canonical file.
//! - `diff-tree -r -M -z --name-status`: rename pairs with similarity scores.
//!
//! Both only ANNOTATE the canonical view; they never add or remove entries.

use std::collections::HashMap;

use crate::EngineError;
use crate::model::{DiffView, Disposition};

/// One record from `--raw -z --full-index --no-renames`.
#[derive(Debug, Clone)]
pub struct RawRecord {
    pub old_mode: String,
    pub new_mode: String,
    pub old_oid: String,
    pub new_oid: String,
    pub status: u8,
    pub path: Vec<u8>,
}

const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Parse `:100644 100755 <old> <new> M\0path\0` records.
pub fn parse_raw_z(raw: &[u8]) -> Result<Vec<RawRecord>, EngineError> {
    let mut out = Vec::new();
    let mut fields = raw.split(|&b| b == 0);
    while let Some(meta) = fields.next() {
        if meta.is_empty() {
            continue;
        }
        let meta = meta
            .strip_prefix(b":".as_slice())
            .ok_or_else(|| bad(meta))?;
        let parts: Vec<&[u8]> = meta.split(|&b| b == b' ').collect();
        if parts.len() != 5 {
            return Err(bad(meta));
        }
        let path = fields.next().ok_or_else(|| bad(meta))?;
        out.push(RawRecord {
            old_mode: String::from_utf8_lossy(parts[0]).into_owned(),
            new_mode: String::from_utf8_lossy(parts[1]).into_owned(),
            old_oid: String::from_utf8_lossy(parts[2]).into_owned(),
            new_oid: String::from_utf8_lossy(parts[3]).into_owned(),
            status: *parts[4].first().ok_or_else(|| bad(meta))?,
            path: path.to_vec(),
        });
    }
    Ok(out)
}

fn bad(meta: &[u8]) -> EngineError {
    EngineError::Parse {
        line: 0,
        msg: format!(
            "unparseable raw diff record: {}",
            String::from_utf8_lossy(meta)
        ),
    }
}

/// Overlay authoritative modes/oids/dispositions onto the parsed canonical view.
pub fn merge_raw(view: &mut DiffView, records: &[RawRecord]) -> Result<(), EngineError> {
    let by_path: HashMap<&[u8], &RawRecord> =
        records.iter().map(|r| (r.path.as_slice(), r)).collect();
    for f in &mut view.files {
        let Some(r) = by_path.get(f.path.as_slice()) else {
            return Err(EngineError::Invariant(format!(
                "file {} present in patch but missing from raw records",
                String::from_utf8_lossy(&f.path)
            )));
        };
        f.disposition = match r.status {
            b'A' => Disposition::Added,
            b'D' => Disposition::Deleted,
            _ => Disposition::Modified,
        };
        if r.old_mode != "000000" {
            f.old_mode = Some(r.old_mode.clone());
        }
        f.new_mode = (r.new_mode != "000000").then(|| r.new_mode.clone());
        // A mode that did not change is not "old_mode" in the schema sense.
        if f.old_mode == f.new_mode {
            f.old_mode = None;
        }
        if f.disposition == Disposition::Deleted {
            f.old_mode = Some(r.old_mode.clone());
        }
        f.old_oid = (r.old_oid != ZERO_OID).then(|| r.old_oid.clone());
        f.new_oid = (r.new_oid != ZERO_OID).then(|| r.new_oid.clone());
    }
    if view.files.len() != records.len() {
        return Err(EngineError::Invariant(format!(
            "patch has {} files but raw listing has {} — enumeration hole",
            view.files.len(),
            records.len()
        )));
    }
    Ok(())
}

/// One rename pair from the `-M` view.
#[derive(Debug, Clone, PartialEq)]
pub struct RenamePair {
    pub old_path: Vec<u8>,
    pub new_path: Vec<u8>,
    /// 0-100.
    pub similarity: u8,
}

/// Parse `-M -z --name-status` output, keeping only `R<score>` records.
pub fn parse_renames_z(raw: &[u8]) -> Result<Vec<RenamePair>, EngineError> {
    let mut out = Vec::new();
    let mut fields = raw.split(|&b| b == 0);
    while let Some(status) = fields.next() {
        if status.is_empty() {
            continue;
        }
        match status.first() {
            Some(b'R') | Some(b'C') => {
                let score: u8 = std::str::from_utf8(&status[1..])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| EngineError::Parse {
                        line: 0,
                        msg: format!(
                            "unparseable rename score: {}",
                            String::from_utf8_lossy(status)
                        ),
                    })?;
                let old_path = fields.next().ok_or_else(missing)?.to_vec();
                let new_path = fields.next().ok_or_else(missing)?.to_vec();
                out.push(RenamePair {
                    old_path,
                    new_path,
                    similarity: score,
                });
            }
            _ => {
                // A/D/M/T: one path field, no annotation to extract.
                fields.next().ok_or_else(missing)?;
            }
        }
    }
    Ok(out)
}

fn missing() -> EngineError {
    EngineError::Parse {
        line: 0,
        msg: "rename record missing path".into(),
    }
}

/// Annotate both halves of each detected rename in the canonical view.
pub fn merge_renames(view: &mut DiffView, pairs: &[RenamePair]) {
    for p in pairs {
        for f in &mut view.files {
            if f.disposition == Disposition::Added && f.path == p.new_path {
                f.rename_from = Some(p.old_path.clone());
                f.rename_similarity = Some(p.similarity);
            } else if f.disposition == Disposition::Deleted && f.path == p.old_path {
                f.rename_to = Some(p.new_path.clone());
                f.rename_similarity = Some(p.similarity);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_records_parse() {
        let raw = b":100644 100755 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb M\0tools/run.sh\0:000000 100644 0000000000000000000000000000000000000000 cccccccccccccccccccccccccccccccccccccccc A\0new.txt\0";
        let rs = parse_raw_z(raw).unwrap();
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].status, b'M');
        assert_eq!(rs[0].new_mode, "100755");
        assert_eq!(rs[1].status, b'A');
        assert_eq!(rs[1].path, b"new.txt");
    }

    #[test]
    fn rename_records_parse_with_scores() {
        let raw =
            b"R062\0src/lexer.rs\0src/token/lexer.rs\0M\0src/main.rs\0R100\0old.txt\0new.txt\0";
        let rs = parse_renames_z(raw).unwrap();
        assert_eq!(
            rs,
            vec![
                RenamePair {
                    old_path: b"src/lexer.rs".to_vec(),
                    new_path: b"src/token/lexer.rs".to_vec(),
                    similarity: 62
                },
                RenamePair {
                    old_path: b"old.txt".to_vec(),
                    new_path: b"new.txt".to_vec(),
                    similarity: 100
                },
            ]
        );
    }
}
