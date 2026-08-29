#!/usr/bin/env python3
"""Write the fake crate the theme screenshots are taken of.

    python3 assets/fixture.py base   # the before state
    python3 assets/fixture.py head   # the after state, in the same directory

Called by `assets/themes.sh`, which commits between the two. The shape of the
change is what matters: one substantial behavioural edit for the focus tier,
the same mechanical edit repeated across several files for the skim tier, and
a generated file for noise — so the plan pane has all three to show, and the
diff pane is full rather than half empty.
"""

import pathlib
import sys

SRC = pathlib.Path("src")

LEXER_BASE = '''use std::fmt;
use std::str::Chars;

/// A token, and where in the source it began.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: Kind,
    pub span: Span,
}

/// A half-open byte range into the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ident,
    Number,
    String,
    Comment,
    Punct(char),
}

impl Kind {
    /// Whether a stage may skip this token without changing meaning.
    pub fn is_trivia(&self) -> bool {
        matches!(self, Kind::Comment)
    }
}

/// A hand-written tokeniser over a string.
///
/// One character of lookahead, no allocation, and no error recovery: an
/// unterminated string simply ends the token stream.
pub struct Lexer<'a> {
    chars: Chars<'a>,
    offset: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Lexer {
            chars: source.chars(),
            offset: 0,
        }
    }

    /// Read the next token, or `None` at end of input.
    pub fn next_token(&mut self) -> Option<Token> {
        let start = self.offset;
        let c = self.bump()?;
        let kind = match c {
            c if c.is_alphabetic() => Kind::Ident,
            c if c.is_ascii_digit() => Kind::Number,
            '"' => Kind::String,
            c => Kind::Punct(c),
        };
        Some(Token {
            kind,
            span: Span {
                start,
                end: self.offset,
            },
        })
    }

    /// Every token, to end of input.
    pub fn collect_all(&mut self) -> Vec<Token> {
        let mut out = Vec::new();
        while let Some(token) = self.next_token() {
            out.push(token);
        }
        out
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        self.offset += c.len_utf8();
        Some(c)
    }
}
'''

LEXER_HEAD = '''use std::fmt;
use std::str::Chars;

/// A token, and where in the source it began.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: Kind,
    pub span: Span,
}

/// A half-open byte range into the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ident,
    Number,
    String,
    Comment,
    Punct(char),
}

impl Kind {
    /// Whether a stage may skip this token without changing meaning.
    pub fn is_trivia(&self) -> bool {
        matches!(self, Kind::Comment)
    }
}

/// A hand-written tokeniser over a string.
///
/// One character of lookahead, no allocation, and no error recovery: an
/// unterminated string simply ends the token stream.
pub struct Lexer<'a> {
    chars: Chars<'a>,
    offset: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Lexer {
            chars: source.chars(),
            offset: 0,
        }
    }

    /// Read the next token, or `None` at end of input.
    ///
    /// Whitespace is skipped here rather than by the caller. Every caller was
    /// doing it, one of them was doing it wrong, and the wrong one only showed
    /// up on input that began with a tab.
    pub fn next_token(&mut self) -> Option<Token> {
        self.skip_whitespace();
        let start = self.offset;
        let c = self.bump()?;
        let kind = match c {
            '/' if self.peek() == Some('/') => self.line_comment(),
            c if c.is_alphabetic() || c == '_' => self.ident(),
            c if c.is_ascii_digit() => self.number(),
            '"' => self.string()?,
            c => Kind::Punct(c),
        };
        Some(Token {
            kind,
            span: Span {
                start,
                end: self.offset,
            },
        })
    }

    /// Every token, to end of input.
    pub fn collect_all(&mut self) -> Vec<Token> {
        let mut out = Vec::new();
        while let Some(token) = self.next_token() {
            out.push(token);
        }
        out
    }

    /// Every token that is not trivia — what a parser actually wants.
    pub fn collect_significant(&mut self) -> Vec<Token> {
        self.collect_all()
            .into_iter()
            .filter(|t| !t.kind.is_trivia())
            .collect()
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    /// A token runs to the end of the run of characters that can be in it.
    /// It used to be one character long, which made every identifier in the
    /// source into one token per letter.
    fn ident(&mut self) -> Kind {
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.bump();
        }
        Kind::Ident
    }

    fn number(&mut self) -> Kind {
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
            self.bump();
        }
        Kind::Number
    }

    fn string(&mut self) -> Option<Kind> {
        loop {
            match self.bump()? {
                '\\\\' => {
                    self.bump()?;
                }
                '"' => return Some(Kind::String),
                _ => {}
            }
        }
    }

    fn line_comment(&mut self) -> Kind {
        while self.peek().is_some_and(|c| c != '\\n') {
            self.bump();
        }
        Kind::Comment
    }

    fn peek(&self) -> Option<char> {
        self.chars.clone().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        self.offset += c.len_utf8();
        Some(c)
    }
}
'''

