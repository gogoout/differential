//! What each reader actually extracts, per language.
//!
//! Every snippet carries the same six things, so the table below reads the same
//! way for all of them:
//!
//! - a plain call and a method call — both wanted,
//! - a type used in a signature — wanted,
//! - a mention inside a comment and one inside a string — both noise,
//! - a declaration that names no usable symbol, and a method — neither is a
//!   definition, and counting them is what made 64% of one corpus range's
//!   dependency edges false.

use differential_engine::artefact::symbols::{FileSymbols, Scope, Symbol, SymbolSource};
use differential_symbols::{AstSymbols, AstTier2Symbols, NaiveSymbols};
use sha1::{Digest, Sha1};

/// One reader's answer, split by how far each name reaches.
///
/// The two `local_` sets are the ones a name may only be compared against
/// inside its own file (ADR 0030). Keeping them apart here is the point: a
/// test that means "this is a name other files can use" must not pass because
/// the identifier turned up in the file-local set.
struct Read {
    defines: Vec<String>,
    references: Vec<String>,
    local_defines: Vec<String>,
    local_references: Vec<String>,
}

fn flatten(rows: &[Vec<Symbol>], want: Scope) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .flatten()
        .filter(|s| s.scope == want)
        .map(|s| String::from_utf8_lossy(&s.name).into_owned())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn read(reader: &dyn SymbolSource, path: &[u8], src: &str) -> Read {
    assert!(
        reader.priority(path).is_some(),
        "{} does not claim {}",
        reader.fingerprint(),
        String::from_utf8_lossy(path)
    );
    let s: FileSymbols = reader
        .file_symbols(path, src.as_bytes())
        .expect("the reader claimed this file");
    Read {
        defines: flatten(&s.defines, Scope::Global),
        references: flatten(&s.references, Scope::Global),
        local_defines: flatten(&s.defines, Scope::File),
        local_references: flatten(&s.references, Scope::File),
    }
}

fn has(set: &[String], want: &[&str]) {
    for w in want {
        assert!(set.contains(&w.to_string()), "missing {w:?} in {set:?}");
    }
}

fn lacks(set: &[String], unwanted: &[&str]) {
    for u in unwanted {
        assert!(!set.contains(&u.to_string()), "unwanted {u:?} in {set:?}");
    }
}

// ------------------------------------------------------------ tuned readers

#[test]
fn every_tuned_query_compiles_against_its_pinned_grammar() {
    // A query and its grammar are both ours and both pinned, so a mismatch is a
    // bug to fix rather than a state to ship. This is the loud failure that a
    // hand-written tree walk could not give us.
    let failures = AstSymbols::new();
    assert!(
        failures.failures().is_empty(),
        "queries that would not compile: {:?}",
        failures.failures()
    );
}

/// Rust: modules and TRAIT methods are not definitions, inherent ones are.
///
/// `fn from` in `impl From<u8> for Widget` shares its name with every other
/// conversion in the tree — the ambiguity ADR 0023 measured. `fn serve` in
/// `impl Widget` shares its name with nothing, and callers in other files
/// reach it by that name (ADR 0030). `!trait` is the whole of the difference.
#[test]
fn rust_takes_inherent_methods_and_still_refuses_modules_and_trait_methods() {
    let r = read(
        &AstSymbols::new(),
        b"src/lib.rs",
        r#"
mod template;
pub struct Widget;
impl From<u8> for Widget { fn from(v: u8) -> Self { Widget } }
impl Widget { pub fn serve(&self) -> u8 { 0 } }
// mentions NoiseA
pub fn render(w: Widget) -> u8 { plain_call(); w.method_call(); let s = "NoiseB"; 0 }
pub fn wire() { route(other::module::handler); }
"#,
    );
    has(&r.defines, &["Widget", "render", "serve"]);
    lacks(
        &r.defines,
        &["template", "from", "NoiseA", "NoiseB", "method_call"],
    );
    has(&r.references, &["plain_call", "method_call", "Widget"]);
    // Handed to a router rather than invoked. Only the callee position was
    // captured before, so a registration table drew no edge at all.
    has(&r.references, &["handler"]);
    lacks(&r.references, &["NoiseA", "NoiseB", "template"]);
}

