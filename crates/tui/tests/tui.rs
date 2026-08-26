//! TUI model tests: key events → state transitions + a TestBackend draw smoke
//! test. No real terminal, no real LLM.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use differential_engine::ReviewSession;
use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_grouped_pipeline;
use differential_engine::ports::ReviewStore;
use differential_engine::schema::SourceKind;
use differential_engine::store::{FsGroupingCache, FsReviewStore};
use differential_testutil::{FakeBackend, TestRepo, json_group};
use differential_tui::app::{App, Effect, Focus, Mode, ReviewOptions, ViewMode, Viewport};
use differential_tui::rows::{BoxStyle, RowFactory, RowKind};
use differential_tui::theme::THEME;
use differential_tui::window::Side;
use ratatui::style::Color;

/// First-listed class (the largest) becomes the skim sweep; the rest are
/// focus work — so the skim group has a foldable remainder.
fn skim_first_backend() -> FakeBackend {
    FakeBackend::new("fake", |ids| {
        let skim = ids.first().map(String::as_str).unwrap_or("C0");
        let rest: Vec<&str> = ids.iter().skip(1).map(String::as_str).collect();
        let mut groups = vec![json_group("Skim sweep", "skim", &[skim])];
        if !rest.is_empty() {
            groups.push(json_group("Focus work", "focus", &rest));
        }
        format!(r#"{{"groups": [{}]}}"#, groups.join(", "))
    })
}

/// Open an App over HEAD~1..HEAD of `r` with an explicit backend.
fn open_app_with(r: &TestRepo, backend: &FakeBackend, store: &str) -> App {
    let repo = Repo::open(Path::new(&r.root)).unwrap();
    let base = r.git(&["rev-parse", "HEAD~1"]);
    let head = r.git(&["rev-parse", "HEAD"]);
    let out = run_grouped_pipeline(
        &repo,
        &base,
        &head,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &differential_engine::grouping::GroupingOptions {
            backend,
            cache: &FsGroupingCache::disabled(),
            progress: None,
        },
    )
    .unwrap();
    let factory = RowFactory::new(repo, out.base.clone(), out.head.clone());
    let session = ReviewSession::open(
        FsReviewStore::at(r.root.join(store)).unwrap(),
        out.document.unwrap(),
        out.view,
    )
    .unwrap();
    App::new(session, factory, ReviewOptions::default())
}

/// Open an App over HEAD~1..HEAD of `r`, with the review store inside the
/// repo dir — reopening yields a resumed session over the same store.
fn open_app(r: &TestRepo) -> App {
    let repo = Repo::open(Path::new(&r.root)).unwrap();
    let base = r.git(&["rev-parse", "HEAD~1"]);
    let head = r.git(&["rev-parse", "HEAD"]);
    let backend = skim_first_backend();
    let out = run_grouped_pipeline(
        &repo,
        &base,
        &head,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &differential_engine::grouping::GroupingOptions {
            backend: &backend,
            cache: &FsGroupingCache::disabled(),
            progress: None,
        },
    )
    .unwrap();
    let factory = RowFactory::new(repo, out.base.clone(), out.head.clone());
    let session = ReviewSession::open(
        FsReviewStore::at(r.root.join(".dfr-test-store")).unwrap(),
        out.document.unwrap(),
        out.view,
    )
    .unwrap();
    App::new(session, factory, ReviewOptions::default())
}

/// Repo with one behavioural change + a 3-file repeated edit (skim material).
fn make_app() -> (TestRepo, App) {
    let r = TestRepo::new();
    r.write("src/main.txt", b"fn main() { run_slowly() }\n");
    for n in ["a", "b", "c"] {
        r.write(
            &format!("src/{n}.txt"),
            b"use old_helper_name;\nother content here\n",
        );
    }
    r.commit_all("base");
    r.write("src/main.txt", b"fn main() { run_with_retries(3) }\n");
    for n in ["a", "b", "c"] {
        r.write(
            &format!("src/{n}.txt"),
            b"use new_helper_name;\nother content here\n",
        );
    }
    r.commit_all("head");
    let app = open_app(&r);
    (r, app)
}

/// Switch the left pane between the reading plan and the file tree. `f` acts
/// on the pane it is pressed in, so this presses it there and puts focus back.
fn switch_left_pane(app: &mut App) {
    let focus = app.focus;
    app.focus = Focus::Groups;
    app.handle_key(key('f'));
    app.focus = focus;
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

#[test]
fn navigation_group_switch_and_focus() {
    let (_r, mut app) = make_app();
    assert_eq!(app.groups().len(), 2);
    assert_eq!(app.focus, Focus::Groups);

    // j in the groups pane switches group and rebuilds rows.
    let before_rows: Vec<_> = app.rows.iter().map(|r| r.kind.clone()).collect();
    app.handle_key(key('j'));
    assert_eq!(app.selected_group, 1);
    let after_rows: Vec<_> = app.rows.iter().map(|r| r.kind.clone()).collect();
    assert_ne!(before_rows, after_rows);

    // Skim group shows a fold row; z opens it.
    assert!(app.rows.iter().any(|r| r.kind == RowKind::Fold));
    app.handle_key(key('z'));
    assert!(!app.rows.iter().any(|r| r.kind == RowKind::Fold));

    // Tab moves focus to the diff pane; j moves the cursor over selectables.
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.focus, Focus::Detail);
    let c0 = app.cursor;
    app.handle_key(key('j'));
    assert!(app.cursor > c0);
    assert!(app.rows[app.cursor].kind.selectable());
}

#[test]
fn space_toggles_class_reviewed_and_persists() {
    let (r, mut app) = make_app();
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(key(' '));
    assert_eq!(app.session.reviewed_count(), 1);

    // The renderer is stateless: the mark is already on disk.
    let store = FsReviewStore::at(r.root.join(".dfr-test-store")).unwrap();
    assert_eq!(store.load_state().unwrap().reviewed_classes.len(), 1);

    // Toggling again clears it.
    app.handle_key(key(' '));
    assert_eq!(app.session.reviewed_count(), 0);
    assert!(store.load_state().unwrap().reviewed_classes.is_empty());
}

#[test]
fn finding_lifecycle_add_copy_delete() {
    let (_r, mut app) = make_app();
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    // c opens the editor on the current hunk.
    app.handle_key(key('c'));
    assert!(matches!(app.mode, Mode::Editing { .. }));
    for ch in "off by one".chars() {
        app.handle_key(key(ch));
    }
    app.handle_key(ctrl('s'));
    assert_eq!(app.session.findings().len(), 1);
    assert_eq!(app.session.findings()[0].body, "off by one");
    assert!(!app.session.findings()[0].anchor.hunk_digest.is_empty());

    // The finding renders as a row and the summary contains it.
    assert!(
        app.rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Finding(_, _)))
    );
    let effects = app.handle_key(key('y'));
    match effects.first() {
        Some(Effect::CopySummary(text)) => {
            assert!(text.contains("off by one"));
            assert!(text.contains(":"));
        }
        other => panic!("expected a copied summary, got {other:?}"),
    }

    // dd on the finding row deletes it.
    let finding_row = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Finding(_, _)))
        .unwrap();
    app.cursor = finding_row;
    app.handle_key(key('d'));
    app.handle_key(key('d'));
    assert!(app.session.findings().is_empty());
}

#[test]
fn esc_discards_editor_and_empty_findings_are_dropped() {
    let (_r, mut app) = make_app();
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(key('c'));
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    app.handle_key(key('c'));
    let effects = app.handle_key(ctrl('s')); // empty body
    assert!(effects.is_empty());
    assert!(app.session.findings().is_empty());
}

#[test]
fn quit_saves_cursor() {
    let (r, mut app) = make_app();
    let effects = app.handle_key(key('q'));
    assert_eq!(effects, vec![Effect::Quit]);
    let store = FsReviewStore::at(r.root.join(".dfr-test-store")).unwrap();
    assert!(store.load_state().unwrap().cursor.is_some());
}

#[test]
fn group_counts_files_and_line_totals() {
    let (_r, app) = make_app();
    // Across both groups: 4 files, 4 hunks, each hunk one line replaced.
    let files: usize = app.groups().iter().map(|g| g.n_files).sum();
    let adds: usize = app.groups().iter().map(|g| g.counts.adds).sum();
    let dels: usize = app.groups().iter().map(|g| g.counts.dels).sum();
    assert_eq!(files, 4);
    assert_eq!(adds, 4);
    assert_eq!(dels, 4);
    // The 3-file repeated edit lands in one group.
    assert!(app.groups().iter().any(|g| g.n_files == 3));
}

#[test]
fn file_view_lists_all_files_and_shares_review_marks() {
    use differential_tui::app::ViewMode;
    let (r, mut app) = make_app();
    assert_eq!(app.view_mode, ViewMode::Groups);

    switch_left_pane(&mut app);
    assert_eq!(app.view_mode, ViewMode::Files);
    assert_eq!(app.files().len(), 4);
    let store = FsReviewStore::at(r.root.join(".dfr-test-store")).unwrap();
    assert!(store.load_state().unwrap().file_view);

    // The right pane shows the selected file: one header + its hunks.
    assert!(
        app.rows
            .iter()
            .any(|row| matches!(row.kind, RowKind::FileHeader(_)))
    );
    assert!(
        app.rows
            .iter()
            .any(|row| matches!(row.kind, RowKind::HunkHeader { .. }))
    );

    // space in file view marks the class — visible back in group view too.
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(key(' '));
    assert_eq!(app.session.reviewed_count(), 1);
    switch_left_pane(&mut app);
    assert_eq!(app.view_mode, ViewMode::Groups);
    assert!(!store.load_state().unwrap().file_view);
    assert_eq!(app.session.reviewed_count(), 1);
}

#[test]
fn file_view_shows_hunks_across_groups_with_labels() {
    // One file with two separated edits of different shapes: the two hunks
    // land in different classes and (per the fake backend) different groups.
    let r = TestRepo::new();
    r.write(
        "src/dual.txt",
        b"first_region = old_alpha\npad1\npad2\npad3\npad4\npad5\nfn second() { call_old_api() }\n",
    );
    r.commit_all("base");
    r.write(
        "src/dual.txt",
        b"first_region = new_beta_value\npad1\npad2\npad3\npad4\npad5\nfn second() { call_new_api(42) }\n",
    );
    r.commit_all("head");
    let mut app = open_app(&r);
    assert_eq!(app.groups().len(), 2, "two shapes → two groups");

    switch_left_pane(&mut app);
    let hunk_headers = app
        .rows
        .iter()
        .filter(|row| matches!(row.kind, RowKind::HunkHeader { .. }))
        .count();
    assert_eq!(
        hunk_headers, 2,
        "file view shows the file's hunks from BOTH groups"
    );
}

#[test]
fn file_view_resume_restores_view_and_file() {
    use differential_tui::app::ViewMode;
    let (r, mut app) = make_app();
    switch_left_pane(&mut app);
    app.handle_key(key('J')); // next tree row
    let path = app.selected_path().unwrap();
    app.handle_key(key('q'));
    drop(app);

    let app2 = open_app(&r);
    assert_eq!(app2.view_mode, ViewMode::Files);
    assert_eq!(app2.selected_path().unwrap(), path);
}

#[test]
fn file_list_modal_opens_jumps_and_closes() {
    use differential_tui::app::Mode;
    let (_r, mut app) = make_app();
    // `f` opens the list from the DIFF pane; in the plan pane it switches the
    // left pane instead.
    app.focus = Focus::Detail;
    app.handle_key(key('f'));
    let (n_entries, first_path) = match &app.mode {
        Mode::FileList { entries, .. } => (entries.len(), entries[0].path.clone()),
        _ => panic!("f should open the file list"),
    };
    assert!(n_entries >= 1);

    // Enter jumps the cursor to (the first selectable after) that header.
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.focus, Focus::Detail);
    let header_row = app
        .rows
        .iter()
        .position(|r| matches!(&r.kind, RowKind::FileHeader(p) if *p == first_path))
        .unwrap();
    assert!(app.cursor >= header_row);
    assert!(app.rows[app.cursor].kind.selectable());

    // Esc closes without moving.
    app.handle_key(key('f'));
    assert!(matches!(app.mode, Mode::FileList { .. }));
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn split_view_toggles_and_keeps_cursor_on_hunk() {
    use differential_tui::rows::RowContent;
    let (r, mut app) = make_app();
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(
        !app.rows
            .iter()
            .any(|row| matches!(row.content, RowContent::Split { .. }))
    );
    let unified_diff_rows = app
        .rows
        .iter()
        .filter(|row| matches!(row.kind, RowKind::Diff(_)))
        .count();

    let hunk_before = app.rows[app.cursor].kind.hunk();
    app.handle_key(key('s'));

    // Split rows exist, the layout persisted, and a Modified line now takes
    // one row instead of two.
    assert!(
        app.rows
            .iter()
            .any(|row| matches!(row.content, RowContent::Split { .. }))
    );
    let split_diff_rows = app
        .rows
        .iter()
        .filter(|row| matches!(row.kind, RowKind::Diff(_)))
        .count();
    assert!(split_diff_rows < unified_diff_rows);
    assert_eq!(app.rows[app.cursor].kind.hunk(), hunk_before);
    let store = FsReviewStore::at(r.root.join(".dfr-test-store")).unwrap();
    assert!(store.load_state().unwrap().split_diff);

    // Toggling back restores the unified layout.
    app.handle_key(key('s'));
    assert!(
        !app.rows
            .iter()
            .any(|row| matches!(row.content, RowContent::Split { .. }))
    );
    assert!(!store.load_state().unwrap().split_diff);
}

#[test]
fn split_view_draw_smoke_shows_separator() {
    let (_r, mut app) = make_app();
    app.handle_key(key('s'));
    for width in [100u16, 60] {
        let backend = ratatui::backend::TestBackend::new(width, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("│"), "no separator at width {width}");
    }
}

#[test]
fn draw_smoke_test_renders_group_label() {
    let (_r, app) = make_app();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
    assert!(content.contains("Focus work"));
    assert!(content.contains("reading plan"));
    assert!(content.contains("classes reviewed"));
}

#[test]
fn reading_plan_shows_ids_and_flags_unsatisfiable_dependencies() {
    let (_r, app) = make_app();

    // Every dependency names a real group id — the id column makes them
    // resolvable, which is the whole point of showing it.
    let ids: Vec<&str> = app.groups().iter().map(|g| g.id.as_str()).collect();
    for g in app.groups() {
        for d in &g.depends_on {
            assert!(
                ids.contains(&d.id.as_str()),
                "dependency {:?} is not a group id",
                d.id
            );
            // The flag must agree with the plan order: it means "this
            // dependency appears further down", i.e. a cycle the toposort
            // had to break.
            let dep_pos = app.groups().iter().position(|o| o.id == d.id).unwrap();
            let self_pos = app.groups().iter().position(|o| o.id == g.id).unwrap();
            assert_eq!(
                d.unsatisfied,
                dep_pos > self_pos,
                "cycle flag disagrees with the order"
            );
        }
    }

    // The pane renders the id, the tier, and the counts.
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let content: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        content.contains(&app.groups()[0].id),
        "group id missing from the plan"
    );
    assert!(content.contains("files"), "per-group file count missing");
    assert!(content.contains("−"), "removed-line count missing");
}

#[test]
fn space_in_the_plan_pane_marks_the_whole_group() {
    let (_r, mut app) = make_app();
    assert_eq!(app.focus, Focus::Groups);
    // Pick the group with the most classes so "whole group" is meaningful.
    let target = app
        .groups()
        .iter()
        .enumerate()
        .max_by_key(|(_, g)| g.class_keys.len())
        .map(|(i, _)| i)
        .unwrap();
    while app.selected_group != target {
        app.handle_key(key('j'));
    }
    let want = app.groups()[target].class_keys.len();
    assert!(want >= 1);

    app.handle_key(key(' '));
    assert_eq!(
        app.session.reviewed_count(),
        want,
        "whole group should be marked"
    );
    // Pressing again clears the whole group (set semantics, not per-class flip).
    app.handle_key(key(' '));
    assert_eq!(app.session.reviewed_count(), 0);

    // In the diff pane, space still marks just the class under the cursor.
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(key(' '));
    assert_eq!(app.session.reviewed_count(), 1);
}

