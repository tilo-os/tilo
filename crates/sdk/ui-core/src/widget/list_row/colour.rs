//! The colour a list row's leading avatar is drawn in, chosen rather than
//! derived from the identity it shows.

use vello::peniko::Color;

use super::{AvatarTint, Leading, ListRow};

impl ListRow {
    /// The colour the leading avatar is drawn in: its own presence colour,
    /// for an agent whose keeper chose one, rather than the colour its
    /// identity derives.
    #[must_use]
    pub fn avatar_colour(mut self, colour: Color) -> Self {
        if let Some(Leading::Avatar { tint, .. }) = &mut self.leading {
            *tint = Some(AvatarTint::Colour(colour));
        }
        self
    }
}