STAGE = '''use crate::lexer::{{Kind, Token}};
use crate::{err_import}

/// The {lower} stage.
///
/// Walks the token stream it was handed and reports whatever it could not
/// account for. Every stage in this crate has the same shape, which is the
/// point: they differ in `visit`, and in nothing else.
pub struct {upper} {{
    tokens: Vec<Token>,
    errors: Vec<{err}>,
    depth: usize,
}}

impl {upper} {{
    pub fn new(tokens: Vec<Token>) -> Self {{
        {upper} {{
            tokens,
            errors: Vec::new(),
            depth: 0,
        }}
    }}

    /// Run over every token, and report what could not be handled.
    pub fn run(&mut self) -> Result<usize, {ret}> {{
        let mut handled = 0;
        for token in &self.tokens.clone() {{
            match token.kind {{
                Kind::Punct('{{') => self.depth += 1,
                Kind::Punct('}}') => self.depth = self.depth.saturating_sub(1),
                Kind::Ident => handled += 1,
                _ => {{}}
            }}
            self.visit(token);
        }}
        if self.errors.is_empty() {{
            Ok(handled)
        }} else {{
            Err({err_expr})
        }}
    }}

    fn visit(&mut self, token: &Token) {{
        if token.span.is_empty() {{
            self.report("empty span", token);
        }}
    }}

    fn report(&mut self, message: &str, token: &Token) {{
        {report}
    }}
}}
'''

BASE_STAGE = dict(
    err="String",
    err_import="lexer::Span;",
    ret="String",
    err_expr='self.errors.join(", ")',
    report='self.errors.push(format!("{}: {}", message, token.span));',
)

HEAD_STAGE = dict(
    err="Diagnostic",
    err_import="Diagnostic;",
    ret="Vec<Diagnostic>",
    err_expr="self.errors.clone()",
    report="""self.errors.push(Diagnostic {
            message: message.to_string(),
            span: token.span,
        });""",
)

STAGES = [("parser", "Parser"), ("resolver", "Resolver"), ("emitter", "Emitter")]

LIB_BASE = '''pub mod emitter;
pub mod generated;
pub mod lexer;
pub mod parser;
pub mod resolver;
'''

LIB_HEAD = '''pub mod emitter;
pub mod generated;
pub mod lexer;
pub mod parser;
pub mod resolver;

use lexer::Span;

/// One thing a stage could not account for, and where it was.
///
/// Was a `String` per stage, formatted at the point of failure, which meant
/// nothing downstream could do anything with one but print it.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.message, self.span)
    }
}
'''


def generated(n: int) -> str:
    """A table nobody reads, marked so the tool folds it as noise."""
    rows = "\n".join(
        f'    ("{c:04x}", "U+{c:04X}", {c}),' for c in range(0x2500, 0x2500 + n)
    )
    return (
        "// @generated by codegen.py — DO NOT EDIT\n"
        "\n"
        "/// Box-drawing characters, by codepoint.\n"
        "pub const GLYPHS: &[(&str, &str, u32)] = &[\n" + rows + "\n];\n"
    )


def write(state: str) -> None:
    SRC.mkdir(exist_ok=True)
    fields = BASE_STAGE if state == "base" else HEAD_STAGE
    (SRC / "lexer.rs").write_text(LEXER_BASE if state == "base" else LEXER_HEAD)
    (SRC / "lib.rs").write_text(LIB_BASE if state == "base" else LIB_HEAD)
    for lower, upper in STAGES:
        (SRC / f"{lower}.rs").write_text(STAGE.format(lower=lower, upper=upper, **fields))
    # The generated table grows by a few rows, so it is a real change that the
    # plan still folds away.
    (SRC / "generated.rs").write_text(generated(40 if state == "base" else 44))


if __name__ == "__main__":
    write(sys.argv[1] if len(sys.argv) > 1 else "base")
