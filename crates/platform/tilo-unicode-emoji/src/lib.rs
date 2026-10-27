//! The Unicode emoji a person picks a reaction from, handed to the toolkit's
//! shared emoji picker and reaction strip (`ui::emoji_catalogue::CATALOGUE`)
//! wherever the program's own code carries them.
//!
//! The catalogue is about half a megabyte of names and keywords, so it is not
//! part of the toolkit: only a program that shows a picker carries it, and it
//! offers it before its first frame. Whether that program's own module carries
//! it is the target's to answer, which is why the answer lives here:
//!
//! * a native build links the catalogue (statically, or from its engine
//!   library in a `thin` build), and offering it is `tilo_emoji_catalogue`'s
//!   own `offer`;
//! * the browser desktop starts one module that holds several apps and does
//!   not carry the catalogue: there the module that does carry it (Chat's)
//!   offers it, and an app the start module holds offers nothing.
//!
//! So an app calls [`offer`] and names no target. A build that carries no
//! catalogue installs nothing at all: the picker keeps its catalogue absent
//! rather than an empty one, and an absent catalogue is a state the picker
//! reads (it offers the workspace's emoji and the recents, and says the rest
//! is on its way) where an empty one would claim there is nothing to offer.

/// Hand the Unicode catalogue this build carries to the toolkit's shared
/// emoji picker and reaction strip. Calling it again is free, so a program
/// calls it wherever it loads its own messages.
pub fn offer() {
    // A native build carries the catalogue; a module of the browser desktop's
    // start bundle carries none, and the module that links the catalogue
    // offers it.
    #[cfg(not(target_arch = "wasm32"))]
    tilo_emoji_catalogue::offer();
}

#[cfg(test)]
mod tests;
