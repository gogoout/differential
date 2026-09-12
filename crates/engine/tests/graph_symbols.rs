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

use differential_engine::artefact::symbols::{
    FileSymbols, Scope, Symbol, SymbolReaders, SymbolSource,
};
use differential_engine::config::Config;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_pipeline;
use differential_engine::plan::ReviewSource;
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
        &ReviewSource::range(base.to_string(), head.to_string(), head.to_string()),
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

// ---------------------------------------------------- file-local symbols

/// `let name` defines, any word of four characters or more refers — and every
/// answer carries the scope it was built with.
///
/// The point is the SCOPE, not the extraction: the readers are measured where
/// they live. What the domain owns is what an answer may be compared against,
/// and the two instances of this differ in nothing else, so the edges they
/// produce differ for exactly one reason.
struct Scoped(Scope);

impl SymbolSource for Scoped {
    fn priority(&self, _path: &[u8]) -> Option<u8> {
        Some(9)
    }
    fn file_symbols(&self, _path: &[u8], content: &[u8]) -> Option<FileSymbols> {
        let scope = self.0;
        // No site: this reader is about SCOPE, and the graph never reads a
        // site. Leaving it at the default is the point — the edges below have
        // to come from the scope and nothing else.
        let sym = |name: &str| Symbol {
            name: name.as_bytes().to_vec(),
            scope,
            site: Default::default(),
        };
        let mut out = FileSymbols::default();
        for line in content.split(|&b| b == b'\n') {
            let words: Vec<&str> = std::str::from_utf8(line)
                .unwrap_or("")
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|w| !w.is_empty())
                .collect();
            out.defines.push(
                words
                    .windows(2)
                    .filter(|p| p[0] == "let")
                    .map(|p| sym(p[1]))
                    .collect(),
            );
            out.references.push(
                words
                    .iter()
                    .filter(|w| w.len() >= 4)
                    .map(|w| sym(w))
                    .collect(),
            );
        }
        Some(out)
    }
    fn fingerprint(&self) -> String {
        format!("test-scoped-{:?}-v1", self.0)
    }
}

fn scoped(scope: Scope) -> SymbolReaders {
    let mut r = SymbolReaders::default();
    r.register(Box::new(Scoped(scope)));
    r
}

/// Two files declaring the same name, and a use of it in each.
///
/// Four classes are needed — a declaration and a use per file — and two things
/// conspire against that. Adjacent added lines are ONE hunk, so an unchanged
/// line has to sit between them; and identical lines normalise to one shape
/// class across files, so the two files say the same thing in different shapes.
fn two_files_one_name() -> (TestRepo, String, String) {
    let r = TestRepo::new();
    r.write("src/a.rs", b"// a\n// keep\n");
    r.write("src/b.rs", b"// b\n// keep\n");
    let base = r.commit_all("base");
    r.write(
        "src/a.rs",
        b"// a\nlet sharedName = 1;\n// keep\ncallOne(sharedName);\n",
    );
    r.write(
        "src/b.rs",
        b"// b\nlet sharedName = 2 + 2;\n// keep\ncallTwo(sharedName, 3);\n",
    );
    let head = r.commit_all("head");
    (r, base, head)
}

/// A file-local name draws edges inside its file, and cannot leave it.
///
/// This is the whole of ADR 0030's guard. `sharedName` is declared in both
/// files, and a global symbol would therefore be ambiguous — the single-definer
/// rule would drop it and the change would order with no edges at all. Scoped
/// to its file, each declaration is unambiguous where it lives, and neither can
/// reach the other file's use.
#[test]
fn a_file_local_name_links_only_inside_its_own_file() {
    let (r, base, head) = two_files_one_name();
    let out = run_pipeline(
        &r.repo(),
        &ReviewSource::range(base.clone(), head.clone(), head.clone()),
        &Config::default(),
        &LanguageRegistry::builtin(),
        &scoped(Scope::File),
    )
    .unwrap();
    let doc = out.document.expect("document");

    // Which file each class lives in, via its exemplar hunk.
    let file_of_hunk: std::collections::HashMap<&str, &str> = doc
        .hunks
        .iter()
        .map(|h| (h.id.as_str(), h.file.as_str()))
        .collect();
    let file_of_class: std::collections::HashMap<&str, &str> = doc
        .classes
        .iter()
        .map(|c| (c.id.as_str(), file_of_hunk[c.exemplar.as_str()]))
        .collect();

    let edges: Vec<(&str, &str)> = doc
        .classes
        .iter()
        .flat_map(|c| {
            c.depends_on
                .iter()
                .map(move |e| (c.id.as_str(), e.on.as_str()))
        })
        .collect();
    assert_eq!(
        edges.len(),
        2,
        "one edge per file: {edges:?} over {file_of_class:?}"
    );
    for (from, to) in &edges {
        assert_eq!(
            file_of_class[from], file_of_class[to],
            "a file-local name reached out of its file: {from} -> {to}"
        );
    }

    // The same reader, the same names, answering `Global`: two definers, so
    // the single-definer rule drops the symbol and nothing is ordered at all.
    // That is what a file-local name buys — not more edges from looser
    // matching, but an unambiguous answer where the name actually means one
    // thing.
    let (global_edges, _) = graph(&scoped(Scope::Global), &r, &base, &head);
    assert_eq!(global_edges, 0, "globally, `sharedName` has two definers");
}