#[test]
fn n_and_shift_n_jump_between_hunks() {
    let (_r, mut app) = make_app();
    // Move to the skim group (3 hunks of one shape) and unfold its remainder,
    // so the view holds several hunks to jump between.
    app.handle_key(key('j'));
    app.handle_key(key('z'));
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(key('g'));
    let hunk_rows: Vec<usize> = app
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r.kind, RowKind::HunkHeader { .. }))
        .map(|(i, _)| i)
        .collect();
    assert!(hunk_rows.len() >= 2, "fixture needs multiple hunks");

    app.handle_key(key('n'));
    assert!(
        hunk_rows.contains(&app.cursor),
        "n should land on a hunk header"
    );
    let first = app.cursor;
    app.handle_key(key('n'));
    assert!(app.cursor > first);
    app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
    assert_eq!(app.cursor, first, "N goes back");
}

#[test]
fn file_view_is_a_collapsible_tree() {
    use differential_tui::app::{TreeKind, ViewMode};
    let (_r, mut app) = make_app();
    switch_left_pane(&mut app);
    assert_eq!(app.view_mode, ViewMode::Files);

    // The fixture's files all live under src/, so the tree has a src/ node
    // above them — directories are rows, files nest beneath.
    let dir_row = app
        .tree
        .iter()
        .position(|e| matches!(&e.kind, TreeKind::Dir { path } if path == "src"))
        .expect("src/ directory row");
    let files_visible = |a: &differential_tui::app::App| {
        a.tree
            .iter()
            .filter(|e| matches!(e.kind, TreeKind::File { .. }))
            .count()
    };
    assert_eq!(files_visible(&app), 4, "all files visible when expanded");

    // Selecting the directory shows every hunk beneath it.
    while app.selected_file != dir_row {
        app.handle_key(key('j'));
    }
    let hunks_under_dir = app
        .rows
        .iter()
        .filter(|r| matches!(r.kind, RowKind::HunkHeader { .. }))
        .count();
    assert!(hunks_under_dir >= 4, "directory view spans its files");

    // z collapses it: the files disappear, the directory row stays.
    app.handle_key(key('z'));
    assert_eq!(
        files_visible(&app),
        0,
        "collapsed directory hides its files"
    );
    assert!(
        app.tree
            .iter()
            .any(|e| matches!(&e.kind, TreeKind::Dir { path } if path == "src"))
    );
    app.handle_key(key('z'));
    assert_eq!(files_visible(&app), 4, "unfold restores them");
}

/// A repo whose two groups have a real symbol def -> use edge between them, so
/// the ordering stage fills `depends_on` and the gutter has something to draw.
///
/// `make_app`'s fixture has no such edge, which is why the gutter test used to
/// pass while asserting nothing. Mirrors the engine's `def_use_repo`.
fn app_with_dependency_edge() -> (TestRepo, App) {
    let r = TestRepo::new();
    r.write("src/a_core.txt", b"placeholder\n");
    r.write("src/b_user.txt", b"placeholder\n");
    r.commit_all("base");
    r.write(
        "src/a_core.txt",
        b"placeholder\npub struct WidgetCore { pub retries: u32 }\n",
    );
    r.write(
        "src/b_user.txt",
        b"placeholder\nlet core = WidgetCore { retries: 3 };\n",
    );
    r.commit_all("head");

    // One focus group per class, so each stays its own node in the DAG.
    let backend = FakeBackend::new("fake", |ids| {
        let groups: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| json_group(&format!("Group {i}"), "focus", &[id.as_str()]))
            .collect();
        format!(r#"{{"groups": [{}]}}"#, groups.join(", "))
    });
    let app = open_app_with(&r, &backend, ".dfr-edge-store");
    (r, app)
}

#[test]
fn the_plan_gutter_links_the_selected_group_to_what_it_follows() {
    use differential_tui::app::Relation;
    let (_r, mut app) = app_with_dependency_edge();

    // `expect`, not an early return: the previous version skipped silently
    // when the fixture had no edges — which is what it did, so it passed while
    // asserting nothing at all.
    let consumer = app
        .groups()
        .iter()
        .position(|g| !g.depends_on.is_empty())
        .expect("the fixture must produce a dependency edge, or this asserts nothing");
    let follows: Vec<String> = app.groups()[consumer]
        .depends_on
        .iter()
        .map(|d| d.id.clone())
        .collect();
    let foundation = app
        .groups()
        .iter()
        .position(|g| follows.contains(&g.id))
        .expect("the fixture must have a group the consumer follows");

    // j moves down, k up — a one-directional walk would spin forever when the
    // target is above the cursor, and the foundation sits above its consumer.
    let select = |app: &mut App, want: usize| {
        app.focus = Focus::Groups;
        for _ in 0..64 {
            if app.selected_group == want {
                return;
            }
            app.handle_key(key(if app.selected_group < want { 'j' } else { 'k' }));
        }
        panic!("could not reach group {want}");
    };

    // --- selecting the consumer: what it follows is marked ------------------
    select(&mut app, consumer);
    assert_eq!(app.relation_to_selected(consumer), Relation::Selected);

    // Biconditional, so a relation that wrongly returned None fails too. The
    // previous version only checked that a claimed edge was backed by
    // depends_on, never that a real edge produced a mark.
    assert!(follows.contains(&app.groups()[foundation].id));
    for (i, g) in app
        .groups()
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != consumer)
    {
        let marked = app.relation_to_selected(i) == Relation::Dependency;
        assert_eq!(
            marked,
            follows.contains(&g.id),
            "{} marked={marked} but the selected group follows it = {}",
            g.id,
            follows.contains(&g.id)
        );
    }

    let content = drawn(&mut app);
    assert!(content.contains("◆"), "selected group marker missing");
    assert!(
        content.contains("├"),
        "the selected group follows something, so a connector must be drawn"
    );
    // The tick wears the file tree's arm and reaches the title it points at.
    let rows = plan_rows(&mut app);
    assert!(
        rows.iter().any(|r| r.starts_with("◆─")),
        "the selected group's diamond must reach its title: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.starts_with("├─") || r.starts_with("└─")),
        "a dependency tick must reach its title: {rows:?}"
    );

    // --- selecting the foundation: the group that follows IT is not marked --
    // This is the change: the reverse edge used to be drawn, in a second
    // colour of the same glyph.
    select(&mut app, foundation);
    assert_eq!(
        app.relation_to_selected(consumer),
        Relation::None,
        "the consumer follows the selected group and must not be marked"
    );
    let content = drawn(&mut app);
    assert!(content.contains("◆"));
    assert!(
        !plan_pane(&mut app).contains("├"),
        "nothing is followed from here, so no connector should be drawn"
    );
}

/// The plan pane's rows, one string each, trimmed of the pane's own border.
/// Row order, unlike `plan_pane` — a connector's arm is two adjacent cells on
/// one row, and a column-major dump puts a pane's height between them.
fn plan_rows(app: &mut App) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (1..39u16)
        .map(|y| (1..39u16).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

/// Just the plan pane's columns. `├` is also a hunk box's corner over in the
/// diff pane, so an assertion about the connector has to say where it looks.
fn plan_pane(app: &mut App) -> String {
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..40u16)
        .flat_map(|x| (0..40u16).map(move |y| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string())
        .collect()
}

/// Render the DETAIL pane at a fixed size and flatten the buffer to text.
///
/// Focuses it first: the right pane is a map of the selected group while the
/// plan has focus, so a test asserting on diff content has to say it wants the
/// diff. `drawn_as_is` is for the tests that are about focus itself.
fn drawn(app: &mut App) -> String {
    app.focus = Focus::Detail;
    drawn_as_is(app)
}

/// The whole screen as rows, in row order — for assertions about text that
/// has to sit on one line.
fn drawn_rows(app: &mut App) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..40u16)
        .map(|y| (0..100u16).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

/// Render whatever the current focus puts on screen.
fn drawn_as_is(app: &mut App) -> String {
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}

#[test]
fn the_selected_plan_row_is_highlighted_edge_to_edge() {
    let (_r, app) = make_app();
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // The plan pane is the left 40 columns; its border is column 0 and the
    // last inner column is 38. Find the selected row by its background, then
    // assert that background runs to the pane edge rather than stopping at
    // the end of the label.
    let bg_of = |x: u16, y: u16| buf[(x, y)].style().bg;
    let selected_row = (1..39u16)
        .find(|&y| bg_of(2, y).is_some() && bg_of(2, y) == bg_of(3, y))
        .expect("a highlighted row");
    let bg = bg_of(2, selected_row);
    assert_eq!(
        bg_of(38, selected_row),
        bg,
        "selection stops short of the pane edge"
    );
}

#[test]
fn scrolling_back_up_reveals_the_group_header() {
    let (_r, mut app) = make_app();
    app.handle_key(key('z')); // unfold, so there is enough to scroll through
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    // A short pane, as a small terminal would give: the rows now overflow it.
    app.set_viewport(Viewport {
        detail_rows: 8,
        plan_rows: 8,
    });

    // The header block above the first selectable row carries the label,
    // description and dependencies — the cursor can never enter it.
    let first_selectable = app
        .rows
        .iter()
        .position(|r| r.kind.selectable())
        .expect("a selectable row");
    assert!(
        first_selectable > 0,
        "fixture should have header rows on top"
    );
    assert!(matches!(app.rows[0].kind, RowKind::GroupHeader));

    // Scroll to the bottom, then all the way back up.
    app.handle_key(key('G'));
    assert!(app.scroll() > 0, "should have scrolled away from the top");
    for _ in 0..40 {
        app.handle_key(ctrl('u'));
    }
    assert_eq!(
        app.scroll(),
        0,
        "scrolling up must reach row 0, not stop below it"
    );

    // g (top) lands there too.
    app.handle_key(key('G'));
    app.handle_key(key('g'));
    assert_eq!(app.scroll(), 0);
}

/// The ref decoration runs a real `git for-each-ref` and parses its real
/// output — the hand-written bytes in the unit test happily passed while the
/// format string was wrong, so this drives the actual command.
#[test]
fn picker_reads_real_branch_and_tag_names() {
    let r = TestRepo::new();
    r.write("a.txt", b"one\n");
    let first = r.commit_all("first");
    r.git(&["tag", "v0.1.0"]);
    r.git(&["tag", "-a", "v0.2.0", "-m", "annotated"]);
    r.git(&["branch", "feature"]);
    // A remote-tracking ref, without needing a remote.
    r.git(&["update-ref", "refs/remotes/origin/main", &first]);

    let refs = differential_engine::ports::CommitHistory::refs_by_commit(&r.repo());
    let names = refs.get(&first).expect("refs for the commit");
    for want in ["main", "feature", "v0.1.0", "v0.2.0", "origin/main"] {
        assert!(
            names.iter().any(|n| n == want),
            "{want:?} missing from {names:?}"
        );
    }
}

/// The reviewer must say when a group was never classified.
///
/// The stack has always rendered the audit's back-fill as `[unclassified]`
/// (`stack.rs::backfilled_group_renders_as_unclassified`) while the TUI showed
/// it as an ordinary focus group — the same document, described two ways.
/// One projection, one answer.
#[test]
fn a_backfilled_group_renders_as_unclassified() {
    let r = TestRepo::new();
    r.write("a.txt", b"use old_name;\n");
    r.write("b.txt", b"fn main() { slow() }\n");
    r.commit_all("base");
    r.write("a.txt", b"use new_name;\n");
    r.write("b.txt", b"fn main() { fast(3) }\n");
    r.commit_all("head");

    // The model answers with only one of the two class ids; the coverage
    // audit back-fills the other into a trailing must-read group.
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Only one", "focus", &[&ids[1]])
        )
    });
    let mut app = open_app_with(&r, &backend, ".dfr-backfill-store");

    let last = app.groups().last().expect("at least one group");
    assert!(
        last.unclassified,
        "the trailing group is the audit back-fill"
    );
    assert!(
        app.groups()[..app.groups().len() - 1]
            .iter()
            .all(|g| !g.unclassified),
        "only the back-fill is unclassified"
    );

    // And it is visible: the header says so rather than reading as focus work.
    while app.selected_group != app.groups().len() - 1 {
        app.handle_key(key('j'));
    }
    // The group's header row is in the detail pane, which shows a map of the
    // group while the plan has focus.
    app.focus = Focus::Detail;
    let text = drawn_as_is(&mut app);
    assert!(
        text.contains("unclassified"),
        "the back-fill group must be labelled, not shown as ordinary focus work"
    );
}

/// Geometry is state, not a draw-time discovery.
///
/// Before this, scroll math used `viewport_hint.max(8)` — a guess that `draw`
/// corrected one frame later — so a shrunk window kept a stale scroll offset
/// until something else happened to repaint. The clamp now runs in update,
/// with no key pressed and nothing drawn.
#[test]
fn shrinking_the_viewport_re_clamps_scroll_without_a_draw() {
    let (_r, mut app) = make_app();
    app.handle_key(key('z'));
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    // Tall enough that the whole diff fits, so nothing has scrolled yet.
    let tall = app.rows.len() + SCROLL_MARGIN + 2;
    app.set_viewport(Viewport {
        detail_rows: tall,
        plan_rows: tall,
    });
    app.handle_key(key('G'));
    assert_eq!(app.scroll(), 0, "everything fits, so nothing scrolled");

    // Now shrink — above MIN_VIEWPORT, so the floor cannot mask it. No key is
    // pressed and nothing is drawn between here and the assertion.
    app.set_viewport(Viewport {
        detail_rows: SHORT,
        plan_rows: SHORT,
    });
    assert!(
        app.scroll() > 0,
        "a shrunk viewport must scroll the cursor back into view immediately, \
         not on the next repaint"
    );
    assert!(
        app.cursor >= app.scroll() && app.cursor < app.scroll() + SHORT,
        "cursor {} outside the visible rows {}..{}",
        app.cursor,
        app.scroll(),
        app.scroll() + SHORT
    );
}

/// A pane height above the app's `MIN_VIEWPORT` floor, so the clamp cannot
/// mask the shrink.
const SHORT: usize = 9;
/// Mirrors the app's own scroll margin.
const SCROLL_MARGIN: usize = 3;

// ------------------------------------------------- context expansion (ADR 0021)

/// A repo with one long file changed in two places, so a hunk has plenty of
/// context above and below it and the two windows start apart.
fn app_with_a_long_file() -> (TestRepo, App) {
    let r = TestRepo::new();
    let body = |a: &str, b: &str| -> Vec<u8> {
        let mut out = String::new();
        for i in 1..=60 {
            match i {
                20 => out.push_str(a),
                40 => out.push_str(b),
                _ => out.push_str(&format!("let filler{i} = {i};\n")),
            }
        }
        out.into_bytes()
    };
    r.write(
        "src/long.rs",
        &body("let before = 1;\n", "let also_before = 2;\n"),
    );
    r.write("src/other.rs", b"fn untouched() {}\n");
    r.commit_all("base");
    r.write(
        "src/long.rs",
        &body("let after = 99;\n", "let also_after = 98;\n"),
    );
    r.commit_all("head");
    let backend = FakeBackend::new("fake", |ids| {
        let all: Vec<String> = ids.iter().map(|i| format!("{i:?}")).collect();
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group(
                "Everything",
                "focus",
                &all.iter().map(|s| s.trim_matches('"')).collect::<Vec<_>>()
            )
        )
    });
    let app = open_app_with(&r, &backend, ".dfr-long-store");
    (r, app)
}

/// Boundary rows, and how many diff rows a view is showing.
fn edges(app: &App) -> Vec<(usize, Side)> {
    app.rows
        .iter()
        .filter_map(|r| match r.kind {
            RowKind::ContextEdge { hunk, side, .. } => Some((hunk, side)),
            _ => None,
        })
        .collect()
}

fn detail_rows(app: &App) -> usize {
    app.rows
        .iter()
        .filter(|r| matches!(r.kind, RowKind::Diff(_)))
        .count()
}

fn put_cursor_on<F: Fn(&RowKind) -> bool>(app: &mut App, pred: F) -> usize {
    let pos = app
        .rows
        .iter()
        .position(|r| pred(&r.kind))
        .expect("no such row");
    app.cursor = pos;
    app.focus = Focus::Detail;
    pos
}

#[test]
fn context_boundary_rows_appear_and_z_expands_there() {
    let (_r, mut app) = app_with_a_long_file();

    // Each hunk sits mid-file, so both directions have lines left over.
    let before = edges(&app);
    assert!(
        before.iter().any(|(_, s)| *s == Side::Up) && before.iter().any(|(_, s)| *s == Side::Down),
        "expected a boundary at each end, got {before:?}"
    );
    let rows_before = detail_rows(&app);
    let text = drawn(&mut app);
    assert!(
        text.contains("lines hidden"),
        "the boundary says what is hidden: {text}"
    );

    // Stand on the first upward boundary and open it.
    put_cursor_on(&mut app, |k| {
        matches!(k, RowKind::ContextEdge { side: Side::Up, .. })
    });
    app.handle_key(key('z'));

    assert_eq!(
        detail_rows(&app),
        rows_before + ReviewOptions::default().context_step,
        "z pulls in exactly one step"
    );
    // Growing upward inserts rows above, so the cursor has to have followed
    // its boundary row rather than kept its index.
    assert!(matches!(
        app.rows[app.cursor].kind,
        RowKind::ContextEdge { side: Side::Up, .. }
    ));
}

