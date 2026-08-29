//! Gruvbox, light. The same family over a cream ground.
//!
//! Gruvbox's light variants are its "faded" set — darker and less saturated
//! than the dark theme's, because they have to hold their own against cream
//! rather than against near-black.
//!
//! Faded is the trap here. Gruvbox's published green (`#79740e`) and blue
//! (`#076678`) are so low in chroma that against this palette's dark brown
//! prose they read as shades of the text rather than as colours: a reviewed ✓
//! in the published green was invisible in practice, at chroma 0.066 where
//! every other ink in the set clears 0.10. Both are pushed up in chroma while
//! staying in Gruvbox's own hues — see
//! `a_semantic_ink_is_tellable_from_ordinary_text`.

use two_face::theme::EmbeddedThemeName;

use super::{Seed, rgb};

pub(super) fn seed() -> Seed {
    Seed {
        syntax: EmbeddedThemeName::GruvboxLight,
        add: rgb(0x4C, 0x7A, 0x0B),
        del: rgb(0x9D, 0x00, 0x06),
        accent: rgb(0x0A, 0x5F, 0x9C),
        skim: rgb(0x8F, 0x5D, 0x10),
        finding: rgb(0x8F, 0x3F, 0x71),
        reviewed: rgb(0x4C, 0x7A, 0x0B),
    }
}
