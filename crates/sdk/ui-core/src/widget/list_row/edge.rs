//! What a row shows and offers at its edges: the unseen change its trailing side carries,
//! the action a hover offers, the action a swipe reveals.

use crate::icons::Icon;
use crate::theme::Theme;
use crate::tokens::colors::PresenceHue;
use vello::peniko::Color;

/// A change someone else made to what the row shows and the person has not seen yet (task-
/// legibility §3): their face on the row's trailing edge, their colour faintly under it, at
/// the row's own height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowChange<'a> {
    pub identity: &'a str,
    pub name: &'a str,
    pub spoken: &'a str,
    pub said: &'a str,
}

impl RowChange<'_> {
    pub(super) fn colour(self, theme: &Theme) -> Color {
        theme
            .tokens
            .colors
            .presence
            .hue(PresenceHue::of_identity(self.identity))
    }
}

/// One of a row's hover actions: an icon button that names what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowAction<'a> {
    pub icon: Icon,
    pub label: &'a str,
}

/// The action a swipe reveals under the row's trailing edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwipeAction<'a> {
    pub icon: Icon,
    pub label: &'a str,
    /// Deleting, archiving away, revoking: the revealed area is the danger
    /// status colour, because the completed swipe is the confirmation.
    pub destructive: bool,
}
