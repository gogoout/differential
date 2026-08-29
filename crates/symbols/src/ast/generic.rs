//! The reader for grammars with no hand-written query.
//!
//! It works from **field names**, which are far more consistent across grammars
//! than kind names are, because a grammar author names a field after the role
//! it plays:
//!
//! | Language | plain call | method call |
//! |---|---|---|
//! | Rust | `call_expression` `function:` | `field_expression` `field:` |
//! | Python | `call` `function:` | `attribute` `attribute:` |
//! | Go | `call_expression` `function:` | `selector_expression` `field:` |
//! | TS/JS | `call_expression` `function:` | `member_expression` `property:` |
//! | Java | `method_invocation` `name:` | same |
//! | C/C++ | `call_expression` `function:` | — |
//!
//! Verified against ten grammars before this was written: nine captured the
//! plain call, the method call and the type, and dropped both a comment mention
//! and a string mention. The tenth was Kotlin, which is why Kotlin has a query
//! instead.

use differential_engine::artefact::symbols::{FileSymbols, SymbolSource};
use tree_sitter::{Language, Node, TreeCursor};

use super::{is_prose, line_count, line_of, parse, text_of};

/// Grammars this reader handles. One entry per extension.
struct Grammar {
    extensions: &'static [&'static [u8]],
    language: fn() -> Language,
}

fn grammars() -> Vec<Grammar> {
    vec![
        Grammar {
            extensions: &[b".js", b".jsx", b".mjs", b".cjs"],
            language: || tree_sitter_javascript::LANGUAGE.into(),
        },
        Grammar {
            extensions: &[b".java"],
            language: || tree_sitter_java::LANGUAGE.into(),
        },
        Grammar {
            extensions: &[b".c", b".h"],
            language: || tree_sitter_c::LANGUAGE.into(),
        },
        Grammar {
            extensions: &[b".cc", b".cpp", b".cxx", b".hpp", b".hh"],
            language: || tree_sitter_cpp::LANGUAGE.into(),
        },
        Grammar {
            extensions: &[b".cs"],
            language: || tree_sitter_c_sharp::LANGUAGE.into(),
        },
    ]
}

/// Fields whose occupant is the thing being called.
const CALLEE_FIELDS: &[&str] = &["function", "callee"];

/// Fields that hold a call's name only when the parent says it is a call.
const MEMBER_FIELDS: &[&str] = &["field", "property", "attribute", "name"];

/// Fields whose occupant is the name a declaration introduces.
const NAME_FIELDS: &[&str] = &["name", "declarator"];

/// Kinds naming a type, so a declaration of one introduces a usable symbol.
const TYPE_LIKE: &[&str] = &[
    "class",
    "struct",
    "enum",
    "interface",
    "trait",
    "union",
    "record",
];

pub struct AstTier2Symbols {
    grammars: Vec<Grammar>,
}

impl Default for AstTier2Symbols {
    fn default() -> Self {
        Self::new()
    }
}

impl AstTier2Symbols {
    /// Probes every grammar and keeps only those its rule can actually work on.
    ///
    /// **A grammar that lacks a `function` field and a `call`-ish kind would
    /// answer empty rather than fail**, and a silent zero is the failure nobody
    /// sees. Dropping it here lets the crude reader win the file honestly.
    pub fn new() -> Self {
        let grammars = grammars()
            .into_iter()
            .filter(|g| {
                let language = (g.language)();
                // Field ids count from 1, and `field_count` is how many there
                // are — so the range is 1..=count.
                let fields: Vec<&str> = (1..=language.field_count() as u16)
                    .filter_map(|i| language.field_name_for_id(i))
                    .collect();
                // Either shape will do. Rust, Go, C and TypeScript put the
                // callee in `function:`; Java puts it in `name:` under
                // `method_invocation`. A grammar with neither cannot be read
                // this way at all.
                let names_a_callee = fields
                    .iter()
                    .any(|f| CALLEE_FIELDS.contains(f) || MEMBER_FIELDS.contains(f));
                let has_call = (0..language.node_kind_count())
                    .filter_map(|i| language.node_kind_for_id(i as u16))
                    .any(|k| k.contains("call") || k.contains("invocation"));
                names_a_callee && has_call
            })
            .collect();
        AstTier2Symbols { grammars }
    }

