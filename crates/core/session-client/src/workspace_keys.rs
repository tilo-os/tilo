//! This device's end-to-end encrypted groups: the governance that decides who reads each sealed
//! record the workspaces it holds keep, and the devices each workspace admits, which its window
//! streams carry between.
//! A sealed record is opened on no one's machine but a member's: the workspace's devices agree,
//! group by group, that exactly the devices its access policy admits take part
//! ([`tilo_workspace_keys::KeyDirectory`]), and each sealed write travels with the key
//! material of its group. The keys live here, never in the document, which is what
//! lets a person invited today read what the workspace said a year ago while its
//! membership moves on.
//! Nothing here decides who may read what: the workspace's governance does, and
//! this device only follows it.

use std::collections::{BTreeMap, BTreeSet};

use crate::network::RosterDevice;

use tilo_desktop_model::WorkspaceId as ModelWorkspaceId;
use tilo_identity::policy_change;
use tilo_peer_state::StatePeer;
use tilo_peer_state::key_directory::{Follower, KeyStates, Readers, StateDirectory};
use tilo_peer_state::owner::DeviceWorkspaces;

use crate::engine::Engine;

pub mod vault;
pub use vault::MlsVault;

/// The workspace's access policy as this device's own workspaces hold it: which devices the
/// workspace lets read each group.
pub struct OwnerDirectory {
    owner: DeviceWorkspaces,
    /// A reader also follows what the host it joined a workspace through and it itself sign: the
    /// host vouched for the workspace it was let into.
    reader: bool,
}

impl OwnerDirectory {
    /// The directory of a device that settles its groups, or follows them as `reader`.
    #[must_use]
    pub const fn new(owner: DeviceWorkspaces, reader: bool) -> StateDirectory<Self> {
        StateDirectory {
            states: Self { owner, reader },
        }
    }
}

impl KeyStates for OwnerDirectory {
    fn with_key_state<R>(
        &self,
        workspace: ModelWorkspaceId,
        read: impl FnOnce(&StatePeer, Option<Follower>, Readers<'_>) -> R,
    ) -> Option<R> {
        self.owner
            .with_current(workspace, |view| {
                let follower = self.reader.then(|| {
                    (
                        view.workspace_host().device_public_key,
                        view.device().device_public_key,
                        view.device().user_public_key,
                    )
                });
                let readers = |at| view.key_holders(at);
                Ok(read(view.peer(), follower, &readers))
            })
            .ok()
    }

    fn founds(&self, workspace: ModelWorkspaceId) -> bool {
        self.owner
            .with_current(workspace, |view| Ok(view.founded_here()))
            .unwrap_or(false)
    }

    /// Announced whenever a workspace's governance moves, so a group re-checks on the change itself
    /// rather than on a clock; without the announcements the service keeps its one-second re-check.
    fn policy_epoch(&self) -> Option<u64> {
        (policy_change::timers_reliable() && policy_change::identity_files_watched())
            .then(policy_change::epoch)
    }

    /// The first second after `now` at which the workspace's groups can answer differently with
    /// nothing written ([`next_policy_change`]).
    fn next_policy_change(&self, workspace: ModelWorkspaceId, now: u64) -> Option<u64> {
        next_policy_change(&self.owner, workspace, now)
    }
}

/// The first second after `now` at which who may read `workspace`'s records can change with
/// nothing written: a capability or a device certificate expiring.
pub fn next_policy_change(
    owner: &DeviceWorkspaces,
    workspace: ModelWorkspaceId,
    now: u64,
) -> Option<u64> {
    let second = tilo_workspace_keys::schedule::second_at_or_after;
    let (certificates, capabilities) = owner
        .with_current(workspace, |view| {
            let document = view
                .peer()
                .admitted_workspace()
                .ok_or("no admitted workspace")?;
            let governance = tilo_peer_state::workspace::governance_of(Some(document));
            let certificates = governance
                .identities
                .values()
                .filter_map(|bytes| tilo_peer_state::workspace::identity_row(bytes).ok())
                .filter_map(|row| row.device_cert.expires_at_ns)
                .map(second);
            let capabilities = tilo_peer_state::members::replay_declared(document)
                .grants
                .into_iter()
                .filter_map(|grant| match grant.lifetime {
                    tilo_action_types::Lifetime::Until(ms) => Some(ms.div_ceil(1_000)),
                    _ => None,
                });
            Ok((
                certificates.collect::<Vec<_>>(),
                capabilities.collect::<Vec<_>>(),
            ))
        })
        .ok()?;
    certificates
        .into_iter()
        .chain(capabilities)
        .filter(|at| *at > now)
        .min()
}

/// The devices a workspace admits, by the publisher each of their sessions is known by, and when
/// the certificate the workspace holds of them expires (Unix nanoseconds).
pub type Roster = Vec<RosterDevice>;

/// One device `workspace` admits, with the certificate the workspace certified it under: what its
/// role in the workspace is read from.
#[derive(Clone, Debug)]
pub struct Device {
    pub device: [u8; 32],
    pub cert: tilo_user_identity::DeviceCert,
}

/// The devices `workspace` admits now, with the certificate it holds of each. A device the
/// workspace removed, or whose certificate ran out, is not one, and what it sent or would receive
/// is refused from then on.
pub fn devices_of(engine: &Engine, workspace: ModelWorkspaceId) -> Result<Vec<Device>, String> {
    engine
        .owner()
        .with_current(workspace, |view| admitted_devices(view.peer()))
}

/// The devices `peer`'s workspace admits now, with the certificate it holds of each: what
/// [`devices_of`] reads, wherever the workspace's replica is held.
///
/// # Errors
/// The workspace's governance or its admitted state does not read.
pub fn admitted_devices(peer: &tilo_peer_state::StatePeer) -> Result<Vec<Device>, String> {
    let now = tilo_clock::wall();
    let admitted: BTreeSet<[u8; 32]> = peer
        .governance()
        .member_list_access(now)?
        .member_devices
        .into_iter()
        .collect();
    let document = peer.admitted_workspace().ok_or("no admitted workspace")?;
    let mut devices = BTreeMap::new();
    for bytes in tilo_peer_state::workspace::governance_of(Some(document))
        .identities
        .values()
    {
        let Ok(row) = tilo_peer_state::workspace::identity_row(bytes) else {
            continue;
        };
        let device = row.device_cert.device_public_key;
        if admitted.contains(&device) {
            devices.insert(
                device,
                Device {
                    device,
                    cert: row.device_cert,
                },
            );
        }
    }
    Ok(devices.into_values().collect())
}

/// The roster of `workspace` , as [`devices_of`] reads it: the devices whose window streams carry
/// between.
pub fn roster_of(engine: &Engine, workspace: ModelWorkspaceId) -> Result<Roster, String> {
    devices_of(engine, workspace)?
        .into_iter()
        .map(|device| {
            iroh_base::EndpointId::from_bytes(&device.device)
                .map(|publisher| (publisher, device.cert.expires_at_ns))
                .map_err(|error| error.to_string())
        })
        .collect()
}
