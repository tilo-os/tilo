//! Who this person is to the people they work with: the colour they wear
//! everywhere (`identity.colour`, a presence hue id), read through
//! `ui::tokens::colors::PresenceHue::chosen_in`.

use crate::Key;

pub const SECTION: &str = "identity";

pub const COLOUR: Key<Option<String>> = Key::new(SECTION, "colour", || None);
