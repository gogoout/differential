//! What a review is *of*: endpoints, and the identity it is filed under.

use crate::EngineError;
use crate::schema;

/// A revision-range spec, parsed. Pure — resolving `MergeBase` needs a
/// repository, but deciding what the user asked for does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeSpec {
    /// `a..b`, or two separate revs: the endpoints as typed.
    Direct { base: String, head: String },
    /// `a...b`: the base is the merge-base, which is what a merge request's
    /// diff actually shows.
    MergeBase { a: String, b: String },
}

impl RangeSpec {
    /// The head endpoint **as typed**.
    ///
    /// This is a review's identity, not an endpoint: a branch name keeps a
    /// review stable while its tip moves, where the resolved sha would file
    /// every new commit as a different review.
    pub fn head_spec(&self) -> &str {
        match self {
            RangeSpec::Direct { head, .. } => head,
            RangeSpec::MergeBase { b, .. } => b,
        }
    }
}

/// Parse `a..b`, `a...b`, or two separate revs.
///
/// One parser, so the endpoints and the review's identity can never disagree
/// about which side is the head. There used to be a second copy of this in the
/// CLI, whose only protection against divergence was that its extra arms were
/// unreachable.
pub fn parse_range(spec: &[&str]) -> Result<RangeSpec, EngineError> {
    match spec {
        [one] => {
            if let Some((a, b)) = one.split_once("...") {
                Ok(RangeSpec::MergeBase {
                    a: a.to_string(),
                    b: b.to_string(),
                })
            } else if let Some((a, b)) = one.split_once("..") {
                Ok(RangeSpec::Direct {
                    base: a.to_string(),
                    head: b.to_string(),
                })
            } else {
                Err(EngineError::Range(format!(
                    "single argument must be <base>..<head> or <a>...<b>, got {one:?}"
                )))
            }
        }
        [a, b] => Ok(RangeSpec::Direct {
            base: (*a).to_string(),
            head: (*b).to_string(),
        }),
        other => Err(EngineError::Range(format!(
            "expected one range or two revs, got {} arguments",
            other.len()
        ))),
    }
}

/// A resolved review source: where the diff comes from, and what the review is
/// filed under.
///
/// `base`/`head` are the diff's endpoints. `head_spec` and `identity_base` are
/// its *identity* — deliberately separate, because reviewing uncommitted work
/// diffs against synthesized trees that churn on every edit while the review
/// itself must survive (ADR 0017).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSource {
    pub base: String,
    pub head: String,
    pub kind: schema::SourceKind,
    /// The head endpoint as typed.
    pub head_spec: String,
    /// The base a review is filed under when it differs from `base` — set for
    /// uncommitted sources, whose `base`/`head` may be synthesized tree oids.
    pub identity_base: Option<String>,
}

impl ReviewSource {
    /// A plain committed range.
    pub fn range(base: String, head: String, head_spec: String) -> Self {
        ReviewSource {
            base,
            head,
            kind: schema::SourceKind::Range,
            head_spec,
            identity_base: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_range_agrees_on_which_side_is_the_head() {
        // The property that made the CLI's second parser safe only by
        // accident: whatever the spelling, `head_spec` is the right-hand side.
        for spec in [
            vec!["a..b"],
            vec!["a...b"],
            vec!["a", "b"],
            vec!["refs/heads/a..b"],
        ] {
            assert_eq!(parse_range(&spec).unwrap().head_spec(), "b", "{spec:?}");
        }
    }

    #[test]
    fn three_dots_means_merge_base() {
        assert_eq!(
            parse_range(&["main...feature"]).unwrap(),
            RangeSpec::MergeBase {
                a: "main".into(),
                b: "feature".into()
            }
        );
        assert_eq!(
            parse_range(&["main..feature"]).unwrap(),
            RangeSpec::Direct {
                base: "main".into(),
                head: "feature".into()
            }
        );
    }

    #[test]
    fn a_bare_rev_is_not_a_range() {
        // The CLI's old helper returned the rev itself here, which no caller
        // ever saw because resolution errored first. One parser, one answer.
        assert!(matches!(parse_range(&["main"]), Err(EngineError::Range(_))));
        assert!(matches!(parse_range(&[]), Err(EngineError::Range(_))));
        assert!(matches!(
            parse_range(&["a", "b", "c"]),
            Err(EngineError::Range(_))
        ));
    }
}
