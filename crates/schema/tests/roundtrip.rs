use differential_schema::{PlanDocument, SchemaError};

const FIXTURE: &str = include_str!("fixtures/plan-v1.json");

#[test]
fn fixture_roundtrips() {
    let doc = PlanDocument::from_json(FIXTURE).expect("fixture must parse");
    let json = doc.to_json_pretty().unwrap();
    let doc2 = PlanDocument::from_json(&json).expect("serialised form must parse");
    assert_eq!(doc, doc2);
}

#[test]
fn fixture_is_internally_consistent() {
    let doc = PlanDocument::from_json(FIXTURE).unwrap();

    assert_eq!(doc.stats.files as usize, doc.files.len());
    assert_eq!(doc.stats.hunks as usize, doc.hunks.len());
    assert_eq!(doc.stats.classes as usize, doc.classes.len());

    // Every file hunk id resolves, in order, with the right back-reference.
    let mut seen = 0usize;
    for f in &doc.files {
        for hid in &f.hunk_ids {
            let h = doc
                .hunks
                .iter()
                .find(|h| &h.id == hid)
                .expect("dangling hunk id");
            assert_eq!(h.file, f.path);
            seen += 1;
        }
    }
    assert_eq!(
        seen,
        doc.hunks.len(),
        "every hunk belongs to exactly one file"
    );

    // Class partition covers every hunk exactly once.
    let mut covered: Vec<&str> = doc
        .classes
        .iter()
        .flat_map(|c| c.hunk_ids.iter().map(String::as_str))
        .collect();
    covered.sort_unstable();
    let mut all: Vec<&str> = doc.hunks.iter().map(|h| h.id.as_str()).collect();
    all.sort_unstable();
    assert_eq!(covered, all);

    // Exemplar is a member of its own class; hunk class back-references match.
    for c in &doc.classes {
        assert!(c.hunk_ids.contains(&c.exemplar));
        for hid in &c.hunk_ids {
            let h = doc.hunks.iter().find(|h| &h.id == hid).unwrap();
            assert_eq!(h.class, c.id);
        }
    }
}

#[test]
fn core_only_documents_have_null_groups_not_empty() {
    let doc = PlanDocument::from_json(FIXTURE).unwrap();
    assert!(doc.groups.is_none());
    assert!(doc.reading_plan.is_none());
    assert_eq!(doc.generator.stages, ["enumerate", "classify"]);

    // The distinction survives serialisation: null, not [] and not omitted.
    let json = doc.to_json().unwrap();
    assert!(json.contains("\"groups\":null"));
    assert!(json.contains("\"reading_plan\":null"));
}

#[test]
fn unknown_schema_version_is_rejected() {
    let bumped = FIXTURE.replacen("\"schema_version\": 1", "\"schema_version\": 999", 1);
    match PlanDocument::from_json(&bumped) {
        Err(SchemaError::UnsupportedVersion { found: 999 }) => {}
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
}

#[test]
fn unknown_fields_are_tolerated() {
    // Additive schema changes must not break older readers.
    let with_extra = FIXTURE.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1, \"some_future_field\": {\"x\": 1},",
        1,
    );
    PlanDocument::from_json(&with_extra).expect("unknown fields must be ignored");
}
