//! Nearby Send for the certified Files application over the personal device
//! fabric. The shared broker admits the route and bounds the wire; this host
//! owns the opaque handles, each bound to the connection and channel it was
//! issued to, and the Files instance's live filesystem grants.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use tilo_action_broker::services::{NearbyTransferRequestContext, NearbyTransferServices};

use super::super::state::WorkspaceAuthority as _;
use super::super::state::{Capability, FsMode, WorkspaceTreeHandle};
use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::device_fabric_runtime as fabric;
use crate::shell::nearby_transfer::{SecureNearbyDestination, SecureNearbySource};
use crate::shell::personal_continuity::DeviceId;

const NEARBY_TARGET_HANDLE_TTL: Duration = Duration::from_secs(30);
const NEARBY_TRANSFER_HANDLE_TTL: Duration = Duration::from_secs(120);
const MAX_NEARBY_TRANSFER_HANDLES: usize = 256;
const NEARBY_DESTINATION_HANDLE_TTL: Duration = Duration::from_secs(120);
const FILES_APP_ID: &str = "so.tilo.file-explorer";
const EXPIRED_TRANSFER: &str = "Nearby Send transfer expired or belongs to another Files workspace";

/// When a nearby handle issued or refreshed at `now` stops being valid.
///
/// An expiry the clock cannot represent fails closed: the handle expires at
/// `now` instead of never, so an unrepresentable deadline cannot mint an
/// immortal capability handle.
fn nearby_handle_expiry(now: Instant, ttl: Duration) -> Instant {
    now.checked_add(ttl).unwrap_or(now)
}

fn fresh_handle_bytes() -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    UnwrapErr(SysRng).fill_bytes(&mut bytes);
    bytes
}

#[derive(Clone)]
struct NearbyTargetBinding {
    peer_id: u64,
    channel: String,
    target: DeviceId,
    expires_at: Instant,
}

#[derive(Default)]
struct NearbyTargetRegistry {
    bindings: HashMap<runtime_collab::NearbyTargetHandle, NearbyTargetBinding>,
}

impl NearbyTargetRegistry {
    fn begin_snapshot(&mut self, peer_id: u64, channel: &str, targets: &[DeviceId], now: Instant) {
        self.bindings.retain(|_, binding| {
            binding.expires_at > now
                && (binding.peer_id != peer_id
                    || binding.channel != channel
                    || targets.contains(&binding.target))
        });
    }

    fn issue(
        &mut self,
        peer_id: u64,
        channel: &str,
        target: DeviceId,
        now: Instant,
    ) -> runtime_collab::NearbyTargetHandle {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        if let Some((handle, binding)) = self.bindings.iter_mut().find(|(_, binding)| {
            binding.peer_id == peer_id && binding.channel == channel && binding.target == target
        }) {
            binding.expires_at = nearby_handle_expiry(now, NEARBY_TARGET_HANDLE_TTL);
            return *handle;
        }
        loop {
            let handle =
                runtime_collab::NearbyTargetHandle::from_issued_bytes(fresh_handle_bytes());
            if handle.valid() && !self.bindings.contains_key(&handle) {
                self.bindings.insert(
                    handle,
                    NearbyTargetBinding {
                        peer_id,
                        channel: channel.to_owned(),
                        target,
                        expires_at: nearby_handle_expiry(now, NEARBY_TARGET_HANDLE_TTL),
                    },
                );
                return handle;
            }
        }
    }

    fn consume(
        &mut self,
        handle: runtime_collab::NearbyTargetHandle,
        peer_id: u64,
        channel: &str,
        now: Instant,
    ) -> Option<DeviceId> {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        let binding = self.bindings.get(&handle)?;
        if binding.peer_id != peer_id || binding.channel != channel {
            return None;
        }
        self.bindings.remove(&handle).map(|binding| binding.target)
    }

    fn remove_peer(&mut self, peer_id: u64) {
        self.bindings
            .retain(|_, binding| binding.peer_id != peer_id);
    }
}