// ------------------------------------------------------- the symbol index

/// Run the pipeline and hand back the index (ADR 0032).
fn symbol_index(
    symbols: &SymbolReaders,
    r: &TestRepo,
    base: &str,
    head: &str,
) -> differential_engine::schema::SymbolIndex {
    let out = run_pipeline(
        &r.repo(),
        &ReviewSource::range(base.to_string(), head.to_string(), head.to_string()),
        &Config::default(),
        &LanguageRegistry::builtin(),
        symbols,
    )
    .unwrap();
    out.document
        .expect("document")
        .symbols
        .expect("classify produced an index")
}

/// A NEW call to a helper that was already there resolves.
///
/// This is the commonest shape of the reviewer's question and the one the first
/// cut could not answer: definitions came from added lines only, so a helper the
/// change never touched was invisible and the call site lit nothing.
#[test]
fn a_new_call_to_an_existing_helper_resolves() {
    let r = TestRepo::new();
    // `sum_xy` exists at base and is never edited. Its file is still in the
    // diff, because a later line of it changes.
    let base_lib = b"// lib\nfn sum_xy(a: u8, b: u8) {\n    a + b\n}\nfn other() {}\n";
    r.write("src/lib.rs", base_lib);
    r.write("src/call.rs", b"// call\nfn caller() {\n}\n");
    let base = r.commit_all("base");
    r.write(
        "src/lib.rs",
        b"// lib\nfn sum_xy(a: u8, b: u8) {\n    a + b\n}\nfn other() { changed() }\n",
    );
    r.write(
        "src/call.rs",
        b"// call\nfn caller() {\n    let n = sum_xy(1, 2);\n}\n",
    );
    let head = r.commit_all("head");

    let index = symbol_index(&readers(None), &r, &base, &head);
    let def = index
        .definitions
        .iter()
        .find(|d| d.name == "sum_xy")
        .expect("an untouched declaration in a touched file is still indexed");
    assert_eq!((def.file.as_str(), def.line), ("src/lib.rs", 2));
    assert_eq!(
        def.class, None,
        "the change did not write this line, so it belongs to no class"
    );

    let sites: Vec<(&str, u32)> = index
        .uses
        .iter()
        .filter(|u| u.on == def.id)
        .map(|u| (u.file.as_str(), u.line))
        .collect();
    assert_eq!(sites, vec![("src/call.rs", 3)], "the new call site");
}

/// A use is recorded only on a line the change WROTE.
///
/// The other half of the rule above, and what bounds the index: with any
/// declaration resolvable, recording every mention in every parsed file would
/// grow this with the size of the FILES rather than of the change. The cost is
/// that an older call site lights nothing, which is the right way round — the
/// change is the thing being read.
#[test]
fn an_older_call_site_is_not_a_use() {
    let r = TestRepo::new();
    r.write("src/a.rs", b"// a\n");
    r.write(
        "src/b.rs",
        b"fn caller_one() { widget_maker() }\nfn caller_two() { widget_maker() }\n",
    );
    let base = r.commit_all("base");
    r.write("src/a.rs", b"// a\nfn widget_maker() {}\n");
    r.write(
        "src/b.rs",
        b"fn caller_one() { widget_maker() }\nfn caller_two() { widget_maker() }\nfn caller_three() { widget_maker() }\n",
    );
    let head = r.commit_all("head");

    let index = symbol_index(&readers(None), &r, &base, &head);
    let def = index
        .definitions
        .iter()
        .find(|d| d.name == "widget_maker")
        .expect("one definer");
    let uses: Vec<u32> = index
        .uses
        .iter()
        .filter(|u| u.on == def.id && u.file == "src/b.rs")
        .map(|u| u.line)
        .collect();
    assert_eq!(
        uses,
        vec![3],
        "lines 1 and 2 were already there; only line 3 was written"
    );
}

