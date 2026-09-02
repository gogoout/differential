//! Symbol extraction: the port, and the rule for choosing between readers.
//!
//! **This is a domain use case, not a mechanism.** The graph needs to know what
//! each line defines and references. Whether a regex or a parser answered is
//! not a distinction it can see or act on — those are the same capability at
//! different effort and precision.
//!
//! So the trait lives here, beside its only consumer ([`super::graph`]), and
//! the readers live in an adapter crate that depends on this one. The arrow
//! never points the other way (ADR 0020).
//!
//! `dyn` is correct here, and for the reason `CLAUDE.md` allows it: which
//! reader answers is chosen at RUN time, per file. A Rust file picks a reader
//! with a tuned query, a Java file one with generic rules, a shell script the
//! crude one. `lang::LanguageRegistry` already selects this way.

/// A file's symbols, indexed by NEW-SIDE line number.
///
/// Both vectors are parallel and one entry per line, so an entry is addressed
/// by the line number a hunk already carries. Lines with no symbols hold an
/// empty `Vec`, which does not allocate — a 100k-line file costs pointers.
#[derive(Debug, Default, Clone)]
pub struct FileSymbols {
    pub defines: Vec<Vec<Vec<u8>>>,
    pub references: Vec<Vec<Vec<u8>>>,
}

impl FileSymbols {
    /// Symbols defined on `line`, counting from 1. Empty when out of range.
    pub fn defines_at(&self, line: u32) -> &[Vec<u8>] {
        at(&self.defines, line)
    }

    /// Symbols referenced on `line`, counting from 1.
    pub fn references_at(&self, line: u32) -> &[Vec<u8>] {
        at(&self.references, line)
    }
}

fn at(rows: &[Vec<Vec<u8>>], line: u32) -> &[Vec<u8>] {
    line.checked_sub(1)
        .and_then(|i| rows.get(i as usize))
        .map_or(&[], |v| v.as_slice())
}

/// One way of reading a file's symbols.
///
/// Reading is per FILE, never per line or per hunk: a line inside a block
/// comment or a multi-line string cannot be told from code on its own, and
/// those are the tokens most worth dropping.
pub trait SymbolSource: Send + Sync {
    /// How good this reader's answer would be for `path`. Higher wins.
    ///
    /// `None` means it does not read this file at all.
    ///
    /// **A reader ranks itself.** Nothing outside it knows why one beats
    /// another, and no registration order can get the ranking wrong — which is
    /// why this sits on the port rather than in whatever wires the readers up.
    fn priority(&self, path: &[u8]) -> Option<u8>;

    /// The file's symbols, per new-side line.
    ///
    /// `None` means this reader claimed the file and then could not read it —
    /// a parser meeting something it cannot parse. The caller falls to the next
    /// best reader rather than letting the file lose every symbol.
    fn file_symbols(&self, path: &[u8], content: &[u8]) -> Option<FileSymbols>;

    /// Identifies this reader's extraction behaviour.
    ///
    /// Part of the grouping cache key: the class graph is what the model reads
    /// (ADR 0022), so a reader that answers differently must cold the cache.
    /// Behaviour changes therefore need a new fingerprint, exactly as
    /// `Language::id` works for normalisation.
    fn fingerprint(&self) -> String;
}

/// Every reader available, and the rule for choosing between them.
///
/// The rule is the whole of the policy, and it is business logic: **ask the
/// best reader that claims the file; if it fails, ask the next best; if none
/// claims it, the file contributes no symbols.**
///
/// That last clause is the load-bearing one. A file nothing can read — a
/// lockfile, a README, a SQL query — used to get crude guesses, and on the
/// validation corpus 32% of all dependency edges came from exactly those files.
/// A guess costs more than silence.
#[derive(Default)]
pub struct SymbolReaders {
    readers: Vec<Box<dyn SymbolSource>>,
}

impl SymbolReaders {
    /// Add a reader. **Order does not matter** — each reader ranks itself.
    pub fn register(&mut self, reader: Box<dyn SymbolSource>) {
        self.readers.push(reader);
    }

    /// The best claimant's answer, falling to the next best on failure.
    pub fn of_file(&self, path: &[u8], content: &[u8]) -> Option<FileSymbols> {
        let mut ranked: Vec<(u8, &dyn SymbolSource)> = self
            .readers
            .iter()
            .filter_map(|r| r.priority(path).map(|p| (p, r.as_ref())))
            .collect();
        // Stable, so equal priorities keep registration order and the answer
        // stays deterministic across runs.
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        ranked
            .into_iter()
            .find_map(|(_, r)| r.file_symbols(path, content))
    }

