//! Grouping-stage tests: real temp repos, fake LLM backend. No model runs.

use differential_engine::plan::ReviewSource;
use differential_engine::schema::{Effort, PlanDocument, ReadAction};
use differential_engine::store::FsGroupingCache;
use differential_testutil::{
    FakeBackend, TestRepo, grouped, grouped_with_cache, ids_in_prompt, json_group, two_class_repo,
};

#[test]
fn happy_path_fills_groups_plan_and_audit() {
    let (r, base, head) = two_class_repo();
    // C0 = the 3-member class (largest first), C1 = the singleton.
    let backend = FakeBackend::new("fake", |ids| {
        assert_eq!(ids.len(), 2);
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("Behaviour change", "focus", &[&ids[1]]),
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
    assert_eq!(groups[0].effort, Effort::Focus);
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
    // 1 focus hunk + 1 skim exemplar read; 2 skim remainders skipped.
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
            json_group("Only one", "focus", &[&ids[1]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);

    let groups = d.groups.as_ref().unwrap();
    assert_eq!(groups.len(), 2);
    let backfill = groups.last().unwrap();
    assert_eq!(backfill.effort, Effort::Focus);
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
    // Everything must still be read: back-fill is focus.
    assert_eq!(d.audit.read_hunks, Some(4));
    assert_eq!(d.audit.skipped_hunks, Some(0));
}

#[test]
fn duplicated_class_kept_by_first_group_only() {
    let (r, base, head) = two_class_repo();
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("First", "focus", &[&ids[0], &ids[1]]),
            json_group("Second", "focus", &[&ids[0]])
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
            json_group("All", "focus", &[&ids[0], &ids[1], "C999"])
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
            json_group("B", "focus", &[&ids[1]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    assert!(
        d.groups
            .as_ref()
            .unwrap()
            .iter()
            .all(|g| g.effort == Effort::Focus)
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
            json_group("Code", "focus", &[&ids[0]])
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

/// A generated file and a source file never share a class, however alike their
/// text.
///
/// They used to. The class key was the normalised text plus the disposition, so
/// one shaped edit made in both places was one class — neither generated nor
/// not. `class_is_generated` could only ask "is every member generated?", which
/// such a class answers no, so it was offered, and its lockfile hunk went
/// wherever the model put the class. A generated hunk in a focus group was the
/// symptom.
///
/// `generated` is in the class key now, so the split happens before anything
/// has to route it.
#[test]
fn a_generated_file_never_shares_a_class_with_a_source_file() {
    let r = TestRepo::new();
    r.write("Cargo.lock", b"shared_edit_line = old_value\n");
    r.write("src/x.txt", b"shared_edit_line = old_value\n");
    let base = r.commit_all("base");
    r.write("Cargo.lock", b"shared_edit_line = new_value\n");
    r.write("src/x.txt", b"shared_edit_line = new_value\n");
    let head = r.commit_all("head");

    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Edit", "focus", &[&ids[0]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);

    // One shape, two files, two classes.
    assert_eq!(d.classes.len(), 2, "identical text, but not one class");

    // Exactly one of them is offered, and it is the source one. The model is
    // never handed a class id the audit would reject.
    let offered = ids_in_prompt(&backend.last_prompt());
    assert_eq!(offered.len(), 1, "{offered:?}");
    let hunks_of = |cid: &str| -> Vec<&str> {
        let c = d.classes.iter().find(|c| c.id == cid).unwrap();
        c.hunk_ids
            .iter()
            .map(|h| d.hunks[h[1..].parse::<usize>().unwrap()].file.as_str())
            .collect()
    };
    assert_eq!(hunks_of(&offered[0]), ["src/x.txt"]);

    // And the lockfile half is folded, which is the whole point.
    let groups = d.groups.as_ref().unwrap();
    let noise = groups
        .iter()
        .find(|g| g.effort == Effort::Noise)
        .expect("the generated half must be folded");
    assert_eq!(noise.class_ids.len(), 1);
    assert_eq!(hunks_of(&noise.class_ids[0]), ["Cargo.lock"]);
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
    assert_eq!(gate.effort, Effort::Focus);
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
                "focus",
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
                "focus",
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
                "focus",
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
            json_group("Use the widget", "focus", &[&ids[1]]),
            json_group("Introduce the widget", "focus", &[&ids[0]])
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
    // The edge names its cause: the symbol the consumer referenced.
    assert_eq!(groups[1].depends_on.len(), 1);
    assert_eq!(groups[1].depends_on[0].on, groups[0].id);
    assert_eq!(groups[1].depends_on[0].via, vec!["WidgetCore".to_string()]);
    assert!(
        groups[1].depends_on[0].cycle.is_none(),
        "the sort honoured this edge, so there is no cycle to report"
    );
    assert!(groups[0].depends_on.is_empty());
    assert!(groups.iter().all(|g| g.pivot.is_none()), "no cycle here");

    // The graph is on the classes, and it says the same thing one level down.
    let consumer = d
        .classes
        .iter()
        .find(|c| !c.depends_on.is_empty())
        .expect("the consumer class");
    assert_eq!(consumer.depends_on[0].via, vec!["WidgetCore".to_string()]);
    assert!(
        d.classes.iter().any(|c| c.defines == ["WidgetCore"]),
        "the definition is recorded on the class that introduces it"
    );

    // The reading plan follows the new order.
    let plan = d.reading_plan.as_ref().unwrap();
    assert_eq!(plan[0].group, groups[0].id);
    assert_eq!(plan[1].group, groups[1].id);

    // Round-trips with ordering fields filled.
    let re = PlanDocument::from_json(&d.to_json_pretty().unwrap()).unwrap();
    assert_eq!(re, d);
}

/// Group A defines a trait in one class and consumes group B in another, and B
/// uses the trait. No group order satisfies both, but the classes are acyclic:
/// `h_def -> B -> h_use`. That is a cycle the grouping made, not the change.
fn contraction_cycle_repo() -> (TestRepo, String, String) {
    let r = TestRepo::new();
    for f in ["src/a_def.txt", "src/a_use.txt", "src/b.txt"] {
        r.write(f, b"placeholder\n");
    }
    let base = r.commit_all("base");
    // Distinct shapes, so each file is its own class.
    r.write(
        "src/a_def.txt",
        b"placeholder\npub trait WidgetPort { fn go(); }\n",
    );
    r.write(
        "src/a_use.txt",
        b"placeholder\nlet made = BuilderKind::new();\n",
    );
    // `impl WidgetPort` would DEFINE the trait as well, giving the symbol two
    // definers and dropping the edge. b must only reference it.
    r.write(
        "src/b.txt",
        b"placeholder\nstruct BuilderKind;\nfn drive(p: &dyn WidgetPort) {}\n",
    );
    let head = r.commit_all("head");
    (r, base, head)
}

#[test]
fn a_cycle_the_grouping_made_is_named_and_ordered_by_the_classes() {
    let (r, base, head) = contraction_cycle_repo();
    // The model merges the definition class with the consumer class, which is
    // what creates the deadlock. It is allowed to; the engine must cope.
    let backend = FakeBackend::new("fake", |ids| {
        let a_def = ids
            .iter()
            .find(|i| *i == "C0")
            .cloned()
            .unwrap_or_else(|| ids[0].clone());
        let rest: Vec<String> = ids.iter().filter(|i| **i != a_def).cloned().collect();
        let a: Vec<&str> = vec![a_def.as_str(), rest[0].as_str()];
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("Port and its caller", "focus", &a),
            json_group("The implementation", "focus", &[rest[1].as_str()])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    let groups = d.groups.as_ref().unwrap();

    let broken: Vec<&differential_engine::schema::Edge> = groups
        .iter()
        .flat_map(|g| g.depends_on.iter())
        .filter(|e| e.cycle.is_some())
        .collect();
    assert!(!broken.is_empty(), "the sort had to break an edge");
    assert!(
        broken
            .iter()
            .all(|e| e.cycle == Some(differential_engine::schema::Cycle::Artefact)),
        "the classes are acyclic, so this cycle came from contracting them"
    );

    // The group that could not be read as one thing says where it divides.
    let pivoted = groups
        .iter()
        .find(|g| g.pivot.is_some())
        .expect("a group was emitted out of the cycle");
    let n = pivoted.pivot.unwrap() as usize;
    assert!(
        n <= pivoted.class_ids.len(),
        "the pivot is an index into this group's classes"
    );

    // Nothing is split, and nothing is lost.
    let assigned: usize = groups.iter().map(|g| g.class_ids.len()).sum();
    assert_eq!(
        assigned,
        d.classes.len(),
        "every class in exactly one group"
    );
    assert_eq!(groups.len(), 2, "the groups the model asked for");
}

#[test]
fn skim_groups_get_the_mechanical_role_and_stay_after_close() {
    let (r, base, head) = two_class_repo();
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("Rename sweep", "skim", &[&ids[0]]),
            json_group("Behaviour", "focus", &[&ids[1]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    let groups = d.groups.as_ref().unwrap();
    assert_eq!(groups[0].effort, Effort::Focus);
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
            json_group("Only one", "focus", &[&ids[1]])
        )
    });
    let d = grouped(&r, &base, &head, &backend);
    let groups = d.groups.as_ref().unwrap();
    assert!(groups.last().unwrap().label.contains("no group"));
    assert_eq!(groups.last().unwrap().rank as usize, groups.len() - 1);
}

/// The progress callback reports the stages a renderer shows on its splash,
/// and says whether the slow grouping stage was a cache hit.
#[test]
fn progress_reports_stages_and_cache_state() {
    use differential_engine::config::Config;
    use differential_engine::grouping::Progress;
    use differential_engine::lang::LanguageRegistry;
    use differential_engine::pipeline::run_grouped_pipeline;
    use std::sync::Mutex;

    let r = TestRepo::new();
    r.write("a.txt", b"alpha_original = 1\n");
    let base = r.commit_all("base");
    r.write("a.txt", b"alpha_changed = 2\n");
    let head = r.commit_all("head");

    let backend = FakeBackend::new("fake-agent", |ids| {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        format!(r#"{{"groups": [{}]}}"#, json_group("All", "focus", &refs))
    });
    let dir = tempfile::TempDir::new().unwrap();
    let cache = FsGroupingCache::at(dir.path().to_path_buf());

    let run = || {
        let seen: Mutex<Vec<Progress>> = Mutex::new(Vec::new());
        let cb = |p: Progress| seen.lock().unwrap().push(p);
        run_grouped_pipeline(
            &r.repo(),
            &ReviewSource::range(base.clone(), head.clone(), head.clone()),
            &Config::default(),
            &LanguageRegistry::builtin(),
            &differential_testutil::stub_readers(),
            &differential_engine::grouping::GroupingOptions {
                backend: &backend,
                cache: &cache,
                artefacts: &differential_engine::store::FsArtefactStore::disabled(),
                fetch: "dfr",
                progress: Some(&cb),
            },
        )
        .unwrap();
        seen.into_inner().unwrap()
    };

    let first = run();
    assert_eq!(first.first(), Some(&Progress::Enumerating));
    assert!(first.contains(&Progress::Classifying));
    assert!(first.contains(&Progress::Ordering));
    assert_eq!(first.last(), Some(&Progress::Done));
    // Cache miss the first time: the backend name is carried for display.
    assert!(first.contains(&Progress::Grouping {
        backend: "fake-agent".into(),
        cached: false,
    }));

    // Second run hits the cache, and says so.
    assert!(run().contains(&Progress::Grouping {
        backend: "fake-agent".into(),
        cached: true,
    }));
}
