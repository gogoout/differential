//! What the graph asks its readers, and what it does with the answers.
//!
//! Three properties, all of them domain behaviour rather than extraction:
//!
//! 1. A reader is handed the WHOLE FILE, not the hunk. Both fixtures here turn
//!    on a line whose `/*` opener is unchanged, and so absent from the hunk.
//!    Nothing short of the file can classify it.
//! 2. A reader's decision reaches the edges. The graph does not second-guess it.
//! 3. A file no reader claims contributes nothing — and the domain never
//!    substitutes one reader's answer for another's.

use std::sync::{Arc, Mutex};

use differential_engine::artefact::symbols::{FileSymbols, SymbolReaders, SymbolSource};
use differential_engine::config::Config;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_pipeline;
use differential_engine::schema::SourceKind;
use differential_testutil::{StubSymbols, TestRepo};

/// What a reader was handed, per call: the path, and the content length.
type Seen = Arc<Mutex<Vec<(Vec<u8>, usize)>>>;

/// Records its arguments, then answers exactly as the stub would — so
/// registering it cannot change an edge.
struct Recorder(Seen);

impl SymbolSource for Recorder {
    fn priority(&self, _path: &[u8]) -> Option<u8> {
        Some(5)
    }
    fn file_symbols(&self, path: &[u8], content: &[u8]) -> Option<FileSymbols> {
        self.0.lock().unwrap().push((path.to_vec(), content.len()));
        StubSymbols.file_symbols(path, content)
    }
    fn fingerprint(&self) -> String {
        "test-recorder-v1".to_string()
    }
}

/// Drops every symbol on a line inside a `/* … */` comment.
///
/// Deliberately a crude state machine: it stands in for a real parser, and its
/// only job is to be unable to work without the whole file.
struct BlockCommentAware;

impl SymbolSource for BlockCommentAware {
    fn priority(&self, _path: &[u8]) -> Option<u8> {
        Some(9)
    }
    fn file_symbols(&self, path: &[u8], content: &[u8]) -> Option<FileSymbols> {
        let mut out = StubSymbols.file_symbols(path, content)?;
        let mut inside = false;
        for (i, line) in content.split(|&b| b == b'\n').enumerate() {
            let opens = has(line, b"/*");
            let closes = has(line, b"*/");
            if inside || opens {
                out.defines[i] = Vec::new();
                out.references[i] = Vec::new();
            }
            inside = if closes { false } else { inside || opens };
        }
        Some(out)
    }
    fn fingerprint(&self) -> String {
        "test-block-comment-v1".to_string()
    }
}

fn has(line: &[u8], needle: &[u8; 2]) -> bool {
    line.windows(2).any(|w| w == needle)
}

fn readers(extra: Option<Box<dyn SymbolSource>>) -> SymbolReaders {
    let mut r = SymbolReaders::default();
    r.register(Box::new(StubSymbols));
    if let Some(e) = extra {
        r.register(e);
    }
    r
}

const A_HEAD: &[u8] = b"// a\nfn widget_maker() {}\n";
const B_HEAD: &[u8] = b"/*\n * see widget_maker for details\n */\n";

/// A definition in one file, and a mention of it inside a block comment whose
/// opener predates the change. `src/b.rs` gains exactly ONE line.
fn corpus() -> (TestRepo, String, String) {
    let r = TestRepo::new();
    r.write("src/a.rs", b"// a\n");
    r.write("src/b.rs", b"/*\n */\n");
    let base = r.commit_all("base");
    r.write("src/a.rs", A_HEAD);
    r.write("src/b.rs", B_HEAD);
    let head = r.commit_all("head");
    (r, base, head)
}

/// Total class edges, and every symbol the graph says is defined.
fn graph(symbols: &SymbolReaders, r: &TestRepo, base: &str, head: &str) -> (usize, Vec<String>) {
    let out = run_pipeline(
        &r.repo(),
        base,
        head,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        symbols,
    )
    .unwrap();
    let doc = out.document.expect("document");
    let edges = doc.classes.iter().map(|c| c.depends_on.len()).sum();
    let mut defines: Vec<String> = doc.classes.iter().flat_map(|c| c.defines.clone()).collect();
    defines.sort();
    (edges, defines)
}

