//! The host's identity store.
//!
//! A person's identity at rest: the public projection every app may see and
//! the root's signing share, either inline or held by a secret provider. This
//! module owns the on-disk format and [`SigningIdentity`], the Secret Service
//! and file secret providers, portable backups and recovery words.
//!
//! The identity is never handed to an app: the [`broker`](crate::broker)
//! reads its public metadata and signs on the app's behalf.

mod registry;
mod unix;

pub mod connected;
#[cfg(test)]
pub(crate) mod test_wire;

pub(crate) use registry::DirStamp;
pub use registry::{
    IdentityProfile, IdentityRegistry, IdentityRegistryStore, identity_for_workspace,
    selected_actor_id_prompt, selected_identity_path,
};
pub use unix::*;

/// Why a device that holds only part of the identity's key signs nothing as
/// the identity, said as a person reads it.
pub const CANNOT_SIGN_ALONE: &str = "this device holds only part of your identity's key, so it \
     signs nothing as your identity: use the device you set up first";

/// Open the person's root from the active identity in `identity_dir`, the one place a root is
/// kept: what signs a certificate for a new device of theirs, or a revocation.
///
/// # Errors
///
/// `identity_dir` holds no active identity, it cannot be opened here, or this device holds only
/// the public half of its key ([`CANNOT_SIGN_ALONE`]).
pub fn load_person_root(identity_dir: &std::path::Path) -> std::io::Result<SigningIdentity> {
    const MISSING_ROOT: &str = "this device does not hold the person's root";
    let file = resolve_active_identity(identity_dir).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "{MISSING_ROOT}: {} holds no active identity",
                identity_dir.display()
            ),
        )
    })?;
    let identity = load_from_path(file.clone()).map_err(|error| {
        std::io::Error::other(format!(
            "{MISSING_ROOT}: cannot open {}: {error} (start this as the account that owns the \
             identity, in a session that can reach its secret store)",
            file.display()
        ))
    })?;
    if !identity.is_single_device() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            CANNOT_SIGN_ALONE,
        ));
    }
    Ok(identity)
}
