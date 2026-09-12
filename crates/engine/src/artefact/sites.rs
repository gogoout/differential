//! The symbol index: which token on which line resolves to which declaration.
//!
//! [`super::graph`] answers "class C2 depends on class C7, via `lookUpName`".
//! That is the question the ORDERING stage asks. It is not the question a
//! reviewer asks, which is "what is this thing on the line in front of me" —
//! and the two are the same extraction, one class apart.
//!
//! So this is built from the same parse, in the same pass. Every fact here was
//! already computed and thrown away inside `graph::build`: the file, the line,
//! the token's columns, and how far a declaration runs.
//!
//! **It never feeds the ordering.** A wrong entry here shows a reader the wrong
//! snippet, which they can see is wrong; it cannot move a group. That is why
//! this could be looser than the graph, and is not:
//!
//! - **A definition must be unambiguous.** [`super::graph`] resolves against
//!   its own single-definer map before calling here, so a name two classes
//!   define is absent from the index exactly as it draws no edge. Two answers
//!   to "who defines this" would be a bug waiting for a corpus to find it.
//! - **A use is recorded wherever it appears in a parsed file**, not only on an
//!   added line. A reader can open context with `z` and land on an unchanged
//!   line; a token that resolves there resolves for them too.
//!
//! What it cannot answer is a name the change never declares. Only files with
//! hunks are parsed ([`super::graph::parse_files`]), so a call into an
//! untouched helper resolves to nothing. That is the honest limit of a tool
//! that reads a diff rather than a repository.

use super::symbols::Site;
use crate::model::DiffView;
use crate::schema;

/// One unambiguous declaration, before it is given an id.
pub(super) struct Definition {
    pub name: Vec<u8>,
    /// Index into `DiffView::files`.
    pub file: usize,
    /// New-side line, counting from 1.
    pub line: u32,
    pub site: Site,
    /// Index into `Partition::classes`.
    pub class: usize,
}

/// One token that resolves to a [`Definition`].
pub(super) struct Use {
    /// Index into the `definitions` slice passed alongside.
    pub def: usize,
    pub file: usize,
    pub line: u32,
    pub site: Site,
}

/// Shape resolved definitions and uses into the document's index.
///
/// Ids are positional and document-local, like `h<N>` and `C<N>`, and they are
/// assigned here in location order. The caller walks hash maps, whose order is
/// not stable across runs, and these rows reach a document that is hashed.
pub(super) fn build(
    view: &DiffView,
    mut definitions: Vec<Definition>,
    mut uses: Vec<Use>,
) -> schema::SymbolIndex {
    let mut order: Vec<usize> = (0..definitions.len()).collect();
    order.sort_by_key(|&i| {
        let d = &definitions[i];
        (d.file, d.line, d.site.start, d.name.clone())
    });
    // `rank[old index] = new index`, so the uses can be renumbered without
    // searching for their definition again.
    let mut rank = vec![0usize; definitions.len()];
    for (new, &old) in order.iter().enumerate() {
        rank[old] = new;
    }

    let out: Vec<schema::SymbolDef> = order
        .iter()
        .enumerate()
        .map(|(new, &old)| {
            let d = &mut definitions[old];
            schema::SymbolDef {
                id: format!("s{new}"),
                name: text(&d.name),
                file: text(&view.files[d.file].path),
                line: d.line,
                // Zero means the reader could not see an extent — a regex has
                // no tree to ask. The declaring line is then all there is.
                through: if d.site.through == 0 {
                    d.line
                } else {
                    d.site.through
                },
                start: d.site.start,
                end: d.site.end,
                class: format!("C{}", d.class),
            }
        })
        .collect();

    for u in &mut uses {
        u.def = rank[u.def];
    }
    uses.sort_by_key(|u| (u.def, u.file, u.line, u.site.start));
    let uses = uses
        .into_iter()
        .map(|u| schema::SymbolUse {
            on: out[u.def].id.clone(),
            file: text(&view.files[u.file].path),
            line: u.line,
            start: u.site.start,
            end: u.site.end,
        })
        .collect();

    schema::SymbolIndex {
        definitions: out,
        uses,
    }
}

/// Paths and names reach the schema as text — the same display boundary
/// `graph::text` is. An identifier is one by construction; a path may not be.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
