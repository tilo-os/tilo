//! Turning events into invocations: which armed program an event fires, with
//! what inputs, under which invocation id, in which order.
//!
//! Pure over what it is shown. The shell feeds it a person's record
//! changes and the events its device's connectors report; a workspace's host
//! feeds it record changes. Both get the same answers, so a person's
//! Shortcut and a program fire on the same change in the same way.
//!
//! The rules: no program fires on its own
//! change; a chain of invocations stops past [`MAX_CHAIN_DEPTH`]; an invocation's id is
//! derived from what fired it, so firing twice reuses every invocation id;
//! one installation's firings run one at a time in event order.

mod event;
mod matching;
pub mod queue;

#[cfg(test)]
mod tests;

pub use event::{
    AppEvent, ConnectorEvent, DocumentEvent, Event, ItemRef, ScheduleEvent, SourceEvent, TimeEvent,
    app_event_object, event_of,
};
pub use matching::{
    Armed, Dispatched, Firer, Firing, Skipped, dispatch, edited_paths, field_matches,
    invocation_id, verbs_of,
};
pub use tilo_actions::context::Cause;

/// An invocation fired at a greater depth than this fires nothing.
pub const MAX_CHAIN_DEPTH: u8 = 4;
/// Firings one installation may have waiting; the oldest beyond are
/// dead-lettered as "fell behind".
pub const MAX_QUEUED_FIRINGS: usize = 64;