#[derive(Clone)]
struct NearbyTransferBinding {
    peer_id: u64,
    channel: String,
    transfer_id: [u8; 16],
    direction: runtime_collab::NearbyTransferDirection,
    expires_at: Instant,
}

#[derive(Default)]
struct NearbyTransferRegistry {
    bindings: HashMap<runtime_collab::NearbyTransferHandle, NearbyTransferBinding>,
}

impl NearbyTransferRegistry {
    fn begin_snapshot(
        &mut self,
        peer_id: u64,
        channel: &str,
        visible_ids: &[[u8; 16]],
        now: Instant,
    ) {
        self.bindings.retain(|_, binding| {
            binding.expires_at > now
                && (binding.peer_id != peer_id
                    || binding.channel != channel
                    || visible_ids.contains(&binding.transfer_id))
        });
    }

    fn issue(
        &mut self,
        peer_id: u64,
        channel: &str,
        transfer_id: [u8; 16],
        direction: runtime_collab::NearbyTransferDirection,
        now: Instant,
    ) -> Option<runtime_collab::NearbyTransferHandle> {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        if let Some((handle, binding)) = self
            .bindings
            .iter_mut()
            .find(|(_, binding)| binding.transfer_id == transfer_id)
        {
            if binding.peer_id != peer_id || binding.channel != channel {
                return None;
            }
            binding.expires_at = nearby_handle_expiry(now, NEARBY_TRANSFER_HANDLE_TTL);
            return Some(*handle);
        }
        if self.bindings.len() >= MAX_NEARBY_TRANSFER_HANDLES {
            return None;
        }
        loop {
            let handle =
                runtime_collab::NearbyTransferHandle::from_issued_bytes(fresh_handle_bytes());
            if handle.valid() && !self.bindings.contains_key(&handle) {
                self.bindings.insert(
                    handle,
                    NearbyTransferBinding {
                        peer_id,
                        channel: channel.to_owned(),
                        transfer_id,
                        direction,
                        expires_at: nearby_handle_expiry(now, NEARBY_TRANSFER_HANDLE_TTL),
                    },
                );
                return Some(handle);
            }
        }
    }

    fn resolve(
        &mut self,
        handle: runtime_collab::NearbyTransferHandle,
        peer_id: u64,
        channel: &str,
        direction: runtime_collab::NearbyTransferDirection,
        now: Instant,
    ) -> Option<[u8; 16]> {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        let binding = self.bindings.get_mut(&handle)?;
        if binding.peer_id != peer_id
            || binding.channel != channel
            || binding.direction != direction
        {
            return None;
        }
        binding.expires_at = nearby_handle_expiry(now, NEARBY_TRANSFER_HANDLE_TTL);
        Some(binding.transfer_id)
    }

    fn remove_peer(&mut self, peer_id: u64) {
        self.bindings
            .retain(|_, binding| binding.peer_id != peer_id);
    }
}

#[derive(Clone)]
struct NearbyDestinationBinding {
    peer_id: u64,
    channel: String,
    grant: PathBuf,
    label: String,
    remembered: bool,
    expires_at: Instant,
}

#[derive(Default)]
struct NearbyDestinationRegistry {
    bindings: HashMap<runtime_collab::NearbyDestinationHandle, NearbyDestinationBinding>,
}

