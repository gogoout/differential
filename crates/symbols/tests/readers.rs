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

use differential_engine::artefact::symbols::{FileSymbols, SymbolSource};
use differential_symbols::{AstSymbols, AstTier2Symbols};

fn flatten(rows: &[Vec<Vec<u8>>]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .flatten()
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn read(reader: &dyn SymbolSource, path: &[u8], src: &str) -> (Vec<String>, Vec<String>) {
    assert!(
        reader.priority(path).is_some(),
        "{} does not claim {}",
        reader.fingerprint(),
        String::from_utf8_lossy(path)
    );
    let s: FileSymbols = reader
        .file_symbols(path, src.as_bytes())
        .expect("the reader claimed this file");
    (flatten(&s.defines), flatten(&s.references))
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

#[test]
fn rust_reads_calls_and_types_and_refuses_modules_and_methods() {
    let (defines, references) = read(
        &AstSymbols::new(),
        b"src/lib.rs",
        r#"
mod template;
pub struct Widget;
impl From<u8> for Widget { fn from(v: u8) -> Self { Widget } }
// mentions NoiseA
pub fn render(w: Widget) -> u8 { plain_call(); w.method_call(); let s = "NoiseB"; 0 }
"#,
    );
    has(&defines, &["Widget", "render"]);
    lacks(
        &defines,
        &["template", "from", "NoiseA", "NoiseB", "method_call"],
    );
    has(&references, &["plain_call", "method_call", "Widget"]);
    lacks(&references, &["NoiseA", "NoiseB", "template"]);
}

#[test]
fn python_reads_annotations_as_types() {
    let (defines, references) = read(
        &AstSymbols::new(),
        b"app.py",
        r#"
class Widget: pass
# mentions NoiseA
def render(w: Widget) -> Widget:
    plain_call()
    w.method_call()
    s = "NoiseB"
    return w
"#,
    );
    has(&defines, &["Widget", "render"]);
    has(&references, &["plain_call", "method_call", "Widget"]);
    lacks(&references, &["NoiseA", "NoiseB"]);
}

#[test]
fn go_reads_selectors_and_refuses_methods() {
    let (defines, references) = read(
        &AstSymbols::new(),
        b"main.go",
        r#"
package p
// mentions NoiseA
type Widget struct{}
func (w Widget) MethodOnType() {}
func Render(w Widget) string { plainCall(); w.MethodCall(); return "NoiseB" }
"#,
    );
    has(&defines, &["Widget", "Render"]);
    lacks(&defines, &["MethodOnType", "NoiseA", "NoiseB"]);
    has(&references, &["plainCall", "MethodCall", "Widget"]);
    lacks(&references, &["NoiseA", "NoiseB"]);
}

#[test]
fn typescript_reads_members_and_exports() {
    let (defines, references) = read(
        &AstSymbols::new(),
        b"app.ts",
        r#"
// mentions NoiseA
export interface Widget { n: number }
export function render(w: Widget): string { plainCall(); w.methodCall(); return "NoiseB" }
"#,
    );
    has(&defines, &["Widget", "render"]);
    has(&references, &["plainCall", "methodCall", "Widget"]);
    lacks(&references, &["NoiseA", "NoiseB"]);
}

#[test]
fn kotlin_needed_a_query_and_now_reads_both_call_shapes() {
    // The generic field rule found NO calls here: Kotlin's `call_expression`
    // has no `function:` field, and `navigation_expression` names none of its
    // children. That is why Kotlin earned a query.
    let (defines, references) = read(
        &AstSymbols::new(),
        b"Main.kt",
        r#"
// mentions NoiseA
class Widget
fun render(w: Widget): Int { plainCall(); w.methodCall(); val s = "NoiseB"; return 0 }
"#,
    );
    has(&defines, &["Widget", "render"]);
    has(&references, &["plainCall", "methodCall", "Widget"]);
    lacks(&references, &["NoiseA", "NoiseB"]);
}

// ------------------------------------------------------ the field-rule reader

#[test]
fn java_reads_through_field_names_with_no_query() {
    let (defines, references) = read(
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
    has(&defines, &["Widget"]);
    lacks(&defines, &["render", "methodOnType"]);
    has(&references, &["plainCall", "methodCall", "Widget"]);
    lacks(&references, &["NoiseA", "NoiseB"]);
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
    let references = flatten(&symbols.references);
    has(&references, &["widgetMaker", "wrap"]);
}
