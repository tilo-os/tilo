//! What the person has learned of the desktop's own controls, so each hint teaches one thing once,
//! whichever of their devices they are on; and whether hints show at all.

use crate::Key;

pub use settings_model::control_hints::{Learned, SECTION};

pub const LEARNED: Key<Learned> = Key::new(SECTION, "learned", Learned::default);
