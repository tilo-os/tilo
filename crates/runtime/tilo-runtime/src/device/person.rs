//! The person's root this computer holds, as the daemon needs it to approve a new computer's
//! certificate when `tilo device add` runs here. A person who joined a workspace from this
//! computer holds no root; adding a device is asked of one of their computers that does.

use anyhow::Result;
use tilo_identity::store::SigningIdentity;

/// Whether this computer holds the root of `person`, read from the identity's public record
/// alone, so the process stays dumpable.
pub(crate) fn holds_root_of(person: &[u8; 32]) -> bool {
    let Ok(dir) = tilo_identity::store::identity_dir_from_env() else {
        return false;
    };
    tilo_identity::store::resolve_active_identity(&dir)
        .and_then(|active| tilo_identity::store::load_metadata_from_path(&active).ok())
        .is_some_and(|metadata| metadata.single_device && metadata.group_public_key == *person)
}

/// The root kept here, if this computer holds one. Reading it makes this process undumpable,
/// so it is read only when a ticket is asked for.
///
/// # Errors
/// The identity store cannot be read.
pub(crate) fn root() -> Result<Option<SigningIdentity>> {
    let Ok(dir) = tilo_identity::store::identity_dir_from_env() else {
        return Ok(None);
    };
    let Some(active) = tilo_identity::store::resolve_active_identity(&dir) else {
        return Ok(None);
    };
    if !tilo_identity::store::load_metadata_from_path(&active)
        .is_ok_and(|metadata| metadata.single_device)
    {
        return Ok(None);
    }
    Ok(Some(tilo_identity::store::load_from_path(active)?))
}
