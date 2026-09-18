//! Durable local development host using the ordinary qOS peer and relay protocols.
pub mod host;
mod identity;
pub mod owner;
mod seed;
pub mod store;
pub use owner::{CommitReceipt, PendingOperation, SessionOwner, Status};

pub mod enrollment;
pub mod enrollment_http;
pub mod recovery;

pub mod owner_commit;

mod action_host;
mod host_signing;
