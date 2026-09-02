//! Invariants 1–4 (spec/invariants.md); every one caught a real bug during
//! prototype validation.
//!
//! They fall into two halves, and the split is the write boundary.
//!
//! **Invariants 1 and 2 are read-only and core.** They run inside the pipeline,
//! and no document is emitted when either fails. Invariant 1b — the enumeration
//! hole — is read-only too, and lives earlier still, in `rename_view::merge_raw`.
//!
//! **Invariants 3 and 4 build a tree, so they write.** Only a consumer that
//! reconstructs a tree is protected by them, which is the shadow-branch builder
//! alone. They run in `pipeline::verify`, which the caller invokes when it wants
//! them, and they land in the report as `Some(TreeReport)`.

use crate::EngineError;
use crate::model::{DiffView, Disposition, Hunk};
use crate::ports::{ObjectReader, ObjectWriter, RecountSource, TreeBuilder, TreeResolver};
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
    /// Invariants 3 and 4. `None` means `pipeline::verify` did not run — not
    /// that it ran and passed. Consumers must not read absence as success.
    pub tree: Option<TreeReport>,
}

/// Invariants 3 and 4: the half that needs a built tree, and so a write.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TreeReport {
    pub built_tree: Option<String>,
    pub head_tree: String,
    pub tree_ok: bool,
    pub recount: usize,
    pub recount_ok: bool,
}

impl InvariantReport {
    /// Invariants 1 and 2 — everything the read-only pipeline can assert.
    ///
    /// This is the gate on emitting a document at all: a bad parse, a dropped
    /// hunk or broken accounting all fail here, and all three would make a
    /// renderer show the wrong thing.
    pub fn fidelity_ok(&self) -> bool {
        self.applier_mismatches.is_empty()
            && self.applier_ok == self.applier_total
            && self.accounting_ok
    }

    /// Invariants 1 to 4. **False when the tree half never ran**, which is
    /// correct rather than a bug: a caller that wants the weaker claim asks
    /// `fidelity_ok`.
    pub fn all_ok(&self) -> bool {
        self.fidelity_ok()
            && self
                .tree
                .as_ref()
                .is_some_and(|t| t.tree_ok && t.recount_ok)
    }

    /// "n/n" for the audit block.
    pub fn applier_exact(&self) -> String {
        format!("{}/{}", self.applier_ok, self.applier_total)
    }
}

impl std::fmt::Display for InvariantReport {
    /// The human form of the report: totals and invariants 1-4, one per line.
    ///
    /// Formatting, not printing — the engine still writes nothing. The
    /// endpoints are deliberately absent: they are not part of the report (nor
    /// of `--json`), so a caller that wants a range header prints its own.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let verdict = |ok: bool| if ok { "PASS" } else { "FAIL" };
        writeln!(
            f,
            "files      {} ({} binary, checked by oid only — tree assertion is tautological for those)",
            self.files_total, self.binary_oid_checked
        )?;
        writeln!(f, "hunks      {}", self.hunks_total)?;
        writeln!(
            f,
            "inv1 applier fidelity   {}  {}",
            self.applier_exact(),
            verdict(self.applier_mismatches.is_empty())
        )?;
        for m in &self.applier_mismatches {
            writeln!(f, "           mismatch: {m}")?;
        }
        writeln!(f, "inv2 hunk accounting    {}", verdict(self.accounting_ok))?;
        let Some(t) = &self.tree else {
            // Say it did not run. A blank here would read as a pass.
            writeln!(f, "inv3 tree assertion     NOT RUN")?;
            return write!(f, "inv4 independent recount NOT RUN");
        };
        writeln!(
            f,
            "inv3 tree assertion     {}  built {} head {}",
            verdict(t.tree_ok),
            t.built_tree.as_deref().unwrap_or("(not built)"),
            t.head_tree
        )?;
        writeln!(
            f,
            "inv4 independent recount {} of {}  {}",
            t.recount,
            self.hunks_total,
            verdict(t.recount_ok)
        )?;
        write!(
            f,
            "note: tree building writes unreferenced loose objects into the odb (gc-able)"
        )
    }
}

