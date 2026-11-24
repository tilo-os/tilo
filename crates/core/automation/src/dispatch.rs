//! Turning events into invocations: which armed program an event fires, with
//! what inputs, under which invocation id, in which order.
//!
//! Pure over what it is shown. The shell feeds it a person's record
//! changes and the events its device's connectors report; a workspace's host
//! feeds it record changes. Both get the same answers, so a person's
//! Shortcut and a program fire on the same change in the same way.
//!
//! The rules: no shortcut fires on its own change, nor on another's that would
//! run the two into each other; an invocation's id is derived from what fired
//! it, so firing twice reuses every invocation id.

mod event;
mod matching;
pub mod retry;

#[cfg(test)]
mod tests;

pub use event::{
    AppEvent, ConnectorEvent, DocumentEvent, Event, InvocationEvent, ItemRef, PresenceEvent,
    ScheduleEvent, SourceEvent, TimeEvent, app_event_object, event_of,
};
pub use matching::{
    Armed, Dispatched, Firer, Firing, Skipped, dispatch, edited_paths, field_matches,
    invocation_id, verbs_of,
};
pub use tilo_actions::context::Cause;
