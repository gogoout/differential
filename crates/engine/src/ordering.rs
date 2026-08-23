//! The ordering stage: foundation-first arrangement of the close section.
//!
//! The measured failure this fixes: the group introducing the abstraction
//! everything else consumes landed 9th of 13 in model order, so the reviewer
//! met consumers before the thing they consume. Symbol-definition → symbol-use
//! is a poor collapse signal but a good ordering signal (ADR 0007): ordering
//! needs only a partial order, and a wrong edge misorders — it can never hide
//! content. Extraction is heuristic, per-language via the `Language` hooks
//! (ADR 0015); no indexer.
//!
//! Deterministic and model-free: runs unconditionally after grouping.

use std::collections::{HashMap, HashSet};

use differential_schema as schema;

use crate::lang::LanguageRegistry;
use crate::model::DiffView;

/// Reorder the close section foundation-first, fill `depends_on` and `role`,
/// rewrite `rank`, regroup the reading plan, and append the `order` stage.
pub fn apply(doc: &mut schema::PlanDocument, view: &DiffView, langs: &LanguageRegistry) {
    let Some(groups) = doc.groups.take() else {
        return;
    };

    // --- symbol sets per group (added lines only: what the change introduces
    // and what the changed code now calls) ------------------------------------
    let hunks_of_class: HashMap<&str, &schema::ClassEntry> =
        doc.classes.iter().map(|c| (c.id.as_str(), c)).collect();
    let hunk_index: HashMap<&str, usize> = doc
        .hunks
        .iter()
        .enumerate()
        .map(|(i, h)| (h.id.as_str(), i))
        .collect();

    let mut defs: Vec<HashSet<Vec<u8>>> = Vec::with_capacity(groups.len());
    let mut refs: Vec<HashSet<Vec<u8>>> = Vec::with_capacity(groups.len());
    for g in &groups {
        let mut d = HashSet::new();
        let mut r = HashSet::new();
        if g.effort != schema::Effort::Noise {
            for cid in &g.class_ids {
                for hid in &hunks_of_class[cid.as_str()].hunk_ids {
                    let h = &view.hunks[hunk_index[hid.as_str()]];
                    let lang = langs.detect(&view.file_of(h).path);
                    for line in &h.added {
                        d.extend(lang.symbol_definitions(line));
                        r.extend(lang.symbol_references(line));
                    }
                }
            }
        }
        defs.push(d);
        refs.push(r);
    }

    // Only symbols defined by exactly ONE group create edges; multi-definer
    // symbols (common names, repeated decls) are noise.
    let mut definer: HashMap<&[u8], Option<usize>> = HashMap::new();
    for (gi, d) in defs.iter().enumerate() {
        for sym in d {
            definer
                .entry(sym.as_slice())
                .and_modify(|e| *e = None)
                .or_insert(Some(gi));
        }
    }

    // --- depends_on edges -----------------------------------------------------
    let mut deps: Vec<HashSet<usize>> = vec![HashSet::new(); groups.len()];
    for (gi, r) in refs.iter().enumerate() {
        for sym in r {
            if let Some(&Some(def_gi)) = definer.get(sym.as_slice())
                && def_gi != gi
            {
                deps[gi].insert(def_gi);
            }
        }
    }

    // --- foundation-first reorder of the contiguous close prefix ---------------
    // The audit back-fill group is always assembled last and must stay trailing
    // (untriaged classes are read at the end); when every group is close it
    // would otherwise fall inside the prefix.
    let mut close_len = groups
        .iter()
        .position(|g| g.effort != schema::Effort::Close)
        .unwrap_or(groups.len());
    if close_len == groups.len() && doc.audit.classes_missing.unwrap_or(0) > 0 {
        close_len -= 1;
    }
    let hunk_count = |gi: usize| -> usize {
        groups[gi]
            .class_ids
            .iter()
            .map(|c| hunks_of_class[c.as_str()].hunk_ids.len())
            .sum()
    };
    let order = toposort_prefix(close_len, &deps, &hunk_count);

    let mut new_order: Vec<usize> = order;
    new_order.extend(close_len..groups.len());

    // --- roles ------------------------------------------------------------------
    let depended_on: HashSet<usize> = deps.iter().flatten().copied().collect();
    let role_of = |gi: usize| -> Option<schema::Role> {
        let g = &groups[gi];
        match g.effort {
            schema::Effort::Noise => g.role, // set by grouping
            schema::Effort::Skim => Some(schema::Role::Mechanical),
            schema::Effort::Close => {
                if depended_on.contains(&gi) {
                    Some(schema::Role::Foundation)
                } else if !deps[gi].is_empty() {
                    Some(schema::Role::Consumer)
                } else {
                    None
                }
            }
        }
    };

    // --- rebuild groups in the new order ----------------------------------------
    let mut reordered: Vec<schema::Group> = Vec::with_capacity(groups.len());
    for (rank, &gi) in new_order.iter().enumerate() {
        let mut g = groups[gi].clone();
        g.rank = rank as u32;
        g.role = role_of(gi);
        let mut edges: Vec<String> = deps[gi].iter().map(|&d| groups[d].id.clone()).collect();
        edges.sort_unstable();
        g.depends_on = edges;
        reordered.push(g);
    }

    // Reading plan: stable regroup of the existing steps by the new group order.
    if let Some(plan) = doc.reading_plan.take() {
        let mut by_group: HashMap<&str, Vec<schema::ReadingStep>> = HashMap::new();
        for step in &plan {
            by_group
                .entry(step.group.as_str())
                .or_default()
                .push(step.clone());
        }
        doc.reading_plan = Some(
            reordered
                .iter()
                .flat_map(|g| by_group.remove(g.id.as_str()).unwrap_or_default())
                .collect(),
        );
    }

    doc.groups = Some(reordered);
    doc.generator.stages.push("order".to_string());
}

