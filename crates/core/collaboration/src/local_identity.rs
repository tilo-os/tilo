//! Who sits at this device: the process's one identity boundary
//! (`tilo_local_identity`), and the colour the person chose, which their
//! settings keep.

pub use tilo_local_identity::*;

/// The colour this person chose to wear everywhere (`identity.colour`, as their
/// own card keeps it), `None` while they chose none and every surface derives
/// theirs from their identity.
///
/// Their apps and their shell announce it where they say who they are, and every
/// surface paints them in it, so one person is one colour here, in every app
/// they edit in and on everyone else's screens.
#[must_use]
pub fn own_colour() -> Option<collab_types::presence::PresenceHue> {
    settings_store::identity::COLOUR
        .get()
        .as_deref()
        .and_then(collab_types::presence::PresenceHue::from_id)
}
