//! Invariants 1–4 (spec/invariants.md). All of them run before any document is
//! emitted; every one caught a real bug during prototype validation.

use crate::EngineError;
use crate::gitio::Repo;
use crate::model::{DiffView, Disposition, Hunk};
use crate::tree::build_tree;

#[derive(Debug, Clone, serde::Serialize)]
pub struct InvariantReport {
    pub files_total: usize,
    /// Text files (non-binary, non-submodule) checked byte-exactly.
    pub applier_total: usize,
    pub applier_ok: usize,
    pub applier_mismatches: Vec<String>,
    /// Binary files verified by oid instead of by reconstruction.
    pub binary_oid_checked: usize,
    pub hunks_total: usize,
    pub accounting_ok: bool,
    pub built_tree: Option<String>,
    pub head_tree: String,
    pub tree_ok: bool,
    pub recount: usize,
    pub recount_ok: bool,
}

impl InvariantReport {
    pub fn all_ok(&self) -> bool {
        self.applier_mismatches.is_empty()
            && self.applier_ok == self.applier_total
            && self.accounting_ok
            && self.tree_ok
            && self.recount_ok
    }

    /// "n/n" for the audit block.
    pub fn applier_exact(&self) -> String {
        format!("{}/{}", self.applier_ok, self.applier_total)
    }
}

/// Run invariants 1–4. Invariant 1 (applier fidelity) is asserted BEFORE the
/// tree is built; if it fails, nothing is built on top of it.
pub fn check_all(
    repo: &Repo,
    base: &str,
    head: &str,
    view: &DiffView,
) -> Result<InvariantReport, EngineError> {
    // ---- Invariant 1: applier fidelity ------------------------------------
    let mut applier_total = 0usize;
    let mut applier_ok = 0usize;
    let mut mismatches = Vec::new();
    let mut binary_checked = 0usize;

    for f in &view.files {
        if f.submodule.is_some() {
            continue;
        }
        if f.binary {
            // Verified by oid: the recorded object must exist in the odb.
            if let Some(oid) = &f.new_oid {
                repo.run(["cat-file", "-e", oid], None)?;
            }
            binary_checked += 1;
            continue;
        }
        applier_total += 1;
        let hunks: Vec<&Hunk> = f.hunks.iter().map(|&i| &view.hunks[i]).collect();
        let base_content = repo.blob(base, &f.path)?;
        let got = crate::apply::apply_hunks(base_content.as_deref(), &hunks);
        let want = if f.disposition == Disposition::Deleted {
            Vec::new()
        } else {
            repo.blob(head, &f.path)?.unwrap_or_default()
        };
        if got == want {
            applier_ok += 1;
        } else {
            mismatches.push(format!(
                "{}: reconstructed {}B, expected {}B",
                String::from_utf8_lossy(&f.path),
                got.len(),
                want.len()
            ));
        }
    }

    // ---- Invariant 2: hunk accounting --------------------------------------
    let mut seen = vec![false; view.hunks.len()];
    let mut accounting_ok = true;
    let mut carried = 0usize;
    for (fi, f) in view.files.iter().enumerate() {
        for &hi in &f.hunks {
            if hi >= seen.len() || seen[hi] || view.hunks[hi].file != fi {
                accounting_ok = false;
                continue;
            }
            seen[hi] = true;
            carried += 1;
        }
    }
    accounting_ok &= carried == view.hunks.len();

    let head_tree = repo.rev_parse_raw(&format!("{head}^{{tree}}"))?;

    // ---- Invariant 3: non-tautological tree assertion ----------------------
    // Refuse to build on a broken applier (the prototype's rule).
    let (built_tree, tree_ok) = if mismatches.is_empty() && applier_ok == applier_total {
        let built = build_tree(repo, base, view)?;
        let ok = built == head_tree;
        (Some(built), ok)
    } else {
        (None, false)
    };

    // ---- Invariant 4: independent recount -----------------------------------
    // Computed from git's own output over the BUILT tree, by a counter that is
    // deliberately not the parser.
    let (recount, recount_ok) = match &built_tree {
        Some(t) => {
            let out = repo.run(
                ["diff-tree", "-r", "-U0", "--no-renames", base, t.as_str()],
                None,
            )?;
            let n = dumb_hunk_count(&out);
            (n, n == view.hunks.len())
        }
        None => (0, false),
    };

    Ok(InvariantReport {
        files_total: view.files.len(),
        applier_total,
        applier_ok,
        applier_mismatches: mismatches,
        binary_oid_checked: binary_checked,
        hunks_total: view.hunks.len(),
        accounting_ok,
        built_tree,
        head_tree,
        tree_ok,
        recount,
        recount_ok,
    })
}

/// The deliberately dumb `@@` counter. Must never share code with `parse.rs` —
/// a shared bug would make invariant 4 circular.
pub fn dumb_hunk_count(patch: &[u8]) -> usize {
    patch
        .split(|&b| b == b'\n')
        .filter(|l| l.starts_with(b"@@ -"))
        .count()
}

#[cfg(test)]
mod tests {
    use super::dumb_hunk_count;

    #[test]
    fn dumb_counter_counts_headers_only() {
        let patch = b"diff --git a/f b/f\n@@ -1,2 +1,2 @@\n-a\n+b\n@@ -9 +9 @@\n-x\n+y\n";
        assert_eq!(dumb_hunk_count(patch), 2);
    }

    #[test]
    fn dumb_counter_ignores_content_lines_that_mention_hunks() {
        // An added line whose content starts with "@@ -" gets a "+" prefix in
        // the patch, so it cannot collide.
        let patch = b"@@ -1,0 +1,1 @@\n+@@ -5,5 +5,5 @@\n";
        assert_eq!(dumb_hunk_count(patch), 1);
    }
}
