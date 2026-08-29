//! Tree-sitter readers, and what they share.
//!
//! Two readers sit on top of this module and neither knows about the other.
//! [`tuned::AstSymbols`] has a hand-written query per language. [`generic::
//! AstTier2Symbols`] has none, and works from field names instead. They claim
//! disjoint sets of languages, so they never rank against each other.
//!
//! What they share is here: parsing, and the rule for deciding that a token is
//! prose rather than code.

pub mod generic;
pub mod tuned;

use tree_sitter::{Node, Parser, Tree};

/// Parse `content`, or `None` if the grammar cannot be installed.
///
/// A tree with errors in it is still returned. Tree-sitter recovers, and a
/// half-parsed file yields the symbols it did understand — which beats falling
/// through to a reader that understands nothing.
fn parse(language: &tree_sitter::Language, content: &[u8]) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(language).ok()?;
    parser.parse(content, None)
}

/// How many lines `content` has, for sizing a `FileSymbols`.
fn line_count(content: &[u8]) -> usize {
    content.iter().filter(|&&b| b == b'\n').count() + 1
}

/// Is this token prose rather than code?
///
/// **Comments and strings are 44.5% of reference tokens** on a measured range,
/// and dropping them needs no query: every grammar gives comments and strings
/// their own node types, and names them with those words.
///
/// Strings need the one exception. `"${resolve(id)}"` holds a real call, so a
/// token is only prose if it reaches a string WITHOUT passing through an
/// interpolation on the way up.
fn is_prose(node: Node) -> bool {
    let mut current = node;
    let mut through_interpolation = false;
    while let Some(parent) = current.parent() {
        let kind = parent.kind();
        if kind.contains("comment") {
            return true;
        }
        if kind.contains("interpolation") || kind.contains("substitution") {
            through_interpolation = true;
        }
        if kind.contains("string") && !through_interpolation {
            return true;
        }
        current = parent;
    }
    false
}

/// The line a node starts on, counting from 1.
fn line_of(node: Node) -> usize {
    node.start_position().row + 1
}

/// A node's text, or `None` if it is not UTF-8.
fn text_of<'a>(node: Node, content: &'a [u8]) -> Option<&'a [u8]> {
    content.get(node.byte_range())
}
