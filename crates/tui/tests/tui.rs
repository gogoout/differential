//! TUI model tests: key events → state transitions + a TestBackend draw smoke
//! test. No real terminal, no real LLM.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use differential_engine::ReviewSession;
use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_grouped_pipeline;
use differential_engine::schema::SourceKind;
use differential_testutil::{FakeBackend, TestRepo, json_group};
use differential_tui::app::{App, Effect, Focus, Mode};
use differential_tui::rows::{RowFactory, RowKind};

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
            backend: Some(&backend),
            cache_dir: None,
            progress: None,
            cancel: None,
        },
    )
    .unwrap();
    let factory = RowFactory::new(repo, out.base.clone(), out.head.clone());
    let session = ReviewSession::open_at(
        r.root.join(".dfr-test-store"),
        out.document.unwrap(),
        out.view,
    )
    .unwrap();
    App::new(session, factory)
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
    assert_eq!(app.groups.len(), 2);
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
    let store =
        differential_engine::review_state::ReviewStore::open_at(r.root.join(".dfr-test-store"))
            .unwrap();
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
    let store =
        differential_engine::review_state::ReviewStore::open_at(r.root.join(".dfr-test-store"))
            .unwrap();
    assert!(store.load_state().unwrap().cursor.is_some());
}

#[test]
fn group_counts_files_and_line_totals() {
    let (_r, app) = make_app();
    // Across both groups: 4 files, 4 hunks, each hunk one line replaced.
    let files: usize = app.groups.iter().map(|g| g.n_files).sum();
    let adds: usize = app.groups.iter().map(|g| g.adds).sum();
    let dels: usize = app.groups.iter().map(|g| g.dels).sum();
    assert_eq!(files, 4);
    assert_eq!(adds, 4);
    assert_eq!(dels, 4);
    // The 3-file repeated edit lands in one group.
    assert!(app.groups.iter().any(|g| g.n_files == 3));
}

#[test]
fn file_view_lists_all_files_and_shares_review_marks() {
    use differential_tui::app::ViewMode;
    let (r, mut app) = make_app();
    assert_eq!(app.view_mode, ViewMode::Groups);

    app.handle_key(key('v'));
    assert_eq!(app.view_mode, ViewMode::Files);
    assert_eq!(app.files.len(), 4);
    let store =
        differential_engine::review_state::ReviewStore::open_at(r.root.join(".dfr-test-store"))
            .unwrap();
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
    assert_eq!(app.groups.len(), 2, "two shapes → two groups");

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
    let store =
        differential_engine::review_state::ReviewStore::open_at(r.root.join(".dfr-test-store"))
            .unwrap();
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
    let (_r, mut app) = make_app();
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
    let (_r, mut app) = make_app();

    // Every dependency names a real group id — the id column makes them
    // resolvable, which is the whole point of showing it.
    let ids: Vec<&str> = app.groups.iter().map(|g| g.id.as_str()).collect();
    for g in &app.groups {
        for (dep, later) in &g.after {
            assert!(
                ids.contains(&dep.as_str()),
                "dependency {dep:?} is not a group id"
            );
            // The flag must agree with the plan order: it means "this
            // dependency appears further down", i.e. a cycle the toposort
            // had to break.
            let dep_pos = app.groups.iter().position(|o| &o.id == dep).unwrap();
            let self_pos = app.groups.iter().position(|o| o.id == g.id).unwrap();
            assert_eq!(
                *later,
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
        content.contains(&app.groups[0].id),
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
        .groups
        .iter()
        .enumerate()
        .max_by_key(|(_, g)| g.class_keys.len())
        .map(|(i, _)| i)
        .unwrap();
    while app.selected_group != target {
        app.handle_key(key('j'));
    }
    let want = app.groups[target].class_keys.len();
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

#[test]
fn the_plan_gutter_links_the_selected_group_to_its_neighbours() {
    use differential_tui::app::Relation;
    let (_r, mut app) = make_app();

    // Land on a group that actually has an edge, so the connector has
    // something to draw.
    let with_edge = app
        .groups
        .iter()
        .position(|g| !g.after.is_empty())
        .or_else(|| {
            let ids: Vec<String> = app.groups.iter().map(|g| g.id.clone()).collect();
            ids.iter().position(|id| {
                app.groups
                    .iter()
                    .any(|o| o.after.iter().any(|(d, _)| d == id))
            })
        });
    let Some(target) = with_edge else {
        return; // no edges in this fixture: nothing to assert
    };
    while app.selected_group != target {
        app.handle_key(key('j'));
    }

    // The relation model matches depends_on in both directions, and the
    // selected row is the anchor.
    assert_eq!(app.relation_to_selected(target), Relation::Selected);
    for (i, g) in app.groups.iter().enumerate() {
        match app.relation_to_selected(i) {
            Relation::Dependency => assert!(
                app.groups[target].after.iter().any(|(d, _)| *d == g.id),
                "{} marked as a dependency but the selected group does not follow it",
                g.id
            ),
            Relation::Dependent => assert!(
                g.after.iter().any(|(d, _)| *d == app.groups[target].id),
                "{} marked as a dependent but does not follow the selected group",
                g.id
            ),
            _ => {}
        }
    }

    // It reaches the screen.
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
    assert!(content.contains("◆"), "selected group marker missing");
}

#[test]
fn the_selected_plan_row_is_highlighted_edge_to_edge() {
    let (_r, mut app) = make_app();
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
    app.viewport_hint = 8;

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
    assert!(app.scroll > 0, "should have scrolled away from the top");
    for _ in 0..40 {
        app.handle_key(ctrl('u'));
    }
    assert_eq!(
        app.scroll, 0,
        "scrolling up must reach row 0, not stop below it"
    );

    // g (top) lands there too.
    app.handle_key(key('G'));
    app.handle_key(key('g'));
    assert_eq!(app.scroll, 0);
}