#[test]
fn expanding_to_the_edge_of_the_gap_drops_the_boundary() {
    let (_r, mut app) = app_with_a_long_file();
    let hunk = match edges(&app).first() {
        Some((h, _)) => *h,
        None => panic!("no boundary to expand"),
    };

    // Nineteen lines precede the first change; keep pressing z until they are
    // all on screen and the boundary has nothing left to offer.
    for _ in 0..10 {
        if !edges(&app)
            .iter()
            .any(|(h, s)| *h == hunk && *s == Side::Up)
        {
            break;
        }
        put_cursor_on(
            &mut app,
            |k| matches!(*k, RowKind::ContextEdge { hunk: h, side: Side::Up, .. } if h == hunk),
        );
        app.handle_key(key('z'));
    }

    assert!(
        !edges(&app)
            .iter()
            .any(|(h, s)| *h == hunk && *s == Side::Up),
        "the boundary should be gone once the whole gap is shown"
    );
    assert!(
        app.status.contains("top of"),
        "the reviewer should be told why it vanished, got {:?}",
        app.status
    );
    // The cursor landed somewhere real, and line 1 of the file is now drawn.
    assert!(app.rows[app.cursor].kind.selectable());
    assert!(drawn(&mut app).contains("let filler1 ="));
}

#[test]
fn expanded_windows_that_meet_merge_into_one_block() {
    let (_r, mut app) = app_with_a_long_file();
    // Two hunks nineteen lines apart, each already showing three: expanding
    // the lower one's window upward closes the gap.
    let lower = edges(&app)
        .iter()
        .filter(|(_, s)| *s == Side::Up)
        .map(|(h, _)| *h)
        .next_back()
        .expect("a second hunk");

    // Press on whichever end of the middle gap is still offered. Once one press
    // would close it the two rows collapse to one, so "the upward one is gone"
    // is not the same as "the gap is closed".
    for _ in 0..8 {
        let mid = edges(&app)
            .into_iter()
            .find(|&(h, s)| (h == lower && s == Side::Up) || (h != lower && s == Side::Down));
        let Some((h, side)) = mid else { break };
        put_cursor_on(
            &mut app,
            |k| matches!(*k, RowKind::ContextEdge { hunk: x, side: sd, .. } if x == h && sd == side),
        );
        app.handle_key(key('z'));
    }

    // One block: one boundary at each outer end, and no line drawn twice.
    let e = edges(&app);
    assert_eq!(
        e.iter().filter(|(_, s)| *s == Side::Up).count(),
        1,
        "merged windows share one top boundary, got {e:?}"
    );
    assert_eq!(
        e.iter().filter(|(_, s)| *s == Side::Down).count(),
        1,
        "and one bottom, got {e:?}"
    );
    let text = drawn(&mut app);
    assert_eq!(
        text.matches("let filler25 =").count(),
        1,
        "a merged block must not repeat a line"
    );
    // Both hunk headers are still there, so n/N and findings still work.
    assert_eq!(
        app.rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::HunkHeader { .. }))
            .count(),
        2
    );
}

/// The property the whole windowed rebuild buys: syntect's cost tracks what is
/// drawn, not the size of the files touched.
#[test]
fn highlighting_is_windowed_not_whole_file() {
    let r = TestRepo::new();
    let big = |changed: &str| -> Vec<u8> {
        let mut out = String::new();
        for i in 1..=5_000 {
            if i == 4_900 {
                out.push_str(changed);
            } else {
                out.push_str(&format!("let filler{i} = {i};\n"));
            }
        }
        out.into_bytes()
    };
    r.write("src/big.rs", &big("let before = 1;\n"));
    r.commit_all("base");
    r.write("src/big.rs", &big("let after = 2;\n"));
    r.commit_all("head");

    let repo = Repo::open(Path::new(&r.root)).unwrap();
    let base = r.git(&["rev-parse", "HEAD~1"]);
    let head = r.git(&["rev-parse", "HEAD"]);
    let backend = skim_first_backend();
    let out = run_grouped_pipeline(
        &repo,
        &base,
        &head,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &differential_engine::grouping::GroupingOptions {
            backend: &backend,
            cache: &FsGroupingCache::disabled(),
            progress: None,
        },
    )
    .unwrap();
    let factory = RowFactory::new(repo, out.base.clone(), out.head.clone());
    let session = ReviewSession::open(
        FsReviewStore::at(r.root.join(".dfr-big-store")).unwrap(),
        out.document.unwrap(),
        out.view,
    )
    .unwrap();
    let app = App::new(session, factory, ReviewOptions::default());

    // One hunk at line 4,900 of a 5,000-line file. Two sides, each a window of
    // a few lines plus a bounded lookback — nowhere near the 10,000 lines the
    // whole-file pass used to parse.
    let scanned = app.highlighted_lines();
    assert!(
        scanned > 0,
        "the window really was highlighted, not skipped"
    );
    assert!(
        scanned < 400,
        "highlighting should track the window, not the file: scanned {scanned} lines"
    );
}

// ------------------------------------------------------- the lumen styling

/// The cell styles of one drawn row of the diff pane, left to right.
fn diff_pane_row(app: &App, y: u16) -> Vec<(String, Option<ratatui::style::Color>)> {
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    // The diff pane starts at column 40; its border is 40 and 99, so a row's
    // content is 41..=98. A hunk's box shares those border columns rather than
    // spending any of the content.
    (41..99u16)
        .map(|x| (buf[(x, y)].symbol().to_string(), buf[(x, y)].style().bg))
        .collect()
}

/// An unstyled ratatui cell reports `Color::Reset`, which is not a colour
/// anyone painted — only an explicit RGB counts as a background here.
fn painted(bg: Option<ratatui::style::Color>) -> Option<ratatui::style::Color> {
    match bg {
        Some(ratatui::style::Color::Rgb(..)) => bg,
        _ => None,
    }
}

#[test]
fn a_changed_row_paints_its_background_to_the_pane_edge() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;
    let app = app;
    // A changed row is one whose gutter block is painted; the last inner
    // column of the pane must then carry the line's own background too.
    let row = (1..39u16)
        .find(|&y| {
            let cells = diff_pane_row(&app, y);
            // A boundary band paints every cell one colour; a changed row's
            // gutter block and line tint differ, which is the point.
            painted(cells[1].1).is_some()
                && painted(cells.last().unwrap().1).is_some()
                && painted(cells[1].1) != painted(cells.last().unwrap().1)
        })
        .expect("no changed row found in the diff pane");
    let cells = diff_pane_row(&app, row);
    assert_eq!(
        painted(cells.last().unwrap().1),
        painted(cells[cells.len() - 20].1),
        "the background stops before the pane edge"
    );
    // The gutter block is a DIFFERENT colour from the line body, which is what
    // makes it read as an edge.
    assert_ne!(
        painted(cells[1].1),
        painted(cells.last().unwrap().1),
        "the gutter should be a stronger block than the line tint"
    );
    // A context row, by contrast, is painted nowhere.
    let context = (1..39u16)
        .find(|&y| {
            let cells = diff_pane_row(&app, y);
            let text: String = cells.iter().map(|(sym, _)| sym.as_str()).collect();
            text.contains("filler") && painted(cells[1].1).is_none()
        })
        .expect("no context row found");
    assert!(
        diff_pane_row(&app, context)
            .iter()
            .all(|(_, bg)| painted(*bg).is_none()),
        "unchanged context should carry no background at all"
    );
}

#[test]
fn colour_carries_the_change_so_there_are_no_marker_columns() {
    let (_r, mut app) = app_with_a_long_file();
    let text = drawn(&mut app);
    assert!(
        text.contains("let after = 99;"),
        "the changed line is drawn"
    );
    // The old `-`/`+` gutter put a marker between the numbers and the code.
    assert!(
        !text.contains("99 + let") && !text.contains(" - let"),
        "marker columns should be gone"
    );
}

#[test]
fn the_absent_side_of_a_split_row_is_hatched() {
    let r = TestRepo::new();
    r.write("src/a.rs", b"let keep = 1;\nlet gone = 2;\nlet tail = 3;\n");
    r.commit_all("base");
    // A pure deletion: the new side has no line for it at all.
    r.write("src/a.rs", b"let keep = 1;\nlet tail = 3;\n");
    r.commit_all("head");
    let backend = skim_first_backend();
    let mut app = open_app_with(&r, &backend, ".dfr-hatch-store");

    app.handle_key(key('s')); // split
    let text = drawn(&mut app);
    assert!(
        text.contains('╱'),
        "a side with no line should be hatched, not left blank"
    );
    assert!(
        text.contains("old") && text.contains("new"),
        "column labels"
    );
}

/// Background colours of the cells on one drawn row, left to right.
fn row_backgrounds(app: &mut App, y: u16) -> Vec<ratatui::style::Color> {
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..100u16).map(|x| buf[(x, y)].bg).collect()
}

/// The brighter line-number block the cursor's row wears on a changed line.
fn is_cursor_block(c: &ratatui::style::Color) -> bool {
    *c == THEME.added_gutter_cursor_bg || *c == THEME.deleted_gutter_cursor_bg
}

/// The screen row the cursor is drawn on, given the pane's scroll.
fn cursor_screen_row(app: &App) -> u16 {
    (app.cursor - app.scroll()) as u16 + 1
}

#[test]
fn the_cursor_is_a_brighter_gutter_block_on_a_changed_row() {
    let (_r, mut app) = app_with_a_long_file();
    // A changed line carries its own background, and a line style sits under
    // span styles — so the gutter block is what has to show the cursor there.
    put_cursor_on(&mut app, |k| matches!(k, RowKind::Diff(_)));
    // Walk onto a row that actually has a change colour.
    for _ in 0..20 {
        let y = cursor_screen_row(&app);
        if row_backgrounds(&mut app, y).iter().any(is_cursor_block) {
            return;
        }
        app.handle_key(key('j'));
    }
    panic!("the cursor's gutter must wear the brighter block of its change colour");
}

#[test]
fn the_cursor_lights_the_gutter_on_both_sides_of_a_split_row() {
    let (_r, mut app) = app_with_a_long_file();
    app.handle_key(key('s')); // split
    put_cursor_on(&mut app, |k| matches!(k, RowKind::Diff(_)));
    // A modification exists on both sides, so both gutters must light.
    for _ in 0..30 {
        let y = cursor_screen_row(&app);
        let bgs = row_backgrounds(&mut app, y);
        // The separator column splits the row into its two halves.
        if bgs[..50].iter().any(is_cursor_block) && bgs[50..].iter().any(is_cursor_block) {
            return;
        }
        app.handle_key(key('j'));
    }
    panic!("both halves of a modified split row must carry the cursor block");
}

#[test]
fn the_cursor_lights_the_absent_side_of_a_split_row_too() {
    let r = TestRepo::new();
    r.write("src/a.rs", b"let keep = 1;\nlet tail = 3;\n");
    r.commit_all("base");
    // A pure insertion: the OLD side has no line, so it is hatched.
    r.write(
        "src/a.rs",
        b"let keep = 1;\nlet fresh = 2;\nlet tail = 3;\n",
    );
    r.commit_all("head");
    let backend = skim_first_backend();
    let mut app = open_app_with(&r, &backend, ".dfr-cursor-hatch-store");
    app.handle_key(key('s'));
    put_cursor_on(&mut app, |k| matches!(k, RowKind::Diff(_)));
    for _ in 0..20 {
        let y = cursor_screen_row(&app);
        let bgs = row_backgrounds(&mut app, y);
        // The hatched half keeps a blank gutter of the same width, so the
        // cursor block lands in the same column on both sides.
        if bgs[..50].contains(&THEME.cursor_bg) {
            return;
        }
        app.handle_key(key('j'));
    }
    panic!("an absent side must still carry the cursor's gutter block");
}

#[test]
fn a_context_boundary_row_is_not_a_hunk_for_marking_or_findings() {
    let (_r, mut app) = app_with_a_long_file();
    put_cursor_on(&mut app, |k| matches!(k, RowKind::ContextEdge { .. }));
    app.handle_key(key('c'));
    assert!(
        matches!(app.mode, Mode::Normal),
        "c on a boundary row should not open the finding editor"
    );
    assert!(app.status.contains("move onto a hunk"), "{}", app.status);
}

#[test]
fn space_on_context_marks_the_hunk_that_context_belongs_to() {
    let (_r, mut app) = app_with_a_long_file();
    // Merge the two windows so one block spans both hunks, then check that a
    // context row acts on the hunk it is next to rather than on the block's
    // first one.
    let lower = edges(&app)
        .iter()
        .filter(|(_, s)| *s == Side::Up)
        .map(|(h, _)| *h)
        .next_back()
        .expect("a second hunk");
    for _ in 0..4 {
        if !edges(&app)
            .iter()
            .any(|(h, s)| *h == lower && *s == Side::Up)
        {
            break;
        }
        put_cursor_on(
            &mut app,
            |k| matches!(*k, RowKind::ContextEdge { hunk: h, side: Side::Up, .. } if h == lower),
        );
        app.handle_key(key('z'));
    }

    // The last diff row of the block is trailing context below the LOWER hunk.
    let last = app
        .rows
        .iter()
        .rposition(|r| matches!(r.kind, RowKind::Diff(_)))
        .expect("a diff row");
    assert_eq!(
        app.rows[last].kind.hunk(),
        Some(lower),
        "trailing context belongs to the hunk above it"
    );
    // The first diff row is leading context, above the FIRST hunk.
    let first = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Diff(_)))
        .expect("a diff row");
    assert_ne!(
        app.rows[first].kind.hunk(),
        Some(lower),
        "leading context belongs to the hunk below it"
    );
}

/// A hunk header is a band, not a `@@` line: every row already carries both
/// line numbers, so the coordinates repeated what was on screen in a notation
/// you had to decode.
#[test]
fn a_hunk_header_is_a_band_carrying_the_class_and_the_size() {
    let (_r, mut app) = app_with_a_long_file();
    // Still a selectable row, so n/N, space and c keep working on it.
    let header = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::HunkHeader { .. }))
        .expect("a hunk header row");
    assert!(app.rows[header].kind.selectable());
    assert!(app.rows[header].kind.hunk().is_some());

    // Idle, the header is the band and nothing else.
    let boundary = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::ContextEdge { .. }))
        .expect("a boundary row");
    app.cursor = boundary;
    app.focus = Focus::Detail;
    let idle = drawn(&mut app);
    assert!(
        !idle.contains("· C"),
        "an idle header shows no pill: {idle}"
    );

    // Move into the hunk and it says what it uniquely says: the size of the
    // change and the shape class. Each hunk here replaces one line with one.
    cursor_into_first_box(&mut app);
    let text = drawn(&mut app);
    assert!(
        !text.contains("@@"),
        "the diff-syntax coordinates should be gone"
    );
    assert!(text.contains("+1"), "the added count: {text}");
    assert!(text.contains("−1"), "the removed count");
}

/// Two boundary rows describing one gap sit adjacent with no blank between
/// them, so the seam reads as one band rather than two unrelated notices.
#[test]
fn two_boundaries_over_one_gap_are_one_band() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;

    // The two hunks are far enough apart that each block bounds the same gap.
    let at: Vec<usize> = app
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r.kind, RowKind::ContextEdge { .. }))
        .map(|(i, _)| i)
        .collect();
    let pair = at
        .windows(2)
        .find(|w| {
            matches!(
                app.rows[w[0]].kind,
                RowKind::ContextEdge {
                    side: Side::Down,
                    ..
                }
            ) && matches!(
                app.rows[w[1]].kind,
                RowKind::ContextEdge { side: Side::Up, .. }
            )
        })
        .expect("no down/up pair over one gap");
    assert_eq!(
        pair[1],
        pair[0] + 1,
        "the two rows of a band must be adjacent, with no blank between them"
    );

    // Each keeps its own button in the pane's border column — GitHub's expander
    // is two rows styled as one, so no key has to mean two directions.
    assert_eq!(app.rows[pair[0]].button, Some("↓"));
    assert_eq!(app.rows[pair[1]].button, Some("↑"));

    let buf = buffer_of(&app);
    let row_text = |y: u16| -> String { (41..99u16).map(|x| buf[(x, y)].symbol()).collect() };
    let band = (1..39u16)
        .find(|&y| row_text(y).contains("lines hidden"))
        .expect("no band row");
    let text = row_text(band);
    // A tinted body, not a rule, and none of the notation the hunk pills lost.
    assert!(
        !text.contains('\u{2508}'),
        "the dotted stub should be gone: {text:?}"
    );
    assert!(
        !text.contains("@@"),
        "`@@` was removed from headers: {text:?}"
    );
    // One tint the whole way across. Which of the two it is depends on where
    // the cursor is standing, so the assertion is about uniformity.
    let tint = buf[(41, band)].style().bg;
    assert!(
        tint == Some(THEME.hint_bg) || tint == Some(THEME.hint_cursor_bg),
        "the band should wear a band colour: {tint:?}"
    );
    assert!(
        (41..99u16).all(|x| buf[(x, band)].style().bg == tint),
        "the band should be tinted the whole way across: {text:?}"
    );
}