#[test]
fn python_reads_annotations_as_types() {
    let r = read(
        &AstSymbols::new(),
        b"app.py",
        r#"
class Widget:
    def method_on_class(self): pass
# mentions NoiseA
def render(w: Widget) -> Widget:
    plain_call()
    w.method_call()
    s = "NoiseB"
    return w
"#,
    );
    has(&r.defines, &["Widget", "render", "method_on_class"]);
    has(&r.references, &["plain_call", "method_call", "Widget"]);
    lacks(&r.references, &["NoiseA", "NoiseB"]);
}

/// Go: a method's receiver names exactly one type, so the method is a
/// definition (ADR 0030) — the analogue of Rust's inherent `impl`, and Go has
/// no trait-impl case to separate out.
#[test]
fn go_reads_selectors_and_takes_methods() {
    let r = read(
        &AstSymbols::new(),
        b"main.go",
        r#"
package p
// mentions NoiseA
type Widget struct{}
func (w Widget) MethodOnType() {}
func Render(w Widget) string { plainCall(); w.MethodCall(); return "NoiseB" }
func Wire() { mux.Handle("/x", handlers.Listing) }
"#,
    );
    has(&r.defines, &["Widget", "Render", "MethodOnType"]);
    lacks(&r.defines, &["NoiseA", "NoiseB"]);
    has(&r.references, &["plainCall", "MethodCall", "Widget"]);
    // Handed over rather than called. Go spells this the same way as a struct
    // field read, so the capture takes both — ADR 0030 states the cost.
    has(&r.references, &["Listing"]);
    lacks(&r.references, &["NoiseA", "NoiseB"]);
}

#[test]
fn typescript_reads_members_and_exports() {
    let r = read(
        &AstSymbols::new(),
        b"app.ts",
        r#"
// mentions NoiseA
export interface Widget { n: number }
export function render(w: Widget): string { plainCall(); w.methodCall(); return "NoiseB" }
"#,
    );
    has(&r.defines, &["Widget", "render"]);
    has(&r.references, &["plainCall", "methodCall", "Widget"]);
    lacks(&r.references, &["NoiseA", "NoiseB"]);
}

#[test]
fn kotlin_needed_a_query_and_now_reads_both_call_shapes() {
    // The generic field rule found NO calls here: Kotlin's `call_expression`
    // has no `function:` field, and `navigation_expression` names none of its
    // children. That is why Kotlin earned a query.
    let r = read(
        &AstSymbols::new(),
        b"Main.kt",
        r#"
// mentions NoiseA
class Widget { fun serve(): Int { return 0 } }
fun render(w: Widget): Int { plainCall(); w.methodCall(); val s = "NoiseB"; return 0 }
"#,
    );
    has(&r.defines, &["Widget", "render", "serve"]);
    has(&r.references, &["plainCall", "methodCall", "Widget"]);
    lacks(&r.references, &["NoiseA", "NoiseB"]);
}

// ------------------------------------------------------ the field-rule reader

#[test]
fn java_reads_through_field_names_with_no_query() {
    let r = read(
        &AstTier2Symbols::new(),
        b"Main.java",
        r#"
// mentions NoiseA
class Widget {
  void methodOnType() {}
  String render(Widget w) { plainCall(); w.methodCall(); return "NoiseB"; }
}
"#,
    );
    has(&r.defines, &["Widget"]);
    lacks(&r.defines, &["render", "methodOnType"]);
    has(&r.references, &["plainCall", "methodCall", "Widget"]);
    lacks(&r.references, &["NoiseA", "NoiseB"]);
}

#[test]
fn the_tuned_reader_outranks_the_field_reader_and_they_never_overlap() {
    let tuned = AstSymbols::new();
    let fields = AstTier2Symbols::new();
    // Disjoint by construction: a language has a query or it does not.
    for path in [b"src/lib.rs".as_slice(), b"Main.kt", b"app.ts", b"main.go"] {
        assert!(tuned.priority(path).is_some());
        assert!(fields.priority(path).is_none(), "both claimed {path:?}");
    }
    for path in [b"Main.java".as_slice(), b"a.c", b"a.cpp", b"a.cs", b"a.js"] {
        assert!(tuned.priority(path).is_none(), "both claimed {path:?}");
        assert!(fields.priority(path).is_some());
    }
    // The ranking, on the ONE path where it could ever be consulted. This used
    // to compare the two readers over two different files — a contest the
    // loops above prove can never happen, so it reduced to comparing two
    // unrelated constants. `SymbolReaders::of_file` only ranks readers that
    // both claim the SAME path, and today no path is claimed twice.
    let both: Vec<&[u8]> = [
        b"src/lib.rs".as_slice(),
        b"Main.kt",
        b"app.ts",
        b"main.go",
        b"Main.java",
        b"a.c",
        b"a.cpp",
        b"a.cs",
        b"a.js",
    ]
    .into_iter()
    .filter(|p| tuned.priority(p).is_some() && fields.priority(p).is_some())
    .collect();
    assert!(
        both.is_empty(),
        "these paths are claimed twice, so the ranking now decides them: {both:?}"
    );
    // And when a language does gain a query, the tuned reader must win it.
    assert!(
        AstSymbols::new().priority(b"x.kt") > AstTier2Symbols::new().priority(b"x.kt").or(Some(0)),
        "a tuned reader must outrank the field reader wherever both could claim"
    );
}

