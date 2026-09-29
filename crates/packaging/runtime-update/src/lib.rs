//! Self-update for the macOS `Tilo.app`.
//!
//! A release is described by a small JSON feed, [`Release`], signed with a
//! detached Ed25519 signature by the key whose public half the running
//! bundle's `Info.plist` names. The update is accepted only when the feed
//! verifies, it is newer than the running bundle, the downloaded archive has
//! the feed's SHA-256, and the unpacked bundle satisfies the running bundle's
//! own designated code requirement (same identifier, same Developer ID team).
//! The bundle is then replaced by [`swap::replace_bundle`] and the runtime
//! agent restarted.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(target_os = "macos")]
pub mod macos;
pub mod release;
pub mod requirement;
pub mod swap;

pub use release::{Error, Release};
