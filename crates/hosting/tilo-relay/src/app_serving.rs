//! The seam through which a host may serve apps to remote callers.
//!
//! The relay itself runs no apps and links none: it carries
//! [`Envelope::RemoteAction`](crate::Envelope::RemoteAction) frames between
//! devices of a workspace without executing them. A process that embeds the
//! relay and *does* host apps supplies an [`AppServingPlane`] to
//! [`Relay::start_with_app_serving`](crate::Relay::start_with_app_serving);
//! frames addressed to the relay's own device are then answered by it, and
//! the host advertises [`ServiceCapabilities::app_serving`](crate::ServiceCapabilities::app_serving).
//! Without one, the capability stays off and such frames are refused as
//! undeliverable.

use tilo_actions::remote::{ActionHost, Caller, Pending, RemoteActionMessage};

/// Answers remote-action requests addressed to this host.
///
/// `caller` is the principal the relay authenticated for the connection the
/// request arrived on; implementations decide authority from it alone.
pub trait AppServingPlane: Send + Sync {
    /// Answer one message for `workspace_id` — whenever the answer is ready,
    /// never blocking the connection — or `None` when it needs no reply.
    fn serve(
        &self,
        workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>>;
}

/// An [`AppServingPlane`] backed by one [`ActionHost`] for every workspace.
pub struct ActionHostPlane<H> {
    host: H,
}

impl<H> ActionHostPlane<H> {
    /// Serve every workspace from `host`.
    pub const fn new(host: H) -> Self {
        Self { host }
    }
}

/// `H` is cloned into an answer that waits for an invocation to move.
impl<H: ActionHost + Clone + Send + Sync + 'static> AppServingPlane for ActionHostPlane<H> {
    fn serve(
        &self,
        _workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        tilo_actions::remote::serve(&self.host, caller, message)
    }
}
