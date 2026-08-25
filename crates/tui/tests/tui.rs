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
use differential_tui::app::{App, Effect, Focus, Mode, ReviewOptions, Viewport};
use differential_tui::rows::{RowFactory, RowKind};
use differential_tui::window::Side;

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
    assert_eq!(app.focus, Focus::Diff);
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
    assert!(matches!(app.mode, Mode::Editing(_, _)));
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

    app.handle_key(key('v'));
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
            .any(|row| matches!(row.kind, RowKind::HunkHeader(_)))
    );

    // space in file view marks the class — visible back in group view too.
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(key(' '));
    assert_eq!(app.session.reviewed_count(), 1);
    app.handle_key(key('v'));
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

    app.handle_key(key('v'));
    let hunk_headers = app
        .rows
        .iter()
        .filter(|row| matches!(row.kind, RowKind::HunkHeader(_)))
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
    app.handle_key(key('v'));
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
    // Group 1 ("Close work" or the skim sweep) — use whichever is selected;
    // ensure some file headers exist in the current rows.
    app.handle_key(key('f'));
    let (n_entries, first_path) = match &app.mode {
        Mode::FileList { entries, .. } => (entries.len(), entries[0].path.clone()),
        _ => panic!("f should open the file list"),
    };
    assert!(n_entries >= 1);

    // Enter jumps the cursor to (the first selectable after) that header.
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.focus, Focus::Diff);
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
        .filter(|(_, r)| matches!(r.kind, RowKind::HunkHeader(_)))
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
    app.handle_key(key('v'));
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
        .filter(|r| matches!(r.kind, RowKind::HunkHeader(_)))
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
        !content.contains("├"),
        "nothing is followed from here, so no connector should be drawn"
    );
}

/// Render at a fixed size and flatten the buffer to text.
fn drawn(app: &mut App) -> String {
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
        diff_rows: 8,
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
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
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
        diff_rows: tall,
        plan_rows: tall,
    });
    app.handle_key(key('G'));
    assert_eq!(app.scroll(), 0, "everything fits, so nothing scrolled");

    // Now shrink — above MIN_VIEWPORT, so the floor cannot mask it. No key is
    // pressed and nothing is drawn between here and the assertion.
    app.set_viewport(Viewport {
        diff_rows: SHORT,
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
            RowKind::ContextEdge(h, side) => Some((h, side)),
            _ => None,
        })
        .collect()
}