    /// Every reader, in a stable order. Feeds the grouping cache key, so it
    /// must change whenever any reader's behaviour does.
    pub fn fingerprint(&self) -> String {
        let mut parts: Vec<String> = self.readers.iter().map(|r| r.fingerprint()).collect();
        parts.sort();
        parts.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        id: &'static str,
        priority: Option<u8>,
        answer: Option<&'static str>,
    }

    impl SymbolSource for Fake {
        fn priority(&self, _path: &[u8]) -> Option<u8> {
            self.priority
        }
        fn file_symbols(&self, _path: &[u8], _content: &[u8]) -> Option<FileSymbols> {
            self.answer.map(|a| FileSymbols {
                defines: vec![vec![a.as_bytes().to_vec()]],
                references: vec![Vec::new()],
            })
        }
        fn fingerprint(&self) -> String {
            self.id.to_string()
        }
    }

    fn readers(fakes: Vec<Fake>) -> SymbolReaders {
        let mut r = SymbolReaders::default();
        for f in fakes {
            r.register(Box::new(f));
        }
        r
    }

    fn first_define(s: &FileSymbols) -> String {
        String::from_utf8_lossy(&s.defines[0][0]).into_owned()
    }

    fn low() -> Fake {
        Fake {
            id: "low",
            priority: Some(1),
            answer: Some("crude"),
        }
    }

    fn high() -> Fake {
        Fake {
            id: "high",
            priority: Some(9),
            answer: Some("precise"),
        }
    }

    #[test]
    fn symbols_are_addressed_by_line_number_counting_from_one() {
        let fs = FileSymbols {
            defines: vec![vec![b"a".to_vec()], Vec::new()],
            references: vec![Vec::new(), vec![b"b".to_vec()]],
        };
        assert_eq!(fs.defines_at(1), [b"a".to_vec()]);
        assert!(fs.defines_at(2).is_empty());
        assert_eq!(fs.references_at(2), [b"b".to_vec()]);
        // Line 0 does not exist, and neither does line 3. Both answer empty
        // rather than panic: a reader that returns fewer lines than the diff
        // expects loses those lines' symbols, it does not crash the run.
        assert!(fs.defines_at(0).is_empty());
        assert!(fs.references_at(3).is_empty());
    }

    #[test]
    fn the_highest_priority_claimant_answers_whatever_the_registration_order() {
        // The whole reason priority sits on the port: wiring cannot get the
        // ranking wrong, because the readers rank themselves.
        let forwards = readers(vec![low(), high()]);
        let backwards = readers(vec![high(), low()]);
        assert_eq!(
            first_define(&forwards.of_file(b"x.rs", b"").unwrap()),
            "precise"
        );
        assert_eq!(
            first_define(&backwards.of_file(b"x.rs", b"").unwrap()),
            "precise"
        );
    }

    #[test]
    fn a_claimant_that_fails_falls_to_the_next_best() {
        // A parser can claim a file and still meet something it cannot parse.
        // The file must not lose every symbol because of it.
        let r = readers(vec![
            Fake {
                id: "high",
                priority: Some(9),
                answer: None,
            },
            Fake {
                id: "low",
                priority: Some(1),
                answer: Some("crude"),
            },
        ]);
        assert_eq!(first_define(&r.of_file(b"x.rs", b"").unwrap()), "crude");
    }

    #[test]
    fn a_file_no_reader_claims_contributes_nothing() {
        // The rule that removes 32% of the corpus's edges: a lockfile or a
        // README gets silence, not guesses.
        let r = readers(vec![Fake {
            id: "ast",
            priority: None,
            answer: Some("never asked"),
        }]);
        assert!(r.of_file(b"Cargo.lock", b"").is_none());
        assert!(SymbolReaders::default().of_file(b"x.rs", b"").is_none());
    }

    #[test]
    fn the_fingerprint_covers_every_reader_and_ignores_their_order() {
        let a = readers(vec![
            Fake {
                id: "ast-v1",
                priority: Some(9),
                answer: None,
            },
            Fake {
                id: "naive-v1",
                priority: Some(1),
                answer: None,
            },
        ]);
        let b = readers(vec![
            Fake {
                id: "naive-v1",
                priority: Some(1),
                answer: None,
            },
            Fake {
                id: "ast-v1",
                priority: Some(9),
                answer: None,
            },
        ]);
        assert_eq!(a.fingerprint(), "ast-v1+naive-v1");
        assert_eq!(a.fingerprint(), b.fingerprint());
    }
}