/// Deep nesting must cost neither stack nor quadratic time.
///
/// This reader takes JavaScript, Java, C, C++ and C#, where a minified bundle
/// or a generated literal makes AST depth track nesting. Two separate hazards
/// live there, and this one test catches both:
///
/// - **Stack.** A recursive walk takes a frame per node. 20,000 levels is far
///   past what a 2 MiB test thread survives, so a return to recursion aborts
///   rather than merely slowing down.
/// - **Time.** `Node::parent` walks down from the root every call, so asking
///   each node for its parent costs depth per node. Writing it that way made
///   this test exceed two minutes at this depth; carrying the parent's kind
///   down on the
///   walk's own stack makes it finish in milliseconds.
///
/// **An identifier at every level, not just the deepest one.** Nesting bare
/// brackets would leave one token at maximum depth, and a per-token ancestor
/// walk — the shape `is_prose` has — would still pass. `f(f(f(…)))` puts a
/// token in callee position at each level, so any rule that climbs from a token
/// to the root goes quadratic here too.
///
/// Tree-sitter parses this input in milliseconds, so anything slower is ours.
#[test]
fn deep_nesting_costs_neither_stack_nor_quadratic_time() {
    const DEPTH: usize = 20_000;
    let mut src = String::with_capacity(DEPTH * 6 + 32);
    src.push_str("const deep = ");
    for _ in 0..DEPTH {
        src.push_str("wrap(");
    }
    src.push_str("widgetMaker()");
    for _ in 0..DEPTH {
        src.push(')');
    }
    src.push_str(";\n");

    // The "nor quadratic time" half of the name, measured. Without a clock
    // this test only proved the stack held: a reintroduced per-token ancestor
    // walk took minutes and still passed green, because the Rust harness
    // imposes no timeout of its own.
    //
    // The budget is deliberately loose — thirty seconds on a machine that
    // does this in milliseconds. It is a trip-wire for a quadratic walk, not
    // a benchmark, so an unloaded laptop and a busy CI runner both clear it.
    let started = std::time::Instant::now();
    let symbols = AstTier2Symbols::new()
        .file_symbols(b"bundle.js", src.as_bytes())
        .expect("the reader claimed this file and must answer");
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_secs(30),
        "reading {DEPTH} levels took {took:?}: something walks from a token to \
         the root, which is quadratic in the depth"
    );

    // The innermost call is still found, so the walk reached the bottom rather
    // than stopping part way.
    let references = flatten(&symbols.references, Scope::Global);
    has(&references, &["widgetMaker", "wrap"]);
}

// ------------------------------------------------------------ file-local names

/// The change that made this whole distinction necessary, in miniature.
///
/// A React component is `export const Panel = …`, its dependencies are
/// `<Child/>` and `{label}`, and none of those three shapes drew an edge before
/// (ADR 0030). The label is declared INSIDE the component, so it is file-local:
/// the reviewer still needs to read the declaration before the three lines that
/// render it, and nothing else in the change can say so.
#[test]
fn tsx_reads_components_jsx_and_the_locals_a_render_consumes() {
    let r = read(
        &AstSymbols::new(),
        b"panel.tsx",
        r#"
// mentions NoiseA
import { Child } from './child';
export interface PanelProps { fallback: string }
type Local = { n: number };
export const Panel = ({ fallback }: PanelProps) => {
  const label = lookUpName() ?? fallback;
  const unused = "NoiseB";
  return <Child title={label}>{label}</Child>;
};
"#,
    );
    // An exported file-scope `const` is a definition, exactly as an exported
    // `function Panel()` is.
    has(&r.defines, &["Panel", "PanelProps"]);
    // An UNEXPORTED type is not, however file-scope it looks. `type FormData =
    // …` in a test file was once the only "definition" of that name in a
    // change, and every file mentioning it linked to the test.
    lacks(&r.defines, &["Local"]);
    has(&r.local_defines, &["Local"]);
    // The component it renders is consumed, and the props type is used.
    has(&r.references, &["Child", "PanelProps", "lookUpName"]);
    // The binding inside the body reaches only this file — and the JSX that
    // reads it says so.
    has(&r.local_defines, &["label", "fallback", "unused"]);
    has(&r.local_references, &["label"]);
    // Neither set takes anything from a comment or a string.
    lacks(&r.defines, &["NoiseA", "NoiseB", "label"]);
    lacks(&r.references, &["NoiseA", "NoiseB"]);
    lacks(&r.local_defines, &["NoiseA", "NoiseB"]);
    lacks(&r.local_references, &["NoiseA", "NoiseB"]);
}