// ------------------------------------------- crossing into another group (#21)

/// A file whose two changes land in DIFFERENT groups, which is what makes one
/// of them foreign to the other's view.
fn app_with_two_groups_in_one_file() -> (TestRepo, App) {
    let r = TestRepo::new();
    let body = |a: &str, b: &str| -> Vec<u8> {
        let mut out = String::new();
        for i in 1..=40 {
            match i {
                10 => out.push_str(a),
                22 => out.push_str(b),
                _ => out.push_str(&format!("let filler{i} = {i};\n")),
            }
        }
        out.into_bytes()
    };
    r.write("src/f.rs", &body("let one = 1;\n", "let two = 2;\n"));
    r.commit_all("base");
    r.write("src/f.rs", &body("let one = 111;\n", "let two = 222;\n"));
    r.commit_all("head");

    // One class per group, so the two hunks cannot share one.
    let backend = FakeBackend::new("fake", |ids| {
        let groups: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(n, id)| json_group(&format!("Group {n}"), "focus", &[id.as_str()]))
            .collect();
        format!(r#"{{"groups": [{}]}}"#, groups.join(", "))
    });
    let app = open_app_with(&r, &backend, ".dfr-cross-store");
    (r, app)
}

/// Has a hunk from another group been pulled in?
fn shows_a_foreign_hunk(app: &App) -> bool {
    app.rows
        .iter()
        .any(|r| matches!(r.kind, RowKind::HunkHeader { foreign: true, .. }))
}

/// Walk the Down boundary open until it offers the hunk beyond, pressing `z`.
fn press_z_on_down_boundary(app: &mut App) -> bool {
    let Some(pos) = app.rows.iter().position(|r| {
        matches!(
            r.kind,
            RowKind::ContextEdge {
                side: Side::Down,
                ..
            }
        )
    }) else {
        return false;
    };
    app.cursor = pos;
    app.focus = Focus::Detail;
    app.handle_key(key('z'));
    true
}

#[test]
fn a_wall_is_named_rather_than_silent_and_z_crosses_it() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    assert!(
        !shows_a_foreign_hunk(&app),
        "nothing foreign is shown by default"
    );

    // Expand until the gap is spent. The boundary must NOT disappear — that is
    // the whole defect: a wall that looked like the end of the file.
    let mut crossed = false;
    for _ in 0..6 {
        let prompting = app.rows.iter().any(|r| {
            matches!(
                r.kind,
                RowKind::ContextEdge {
                    side: Side::Down,
                    crossing: true,
                    ..
                }
            )
        });
        if prompting {
            assert!(
                drawn(&mut app).contains("next:"),
                "the boundary should name what is beyond it"
            );
            press_z_on_down_boundary(&mut app);
            crossed = true;
            break;
        }
        assert!(
            press_z_on_down_boundary(&mut app),
            "boundary vanished early"
        );
    }
    assert!(crossed, "never reached the crossing prompt");
    assert!(
        shows_a_foreign_hunk(&app),
        "z on the prompt should have pulled the hunk in"
    );
}

#[test]
fn a_foreign_hunk_is_dashed_and_names_its_group() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    for _ in 0..6 {
        if shows_a_foreign_hunk(&app) {
            break;
        }
        press_z_on_down_boundary(&mut app);
    }

    // The distinction lives on the model, so assert it there rather than
    // depending on both hunks happening to share a viewport.
    let styles: Vec<BoxStyle> = app
        .rows
        .iter()
        .filter(|r| matches!(r.kind, RowKind::HunkHeader { .. }))
        .filter_map(|r| r.border)
        .map(|b| b.box_style)
        .collect();
    assert!(
        styles.contains(&BoxStyle::Own),
        "this group's own hunk should keep a solid edge"
    );
    assert!(
        styles.contains(&BoxStyle::Foreign),
        "the crossed hunk should be edged as foreign"
    );

    // Then look at the pixels, with the foreign hunk in view and active.
    let pos = app
        .rows
        .iter()
        .position(|r| {
            r.border.is_some_and(|b| b.box_style == BoxStyle::Foreign)
                && matches!(r.kind, RowKind::Diff(_))
        })
        .expect("a foreign hunk row");
    app.cursor = pos;
    app.focus = Focus::Detail;
    app.set_viewport(Viewport {
        detail_rows: 38,
        plan_rows: 38,
    });
    let buf = buffer_of(&app);
    let dashed: Vec<u16> = (1..39u16)
        .filter(|&y| buf[(40, y)].symbol() == "\u{254e}")
        .collect();
    assert!(!dashed.is_empty(), "a foreign hunk's edge should be dashed");

    // A foreign hunk wears the same cyan the hunk you ARE reading wears, muted:
    // same family, but plainly not on this reading list.
    assert_eq!(buf[(40, dashed[0])].style().fg, Some(THEME.foreign_fg));
    assert_ne!(THEME.foreign_fg, THEME.header_fg);

    // And it says whose it is, by id and label.
    let text = drawn(&mut app);
    let foreign = app
        .rows
        .iter()
        .find_map(|r| match r.kind {
            RowKind::HunkHeader {
                hunk,
                foreign: true,
            } => Some(hunk),
            _ => None,
        })
        .expect("a foreign hunk");
    let owner = app
        .session
        .plan()
        .group_of_hunk(differential_engine::plan::HunkId::from_index(foreign))
        .expect("the foreign hunk belongs to a group");
    // The id, not the label: the id is what the plan pane's `after:` lines are
    // keyed by, and the label is a sentence.
    let (want, label) = (format!("\u{b7} {}", owner.id), owner.label.clone());
    assert!(
        text.contains(&want),
        "a foreign header must name its group by id; looked for {want:?} in:\n{text}"
    );
    let header = drawn_rows(&mut app)
        .into_iter()
        .find(|r| r.contains(&want))
        .expect("the foreign header's row");
    assert!(
        !header.contains(&label),
        "the group's label belongs in the plan pane, not on a hunk header: {header:?}"
    );
}

/// A box borrows the pane's border columns rather than spending content ones,
/// so a line number inside a box sits where a line number outside one sits.
#[test]
fn a_box_costs_the_content_no_columns() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    cursor_into_first_box(&mut app);
    let buf = buffer_of(&app);
    // Content is 41..=98; the box lives in the pane's own border columns (40
    // and 99), so it costs the content nothing.
    let row_text = |y: u16| -> String { (41..99u16).map(|x| buf[(x, y)].symbol()).collect() };
    // Pick the framed row from the MODEL: the pane's own border is `│` too, so
    // a glyph test would happily match a row outside every box and compare it
    // with itself.
    let y_of = |i: usize| 1 + (i - app.scroll()) as u16;
    let framed_row = y_of(
        app.rows
            .iter()
            .position(|r| r.border.is_some() && matches!(r.kind, RowKind::Diff(_)))
            .expect("no row inside a box"),
    );
    let framed = row_text(framed_row);
    assert!(
        framed.contains("let "),
        "framed row has no code: {framed:?}"
    );
    let context = (1..39u16)
        .map(row_text)
        .find(|t| t.contains("let filler"))
        .expect("no context row");

    // Line numbers are right-aligned in a fixed field, so what must match is
    // where the CODE starts. By CHARACTER: `str::find` counts bytes.
    let code_col = |t: &str| {
        let at = t.find("let ").expect("no code on the row");
        t[..at].chars().count()
    };
    assert_eq!(
        code_col(&framed),
        code_col(&context),
        "a box must cost the content no columns:\n{framed}\n{context}"
    );

    // And the box side really is the pane's border column, not a cell inside it.
    assert_eq!(buf[(40, framed_row)].symbol(), "│");
    assert_ne!(
        buf[(41, framed_row)].symbol(),
        "│",
        "there should be no second vertical line beside the pane border"
    );
}

#[test]
fn n_skips_a_foreign_hunk_but_space_still_marks_it() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    for _ in 0..6 {
        if shows_a_foreign_hunk(&app) {
            break;
        }
        press_z_on_down_boundary(&mut app);
    }

    // n never lands on a foreign header: it is context the reviewer asked for,
    // not an entry on this group's reading list.
    app.cursor = 0;
    app.focus = Focus::Detail;
    for _ in 0..8 {
        app.handle_key(key('n'));
        assert!(
            !matches!(
                app.rows[app.cursor].kind,
                RowKind::HunkHeader { foreign: true, .. }
            ),
            "n landed on a foreign hunk header"
        );
    }

    // But space still marks it — the mark keys on class content and is shared
    // across groups, so reading it here is reading it everywhere.
    let (pos, hunk) = app
        .rows
        .iter()
        .enumerate()
        .find_map(|(i, r)| match r.kind {
            RowKind::HunkHeader {
                hunk,
                foreign: true,
            } => Some((i, hunk)),
            _ => None,
        })
        .expect("a foreign header");
    let before = app.session.reviewed_count();
    app.cursor = pos;
    app.handle_key(key(' '));
    assert_eq!(
        app.session.reviewed_count(),
        before + 1,
        "space on a foreign hunk should mark its class"
    );
    assert!(app.session.reviewed_hunks().contains(&hunk));
}

/// Put the cursor inside the first hunk's box, so that box is the active one.
fn cursor_into_first_box(app: &mut App) {
    let pos = app
        .rows
        .iter()
        .position(|r| r.border.is_some() && matches!(r.kind, RowKind::Diff(_)))
        .expect("no row inside a box");
    app.cursor = pos;
    app.focus = Focus::Detail;
    app.set_viewport(Viewport {
        detail_rows: 38,
        plan_rows: 38,
    });
}

fn buffer_of(app: &App) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    terminal.backend().buffer().clone()
}

/// A hunk is marked by an EDGE, not a box. Closing it top and bottom cut the
/// file into slabs; a vertical run down one side says where a hunk begins and
/// ends without chopping up the page.
#[test]
fn a_hunks_edge_runs_down_the_panes_own_border_column() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    cursor_into_first_box(&mut app);
    let buf = buffer_of(&app);

    let lit: Vec<u16> = (1..39u16)
        .filter(|&y| {
            buf[(40, y)].style().fg == Some(THEME.header_fg) && buf[(40, y)].symbol() == "\u{2502}"
        })
        .collect();
    assert!(lit.len() > 1, "the edge should run, not mark a single row");
    assert!(
        lit.windows(2).all(|w| w[1] == w[0] + 1),
        "the edge should be continuous, got rows {lit:?}"
    );
    for y in &lit {
        assert_eq!(buf[(40, *y)].symbol(), "\u{2502}");
    }

    // No horizontal rule closes it, and the right-hand border is left alone.
    let text: String = lit
        .iter()
        .flat_map(|&y| (41..99u16).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol())
        .collect();
    assert!(
        !text.contains('\u{2500}'),
        "a hunk should not be ruled off: {text:?}"
    );
    assert_ne!(buf[(99, lit[0])].style().fg, Some(THEME.skim_fg));
}

/// Only the hunk the cursor is in wears a colour. Every hunk accented at once
/// is no accent at all.
#[test]
fn only_the_active_hunks_edge_is_coloured() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    // With the cursor on a boundary row, no hunk is active and nothing is lit.
    let boundary = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::ContextEdge { .. }))
        .expect("a boundary row");
    app.cursor = boundary;
    app.focus = Focus::Detail;
    let buf = buffer_of(&app);
    assert!(
        (1..39u16).all(|y| buf[(40, y)].style().fg != Some(THEME.header_fg)),
        "no hunk is under the cursor, so no edge should be lit"
    );
    // The edges are still there — muted, not missing.
    assert!(
        app.rows.iter().any(|r| r.border.is_some()),
        "a muted edge is still an edge"
    );

    // Move into a hunk and exactly that edge lights up.
    cursor_into_first_box(&mut app);
    let active = app.rows[app.cursor].border.unwrap().hunk;
    let buf = buffer_of(&app);
    let lit: Vec<u16> = (1..39u16)
        .filter(|&y| buf[(40, y)].style().fg == Some(THEME.header_fg))
        .collect();
    assert!(!lit.is_empty(), "the hunk under the cursor should be lit");
    let rows_on_screen = &app.rows[app.scroll()..];
    for y in lit {
        let row = &rows_on_screen[(y - 1) as usize];
        assert_eq!(
            row.border.map(|b| b.hunk),
            Some(active),
            "a hunk other than the active one is lit at row {y}"
        );
    }
}

/// The band says the two things a reviewer can act on: how much is hidden, and
/// what stands beyond it once the gap is spent.
#[test]
fn a_band_says_what_is_hidden_and_what_is_beyond() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    app.focus = Focus::Detail;
    let text = drawn_as_is(&mut app);
    assert!(
        text.contains("lines hidden"),
        "a band should say how much is hidden: {text}"
    );

    // Spend the gap and the band names the hunk beyond instead.
    for _ in 0..6 {
        if app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::ContextEdge { crossing: true, .. }))
        {
            break;
        }
        press_z_on_down_boundary(&mut app);
    }
    let text = drawn_as_is(&mut app);
    assert!(
        text.contains("next: C"),
        "a spent gap should name what stands beyond it: {text}"
    );

    // When the whole gap fits in one press there is no direction to choose.
    let one_press = app.rows.iter().any(|r| r.button == Some("↕"));
    assert!(
        one_press || app.rows.iter().any(|r| r.button.is_some()),
        "a band always carries a button"
    );
}

/// A hunk's pill keeps ONE palette. The cursor being in it lights the pill's
/// leading cell, in the same colour as the edge below — so the marker and the
/// run read as one thing without a block of colour the eye goes to first.
#[test]
fn the_lit_hunk_pill_is_a_leading_bar_not_a_fill() {
    let (_r, mut app) = app_with_two_groups_in_one_file();

    // The pill's rows: a tinted row carrying `·` that is not a boundary band.
    let pill_rows = |buf: &ratatui::buffer::Buffer| -> Vec<u16> {
        (1..39u16)
            .filter(|&y| {
                let t: String = (41..99u16).map(|x| buf[(x, y)].symbol()).collect();
                t.contains('·') && !t.contains("hidden") && !t.contains("next:")
            })
            .collect()
    };
    let pill_bg = |buf: &ratatui::buffer::Buffer| -> Option<Color> {
        pill_rows(buf)
            .into_iter()
            .flat_map(|y| (41..99u16).map(move |x| (x, y)))
            .find_map(|(x, y)| buf[(x, y)].style().bg.filter(|b| *b != Color::Reset))
    };

    // Nothing active: no pill at all, just the hatched band.
    let boundary = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::ContextEdge { .. }))
        .expect("a boundary row");
    app.cursor = boundary;
    app.focus = Focus::Detail;
    let buf = buffer_of(&app);
    assert_eq!(pill_bg(&buf), None, "an idle header carries no pill");

    // Cursor in the hunk: the pill appears, muted fill, with one lit cell at
    // its head in the colour the edge beside it wears.
    cursor_into_first_box(&mut app);
    let buf = buffer_of(&app);
    assert_eq!(
        pill_bg(&buf),
        Some(THEME.button_bg),
        "a lit pill keeps the muted fill; only its leading cell changes"
    );
    let edge = (1..39u16)
        .find_map(|y| buf[(40, y)].style().fg.filter(|c| *c == THEME.header_fg))
        .expect("no lit edge");
    let bar = pill_rows(&buf)
        .into_iter()
        .flat_map(|y| (41..99u16).map(move |x| (x, y)))
        .find(|&(x, y)| {
            buf[(x, y)].symbol() == "▌" && buf[(x, y)].style().bg == Some(THEME.button_bg)
        })
        .expect("no lit bar at the head of the pill");
    assert_eq!(
        buf[bar].style().fg,
        Some(edge),
        "the bar should wear the edge's colour"
    );
}

