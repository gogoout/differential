//! Grouping-stage tests: real temp repos, fake LLM backend. No model runs.

mod common;

use common::{FakeBackend, TestRepo, grouped, grouped_with_cache, ids_in_prompt, json_group};
use differential_engine::schema::{Effort, PlanDocument, ReadAction};

/// Standard fixture: one 3-hunk rename-shaped class + one behavioural class.
fn two_class_repo() -> (TestRepo, String, String) {
    let r = TestRepo::new();
    for name in ["a", "b", "c"] {
        r.write(
            &format!("src/{name}.txt"),
            b"use old_helper_name;\nother content\n",
        );
    }
    r.write("src/main.txt", b"fn main() { run_slowly() }\n");
    let base = r.commit_all("base");
    for name in ["a", "b", "c"] {
        r.write(
            &format!("src/{name}.txt"),
            b"use new_helper_name;\nother content\n",
        );
    }
    r.write("src/main.txt", b"fn main() { run_with_retries(3) }\n");
    let head = r.commit_all("head");
    (r, base, head)
}

#[test]
fn happy_path_fills_groups_plan_and_audit() {
    let (r, base, head) = two_class_repo();
    // C0 = the 3-member class (largest first), C1 = the singleton.
    let backend = FakeBackend::new("fake", |ids| {
        assert_eq!(ids.len(), 2);
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("Behaviour change", "close", &[&ids[1]]),
            json_group("Helper rename", "skim", &[&ids[0]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);

    assert_eq!(
        d.generator.stages,
        ["enumerate", "classify", "group", "order"]
    );
    let groups = d.groups.as_ref().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].effort, Effort::Close);
    assert_eq!(groups[0].label, "Behaviour change");
    assert_eq!(groups[0].rank, 0);
    assert_eq!(groups[0].role, None);
    assert_eq!(groups[1].effort, Effort::Skim);

    let plan = d.reading_plan.as_ref().unwrap();
    let actions: Vec<ReadAction> = plan.iter().map(|s| s.action).collect();
    assert_eq!(
        actions,
        [ReadAction::Read, ReadAction::Exemplars, ReadAction::Skip]
    );

    assert_eq!(d.audit.coverage, Some(1.0));
    assert_eq!(d.audit.classes_missing, Some(0));
    assert_eq!(d.audit.classes_duplicated.as_deref(), Some(&[][..]));
    assert_eq!(d.audit.classes_hallucinated.as_deref(), Some(&[][..]));
    // 1 close hunk + 1 skim exemplar read; 2 skim remainders skipped.
    assert_eq!(d.audit.read_hunks, Some(2));
    assert_eq!(d.audit.skipped_hunks, Some(2));

    // The grouped document round-trips through the frozen schema.
    let re = PlanDocument::from_json(&d.to_json_pretty().unwrap()).unwrap();
    assert_eq!(re, d);
}

#[test]
fn omitted_class_is_backfilled_never_dropped() {
    let (r, base, head) = two_class_repo();
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Only one", "close", &[&ids[1]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);

    let groups = d.groups.as_ref().unwrap();
    assert_eq!(groups.len(), 2);
    let backfill = groups.last().unwrap();
    assert_eq!(backfill.effort, Effort::Close);
    assert!(backfill.label.contains("no group"));
    assert_eq!(d.audit.classes_missing, Some(1));
    // 3 of 4 hunks unassigned by the model.
    assert_eq!(d.audit.coverage, Some(0.25));

    // Invariant 2 over groups: every class in exactly one group.
    let mut all: Vec<&str> = groups
        .iter()
        .flat_map(|g| g.class_ids.iter().map(String::as_str))
        .collect();
    all.sort_unstable();
    let mut expect: Vec<&str> = d.classes.iter().map(|c| c.id.as_str()).collect();
    expect.sort_unstable();
    assert_eq!(all, expect);
    // Everything must still be read: back-fill is close.
    assert_eq!(d.audit.read_hunks, Some(4));
    assert_eq!(d.audit.skipped_hunks, Some(0));
}

#[test]
fn duplicated_class_kept_by_first_group_only() {
    let (r, base, head) = two_class_repo();
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("First", "close", &[&ids[0], &ids[1]]),
            json_group("Second", "close", &[&ids[0]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    let groups = d.groups.as_ref().unwrap();
    // Second group became empty after dedup and was dropped.
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].class_ids.len(), 2);
    assert_eq!(d.audit.classes_duplicated.as_ref().unwrap().len(), 1);
    assert_eq!(d.audit.coverage, Some(1.0));
}

#[test]
fn hallucinated_class_is_dropped_and_recorded() {
    let (r, base, head) = two_class_repo();
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("All", "close", &[&ids[0], &ids[1], "C999"])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    assert_eq!(
        d.audit.classes_hallucinated.as_deref(),
        Some(&["C999".to_string()][..])
    );
    let groups = d.groups.as_ref().unwrap();
    assert!(
        groups
            .iter()
            .all(|g| !g.class_ids.contains(&"C999".to_string()))
    );
}

#[test]
fn unknown_effort_defaults_to_close() {
    let (r, base, head) = two_class_repo();
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("A", "medium-ish", &[&ids[0]]),
            json_group("B", "close", &[&ids[1]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    assert!(
        d.groups
            .as_ref()
            .unwrap()
            .iter()
            .all(|g| g.effort == Effort::Close)
    );
}

#[test]
fn generated_classes_never_reach_the_model_and_fold_as_noise() {
    let r = TestRepo::new();
    r.write("Cargo.lock", b"version = 1\n");
    r.write("src/lib.txt", b"real code here\n");
    let base = r.commit_all("base");
    r.write("Cargo.lock", b"version = 2\n");
    r.write("src/lib.txt", b"real code changed\n");
    let head = r.commit_all("head");

    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Code", "close", &[&ids[0]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);

    // Only the non-generated class was offered.
    assert_eq!(ids_in_prompt(&backend.last_prompt()).len(), 1);

    let groups = d.groups.as_ref().unwrap();
    let noise = groups.iter().find(|g| g.effort == Effort::Noise).unwrap();
    assert_eq!(noise.role, Some(differential_engine::schema::Role::Noise));
    let plan = d.reading_plan.as_ref().unwrap();
    assert!(
        plan.iter()
            .any(|s| s.group == noise.id && s.action == ReadAction::Fold)
    );
    assert_eq!(d.audit.skipped_hunks, Some(1)); // the lockfile hunk
    assert_eq!(d.audit.coverage, Some(1.0)); // offered classes fully assigned
}

#[test]
fn mixed_generated_class_stays_with_the_model() {
    let r = TestRepo::new();
    // The same shaped edit in a generated and a source file: one class,
    // not all-generated, so it must be offered.
    r.write("Cargo.lock", b"shared_edit_line = old_value\n");
    r.write("src/x.txt", b"shared_edit_line = old_value\n");
    let base = r.commit_all("base");
    r.write("Cargo.lock", b"shared_edit_line = new_value\n");
    r.write("src/x.txt", b"shared_edit_line = new_value\n");
    let head = r.commit_all("head");

    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Edit", "close", &[&ids[0]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    assert_eq!(d.classes.len(), 1);
    assert_eq!(ids_in_prompt(&backend.last_prompt()).len(), 1);
    assert!(
        d.groups
            .as_ref()
            .unwrap()
            .iter()
            .all(|g| g.effort != Effort::Noise)
    );
}

#[test]
fn low_similarity_rename_is_extracted_from_skim() {
    let r = TestRepo::new();
    let mut body = String::new();
    for i in 0..40 {
        body.push_str(&format!("shared_line_number_{i} = value_{i}\n"));
    }
    r.write("mod/original.txt", body.as_bytes());
    let base = r.commit_all("base");
    std::fs::remove_file(r.root.join("mod/original.txt")).unwrap();
    let mut edited = String::new();
    for i in 0..40 {
        if i % 4 == 0 {
            edited.push_str(&format!("rewritten_entry_{i} -> different({i})\n"));
        } else {
            edited.push_str(&format!("shared_line_number_{i} = value_{i}\n"));
        }
    }
    r.write("mod/relocated.txt", edited.as_bytes());
    let head = r.commit_all("move and rewrite");

    // The model (wrongly) marks everything skim, and its payload must have
    // told it about the rename.
    let backend = FakeBackend::new("fake", |ids| {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        format!(r#"{{"groups": [{}]}}"#, json_group("Move", "skim", &refs))
    });
    let d = grouped(&r, &base, &head, &backend);
    assert!(backend.last_prompt().contains("% similar"));

    let groups = d.groups.as_ref().unwrap();
    // The gate pulled every gated class out; nothing skim remains.
    assert!(groups.iter().all(|g| g.effort != Effort::Skim));
    let gate = groups
        .iter()
        .find(|g| g.label == "Modified during move")
        .expect("gate group");
    assert_eq!(gate.effort, Effort::Close);
    assert_eq!(d.audit.skipped_hunks, Some(0));
}

#[test]
fn verbatim_rename_stays_skim() {
    let r = TestRepo::new();
    let body = b"fn alpha() {}\nfn beta() {}\nfn gamma() {}\nfn delta() {}\nfn epsilon() {}\n";
    r.write("src/old_name.rs", body);
    let base = r.commit_all("base");
    r.git(&["mv", "src/old_name.rs", "src/new_name.rs"]);
    let head = r.commit_all("move");

    let backend = FakeBackend::new("fake", |ids| {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Verbatim move", "skim", &refs)
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    let groups = d.groups.as_ref().unwrap();
    assert!(groups.iter().any(|g| g.effort == Effort::Skim));
    assert!(!groups.iter().any(|g| g.label == "Modified during move"));
}

#[test]
fn cache_pins_the_grouping() {
    let (r, base, head) = two_class_repo();
    let cache = tempfile::TempDir::new().unwrap();
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group(
                "All",
                "close",
                &ids.iter().map(String::as_str).collect::<Vec<_>>()
            )
        )
    });

    let d1 = grouped_with_cache(&r, &base, &head, &backend, Some(cache.path()));
    let d2 = grouped_with_cache(&r, &base, &head, &backend, Some(cache.path()));
    assert_eq!(backend.calls(), 1, "second run must be a cache hit");
    assert_eq!(d1, d2, "pinned grouping is byte-identical");

    // A different backend identity is a different key.
    let other = FakeBackend::new("other-model", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group(
                "All",
                "close",
                &ids.iter().map(String::as_str).collect::<Vec<_>>()
            )
        )
    });
    grouped_with_cache(&r, &base, &head, &other, Some(cache.path()));
    assert_eq!(other.calls(), 1);

    // No cache dir: every run calls the backend, nothing is written.
    let uncached = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group(
                "All",
                "close",
                &ids.iter().map(String::as_str).collect::<Vec<_>>()
            )
        )
    });
    grouped(&r, &base, &head, &uncached);
    grouped(&r, &base, &head, &uncached);
    assert_eq!(uncached.calls(), 2);
}

