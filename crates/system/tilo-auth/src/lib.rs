//! One secret per person (docs/security/sign-in.md §4, §5.2).

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod client;
pub mod protocol;

#[cfg(feature = "service")]
pub mod accounts;
#[cfg(feature = "service")]
pub mod boot;
#[cfg(feature = "service")]
pub mod comforts;
#[cfg(feature = "service")]
pub mod daemon;
#[cfg(feature = "service")]
pub mod guard;
#[cfg(feature = "service")]
pub mod handoff;
#[cfg(feature = "service")]
pub mod keyring;
#[cfg(feature = "service")]
pub mod lockout_store;
#[cfg(feature = "service")]
pub mod luks;
#[cfg(feature = "service")]
pub mod record;
#[cfg(feature = "service")]
pub mod recovery;
#[cfg(feature = "service")]
pub mod rollback_set;
#[cfg(feature = "service")]
pub mod secret;
#[cfg(feature = "service")]
pub mod strength;
#[cfg(feature = "service")]
pub mod tpm;
#[cfg(feature = "service")]
pub mod userkey;
#[cfg(feature = "service")]
pub mod wrapper;

pub const SOCKET: &str = "/run/tilo-auth/socket";