/// The counts say added and removed in one pair, everywhere. They used to need
/// a second, darker pair because a lit pill filled with the hunk's accent and
/// the bright inks vanished on it; a lit pill is one cell now, so it does not.
#[test]
fn the_counts_keep_one_pair_of_colours() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    let inks = |app: &App| -> Vec<Color> {
        let buf = buffer_of(app);
        (1..39u16)
            .filter(|&y| {
                let t: String = (41..99u16).map(|x| buf[(x, y)].symbol()).collect();
                t.contains('·') && !t.contains("hidden") && !t.contains("next:")
            })
            .flat_map(|y| (41..99u16).map(move |x| (x, y)))
            .filter(|&(x, y)| matches!(buf[(x, y)].symbol(), "+" | "−"))
            .filter_map(|(x, y)| buf[(x, y)].style().fg)
            .collect()
    };

    cursor_into_first_box(&mut app);
    let lit = inks(&app);
    assert!(lit.contains(&THEME.add_fg), "no + colour: {lit:?}");
    assert!(lit.contains(&THEME.del_fg), "no − colour: {lit:?}");
}

// -------------------------------------------------- the overview surfaces

/// The map folds on the GROUP: a directory the group never enters is one row,
/// and the files it does not touch inside one it does enter are a count. A
/// document of any size then fits the float instead of running past it.
#[test]
fn the_group_map_folds_what_the_group_does_not_touch() {
    let r = TestRepo::new();
    // Every file changes, so every one is a row in the document's tree. Only
    // ONE of them lands in the group the map is drawn for.
    let files = [
        "deep/a/b/c/buried.rs",
        "src/one.rs",
        "src/two.rs",
        "src/three.rs",
        "src/four.rs",
        "src/five.rs",
        "src/target.rs",
    ];
    // Structurally distinct, so each file lands in its own shape class and so
    // in its own group — the map then has six files it must fold.
    let shapes = [
        ("fn f() { g(); }\n", "fn f() { h(); }\n"),
        ("let x = 1;\n", "let x = 2;\n"),
        ("struct S { a: u8 }\n", "struct S { a: u16 }\n"),
        ("use a::b;\n", "use a::c;\n"),
        ("const K: u8 = 1;\n", "const K: u8 = 2;\n"),
        ("impl T for S {}\n", "impl U for S {}\n"),
        ("enum E { A, B }\n", "enum E { A, C }\n"),
    ];
    for (path, (before, _)) in files.iter().zip(shapes) {
        r.write(path, before.as_bytes());
    }
    r.commit_all("base");
    for (path, (_, after)) in files.iter().zip(shapes) {
        r.write(path, after.as_bytes());
    }
    r.commit_all("head");

    // One class per group, so the selected group touches exactly one file.
    let backend = FakeBackend::new("fake", |ids| {
        let groups: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(n, id)| json_group(&format!("Group {n}"), "focus", &[id.as_str()]))
            .collect();
        format!(r#"{{"groups": [{}]}}"#, groups.join(", "))
    });
    let mut app = open_app_with(&r, &backend, ".dfr-map-fold-store");
    app.focus = Focus::Groups;

    // Walk to the group that owns `src/target.rs`: it is the one whose map has
    // a folded chain above it AND folded siblings beside it.
    let mut rows = drawn_rows(&mut app);
    for _ in 0..files.len() {
        if rows.iter().any(|l| l.contains("● target.rs")) {
            break;
        }
        app.handle_key(key('j'));
        rows = drawn_rows(&mut app);
    }

    // The chain the group never enters is ONE row, with its path joined.
    assert!(
        rows.iter().any(|l| l.contains("▸ deep/a/b/c/")),
        "an untouched chain must fold to one joined row: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|l| l.contains("buried.rs")),
        "a folded directory must not list its files: {rows:#?}"
    );
    // The changed file is lit, and its siblings are a count.
    assert!(
        rows.iter().any(|l| l.contains("● target.rs")),
        "the group's own file must still be lit: {rows:#?}"
    );
    assert!(
        rows.iter().any(|l| l.contains("more")),
        "the files the group misses must fold to a count: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|l| l.contains("three.rs")),
        "a folded file must not be named: {rows:#?}"
    );
}

/// Reading the plan, a map of the selected group FLOATS over the detail pane —
/// below the group's header, so its full label survives the plan pane's 40
/// columns, and above the diff, which carries on underneath as a preview of
/// what entering the group will show.
#[test]
fn the_group_map_floats_over_the_diff_when_the_plan_is_focused() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    app.focus = Focus::Groups;
    let text = drawn_as_is(&mut app);

    assert!(
        text.contains("files in g"),
        "the float names the group: {text}"
    );
    assert!(text.contains("f.rs"), "the tree should list files: {text}");
    assert!(
        text.contains('●'),
        "no file is marked as the group's: {text}"
    );
    // Tree guides, not bare indentation.
    assert!(
        text.contains('└') || text.contains('├'),
        "no tree guides: {text}"
    );

    // The group's header is above the float, and the diff below it.
    assert!(
        text.contains("[focus] Group 0"),
        "the group's title should be uncovered: {text}"
    );
    assert!(
        text.contains("let filler"),
        "the diff should carry on underneath the float: {text}"
    );

    // Tab and the float is gone.
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let text = drawn_as_is(&mut app);
    assert!(
        !text.contains("files in g"),
        "the float should lift: {text}"
    );
    assert!(text.contains("let filler"));
}

/// Reading the detail, a list of the files in view floats over the foot of the
/// plan pane to say where you are and how much is left.
#[test]
fn the_file_list_floats_over_the_plan_when_the_detail_is_focused() {
    let (_r, mut app) = app_with_two_groups_in_one_file();

    app.focus = Focus::Groups;
    assert!(
        !drawn_as_is(&mut app).contains("file 1 of"),
        "the list belongs to the detail pane's focus"
    );

    app.focus = Focus::Detail;
    let text = drawn_as_is(&mut app);
    assert!(text.contains("file 1 of 1"), "no file list drawn: {text}");
    assert!(text.contains("f.rs"), "the file is not listed: {text}");

    // The current file is marked by the row being lit edge to edge, not by a
    // glyph in a column of its own.
    let buf = buffer_of(&app);
    let row = (1..39u16)
        .find(|&y| {
            (1..39u16)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .contains("f.rs")
        })
        .expect("the file's row");
    assert!(
        (1..39u16).all(|x| buf[(x, row)].bg == THEME.selected_bg),
        "the current file's row should be lit the whole way across"
    );
}

/// Both overviews FLOAT, so focus never changes a pane's height. This is the
/// guarantee `spec/tui.md` opens with, and the reason splitting a pane on focus
/// was the wrong shape.
#[test]
fn focus_never_changes_a_pane_height() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    app.set_viewport(Viewport {
        detail_rows: 30,
        plan_rows: 30,
    });
    let before = app.viewport();
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.focus, Focus::Detail);
    assert_eq!(
        app.viewport(),
        before,
        "a float must not take room from the pane it covers"
    );
}

/// A long file stops saying which file it is once its header scrolls away.
#[test]
fn a_file_header_sticks_while_scrolled_past_it() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;
    app.set_viewport(Viewport {
        detail_rows: 10,
        plan_rows: 10,
    });

    let header = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::FileHeader(_)))
        .expect("a file header");
    // Park the cursor well below it so the header is off-screen.
    app.cursor = app.rows.len() - 1;
    app.set_viewport(Viewport {
        detail_rows: 10,
        plan_rows: 10,
    });
    assert!(
        app.scroll() > header,
        "not actually scrolled past the header"
    );

    let buf = buffer_of(&app);
    let top: String = (41..99u16).map(|x| buf[(x, 1)].symbol()).collect();
    assert!(
        top.contains("long.rs"),
        "the filename should stick to the top row: {top:?}"
    );

    // Back at the top, nothing is stuck: row one is the group header the rows
    // actually start with, not a filename pinned over it.
    app.cursor = 0;
    app.set_viewport(Viewport {
        detail_rows: 10,
        plan_rows: 10,
    });
    assert_eq!(app.scroll(), 0);
    let buf = buffer_of(&app);
    let top: String = (41..99u16).map(|x| buf[(x, 1)].symbol()).collect();
    assert!(
        top.contains("Everything") && !top.contains("long.rs"),
        "nothing should be stuck at the top of the rows: {top:?}"
    );
}

/// Colour on the counts, in the two places that were still grey.
#[test]
fn counts_are_coloured_in_the_file_modal_and_the_role_is_a_pill() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;
    app.handle_key(key('f'));
    let buf = buffer_of(&app);
    let inks: Vec<_> = buf.content().iter().filter_map(|c| c.style().fg).collect();
    assert!(
        inks.contains(&THEME.add_fg) && inks.contains(&THEME.del_fg),
        "the file modal's counts should say added and removed"
    );
}

/// The role pill hangs off the pane's right edge, so the roles read as a
/// column. Trailing the counts, each started wherever the counts happened to
/// end — a word you could only read by finding it first.
#[test]
fn the_role_pill_hangs_off_the_plan_panes_right_edge() {
    let (_r, mut app) = app_with_dependency_edge();
    app.focus = Focus::Groups;
    let rows = plan_rows(&mut app);
    let ends: Vec<usize> = rows
        .iter()
        .filter(|r| r.contains("foundation") || r.contains("consumer"))
        .map(|r| r.trim_end().chars().count())
        .collect();
    assert!(ends.len() >= 2, "the fixture needs two roles: {rows:?}");
    assert!(
        ends.windows(2).all(|w| w[0] == w[1]),
        "every role should end in the same column: {ends:?}"
    );
    // And that column is the pane's edge, not somewhere in the middle.
    let width = rows[0].chars().count();
    assert!(
        ends[0] + 2 >= width,
        "the pill should reach the right edge: ends at {} of {width}",
        ends[0]
    );
}

/// One fact, one rendering. The role was a pill in the plan pane and grey
/// suffix text on the group header three columns away.
#[test]
fn the_role_wears_the_same_pill_in_both_panes() {
    let (_r, mut app) = app_with_dependency_edge();
    app.focus = Focus::Detail;
    let buf = buffer_of(&app);
    let (_, pill_bg) = THEME.pill();

    // The group header row leads the detail pane and carries the role.
    let detail = (1..39u16)
        .find(|&y| {
            (41..99u16)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .contains("foundation")
        })
        .expect("no role on the group header");
    assert!(
        (41..99u16).any(|x| buf[(x, detail)].style().bg == Some(pill_bg)),
        "the group header's role should be a pill, not grey text"
    );

    // And the plan pane's copy of the same fact wears the same fill.
    let plan = (1..39u16)
        .find(|&y| {
            (1..39u16)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .contains("foundation")
        })
        .expect("no role in the plan pane");
    assert!((1..39u16).any(|x| buf[(x, plan)].style().bg == Some(pill_bg)));
}

/// Drawing a document with no groups must not panic. It was harmlessly a no-op
/// before the sticky header needed to know which file it was in.
#[test]
fn an_empty_document_draws_without_panicking() {
    let (_r, mut app) = make_app();
    app.rows.clear();
    app.cursor = 0;
    app.focus = Focus::Detail;
    let _ = drawn_as_is(&mut app);
    assert!(app.file_at_cursor().is_none());
}

/// A gap wide enough to need several presses shows both of its ends. Narrow it
/// until one press would close it and there is nothing left to choose between
/// them, so it collapses to a single row.
#[test]
fn a_gap_one_press_wide_is_one_row_not_two() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;

    let edge_rows = |app: &App| -> Vec<(usize, Side)> {
        app.rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::ContextEdge { hunk, side, .. } => Some((hunk, side)),
                _ => None,
            })
            .collect()
    };
    // Two hunks mid-file: an outer end each, plus both ends of the gap between.
    assert_eq!(
        edge_rows(&app).len(),
        4,
        "expected two outer ends and both ends of the middle gap"
    );

    // The middle gap is thirteen lines against a ten-line step, so one press
    // leaves three — close enough that the next press finishes it.
    let (upper, _) = edge_rows(&app)[0];
    put_cursor_on(
        &mut app,
        |k| matches!(*k, RowKind::ContextEdge { hunk: h, side: Side::Down, .. } if h == upper),
    );
    app.handle_key(key('z'));

    let rows = edge_rows(&app);
    assert_eq!(
        rows.len(),
        3,
        "the middle gap should now speak with one row, got {rows:?}"
    );
    assert!(
        app.rows.iter().any(|r| r.button == Some("↕")),
        "a one-press gap should offer both directions at once"
    );
    // And it still works: one more press closes it and the blocks merge.
    put_cursor_on(
        &mut app,
        |k| matches!(*k, RowKind::ContextEdge { hunk: h, side: Side::Down, .. } if h == upper),
    );
    app.handle_key(key('z'));
    assert_eq!(
        edge_rows(&app).len(),
        2,
        "closing the gap should leave only the outer ends"
    );
}

/// The file view's left pane IS a file tree, so neither float belongs there: a
/// map of one group would name a group nothing is selecting, and a file list
/// would be the pane behind it.
#[test]
fn neither_float_appears_in_the_file_view() {
    let (_r, mut app) = app_with_two_groups_in_one_file();

    app.focus = Focus::Groups;
    assert!(
        drawn_as_is(&mut app).contains("files in g"),
        "no map to lose"
    );
    switch_left_pane(&mut app);
    assert_eq!(app.view_mode, ViewMode::Files);
    let text = drawn_as_is(&mut app);
    assert!(
        !text.contains("files in g"),
        "the group map should not follow into the file view: {text}"
    );

    app.focus = Focus::Detail;
    let text = drawn_as_is(&mut app);
    assert!(
        !text.contains("file 1 of"),
        "nor should the file list: {text}"
    );

    // Both come back on the way out.
    switch_left_pane(&mut app);
    assert_eq!(app.view_mode, ViewMode::Groups);
    assert!(drawn_as_is(&mut app).contains("file 1 of"));
}

/// The file view's tree gets the same connectors the floating map draws.
#[test]
fn the_file_view_tree_is_drawn_with_guides() {
    let (_r, mut app) = app_with_two_groups_in_one_file();
    switch_left_pane(&mut app);
    let text = drawn_as_is(&mut app);
    assert!(
        text.contains('└') || text.contains('├'),
        "the file tree should have guides: {text}"
    );
}

/// Two blocks either side of one unlisted hunk both name it as what comes
/// next, and pressing either crosses the same hunk — so one row says it.
#[test]
fn one_hunk_between_two_blocks_is_offered_once_not_twice() {
    let r = TestRepo::new();
    // Three changes, the middle one far enough from both to stay its own block.
    let body = |a: &str, b: &str, c: &str| -> Vec<u8> {
        let mut out = String::new();
        for i in 1..=60 {
            match i {
                10 => out.push_str(a),
                30 => out.push_str(b),
                50 => out.push_str(c),
                _ => out.push_str(&format!("let filler{i} = {i};\n")),
            }
        }
        out.into_bytes()
    };
    r.write(
        "src/f.rs",
        &body("let a = 1;\n", "let b = 2;\n", "let c = 3;\n"),
    );
    r.commit_all("base");
    r.write(
        "src/f.rs",
        &body("let a = 11;\n", "let b = 22;\n", "let c = 33;\n"),
    );
    r.commit_all("head");
    // The outer two share a group; the middle one is its own, so it is foreign
    // to the view and sits between two blocks.
    let backend = FakeBackend::new("fake", |ids| {
        let mut outer: Vec<&str> = ids.iter().map(String::as_str).collect();
        let middle = outer.remove(1);
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("Outer", "focus", &outer),
            json_group("Middle", "focus", &[middle])
        )
    });
    let mut app = open_app_with(&r, &backend, ".dfr-between-store");
    app.focus = Focus::Detail;

    // Open both inner gaps until each names the hunk between them.
    for _ in 0..12 {
        let spent = app.rows.iter().enumerate().find_map(|(i, r)| {
            matches!(
                r.kind,
                RowKind::ContextEdge {
                    crossing: false,
                    ..
                }
            )
            .then_some(i)
        });
        let Some(i) = spent else { break };
        app.cursor = i;
        app.handle_key(key('z'));
    }

    let naming: Vec<usize> = app
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r.kind, RowKind::ContextEdge { crossing: true, .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        naming.len(),
        1,
        "one hunk between two blocks should be offered once, got {} rows",
        naming.len()
    );
    // And that row says it reaches both ways.
    assert_eq!(app.rows[naming[0]].button, Some("↕"));
    // Pressing it still works: the hunk arrives, marked foreign.
    app.cursor = naming[0];
    app.handle_key(key('z'));
    assert!(shows_a_foreign_hunk(&app), "z should have crossed it");
}

