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
use tree_sitter::{Language, Node, Tree};

use super::{line_count, line_of, parse, text_of};

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
        walk(&tree, content, &mut out);
        Some(out)
    }

    fn fingerprint(&self) -> String {
        "ast-fields-v1".to_string()
    }
}

/// Visit every node, carrying down everything a decision needs.
///
/// `in_callee` propagates because a method call nests: `foo.bar()` puts
/// `field_expression` in the `function:` field, and `bar` one level below that.
/// A rule that looked only at a token's own parent would miss every method call
/// in Rust, Go, Python, TypeScript and C++.
///
/// **Iterative, and it never calls `Node::parent`.** Both matter for the same
/// input. This reader takes JavaScript, Java, C, C++ and C#, where a minified
/// bundle or a generated literal makes AST depth track nesting. Recursion would
/// abort the process on overflow instead of returning `None` and letting a
/// cruder reader answer — and `parent()` walks down from the root each time it
/// is called, so asking every node for its parent costs depth per node. One
/// stack carries the parent's kind and the prose flags down instead, which
/// makes the whole pass linear in nodes.
fn walk(tree: &Tree, content: &[u8], out: &mut FileSymbols) {
    /// What a node inherits from the level above it.
    #[derive(Clone, Copy)]
    struct Above {
        parent_kind: &'static str,
        in_callee: bool,
        in_comment: bool,
        /// Inside a string, and no interpolation since — `"${resolve(id)}"`
        /// holds a real call, so an interpolation clears this.
        in_string: bool,
    }

    let mut cursor = tree.walk();
    // One entry per depth, pushed on descent and popped on ascent, so it
    // unwinds exactly with the cursor. The root's entry is never popped.
    let mut stack: Vec<Above> = vec![Above {
        parent_kind: "",
        in_callee: false,
        in_comment: false,
        in_string: false,
    }];

    loop {
        let node = cursor.node();
        let kind = node.kind();
        let field = cursor.field_name().unwrap_or("");
        let above = *stack.last().expect("the root entry is never popped");

        let here_is_callee = CALLEE_FIELDS.contains(&field)
            || (MEMBER_FIELDS.contains(&field)
                && (above.parent_kind.contains("call")
                    || above.parent_kind.contains("invocation")));
        let in_callee = above.in_callee || here_is_callee;

        if node.child_count() == 0 {
            if !above.in_comment && !above.in_string {
                record(node, field, above.parent_kind, in_callee, content, out);
            }
        } else if cursor.goto_first_child() {
            let interpolates = kind.contains("interpolation") || kind.contains("substitution");
            stack.push(Above {
                parent_kind: kind,
                in_callee,
                in_comment: above.in_comment || kind.contains("comment"),
                in_string: !interpolates && (above.in_string || kind.contains("string")),
            });
            continue;
        }
        // A sibling shares our depth, so it inherits the same entry; only
        // climbing out of a level pops one.
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
            stack.pop();
        }
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