impl NearbyDestinationRegistry {
    fn issue(
        &mut self,
        peer_id: u64,
        channel: &str,
        grant: PathBuf,
        label: String,
        now: Instant,
    ) -> runtime_collab::NearbyTransferDestination {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        if let Some((handle, binding)) = self.bindings.iter_mut().find(|(_, binding)| {
            binding.peer_id == peer_id && binding.channel == channel && binding.grant == grant
        }) {
            binding.expires_at = nearby_handle_expiry(now, NEARBY_DESTINATION_HANDLE_TTL);
            return runtime_collab::NearbyTransferDestination {
                handle: *handle,
                label: binding.label.clone(),
                remembered_for_session: binding.remembered,
            };
        }
        loop {
            let handle =
                runtime_collab::NearbyDestinationHandle::from_issued_bytes(fresh_handle_bytes());
            if handle.valid() && !self.bindings.contains_key(&handle) {
                self.bindings.insert(
                    handle,
                    NearbyDestinationBinding {
                        peer_id,
                        channel: channel.to_owned(),
                        grant,
                        label: label.clone(),
                        remembered: false,
                        expires_at: nearby_handle_expiry(now, NEARBY_DESTINATION_HANDLE_TTL),
                    },
                );
                return runtime_collab::NearbyTransferDestination {
                    handle,
                    label,
                    remembered_for_session: false,
                };
            }
        }
    }

    fn resolve(
        &mut self,
        handle: runtime_collab::NearbyDestinationHandle,
        peer_id: u64,
        channel: &str,
        remember: bool,
        now: Instant,
    ) -> Option<SecureNearbyDestination> {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        let binding = self.bindings.get_mut(&handle)?;
        if binding.peer_id != peer_id || binding.channel != channel {
            return None;
        }
        binding.remembered |= remember;
        binding.expires_at = nearby_handle_expiry(now, NEARBY_DESTINATION_HANDLE_TTL);
        SecureNearbyDestination::open_grant(&binding.grant).ok()
    }

    fn remove_peer(&mut self, peer_id: u64) {
        self.bindings
            .retain(|_, binding| binding.peer_id != peer_id);
    }
}

fn nearby_source_from_grants(
    raw_path: &[u8],
    grants: &[Capability],
) -> Result<SecureNearbySource, String> {
    use std::os::unix::ffi::OsStringExt as _;

    let source = PathBuf::from(std::ffi::OsString::from_vec(raw_path.to_vec()));
    if !source.is_absolute() {
        return Err("Nearby Send source must be an absolute Files grant path".into());
    }
    grants
        .iter()
        .find_map(|grant| {
            let Capability::Filesystem { path, .. } = grant else {
                return None;
            };
            SecureNearbySource::open_beneath(path, &source).ok()
        })
        .ok_or_else(|| "Nearby Send source is outside this Files instance's grants".into())
}

fn nearby_policy_enabled(policy: ContinuityPolicySnapshot) -> bool {
    policy.permits(ContinuityCapability::FileTransfer)
}

fn rejected(reason: impl Into<String>) -> runtime_collab::NearbyTransferReply {
    runtime_collab::NearbyTransferReply::Rejected {
        reason: reason.into(),
    }
}

fn outcome(result: Result<(), String>) -> runtime_collab::NearbyTransferReply {
    match result {
        Ok(()) => runtime_collab::NearbyTransferReply::Accepted,
        Err(reason) => rejected(reason),
    }
}

/// Nearby Send over the certified personal-device network, confined to the
/// Files instance's grants in its workspace.
pub struct NativeNearbyTransfer {
    network: Arc<crate::shell::network::ShellNetwork>,
    membership: WorkspaceTreeHandle,
    targets: Mutex<NearbyTargetRegistry>,
    transfers: Mutex<NearbyTransferRegistry>,
    destinations: Mutex<NearbyDestinationRegistry>,
}

impl NativeNearbyTransfer {
    pub fn new(
        network: Arc<crate::shell::network::ShellNetwork>,
        membership: WorkspaceTreeHandle,
    ) -> Self {
        Self {
            network,
            membership,
            targets: Mutex::default(),
            transfers: Mutex::default(),
            destinations: Mutex::default(),
        }
    }

