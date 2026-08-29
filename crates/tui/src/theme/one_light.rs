//! One Light. A near-white ground, and the light theme that replaced the
//! GitHub-based one.
//!
//! Almost every published accent had to come down in lightness. One Light's
//! syntax colours are tuned to be read as CODE on white, where a keyword can
//! sit at 3:1 and nobody minds; the same colours naming a deletion in the
//! plan pane are UI text, and 3:1 is not enough for that.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::OneHalfLight,
        add: rgb(0x2F, 0x7D, 0x32),
        del: rgb(0xC9, 0x38, 0x2C),
        accent: rgb(0x01, 0x6A, 0x99),
        skim: rgb(0x8F, 0x62, 0x00),
        finding: rgb(0xA6, 0x26, 0xA4),
    }
}
