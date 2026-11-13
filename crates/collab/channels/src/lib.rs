//! This device's side of every encrypted collaboration channel it takes part in.
//!
//! A channel is one MLS group per record, or per workspace while every reader of the workspace may
//! read the record. Who is in the group is the workspace's access policy ([`ChannelDirectory`]),
//! and every participant moves its group to that policy: an invite, a removal, a role change that
//! ends access, a new certified device or a revoked one becomes a commit from whichever participant
//! gets there first. The keys live here, never in the document, which is what lets a person invited
//! today read what the workspace said a year ago.
//!
//! [`ChannelEncryption`] is the service, [`ChannelKeyAccess`] the handle a host's identity service
//! and the session's roles reach it through, and [`GovernancePump`] the pass that settles the
//! groups over a host's workspace transport.

#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        clippy::arc_with_non_send_sync,
        reason = "a page runs on one thread, and the directory and gates a page hands the engine \
                  are `MaybeSendSync`, never `Send` there"
    )
)]

mod encryption;
pub use encryption::*;
mod history;
pub use history::*;
mod pump;
pub use pump::*;
pub mod schedule;
pub use schedule::*;
mod gates;
pub use gates::*;

/// Fixtures a host's channel tests drive: an in-memory workspace transport and a fixed policy.
#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::wildcard_imports,
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    reason = "test fixtures: a poisoned lock is the test failing"
)]
pub mod testing;
