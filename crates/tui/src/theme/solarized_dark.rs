//! Solarized, dark. Deep teal ground.
//!
//! Solarized is deliberately low contrast — it puts its own foreground 5.6:1
//! from its own ground, where Monokai manages 15:1. That is the palette this
//! derivation's adaptive muting exists for: a fixed mute fraction leaves its
//! quiet inks below 2:1 and unreadable.
//!
//! The red and the magenta are lifted above Solarized's published values,
//! which sit at 4.2:1 on this ground — under WCAG AA for text that names a
//! deletion.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::SolarizedDark,
        add: rgb(0x9E, 0xB5, 0x00),
        del: rgb(0xFF, 0x6E, 0x6B),
        accent: rgb(0x35, 0xB9, 0xAF),
        skim: rgb(0xCA, 0x9A, 0x00),
        finding: rgb(0xEE, 0x74, 0xAA),
    }
}