    /// The Files application's live grants in `workspace_id`.
    fn files_grants(&self, workspace_id: &str) -> Vec<Capability> {
        self.membership
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == workspace_id)
            .and_then(|workspace| workspace.app_grants_snapshot().remove(FILES_APP_ID))
            .unwrap_or_default()
    }

    fn snapshot(
        &self,
        context: &NearbyTransferRequestContext,
        grants: &[Capability],
    ) -> Result<runtime_collab::NearbyTransferReply, String> {
        let now = Instant::now();
        Ok(runtime_collab::NearbyTransferReply::Snapshot {
            snapshot: runtime_collab::NearbyTransferSnapshot {
                targets: self.snapshot_targets(context, now)?,
                destinations: self.snapshot_destinations(context, grants, now)?,
                transfers: self.snapshot_transfers(context, now)?,
            },
        })
    }

    /// The person's other online devices, each behind a fresh handle.
    fn snapshot_targets(
        &self,
        context: &NearbyTransferRequestContext,
        now: Instant,
    ) -> Result<Vec<runtime_collab::NearbyTransferTarget>, String> {
        let (peer_id, channel) = (context.peer_id(), context.channel());
        let local = self
            .network
            .local_device_fabric_identity()
            .map(|(device, _)| device);
        let targets = self
            .network
            .device_fabric()
            .snapshots()
            .values()
            .filter(|snapshot| snapshot.presence.online && local.as_ref() != Some(&snapshot.owner))
            .take(runtime_collab::MAX_NEARBY_TRANSFER_TARGETS)
            .map(|snapshot| (snapshot.owner.clone(), snapshot.presence.label.clone()))
            .collect::<Vec<_>>();
        let device_ids = targets
            .iter()
            .map(|(device, _)| device.clone())
            .collect::<Vec<_>>();
        let mut registry = self
            .targets
            .lock()
            .map_err(|_| "Nearby Send target authority is unavailable".to_string())?;
        registry.begin_snapshot(peer_id, channel, &device_ids, now);
        let issued = targets
            .into_iter()
            .map(|(device, label)| runtime_collab::NearbyTransferTarget {
                handle: registry.issue(peer_id, channel, device, now),
                label,
                online: true,
            })
            .collect();
        drop(registry);
        Ok(issued)
    }

    /// The transfers this device is running, each behind the handle of the
    /// connection that owns it.
    fn snapshot_transfers(
        &self,
        context: &NearbyTransferRequestContext,
        now: Instant,
    ) -> Result<Vec<runtime_collab::NearbyTransferStatus>, String> {
        let (peer_id, channel) = (context.peer_id(), context.channel());
        let raw_transfers = fabric::nearby_transfer_statuses(&self.network);
        let visible_ids = raw_transfers
            .iter()
            .map(|status| status.transfer_id)
            .collect::<Vec<_>>();
        let mut registry = self
            .transfers
            .lock()
            .map_err(|_| "Nearby Send transfer authority is unavailable".to_string())?;
        registry.begin_snapshot(peer_id, channel, &visible_ids, now);
        let issued = raw_transfers
            .into_iter()
            .filter_map(|status| {
                let handle =
                    registry.issue(peer_id, channel, status.transfer_id, status.direction, now)?;
                Some(runtime_collab::NearbyTransferStatus {
                    handle,
                    name: status.name,
                    device_label: status.device_label,
                    direction: status.direction,
                    phase: status.phase,
                    transferred_bytes: status.transferred_bytes,
                    total_bytes: status.total_bytes,
                })
            })
            .collect();
        drop(registry);
        Ok(issued)
    }

    /// The writable directories Files was granted, where a received file
    /// may land.
    fn snapshot_destinations(
        &self,
        context: &NearbyTransferRequestContext,
        grants: &[Capability],
        now: Instant,
    ) -> Result<Vec<runtime_collab::NearbyTransferDestination>, String> {
        let (peer_id, channel) = (context.peer_id(), context.channel());
        let mut destinations = self
            .destinations
            .lock()
            .map_err(|_| "Nearby Send destination authority is unavailable".to_string())?;
        Ok(grants
            .iter()
            .filter_map(|grant| {
                let Capability::Filesystem {
                    path,
                    mode: FsMode::ReadWrite,
                    ..
                } = grant
                else {
                    return None;
                };
                let grant = std::fs::canonicalize(path).ok()?;
                if !std::fs::metadata(&grant).ok()?.is_dir() {
                    return None;
                }
                let label = grant
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or("Workspace files")
                    .chars()
                    .take(runtime_collab::MAX_NEARBY_TRANSFER_LABEL_BYTES)
                    .collect::<String>();
                Some(destinations.issue(peer_id, channel, grant, label, now))
            })
            .take(runtime_collab::MAX_NEARBY_TRANSFER_DESTINATIONS)
            .collect())
    }

    fn send(
        &self,
        context: &NearbyTransferRequestContext,
        grants: &[Capability],
        target: runtime_collab::NearbyTargetHandle,
        source_path: &[u8],
    ) -> runtime_collab::NearbyTransferReply {
        let (peer_id, channel) = (context.peer_id(), context.channel());
        let source = match nearby_source_from_grants(source_path, grants) {
            Ok(source) => source,
            Err(reason) => return rejected(reason),
        };
        let target =
            self.targets.lock().ok().and_then(|mut registry| {
                registry.consume(target, peer_id, channel, Instant::now())
            });
        let Some(target) = target else {
            return rejected("Nearby Send target expired; reopen the device picker");
        };
        let staging_owner = format!("{peer_id}:{channel}");
        match fabric::request_nearby_drop_secure(&self.network, &target, source, &staging_owner) {
            Ok(transfer_id) => {
                let handle = self.transfers.lock().ok().and_then(|mut registry| {
                    registry.issue(
                        peer_id,
                        channel,
                        transfer_id,
                        runtime_collab::NearbyTransferDirection::Sending,
                        Instant::now(),
                    )
                });
                if let Some(handle) = handle {
                    runtime_collab::NearbyTransferReply::Queued { handle }
                } else {
                    let _ = fabric::cancel_nearby_transfer(&self.network, transfer_id);
                    rejected("Nearby Send transfer authority is full")
                }
            }
            Err(reason) => rejected(reason),
        }
    }

    /// The transfer `handle` names for this connection, when it moves in
    /// `direction`.
    fn transfer(
        &self,
        context: &NearbyTransferRequestContext,
        handle: runtime_collab::NearbyTransferHandle,
        direction: runtime_collab::NearbyTransferDirection,
    ) -> Option<[u8; 16]> {
        self.transfers.lock().ok().and_then(|mut registry| {
            registry.resolve(
                handle,
                context.peer_id(),
                context.channel(),
                direction,
                Instant::now(),
            )
        })
    }

    fn decide(
        &self,
        context: &NearbyTransferRequestContext,
        handle: runtime_collab::NearbyTransferHandle,
        accept: bool,
        destination: Option<runtime_collab::NearbyDestinationHandle>,
        remember_for_session: bool,
    ) -> runtime_collab::NearbyTransferReply {
        let Some(transfer_id) = self.transfer(
            context,
            handle,
            runtime_collab::NearbyTransferDirection::Receiving,
        ) else {
            return rejected(EXPIRED_TRANSFER);
        };
        let destination = if accept {
            destination.and_then(|destination| {
                self.destinations.lock().ok().and_then(|mut destinations| {
                    destinations.resolve(
                        destination,
                        context.peer_id(),
                        context.channel(),
                        remember_for_session,
                        Instant::now(),
                    )
                })
            })
        } else {
            None
        };
        if accept && destination.is_none() {
            return rejected("Nearby Send destination expired; choose it again");
        }
        outcome(fabric::decide_nearby_transfer_at(
            &self.network,
            transfer_id,
            accept,
            destination,
        ))
    }
}

