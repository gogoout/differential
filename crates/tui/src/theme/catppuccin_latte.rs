//! Catppuccin Latte. The light flavour.
//!
//! Catppuccin's light accents are the pastel problem in its sharpest form —
//! its green reaches 3.0:1 on this ground and its yellow 2.3:1, both well
//! under AA for text. Green, blue and yellow are all darkened; the red and
//! the mauve were already strong enough to stand.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::CatppuccinLatte,
        add: rgb(0x2F, 0x7A, 0x20),
        del: rgb(0xD2, 0x0F, 0x39),
        accent: rgb(0x1A, 0x56, 0xCC),
        skim: rgb(0x9A, 0x61, 0x00),
        finding: rgb(0x88, 0x39, 0xEF),
    }
}