/// Invariants 1 and 2. **Read-only**: the bound list is one port, and that is
/// the whole proof that the core pipeline cannot write.
pub fn check_fidelity<G>(
    git: &G,
    base: &str,
    head: &str,
    view: &DiffView,
) -> Result<InvariantReport, EngineError>
where
    G: ObjectReader,
{
    // ---- Invariant 1: applier fidelity ------------------------------------
    let mut applier_total = 0usize;
    let mut applier_ok = 0usize;
    let mut mismatches = Vec::new();
    let mut binary_checked = 0usize;

    for f in &view.files {
        match fidelity(f) {
            Fidelity::Skip => continue,
            Fidelity::ByOid(oid) => {
                // The recorded object must exist in the odb.
                git.require_object(oid)?;
                binary_checked += 1;
                continue;
            }
            Fidelity::NoOid => {
                binary_checked += 1;
                continue;
            }
            Fidelity::Reconstruct => {}
        }
        applier_total += 1;
        let hunks: Vec<&Hunk> = f.hunks.iter().map(|&i| &view.hunks[i]).collect();
        let base_content = git.blob(base, &f.path)?;
        let got = crate::apply::apply_hunks(base_content.as_deref(), &hunks);
        let want = if f.disposition == Disposition::Deleted {
            Vec::new()
        } else {
            git.blob(head, &f.path)?.unwrap_or_default()
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
    let accounting_ok = check_accounting(view);

    Ok(InvariantReport {
        files_total: view.files.len(),
        applier_total,
        applier_ok,
        applier_mismatches: mismatches,
        binary_oid_checked: binary_checked,
        hunks_total: view.hunks.len(),
        accounting_ok,
        tree: None,
    })
}

/// Invariants 3 and 4. **Writes**: it builds a tree from the hunks, which puts
/// unreferenced loose objects in the odb.
///
/// `fidelity` is the report from `check_fidelity`, and it is read for one
/// reason: never build a tree on a broken applier, or the tree assertion is
/// made on top of a failure it cannot see (the prototype's rule).
pub fn check_tree<G>(
    git: &G,
    base: &str,
    head: &str,
    view: &DiffView,
    fidelity: &InvariantReport,
) -> Result<TreeReport, EngineError>
where
    G: ObjectReader + ObjectWriter + TreeResolver + TreeBuilder + RecountSource,
{
    let head_tree = git.tree_of(head)?;

    // ---- Invariant 3: non-tautological tree assertion ----------------------
    let (built_tree, tree_ok) = if may_build_tree(fidelity) {
        let built = build_tree(git, base, view)?;
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
            // Invariant 4's own port, never enumeration's: a change to one
            // cannot move both sides of this comparison.
            let out = git.recount_patch(base, t.as_str())?;
            let n = dumb_hunk_count(&out);
            (n, n == view.hunks.len())
        }
        None => (0, false),
    };

    Ok(TreeReport {
        built_tree,
        head_tree,
        tree_ok,
        recount,
        recount_ok,
    })
}

/// How invariant 1 verifies one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fidelity<'a> {
    /// Submodules: a gitlink has no content to reconstruct.
    Skip,
    /// Binary: no hunks exist, so the recorded oid is all there is to check.
    ByOid(&'a str),
    /// Binary with nothing recorded — counted, but there is nothing to assert.
    NoOid,
    /// Text: rebuild from base + hunks and compare byte for byte.
    Reconstruct,
}

fn fidelity(f: &crate::model::FileChange) -> Fidelity<'_> {
    if f.submodule.is_some() {
        return Fidelity::Skip;
    }
    if f.binary {
        return match f.new_oid.as_deref() {
            Some(oid) => Fidelity::ByOid(oid),
            None => Fidelity::NoOid,
        };
    }
    Fidelity::Reconstruct
}

/// Invariant 2, entire: every hunk belongs to exactly one file, that file
/// agrees, and the per-file lists sum to the canonical count.
///
/// Touches no git — it is a statement about the view's internal consistency,
/// which is why it can be exercised against a hand-built view.
fn check_accounting(view: &DiffView) -> bool {
    let mut seen = vec![false; view.hunks.len()];
    let mut ok = true;
    let mut carried = 0usize;
    for (fi, f) in view.files.iter().enumerate() {
        for &hi in &f.hunks {
            if hi >= seen.len() || seen[hi] || view.hunks[hi].file != fi {
                ok = false;
                continue;
            }
            seen[hi] = true;
            carried += 1;
        }
    }
    ok && carried == view.hunks.len()
}