#[test]
fn a_reader_is_handed_the_whole_file_and_its_path() {
    let (r, base, head) = corpus();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let with_recorder = readers(Some(Box::new(Recorder(Arc::clone(&seen)))));

    // The recorder wraps the stub, so the answer must not move. This test is
    // about the arguments.
    assert_eq!(
        graph(&with_recorder, &r, &base, &head),
        graph(&readers(None), &r, &base, &head)
    );

    let seen = seen.lock().unwrap();
    let len_of = |p: &[u8]| seen.iter().find(|(q, _)| q == p).map(|(_, n)| *n);
    // `src/b.rs` added ONE line. A reader handed the hunk would see 32 bytes;
    // handed the file it sees all 39, including the `/*` two lines up.
    assert_eq!(len_of(b"src/b.rs"), Some(B_HEAD.len()));
    assert_eq!(len_of(b"src/a.rs"), Some(A_HEAD.len()));
    assert_eq!(
        seen.len(),
        2,
        "one read per file, not one per class or hunk"
    );
}

#[test]
fn a_reader_can_drop_a_reference_the_hunk_alone_could_not_classify() {
    let (r, base, head) = corpus();

    // The stub alone: the mention inside the comment is just an identifier, so
    // it produces an edge. On the validation corpus, comments and strings were
    // 44.5% of all reference tokens.
    let (stub_edges, stub_defines) = graph(&readers(None), &r, &base, &head);
    assert_eq!(stub_edges, 1, "the comment mention produced an edge");
    assert!(stub_defines.contains(&"widget_maker".to_string()));

    // A comment-aware reader outranks it, so no edge. The definition survives,
    // because it is not in a comment — a narrower reference set, not a smaller
    // graph.
    let (aware_edges, aware_defines) = graph(
        &readers(Some(Box::new(BlockCommentAware))),
        &r,
        &base,
        &head,
    );
    assert_eq!(aware_edges, 0, "the comment mention is not a reference");
    assert_eq!(aware_defines, stub_defines, "definitions are untouched");
}

/// A gitlink's pseudo-hunk is `Subproject commit <oid>` — diff prose about a
/// commit this repository does not have, not code. It has an added line, so it
/// used to reach the heuristics and its words became references.
///
/// `Subproject` is a plausible identifier, which is what makes this observable:
/// define it in real code and the submodule bump appears to consume it.
#[test]
fn a_gitlink_contributes_no_symbols() {
    let r = TestRepo::new();
    let sha_a = "a".repeat(40);
    let sha_b = "b".repeat(40);
    // The index is driven by hand: `git add -A` would evict a gitlink whose
    // submodule is not checked out, which is what the pseudo-hunk needs.
    r.write("src/a.rs", b"// a\n");
    r.git(&["add", "src/a.rs"]);
    r.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{sha_a},vendor/dep"),
    ]);
    r.git(&["commit", "-q", "-m", "base"]);
    let base = r.git(&["rev-parse", "HEAD"]);

    r.write("src/a.rs", b"// a\nfn Subproject() {}\n");
    r.git(&["add", "src/a.rs"]);
    r.git(&[
        "update-index",
        "--cacheinfo",
        &format!("160000,{sha_b},vendor/dep"),
    ]);
    r.git(&["commit", "-q", "-m", "bump"]);
    let head = r.git(&["rev-parse", "HEAD"]);

    let (edges, defines) = graph(&readers(None), &r, &base, &head);
    assert!(
        defines.contains(&"Subproject".to_string()),
        "the real definition is still found"
    );
    assert_eq!(edges, 0, "the pseudo-hunk's prose is not a reference");
}

/// The rule that removes 32% of the corpus's edges.
#[test]
fn a_file_no_reader_claims_contributes_nothing() {
    let r = TestRepo::new();
    r.write("src/a.rs", b"// a\n");
    r.write("notes.md", b"notes\n");
    let base = r.commit_all("base");
    r.write("src/a.rs", b"// a\nfn widget_maker() {}\n");
    r.write("notes.md", b"notes\nsee widget_maker for details\n");
    let head = r.commit_all("head");

    // The stub claims everything, so the prose links to the definition.
    let (claimed, _) = graph(&readers(None), &r, &base, &head);
    assert_eq!(claimed, 1, "prose became a reference");

    // A reader that declines `.md` leaves nobody to claim it. The domain does
    // not fall back to something cruder — it takes the silence.
    struct CodeOnly;
    impl SymbolSource for CodeOnly {
        fn priority(&self, path: &[u8]) -> Option<u8> {
            path.ends_with(b".rs").then_some(1)
        }
        fn file_symbols(&self, path: &[u8], content: &[u8]) -> Option<FileSymbols> {
            StubSymbols.file_symbols(path, content)
        }
        fn fingerprint(&self) -> String {
            "test-code-only-v1".to_string()
        }
    }
    let mut only = SymbolReaders::default();
    only.register(Box::new(CodeOnly));
    let (unclaimed, defines) = graph(&only, &r, &base, &head);
    assert_eq!(unclaimed, 0, "prose contributes nothing");
    assert!(
        defines.contains(&"widget_maker".to_string()),
        "the code is still read"
    );
}
