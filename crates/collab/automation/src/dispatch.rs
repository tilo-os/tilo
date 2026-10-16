//! Turning events into runs: which armed program an event fires, with
//! what inputs, under which run id, in which order.
//!
//! Pure over what it is shown. The shell feeds it a person's record
//! changes; the room's host feeds it record changes and connector events
//! for bots. Both get the same answers, so a person's Shortcut and a bot
//! fire on the same change in the same way.
//!
//! The rules (docs/architecture/bots.md §4): no program fires on its own
//! change; a chain of runs stops past [`MAX_CHAIN_DEPTH`]; a run's id is
//! derived from what fired it, so firing twice reuses every invocation id;
//! one installation's firings run one at a time in event order.

mod event;
mod matching;
pub mod queue;

#[cfg(test)]
mod tests;

pub use event::{ConnectorEvent, Event, ItemRef, RecordEvent, event_of};
pub use matching::{
    Armed, Dispatched, Firer, Firing, Skipped, dispatch, edited_paths, field_matches, run_id,
    verbs_of,
};
pub use tilo_actions::context::Cause;

/// A run fired at a greater depth than this fires nothing.
pub const MAX_CHAIN_DEPTH: u8 = 4;
/// Firings one installation may have waiting; the oldest beyond are
/// dead-lettered as "fell behind".
pub const MAX_QUEUED_FIRINGS: usize = 64;