/// Kahn's algorithm over the close prefix. Ready-node tie-break: descending
/// hunk count, then original position (stable). Cycle fallback: emit the
/// largest remaining node — deterministic, and the recorded `depends_on` still
/// carries the true edges.
fn toposort_prefix(
    len: usize,
    deps: &[HashSet<usize>],
    hunk_count: &dyn Fn(usize) -> usize,
) -> Vec<usize> {
    let mut remaining: Vec<usize> = (0..len).collect();
    let mut emitted: HashSet<usize> = HashSet::new();
    let mut out = Vec::with_capacity(len);

    while !remaining.is_empty() {
        let ready: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&gi| deps[gi].iter().all(|d| *d >= len || emitted.contains(d)))
            .collect();
        // No ready node means a cycle: break it on the biggest remaining node.
        let pool = if ready.is_empty() { &remaining } else { &ready };
        let chosen = pool
            .iter()
            .copied()
            .max_by_key(|&gi| (hunk_count(gi), usize::MAX - gi))
            .expect("pool is non-empty");

        remaining.retain(|&gi| gi != chosen);
        emitted.insert(chosen);
        out.push(chosen);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[usize]) -> HashSet<usize> {
        v.iter().copied().collect()
    }

    #[test]
    fn foundation_precedes_consumer() {
        // 1 depends on 0; equal sizes → 0 first regardless of position.
        let deps = vec![set(&[]), set(&[0])];
        let counts = [5usize, 5];
        let order = toposort_prefix(2, &deps, &|i| counts[i]);
        assert_eq!(order, vec![0, 1]);

        let deps = vec![set(&[1]), set(&[])];
        let order = toposort_prefix(2, &deps, &|i| counts[i]);
        assert_eq!(order, vec![1, 0]);
    }

    #[test]
    fn ties_break_by_descending_hunk_count_then_position() {
        let deps = vec![set(&[]), set(&[]), set(&[])];
        let counts = [3usize, 9, 9];
        let order = toposort_prefix(3, &deps, &|i| counts[i]);
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn cycle_falls_back_deterministically() {
        // 0 <-> 1 cycle plus independent 2.
        let deps = vec![set(&[1]), set(&[0]), set(&[])];
        let counts = [2usize, 8, 1];
        let order = toposort_prefix(3, &deps, &|i| counts[i]);
        // 2 is ready (largest ready is 2? counts: only 2 is ready with count 1).
        assert_eq!(order[0], 2);
        // Cycle broken on the larger node.
        assert_eq!(order[1], 1);
        assert_eq!(order[2], 0);
    }

    #[test]
    fn edges_outside_the_prefix_do_not_block() {
        // Group 0 depends on group 3 (a skim group outside the close prefix).
        let deps = vec![set(&[3]), set(&[]), set(&[]), set(&[])];
        let counts = [4usize, 2, 1, 9];
        let order = toposort_prefix(3, &deps, &|i| counts[i]);
        assert_eq!(order, vec![0, 1, 2]);
    }
}
