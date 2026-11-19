//! The host's identity store.
//!
//! A person's identity at rest: the public projection every app may see and
//! the root's signing share, either inline or held by a secret provider. This
//! module owns the on-disk format and [`SigningIdentity`], the Secret Service
//! and file secret providers, portable backups and recovery words.
//!
//! The identity is never handed to an app: the [`broker`](crate::broker)
//! reads its public metadata and signs on the app's behalf.

mod unix;

pub mod connected;
#[cfg(test)]
pub(crate) mod test_wire;

pub use unix::*;