/// The finding editor floats over the diff and names what it annotates: a note
/// whose subject you cannot see is one you have to trust yourself about.
#[test]
fn the_finding_editor_floats_and_names_its_subject() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;
    put_cursor_on(&mut app, |k| matches!(k, RowKind::HunkHeader { .. }));
    app.handle_key(key('c'));
    assert!(matches!(app.mode, Mode::Editing { .. }));

    let text = drawn_as_is(&mut app);
    assert!(text.contains("long.rs · L"), "no file·line title: {text}");
    // The keys are in a footer inside the box: `enter` saves, and a newline is
    // shift+enter where the terminal reports it or a trailing `\` where it
    // does not.
    assert!(text.contains("enter save"), "no save key shown: {text}");
    assert!(text.contains("esc"), "no cancel key shown: {text}");
    assert!(text.contains("newline"), "no newline key shown: {text}");

    // It floats: the diff is still there around it.
    assert!(
        text.contains("let filler"),
        "the editor should float over the diff, not replace it: {text}"
    );
}

/// `enter` saves. A newline is shift+enter, or a trailing `\` before `enter`
/// for the terminals that report shift+enter as plain enter — which is most of
/// them without the keyboard enhancements this reviewer does not ask for.
#[test]
fn the_composer_saves_on_enter_and_takes_a_newline_two_ways() {
    let open = |app: &mut App| {
        put_cursor_on(app, |k| matches!(k, RowKind::HunkHeader { .. }));
        app.handle_key(key('c'));
        assert!(matches!(app.mode, Mode::Editing { .. }));
    };
    let typed = |app: &mut App, text: &str| {
        for c in text.chars() {
            app.handle_key(key(c));
        }
    };
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);

    // Plain enter saves.
    let (_r, mut app) = app_with_a_long_file();
    open(&mut app);
    typed(&mut app, "one line");
    app.handle_key(enter);
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.session.findings().len(), 1);
    assert_eq!(app.session.findings()[0].body, "one line");

    // shift+enter makes a second line, and enter then saves both.
    let (_r, mut app) = app_with_a_long_file();
    open(&mut app);
    typed(&mut app, "first");
    app.handle_key(shift_enter);
    typed(&mut app, "second");
    app.handle_key(enter);
    assert_eq!(app.session.findings()[0].body, "first\nsecond");

    // A trailing `\` before enter does the same, and the `\` is not kept.
    let (_r, mut app) = app_with_a_long_file();
    open(&mut app);
    typed(&mut app, "first\\");
    app.handle_key(enter);
    assert!(matches!(app.mode, Mode::Editing { .. }));
    assert!(
        matches!(app.mode, Mode::Editing { .. }),
        "the box should stay open"
    );
    typed(&mut app, "second");
    app.handle_key(enter);
    assert_eq!(app.session.findings()[0].body, "first\nsecond");

    // A `\` the reader went BACK to a line to leave is not a newline request:
    // the key looks at the character before the cursor, and `delete_char`
    // takes what the cursor sits after.
    let (_r, mut app) = app_with_a_long_file();
    open(&mut app);
    typed(&mut app, "ends with a slash\\");
    for _ in 0..5 {
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    }
    app.handle_key(enter);
    assert!(
        matches!(app.mode, Mode::Normal),
        "enter mid-line still saves"
    );
    assert_eq!(
        app.session.findings()[0].body,
        "ends with a slash\\",
        "nothing should have been deleted"
    );

    // `ctrl-s` still saves, for whoever's terminal passes it.
    let (_r, mut app) = app_with_a_long_file();
    open(&mut app);
    typed(&mut app, "by ctrl-s");
    app.handle_key(ctrl('s'));
    assert_eq!(app.session.findings()[0].body, "by ctrl-s");
}

/// Bracketed paste is enabled so a multi-line paste arrives whole. The event
/// was dropped, so pasting into the composer did nothing.
#[test]
fn a_paste_lands_in_the_composer() {
    let (_r, mut app) = app_with_a_long_file();
    put_cursor_on(&mut app, |k| matches!(k, RowKind::HunkHeader { .. }));
    app.handle_key(key('c'));
    app.handle_paste("pasted\nover two lines");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.session.findings()[0].body, "pasted\nover two lines");

    // In normal mode there is no field for it, so it does nothing.
    let (_r, mut app) = app_with_a_long_file();
    let before = app.rows.len();
    app.handle_paste("stray");
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.rows.len(), before);
}

/// `c` on a diff row annotates THAT line, not the whole hunk it sits in.
#[test]
fn c_on_a_line_anchors_to_that_line() {
    let (_r, mut app) = app_with_a_long_file();
    let row = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Diff(_)) && r.line.is_some())
        .expect("a diff row with a line");
    let at = app.rows[row].line.clone().expect("its line");
    app.cursor = row;
    app.focus = Focus::Detail;

    app.handle_key(key('c'));
    app.handle_key(key('x'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let f = &app.session.findings()[0];
    assert_eq!(f.anchor.side, at.side);
    assert_eq!(f.anchor.line, at.line, "the anchor is the cursor's line");
    assert_eq!(f.anchor.end_line, at.line, "one line, not a range");
    assert_eq!(f.anchor.line_span(), at.line.to_string());
}

/// `V` starts a selection the cursor extends; `c` then annotates the run.
#[test]
fn v_selects_lines_and_c_annotates_the_run() {
    let (_r, mut app) = app_with_a_long_file();
    // Three consecutive rows that are all lines of the same side.
    let start = app
        .rows
        .windows(3)
        .position(|w| {
            w.iter()
                .all(|r| r.line.as_ref().is_some_and(|l| l.side == "new"))
        })
        .expect("three new-side rows in a row");
    app.cursor = start;
    app.focus = Focus::Detail;

    app.handle_key(key('v'));
    assert_eq!(app.visual, Some(start));
    app.handle_key(key('j'));
    app.handle_key(key('j'));

    app.handle_key(key('c'));
    assert_eq!(app.visual, None, "writing the finding ends the selection");
    app.handle_key(key('x'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let first = app.rows[start].line.clone().unwrap();
    let last = app.rows[app.cursor].line.clone().unwrap();
    let f = &app.session.findings()[0];
    assert_eq!(f.anchor.line, first.line);
    assert_eq!(f.anchor.end_line, last.line);
    assert_eq!(f.anchor.span, last.line - first.line);
    assert_eq!(
        f.anchor.line_span(),
        format!("{}-{}", first.line, last.line)
    );

    // `esc` drops a selection rather than doing anything else.
    app.handle_key(key('v'));
    assert!(app.visual.is_some());
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.visual, None);
}

/// A finding is drawn under the line it annotates, not under the hunk header.
#[test]
fn a_finding_sits_under_the_line_it_annotates() {
    let (_r, mut app) = app_with_a_long_file();
    let row = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Diff(_)) && r.line.is_some())
        .expect("a diff row with a line");
    app.cursor = row;
    app.focus = Focus::Detail;
    app.handle_key(key('c'));
    app.handle_key(key('x'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let at = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Finding(..)))
        .expect("a finding row");
    let above = app.rows[at - 1]
        .line
        .clone()
        .expect("the row above a finding is the line it annotates");
    assert_eq!(above.line, app.session.findings()[0].anchor.end_line);

    // `dd` still deletes it from wherever it landed.
    app.cursor = at;
    app.handle_key(key('d'));
    app.handle_key(key('d'));
    assert!(app.session.findings().is_empty());
}

/// A note is prose about the code above it, so it is drawn as a quoted panel:
/// every line behind a muted rail, in muted italics.
#[test]
fn a_finding_is_a_quoted_panel_of_all_its_lines() {
    let (_r, mut app) = app_with_a_long_file();
    let row = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Diff(_)) && r.line.is_some())
        .expect("a diff row with a line");
    app.cursor = row;
    app.focus = Focus::Detail;
    app.handle_key(key('c'));
    app.handle_paste("first line\nsecond line");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let at: Vec<usize> = app
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r.kind, RowKind::Finding(..)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(at.len(), 2, "one row per line of the note");
    assert!(
        at.windows(2).all(|w| w[1] == w[0] + 1),
        "the panel is contiguous"
    );

    let text = drawn_rows(&mut app);
    let panel: Vec<&String> = text.iter().filter(|r| r.contains("line")).collect();
    assert!(
        text.iter().any(|r| r.contains("▍ first line")),
        "no rail on the note: {panel:?}"
    );
    assert!(
        text.iter().any(|r| r.contains("▍ second line")),
        "the second line is dropped: {panel:?}"
    );
    assert!(
        !text.iter().any(|r| r.contains("◆ first")),
        "the marker glyph is gone from the note itself"
    );

    // `dd` deletes the note from ANY of its lines.
    app.cursor = at[1];
    app.handle_key(key('d'));
    app.handle_key(key('d'));
    assert!(app.session.findings().is_empty());
}

/// `c` on a line that already carries a note opens THAT note. Two notes on one
/// line would each be half the story, and there was no way to fix a typo but
/// delete and retype.
#[test]
fn c_on_a_commented_line_rewrites_the_note() {
    let (_r, mut app) = app_with_a_long_file();
    let line = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Diff(_)) && r.line.is_some())
        .expect("a diff row with a line");
    app.cursor = line;
    app.focus = Focus::Detail;
    app.handle_key(key('c'));
    app.handle_paste("frist draft");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let id = app.session.findings()[0].id.clone();

    // From the line, and from the note's own row: both open the same note,
    // with its text already in the box.
    let note = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Finding(..)))
        .expect("a note row");
    for at in [line, note] {
        app.cursor = at;
        app.handle_key(key('c'));
        let Mode::Editing {
            editor, rewriting, ..
        } = &app.mode
        else {
            panic!("the box should be open");
        };
        assert_eq!(rewriting.as_deref(), Some(id.as_str()), "from row {at}");
        assert_eq!(editor.lines().join("\n"), "frist draft", "from row {at}");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }

    // The box opens at the END of the note, so a second thought is typed
    // rather than prepended. Rewriting keeps the id and the anchor.
    let before = app.session.findings()[0].anchor.line;
    let clear = |app: &mut App, n: usize| {
        for _ in 0..n {
            app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
    };
    app.cursor = line;
    app.handle_key(key('c'));
    app.handle_paste(", on reflection");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.session.findings().len(), 1, "no second note was filed");
    assert_eq!(app.session.findings()[0].id, id, "the id is a handle");
    assert_eq!(app.session.findings()[0].body, "frist draft, on reflection");
    assert_eq!(app.session.findings()[0].anchor.line, before);

    // Emptying the box leaves the note alone: `dd` is how a note is deleted,
    // and that is a deliberate press.
    app.cursor = line;
    app.handle_key(key('c'));
    clear(&mut app, 64);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.session.findings().len(), 1);
    assert_eq!(app.session.findings()[0].body, "frist draft, on reflection");

    // A selection is the exception: it asks for a note about the run.
    app.cursor = line;
    app.handle_key(key('v'));
    app.handle_key(key('j'));
    app.handle_key(key('c'));
    assert!(
        matches!(
            &app.mode,
            Mode::Editing {
                rewriting: None,
                ..
            }
        ),
        "a selection files a new note"
    );
}

/// A selection has to cross a hunk. Only a gap the reader never opened stops
/// it — a hunk's header and its removed and added rows are one continuous
/// stretch of one file.
#[test]
fn a_selection_crosses_a_hunk_from_either_side() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;
    // The removed half of the modification: an OLD-side row, and the one the
    // run used to get stuck on, since every row after it is new-side.
    let removed = app
        .rows
        .iter()
        .position(|r| r.line.as_ref().is_some_and(|l| l.side == "old"))
        .expect("a removed row");
    let old_line = app.rows[removed].line.clone().unwrap().line;

    app.cursor = removed;
    app.handle_key(key('v'));
    for _ in 0..3 {
        app.handle_key(key('j'));
    }
    app.handle_key(key('c'));
    let Mode::Editing { lines: Some(l), .. } = &app.mode else {
        panic!("no lines picked");
    };
    assert_eq!(l.side, "old", "the anchor's side is the run's side");
    assert_eq!(l.start, old_line);
    assert!(
        l.end > old_line,
        "an old-side run must reach the context below the hunk: {l:?}"
    );
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    // And the same downward, from the context above through the hunk.
    let above = app.rows[..removed]
        .iter()
        .rposition(|r| r.line.as_ref().is_some_and(|l| l.side == "new"))
        .expect("a context row above");
    app.cursor = above;
    app.handle_key(key('v'));
    for _ in 0..4 {
        app.handle_key(key('j'));
    }
    app.handle_key(key('c'));
    let Mode::Editing { lines: Some(l), .. } = &app.mode else {
        panic!("no lines picked");
    };
    let from = app.rows[above].line.clone().unwrap().line;
    assert_eq!((l.side.as_str(), l.start), ("new", from));
    // Past the hunk's header, its removed row and its added row: three rows
    // that are not two consecutive new-side lines, and used to end the run.
    assert!(
        l.end >= from + 2,
        "the run should reach past the hunk: {l:?}"
    );
}

/// `v` is how a reader gets into a selection, so it is the key their hand is
/// on to get out of one.
#[test]
fn v_toggles_the_selection_off() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;
    app.cursor = app
        .rows
        .iter()
        .position(|r| r.line.is_some())
        .expect("a line row");

    app.handle_key(key('v'));
    assert!(app.visual.is_some());
    app.handle_key(key('v'));
    assert_eq!(app.visual, None, "a second v drops it");

    // And it starts a fresh one rather than staying off.
    app.handle_key(key('v'));
    assert_eq!(app.visual, Some(app.cursor));
}

/// A selection stops where the file's line numbers do. Dragging from line 23
/// across `13 lines hidden` to line 37 used to file a note claiming fifteen
/// lines, thirteen of which were never on screen.
#[test]
fn a_selection_stops_at_a_gap_it_never_opened() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;
    let boundary = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::ContextEdge { .. }) && r.button == Some("↓"))
        .expect("a downward boundary");
    let above = app.rows[..boundary]
        .iter()
        .rposition(|r| r.line.is_some())
        .expect("a line above it");
    let last_seen = app.rows[above].line.clone().unwrap();

    // Select from that line and walk down past the gap onto a line beyond it.
    app.cursor = above;
    app.handle_key(key('v'));
    for _ in 0..6 {
        app.handle_key(key('j'));
        if app.cursor > boundary && app.rows[app.cursor].line.is_some() {
            break;
        }
    }
    let beyond = app.rows[app.cursor]
        .line
        .clone()
        .expect("a line past the gap");
    assert!(
        beyond.line > last_seen.line + 1,
        "the fixture needs a real gap: {} to {}",
        last_seen.line,
        beyond.line
    );

    app.handle_key(key('c'));
    let Mode::Editing { lines: Some(l), .. } = &app.mode else {
        panic!("no lines picked");
    };
    assert_eq!(
        (l.start, l.end),
        (last_seen.line, last_seen.line),
        "the selection should have stopped at the last line the reader saw"
    );
}

/// A note over a RANGE is drawn under its last line, so the run above it is
/// not adjacent to it. Standing anywhere in the run lights the whole thing.
#[test]
fn a_ranged_note_lights_every_line_it_covers() {
    let (_r, mut app) = app_with_a_long_file();
    let start = app
        .rows
        .windows(3)
        .position(|w| {
            w.iter()
                .all(|r| r.line.as_ref().is_some_and(|l| l.side == "new"))
        })
        .expect("three new-side rows in a row");
    app.cursor = start;
    app.focus = Focus::Detail;

    app.handle_key(key('v'));
    app.handle_key(key('j'));
    app.handle_key(key('j'));
    app.handle_key(key('c'));
    app.handle_paste("about all three");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let a = app.session.findings()[0].anchor.clone();
    assert_eq!(
        a.end_line - a.line,
        2,
        "the fixture needs a three-line note"
    );

    // The rows of the run, and the note's own row after the last of them.
    let covered: Vec<usize> = app
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.line
                .as_ref()
                .is_some_and(|l| (a.line..=a.end_line).any(|n| l.holds(&a.side, n)))
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(covered.len(), 3, "three lines: {covered:?}");
    let note = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Finding(..)))
        .expect("a note row");
    assert_eq!(note, covered[2] + 1, "the note hangs off the LAST line");

    // Standing anywhere in the run, or on the note: the whole cluster lights.
    let lit = |app: &mut App, rows: &[usize]| -> bool {
        let buf = buffer_of(app);
        rows.iter().all(|&i| {
            let y = (i - app.scroll()) as u16 + 1;
            buf[(40, y)].style().fg == Some(THEME.finding_fg)
        })
    };
    let cluster: Vec<usize> = covered
        .iter()
        .copied()
        .chain(std::iter::once(note))
        .collect();
    for at in cluster.clone() {
        app.cursor = at;
        assert!(
            lit(&mut app, &cluster),
            "standing on row {at} should light every row of the note"
        );
    }

    // And `c` from the FIRST line of the run rewrites that note.
    app.cursor = covered[0];
    app.handle_key(key('c'));
    assert!(
        matches!(
            &app.mode,
            Mode::Editing {
                rewriting: Some(_),
                ..
            }
        ),
        "the first line of a run is in the note about that run"
    );
}

