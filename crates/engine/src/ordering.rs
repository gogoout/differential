//! The ordering stage: foundation-first arrangement of the focus section.
//!
//! The measured failure this fixes: the group introducing the abstraction
//! everything else consumes landed 9th of 13 in model order, so the reviewer
//! met consumers before the thing they consume.
//!
//! It does not compute the dependency graph. `artefact::graph` builds that
//! from classes, before the model runs, and this stage contracts it onto
//! groups. That split matters: the stage used to union symbols across a whole
//! group before computing a single edge, which threw away every distinction
//! inside a group and let a cycle appear where the classes had none.
//!
//! Deterministic and model-free: runs unconditionally after grouping.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use petgraph::algo::is_cyclic_directed;
use petgraph::graph::{DiGraph, NodeIndex};

use crate::schema;

/// Reorder the focus section foundation-first, fill `depends_on`, `role` and
/// `pivot`, order each group's classes, rewrite `rank`, regroup the reading
/// plan, and append the `order` stage.
pub fn apply(doc: &mut schema::PlanDocument) {
    let Some(groups) = doc.groups.take() else {
        return;
    };

    let class_index: HashMap<&str, usize> = doc
        .classes
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    let n_classes = doc.classes.len();

    // Which group owns each class. Noise groups are excluded from the graph
    // entirely, exactly as they were before: generated content orders nothing.
    let mut group_of_class: Vec<Option<usize>> = vec![None; n_classes];
    for (gi, g) in groups.iter().enumerate() {
        if g.effort == schema::Effort::Noise {
            continue;
        }
        for cid in &g.class_ids {
            if let Some(&ci) = class_index.get(cid.as_str()) {
                group_of_class[ci] = Some(gi);
            }
        }
    }

    // --- the class edges, kept whole and contracted onto groups -------------
    // `class_deps` keeps the intra-group edges the group graph cannot express.
    // They are what orders the classes inside a group, and what tells a group
    // cycle apart from a real one.
    let mut class_deps: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n_classes];
    let mut group_edges: Vec<BTreeMap<usize, BTreeSet<&str>>> = vec![BTreeMap::new(); groups.len()];
    for (ci, c) in doc.classes.iter().enumerate() {
        let Some(gi) = group_of_class[ci] else {
            continue;
        };
        for e in &c.depends_on {
            let Some(&target) = class_index.get(e.on.as_str()) else {
                continue;
            };
            let Some(gj) = group_of_class[target] else {
                continue;
            };
            class_deps[ci].insert(target);
            if gj != gi {
                group_edges[gi]
                    .entry(gj)
                    .or_default()
                    .extend(e.via.iter().map(String::as_str));
            }
        }
    }
    let deps: Vec<HashSet<usize>> = group_edges
        .iter()
        .map(|m| m.keys().copied().collect())
        .collect();

    // --- classes inside each group, foundation-first ------------------------
    let class_order: Vec<Vec<String>> = groups
        .iter()
        .map(|g| order_classes(g, &class_index, &class_deps, doc))
        .collect();

    // --- foundation-first reorder of the contiguous focus prefix -------------
    // The audit back-fill group is always assembled last and must stay trailing
    // (untriaged classes are read at the end); when every group is focus it
    // would otherwise fall inside the prefix.
    let mut focus_len = groups
        .iter()
        .position(|g| g.effort != schema::Effort::Focus)
        .unwrap_or(groups.len());
    if focus_len == groups.len() && doc.audit.classes_missing.unwrap_or(0) > 0 {
        focus_len -= 1;
    }
    let classes_of = |gi: usize| -> Vec<usize> {
        groups[gi]
            .class_ids
            .iter()
            .filter_map(|c| class_index.get(c.as_str()).copied())
            .collect()
    };
    let hunk_count = |gi: usize| -> usize {
        classes_of(gi)
            .iter()
            .map(|&ci| doc.classes[ci].hunk_ids.len())
            .sum()
    };
    let sorted = toposort_prefix(focus_len, &deps, &hunk_count, &|remaining| {
        break_cycle(remaining, &classes_of, &class_deps)
    });

    let mut new_order: Vec<usize> = sorted.order;
    new_order.extend(focus_len..groups.len());
    let rank_of: HashMap<usize, usize> = new_order
        .iter()
        .enumerate()
        .map(|(rank, &gi)| (gi, rank))
        .collect();

    // --- roles ------------------------------------------------------------------
    let depended_on: HashSet<usize> = deps.iter().flatten().copied().collect();
    let role_of = |gi: usize| -> Option<schema::Role> {
        let g = &groups[gi];
        match g.effort {
            schema::Effort::Noise => g.role, // set by grouping
            schema::Effort::Skim => Some(schema::Role::Mechanical),
            schema::Effort::Focus => {
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
        g.class_ids = class_order[gi].clone();
        g.depends_on = group_edges[gi]
            .iter()
            .map(|(&target, via)| schema::Edge {
                on: groups[target].id.clone(),
                via: via.iter().map(|s| (*s).to_string()).collect(),
                // Only an edge the sort could not honour carries a verdict, and
                // only the sort knows which those were.
                cycle: sorted.broken.get(&(gi, target)).copied(),
            })
            .collect();
        g.pivot = sorted.broken.keys().any(|&(from, _)| from == gi).then(|| {
            pivot(
                &g.class_ids,
                &class_index,
                &class_deps,
                &group_of_class,
                &rank_of,
                rank,
            )
        });
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

/// A group's classes, foundation-first. Ties break by descending member count,
/// then original position — the same rule the group sort uses.
///
/// This is the information `def_gi != gi` used to discard. An edge between two
/// classes of one group said nothing at group level, so it was dropped; here it
/// is the only thing that can order them.
fn order_classes(
    group: &schema::Group,
    class_index: &HashMap<&str, usize>,
    class_deps: &[BTreeSet<usize>],
    doc: &schema::PlanDocument,
) -> Vec<String> {
    let members: Vec<usize> = group
        .class_ids
        .iter()
        .filter_map(|c| class_index.get(c.as_str()).copied())
        .collect();
    if members.len() < 2 {
        return group.class_ids.clone();
    }
    let inside: HashSet<usize> = members.iter().copied().collect();
    let deps: HashMap<usize, HashSet<usize>> = members
        .iter()
        .map(|&ci| {
            (
                ci,
                class_deps[ci]
                    .iter()
                    .copied()
                    .filter(|d| inside.contains(d))
                    .collect(),
            )
        })
        .collect();

    let mut remaining = members.clone();
    let mut emitted: HashSet<usize> = HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(members.len());
    while !remaining.is_empty() {
        let ready: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|ci| deps[ci].iter().all(|d| emitted.contains(d)))
            .collect();
        let pool = if ready.is_empty() { &remaining } else { &ready };
        let chosen = *pool
            .iter()
            .max_by_key(|&&ci| {
                (
                    doc.classes[ci].hunk_ids.len(),
                    usize::MAX - members.iter().position(|&m| m == ci).unwrap_or(0),
                )
            })
            .expect("pool is non-empty");
        remaining.retain(|&ci| ci != chosen);
        emitted.insert(chosen);
        out.push(doc.classes[chosen].id.clone());
    }
    out
}

/// How many leading classes depend on nothing ranked later.
///
/// The index where the group stops being a foundation and starts being a
/// consumer. Nothing acts on it: the group is never split (ADR 0022).
fn pivot(
    class_ids: &[String],
    class_index: &HashMap<&str, usize>,
    class_deps: &[BTreeSet<usize>],
    group_of_class: &[Option<usize>],
    rank_of: &HashMap<usize, usize>,
    own_rank: usize,
) -> u32 {
    let mut n = 0u32;
    for cid in class_ids {
        let Some(&ci) = class_index.get(cid.as_str()) else {
            break;
        };
        let looks_later = class_deps[ci].iter().any(|&d| {
            group_of_class[d]
                .and_then(|g| rank_of.get(&g))
                .is_some_and(|&r| r > own_rank)
        });
        if looks_later {
            break;
        }
        n += 1;
    }
    n
}

/// Which group to emit when nothing is ready, and why the cycle exists.
///
/// The class graph is finer than the group graph, and contracting a directed
/// acyclic graph can create cycles. So when the groups deadlock, ask the
/// classes: if they are acyclic here, the deadlock is an artefact of grouping
/// and their order is the right one to follow. If they deadlock too, the mutual
/// dependency is in the change and the old size-based fallback is as good an
/// answer as there is.
fn break_cycle(
    remaining: &[usize],
    classes_of: &dyn Fn(usize) -> Vec<usize>,
    class_deps: &[BTreeSet<usize>],
) -> (Option<usize>, schema::Cycle) {
    let mut owner: HashMap<usize, usize> = HashMap::new();
    for &gi in remaining {
        for ci in classes_of(gi) {
            owner.insert(ci, gi);
        }
    }
    let inside: HashSet<usize> = owner.keys().copied().collect();
    let mut ids: Vec<usize> = inside.iter().copied().collect();
    ids.sort_unstable();

    // Do the classes deadlock too? This was a Kahn walk that rescanned every
    // remaining class on every step to find out — O(n^2) to answer a yes/no
    // question about a graph. `is_cyclic_directed` is the same answer: a Kahn
    // walk runs out of ready nodes exactly when a cycle is left.
    let mut graph = DiGraph::<(), ()>::new();
    let nodes: HashMap<usize, NodeIndex> = ids.iter().map(|&ci| (ci, graph.add_node(()))).collect();
    for &ci in &ids {
        for d in &class_deps[ci] {
            if let Some(&to) = nodes.get(d) {
                graph.add_edge(nodes[&ci], to, ());
            }
        }
    }
    if is_cyclic_directed(&graph) {
        // A real mutual dependency, in the change rather than in the grouping.
        return (None, schema::Cycle::Mutual);
    }

    // Which group to emit: the owner of the lowest-numbered class nothing in
    // play blocks. The walk assigned this on its FIRST step and never again,
    // so every later step only ever decided the verdict above. Ties by class
    // index, which is descending member count already.
    let first = ids
        .iter()
        .copied()
        .find(|&ci| class_deps[ci].iter().all(|d| !inside.contains(d)));
    (first.map(|ci| owner[&ci]), schema::Cycle::Artefact)
}

/// What to do when the group sort deadlocks: which group to emit first, and why
/// the cycle exists at all.
type CycleBreaker<'a> = dyn Fn(&[usize]) -> (Option<usize>, schema::Cycle) + 'a;

struct Sorted {
    order: Vec<usize>,
    /// Edges the sort could not honour, and why. Keyed `(from, to)` by group
    /// position in the pre-sort order.
    broken: HashMap<(usize, usize), schema::Cycle>,
}

/// Kahn's algorithm over the focus prefix. Ready-node tie-break: descending
/// hunk count, then original position (stable). On a deadlock, `resolve` picks
/// the node and says why the cycle exists; the edges that node could not
/// honour are recorded with that verdict.
fn toposort_prefix(
    len: usize,
    deps: &[HashSet<usize>],
    hunk_count: &dyn Fn(usize) -> usize,
    resolve: &CycleBreaker<'_>,
) -> Sorted {
    let mut remaining: Vec<usize> = (0..len).collect();
    let mut emitted: HashSet<usize> = HashSet::new();
    let mut out = Vec::with_capacity(len);
    let mut broken: HashMap<(usize, usize), schema::Cycle> = HashMap::new();

    while !remaining.is_empty() {
        let ready: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&gi| deps[gi].iter().all(|d| *d >= len || emitted.contains(d)))
            .collect();
        let chosen = if let Some(&gi) = ready
            .iter()
            .max_by_key(|&&gi| (hunk_count(gi), usize::MAX - gi))
        {
            gi
        } else {
            // No ready node means a cycle. The class graph decides which group
            // goes first and what kind of cycle this is; a size-based pick is
            // the fallback when even the classes deadlock.
            let (pick, why) = resolve(&remaining);
            let gi = pick
                .filter(|gi| remaining.contains(gi))
                .or_else(|| {
                    remaining
                        .iter()
                        .copied()
                        .max_by_key(|&gi| (hunk_count(gi), usize::MAX - gi))
                })
                .expect("remaining is non-empty");
            for &d in &deps[gi] {
                if d < len && !emitted.contains(&d) {
                    broken.insert((gi, d), why);
                }
            }
            gi
        };

        remaining.retain(|&gi| gi != chosen);
        emitted.insert(chosen);
        out.push(chosen);
    }
    Sorted { order: out, broken }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[usize]) -> HashSet<usize> {
        v.iter().copied().collect()
    }

    /// The classes deadlock too, so every test that is only about the group
    /// sort gets the old size-based fallback.
    fn mutual(_: &[usize]) -> (Option<usize>, schema::Cycle) {
        (None, schema::Cycle::Mutual)
    }

    #[test]
    fn foundation_precedes_consumer() {
        // 1 depends on 0; equal sizes → 0 first regardless of position.
        let deps = vec![set(&[]), set(&[0])];
        let counts = [5usize, 5];
        let s = toposort_prefix(2, &deps, &|i| counts[i], &mutual);
        assert_eq!(s.order, vec![0, 1]);
        assert!(s.broken.is_empty());

        let deps = vec![set(&[1]), set(&[])];
        let s = toposort_prefix(2, &deps, &|i| counts[i], &mutual);
        assert_eq!(s.order, vec![1, 0]);
    }

    #[test]
    fn ties_break_by_descending_hunk_count_then_position() {
        let deps = vec![set(&[]), set(&[]), set(&[])];
        let counts = [3usize, 9, 9];
        let s = toposort_prefix(3, &deps, &|i| counts[i], &mutual);
        assert_eq!(s.order, vec![1, 2, 0]);
    }

    #[test]
    fn edges_outside_the_prefix_do_not_block() {
        // Group 0 depends on group 3 (a skim group outside the focus prefix).
        let deps = vec![set(&[3]), set(&[]), set(&[]), set(&[])];
        let counts = [4usize, 2, 1, 9];
        let s = toposort_prefix(3, &deps, &|i| counts[i], &mutual);
        assert_eq!(s.order, vec![0, 1, 2]);
        assert!(s.broken.is_empty(), "an edge it never had to honour");
    }

    #[test]
    fn a_mutual_cycle_falls_back_to_size_and_says_so() {
        // 0 <-> 1 cycle plus independent 2.
        let deps = vec![set(&[1]), set(&[0]), set(&[])];
        let counts = [2usize, 8, 1];
        let s = toposort_prefix(3, &deps, &|i| counts[i], &mutual);
        assert_eq!(s.order[0], 2, "the only ready node goes first");
        assert_eq!(s.order[1], 1, "cycle broken on the larger node");
        assert_eq!(s.order[2], 0);
        assert_eq!(s.broken.get(&(1, 0)), Some(&schema::Cycle::Mutual));
        assert_eq!(s.broken.len(), 1, "only the edge it could not honour");
    }

    #[test]
    fn an_artefact_cycle_follows_the_classes_instead_of_size() {
        // The classes say group 0 first; size says group 1. The classes win,
        // which is the whole point: contracting them is what made the cycle.
        let deps = vec![set(&[1]), set(&[0])];
        let counts = [2usize, 8];
        let s = toposort_prefix(2, &deps, &|i| counts[i], &|_| {
            (Some(0), schema::Cycle::Artefact)
        });
        assert_eq!(s.order, vec![0, 1]);
        assert_eq!(s.broken.get(&(0, 1)), Some(&schema::Cycle::Artefact));
    }

    #[test]
    fn a_pick_outside_the_remaining_set_is_ignored() {
        // Defensive: the verdict is still used, the pick is not.
        let deps = vec![set(&[1]), set(&[0])];
        let counts = [2usize, 8];
        let s = toposort_prefix(2, &deps, &|i| counts[i], &|_| {
            (Some(99), schema::Cycle::Artefact)
        });
        assert_eq!(s.order, vec![1, 0], "falls back to size");
    }

    #[test]
    fn classes_deadlocking_is_a_mutual_cycle() {
        // c0 and c1 need each other; they sit in groups 0 and 1.
        let class_deps = vec![set2(&[1]), set2(&[0])];
        let classes_of = |gi: usize| vec![gi];
        let (pick, why) = break_cycle(&[0, 1], &classes_of, &class_deps);
        assert_eq!(why, schema::Cycle::Mutual);
        assert!(pick.is_none(), "no honest order to offer");
    }

    #[test]
    fn acyclic_classes_name_the_group_to_read_first() {
        // c0 defines, c1 uses it. Group 1 owns c0, group 0 owns c1: the group
        // graph is a cycle only because group 0 also holds c2, which c0 uses.
        let class_deps = vec![set2(&[]), set2(&[0]), set2(&[])];
        let classes_of = |gi: usize| if gi == 0 { vec![1, 2] } else { vec![0] };
        let (pick, why) = break_cycle(&[0, 1], &classes_of, &class_deps);
        assert_eq!(why, schema::Cycle::Artefact);
        assert_eq!(pick, Some(1), "the group holding the earliest class");
    }

    fn set2(v: &[usize]) -> BTreeSet<usize> {
        v.iter().copied().collect()
    }
}
