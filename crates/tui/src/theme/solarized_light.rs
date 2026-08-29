//! Solarized, light. Pale sand ground.
//!
//! The lowest-contrast palette shipped: Solarized Light puts its foreground
//! 4.1:1 from its ground, under WCAG AA before this crate touches it. That is
//! Solarized's design rather than a defect, so the legibility test holds each
//! theme to its own ceiling and checks that the DERIVATION does not make
//! things worse — see `every_theme_is_legible_on_its_own_ground`.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::SolarizedLight,
        add: rgb(0x5B, 0x69, 0x00),
        del: rgb(0xC2, 0x2B, 0x28),
        accent: rgb(0x1E, 0x6F, 0xA8),
        skim: rgb(0x8A, 0x68, 0x00),
        finding: rgb(0xB0, 0x2B, 0x6C),
        reviewed: rgb(0x5B, 0x69, 0x00),
    }
}