/// A note is drawn under its line, and the only sign the two belonged together
/// was that they were adjacent. Standing on either lights both, in the border
/// column and on the note's own rail.
#[test]
fn standing_on_a_note_or_its_line_lights_both() {
    let (_r, mut app) = app_with_a_long_file();
    let row = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Diff(_)) && r.line.is_some())
        .expect("a diff row with a line");
    app.cursor = row;
    app.focus = Focus::Detail;
    app.handle_key(key('c'));
    app.handle_paste("first\nsecond");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let note = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Finding(..)))
        .expect("a note row");
    let line = note - 1;

    // The border column of the line and of both note rows, and the rails.
    let lit = |app: &mut App| -> (Vec<bool>, Vec<bool>) {
        let buf = buffer_of(app);
        let y = |i: usize| (i - app.scroll()) as u16 + 1;
        let border = (line..=note + 1)
            .map(|i| buf[(40, y(i))].style().fg == Some(THEME.finding_fg))
            .collect();
        let rails = (note..=note + 1)
            .map(|i| {
                (41..99u16).any(|x| {
                    buf[(x, y(i))].symbol() == "▍"
                        && buf[(x, y(i))].style().fg == Some(THEME.finding_fg)
                })
            })
            .collect();
        (border, rails)
    };

    // The cursor is elsewhere: nothing is lit.
    app.cursor = app
        .rows
        .iter()
        .enumerate()
        .position(|(i, r)| !(line..=note + 1).contains(&i) && r.kind.selectable())
        .expect("a row outside the cluster");
    let (border, rails) = lit(&mut app);
    assert!(
        !border.iter().any(|b| *b),
        "nothing should be lit from away"
    );
    assert!(!rails.iter().any(|b| *b), "the rail stays muted from away");

    // On the line, then on each row of the note: all three light every time.
    for at in [line, note, note + 1] {
        app.cursor = at;
        let (border, rails) = lit(&mut app);
        assert!(
            border.iter().all(|b| *b),
            "the border should run findings-coloured down the cluster, from row {at}"
        );
        assert!(
            rails.iter().all(|b| *b),
            "both rails should be findings-coloured, from row {at}"
        );
    }
}

/// The summary is pasted where nothing knows what `g7` was, so it carries the
/// file, the lines and the note — and no group.
#[test]
fn the_findings_summary_names_no_group() {
    let (_r, mut app) = app_with_a_long_file();
    let row = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Diff(_)) && r.line.is_some())
        .expect("a diff row with a line");
    let at = app.rows[row].line.clone().unwrap();
    app.cursor = row;
    app.focus = Focus::Detail;
    app.handle_key(key('c'));
    app.handle_paste("look here");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let summary = app.findings_summary();
    assert!(
        summary.contains(&format!("src/long.rs:{}: look here", at.line)),
        "the summary should be file:lines: note — got {summary:?}"
    );
    for label in app.groups().iter().map(|g| g.label.clone()) {
        assert!(
            !summary.contains(&label),
            "the group's label leaked: {summary:?}"
        );
    }
}

/// A finding filed from a hunk header has no line, so it annotates the hunk —
/// and it is drawn where every finding used to be, under that header.
#[test]
fn a_finding_from_a_header_anchors_the_hunk_and_sits_under_it() {
    let (_r, mut app) = app_with_a_long_file();
    let header = put_cursor_on(&mut app, |k| matches!(k, RowKind::HunkHeader { .. }));
    let hunk = app.rows[header].kind.hunk().expect("its hunk");
    app.handle_key(key('c'));
    app.handle_key(key('x'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let f = &app.session.findings()[0];
    assert_eq!(
        f.anchor.offset, 0,
        "a header annotates the hunk's first line"
    );
    assert_eq!(f.anchor.span, 0);
    assert_eq!(f.anchor.hunk_digest, app.session.doc().hunks[hunk].digest);

    let at = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Finding(..)))
        .expect("a finding row");
    assert!(
        app.rows[..at]
            .iter()
            .rev()
            .find_map(|r| r.line.as_ref().map(|l| l.line))
            .is_some_and(|l| l == f.anchor.end_line)
            || matches!(app.rows[at - 1].kind, RowKind::HunkHeader { .. }),
        "it belongs under its line or, failing that, under its header"
    );
}

/// Write a note on the first row that can carry one, and return its row.
fn note_on(
    app: &mut App,
    pred: impl Fn(&differential_tui::rows::Row) -> bool,
    body: &str,
) -> usize {
    let at = app.rows.iter().position(&pred).expect("no row matches");
    app.cursor = at;
    app.focus = Focus::Detail;
    app.handle_key(key('c'));
    app.handle_paste(body);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    at
}

fn findings_modal(app: &App) -> (usize, usize) {
    match &app.mode {
        Mode::Findings {
            entries, selected, ..
        } => (entries.len(), *selected),
        _ => panic!("the findings modal should be open"),
    }
}

/// A note is written on a line and drawn under it, which is no help at all in
/// answering "what have I found". `F` is the list.
#[test]
fn f_opens_every_finding_in_one_list() {
    let (_r, mut app) = app_with_a_long_file();
    note_on(&mut app, |r| r.line.is_some(), "the first thing");
    let second = app
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.line.is_some() && !matches!(r.kind, RowKind::Finding(..)))
        .map(|(i, _)| i)
        .nth(4)
        .expect("a second line");
    app.cursor = second;
    app.handle_key(key('c'));
    app.handle_paste("the second thing");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let before = app.cursor;
    app.handle_key(key('F'));
    assert_eq!(findings_modal(&app), (2, 0));

    let rows = drawn_rows(&mut app);
    for want in ["findings · 2", "the first thing", "the second thing"] {
        assert!(
            rows.iter().any(|r| r.contains(want)),
            "{want:?} missing from the list: {rows:#?}"
        );
    }
    // Each note says where it is, so the list answers without the diff.
    assert!(
        rows.iter().any(|r| r.contains("src/long.rs:")),
        "no location on the notes"
    );

    // `esc` closes it and moves nothing.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.cursor, before, "closing the list should move nothing");

    // And it opens from the plan pane too: it is about the review, not a pane.
    app.focus = Focus::Groups;
    app.handle_key(key('F'));
    assert!(matches!(app.mode, Mode::Findings { .. }));
}

/// `enter` puts the cursor on the note. A note with no row in this view says
/// so instead — it does not drag the reader into another group.
#[test]
fn enter_jumps_to_a_note_or_says_why_not() {
    let (_r, mut app) = app_with_a_long_file();
    note_on(&mut app, |r| r.line.is_some(), "look here");
    app.cursor = 0;
    app.handle_key(key('F'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.focus, Focus::Detail);
    let note = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Finding(..)))
        .expect("the note's row");
    assert_eq!(app.cursor, note, "the cursor should land on the note");

    // A note with no row: the list says which case it is and closes.
    app.handle_key(key('F'));
    if let Mode::Findings { entries, .. } = &mut app.mode {
        entries[0].row_idx = None;
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.status.contains("not in this view"),
        "no reason given: {:?}",
        app.status
    );
}

/// `dd` deletes the selected note and the list stays open — a reviewer
/// clearing up has more than one to clear.
#[test]
fn dd_in_the_list_deletes_one_and_stays() {
    let (_r, mut app) = app_with_a_long_file();
    for (n, body) in ["one", "two", "three"].iter().enumerate() {
        let at = app
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.line.is_some() && !matches!(r.kind, RowKind::Finding(..)))
            .map(|(i, _)| i)
            .nth(n * 3)
            .expect("a line");
        app.cursor = at;
        app.focus = Focus::Detail;
        app.handle_key(key('c'));
        app.handle_paste(body);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }
    assert_eq!(app.session.findings().len(), 3);

    app.handle_key(key('F'));
    app.handle_key(key('j'));
    assert_eq!(findings_modal(&app), (3, 1));
    let Mode::Findings {
        entries, selected, ..
    } = &app.mode
    else {
        unreachable!()
    };
    let doomed = entries[*selected].body.clone();

    // One `d` is a latch, not a delete.
    app.handle_key(key('d'));
    assert_eq!(app.session.findings().len(), 3, "one d deletes nothing");
    app.handle_key(key('d'));

    assert_eq!(app.session.findings().len(), 2);
    assert!(
        !app.session.findings().iter().any(|f| f.body == doomed),
        "the wrong note went"
    );
    assert_eq!(
        findings_modal(&app).0,
        2,
        "the list stays open, one shorter"
    );
    // And the diff lost it too, not just the list.
    let notes = app
        .rows
        .iter()
        .filter(|r| matches!(r.kind, RowKind::Finding(..)))
        .count();
    assert_eq!(notes, 2, "the diff should carry the two that are left");
}

/// Clearing every note is the only irreversible thing in this app, so it asks.
#[test]
fn d_clears_everything_but_only_after_a_yes() {
    let (_r, mut app) = app_with_a_long_file();
    note_on(&mut app, |r| r.line.is_some(), "the only one");
    app.handle_key(key('F'));

    // `D` alone deletes nothing; it asks, and the box says so.
    app.handle_key(key('D'));
    assert_eq!(app.session.findings().len(), 1, "D alone deletes nothing");
    assert!(
        drawn_rows(&mut app)
            .iter()
            .any(|r| r.contains("delete all 1 findings?")),
        "the confirmation should be on screen"
    );

    // Anything but `y` is a slip, and a slip must not empty the store.
    app.handle_key(key('n'));
    assert_eq!(app.session.findings().len(), 1);
    assert!(app.status.contains("nothing deleted"));

    app.handle_key(key('D'));
    app.handle_key(key('y'));
    assert!(app.session.findings().is_empty(), "D then y clears the lot");
    assert!(matches!(app.mode, Mode::Normal), "an empty list closes");
}

/// An orphaned note — one whose code is gone — has no row anywhere: it matches
/// no line and no hunk digest, so `place_findings` emits nothing for it. Before
/// this list its body could not be read in the app at all, and `dd` could not
/// reach it. It is the one thing here the list is not a convenience for.
#[test]
fn the_list_is_the_only_door_to_an_orphaned_note() {
    use differential_engine::review_state::{Anchor, Finding};

    let r = TestRepo::new();
    r.write("src/f.txt", b"alpha = 1\nbeta = 2\n");
    r.commit_all("base");
    r.write("src/f.txt", b"alpha = 11\nbeta = 2\n");
    r.commit_all("head");

    // A note the current plan can re-anchor to nothing: no hunk carries that
    // digest, and no hunk carries that text.
    let store_dir = r.root.join(".dfr-orphan-store");
    let store = FsReviewStore::at(store_dir.clone()).unwrap();
    let orphan = Finding::new(
        1,
        "the code this was about is gone".into(),
        "an older plan".into(),
        Anchor {
            file: "src/vanished.txt".into(),
            side: "new".into(),
            line: 12,
            end_line: 12,
            hunk_digest: "a digest no hunk has".into(),
            line_text: "a line no file has".into(),
            ..Anchor::default()
        },
    );
    store.save_findings(&[orphan]).unwrap();

    let backend = skim_first_backend();
    let mut app = open_app_with(&r, &backend, ".dfr-orphan-store");
    assert_eq!(app.session.findings().len(), 1);
    assert_eq!(
        app.session.findings()[0].status,
        differential_engine::review_state::FindingStatus::Orphaned
    );

    // It has no row, so nothing in the diff pane can reach it.
    assert!(
        !app.rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Finding(..))),
        "an orphan has no row to stand on"
    );

    // The list has it, under its own rule, with its body readable.
    app.handle_key(key('F'));
    let rows = drawn_rows(&mut app);
    for want in [
        "1 orphaned",
        "orphaned ──",
        "the code this was about is gone",
    ] {
        assert!(
            rows.iter().any(|r| r.contains(want)),
            "{want:?} missing: {rows:#?}"
        );
    }

    // And `dd` reaches it, which nothing else does.
    app.handle_key(key('d'));
    app.handle_key(key('d'));
    assert!(app.session.findings().is_empty(), "dd must reach an orphan");
}

/// More notes than the box is tall. The file-list modal has no scroll and
/// clips; a review has more notes than it has files.
#[test]
fn the_list_scrolls_to_keep_the_selection_on_screen() {
    let (_r, mut app) = app_with_a_long_file();
    app.set_viewport(Viewport {
        detail_rows: 4,
        plan_rows: 4,
    });
    let lines: Vec<usize> = app
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.line.is_some())
        .map(|(i, _)| i)
        .collect();
    assert!(lines.len() >= 6, "the fixture needs six lines to annotate");
    for (n, at) in lines.iter().take(6).enumerate() {
        // Each note adds a row, so re-find the line rather than trusting `at`.
        let at = app
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.line.is_some())
            .map(|(i, _)| i)
            .nth(n)
            .unwrap_or(*at);
        app.cursor = at;
        app.focus = Focus::Detail;
        app.handle_key(key('c'));
        app.handle_paste(&format!("note {n}"));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }
    let total = app.session.findings().len();
    assert!(total >= 5, "wrote {total} notes, wanted at least five");

    app.handle_key(key('F'));
    for _ in 0..total {
        app.handle_key(key('j'));
    }
    let Mode::Findings {
        selected, scroll, ..
    } = &app.mode
    else {
        panic!("the list should be open");
    };
    assert_eq!(*selected, total - 1, "j should reach the last note");
    assert!(
        *scroll > 0,
        "the window should have moved to keep it on screen"
    );
    assert!(*scroll <= *selected, "and never past the selection");
}

#[test]
#[ignore = "prints the pane for a human to look at"]
fn render_dump_findings() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;
    // Three notes: one on a line, one over a range, one on a hunk header.
    let mut wrote = 0;
    for i in 0..app.rows.len() {
        if wrote == 3 {
            break;
        }
        if app.rows[i].line.is_none() && !matches!(app.rows[i].kind, RowKind::HunkHeader { .. }) {
            continue;
        }
        app.cursor = i;
        if wrote == 1 {
            app.handle_key(key('v'));
            app.handle_key(key('j'));
        }
        app.handle_key(key('c'));
        app.handle_paste(&format!("note number {wrote} about this"));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        wrote += 1;
    }
    app.handle_key(key('F'));
    println!("\n=== findings modal ===");
    println!("{}", ansi_dump(&mut app, 120, 16));
    app.handle_key(key('D'));
    println!("\n=== confirming ===");
    println!("{}", ansi_dump(&mut app, 120, 16));
}

/// The help modal is keys and nothing else. Five lines of prose about the plan
/// pane and the diff's colours used to sit between `n/N` and `s`.
#[test]
fn the_help_modal_is_only_keys() {
    let (_r, mut app) = make_app();
    app.handle_key(key('?'));
    assert!(matches!(app.mode, Mode::Help));

    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..40u16)
        .map(|y| (0..100u16).map(|x| buf[(x, y)].symbol()).collect())
        .collect();

    let at = |needle: &str| {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} missing from help"))
    };
    // Every key row, then the footer. No legend in between, and none after.
    let footer = at("press any key");
    for k in ["j/k", "n/N", "z ", "s ", "f ", "space", "dd", "quit"] {
        assert!(at(k) < footer, "{k:?} should be in the key table");
    }
    for prose in [
        "reading the panes",
        "plan row",
        "no -/+ columns",
        "floats a map",
    ] {
        assert!(
            !rows.iter().any(|r| r.contains(prose)),
            "{prose:?} is a legend line and should not be in the help modal"
        );
    }

    // The key column is aligned: every description starts in one column.
    let col = |needle: &str| rows[at(needle)].find(needle).unwrap();
    assert_eq!(col("previous / next group"), col("half page"));
    assert_eq!(col("half page"), col("unified / split diff"));
}

