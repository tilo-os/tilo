//! The Unicode emoji a person picks a reaction from, handed to the toolkit's shared
//! emoji picker and reaction strip (`ui::emoji_catalogue::CATALOGUE`).
//!
//! The catalogue is about half a megabyte, so the toolkit does not link it: only a
//! program that shows a picker carries it, and offers it before its first frame.
//! Whether that program's module carries it is the target's to answer, so an app
//! calls [`offer`] and names no target: a native build links the catalogue, and the
//! browser desktop's start module leaves the offering to the module that carries it
//! (Chat's).
//!
//! A build that carries no catalogue installs nothing at all: the picker keeps its
//! catalogue absent rather than empty, and reads an absent one as "on its way".

/// Hand the Unicode catalogue this build carries to the toolkit's shared emoji
/// picker and reaction strip. Calling it again is free, so a program calls it
/// wherever it loads its own messages.
pub fn offer() {
    // A native build carries the catalogue; a module of the browser desktop's
    // start bundle carries none, and the module that links the catalogue
    // offers it.
    #[cfg(not(target_arch = "wasm32"))]
    tilo_emoji_catalogue::offer();
}

#[cfg(test)]
mod tests;