#[test]
fn empty_diff_groups_without_calling_the_model() {
    let r = TestRepo::new();
    r.write("f.txt", b"x\n");
    let base = r.commit_all("base");
    let head = r.commit_all("same tree");
    let backend = FakeBackend::new("fake", |_| unreachable!("no classes to offer"));
    let d = grouped(&r, &base, &head, &backend);
    assert_eq!(backend.calls(), 0);
    assert_eq!(d.groups.as_ref().unwrap().len(), 0);
    assert_eq!(d.reading_plan.as_ref().unwrap().len(), 0);
    assert_eq!(d.audit.coverage, Some(1.0));
}

#[test]
fn skim_group_without_remainder_has_no_skip_step() {
    let r = TestRepo::new();
    r.write("one.txt", b"single_change_here = old\n");
    let base = r.commit_all("base");
    r.write("one.txt", b"single_change_here = new\n");
    let head = r.commit_all("head");
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Tiny", "skim", &[&ids[0]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    let actions: Vec<ReadAction> = d
        .reading_plan
        .as_ref()
        .unwrap()
        .iter()
        .map(|s| s.action)
        .collect();
    assert_eq!(actions, [ReadAction::Exemplars]);
    assert_eq!(d.audit.read_hunks, Some(1));
    assert_eq!(d.audit.skipped_hunks, Some(0));
}