/// Not an assertion — a readable dump of the pane, so the styling can be
/// eyeballed with `cargo test -- --ignored --nocapture render_dump`.
#[test]
#[ignore = "prints the pane for a human to look at"]
fn render_dump() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Detail;
    put_cursor_on(&mut app, |k| matches!(k, RowKind::Diff(_)));
    for mode in ["unified", "split"] {
        // Park on a changed row, so the cursor's gutter block is in the dump.
        for _ in 0..20 {
            let y = cursor_screen_row(&app);
            if row_backgrounds(&mut app, y).iter().any(is_cursor_block) {
                break;
            }
            app.handle_key(key('j'));
        }
        println!("\n=== diff: {mode} ===");
        println!("{}", ansi_dump(&mut app, 120, 26));
        app.handle_key(key('s'));
    }
}

/// The screen as ANSI truecolour, so a paste of it shows what a terminal shows.
/// `TestBackend` renders styles but only exposes them cell by cell.
fn ansi_dump(app: &mut App, w: u16, h: u16) -> String {
    use ratatui::style::{Color, Modifier};
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    // The named colours the theme still uses, as the 8-bit codes a terminal
    // reads — truecolour and named inks have to end up in one escape.
    let code = |c: Color, base: u8| -> String {
        match c {
            Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
            Color::Reset => format!("{}", base + 9),
            Color::Black => format!("{}", base),
            Color::Red => format!("{}", base + 1),
            Color::Green => format!("{}", base + 2),
            Color::Yellow => format!("{}", base + 3),
            Color::Blue => format!("{}", base + 4),
            Color::Magenta => format!("{}", base + 5),
            Color::Cyan => format!("{}", base + 6),
            Color::Gray => format!("{}", base + 7),
            Color::DarkGray => format!("{}", base + 60),
            Color::LightRed => format!("{}", base + 61),
            Color::LightGreen => format!("{}", base + 62),
            Color::LightYellow => format!("{}", base + 63),
            Color::LightBlue => format!("{}", base + 64),
            Color::LightMagenta => format!("{}", base + 65),
            Color::LightCyan => format!("{}", base + 66),
            Color::White => format!("{}", base + 67),
            _ => format!("{}", base + 9),
        }
    };
    let mut out = String::new();
    for y in 0..h {
        // One escape per RUN, not per cell: a cell-by-cell dump is twenty
        // times the bytes and unreadable in a diff.
        let mut worn = String::new();
        for x in 0..w {
            let cell = &buf[(x, y)];
            let mut parts = vec![code(cell.fg, 30), code(cell.bg, 40)];
            if cell.modifier.contains(Modifier::BOLD) {
                parts.push("1".to_string());
            }
            let style = parts.join(";");
            if style != worn {
                out.push_str(&format!("\x1b[0;{style}m"));
                worn = style;
            }
            out.push_str(cell.symbol());
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

/// The group map float, folded, over a document with more files than the
/// selected group touches.
#[test]
#[ignore = "prints the pane for a human to look at"]
fn render_dump_map() {
    let r = TestRepo::new();
    let files = [
        "deep/a/b/c/buried.rs",
        "src/one.rs",
        "src/two.rs",
        "src/three.rs",
        "src/four.rs",
        "src/five.rs",
        "src/target.rs",
    ];
    let shapes = [
        ("fn f() { g(); }\n", "fn f() { h(); }\n"),
        ("let x = 1;\n", "let x = 2;\n"),
        ("struct S { a: u8 }\n", "struct S { a: u16 }\n"),
        ("use a::b;\n", "use a::c;\n"),
        ("const K: u8 = 1;\n", "const K: u8 = 2;\n"),
        ("impl T for S {}\n", "impl U for S {}\n"),
        ("enum E { A, B }\n", "enum E { A, C }\n"),
    ];
    for (path, (before, _)) in files.iter().zip(shapes) {
        r.write(path, before.as_bytes());
    }
    r.commit_all("base");
    for (path, (_, after)) in files.iter().zip(shapes) {
        r.write(path, after.as_bytes());
    }
    r.commit_all("head");
    let backend = FakeBackend::new("fake", |ids| {
        let groups: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(n, id)| json_group(&format!("Group {n}"), "focus", &[id.as_str()]))
            .collect();
        format!(r#"{{"groups": [{}]}}"#, groups.join(", "))
    });
    let mut app = open_app_with(&r, &backend, ".dfr-map-dump-store");
    app.focus = Focus::Groups;
    for _ in 0..files.len() {
        if drawn_rows(&mut app)
            .iter()
            .any(|l| l.contains("● target.rs"))
        {
            break;
        }
        app.handle_key(key('j'));
    }
    println!("\n=== group map, folded ===");
    println!("{}", ansi_dump(&mut app, 120, 20));
}

/// The cursor's bar sits just inside the frame on EVERY selectable row — a
/// header, a fold and a boundary have no line-number block to brighten, and
/// the row tint alone was too faint to find.
#[test]
fn the_cursor_bar_shows_on_rows_that_have_no_gutter() {
    // The column just inside the detail pane's left border, at width 100.
    const BAR_X: usize = 41;
    type Case = (&'static str, fn(&RowKind) -> bool);
    let cases: [Case; 2] = [
        ("a context boundary", |k| {
            matches!(k, RowKind::ContextEdge { .. })
        }),
        ("a hunk header", |k| matches!(k, RowKind::HunkHeader { .. })),
    ];
    let (_r, mut app) = app_with_a_long_file();
    for (name, pred) in cases {
        put_cursor_on(&mut app, pred);
        let rows = drawn_rows(&mut app);
        let y = cursor_screen_row(&app) as usize;
        assert_eq!(
            rows[y].chars().nth(BAR_X),
            Some('▌'),
            "no cursor bar on {name}: {:?}",
            rows[y]
        );
    }

    // A fold row: the skim group's remainder, which no fixture above has.
    let (_r, mut app) = make_app();
    app.handle_key(key('j'));
    put_cursor_on(&mut app, |k| *k == RowKind::Fold);
    let rows = drawn_rows(&mut app);
    let y = cursor_screen_row(&app) as usize;
    assert_eq!(
        rows[y].chars().nth(BAR_X),
        Some('▌'),
        "no cursor bar on a fold row: {:?}",
        rows[y]
    );
}

/// The footer is two pills and two keys: what the review stands at, and the
/// way to the full key list. Everything else it used to name lives in `?`.
#[test]
fn the_footer_is_pills_on_the_left_and_two_keys_on_the_right() {
    let (_r, mut app) = make_app();
    let rows = drawn_rows(&mut app);
    let footer = rows.last().expect("no footer row").clone();

    assert!(
        footer.contains("classes reviewed") && footer.contains("finding"),
        "the tallies must still be there: {footer:?}"
    );
    assert!(
        footer.trim_end().ends_with("q quit"),
        "the keys belong against the right edge: {footer:?}"
    );
    for gone in [
        "j/k",
        "n/N",
        "space reviewed",
        "s split",
        "v files",
        "z fold",
    ] {
        assert!(
            !footer.contains(gone),
            "{gone} moved to the help modal: {footer:?}"
        );
    }
    // Whatever left the footer has to be reachable, so `?` has to name it.
    app.handle_key(key('?'));
    let help = drawn_as_is(&mut app);
    for key_name in ["j/k", "n/N", "space", "s", "v", "z"] {
        assert!(help.contains(key_name), "`?` must still list {key_name}");
    }

    // A pill, not a run of grey words: the tally sits on the pill's fill.
    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    app.mode = Mode::Normal;
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let (_, fill) = THEME.pill();
    assert!(
        (0..100u16).any(|x| buf[(x, 39)].bg == fill),
        "the tallies must wear the pill's fill"
    );
}

/// Standing ON a hunk's header lights its pill's leading cell in the hunk's
/// own accent. The cursor's bar sits in that same column, so drawing it there
/// would repaint green (reviewed) or muted cyan (foreign) as plain cyan — and
/// say the hunk was neither.
#[test]
fn the_cursor_on_a_hunk_header_keeps_the_hunks_own_accent() {
    let (_r, mut app) = app_with_a_long_file();
    // Reviewed, so the accent is green: an unread own hunk wears the cursor's
    // own cyan already, and the collision would prove nothing.
    let header = put_cursor_on(&mut app, |k| matches!(k, RowKind::HunkHeader { .. }));
    app.handle_key(key(' '));
    app.cursor = header;
    let buf = buffer_of(&app);
    let y = cursor_screen_row(&app);
    assert_eq!(buf[(41, y)].symbol(), "▌", "no marker on the header");
    assert_eq!(
        buf[(41, y)].style().fg,
        Some(THEME.reviewed_fg),
        "the header's leading cell must keep the hunk's accent"
    );
}

/// `z` acts on the pane it is pressed in. `self.cursor` is a DIFF row wherever
/// the focus is, so a press in the file tree used to open whatever the diff's
/// cursor happened to be parked on.
#[test]
fn z_in_the_file_tree_folds_the_tree_not_the_diff() {
    use differential_tui::app::TreeKind;
    let (_r, mut app) = make_app();
    switch_left_pane(&mut app);
    assert_eq!(app.view_mode, ViewMode::Files);

    // Park the diff's cursor on a boundary row, then press `z` in the tree.
    let boundary = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::ContextEdge { .. }));
    if let Some(b) = boundary {
        app.cursor = b;
    }
    let rows_before = app.rows.len();
    let tree_before = app.tree.len();

    app.focus = Focus::Groups;
    app.selected_file = app
        .tree
        .iter()
        .position(|e| matches!(e.kind, TreeKind::Dir { .. }))
        .expect("a directory row");
    app.handle_key(key('z'));

    assert!(
        app.tree.len() < tree_before,
        "the directory should have folded"
    );
    assert_eq!(
        app.rows.len(),
        rows_before,
        "the diff should not have moved"
    );
}

/// A control has to say how to work it — but a screenful of bands each naming
/// the same key is a wall. The key shows on the cursor's row only.
#[test]
fn a_boundary_names_its_key_on_the_cursors_row_only() {
    let (_r, mut app) = app_with_a_long_file();
    let pos = put_cursor_on(&mut app, |k| matches!(k, RowKind::ContextEdge { .. }));
    let rows = drawn_rows(&mut app);
    let here = cursor_screen_row(&app) as usize;
    assert!(
        rows[here].contains("lines hidden") && rows[here].contains("z shows"),
        "the cursor's band must name its key: {:?}",
        rows[here]
    );

    // Every other band says what it hides and nothing more.
    let elsewhere: Vec<&String> = rows
        .iter()
        .enumerate()
        .filter(|(y, r)| *y != here && r.contains("lines hidden"))
        .map(|(_, r)| r)
        .collect();
    assert!(!elsewhere.is_empty(), "the fixture needs a second band");
    for row in elsewhere {
        assert!(
            !row.contains("z shows"),
            "a band off the cursor names no key: {row:?}"
        );
    }

    // The key follows the label rather than sitting out at the pane's edge.
    let text = &rows[here];
    let label = text.find("lines hidden").expect("the label");
    let key = text.find("z shows").expect("the key");
    assert!(key > label, "the key follows the label: {text:?}");
    assert!(
        key - label < 24,
        "the key should sit with the label, not a screen away: {text:?}"
    );

    // The band still fills the pane: the hint eats padding, not width.
    let width = text.chars().count();
    app.cursor = app
        .rows
        .iter()
        .enumerate()
        .position(|(i, r)| i != pos && r.kind.selectable())
        .expect("another selectable row");
    let after = drawn_rows(&mut app);
    assert_eq!(
        after[here].chars().count(),
        width,
        "showing the key must not change the row's width"
    );
}

/// A context boundary is a control, and its band carries its own colour the
/// whole way across — so the row tint that marks the cursor elsewhere never
/// showed through it. On the cursor's row the band lightens instead.
#[test]
fn a_context_boundary_band_lightens_under_the_cursor() {
    let (_r, mut app) = app_with_a_long_file();
    let pos = put_cursor_on(&mut app, |k| matches!(k, RowKind::ContextEdge { .. }));
    let y = cursor_screen_row(&app);
    let lit = row_backgrounds(&mut app, y);
    assert!(
        lit.contains(&THEME.hint_cursor_bg),
        "the band under the cursor must lighten: {lit:?}"
    );

    // The same row, with the cursor elsewhere, keeps the muted band.
    app.cursor = app
        .rows
        .iter()
        .enumerate()
        .position(|(i, r)| i != pos && r.kind.selectable())
        .expect("another selectable row");
    let muted = row_backgrounds(&mut app, y);
    assert!(
        !muted.contains(&THEME.hint_cursor_bg),
        "only the cursor's band lightens: {muted:?}"
    );
}

/// A split diff over a pure insertion: one side is hatched, and the cursor's
/// block still lands in the same column on both.
#[test]
#[ignore = "prints the pane for a human to look at"]
fn render_dump_hatch() {
    let r = TestRepo::new();
    r.write("src/a.rs", b"let keep = 1;\nlet tail = 3;\n");
    r.commit_all("base");
    r.write(
        "src/a.rs",
        b"let keep = 1;\nlet fresh = 2;\nlet tail = 3;\n",
    );
    r.commit_all("head");
    let backend = skim_first_backend();
    let mut app = open_app_with(&r, &backend, ".dfr-hatch-dump-store");
    app.handle_key(key('s'));
    put_cursor_on(&mut app, |k| matches!(k, RowKind::Diff(_)));
    for _ in 0..20 {
        let y = cursor_screen_row(&app);
        if row_backgrounds(&mut app, y).iter().any(is_cursor_block) {
            break;
        }
        app.handle_key(key('j'));
    }
    println!("\n=== split over an insertion ===");
    println!("{}", ansi_dump(&mut app, 120, 16));
}

/// The plan pane's connector, with a group that follows two others.
#[test]
#[ignore = "prints the pane for a human to look at"]
fn render_dump_plan() {
    let (_r, mut app) = app_with_dependency_edge();
    app.focus = Focus::Groups;
    let follower = (0..app.groups().len())
        .find(|i| !app.groups()[*i].depends_on.is_empty())
        .expect("no group follows another");
    while app.selected_group != follower {
        app.handle_key(key(if app.selected_group < follower {
            'j'
        } else {
            'k'
        }));
    }
    println!("\n=== plan connector ===");
    println!("{}", ansi_dump(&mut app, 120, 20));
}

/// A note stays on the line it was written on when the layout changes.
///
/// A modification is TWO rows in unified — the removed line and the added one
/// — and ONE in split. A note written on the removed half anchors to the old
/// side, and in split there was no old-side row to hold it, so it fell back to
/// the hunk's header on every `s`.
#[test]
fn a_note_survives_the_diff_layout_it_was_written_in() {
    let r = TestRepo::new();
    // 18 lines inserted at the top, so old and new numbers differ throughout.
    let body = |lead: usize, change: &str| -> Vec<u8> {
        let mut out = String::new();
        for i in 1..=lead {
            out.push_str(&format!("inserted{i} = {i}\n"));
        }
        for i in 1..=40 {
            if i == 30 {
                out.push_str(change);
            } else {
                out.push_str(&format!("keep{i} = {i}\n"));
            }
        }
        out.into_bytes()
    };
    r.write("src/f.rs", &body(0, "target = 1\n"));
    r.commit_all("base");
    r.write("src/f.rs", &body(18, "target = 2\n"));
    r.commit_all("head");

    let backend = skim_first_backend();
    let mut app = open_app_with(&r, &backend, ".dfr-layout-note-store");
    app.focus = Focus::Detail;

    // The two halves of the modification, in the unified layout.
    let halves: Vec<usize> = app
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.line
                .as_ref()
                .is_some_and(|l| l.text.starts_with("target"))
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(halves.len(), 2, "a modification is two rows in unified");
    let removed = app.rows[halves[0]].line.clone().unwrap();
    assert_eq!(removed.side, "old", "the first half is the removed line");

    // Write a note on the removed half, then switch layout.
    app.cursor = halves[0];
    app.handle_key(key('c'));
    app.handle_paste("about the old line");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let anchor = app.session.findings()[0].anchor.clone();
    assert_eq!((anchor.side.as_str(), anchor.line), ("old", removed.line));

    let sits_on_its_line = |app: &App| {
        let at = app
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Finding(..)))
            .expect("a note row");
        app.rows[at - 1]
            .line
            .as_ref()
            .is_some_and(|l| l.holds(&anchor.side, anchor.end_line))
    };
    assert!(sits_on_its_line(&app), "unified: the note left its line");
    app.handle_key(key('s'));
    assert!(
        sits_on_its_line(&app),
        "split: the note should still sit on the line it annotates"
    );
    app.handle_key(key('s'));
    assert!(sits_on_its_line(&app), "and back again");
}
