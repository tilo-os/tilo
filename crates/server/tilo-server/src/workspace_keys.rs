//! The server's part in the encrypted groups of the workspaces it governs.
//!
//! A server whose device may own workspaces takes part in every group of the workspaces it
//! governs, with the key service every device runs beside its own workspace service
//! ([`KeyService`]): what its members send for a group reaches it through the service's sink, and
//! what it settles is retained by the workspace as its own governance.
//!
//! A server whose device may not own workspaces runs no key service: its workspaces retain what
//! their members' groups are made of, and this device reads none of it.

use std::{io, path::Path, sync::Arc};

use ed25519_dalek::SigningKey;
use key_group::VaultKey;
use tilo_workspace_keys::NoSealChecks;
use tilo_workspace_service::own_device::KeyService;

/// Where the key service keeps its state under the state root.
const STATE: &str = "groups";

/// Open the server's group state under `state_root`, taking part as `device`.
///
/// # Errors
///
/// The sealed state cannot be read or created, or the thread settling it cannot start.
pub(crate) fn open(state_root: &Path, device: SigningKey) -> io::Result<KeyService> {
    KeyService::open(
        "tilo-server-groups",
        &state_root.join(STATE),
        vault_key(&device),
        Arc::new(device),
        Arc::new(NoSealChecks),
    )
}

/// The key sealing the server's group state: derived from the device key the server signs with.
fn vault_key(device: &SigningKey) -> VaultKey {
    VaultKey::new(blake3::derive_key(
        "tilo-server workspace key vault v1",
        &device.to_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{STATE, SigningKey, open};

    #[test]
    fn a_server_that_may_own_workspaces_takes_part_in_their_groups() {
        let state = tempfile::tempdir().unwrap();
        let groups = open(state.path(), SigningKey::from_bytes(&[7; 32])).unwrap();
        assert!(state.path().join(STATE).is_dir());
        assert!(groups.groups().sink.is_some());
    }
}
