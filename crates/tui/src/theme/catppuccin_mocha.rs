//! Catppuccin Mocha. The flagship dark flavour.
//!
//! Catppuccin is a pastel set by design, which is exactly the pull this
//! palette has to resist: its yellow lands at chroma 0.070, well under the
//! floor a mark needs to read as a mark rather than as pale text. Lifted,
//! while staying in Catppuccin's hue.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::CatppuccinMocha,
        add: rgb(0xA6, 0xE3, 0xA1),
        del: rgb(0xF3, 0x8B, 0xA8),
        accent: rgb(0x89, 0xB4, 0xFA),
        skim: rgb(0xF5, 0xD8, 0x7A),
        finding: rgb(0xCB, 0xA6, 0xF7),
        reviewed: rgb(0xA6, 0xE3, 0xA1),
    }
}
