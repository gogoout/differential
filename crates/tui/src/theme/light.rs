//! A light ground, for a light terminal.
//!
//! The gap the whole theme system was built to close: the old palette's ANSI
//! greys and cyans are whatever the terminal says they are, which on a white
//! background is somewhere between muddy and invisible.
//!
//! GitHub's syntax theme, and GitHub's own diff greens and reds with it —
//! the colours most readers already associate with a diff on a white page.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::Github,
        add: rgb(0x1A, 0x7F, 0x37),
        del: rgb(0xCF, 0x22, 0x2E),
        accent: rgb(0x0A, 0x5C, 0xC2),
        skim: rgb(0x8A, 0x5A, 0x00),
        finding: rgb(0x7A, 0x3D, 0xD0),
        reviewed: rgb(0x1A, 0x7F, 0x37),
    }
}
