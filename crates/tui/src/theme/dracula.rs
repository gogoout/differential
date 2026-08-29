//! Dracula. Near-black ground with the loudest accents of the set.
//!
//! Dracula's colours are already vivid enough that only its cyan needed
//! anything — at chroma 0.093 it is so pale it reads as white-ish rather than
//! as the accent, which on the hunk you are reading is the one cell that has
//! to say "here".

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::Dracula,
        add: rgb(0x50, 0xFA, 0x7B),
        del: rgb(0xFF, 0x55, 0x55),
        accent: rgb(0x6F, 0xE3, 0xFB),
        skim: rgb(0xF1, 0xFA, 0x8C),
        finding: rgb(0xBD, 0x93, 0xF9),
        reviewed: rgb(0x50, 0xFA, 0x7B),
    }
}