/// A `.ts` file is the tuned reader's, and it now captures every identifier.
///
/// The sibling test below covers the field-rule reader. This one exists because
/// `(identifier) @local_ref` turned a handful of captures per file into one per
/// token, and `is_prose` answers each of them by climbing to the root — which
/// is a per-token ancestor walk, the exact shape that went quadratic once
/// before. A minified `.ts` bundle is where it would show.
#[test]
fn the_tuned_reader_survives_deep_nesting_too() {
    const DEPTH: usize = 20_000;
    let mut src = String::with_capacity(DEPTH * 6 + 32);
    src.push_str("const deep = ");
    for _ in 0..DEPTH {
        src.push_str("wrap(");
    }
    src.push_str("widgetMaker()");
    for _ in 0..DEPTH {
        src.push(')');
    }
    src.push_str(";\n");

    let started = std::time::Instant::now();
    let symbols = AstSymbols::new()
        .file_symbols(b"bundle.ts", src.as_bytes())
        .expect("the reader claimed this file and must answer");
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_secs(30),
        "reading {DEPTH} levels took {took:?}: something walks from a token to \
         the root once per capture, which is quadratic in the depth"
    );
    has(
        &flatten(&symbols.references, Scope::Global),
        &["widgetMaker", "wrap"],
    );
}

/// Every definition names its own columns, and covers its own body (ADR 0032).
///
/// Both halves matter to a reader who wants to SEE the dependency rather than
/// be told it exists: the columns are what a highlight lands on, and `through`
/// is how much of the declaration there is to show.
#[test]
fn a_definition_carries_its_columns_and_its_body() {
    struct Case {
        path: &'static [u8],
        src: &'static str,
        name: &'static str,
        /// Byte columns of the name, then the declaration's last line.
        want: (u32, u32, u32),
    }

    let tuned = AstSymbols::new();
    // Four languages, four declaration shapes, one rule: the captured name's
    // parent is the declaration, so its end row is the body's end.
    let cases = [
        Case {
            path: b"a.rs",
            src: "pub fn serve(x: u8) -> u8 {\n    x + 1\n}\n",
            name: "serve",
            want: (7, 12, 3),
        },
        Case {
            path: b"a.py",
            src: "def handle(e):\n    log(e)\n    return 1\n",
            name: "handle",
            want: (4, 10, 3),
        },
        Case {
            path: b"a.go",
            src: "package p\nfunc Serve(x int) int {\n\treturn x\n}\n",
            name: "Serve",
            want: (5, 10, 4),
        },
        Case {
            path: b"a.ts",
            src: "export function lookUpName(): string {\n  return '';\n}\n",
            name: "lookUpName",
            want: (16, 26, 3),
        },
    ];
    for c in cases {
        let where_ = String::from_utf8_lossy(c.path).into_owned();
        let s = tuned
            .file_symbols(c.path, c.src.as_bytes())
            .expect("the tuned reader claims these");
        let found = s
            .defines
            .iter()
            .flatten()
            .find(|y| y.name == c.name.as_bytes())
            .unwrap_or_else(|| panic!("{} was not defined in {where_}", c.name));
        assert_eq!(
            (found.site.start, found.site.end, found.site.through),
            c.want,
            "{} in {where_}",
            c.name
        );
    }
}

