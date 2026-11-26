//! The host's identity store.
//!
//! A person's identity at rest: the public projection every app may see and
//! the root's signing share, either inline or held by a secret provider. This
//! module owns the on-disk format and [`SigningIdentity`], the Secret Service
//! and file secret providers, portable backups and recovery words.
//!
//! The identity is never handed to an app: the [`broker`](crate::broker)
//! reads its public metadata and signs on the app's behalf.

mod kept_above;
mod records;
mod registry;

#[cfg(test)]
#[cfg(unix)]
pub(crate) mod test_wire;

pub use kept_above::take_up_person_kept_above;
pub use records::*;
pub use registry::{
    IdentityProfile, IdentityRegistry, IdentityRegistryStore, identity_for_workspace,
    selected_actor_id_prompt, selected_identity_path,
};

/// Why a device that holds only part of the identity's key signs nothing as
/// the identity, said as a person reads it.
pub const CANNOT_SIGN_ALONE: &str = "this device holds only part of your identity's key, so it \
     signs nothing as your identity: use the device you set up first";