/// An ambiguous name is absent, for the same reason it draws no edge.
///
/// The index reuses the graph's own single-definer verdict rather than forming
/// a second opinion — two answers to "who defines this" would be a bug waiting
/// for a corpus to find it.
#[test]
fn a_name_two_classes_define_is_in_no_index() {
    let r = TestRepo::new();
    r.write("src/a.rs", b"// a\n");
    r.write("src/b.rs", b"// b\n");
    let base = r.commit_all("base");
    // Two files, each declaring the same global name, in two shape classes.
    // `only_here` is the control: unambiguous, and CALLED — an uncalled
    // declaration is dropped whether it is ambiguous or not, so a control
    // nothing reaches would pass for the wrong reason.
    r.write("src/a.rs", b"// a\nfn shared_name() {}\n");
    r.write(
        "src/b.rs",
        b"// b\nfn shared_name() {}\nfn only_here() {}\nfn go() { only_here(); shared_name() }\n",
    );
    let head = r.commit_all("head");

    let index = symbol_index(&readers(None), &r, &base, &head);
    assert!(
        !index.definitions.iter().any(|d| d.name == "shared_name"),
        "two definers, so nothing can say which one a use meant: {:?}",
        index.definitions
    );
    assert!(
        index.definitions.iter().any(|d| d.name == "only_here"),
        "an unambiguous name in the same change is still indexed"
    );
}

/// A document written before the index existed still loads.
///
/// The field is additive, so `schema_version` stays 3 — and stored artefacts
/// really are re-read (`dfr agent --doc`, the grouping cache), so this is a
/// live case rather than a theoretical one.
#[test]
fn a_document_without_the_index_still_deserialises() {
    use differential_engine::schema::PlanDocument;

    let r = TestRepo::new();
    r.write("src/a.rs", b"// a\n");
    let base = r.commit_all("base");
    r.write("src/a.rs", b"// a\nfn widget_maker() {}\n");
    let head = r.commit_all("head");

    let out = run_pipeline(
        &r.repo(),
        &ReviewSource::range(base.clone(), head.clone(), head.clone()),
        &Config::default(),
        &LanguageRegistry::builtin(),
        &readers(None),
    )
    .unwrap();
    let json = out.document.expect("document").to_json().unwrap();

    // Strip the key entirely, the way a document written before this field
    // would have it.
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value.as_object_mut().unwrap().remove("symbols");
    let without = serde_json::to_string(&value).unwrap();

    let doc = PlanDocument::from_json(&without).expect("an older document must still load");
    assert!(
        doc.symbols.is_none(),
        "absent reads as None, never an error"
    );
}

/// A declaration is not a use of itself.
///
/// The crude reader has no veto: its reference regex takes every identifier on
/// a line, the name just declared included, so `fn helper()` reports `helper`
/// as reading `helper`. The stub here behaves the same way on purpose. Pointing
/// a reader at the line they are already standing on is the one answer never
/// worth giving.
#[test]
fn a_declaration_does_not_read_itself() {
    let r = TestRepo::new();
    r.write("src/a.rs", b"// a\n");
    r.write("src/b.rs", b"// b\n");
    let base = r.commit_all("base");
    r.write("src/a.rs", b"// a\nfn widget_maker() {}\n");
    r.write("src/b.rs", b"// b\nfn caller() { widget_maker() }\n");
    let head = r.commit_all("head");

    let index = symbol_index(&readers(None), &r, &base, &head);
    let def = index
        .definitions
        .iter()
        .find(|d| d.name == "widget_maker")
        .expect("one definer");
    let sites: Vec<(&str, u32)> = index
        .uses
        .iter()
        .filter(|u| u.on == def.id)
        .map(|u| (u.file.as_str(), u.line))
        .collect();
    assert_eq!(
        sites,
        vec![("src/b.rs", 2)],
        "the call site only — not the declaring line itself"
    );
}
