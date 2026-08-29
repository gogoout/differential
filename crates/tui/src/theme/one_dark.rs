//! One Dark. The Atom-descended palette, over One Half's syntax theme.
//!
//! Its published red sits at 4.4:1 on this ground and its yellow at chroma
//! 0.097 — both a shade under the bars, so both are nudged. Everything else is
//! One's own.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::OneHalfDark,
        add: rgb(0x98, 0xC3, 0x79),
        del: rgb(0xE8, 0x79, 0x7F),
        accent: rgb(0x61, 0xAF, 0xEF),
        skim: rgb(0xE8, 0xBC, 0x5F),
        finding: rgb(0xC6, 0x78, 0xDD),
    }
}
