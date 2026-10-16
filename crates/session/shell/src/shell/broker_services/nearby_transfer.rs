//! Nearby Send for the certified Files application over the personal device
//! fabric. The shared broker admits the route and bounds the wire; this host
//! owns the opaque handles, each bound to the connection and channel it was
//! issued to, and the Files instance's live filesystem grants.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_action_broker::services::{
    NearbyTransferChange, NearbyTransferRequestContext, NearbyTransferServices,
};

use super::super::state::WorkspaceAuthority as _;
use super::super::state::{Capability, FsMode, WorkspaceTreeHandle};
use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::device_fabric_runtime as fabric;
use crate::shell::nearby_transfer::{SecureNearbyDestination, SecureNearbySource};
use crate::shell::personal_continuity::DeviceId;

/// As long as transfer and destination handles: Files no longer re-asks
/// every two seconds, so a target handle published through Files' actions is
/// kept fresh by one snapshot shortly before it lapses
/// ([`NEARBY_HANDLE_REFRESH_LEAD`]), not by a poll.
const NEARBY_TARGET_HANDLE_TTL: Duration = Duration::from_secs(120);
const NEARBY_TRANSFER_HANDLE_TTL: Duration = Duration::from_secs(120);
/// How long before a live handle lapses its holder is told to ask again, so
/// the handles Files publishes stay usable while nothing else changes.
const NEARBY_HANDLE_REFRESH_LEAD: Duration = Duration::from_secs(15);
const _: () = assert!(
    NEARBY_HANDLE_REFRESH_LEAD.as_secs() < NEARBY_TARGET_HANDLE_TTL.as_secs()
        && NEARBY_HANDLE_REFRESH_LEAD.as_secs() < NEARBY_TRANSFER_HANDLE_TTL.as_secs()
);
const MAX_NEARBY_TRANSFER_HANDLES: usize = 256;
const NEARBY_DESTINATION_HANDLE_TTL: Duration = Duration::from_secs(120);
pub(super) const FILES_APP_ID: &str = "so.tilo.file-explorer";
const EXPIRED_TRANSFER: &str = "Nearby Send transfer expired or belongs to another Files workspace";

/// When a nearby handle issued or refreshed at `now` stops being valid.
///
/// An expiry the clock cannot represent fails closed: the handle expires at
/// `now` instead of never, so an unrepresentable deadline cannot mint an
/// immortal capability handle.
fn nearby_handle_expiry(now: Instant, ttl: Duration) -> Instant {
    now.checked_add(ttl).unwrap_or(now)
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
                runtime_collab::NearbyTargetHandle::from_issued_bytes(crate::random::bytes::<16>());
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

    /// When the first handle still held by this connection lapses.
    fn next_expiry(&self, peer_id: u64, channel: &str, now: Instant) -> Option<Instant> {
        self.bindings
            .values()
            .filter(|binding| binding.peer_id == peer_id && binding.channel == channel)
            .map(|binding| binding.expires_at)
            .filter(|expires_at| *expires_at > now)
            .min()
    }
}

