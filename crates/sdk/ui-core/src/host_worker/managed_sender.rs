//! Managed sender data, owner-thread state and its installed runtime.
//! The codec is inert: decoding bytes never establishes a durable receipt.
//! [`SenderRuntime`] is the only public door; it binds every child to a
//! durable parent step and releases it only after the parent's commit.

mod attempt;
mod codec;
mod lifecycle;
mod observation;
mod runtime;
pub(crate) use attempt::AttemptFailure;
#[cfg(feature = "broker-transport")]
pub(crate) use attempt::RequestView;
pub use attempt::{LocalFailure, LocalRequest, ManagedAttempt, ManagedAttemptResult};
pub use codec::{NativeSenderOutboxV1, OutboxFailure};
pub use runtime::{
    AcceptRefusal, AttemptKind, ChildCommit, ChildIntent, ChildTicket, Continuation,
    InstallRefusal, ParentCommitted, ParentFailure, ParentOwner, ParentReceipt, ParentRecord,
    ParentStep, Recovered, Recovery, Reply, SenderCaller, SenderError, SenderRuntime, StepStart,
};

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "macos"))]
pub(crate) use lifecycle::transport_fixture::TransportFixture;
