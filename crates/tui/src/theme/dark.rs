//! The original palette: a dark slate ground with a cyan accent.
//!
//! The default, and the only palette anybody had before themes existed. Its
//! accents are chosen to land near where the hand-tuned version sat, but they
//! go through the same derivation as every other theme — a default the rules
//! were never tested against is the one palette that can quietly get worse
//! than the rest.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::Base16EightiesDark,
        add: rgb(0x7C, 0xC7, 0x7F),
        del: rgb(0xEF, 0x8A, 0x8A),
        accent: rgb(0x5D, 0xD5, 0xE8),
        skim: rgb(0xF2, 0xC9, 0x60),
        finding: rgb(0xCB, 0x8A, 0xD6),
    }
}
