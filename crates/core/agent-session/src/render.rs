//! Whether an operation an agent asks of a record is answered by its host.
//!
//! An app's long work (a cut rendered, a file encoded) is now one of the app's own engine jobs,
//! asked from its package and answered by the device that runs the app: a record's worker makes
//! it like any other action, so no operation is answered outside the worker and nothing is
//! routed past it.

use tilo_actions::api::ActionDescriptor;

/// Whether `descriptor` is one a host's render service answers: never, now that an app's long
/// work is its own engine job.
#[must_use]
pub fn host_rendered(_descriptor: &ActionDescriptor) -> bool {
    false
}

/// `descriptors` as an agent on this host is offered them: every one the worker lists.
#[must_use]
pub fn offered(descriptors: Vec<ActionDescriptor>) -> Vec<ActionDescriptor> {
    descriptors
}

/// Whether this host makes the renders an agent asks of a record: no, the app's worker does.
#[must_use]
pub fn can_render() -> bool {
    false
}
