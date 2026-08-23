//! TUI model tests: key events → state transitions + a TestBackend draw smoke
//! test. No real terminal, no real LLM.

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use differential::tui::app::{App, Effect, Focus, Mode};
use differential::tui::rows::{RowFactory, RowKind};
use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_grouped_pipeline;
use differential_engine::review_state::ReviewState;
use differential_llm::{LlmBackend, LlmError};
use differential_schema::SourceKind;
use tempfile::TempDir;

// Minimal local copies of the engine's test helpers (test modules are not
// importable across crates).
struct TestRepo {
    _tmp: TempDir,
    root: std::path::PathBuf,
}

impl TestRepo {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let r = TestRepo { _tmp: tmp, root };
        r.git(&["init", "-q", "-b", "main"]);
        r
    }
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=test", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    fn write(&self, path: &str, content: &str) {
        let p = self.root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    fn commit_all(&self, msg: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
        self.git(&["rev-parse", "HEAD"])
    }
}

struct FakeBackend(Mutex<String>);
impl LlmBackend for FakeBackend {
    fn name(&self) -> &str {
        "fake"
    }
    fn complete(&self, prompt: &str) -> Result<String, LlmError> {
        // First-listed class (the largest) becomes the skim sweep; the rest
        // are close work — so the skim group has a foldable remainder.
        let ids: Vec<&str> = prompt
            .lines()
            .filter_map(|l| {
                let rest = l.strip_prefix('[')?;
                let id = &rest[..rest.find(']')?];
                id.starts_with('C').then_some(id)
            })
            .collect();
        let skim = ids.first().copied().unwrap_or("C0");
        let closes: Vec<String> = ids[1..].iter().map(|c| format!("\"{c}\"")).collect();
        let mut groups = vec![format!(
            r#"{{"label": "Skim sweep", "description": "d", "classes": ["{skim}"], "effort": "skim", "reason": "r"}}"#
        )];
        if !closes.is_empty() {
            groups.push(format!(
                r#"{{"label": "Close work", "description": "d", "classes": [{}], "effort": "close", "reason": "r"}}"#,
                closes.join(", ")
            ));
        }
        *self.0.lock().unwrap() = prompt.to_string();
        Ok(format!(r#"{{"groups": [{}]}}"#, groups.join(", ")))
    }
}

/// Repo with one behavioural change + a 3-file repeated edit (skim material).
fn make_app() -> (TestRepo, App) {
    let r = TestRepo::new();
    r.write("src/main.txt", "fn main() { run_slowly() }\n");
    for n in ["a", "b", "c"] {
        r.write(
            &format!("src/{n}.txt"),
            "use old_helper_name;\nother content here\n",
        );
    }
    let base = r.commit_all("base");
    r.write("src/main.txt", "fn main() { run_with_retries(3) }\n");
    for n in ["a", "b", "c"] {
        r.write(
            &format!("src/{n}.txt"),
            "use new_helper_name;\nother content here\n",
        );
    }
    let head = r.commit_all("head");

    let repo = Repo::open(Path::new(&r.root)).unwrap();
    let backend = FakeBackend(Mutex::new(String::new()));
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
        },
    )
    .unwrap();
    let factory = RowFactory::new(repo, out.base.clone(), out.head.clone());
    let app = App::new(
        out.document.unwrap(),
        out.view,
        "planhash".into(),
        factory,
        ReviewState::default(),
        Vec::new(),
    );
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
fn space_toggles_class_reviewed_and_saves() {
    let (_r, mut app) = make_app();
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let effects = app.handle_key(key(' '));
    assert!(effects.contains(&Effect::SaveState));
    assert_eq!(app.state.reviewed_classes.len(), 1);
    // Toggling again clears it.
    app.handle_key(key(' '));
    assert!(app.state.reviewed_classes.is_empty());
}

#[test]
fn finding_lifecycle_add_yank_delete() {
    let (_r, mut app) = make_app();
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    // c opens the editor on the current hunk.
    app.handle_key(key('c'));
    assert!(matches!(app.mode, Mode::Editing(_, _)));
    for ch in "off by one".chars() {
        app.handle_key(key(ch));
    }
    let effects = app.handle_key(ctrl('s'));
    assert!(effects.contains(&Effect::SaveFindings));
    assert_eq!(app.findings.len(), 1);
    assert_eq!(app.findings[0].body, "off by one");
    assert!(!app.findings[0].anchor.hunk_digest.is_empty());

    // The finding renders as a row and the summary contains it.
    assert!(
        app.rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Finding(_, _)))
    );
    let effects = app.handle_key(key('y'));
    match effects.first() {
        Some(Effect::Yank(text)) => {
            assert!(text.contains("off by one"));
            assert!(text.contains(":"));
        }
        other => panic!("expected yank, got {other:?}"),
    }

    // dd on the finding row deletes it.
    let finding_row = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Finding(_, _)))
        .unwrap();
    app.cursor = finding_row;
    app.handle_key(key('d'));
    let effects = app.handle_key(key('d'));
    assert!(effects.contains(&Effect::SaveFindings));
    assert!(app.findings.is_empty());
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
    assert!(app.findings.is_empty());
}

#[test]
fn quit_saves_state() {
    let (_r, mut app) = make_app();
    let effects = app.handle_key(key('q'));
    assert_eq!(effects, vec![Effect::SaveState, Effect::Quit]);
}

#[test]
fn draw_smoke_test_renders_group_label() {
    let (_r, mut app) = make_app();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let content: String = buffer.content().iter().map(|c| c.symbol()).collect();
    assert!(content.contains("Close work"));
    assert!(content.contains("reading plan"));
    assert!(content.contains("classes reviewed"));
}