    fn language_for(&self, path: &[u8]) -> Option<Language> {
        self.grammars
            .iter()
            .find(|g| g.extensions.iter().any(|e| path.ends_with(e)))
            .map(|g| (g.language)())
    }
}

impl SymbolSource for AstTier2Symbols {
    /// Above the crude reader, below anything with a query.
    fn priority(&self, path: &[u8]) -> Option<u8> {
        self.language_for(path).map(|_| 5)
    }

    fn file_symbols(&self, path: &[u8], content: &[u8]) -> Option<FileSymbols> {
        let language = self.language_for(path)?;
        let tree = parse(&language, content)?;
        let lines = line_count(content);
        let mut out = FileSymbols {
            defines: vec![Vec::new(); lines],
            references: vec![Vec::new(); lines],
        };
        walk(&mut tree.walk(), content, false, &mut out);
        Some(out)
    }

    fn fingerprint(&self) -> String {
        "ast-fields-v1".to_string()
    }
}

/// Descend, carrying whether we are already inside a callee position.
///
/// `in_callee` propagates because a method call nests: `foo.bar()` puts
/// `field_expression` in the `function:` field, and `bar` one level below that.
/// A rule that looked only at a token's own parent would miss every method call
/// in Rust, Go, Python, TypeScript and C++.
fn walk(cursor: &mut TreeCursor, content: &[u8], in_callee: bool, out: &mut FileSymbols) {
    let node = cursor.node();
    let field = cursor.field_name().unwrap_or("");
    let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");

    let callee_here = CALLEE_FIELDS.contains(&field)
        || (MEMBER_FIELDS.contains(&field)
            && (parent_kind.contains("call") || parent_kind.contains("invocation")));
    let inside_callee = in_callee || callee_here;

    if node.child_count() == 0 {
        record(node, field, parent_kind, inside_callee, content, out);
    } else if cursor.goto_first_child() {
        loop {
            walk(cursor, content, inside_callee, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn record(
    node: Node,
    field: &str,
    parent_kind: &str,
    inside_callee: bool,
    content: &[u8],
    out: &mut FileSymbols,
) {
    let kind = node.kind();
    if !kind.contains("identifier") && !kind.contains("type") {
        return;
    }
    if is_prose(node) {
        return;
    }
    let Some(text) = text_of(node, content) else {
        return;
    };
    let Some(row) = out.defines.get_mut(line_of(node) - 1) else {
        return;
    };

    if is_definition(node, field, parent_kind) {
        row.push(text.to_vec());
        return;
    }
    // A type position is a reference: `fn f(w: Widget)` consumes Widget.
    let is_type = kind.contains("type") || field == "type";
    if inside_callee || is_type {
        out.references[line_of(node) - 1].push(text.to_vec());
    }
}

/// Does this token name something other files can use?
///
/// The rule the corpus wrote. A regex counted `mod template;` as defining
/// `template`, and `impl From<X> for Y { fn from }` as defining `from` — both
/// unique, so both became global symbols that every mention of a common word
/// then linked to. Six such words produced 64% of one range's edges.
fn is_definition(node: Node, field: &str, parent_kind: &str) -> bool {
    if !NAME_FIELDS.contains(&field) {
        return false;
    }
    if TYPE_LIKE.iter().any(|t| parent_kind.contains(t)) {
        return true;
    }
    // A function is a definition only at file scope. Inside a type's body it is
    // a method: reachable through its type, not by name alone.
    let function_like = parent_kind.contains("function") || parent_kind.contains("method");
    function_like && !inside_a_type_body(node)
}

fn inside_a_type_body(node: Node) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let kind = parent.kind();
        if kind.contains("impl") || TYPE_LIKE.iter().any(|t| kind.contains(t)) {
            return true;
        }
        current = parent;
    }
    false
}
