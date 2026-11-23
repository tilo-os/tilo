//! What a row shows and offers at its edges: the unseen change its trailing side carries,
//! the action a hover offers, the action a swipe reveals.

use crate::component::Person;
use crate::icons::Icon;

/// A change someone else made to what the row shows and the person has not seen yet (task-
/// legibility §3): their face on the row's trailing edge, their colour faintly under it, at
/// the row's own height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowChange<'a> {
    pub by: &'a Person,
    pub spoken: &'a str,
    pub said: &'a str,
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
