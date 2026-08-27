//! Turn audited work groups into the document's `groups`, `reading_plan` and
//! grouping-audit fields. Ordering here is presentation only (focus → skim →
//! noise → back-fill); the foundation-first sort is the ordering stage's job.

use std::collections::HashMap;

use crate::schema;

use super::{Audited, ClassInfo, WorkGroup};

pub fn assemble(
    doc: &schema::PlanDocument,
    infos: &[ClassInfo],
    noise: &[&ClassInfo],
    audited: Audited,
) -> schema::PlanDocument {
    let by_id: HashMap<&str, &ClassInfo> = infos.iter().map(|c| (c.id.as_str(), c)).collect();
    let hunks_of =
        |g: &WorkGroup| -> usize { g.class_ids.iter().map(|c| by_id[c.as_str()].n_hunks).sum() };

    // focus (model order, gate group last) → skim (desc hunks) → noise → back-fill.
    let mut focus: Vec<&WorkGroup> = Vec::new();
    let mut skim: Vec<&WorkGroup> = Vec::new();
    let mut backfill: Vec<&WorkGroup> = Vec::new();
    for g in &audited.groups {
        if g.backfill {
            backfill.push(g);
        } else if g.skim {
            skim.push(g);
        } else {
            focus.push(g);
        }
    }
    skim.sort_by_key(|g| usize::MAX - hunks_of(g));

    let mut groups: Vec<schema::Group> = Vec::new();
    let mut plan: Vec<schema::ReadingStep> = Vec::new();
    let mut read_hunks = 0usize;
    let mut skipped_hunks = 0usize;

    let mut push = |g: &WorkGroup,
                    effort: schema::Effort,
                    groups: &mut Vec<schema::Group>,
                    plan: &mut Vec<schema::ReadingStep>| {
        let id = format!("g{}", groups.len());
        let n_hunks = hunks_of(g);
        let n_classes = g.class_ids.len();
        match effort {
            schema::Effort::Focus => {
                read_hunks += n_hunks;
                plan.push(step(&id, schema::ReadAction::Read));
            }
            schema::Effort::Skim => {
                // Exemplars still get read — one per shape class. Only the
                // remainder is the genuine saving (ADR 0006).
                read_hunks += n_classes;
                skipped_hunks += n_hunks - n_classes;
                plan.push(step(&id, schema::ReadAction::Exemplars));
                if n_hunks > n_classes {
                    plan.push(step(&id, schema::ReadAction::Skip));
                }
            }
            schema::Effort::Noise => {
                skipped_hunks += n_hunks;
                plan.push(step(&id, schema::ReadAction::Fold));
            }
        }
        groups.push(schema::Group {
            id,
            label: g.label.clone(),
            description: g.description.clone(),
            reason: g.reason.clone(),
            effort,
            role: if effort == schema::Effort::Noise {
                Some(schema::Role::Noise)
            } else {
                None
            },
            class_ids: g.class_ids.clone(),
            depends_on: Vec::new(),
            rank: groups.len() as u32,
        });
    };

    for g in focus {
        push(g, schema::Effort::Focus, &mut groups, &mut plan);
    }
    for g in skim {
        push(g, schema::Effort::Skim, &mut groups, &mut plan);
    }
    if !noise.is_empty() {
        let noise_group = WorkGroup {
            label: "Generated files".to_string(),
            description: "Generated content (lockfiles, snapshots, build artefacts): folded, \
                          verified by provenance rather than read."
                .to_string(),
            reason: "Every hunk lives in a file marked generated (builtin list, \
                     gitattributes, or repo config)."
                .to_string(),
            skim: false,
            class_ids: noise.iter().map(|c| c.id.clone()).collect(),
            backfill: false,
        };
        push(&noise_group, schema::Effort::Noise, &mut groups, &mut plan);
    }
    for g in backfill {
        push(g, schema::Effort::Focus, &mut groups, &mut plan);
    }

    let mut out = doc.clone();
    out.generator.stages.push("group".to_string());
    out.groups = Some(groups);
    out.reading_plan = Some(plan);
    out.audit.coverage = Some(audited.coverage);
    out.audit.classes_missing = Some(audited.missing.len() as u32);
    out.audit.classes_duplicated = Some(audited.dupes);
    out.audit.classes_hallucinated = Some(audited.halluc);
    out.audit.read_hunks = Some(read_hunks as u32);
    out.audit.skipped_hunks = Some(skipped_hunks as u32);
    out
}

fn step(group: &str, action: schema::ReadAction) -> schema::ReadingStep {
    schema::ReadingStep {
        group: group.to_string(),
        action,
    }
}
