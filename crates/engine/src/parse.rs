//! Parser for `git diff-tree -r -U0 --no-renames` patch output.
//!
//! Count-driven: the `@@ -a,b +c,d @@` header states exactly how many removed and
//! added lines follow, so hunk bodies are consumed by count, never by prefix
//! sniffing. (Prefix sniffing silently drops deleted lines that start with `--`:
//! deleting a line `--help` emits `---help`, which a `startswith("---")` header
//! guard eats. That was a real latent bug in the validated prototype.)
//!
//! Unknown lines are hard errors, not skips — a parser that guesses is how hunks
//! get lost silently.

use std::collections::HashMap;

use crate::EngineError;
use crate::model::{DiffView, Disposition, FileChange, Hunk};
use crate::paths::parse_diff_git_path;

/// Parse `-z --name-status` output (`git diff-tree -r --no-renames -z --name-status`).
/// Typechange (`T`) maps to `Modified`.
pub fn parse_name_status_z(raw: &[u8]) -> Result<HashMap<Vec<u8>, Disposition>, EngineError> {
    let mut out = HashMap::new();
    let mut it = raw.split(|&b| b == 0).filter(|s| !s.is_empty());
    while let Some(status) = it.next() {
        let path = it.next().ok_or_else(|| EngineError::Parse {
            line: 0,
            msg: "name-status record missing path".into(),
        })?;
        let disp = match status.first() {
            Some(b'A') => Disposition::Added,
            Some(b'D') => Disposition::Deleted,
            Some(b'M') | Some(b'T') => Disposition::Modified,
            other => {
                return Err(EngineError::Parse {
                    line: 0,
                    msg: format!("unexpected name-status {other:?} (renames must be off)"),
                });
            }
        };
        out.insert(path.to_vec(), disp);
    }
    Ok(out)
}

