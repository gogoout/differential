//! Symbol readers: the adapter side of `engine::artefact::symbols`.
//!
//! The engine owns the port and the rule for choosing between readers. This
//! crate owns the readers, and depends on the engine — never the reverse
//! (ADR 0020). Each reader ranks itself, so wiring is `register` calls in any
//! order.
//!
//! It lives apart from the engine for two reasons. It carries a tree-sitter
//! grammar per language, which the engine's own test builds should not pay
//! for. And the whole mechanism may move out of the engine later, in which
//! case the graph, the port and these readers travel together.

mod ast;
mod naive;

pub use ast::generic::AstTier2Symbols;
pub use ast::tuned::AstSymbols;
pub use naive::NaiveSymbols;

use differential_engine::artefact::symbols::SymbolReaders;

/// Every shipped reader, registered. This is what the application wires.
///
/// **Not optional.** The readers are the mechanism, not a plugin set someone
/// may forget: a build with no readers produces no dependency edges at all.
pub fn readers() -> SymbolReaders {
    let mut r = SymbolReaders::default();
    r.register(Box::new(AstSymbols::new()));
    r.register(Box::new(AstTier2Symbols::new()));
    r.register(Box::new(NaiveSymbols));
    r
}