/// Whether invariant 3 may run: never build a tree on a broken applier, or the
/// tree assertion is being made on top of a failure it cannot see.
///
/// Accounting is deliberately not consulted. Invariant 2 is about the view's
/// bookkeeping; the applier is what the tree is built from.
fn may_build_tree(fidelity: &InvariantReport) -> bool {
    fidelity.applier_mismatches.is_empty() && fidelity.applier_ok == fidelity.applier_total
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
    use super::{
        Fidelity, InvariantReport, TreeReport, check_accounting, dumb_hunk_count, fidelity,
        may_build_tree,
    };
    use crate::model::{DiffView, Disposition, FileChange, Hunk};

    fn hunk(file: usize) -> Hunk {
        Hunk {
            file,
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            removed: vec![],
            added: vec![],
            nonl_old: false,
            nonl_new: false,
        }
    }

    fn file(hunks: Vec<usize>) -> FileChange {
        FileChange {
            path: b"f".to_vec(),
            disposition: Disposition::Modified,
            new_mode: Some("100644".into()),
            old_mode: None,
            binary: false,
            submodule: None,
            old_oid: None,
            new_oid: None,
            hunks,
            rename_similarity: None,
            rename_from: None,
            rename_to: None,
            generated: None,
        }
    }

    /// Invariant 2 was 14 lines inlined in a function that needed a repository,
    /// so none of these cases had ever been asserted directly.
    #[test]
    fn accounting_holds_when_every_hunk_belongs_to_exactly_one_file() {
        let view = DiffView {
            files: vec![file(vec![0, 1]), file(vec![2])],
            hunks: vec![hunk(0), hunk(0), hunk(1)],
        };
        assert!(check_accounting(&view));
    }

    #[test]
    fn accounting_catches_a_hunk_claimed_twice() {
        let view = DiffView {
            files: vec![file(vec![0]), file(vec![0])],
            hunks: vec![hunk(0)],
        };
        assert!(!check_accounting(&view), "one hunk in two files");
    }

    #[test]
    fn accounting_catches_a_hunk_no_file_claims() {
        let view = DiffView {
            files: vec![file(vec![0])],
            hunks: vec![hunk(0), hunk(0)],
        };
        assert!(!check_accounting(&view), "h1 is carried by nothing");
    }

    #[test]
    fn accounting_catches_a_file_claiming_another_files_hunk() {
        // The disagreement that matters: the file lists it, the hunk denies it.
        let view = DiffView {
            files: vec![file(vec![0]), file(vec![1])],
            hunks: vec![hunk(0), hunk(0)],
        };
        assert!(!check_accounting(&view));
    }

    #[test]
    fn accounting_catches_an_out_of_range_index() {
        let view = DiffView {
            files: vec![file(vec![7])],
            hunks: vec![hunk(0)],
        };
        assert!(!check_accounting(&view));
    }

    #[test]
    fn binary_and_submodule_files_are_verified_differently_from_text() {
        let mut f = file(vec![]);
        assert_eq!(fidelity(&f), Fidelity::Reconstruct);

        f.binary = true;
        f.new_oid = Some("abc".into());
        assert_eq!(fidelity(&f), Fidelity::ByOid("abc"));

        f.new_oid = None;
        assert_eq!(fidelity(&f), Fidelity::NoOid);

        f.binary = false;
        f.submodule = Some((None, Some("s".into())));
        assert_eq!(fidelity(&f), Fidelity::Skip);
    }

    fn report(applier_total: usize, applier_ok: usize, mismatches: Vec<String>) -> InvariantReport {
        InvariantReport {
            files_total: applier_total,
            applier_total,
            applier_ok,
            applier_mismatches: mismatches,
            binary_oid_checked: 0,
            hunks_total: 0,
            accounting_ok: true,
            tree: None,
        }
    }

    /// The prototype's rule: a tree built on a broken applier would assert
    /// nothing, so invariant 3 does not run at all.
    #[test]
    fn a_broken_applier_stops_the_tree_from_being_built() {
        assert!(may_build_tree(&report(3, 3, vec![])));
        assert!(!may_build_tree(&report(3, 2, vec![])));
        assert!(!may_build_tree(&report(
            3,
            3,
            vec!["f: mismatch".to_string()]
        )));
    }

    /// `all_ok` must not read a missing tree half as a pass. This is the whole
    /// hazard the split introduces, so it is asserted directly.
    #[test]
    fn an_unverified_report_is_not_all_ok() {
        let r = report(3, 3, vec![]);
        assert!(r.fidelity_ok(), "invariants 1 and 2 passed");
        assert!(!r.all_ok(), "invariants 3 and 4 never ran");
    }

    #[test]
    fn a_verified_report_is_all_ok_only_when_both_halves_pass() {
        let mut r = report(3, 3, vec![]);
        r.tree = Some(TreeReport {
            built_tree: Some("t".into()),
            head_tree: "t".into(),
            tree_ok: true,
            recount: 0,
            recount_ok: true,
        });
        assert!(r.all_ok());
        r.tree.as_mut().unwrap().recount_ok = false;
        assert!(!r.all_ok(), "invariant 4 failed");
        assert!(r.fidelity_ok(), "but 1 and 2 still hold");
    }

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