/// Parse the canonical `-U0 --no-renames` patch into a [`DiffView`].
///
/// `dispositions` comes from a separate `--name-status -z` call; header hints
/// (`new file mode` / `deleted file mode`) are only a fallback.
pub fn parse_canonical(
    raw: &[u8],
    dispositions: &HashMap<Vec<u8>, Disposition>,
) -> Result<DiffView, EngineError> {
    let lines: Vec<&[u8]> = split_lines(raw);
    let mut files: Vec<FileChange> = Vec::new();
    let mut hunks: Vec<Hunk> = Vec::new();
    // Extra per-file parse state, parallel to `files`.
    let mut header_disp: Vec<Option<Disposition>> = Vec::new();

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];

        if let Some(rest) = line.strip_prefix(b"diff --git ".as_slice()) {
            let path = parse_diff_git_path(rest).ok_or_else(|| EngineError::Parse {
                line: i + 1,
                msg: format!("unparseable diff --git header: {}", lossy(line)),
            })?;
            files.push(FileChange {
                path,
                disposition: Disposition::Modified, // resolved in finalise
                new_mode: None,
                old_mode: None,
                binary: false,
                submodule: None,
                hunks: Vec::new(),
                rename_similarity: None,
                rename_from: None,
                rename_to: None,
                generated: None,
                old_oid: None,
                new_oid: None,
            });
            header_disp.push(None);
            i += 1;
            continue;
        }

        let Some(f) = files.last_mut() else {
            // diff-tree between two trees may emit nothing at all; anything else
            // before the first file header is unexpected.
            if line.is_empty() {
                i += 1;
                continue;
            }
            return Err(EngineError::Parse {
                line: i + 1,
                msg: format!("content before first diff header: {}", lossy(line)),
            });
        };

        if let Some(rest) = line.strip_prefix(b"old mode ".as_slice()) {
            f.old_mode = Some(lossy(rest));
        } else if let Some(rest) = line.strip_prefix(b"new mode ".as_slice()) {
            f.new_mode = Some(lossy(rest));
        } else if let Some(rest) = line.strip_prefix(b"new file mode ".as_slice()) {
            f.new_mode = Some(lossy(rest));
            *header_disp.last_mut().unwrap() = Some(Disposition::Added);
        } else if let Some(rest) = line.strip_prefix(b"deleted file mode ".as_slice()) {
            f.old_mode = Some(lossy(rest));
            *header_disp.last_mut().unwrap() = Some(Disposition::Deleted);
        } else if let Some(rest) = line.strip_prefix(b"index ".as_slice()) {
            // `index <old>..<new>[ <mode>]` — mode present when unchanged.
            if let Some(pos) = rest.iter().rposition(|&b| b == b' ') {
                let mode = &rest[pos + 1..];
                if mode.len() == 6 && mode.iter().all(u8::is_ascii_digit) && f.new_mode.is_none() {
                    f.new_mode = Some(lossy(mode));
                }
            }
        } else if line.starts_with(b"Binary files ") || line == b"GIT binary patch" {
            f.binary = true;
        } else if line.starts_with(b"--- ") || line.starts_with(b"+++ ") {
            // Paths already known from the diff --git header.
        } else if line.starts_with(b"@@ ") {
            let (old_start, old_count, new_start, new_count) =
                parse_hunk_header(line).ok_or_else(|| EngineError::Parse {
                    line: i + 1,
                    msg: format!("unparseable hunk header: {}", lossy(line)),
                })?;

            let mut removed = Vec::with_capacity(old_count as usize);
            let mut added = Vec::with_capacity(new_count as usize);
            let mut nonl_old = false;
            let mut nonl_new = false;
            i += 1;

            for _ in 0..old_count {
                let body = *lines.get(i).ok_or_else(|| truncated(i))?;
                let content =
                    body.strip_prefix(b"-".as_slice())
                        .ok_or_else(|| EngineError::Parse {
                            line: i + 1,
                            msg: format!("expected removed line, got: {}", lossy(body)),
                        })?;
                removed.push(content.to_vec());
                i += 1;
            }
            if old_count > 0 && lines.get(i).is_some_and(|l| l.starts_with(b"\\")) {
                nonl_old = true;
                i += 1;
            }
            for _ in 0..new_count {
                let body = *lines.get(i).ok_or_else(|| truncated(i))?;
                let content =
                    body.strip_prefix(b"+".as_slice())
                        .ok_or_else(|| EngineError::Parse {
                            line: i + 1,
                            msg: format!("expected added line, got: {}", lossy(body)),
                        })?;
                added.push(content.to_vec());
                i += 1;
            }
            if new_count > 0 && lines.get(i).is_some_and(|l| l.starts_with(b"\\")) {
                nonl_new = true;
                i += 1;
            }

            let file_idx = files.len() - 1;
            files[file_idx].hunks.push(hunks.len());
            hunks.push(Hunk {
                file: file_idx,
                old_start,
                old_count,
                new_start,
                new_count,
                removed,
                added,
                nonl_old,
                nonl_new,
            });
            continue; // i already advanced past the body
        } else if line.is_empty() && i == lines.len() {
            // unreachable; kept for clarity
        } else if line.is_empty() {
            // Blank separator lines do not occur in -U0 patch output; treat as
            // end-of-input noise only if nothing follows.
            if lines[i + 1..].iter().all(|l| l.is_empty()) {
                i += 1;
                continue;
            }
            return Err(EngineError::Parse {
                line: i + 1,
                msg: "unexpected blank line inside patch".into(),
            });
        } else {
            return Err(EngineError::Parse {
                line: i + 1,
                msg: format!("unrecognised patch line: {}", lossy(line)),
            });
        }
        i += 1;
    }

    finalise(files, hunks, header_disp, dispositions)
}

/// Resolve dispositions, extract submodule ids, and merge duplicate path entries
/// (a typechange emits two file headers for the same path).
fn finalise(
    mut files: Vec<FileChange>,
    mut hunks: Vec<Hunk>,
    header_disp: Vec<Option<Disposition>>,
    dispositions: &HashMap<Vec<u8>, Disposition>,
) -> Result<DiffView, EngineError> {
    for (f, hd) in files.iter_mut().zip(&header_disp) {
        f.disposition = dispositions
            .get(&f.path)
            .copied()
            .or(*hd)
            .unwrap_or(Disposition::Modified);
    }

    // Merge consecutive duplicate-path entries (typechange): hunks concatenate,
    // old side from the first, new side from the second.
    let mut merged: Vec<FileChange> = Vec::with_capacity(files.len());
    for f in files.into_iter() {
        let last_idx = merged.len().wrapping_sub(1);
        match merged.last_mut() {
            Some(prev) if prev.path == f.path => {
                if f.new_mode.is_some() {
                    prev.new_mode = f.new_mode;
                }
                if prev.old_mode.is_none() {
                    prev.old_mode = f.old_mode;
                }
                prev.binary |= f.binary;
                prev.disposition = dispositions
                    .get(&prev.path)
                    .copied()
                    .unwrap_or(Disposition::Modified);
                for h in &f.hunks {
                    hunks[*h].file = last_idx;
                    prev.hunks.push(*h);
                }
            }
            _ => {
                let idx = merged.len();
                for h in &f.hunks {
                    hunks[*h].file = idx;
                }
                merged.push(f);
            }
        }
    }

    // Submodules: gitlink mode; commit ids live in the pseudo-hunk body.
    for f in merged.iter_mut() {
        let is_gitlink =
            f.new_mode.as_deref() == Some("160000") || f.old_mode.as_deref() == Some("160000");
        if !is_gitlink {
            continue;
        }
        let mut old = None;
        let mut new = None;
        for &hi in &f.hunks {
            for l in &hunks[hi].removed {
                if let Some(rest) = l.strip_prefix(b"Subproject commit ".as_slice()) {
                    old = Some(lossy(rest));
                }
            }
            for l in &hunks[hi].added {
                if let Some(rest) = l.strip_prefix(b"Subproject commit ".as_slice()) {
                    new = Some(lossy(rest));
                }
            }
        }
        f.submodule = Some((old, new));
    }

    Ok(DiffView {
        files: merged,
        hunks,
    })
}

