//! Who this person is to the people they work with: the colour they wear
//! everywhere (`identity.colour`, a presence hue id), read through
//! `ui::tokens::colors::PresenceHue::chosen_in`.

use crate::Key;

/// The section these choices live in.
pub const SECTION: &str = "identity";

/// The presence hue this person chose to wear, by id (`teal`, `violet`, …);
/// `None` until they choose, when their identity picks one.
pub const COLOUR: Key<Option<String>> = Key::new(SECTION, "colour", || None);