/// A column is a RAW byte offset, not a display column.
///
/// The TUI expands tabs before drawing, so a reader that reported expanded
/// columns would mis-highlight every tab-indented file — and the failure is
/// silent, because the highlight still lands on SOMETHING. Go is the case that
/// matters: gofmt indents with tabs.
#[test]
fn a_column_counts_raw_bytes_and_never_expands_a_tab() {
    let tuned = AstSymbols::new();
    let s = tuned
        .file_symbols(b"a.go", b"package p\nfunc f() {\n\tplain()\n}\n")
        .expect("the tuned reader claims .go");
    let call = s
        .references
        .iter()
        .flatten()
        .find(|y| y.name == b"plain")
        .expect("the call is a reference");
    // One tab, then the name. Raw: column 1. Expanded at any tab width it
    // would be 4 or 8, so this fails loudly if the offsets ever change base.
    assert_eq!(
        (call.site.start, call.site.end),
        (1, 6),
        "a tab is one byte, whatever it draws as"
    );
}

/// The crude reader reports columns and refuses to invent an extent.
///
/// A regex has no tree to ask how far a declaration runs. Zero says so; a guess
/// at the next blank line would be a snippet that is confidently wrong.
#[test]
fn the_crude_reader_places_a_name_but_claims_no_body() {
    let s = NaiveSymbols
        .file_symbols(b"a.rb", b"class Widget\n  def serve\n  end\nend\n")
        .expect("the crude reader claims .rb");
    let def = s
        .defines
        .iter()
        .flatten()
        .find(|y| y.name == b"Widget")
        .expect("`class Widget` defines Widget");
    assert_eq!((def.site.start, def.site.end), (6, 12));
    assert_eq!(def.site.through, 0, "a regex cannot see an extent");
}

/// A query's version reaches the grouping cache key, so editing a pattern
/// without bumping the version serves a stale grouping for a graph that moved.
///
/// **Patterns, not prose.** The hash is taken over the `.scm` with its comment
/// and blank lines removed, because those cannot change an answer and a bump
/// costs every cached grouping in every checkout. That is also why the version
/// is a hand-written string rather than the hash itself: hashing the file into
/// the cache key would cold the cache to fix a typo in a comment.
///
/// Update both together: change a pattern, bump the `-vN` in `tuned.rs`, then
/// paste the hash this test prints.
///
/// (A `;` inside a query string literal would be mistaken for a comment here.
/// No query uses one; a predicate like `(#eq? @x ";")` would need this to
/// strip comments with a real tokeniser instead.)
#[test]
fn every_query_version_pins_its_patterns() {
    const PINNED: &[(&str, &str)] = &[
        ("rust-v3", "714cdaa7ba1c48f03fa5f7d5c8930b80cefd3753"),
        ("python-v3", "b60630bca55fa7759a1bdc68512e98daef1c48d9"),
        ("go-v3", "390fb0cf48f3bf526e585f9c8b091baad394aa8d"),
        ("typescript-v3", "9940745968bfbae0ce51fa46ef992ccb1c5252f4"),
        ("tsx-v3", "34b3fe8b79a4da32583d0391bc92a268f1ad4943"),
        ("kotlin-v3", "9fb6256cbccb80fcb82cfd4fb2b307219a34ee40"),
    ];
    let actual: Vec<(String, String)> = AstSymbols::queries()
        .into_iter()
        .map(|(version, text)| {
            let patterns: Vec<&str> = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with(';'))
                .collect();
            let mut hasher = Sha1::new();
            hasher.update(patterns.join("\n").as_bytes());
            (version.to_string(), hex::encode(hasher.finalize()))
        })
        .collect();
    let pinned: Vec<(String, String)> = PINNED
        .iter()
        .map(|(v, h)| (v.to_string(), h.to_string()))
        .collect();
    assert_eq!(
        actual, pinned,
        "a query's patterns moved without its version: bump the `-vN` in \
         tuned.rs and paste the hashes above"
    );
}