impl NearbyTransferServices for NativeNearbyTransfer {
    fn command(
        &self,
        context: &NearbyTransferRequestContext,
        command: runtime_collab::NearbyTransferCommand,
    ) -> Result<runtime_collab::NearbyTransferReply, String> {
        if !nearby_policy_enabled(ContinuityPolicySnapshot::load()) {
            return Err("Nearby Send is disabled by continuity privacy settings".into());
        }
        let grants = self.files_grants(context.workspace_id());
        Ok(match command {
            runtime_collab::NearbyTransferCommand::Snapshot => self.snapshot(context, &grants)?,
            runtime_collab::NearbyTransferCommand::Send {
                target,
                source_path,
            } => self.send(context, &grants, target, &source_path),
            runtime_collab::NearbyTransferCommand::Cancel { handle } => {
                match self.transfer(
                    context,
                    handle,
                    runtime_collab::NearbyTransferDirection::Sending,
                ) {
                    Some(transfer_id) => {
                        outcome(fabric::cancel_nearby_transfer(&self.network, transfer_id))
                    }
                    None => rejected(EXPIRED_TRANSFER),
                }
            }
            runtime_collab::NearbyTransferCommand::Decide {
                handle,
                accept,
                destination,
                remember_for_session,
            } => self.decide(context, handle, accept, destination, remember_for_session),
            runtime_collab::NearbyTransferCommand::Retry { handle } => {
                match self.transfer(
                    context,
                    handle,
                    runtime_collab::NearbyTransferDirection::Sending,
                ) {
                    Some(transfer_id) => {
                        outcome(fabric::retry_nearby_transfer(&self.network, transfer_id))
                    }
                    None => rejected(EXPIRED_TRANSFER),
                }
            }
        })
    }

