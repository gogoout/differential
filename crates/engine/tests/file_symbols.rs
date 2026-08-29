//! The symbol hook sees the whole file, and what that buys.
//!
//! The hook used to be handed one diff line at a time. A line inside a block
//! comment is indistinguishable from code on its own, so the cut that matters
//! most — drop comment and string tokens, which are 44.5% of reference tokens
//! on a measured range — was not decidable where the decision was being made.
//!
//! Both tests turn on a line whose `/*` opener is UNCHANGED, and so absent from
//! the hunk. Nothing short of the file can classify it.

use std::sync::{Arc, Mutex};

use differential_engine::config::Config;
use differential_engine::lang::{FileSymbols, Language, LanguageRegistry, generic};
use differential_engine::pipeline::run_pipeline;
use differential_engine::schema::SourceKind;
use differential_testutil::TestRepo;

/// What the hook was handed, per call: the path, and the content length.
type Seen = Arc<Mutex<Vec<(Vec<u8>, usize)>>>;

/// Records its arguments, then answers exactly as the generic default would —
/// so registering it cannot change an edge.
struct Recorder(Seen);

impl Language for Recorder {
    fn id(&self) -> &'static str {
        "test-recorder-v1"
    }
    fn claims(&self, path: &[u8]) -> bool {
        path.ends_with(b".rs")
    }
    fn file_symbols(&self, path: &[u8], content: &[u8]) -> FileSymbols {
        self.0.lock().unwrap().push((path.to_vec(), content.len()));
        generic::file_symbols(content)
    }
}

/// Drops every symbol on a line inside a `/* … */` comment.
///
/// Deliberately a crude state machine: it stands in for a real parser, and its
/// only job is to be unable to work without the whole file.
struct BlockCommentAware;

impl Language for BlockCommentAware {
    fn id(&self) -> &'static str {
        "test-block-comment-v1"
    }
    fn claims(&self, path: &[u8]) -> bool {
        path.ends_with(b".rs")
    }
    fn file_symbols(&self, _path: &[u8], content: &[u8]) -> FileSymbols {
        let mut out = FileSymbols::default();
        let mut inside = false;
        for line in content.split(|&b| b == b'\n') {
            let opens = has(line, b"/*");
            let closes = has(line, b"*/");
            let commented = inside || opens;
            inside = if closes { false } else { inside || opens };
            let (d, r) = if commented {
                (Vec::new(), Vec::new())
            } else {
                (
                    generic::symbol_definitions(line),
                    generic::symbol_references(line),
                )
            };
            out.defines.push(d);
            out.references.push(r);
        }
        out
    }
}

fn has(line: &[u8], needle: &[u8; 2]) -> bool {
    line.windows(2).any(|w| w == needle)
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
fn graph(langs: &LanguageRegistry, r: &TestRepo, base: &str, head: &str) -> (usize, Vec<String>) {
    let out = run_pipeline(
        &r.repo(),
        base,
        head,
        SourceKind::Range,
        &Config::default(),
        langs,
    )
    .unwrap();
    let doc = out.document.expect("document");
    let edges = doc.classes.iter().map(|c| c.depends_on.len()).sum();
    let mut defines: Vec<String> = doc.classes.iter().flat_map(|c| c.defines.clone()).collect();
    defines.sort();
    (edges, defines)
}

#[test]
fn the_hook_is_handed_the_whole_file_and_its_path() {
    let (r, base, head) = corpus();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let mut langs = LanguageRegistry::builtin();
    langs.register(Box::new(Recorder(Arc::clone(&seen))));

    // The recorder wraps the generic function, so the answer must not move.
    // This test is about the arguments.
    assert_eq!(
        graph(&langs, &r, &base, &head),
        graph(&LanguageRegistry::builtin(), &r, &base, &head)
    );

    let seen = seen.lock().unwrap();
    let len_of = |p: &[u8]| seen.iter().find(|(q, _)| q == p).map(|(_, n)| *n);
    // `src/b.rs` added ONE line. A hook handed the hunk would see 32 bytes;
    // handed the file it sees all 39, including the `/*` two lines up.
    assert_eq!(len_of(b"src/b.rs"), Some(B_HEAD.len()));
    assert_eq!(len_of(b"src/a.rs"), Some(A_HEAD.len()));
    assert_eq!(
        seen.len(),
        2,
        "one parse per file, not one per class or hunk"
    );
}

#[test]
fn a_plugin_can_drop_a_reference_the_hunk_alone_could_not_classify() {
    let (r, base, head) = corpus();

    // Generic: the mention inside the comment is just an identifier, so it
    // produces an edge — the 37.3% of reference tokens the measurement
    // attributed to comments.
    let (generic_edges, generic_defines) = graph(&LanguageRegistry::builtin(), &r, &base, &head);
    assert_eq!(generic_edges, 1, "the comment mention produced an edge");
    assert!(generic_defines.contains(&"widget_maker".to_string()));

    // Comment-aware: no edge. The definition survives, because it is not in a
    // comment — a narrower reference set, not a smaller graph.
    let mut langs = LanguageRegistry::builtin();
    langs.register(Box::new(BlockCommentAware));
    let (aware_edges, aware_defines) = graph(&langs, &r, &base, &head);
    assert_eq!(aware_edges, 0, "the comment mention is not a reference");
    assert_eq!(aware_defines, generic_defines, "definitions are untouched");
}
