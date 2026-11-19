//! The types Console's rows are items of: one client a person works with
//! agents through, and one of its sessions.

use crate::ConsoleEffect;

/// An agent as the type its rows are items of: what a published agent says
/// it is, so search names it by its type rather than by a tag the card
/// writes.
#[derive(tilo_resource::Row)]
#[row(kind = "console.agent", label = "Agent", effects = ConsoleEffect)]
pub struct Agent;

/// One session an agent ran, as a row of its own.
#[derive(tilo_resource::Row)]
#[row(kind = "console.session", label = "Session", effects = ConsoleEffect)]
pub struct Session;