/// Every reader's fingerprint, pinned to what it actually ANSWERS.
///
/// `every_query_version_pins_its_patterns` covers a `.scm` edit. Nothing
/// covered a change to the readers' Rust, and the gap was not theoretical: the
/// crude reader began answering with a namespace (ADR 0031) — changing which
/// cross-file symbols match, for the several languages it is the only reader
/// for — and kept `naive-v1`. A reviewer caught it, which is not a mechanism.
///
/// The fingerprint reaches the grouping cache key, so a reader that answers
/// differently and keeps its version serves a grouping built from a graph that
/// has since moved. This makes that impossible: hash each reader's answer over
/// fixed samples, and pin it beside the version.
///
/// **A tree-sitter grammar upgrade fails this too, and should.** A new grammar
/// can change extraction, which changes the graph, which must cold the cache —
/// and nothing else in the tree would have noticed.
///
/// To update: change the reader, bump its version, paste the hashes below.
#[test]
fn every_reader_fingerprint_pins_its_answers() {
    // One sample per reader tier, kept deliberately small — this pins CHANGE,
    // not coverage. What each reader extracts is tested above, by name.
    const SAMPLES: &[(&str, &str)] = &[
        (
            "a.rs",
            "pub struct W;\nimpl W { pub fn serve(&self) -> u8 { call(); 0 } }\n",
        ),
        (
            "a.py",
            "class W:\n    def m(self): pass\ndef r(w: W): plain(); w.meth()\n",
        ),
        (
            "a.go",
            "package p\ntype W struct{}\nfunc (w W) M() { plain(); w.Meth() }\n",
        ),
        (
            "a.ts",
            "export interface P { n: number }\nexport const f = (p: P) => call(p);\n",
        ),
        ("a.tsx", "export const C = () => <Child n={1} />;\n"),
        (
            "A.kt",
            "class W { fun serve(): Int { plain(); return 0 } }\n",
        ),
        ("a.js", "export const f = (p) => call(p);\n"),
        (
            "A.java",
            "class W { String r(W w) { plain(); return w.meth(); } }\n",
        ),
        ("a.c", "int r(struct W *w) { return plain(w); }\n"),
        ("a.rb", "class W\n  def serve\n    plain_call\n  end\nend\n"),
        ("q.sql", "select id from widgets where owner_id = $1\n"),
    ];

    /// One reader's answer for one sample, as bytes that change iff it does.
    fn answer(reader: &dyn SymbolSource, path: &str, src: &str) -> String {
        let Some(s) = reader.file_symbols(path.as_bytes(), src.as_bytes()) else {
            return "declined\n".to_string();
        };
        let mut out = format!("ns={}\n", String::from_utf8_lossy(&s.namespace));
        for (line, (defines, references)) in s.defines.iter().zip(&s.references).enumerate() {
            // The SITE is hashed too. It is not part of any query, and the
            // graph never reads it — so a change that moved only a column or
            // an extent would otherwise pass this test while serving a stale
            // grouping for a reader that answers differently (ADR 0032).
            let show = |kind: &str, syms: &[Symbol]| -> String {
                syms.iter()
                    .map(|y| {
                        let scope = if y.scope == Scope::Global { "g" } else { "f" };
                        format!(
                            "{kind}{scope}:{}@{}-{}+{}",
                            String::from_utf8_lossy(&y.name),
                            y.site.start,
                            y.site.end,
                            y.site.through
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            };
            if !defines.is_empty() || !references.is_empty() {
                out.push_str(&format!(
                    "{line} {} {}\n",
                    show("d", defines),
                    show("r", references)
                ));
            }
        }
        out
    }

    let readers: Vec<Box<dyn SymbolSource>> = vec![
        Box::new(AstSymbols::new()),
        Box::new(AstTier2Symbols::new()),
        Box::new(NaiveSymbols),
    ];
    let actual: Vec<(String, String)> = readers
        .iter()
        .map(|reader| {
            let mut hasher = Sha1::new();
            for (path, src) in SAMPLES {
                // Only what this reader claims: a reader must not be pinned to
                // another's files, or every bump would cascade.
                if reader.priority(path.as_bytes()).is_some() {
                    hasher.update(path.as_bytes());
                    hasher.update(answer(reader.as_ref(), path, src).as_bytes());
                }
            }
            (reader.fingerprint(), hex::encode(hasher.finalize()))
        })
        .collect();

    const PINNED: &[(&str, &str)] = &[
        (
            "ast-tuned-v2[go-v3,kotlin-v3,python-v3,rust-v3,tsx-v3,typescript-v3]",
            "eebeb73419f0e35f3573f3aa781ec21614f8f6c9",
        ),
        ("ast-fields-v3", "7d1eb9f41d9cb7aff66aa240710a0337c62ba2c6"),
        ("naive-v3", "3b6a6cfece0afc2ea5d6172739e6e6d8747305bc"),
    ];
    let pinned: Vec<(String, String)> = PINNED
        .iter()
        .map(|(v, h)| (v.to_string(), h.to_string()))
        .collect();
    assert_eq!(
        actual, pinned,
        "a reader answers differently: bump its version and paste the hashes above"
    );
}