    fn disconnect(&self, peer_id: u64) {
        if let Ok(mut targets) = self.targets.lock() {
            targets.remove_peer(peer_id);
        }
        if let Ok(mut transfers) = self.transfers.lock() {
            transfers.remove_peer(peer_id);
        }
        if let Ok(mut destinations) = self.destinations.lock() {
            destinations.remove_peer(peer_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::GRANT_SESSION;
    use tilo_action_broker::route::CrdtRoute;

    fn channel(room: &str) -> String {
        CrdtRoute::app(FILES_APP_ID, room).channel_key()
    }

    #[test]
    fn nearby_target_handles_reject_spoofed_routes_and_expiry() {
        let route = channel("room-a");
        let other_route = channel("room-b");
        let target = DeviceId::new("certified-device").expect("valid device");
        let now = Instant::now();
        let mut registry = NearbyTargetRegistry::default();
        let handle = registry.issue(41, &route, target.clone(), now);

        assert!(registry.consume(handle, 42, &route, now).is_none());
        assert!(registry.consume(handle, 41, &other_route, now).is_none());
        assert_eq!(registry.consume(handle, 41, &route, now), Some(target));
        assert!(registry.consume(handle, 41, &route, now).is_none());

        let stale = registry.issue(
            41,
            &route,
            DeviceId::new("stale-device").expect("valid device"),
            now,
        );
        assert!(
            registry
                .consume(
                    stale,
                    41,
                    &route,
                    now + NEARBY_TARGET_HANDLE_TTL + Duration::from_millis(1),
                )
                .is_none()
        );

        let retained_target = DeviceId::new("retained-device").expect("valid device");
        let retained = registry.issue(41, &route, retained_target.clone(), now);
        registry.begin_snapshot(41, &route, std::slice::from_ref(&retained_target), now);
        let refreshed = registry.issue(41, &route, retained_target, now);
        assert_eq!(retained, refreshed);
        assert_eq!(registry.bindings.len(), 1);
        registry.begin_snapshot(41, &route, &[], now);
        assert!(registry.consume(retained, 41, &route, now).is_none());
        assert!(registry.bindings.is_empty());
    }

    #[test]
    fn nearby_transfer_handles_reject_cross_workspace_actions_and_disconnect() {
        let route = channel("room-a");
        let other_route = channel("room-b");
        let transfer_id = [7_u8; 16];
        let now = Instant::now();
        let receiving = runtime_collab::NearbyTransferDirection::Receiving;
        let mut registry = NearbyTransferRegistry::default();
        let handle = registry
            .issue(41, &route, transfer_id, receiving, now)
            .expect("owner receives opaque handle");
        assert!(
            registry
                .resolve(handle, 42, &route, receiving, now)
                .is_none()
        );
        assert!(
            registry
                .resolve(handle, 41, &other_route, receiving, now)
                .is_none()
        );
        assert!(
            registry
                .resolve(
                    handle,
                    41,
                    &route,
                    runtime_collab::NearbyTransferDirection::Sending,
                    now
                )
                .is_none()
        );
        assert!(
            registry
                .issue(42, &other_route, transfer_id, receiving, now)
                .is_none()
        );
        registry.remove_peer(41);
        assert!(
            registry
                .resolve(handle, 41, &route, receiving, now)
                .is_none()
        );
    }

    #[test]
    fn nearby_destination_handles_are_route_bound_and_session_remembered() {
        let root = tempfile::tempdir().expect("destination root");
        let route = channel("room-a");
        let other_route = channel("room-b");
        let now = Instant::now();
        let mut registry = NearbyDestinationRegistry::default();
        let destination = registry.issue(
            41,
            &route,
            root.path().to_path_buf(),
            "Workspace files".into(),
            now,
        );
        assert!(
            registry
                .resolve(destination.handle, 42, &route, false, now)
                .is_none()
        );
        assert!(
            registry
                .resolve(destination.handle, 41, &other_route, false, now)
                .is_none()
        );
        assert!(
            registry
                .resolve(destination.handle, 41, &route, true, now)
                .is_some()
        );
        let refreshed = registry.issue(
            41,
            &route,
            root.path().to_path_buf(),
            "Workspace files".into(),
            now,
        );
        assert_eq!(refreshed.handle, destination.handle);
        assert!(refreshed.remembered_for_session);
        registry.remove_peer(41);
        assert!(
            registry
                .resolve(destination.handle, 41, &route, false, now)
                .is_none()
        );
    }

    #[test]
    fn nearby_handle_expiry_adds_ttl_and_fails_closed_on_overflow() {
        let now = Instant::now();
        assert_eq!(
            Some(nearby_handle_expiry(now, NEARBY_TARGET_HANDLE_TTL)),
            now.checked_add(NEARBY_TARGET_HANDLE_TTL)
        );
        assert_eq!(nearby_handle_expiry(now, Duration::ZERO), now);
        assert_eq!(nearby_handle_expiry(now, Duration::MAX), now);
    }

    #[test]
    fn nearby_source_is_canonical_and_confined_to_live_files_grants() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temp grant root");
        let source = root.path().join("report.txt");
        std::fs::write(&source, b"report").expect("write source");
        let grants = vec![Capability::Filesystem {
            path: root.path().to_path_buf(),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        }];
        let opened = nearby_source_from_grants(source.as_os_str().as_encoded_bytes(), &grants)
            .expect("granted source");
        assert_eq!(opened.name(), "report.txt");
        assert_eq!(opened.metadata().expect("source metadata").len(), 6);
        assert!(nearby_source_from_grants(b"/etc/passwd", &grants).is_err());

        let escape = root.path().join("escape");
        symlink("/etc/passwd", &escape).expect("create escape symlink");
        assert!(nearby_source_from_grants(escape.as_os_str().as_encoded_bytes(), &grants).is_err());
    }

    #[test]
    fn nearby_rpc_policy_requires_master_and_file_transfer_switches() {
        let policy = |enabled, file_transfer| {
            let document = serde_json::json!({
                "continuity": {
                    "enabled": enabled,
                    "file_transfer": file_transfer,
                }
            });
            ContinuityPolicySnapshot::from_document(document.as_object().expect("policy object"))
        };
        assert!(!nearby_policy_enabled(policy(false, true)));
        assert!(!nearby_policy_enabled(policy(true, false)));
        assert!(nearby_policy_enabled(policy(true, true)));
    }
}
