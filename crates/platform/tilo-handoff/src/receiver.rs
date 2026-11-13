//! Destination side of the other-computer handoff. The worker in
//! [`super::transport`] owns the endpoint and the ephemeral key.

pub use super::transport::{Received, Receiver, ReceiverIdentity};
