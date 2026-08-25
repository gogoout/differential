//! The dependency direction, enforced (ADR 0020).
//!
//! Business logic owns the trait; the adapter implements it. So a domain
//! module must never name an adapter — not `gitio`, not the filesystem, not a
//! subprocess, not the process environment. The adapter depends on the domain,
//! and never the other way round.
//!
//! That rule is easy to state and easy to break by accident: adding
//! `use crate::gitio::Repo` to a domain module compiles, passes every test,
//! and quietly inverts the arrow back. So this is a **ratchet** rather than a
//! snapshot — `NOT_YET_INVERTED` lists the modules the ports migration has not
//! reached yet, and the test fails both when a module joins the list and when
//! an entry on it is no longer true. The list can only shrink, and it cannot
//! rot: finishing a module without deleting its line is also a failure.
//!
//! Scanning source text is crude, and normally rule 4 would send us looking
//! for a crate. There isn't a boring, widely-used one for intra-crate module
//! layering (`cargo-deny` and friends work at the crate graph level, which is
//! a coarser question than this one), and the check is a dozen lines of string
//! matching — so it is hand-rolled deliberately.

use std::collections::BTreeSet;
use std::path::Path;

/// What a domain module may not name, and how to say so in a failure.
const ADAPTERS: &[(&str, &str)] = &[
    ("crate::gitio", "the git adapter"),
    ("crate::store", "the filesystem adapter"),
    ("std::fs", "the filesystem"),
    ("std::process", "subprocesses"),
    ("std::env", "the process environment"),
    ("etcetera", "platform config directories"),
    ("tempfile", "temporary files"),
];

/// The adapters themselves. The rule constrains what depends on what; these
/// are the modules allowed to know about the outside world, and they may
/// depend on the domain freely — that is the direction the rule wants.
const ADAPTER_MODULES: &[&str] = &["gitio.rs", "llm.rs", "store.rs"];

/// Domain modules the ports migration has not reached yet (ADR 0020, stages
/// 4 and 5). Every line here is a known inversion still to do.
///
/// **This list may only shrink.** Deleting a line is the last step of
/// inverting a module; adding one is never correct — a new domain module that
/// needs the outside world takes a port instead.
const NOT_YET_INVERTED: &[(&str, &str)] = &[
    ("config.rs", "etcetera"),
    ("config.rs", "std::fs"),
    ("grouping/cache.rs", "std::fs"),
    ("review_state.rs", "std::fs"),
];

#[test]
fn domain_modules_do_not_depend_on_adapters() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found: BTreeSet<(String, String)> = BTreeSet::new();

    for path in rust_files(&src) {
        let name = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if name == "lib.rs" || ADAPTER_MODULES.contains(&name.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let text = production_source(&source);
        for (adapter, _) in ADAPTERS {
            if text.contains(adapter) {
                found.insert((name.clone(), (*adapter).to_string()));
            }
        }
    }

    let allowed: BTreeSet<(String, String)> = NOT_YET_INVERTED
        .iter()
        .map(|(m, a)| ((*m).to_string(), (*a).to_string()))
        .collect();

    let new: Vec<_> = found.difference(&allowed).collect();
    assert!(
        new.is_empty(),
        "these domain modules reach for an adapter, which inverts the \
         dependency arrow ADR 0020 fixed:\n{}\n\nA domain module that needs \
         the outside world takes a port (engine::ports) and lets the adapter \
         implement it. Do not add these to NOT_YET_INVERTED — that list is \
         for modules the migration has not reached, and it only shrinks.",
        describe(&new)
    );

    let stale: Vec<_> = allowed.difference(&found).collect();
    assert!(
        stale.is_empty(),
        "NOT_YET_INVERTED lists dependencies that no longer exist:\n{}\n\n\
         Delete these lines. A stale allowlist silently re-permits what it \
         names, which is exactly how this kind of guard stops guarding.",
        describe(&stale)
    );
}

/// `crates/stack` is domain, not a renderer-shaped adapter: `build_stack`
/// carries invariants 2, 3 and 4, so it takes ports like any engine consumer
/// (ADR 0020). `crates/tui` and `crates/cli` are excluded deliberately — they
/// are adapters, and naming `gitio::Repo` concretely is correct there.
///
/// Skipped when the sibling crate is absent, so a published-tarball build of
/// the engine alone still passes.
#[test]
fn the_stack_renderer_is_domain_and_obeys_the_same_rule() {
    /// Same contract as `NOT_YET_INVERTED`: shrinks only, and stale entries
    /// are a failure.
    const STACK_NOT_YET_INVERTED: &[&str] = &[];

    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("stack")
        .join("src");
    if !src.is_dir() {
        return;
    }

    let mut found: BTreeSet<&str> = BTreeSet::new();
    for path in rust_files(&src) {
        let source = std::fs::read_to_string(&path).unwrap();
        let text = production_source(&source);
        for (adapter, _) in ADAPTERS {
            // `crates/stack` says `differential_engine::gitio`, not `crate::`.
            let named = adapter.strip_prefix("crate::").unwrap_or(adapter);
            if text.contains(named) {
                found.insert(adapter);
            }
        }
    }

    let allowed: BTreeSet<&str> = STACK_NOT_YET_INVERTED.iter().copied().collect();
    assert_eq!(
        found, allowed,
        "crates/stack's adapter dependencies changed.\nfound:   {found:?}\n\
         allowed: {allowed:?}\n\nIt is domain — build_stack carries invariants \
         2, 3 and 4 — so it takes ports, not a Repo. Never grow this list; \
         delete entries as the migration reaches them."
    );
}

/// The `plan` module is the domain's shared policy and the one place this rule
/// is load-bearing rather than aspirational: it is what both renderers call
/// instead of re-deriving. It has no entry in the allowlist and never should.
#[test]
fn the_shared_domain_policy_is_pure() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for path in rust_files(&src.join("plan")) {
        let source = std::fs::read_to_string(&path).unwrap();
        let text = production_source(&source);
        for (adapter, what) in ADAPTERS {
            assert!(
                !text.contains(adapter),
                "{} names {what} ({adapter}); engine::plan is pure policy over \
                 the schema and must stay callable without a repository",
                path.display()
            );
        }
    }
}

/// The file's production code: everything above its test module, with comments
/// stripped.
///
/// Two exclusions, both deliberate. A domain module's own unit tests may use a
/// temp directory or read a fixture — that is the test being an adapter, not
/// the domain depending on one; this codebase keeps `#[cfg(test)] mod tests` at
/// the bottom, so truncating there separates them. And a doc comment often has
/// to NAME an adapter to explain why a port exists at all (`ports.rs` says why
/// `ConfigSource::read_required` keeps an error coming from the same `std::fs`
/// call) — flagging prose would push authors toward vaguer comments, which is
/// the opposite of what this file is for.
fn production_source(text: &str) -> String {
    let code = match text.find("#[cfg(test)]") {
        Some(i) => &text[..i],
        None => text,
    };
    code.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn rust_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn describe(pairs: &[&(String, String)]) -> String {
    pairs
        .iter()
        .map(|(module, adapter)| {
            let what = ADAPTERS
                .iter()
                .find(|(a, _)| a == adapter)
                .map(|(_, w)| *w)
                .unwrap_or(adapter.as_str());
            format!("  {module} -> {adapter} ({what})")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