#[derive(Clone)]
struct NearbyTransferBinding {
    peer_id: u64,
    channel: String,
    transfer_id: [u8; 16],
    direction: runtime_collab::NearbyTransferDirection,
    expires_at: Instant,
    /// The transfer completed or was cancelled when last listed: its handle
    /// still names it, but nothing can be done with it, so it is not kept
    /// fresh.
    settled: bool,
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
            let handle = runtime_collab::NearbyTransferHandle::from_issued_bytes(
                crate::random::bytes::<16>(),
            );
            if handle.valid() && !self.bindings.contains_key(&handle) {
                self.bindings.insert(
                    handle,
                    NearbyTransferBinding {
                        peer_id,
                        channel: channel.to_owned(),
                        transfer_id,
                        direction,
                        expires_at: nearby_handle_expiry(now, NEARBY_TRANSFER_HANDLE_TTL),
                        settled: false,
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

    /// Record whether `handle`'s transfer has finished for good.
    fn settle(&mut self, handle: runtime_collab::NearbyTransferHandle, settled: bool) {
        if let Some(binding) = self.bindings.get_mut(&handle) {
            binding.settled = settled;
        }
    }

    /// When the first handle this connection can still act on lapses.
    fn next_expiry(&self, peer_id: u64, channel: &str, now: Instant) -> Option<Instant> {
        self.bindings
            .values()
            .filter(|binding| {
                binding.peer_id == peer_id && binding.channel == channel && !binding.settled
            })
            .map(|binding| binding.expires_at)
            .filter(|expires_at| *expires_at > now)
            .min()
    }
}

/// Whether a transfer moving in `direction` and in `phase` is over, with
/// nothing left to decide, cancel or retry: only a failed send is retried.
fn settled(
    direction: runtime_collab::NearbyTransferDirection,
    phase: runtime_collab::NearbyTransferPhase,
) -> bool {
    use runtime_collab::NearbyTransferPhase as Phase;
    match phase {
        Phase::Completed | Phase::Cancelled => true,
        Phase::Failed => direction == runtime_collab::NearbyTransferDirection::Receiving,
        Phase::Queued
        | Phase::Transferring
        | Phase::AwaitingConsent
        | Phase::AwaitingDestination => false,
    }
}

/// When the connection `peer_id` on `channel` must next ask for a snapshot
/// to keep the handles it can act on usable: shortly before the first of
/// them lapses. A handle already that close is not counted again: its
/// holder was told when it got there, and a wait that counted it would spin
/// until it lapsed. `None` while there is nothing to keep.
fn handles_due(
    targets: &Mutex<NearbyTargetRegistry>,
    transfers: &Mutex<NearbyTransferRegistry>,
    peer_id: u64,
    channel: &str,
    now: Instant,
) -> Option<Instant> {
    let targets = targets
        .lock()
        .ok()
        .and_then(|registry| registry.next_expiry(peer_id, channel, now));
    let transfers = transfers
        .lock()
        .ok()
        .and_then(|registry| registry.next_expiry(peer_id, channel, now));
    targets
        .into_iter()
        .chain(transfers)
        .filter_map(|expiry| expiry.checked_sub(NEARBY_HANDLE_REFRESH_LEAD))
        .filter(|due| *due > now)
        .min()
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
                runtime_collab::NearbyDestinationHandle::from_issued_bytes(crate::random::bytes::<
                    16,
                >());
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

pub(super) fn nearby_source_from_grants(
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
    targets: Arc<Mutex<NearbyTargetRegistry>>,
    transfers: Arc<Mutex<NearbyTransferRegistry>>,
    destinations: Mutex<NearbyDestinationRegistry>,
    /// Moved after every command: a snapshot, send or decision may have
    /// renewed the handles a connection holds, so when it must next ask to
    /// keep them usable moved too.
    renewed: Arc<tokio::sync::watch::Sender<()>>,
    #[cfg(target_os = "linux")]
    airdrop: super::airdrop_transfer::NativeAirDropTransfer,
}

impl NativeNearbyTransfer {
    #[cfg(target_os = "linux")]
    fn send_airdrop(
        &self,
        context: &NearbyTransferRequestContext,
        grants: &[Capability],
        source_path: Vec<u8>,
        address: String,
        port: u16,
    ) -> runtime_collab::NearbyTransferReply {
        let Some(person) = context.person() else {
            return rejected(crate::shell::i18n::t("airdrop-error-unavailable"));
        };
        let source = match nearby_source_from_grants(&source_path, grants) {
            Ok(source) => source,
            Err(reason) => return rejected(reason),
        };
        let request = super::airdrop_transfer::AirDropRequest {
            scope: super::airdrop_transfer::AirDropScope {
                peer_id: context.peer_id(),
                channel: context.channel().into(),
                workspace: context.workspace_id().into(),
                person: person.into(),
            },
            source_path,
            address,
            port,
        };
        let id = match self.airdrop.start(request, source) {
            Ok(id) => id,
            Err(reason) => return rejected(reason),
        };
        let handle = self.transfers.lock().ok().and_then(|mut registry| {
            registry.issue(
                context.peer_id(),
                context.channel(),
                id,
                runtime_collab::NearbyTransferDirection::Sending,
                Instant::now(),
            )
        });
        if let Some(handle) = handle {
            runtime_collab::NearbyTransferReply::Queued { handle }
        } else {
            self.airdrop.cancel(id);
            rejected(crate::shell::i18n::t("airdrop-error-busy"))
        }
    }

    pub fn new(
        network: Arc<crate::shell::network::ShellNetwork>,
        membership: WorkspaceTreeHandle,
    ) -> Self {
        Self {
            #[cfg(target_os = "linux")]
            airdrop: super::airdrop_transfer::NativeAirDropTransfer::session(membership.clone()),
            network,
            membership,
            targets: Arc::default(),
            transfers: Arc::default(),
            destinations: Mutex::default(),
            renewed: Arc::new(tokio::sync::watch::Sender::new(())),
        }
    }

    /// The Files application's live grants in `workspace_id`.
    fn files_grants(&self, workspace_id: &str) -> Vec<Capability> {
        files_grants(&self.membership, workspace_id)
    }
}

pub(super) fn files_grants(
    membership: &WorkspaceTreeHandle,
    workspace_id: &str,
) -> Vec<Capability> {
    membership
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == workspace_id)
        .and_then(|workspace| workspace.app_grants_snapshot().remove(FILES_APP_ID))
        .unwrap_or_default()
}

impl NativeNearbyTransfer {
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
        let fabric::NearbyTransferStatuses {
            statuses: raw_transfers,
            complete,
        } = fabric::nearby_transfer_statuses(&self.network);
        if !complete {
            fabric::served_incomplete_nearby_view();
        }
        #[cfg(target_os = "linux")]
        let external = self.airdrop.statuses(peer_id, channel);
        let visible_ids = raw_transfers
            .iter()
            .map(|status| status.transfer_id)
            .collect::<Vec<_>>();
        #[cfg(target_os = "linux")]
        let visible_ids = {
            let mut visible_ids = visible_ids;
            visible_ids.extend(external.iter().map(|status| status.id));
            visible_ids
        };
        let mut registry = self
            .transfers
            .lock()
            .map_err(|_| "Nearby Send transfer authority is unavailable".to_string())?;
        registry.begin_snapshot(peer_id, channel, &visible_ids, now);
        let issued: Vec<_> = raw_transfers
            .into_iter()
            .filter_map(|status| {
                let handle =
                    registry.issue(peer_id, channel, status.transfer_id, status.direction, now)?;
                registry.settle(handle, settled(status.direction, status.phase));
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
        #[cfg(target_os = "linux")]
        let issued = {
            let mut issued = issued;
            for status in external {
                if let Some(handle) = registry.issue(
                    peer_id,
                    channel,
                    status.id,
                    runtime_collab::NearbyTransferDirection::Sending,
                    now,
                ) {
                    registry.settle(
                        handle,
                        settled(
                            runtime_collab::NearbyTransferDirection::Sending,
                            status.phase,
                        ),
                    );
                    issued.push(runtime_collab::NearbyTransferStatus {
                        handle,
                        name: status.name,
                        device_label: status.endpoint,
                        direction: runtime_collab::NearbyTransferDirection::Sending,
                        phase: status.phase,
                        total_bytes: status.bytes,
                        transferred_bytes: if status.phase
                            == runtime_collab::NearbyTransferPhase::Completed
                        {
                            status.bytes
                        } else {
                            0
                        },
                    });
                }
            }
            issued
        };
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

impl NativeNearbyTransfer {
    fn run_command(
        &self,
        context: &NearbyTransferRequestContext,
        command: runtime_collab::NearbyTransferCommand,
    ) -> Result<runtime_collab::NearbyTransferReply, String> {
        if !nearby_policy_enabled(ContinuityPolicySnapshot::current())
            && !matches!(
                command,
                runtime_collab::NearbyTransferCommand::AirDropAvailability
            )
        {
            return Err("Nearby Send is disabled by continuity privacy settings".into());
        }
        let grants = self.files_grants(context.workspace_id());
        Ok(match command {
            runtime_collab::NearbyTransferCommand::AirDropAvailability => {
                runtime_collab::NearbyTransferReply::AirDropAvailability {
                    available: {
                        #[cfg(target_os = "linux")]
                        {
                            self.airdrop
                                .available_to(context.workspace_id(), context.person())
                        }
                        #[cfg(not(target_os = "linux"))]
                        {
                            false
                        }
                    },
                }
            }
            runtime_collab::NearbyTransferCommand::SendAirDrop {
                source_path,
                address,
                port,
            } => {
                #[cfg(target_os = "linux")]
                {
                    self.send_airdrop(context, &grants, source_path, address, port)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (source_path, address, port);
                    rejected("AirDrop lab is unavailable")
                }
            }
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
                        #[cfg(target_os = "linux")]
                        if self.airdrop.cancel(transfer_id) {
                            return Ok(runtime_collab::NearbyTransferReply::Accepted);
                        }
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
                        #[cfg(target_os = "linux")]
                        if self.airdrop.contains(transfer_id) {
                            return Ok(rejected(crate::shell::i18n::t(
                                "airdrop-error-fresh-consent",
                            )));
                        }
                        outcome(fabric::retry_nearby_transfer(&self.network, transfer_id))
                    }
                    None => rejected(EXPIRED_TRANSFER),
                }
            }
        })
    }
}

impl NearbyTransferServices for NativeNearbyTransfer {
    fn command(
        &self,
        context: &NearbyTransferRequestContext,
        command: runtime_collab::NearbyTransferCommand,
    ) -> Result<runtime_collab::NearbyTransferReply, String> {
        let reads = matches!(
            command,
            runtime_collab::NearbyTransferCommand::Snapshot
                | runtime_collab::NearbyTransferCommand::AirDropAvailability
        );
        let reply = self.run_command(context, command);
        if !reads {
            // A send, decision, cancel or retry moves the view now; every
            // Files instance hears of it without waiting for a compositor
            // turn.
            fabric::observe_nearby_view(&self.network, ContinuityPolicySnapshot::current());
        }
        self.renewed.send_replace(());
        reply
    }

    /// Resolves at the next announced change, at an offer's consent expiry,
    /// or shortly before the handles this connection can act on lapse. With
    /// no other device online and no transfer under way none of those come,
    /// and the wait costs no wakeup at all.
    fn changed(
        &self,
        context: &NearbyTransferRequestContext,
        after: u64,
    ) -> Option<NearbyTransferChange> {
        // Subscribed before anything is read, so a change or a renewal in
        // between is still seen.
        let mut changes = fabric::nearby_changes().subscribe();
        let renewals = Arc::clone(&self.renewed);
        let mut renewed = renewals.subscribe();
        let (targets, transfers) = (Arc::clone(&self.targets), Arc::clone(&self.transfers));
        let (peer_id, channel) = (context.peer_id(), context.channel().to_owned());
        Some(Box::pin(async move {
            // Held so the renewal channel outlives this wait.
            let _renewals = renewals;
            loop {
                let current = *changes.borrow_and_update();
                if current != after {
                    return current;
                }
                let now = Instant::now();
                // A disabled service answers every snapshot with a refusal,
                // renews no handle and expires no consent.
                let deadline = nearby_policy_enabled(ContinuityPolicySnapshot::current())
                    .then(|| {
                        handles_due(&targets, &transfers, peer_id, &channel, now)
                            .into_iter()
                            .chain(fabric::next_nearby_consent_expiry())
                            .filter(|deadline| *deadline > now)
                            .min()
                    })
                    .flatten();
                let due = async {
                    match deadline {
                        Some(deadline) => {
                            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))
                                .await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::select! {
                    // The revision and the renewals live as long as their
                    // senders, which outlive this wait: neither ends.
                    _ = changes.changed() => {}
                    _ = renewed.changed() => {}
                    () = due => fabric::note_nearby_changed(),
                }
            }
        }))
    }

    fn disconnect(&self, peer_id: u64) {
        #[cfg(target_os = "linux")]
        self.airdrop.disconnect(peer_id);
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

    /// Files no longer re-asks on a timer, so the shell tells it to ask
    /// before the handles it can act on lapse, and only then: a connection
    /// that holds none, or only finished transfers, is never woken for them.
    #[test]
    fn only_live_handles_ask_their_holder_to_refresh() {
        let route = channel("room-a");
        let other_route = channel("room-b");
        let now = Instant::now();

        let mut targets = NearbyTargetRegistry::default();
        assert_eq!(targets.next_expiry(41, &route, now), None, "no handles");
        targets.issue(
            41,
            &route,
            DeviceId::new("desk").expect("valid device"),
            now,
        );
        assert_eq!(
            targets.next_expiry(41, &route, now),
            Some(now + NEARBY_TARGET_HANDLE_TTL)
        );
        assert_eq!(targets.next_expiry(42, &route, now), None, "other peer");
        assert_eq!(
            targets.next_expiry(41, &other_route, now),
            None,
            "other workspace"
        );
        assert_eq!(
            targets.next_expiry(41, &route, now + NEARBY_TARGET_HANDLE_TTL),
            None,
            "a lapsed handle is no reason to wake"
        );

        let mut transfers = NearbyTransferRegistry::default();
        let sending = runtime_collab::NearbyTransferDirection::Sending;
        let handle = transfers
            .issue(41, &route, [3; 16], sending, now)
            .expect("handle");
        assert_eq!(
            transfers.next_expiry(41, &route, now),
            Some(now + NEARBY_TRANSFER_HANDLE_TTL)
        );
        transfers.settle(
            handle,
            settled(sending, runtime_collab::NearbyTransferPhase::Completed),
        );
        assert_eq!(
            transfers.next_expiry(41, &route, now),
            None,
            "a completed transfer's handle is not kept fresh"
        );
        transfers.settle(
            handle,
            settled(sending, runtime_collab::NearbyTransferPhase::Failed),
        );
        assert_eq!(
            transfers.next_expiry(41, &route, now),
            Some(now + NEARBY_TRANSFER_HANDLE_TTL),
            "a failed send can still be retried"
        );
        assert!(settled(
            runtime_collab::NearbyTransferDirection::Receiving,
            runtime_collab::NearbyTransferPhase::Failed
        ));

        // Asked shortly before the first live handle lapses, and once: a
        // handle inside that lead is no reason to wake again.
        let (targets, transfers) = (Mutex::new(targets), Mutex::new(transfers));
        let inside_lead = now + NEARBY_TARGET_HANDLE_TTL.saturating_sub(NEARBY_HANDLE_REFRESH_LEAD);
        assert_eq!(
            handles_due(&targets, &transfers, 41, &route, now),
            Some(inside_lead)
        );
        assert_eq!(
            handles_due(&targets, &transfers, 41, &route, inside_lead),
            None
        );
        assert_eq!(handles_due(&targets, &transfers, 42, &route, now), None);
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