// ---------------------------------------------------------------- ordering

/// Definition file + consumer file, model puts the consumer group FIRST;
/// ordering must put the foundation first with real edges and roles.
fn def_use_repo() -> (TestRepo, String, String) {
    let r = TestRepo::new();
    r.write("src/a_core.txt", b"placeholder\n");
    r.write("src/b_user.txt", b"placeholder\n");
    let base = r.commit_all("base");
    r.write(
        "src/a_core.txt",
        b"placeholder\npub struct WidgetCore { pub retries: u32 }\n",
    );
    r.write(
        "src/b_user.txt",
        b"placeholder\nlet core = WidgetCore { retries: 3 };\n",
    );
    let head = r.commit_all("head");
    (r, base, head)
}

#[test]
fn foundation_is_ordered_before_its_consumer() {
    let (r, base, head) = def_use_repo();
    // C0 = a_core class, C1 = b_user class (equal size, first-seen order).
    // The model answers in the WRONG order: consumer first.
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("Use the widget", "close", &[&ids[1]]),
            json_group("Introduce the widget", "close", &[&ids[0]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    let groups = d.groups.as_ref().unwrap();

    assert_eq!(groups[0].label, "Introduce the widget");
    assert_eq!(
        groups[0].role,
        Some(differential_engine::schema::Role::Foundation)
    );
    assert_eq!(groups[0].rank, 0);
    assert_eq!(groups[1].label, "Use the widget");
    assert_eq!(
        groups[1].role,
        Some(differential_engine::schema::Role::Consumer)
    );
    assert_eq!(groups[1].depends_on, vec![groups[0].id.clone()]);
    assert!(groups[0].depends_on.is_empty());

    // The reading plan follows the new order.
    let plan = d.reading_plan.as_ref().unwrap();
    assert_eq!(plan[0].group, groups[0].id);
    assert_eq!(plan[1].group, groups[1].id);

    // Round-trips with ordering fields filled.
    let re = PlanDocument::from_json(&d.to_json_pretty().unwrap()).unwrap();
    assert_eq!(re, d);
}

#[test]
fn skim_groups_get_the_mechanical_role_and_stay_after_close() {
    let (r, base, head) = two_class_repo();
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("Rename sweep", "skim", &[&ids[0]]),
            json_group("Behaviour", "close", &[&ids[1]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    let groups = d.groups.as_ref().unwrap();
    assert_eq!(groups[0].effort, Effort::Close);
    assert_eq!(groups[1].effort, Effort::Skim);
    assert_eq!(
        groups[1].role,
        Some(differential_engine::schema::Role::Mechanical)
    );
}

#[test]
fn backfill_stays_trailing_even_when_everything_is_close() {
    let (r, base, head) = two_class_repo();
    // Model claims only the SMALL class; the 3-hunk class is back-filled and,
    // despite being larger, must not be reordered ahead of triaged groups.
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Only one", "close", &[&ids[1]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    let groups = d.groups.as_ref().unwrap();
    assert!(groups.last().unwrap().label.contains("no group"));
    assert_eq!(groups.last().unwrap().rank as usize, groups.len() - 1);
}
