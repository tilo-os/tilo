//! The machines of the person on this device: the disk recovery keys their devices wrote into
//! their own store when they approved an install.

use std::path::Path;

pub use tilo_peer_state::MachineRecovery;
use tilo_peer_state::{PersonalStore, machines};
use tilo_user_identity::DeviceCert;

/// The recovery keys the person keeps in their store at `state`, opened by `root` and `device`:
/// what a device that approved an install wrote there, and this device reads once their stores
/// have met.
///
/// # Errors
///
/// The store cannot be read, or `device` is not a certificate the person's root signed that
/// still holds.
pub fn saved_recovery_keys(
    state: &Path,
    root: [u8; 32],
    device: DeviceCert,
) -> Result<Vec<MachineRecovery>, String> {
    let store = PersonalStore::open_at(state, root, device, tilo_clock::wall())?;
    Ok(store
        .person_resource(machines::KIND)
        .map(|bytes| machines::read(&bytes))
        .unwrap_or_default())
}
