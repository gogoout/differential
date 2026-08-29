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
    assert!(tuned.priority(b"src/lib.rs") > fields.priority(b"Main.java"));
}