/// `@@ -a[,b] +c[,d] @@…` — counts default to 1 when elided.
fn parse_hunk_header(line: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix(b"@@ -".as_slice())?;
    let (old_start, rest) = take_num(rest)?;
    let (old_count, rest) = take_opt_count(rest)?;
    let rest = rest.strip_prefix(b" +".as_slice())?;
    let (new_start, rest) = take_num(rest)?;
    let (new_count, rest) = take_opt_count(rest)?;
    rest.starts_with(b" @@").then_some(())?;
    Some((old_start, old_count, new_start, new_count))
}

fn take_num(s: &[u8]) -> Option<(u32, &[u8])> {
    let end = s
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let n: u32 = std::str::from_utf8(&s[..end]).ok()?.parse().ok()?;
    Some((n, &s[end..]))
}

fn take_opt_count(s: &[u8]) -> Option<(u32, &[u8])> {
    match s.strip_prefix(b",".as_slice()) {
        Some(rest) => take_num(rest),
        None => Some((1, s)),
    }
}

fn split_lines(raw: &[u8]) -> Vec<&[u8]> {
    let mut v: Vec<&[u8]> = raw.split(|&b| b == b'\n').collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

fn truncated(i: usize) -> EngineError {
    EngineError::Parse {
        line: i + 1,
        msg: "patch truncated inside hunk body".into(),
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(&b[..b.len().min(160)]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disp_map(entries: &[(&[u8], Disposition)]) -> HashMap<Vec<u8>, Disposition> {
        entries.iter().map(|(p, d)| (p.to_vec(), *d)).collect()
    }

    #[test]
    fn simple_modification() {
        let raw = b"diff --git a/f.txt b/f.txt\n\
index 000..111 100644\n\
--- a/f.txt\n\
+++ b/f.txt\n\
@@ -2,1 +2,2 @@\n\
-old line\n\
+new line\n\
+extra\n";
        let v = parse_canonical(raw, &disp_map(&[(b"f.txt", Disposition::Modified)])).unwrap();
        assert_eq!(v.files.len(), 1);
        assert_eq!(v.hunks.len(), 1);
        let h = &v.hunks[0];
        assert_eq!(
            (h.old_start, h.old_count, h.new_start, h.new_count),
            (2, 1, 2, 2)
        );
        assert_eq!(h.removed, vec![b"old line".to_vec()]);
        assert_eq!(h.added, vec![b"new line".to_vec(), b"extra".to_vec()]);
        assert_eq!(v.files[0].new_mode.as_deref(), Some("100644"));
    }

    #[test]
    fn deleted_line_starting_with_dashes_is_kept() {
        // Deleting the line `--help` emits `---help`. Prefix sniffing eats it;
        // count-driven parsing must not.
        let raw = b"diff --git a/cli.txt b/cli.txt\n\
--- a/cli.txt\n\
+++ b/cli.txt\n\
@@ -1,2 +1,1 @@\n\
---help\n\
-+++weird\n\
+usage\n";
        let v = parse_canonical(raw, &Default::default()).unwrap();
        let h = &v.hunks[0];
        assert_eq!(h.removed, vec![b"--help".to_vec(), b"+++weird".to_vec()]);
        assert_eq!(h.added, vec![b"usage".to_vec()]);
    }

    #[test]
    fn count_elision_defaults_to_one() {
        let raw = b"diff --git a/f b/f\n\
--- a/f\n\
+++ b/f\n\
@@ -5 +5,2 @@ fn context()\n\
-x\n\
+y\n\
+z\n";
        let v = parse_canonical(raw, &Default::default()).unwrap();
        let h = &v.hunks[0];
        assert_eq!(
            (h.old_start, h.old_count, h.new_start, h.new_count),
            (5, 1, 5, 2)
        );
    }

    #[test]
    fn no_newline_markers_per_side() {
        let raw = b"diff --git a/f b/f\n\
--- a/f\n\
+++ b/f\n\
@@ -1,1 +1,1 @@\n\
-old\n\
\\ No newline at end of file\n\
+new\n\
\\ No newline at end of file\n";
        let v = parse_canonical(raw, &Default::default()).unwrap();
        let h = &v.hunks[0];
        assert!(h.nonl_old);
        assert!(h.nonl_new);
    }

    #[test]
    fn nonl_old_only() {
        // Newline added to the final line: old side lacks it, new side has it.
        let raw = b"diff --git a/f b/f\n\
--- a/f\n\
+++ b/f\n\
@@ -1,1 +1,1 @@\n\
-old\n\
\\ No newline at end of file\n\
+old\n";
        let v = parse_canonical(raw, &Default::default()).unwrap();
        assert!(v.hunks[0].nonl_old);
        assert!(!v.hunks[0].nonl_new);
    }

    #[test]
    fn binary_file_has_no_hunks() {
        let raw = b"diff --git a/img.png b/img.png\n\
new file mode 100644\n\
index 000..111\n\
Binary files /dev/null and b/img.png differ\n";
        let v = parse_canonical(raw, &disp_map(&[(b"img.png", Disposition::Added)])).unwrap();
        assert!(v.files[0].binary);
        assert!(v.files[0].hunks.is_empty());
        assert_eq!(v.files[0].disposition, Disposition::Added);
    }

    #[test]
    fn mode_only_change_has_no_hunks() {
        let raw = b"diff --git a/run.sh b/run.sh\n\
old mode 100644\n\
new mode 100755\n";
        let v = parse_canonical(raw, &Default::default()).unwrap();
        assert_eq!(v.files[0].old_mode.as_deref(), Some("100644"));
        assert_eq!(v.files[0].new_mode.as_deref(), Some("100755"));
        assert!(v.files[0].hunks.is_empty());
    }

    #[test]
    fn submodule_pseudo_hunk_is_kept_and_ids_extracted() {
        let raw = b"diff --git a/dep b/dep\n\
index aaa..bbb 160000\n\
--- a/dep\n\
+++ b/dep\n\
@@ -1 +1 @@\n\
-Subproject commit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
+Subproject commit bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        let v = parse_canonical(raw, &Default::default()).unwrap();
        assert_eq!(v.hunks.len(), 1, "pseudo-hunk stays in the canonical count");
        let (old, new) = v.files[0].submodule.clone().unwrap();
        assert_eq!(old.unwrap(), "a".repeat(40));
        assert_eq!(new.unwrap(), "b".repeat(40));
    }

    #[test]
    fn unrecognised_line_is_a_hard_error() {
        let raw = b"diff --git a/f b/f\n\
some garbage git will never emit\n";
        assert!(parse_canonical(raw, &Default::default()).is_err());
    }

    #[test]
    fn typechange_double_entry_merges() {
        let raw = b"diff --git a/link b/link\n\
deleted file mode 100644\n\
--- a/link\n\
+++ /dev/null\n\
@@ -1,1 +0,0 @@\n\
-real content\n\
diff --git a/link b/link\n\
new file mode 120000\n\
--- /dev/null\n\
+++ b/link\n\
@@ -0,0 +1,1 @@\n\
+target\n\
\\ No newline at end of file\n";
        let v = parse_canonical(raw, &disp_map(&[(b"link", Disposition::Modified)])).unwrap();
        assert_eq!(v.files.len(), 1);
        assert_eq!(v.files[0].hunks.len(), 2);
        assert_eq!(v.files[0].old_mode.as_deref(), Some("100644"));
        assert_eq!(v.files[0].new_mode.as_deref(), Some("120000"));
        assert_eq!(v.hunks[1].file, 0);
    }

    #[test]
    fn name_status_z_parses() {
        let raw = b"A\0new.txt\0D\0gone.txt\0M\0mod.txt\0T\0type.txt\0";
        let m = parse_name_status_z(raw).unwrap();
        assert_eq!(m[b"new.txt".as_slice()], Disposition::Added);
        assert_eq!(m[b"gone.txt".as_slice()], Disposition::Deleted);
        assert_eq!(m[b"mod.txt".as_slice()], Disposition::Modified);
        assert_eq!(m[b"type.txt".as_slice()], Disposition::Modified);
    }
}
