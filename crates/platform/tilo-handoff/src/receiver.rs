//! Destination side of the other-computer handoff. The worker in
//! [`super::transport`] owns the endpoint and the ephemeral key; this module is
//! the narrow surface first boot and its tests use.

pub use super::transport::{Received, Receiver, ReceiverIdentity};