fn diff_rows(app: &App) -> usize {
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
    app.focus = Focus::Diff;
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
    let rows_before = diff_rows(&app);
    let text = drawn(&mut app);
    assert!(
        text.contains("more above"),
        "the boundary says what is hidden"
    );
    assert!(text.contains("z shows"), "the boundary says how to open it");

    // Stand on the first upward boundary and open it.
    put_cursor_on(&mut app, |k| matches!(k, RowKind::ContextEdge(_, Side::Up)));
    app.handle_key(key('z'));

    assert_eq!(
        diff_rows(&app),
        rows_before + ReviewOptions::default().context_step,
        "z pulls in exactly one step"
    );
    // Growing upward inserts rows above, so the cursor has to have followed
    // its boundary row rather than kept its index.
    assert!(matches!(
        app.rows[app.cursor].kind,
        RowKind::ContextEdge(_, Side::Up)
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
        put_cursor_on(&mut app, |k| *k == RowKind::ContextEdge(hunk, Side::Up));
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

    for _ in 0..4 {
        if !edges(&app)
            .iter()
            .any(|(h, s)| *h == lower && *s == Side::Up)
        {
            break;
        }
        put_cursor_on(&mut app, |k| *k == RowKind::ContextEdge(lower, Side::Up));
        app.handle_key(key('z'));
    }

    // One block: exactly one upward and one downward boundary survive, and no
    // line of the file is drawn twice.
    let e = edges(&app);
    assert_eq!(
        e.iter().filter(|(_, s)| *s == Side::Up).count(),
        1,
        "merged windows share one top boundary, got {e:?}"
    );
    assert_eq!(e.iter().filter(|(_, s)| *s == Side::Down).count(), 1);
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
            .filter(|r| matches!(r.kind, RowKind::HunkHeader(_)))
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
    // The diff pane starts at column 40; its border is 40, inner 41..=98.
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
    let (_r, app) = app_with_a_long_file();
    // A changed row is one whose gutter block is painted; the last inner
    // column of the pane must then carry the line's own background too.
    let row = (1..39u16)
        .find(|&y| {
            let cells = diff_pane_row(&app, y);
            painted(cells[1].1).is_some() && painted(cells.last().unwrap().1).is_some()
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

#[test]
fn the_cursor_stays_visible_on_a_changed_row() {
    let (_r, mut app) = app_with_a_long_file();
    // A changed line carries its own background, and a line style sits under
    // span styles — so only the marker can show the cursor there.
    put_cursor_on(&mut app, |k| matches!(k, RowKind::Diff(_)));
    // Walk onto a row that actually has a change colour.
    for _ in 0..20 {
        if drawn(&mut app).contains('▸') {
            break;
        }
        app.handle_key(key('j'));
    }
    assert!(
        drawn(&mut app).contains('▸'),
        "the cursor must be visible on a diff row"
    );
}

#[test]
fn a_context_boundary_row_is_not_a_hunk_for_marking_or_findings() {
    let (_r, mut app) = app_with_a_long_file();
    put_cursor_on(&mut app, |k| matches!(k, RowKind::ContextEdge(_, _)));
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
        put_cursor_on(&mut app, |k| *k == RowKind::ContextEdge(lower, Side::Up));
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
    let text = drawn(&mut app);
    assert!(
        !text.contains("@@"),
        "the diff-syntax coordinates should be gone"
    );
    // What the header uniquely says survives: the shape class and the change's
    // size. Each hunk here replaces one line with one line.
    assert!(text.contains("+1"), "the added count: {text}");
    assert!(text.contains("−1"), "the removed count");
    // Still a selectable row, so n/N, space and c keep working on it.
    let header = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::HunkHeader(_)))
        .expect("a hunk header row");
    assert!(app.rows[header].kind.selectable());
    assert!(app.rows[header].kind.hunk().is_some());
}

/// A row that is about the whole file runs across the whole pane. Left as a
/// bare line it stopped at its last character, which in split mode punched a
/// hole in the separator column.
#[test]
fn headers_and_boundaries_rule_across_both_columns() {
    let (_r, mut app) = app_with_a_long_file();
    app.handle_key(key('s')); // split

    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();

    // Rows carrying a rule reach the last inner column of the diff pane (98).
    let ruled: Vec<u16> = (1..39u16)
        .filter(|&y| buf[(45, y)].symbol() == "─")
        .collect();
    assert!(!ruled.is_empty(), "no ruled row drawn");
    for y in ruled {
        assert_eq!(
            buf[(98, y)].symbol(),
            "─",
            "row {y} stops before the pane edge"
        );
        assert_ne!(
            buf[(69, y)].symbol(),
            "│",
            "a ruled row crosses the split separator rather than being cut by it"
        );
    }
    // And the separator really is at that column on ordinary rows, so the
    // assertion above is about crossing it rather than about it never existing.
    assert!(
        (1..39u16).any(|y| buf[(69, y)].symbol() == "│"),
        "no split separator found at column 69"
    );
}

/// Not an assertion — a readable dump of the pane, so the styling can be
/// eyeballed with `cargo test -- --ignored --nocapture render_dump`.
#[test]
#[ignore = "prints the pane for a human to look at"]
fn render_dump() {
    let (_r, mut app) = app_with_a_long_file();
    app.focus = Focus::Diff;
    for mode in ["unified", "split"] {
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buf = terminal.backend().buffer().clone();
        println!("\n=== {mode} ===");
        for y in 0..30u16 {
            let line: String = (0..120u16).map(|x| buf[(x, y)].symbol()).collect();
            println!("{line}");
        }
        app.handle_key(key('s'));
    }
}
