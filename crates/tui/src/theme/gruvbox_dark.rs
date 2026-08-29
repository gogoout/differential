//! Gruvbox, dark. Warm brown-black ground, retro accents.
//!
//! Gruvbox's own palette throughout: its yellow-green for an addition rather
//! than a true green, which is what makes it read as Gruvbox and not as a
//! generic dark theme wearing Gruvbox's syntax colours.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::GruvboxDark,
        add: rgb(0xB8, 0xBB, 0x26),
        del: rgb(0xFB, 0x69, 0x54),
        accent: rgb(0x8E, 0xC0, 0x7C),
        skim: rgb(0xFA, 0xBD, 0x2F),
        finding: rgb(0xD9, 0x86, 0xB8),
    }
}
