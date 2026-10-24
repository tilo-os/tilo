//! Handoff: what the shell and one application instance say to each other
//! while that instance's window moves to another workspace.
//!
//! The shell addresses one instance by its primary route and asks it for
//! one step at a time; the instance answers on the same route. Both ride
//! session-local control frames that are never relayed to peers
//! ([`crate::collaboration::CollabEvent::Handoff`],
//! [`crate::collaboration::CollabRequest::ReportHandoff`]).
//!
//! - [`HandoffRequest::Checkpoint`]: make everything the window shows
//!   durable now, so the shell reads the document as the person left it
//!   rather than as its writer last got round to saving it.
//! - [`HandoffRequest::Retire`]: the window has reopened in its new
//!   workspace. Stamp the document it showed retired, as one change of its
//!   own that travels to everyone holding it over its normal route, make
//!   that durable, and say which heads it had before the stamp — everything
//!   up to there is what still has to come along.
//!
//! An instance also reports on its own when a document it shows arrives
//! retired from somewhere else ([`HandoffReport::Retired`]), so its desktop
//! can tell the person who moved it where.

pub use collab_types::handoff::{HandoffReport, HandoffRequest, MAX_MESSAGE_BYTES, Successor};
