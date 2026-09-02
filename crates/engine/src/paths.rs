//! Path extraction from `diff --git a/X b/Y` headers.
//!
//! The prototype used a greedy regex, which breaks on paths containing spaces.
//! Under `--no-renames` the two sides are always the same path, so the header is
//! split symmetrically: find the split point where the `a/` half equals the `b/`
//! half. C-quoted paths (git quotes bytes outside the printable range even with
//! core.quotepath=false) are unquoted first.

use memchr::memmem;

/// Parse the remainder of a `diff --git ` line into the (single) path.
/// `rest` is everything after `"diff --git "`. Returns `None` if the line cannot
/// be understood — callers treat that as a parse error, never a skip.
pub fn parse_diff_git_path(rest: &[u8]) -> Option<Vec<u8>> {
    // Quoted form: "a/pa th" "b/pa th" (either or both sides may be quoted).
    if rest.first() == Some(&b'"') {
        let (a, after) = unquote_c(rest)?;
        let after = after.strip_prefix(b" ")?;
        let b = if after.first() == Some(&b'"') {
            unquote_c(after)?.0
        } else {
            after.to_vec()
        };
        return strip_ab(&a, &b);
    }
    // Second side quoted only.
    if let Some(pos) = memmem::find(rest, b" \"b/") {
        let a = &rest[..pos];
        let (b, _) = unquote_c(&rest[pos + 1..])?;
        return strip_ab(a, &b);
    }
    // Unquoted: try every ` b/` boundary until both halves agree.
    let mut idx = 0;
    while let Some(off) = memmem::find(&rest[idx..], b" b/") {
        let pos = idx + off;
        if let Some(p) = strip_ab(&rest[..pos], &rest[pos + 1..]) {
            return Some(p);
        }
        idx = pos + 1;
    }
    None
}

fn strip_ab(a: &[u8], b: &[u8]) -> Option<Vec<u8>> {
    let a = a.strip_prefix(b"a/")?;
    let b = b.strip_prefix(b"b/")?;
    (a == b).then(|| a.to_vec())
}

/// Decode one git C-quoted string starting at `s[0] == '"'`.
/// Returns the decoded bytes and the remainder after the closing quote.
pub fn unquote_c(s: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    if s.first() != Some(&b'"') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 1;
    while i < s.len() {
        match s[i] {
            b'"' => return Some((out, &s[i + 1..])),
            b'\\' => {
                i += 1;
                let c = *s.get(i)?;
                match c {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'v' => out.push(0x0b),
                    b'\\' => out.push(b'\\'),
                    b'"' => out.push(b'"'),
                    b'0'..=b'7' => {
                        // Up to three octal digits.
                        let mut val = 0u32;
                        let mut n = 0;
                        while n < 3 {
                            match s.get(i) {
                                Some(&d @ b'0'..=b'7') => {
                                    val = val * 8 + u32::from(d - b'0');
                                    i += 1;
                                    n += 1;
                                }
                                _ => break,
                            }
                        }
                        i -= 1; // loop tail advances
                        out.push(val as u8);
                    }
                    _ => return None,
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    None // unterminated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path() {
        assert_eq!(
            parse_diff_git_path(b"a/src/main.rs b/src/main.rs").unwrap(),
            b"src/main.rs"
        );
    }

    #[test]
    fn path_with_spaces() {
        assert_eq!(
            parse_diff_git_path(b"a/docs/my file.md b/docs/my file.md").unwrap(),
            b"docs/my file.md"
        );
    }

    #[test]
    fn adversarial_space_b_slash() {
        // A path containing " b/" itself: symmetric matching still finds the
        // unique split where both halves agree.
        assert_eq!(parse_diff_git_path(b"a/x b/y b/x b/y").unwrap(), b"x b/y");
    }

    #[test]
    fn quoted_path() {
        assert_eq!(
            parse_diff_git_path(br#""a/t\tab.txt" "b/t\tab.txt""#).unwrap(),
            b"t\tab.txt"
        );
    }

    #[test]
    fn quoted_octal() {
        let (v, rest) = unquote_c(br#""\303\251.txt" tail"#).unwrap();
        assert_eq!(v, "é.txt".as_bytes());
        assert_eq!(rest, b" tail");
    }

    #[test]
    fn mismatched_halves_rejected() {
        assert!(parse_diff_git_path(b"a/one b/two").is_none());
    }
}
