//! Handoff: what the shell and one application instance say to each other
//! while that instance's window moves to another workspace.
//!
//! The shell addresses one instance by its primary route and asks it for
//! one step at a time; the instance answers on the same route. Both ride
//! session-local control frames that are never relayed to peers
//! ([`crate::CollabEvent::Handoff`],
//! [`crate::CollabRequest::ReportHandoff`]).
//!
//! - [`HandoffRequest::Checkpoint`]: make everything the window shows
//!   durable now, so the shell reads the document as the person left it
//!   rather than as its writer last got round to saving it.
//!
//! The record itself moves by the workspace store's own `move` action, which
//! the shell invokes once what the window shows is durable.

pub use collab_types::handoff::{HandoffReport, HandoffRequest, MAX_MESSAGE_BYTES};
