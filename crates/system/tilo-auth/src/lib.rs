//! One secret per person (docs/security/sign-in.md §4, §5.2).
//!
//! Each person has a random 256-bit user key, never stored in the clear. Their
//! record holds it several times, each copy wrapped by one factor: the TPM with
//! their secret as the authorisation value, an Argon2id passphrase on machines
//! without a TPM, and their recovery key. Every check of a secret is an unwrap,
//! so no hash of it exists anywhere a running system could read it.
//!
//! People allowed to unlock the machine also have a system-volume keyslot whose
//! passphrase is derived from their user key, and a `tilo-signin` LUKS2 token
//! that mirrors their record so the boot can unwrap before the root is open.

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
pub mod secret;
#[cfg(feature = "service")]
pub mod strength;
#[cfg(feature = "service")]
pub mod tpm;
#[cfg(feature = "service")]
pub mod userkey;
#[cfg(feature = "service")]
pub mod wrapper;

/// Where `tilo-authd` listens.
pub const SOCKET: &str = "/run/tilo-auth/socket";
