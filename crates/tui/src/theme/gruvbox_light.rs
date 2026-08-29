//! Gruvbox, light. The same family over a cream ground.
//!
//! Gruvbox's light variants are its "faded" set — darker and less saturated
//! than the dark theme's, because they have to hold their own against cream
//! rather than against near-black.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::GruvboxLight,
        add: rgb(0x69, 0x64, 0x0C),
        del: rgb(0x9D, 0x00, 0x06),
        accent: rgb(0x07, 0x5F, 0x70),
        skim: rgb(0x8F, 0x5D, 0x10),
        finding: rgb(0x8F, 0x3F, 0x71),
        reviewed: rgb(0x42, 0x6B, 0x4E),
    }
}
