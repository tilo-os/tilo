//! Nearby Send: one of this device's files handed to another of the person's
//! devices over the device fabric this session already runs.
//!
//! One engine carries every file that leaves this device towards another of
//! the person's: a screenshot sent from the capture studio and a file sent
//! from Files are the same offer, staged in the same private spool and
//! streamed from the same object. What leaves is an offer — an ordinary
//! personal fabric action naming an immutable spool object — the receiving
//! device's person chooses where it lands there, and only then do the bytes
//! stream from the checkpoint that device says it holds. No source path
//! leaves this device, and nothing lands on the other one without its
//! person's consent.
//!
//! Files reaches it through the action API ([`NearbyTransferServices`]): the
//! broker binds every handle it is handed to the connection that asked and to
//! that connection's workspace, and this engine refuses a handle of another
//! connection. The spool belongs to a worker thread of the engine's own,
//! which stages, appends and installs off the session's pass; what a snapshot
//! shows is that worker's last projection, never a read that could wait on a
//! disk.
//!
//! A receiver that is not one of the person's devices is reached the `AirDrop`
//! way: a literal endpoint and the receiver's leaf certificate, approved by
//! the person for one attempt on staged bytes (`answer_air_drop`).
//! That attempt runs on a thread of its own, since its helper may wait on a
//! receiver for a minute, and appears in Files' list beside a device send. It is
//! offered only to a development session that asked for it.

use collab_types::caps::{Access, CapDecision, ResourceRef, Scope};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{Hash as _, Hasher as _};
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};

use crate::platform::fabric::{DeviceId, PersonalFabricAction};
use crate::platform::fabric::{FabricLink as _, NearbySpoolOffer, SharedLink};
use runtime_collab::{
    MAX_NEARBY_TRANSFER_DESTINATIONS, MAX_NEARBY_TRANSFER_LABEL_BYTES,
    MAX_NEARBY_TRANSFER_NAME_BYTES, MAX_NEARBY_TRANSFER_STATUSES, MAX_NEARBY_TRANSFER_TARGETS,
    NearbyDestinationHandle, NearbyTargetHandle, NearbyTransferCommand, NearbyTransferDestination,
    NearbyTransferDirection, NearbyTransferHandle, NearbyTransferPhase, NearbyTransferReply,
    NearbyTransferSnapshot, NearbyTransferStatus, NearbyTransferTarget,
};
use tilo_broker_core::services::WorkspaceAuthority;
use tilo_capture_studio::{Refusal, Sent};
use tilo_lock::MutexRecover as _;
use tilo_nearby_service::{
    IncomingManifest, MAX_TRANSFER_BYTES, NearbyTransferStore, SecureNearbyDestination,
    SecureNearbySource, TransferKind,
};
use tilo_network_actor::{
    NearbyTransferCheckpoint, PERSONAL_TRANSFER_CHUNK_BYTES, PersonalNearbyTransferFrame,
    PersonalTransferStatus,
};
use tilo_service::Wake;
use tilo_workspace_service::apps::services::{
    NearbyTransferChange, NearbyTransferRequestContext, NearbyTransferServices,
};
use ui::widget::ScreenshotSendDevice;

use crate::platform::fabric::Continuity;

/// The one app the action API serves: the certified Files application.
pub use ui::apps::FILE_EXPLORER as FILES_APP_ID;

/// How long the handles one Files connection holds stay usable. Files does not ask again on a
/// timer, so a wait it holds is woken to ask shortly before they lapse ([`HANDLE_REFRESH`]).
const HANDLE_TTL: Duration = Duration::from_secs(120);
/// When a wait is woken so Files takes a snapshot and its handles are renewed while they are still
/// good.
const HANDLE_REFRESH: Duration = Duration::from_secs(105);
const MAX_TRANSFER_HANDLES: usize = 256;
/// How long an offer this device received waits for its person's decision before it is withdrawn.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PENDING_OFFERS: usize = 64;
/// How long a staged object no device acknowledged waits before it is offered again: a device that
/// restarted, or whose person was away, still gets it.
const OFFER_AGAIN: Duration = Duration::from_secs(10);
const MAX_HISTORY: usize = 32;
const NOT_YOURS: &str = "this Nearby Send handle belongs to another connection";
const NO_DESTINATION: &str = "that folder is not one of this workspace's files";
const NO_WORKER: &str = "the nearby transfer worker stopped";

/// The program that speaks `AirDrop`'s wire: an independent, bounded helper the session runs with
/// only the descriptors it needs.
const AIRDROP_HELPER: &str = "/usr/bin/python3";
const AIRDROP_SENDER: &str = include_str!("../../../../tools/protocols/airdrop_sender.py");
/// The size the `AirDrop` adapter carries, as the receiver's own protocol limits it.
const AIRDROP_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// The most a helper may say back: one small JSON object.
const AIRDROP_REPLY_BYTES: u64 = 1024;
/// How long one helper process may run, interpreter start included.
const AIRDROP_TIMEOUT: Duration = Duration::from_secs(65);
/// How long the receiver's verification waits for the person before the attempt is withdrawn.
const AIRDROP_CONSENT_TIMEOUT: Duration = Duration::from_secs(120);
/// How long an attempt that ended stays in Files' list.
const AIRDROP_FORGET: Duration = Duration::from_secs(120);
/// One attempt runs at a time; Files' list keeps the ones that ended, up to this many.
const MAX_AIRDROP_JOBS: usize = 32;

/// Where the spool this session stages into lives: beside what the rest of the session keeps,
/// private to the person.
#[must_use]
pub(crate) fn spool_root() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo").join("nearby-transfers"))
}

/// The person's other devices that are here now, as Files and the capture studio list them: online,
/// never this device, by their own names.
#[must_use]
pub(crate) fn devices(link: &SharedLink) -> Vec<ScreenshotSendDevice> {
    let local = link
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let online = link.online_device_ids();
    let fabric = link.devices();
    let mut devices: Vec<ScreenshotSendDevice> = fabric
        .online_devices()
        .filter(|device| online.contains(&device.device))
        .filter(|device| local.as_ref() != Some(&device.device))
        .map(|device| ScreenshotSendDevice {
            id: device.device.as_str().to_owned(),
            label: device.about.label.clone(),
        })
        .collect();
    devices.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
    devices
}

/// What a studio send answers when the worker is done with it, wherever that ends: offered, or
/// refused in the engine's own words.
type StudioReply = Box<dyn FnOnce(Sent) + Send>;

/// One transferred object that is over, as Files' list still shows it.
#[derive(Clone)]
struct Settled {
    transfer_id: [u8; 16],
    peer: DeviceId,
    name: String,
    direction: NearbyTransferDirection,
    phase: NearbyTransferPhase,
    total_bytes: u64,
}

/// One offer that arrived and waits for this device's person to decide.
#[derive(Clone)]
struct Offer {
    source: DeviceId,
    transfer_id: [u8; 16],
    request_id: [u8; 16],
    name: String,
    kind: TransferKind,
    total_bytes: u64,
    content_hash: [u8; 32],
    seen: Instant,
}

impl Offer {
    fn manifest(&self) -> IncomingManifest {
        IncomingManifest {
            id: self.transfer_id,
            source: self.source.as_str().to_owned(),
            request_id: self.request_id,
            name: self.name.clone(),
            kind: self.kind,
            total_bytes: self.total_bytes,
            content_hash: self.content_hash,
        }
    }
}

/// One object this device offered, waiting for the receiving device's checkpoint.
#[derive(Clone)]
struct Offered {
    target: DeviceId,
    request_id: [u8; 16],
}

/// How far one outgoing stream got, as the transport reported it.
#[derive(Clone, Copy)]
struct Stream {
    sent_bytes: u64,
    total_bytes: u64,
    status: PersonalTransferStatus,
}

/// One attempt at a receiver that is not one of the person's devices: a file handed over the
/// `AirDrop` wire, pinned to the leaf certificate the receiver showed and approved by the person on
/// the bytes staged here.
#[derive(Clone)]
struct AirDrop {
    peer_id: u64,
    workspace: String,
    address: String,
    port: u16,
    /// The file Files named, as raw bytes: the attempt reopens it only beneath the workspace's own
    /// Files grant, and each helper run is admitted against the descriptor it was staged from.
    source_path: Vec<u8>,
    name: String,
    /// What Files calls the receiver, as the transfer's device label.
    endpoint: String,
    bytes: u64,
    phase: NearbyTransferPhase,
    created: Instant,
    /// Set to stop the attempt at its next check, whoever stops it.
    cancel: Arc<AtomicBool>,
    /// The receiver's leaf certificate digest, once the helper observed it: what the person is
    /// asked to verify.
    certificate: Option<[u8; 32]>,
    /// The person's answer, once they gave one.
    answer: Option<AirDropAnswer>,
}

/// What the person did with a receiver's verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AirDropAnswer {
    /// Send once, to the receiver whose leaf certificate's SHA-256 digest is `pin`.
    Confirm { pin: String },
    /// Nothing leaves this device.
    Decline,
}

/// What the portal sheet shows of an attempt that waits for the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AirDropAsk {
    pub(crate) id: [u8; 16],
    pub(crate) name: String,
    pub(crate) bytes: u64,
    pub(crate) endpoint: String,
}

/// The one attempt that may run, held for as long as it does.
struct ActiveAirDrop(Arc<AtomicBool>);

impl ActiveAirDrop {
    fn reserve(active: &Arc<AtomicBool>) -> Result<Self, String> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| crate::i18n::t("airdrop-error-busy"))?;
        Ok(Self(Arc::clone(active)))
    }
}

impl Drop for ActiveAirDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Whether an attempt is over, as Files' list reads its phase.
fn over(phase: NearbyTransferPhase) -> bool {
    matches!(
        phase,
        NearbyTransferPhase::Completed
            | NearbyTransferPhase::Failed
            | NearbyTransferPhase::Cancelled
    )
}

/// The spool's two queues, as its worker last left them.
#[derive(Clone, Default)]
struct Queues {
    inbox: Vec<tilo_nearby_service::InboxEntry>,
    outbox: Vec<tilo_nearby_service::OutboxEntry>,
}

#[derive(Clone)]
struct TargetBinding {
    peer_id: u64,
    channel: String,
    target: DeviceId,
    expires_at: Instant,
}

#[derive(Clone)]
struct TransferBinding {
    peer_id: u64,
    channel: String,
    transfer_id: [u8; 16],
    expires_at: Instant,
}

#[derive(Clone)]
struct DestinationBinding {
    peer_id: u64,
    channel: String,
    grant: PathBuf,
    expires_at: Instant,
}

fn expires(now: Instant) -> Instant {
    now.checked_add(HANDLE_TTL).unwrap_or(now)
}

/// Everything a Nearby snapshot is built from, and the handles issued to the connections that asked
/// for one.
#[derive(Default)]
struct State {
    targets: HashMap<NearbyTargetHandle, TargetBinding>,
    transfers: HashMap<NearbyTransferHandle, TransferBinding>,
    destinations: HashMap<NearbyDestinationHandle, DestinationBinding>,
    /// Offers this device received, by transfer, waiting for its person.
    offers: BTreeMap<[u8; 16], Offer>,
    /// Objects this device offered, by transfer, waiting for the receiving device's checkpoint.
    offering: BTreeMap<[u8; 16], Offered>,
    /// Where each accepted receive installs what it receives.
    installing: BTreeMap<[u8; 16], SecureNearbyDestination>,
    progress: BTreeMap<[u8; 16], Stream>,
    history: VecDeque<Settled>,
    /// Attempts to a receiver that is not one of the person's devices, by transfer.
    airdrops: BTreeMap<[u8; 16], AirDrop>,
    last_offer: BTreeMap<[u8; 16], Instant>,
    /// The folder each connection chose to remember for this session.
    remembered: BTreeMap<String, PathBuf>,
    queues: Queues,
    /// The digest of the view the last pass announced, so a change is said once and an idle desktop
    /// wakes nobody.
    observed: Option<u64>,
}

impl State {
    fn record(&mut self, entry: Settled) {
        self.history
            .retain(|existing| existing.transfer_id != entry.transfer_id);
        self.history.push_front(entry);
        self.history.truncate(MAX_HISTORY);
    }

    fn remember(&mut self, channel: &str, grant: Option<PathBuf>) {
        match grant {
            Some(grant) => {
                self.remembered.insert(channel.to_owned(), grant);
            }
            None => {
                self.remembered.remove(channel);
            }
        }
    }

    fn prune(&mut self, now: Instant) {
        self.targets.retain(|_, binding| binding.expires_at > now);
        self.transfers.retain(|_, binding| binding.expires_at > now);
        self.destinations
            .retain(|_, binding| binding.expires_at > now);
        self.airdrops.retain(|_, job| {
            !over(job.phase) || now.saturating_duration_since(job.created) < AIRDROP_FORGET
        });
    }

    fn bind_target(
        &mut self,
        peer_id: u64,
        channel: &str,
        target: &DeviceId,
        now: Instant,
    ) -> NearbyTargetHandle {
        if let Some((handle, binding)) = self.targets.iter_mut().find(|(_, binding)| {
            binding.peer_id == peer_id && binding.channel == channel && &binding.target == target
        }) {
            binding.expires_at = expires(now);
            return *handle;
        }
        let handle =
            NearbyTargetHandle::from_issued_bytes(crate::platform::fabric::nonzero::<16>());
        self.targets.insert(
            handle,
            TargetBinding {
                peer_id,
                channel: channel.to_owned(),
                target: target.clone(),
                expires_at: expires(now),
            },
        );
        handle
    }

    fn bind_destination(
        &mut self,
        peer_id: u64,
        channel: &str,
        grant: &Path,
        now: Instant,
    ) -> NearbyDestinationHandle {
        if let Some((handle, binding)) = self.destinations.iter_mut().find(|(_, binding)| {
            binding.peer_id == peer_id && binding.channel == channel && binding.grant == grant
        }) {
            binding.expires_at = expires(now);
            return *handle;
        }
        let handle =
            NearbyDestinationHandle::from_issued_bytes(crate::platform::fabric::nonzero::<16>());
        self.destinations.insert(
            handle,
            DestinationBinding {
                peer_id,
                channel: channel.to_owned(),
                grant: grant.to_path_buf(),
                expires_at: expires(now),
            },
        );
        handle
    }

    fn bind_transfer(
        &mut self,
        peer_id: u64,
        channel: &str,
        transfer_id: [u8; 16],
        now: Instant,
    ) -> NearbyTransferHandle {
        if let Some((handle, binding)) = self.transfers.iter_mut().find(|(_, binding)| {
            binding.peer_id == peer_id
                && binding.channel == channel
                && binding.transfer_id == transfer_id
        }) {
            binding.expires_at = expires(now);
            return *handle;
        }
        if self.transfers.len() >= MAX_TRANSFER_HANDLES
            && let Some(oldest) = self
                .transfers
                .iter()
                .min_by_key(|(_, binding)| binding.expires_at)
                .map(|(handle, _)| *handle)
        {
            self.transfers.remove(&oldest);
        }
        let handle =
            NearbyTransferHandle::from_issued_bytes(crate::platform::fabric::nonzero::<16>());
        self.transfers.insert(
            handle,
            TransferBinding {
                peer_id,
                channel: channel.to_owned(),
                transfer_id,
                expires_at: expires(now),
            },
        );
        handle
    }
}

/// This session's Nearby Send: what it may offer, what it holds, and the thread that owns the
/// spool.
pub struct NearbyTransfer {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for NearbyTransfer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NearbyTransfer")
            .field("spool", &self.inner.spool)
            .field("devices", &self.devices().len())
            .finish_non_exhaustive()
    }
}

struct Inner {
    /// The fabric this session runs on; `None` on a session that is no certified device of the
    /// person's, which reaches none of their others.
    link: Option<SharedLink>,
    continuity: Continuity,
    /// Where staged objects live: `None` on a session that keeps no state,
    /// where a send is refused rather than half-staged.
    spool: Option<PathBuf>,
    /// What the session's loop is told to turn, so a hand-over that finished on the worker reaches
    /// the person on the next frame.
    wake: Wake,
    jobs: Mutex<Sender<Job>>,
    /// Where a workspace's grants are read: what Files there may read, and where a received file
    /// may land.
    authority: Mutex<Option<Arc<dyn WorkspaceAuthority>>>,
    state: Mutex<State>,
    /// The revision Files' waits follow.
    changes: tokio::sync::watch::Sender<u64>,
    /// Whether this session offers the `AirDrop` path at all: a development build that asked for it,
    /// on a machine with its helper.
    air_drop: bool,
    /// Whether an attempt is running now.
    air_drop_active: Arc<AtomicBool>,
}

/// One piece of work for the thread that owns the spool.
enum Job {
    /// Stage a file and offer it to one of the person's devices.
    Stage {
        transfer_id: [u8; 16],
        target: DeviceId,
        source: SecureNearbySource,
        /// Whether the bytes start at once, or wait for the receiving device's checkpoint: a
        /// screenshot the person already picked a device for streams now, a file whose receiver
        /// must still choose a folder waits for that choice.
        stream_now: bool,
    },
    /// A studio send: open the screenshot, stage it, offer it, and answer.
    Studio {
        target: DeviceId,
        label: String,
        path: PathBuf,
        reply: StudioReply,
    },
    /// Stream the staged object the receiving device's checkpoint names.
    Share {
        source: DeviceId,
        transfer_id: [u8; 16],
        request_id: [u8; 16],
        committed_offset: u64,
        prefix_hash: [u8; 32],
    },
    /// Offer a staged object again, at this device's own initiative.
    Reoffer { transfer_id: [u8; 16] },
    /// Accept an offer this device's person approved, into `destination` .
    Accept {
        transfer_id: [u8; 16],
        manifest: IncomingManifest,
        destination: SecureNearbyDestination,
    },
    /// Drop a receive: declined here, or withdrawn by its sender.
    CancelIncoming { transfer_id: [u8; 16] },
    /// Append bytes of an accepted receive.
    Append {
        source: DeviceId,
        transfer_id: [u8; 16],
        offset: u64,
        bytes: Vec<u8>,
    },
    /// Verify and install a completed receive.
    Finish {
        source: DeviceId,
        transfer_id: [u8; 16],
        request_id: [u8; 16],
    },
    /// Truncate a receive whose prefix the sending device does not hold.
    Restart {
        source: DeviceId,
        transfer_id: [u8; 16],
        request_id: [u8; 16],
    },
    /// This device's person cancelled a send.
    Cancelled { transfer_id: [u8; 16] },
    /// The receiving device said how the transfer ended.
    Settled {
        source: DeviceId,
        transfer_id: [u8; 16],
        accepted: bool,
    },
}

impl NearbyTransfer {
    /// The engine of a session on `link`, staging into `spool` and obeying
    /// `continuity`'s file-transfer switch, with `wake` turning the session's
    /// loop when a hand-over finishes.
    ///
    /// # Errors
    ///
    /// The thread that owns the spool could not start.
    pub fn open(
        link: Option<SharedLink>,
        spool: Option<PathBuf>,
        continuity: Continuity,
        wake: Wake,
    ) -> Result<Arc<Self>, String> {
        Self::open_with(link, spool, continuity, wake, air_drop_lab())
    }

    /// An engine whose `AirDrop` path is offered or not, whatever this host's own switch says: a
    /// test's own choice, so it never depends on what another test set.
    #[cfg(test)]
    fn open_with_air_drop(
        link: Option<SharedLink>,
        spool: Option<PathBuf>,
        continuity: Continuity,
        wake: Wake,
        air_drop: bool,
    ) -> Result<Arc<Self>, String> {
        Self::open_with(link, spool, continuity, wake, air_drop)
    }

    fn open_with(
        link: Option<SharedLink>,
        spool: Option<PathBuf>,
        continuity: Continuity,
        wake: Wake,
        air_drop: bool,
    ) -> Result<Arc<Self>, String> {
        tracing::info!(
            fabric = link.is_some(),
            staging = spool.is_some(),
            air_drop,
            "Nearby Send runs on this session"
        );
        let (jobs, mailbox) = channel();
        let inner = Arc::new(Inner {
            link,
            continuity,
            spool,
            wake,
            jobs: Mutex::new(jobs),
            authority: Mutex::new(None),
            state: Mutex::new(State::default()),
            changes: tokio::sync::watch::Sender::new(1),
            air_drop,
            air_drop_active: Arc::default(),
        });
        let working = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("tilo-nearby-transfer".into())
            .spawn(move || work(&working, mailbox))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(Self { inner }))
    }

    /// The workspaces' grants are read from the session `authority` decides against.
    pub(crate) fn attach_authority(&self, authority: Arc<dyn WorkspaceAuthority>) {
        *self.inner.authority.lock_recover() = Some(authority);
    }

    /// Whether this session reaches the person's other devices at all.
    #[must_use]
    pub(crate) fn has_link(&self) -> bool {
        self.inner.link.is_some()
    }

    /// The person's other devices that are here now, as a picker lists them.
    #[must_use]
    pub(crate) fn devices(&self) -> Vec<ScreenshotSendDevice> {
        self.inner.link.as_ref().map_or_else(Vec::new, devices)
    }

    /// Send the file at `path` to `target` for the capture studio, answering `reply` once it is on
    /// its way or refused. Returns the refusal it can make here, or `None` when the worker took it.
    pub(crate) fn studio_send(
        &self,
        target: DeviceId,
        label: String,
        path: PathBuf,
        reply: StudioReply,
    ) -> Option<Sent> {
        if !self.inner.continuity.file_transfer_permitted() {
            return Some(Sent::refused(label, Refusal::SyncOff));
        }
        let refused = label.clone();
        let job = Job::Studio {
            target,
            label,
            path,
            reply,
        };
        if self.inner.send(job) {
            None
        } else {
            Some(Sent::refused(
                refused,
                Refusal::Refused(NO_WORKER.to_owned()),
            ))
        }
    }

    /// One of the person's devices addressed this one: what it says of the transfer it offers,
    /// accepts, withdraws or finished.
    pub(crate) fn action(
        &self,
        source: &DeviceId,
        request_id: [u8; 16],
        action: &PersonalFabricAction,
    ) {
        use PersonalFabricAction as Action;
        match action {
            Action::NearbyTransferOffer {
                transfer_id,
                name,
                kind,
                total_bytes,
                content_hash,
            } => {
                self.inner.offered(
                    source,
                    request_id,
                    *transfer_id,
                    name,
                    *kind,
                    *total_bytes,
                    *content_hash,
                );
            }
            Action::NearbyTransferReady {
                request_id,
                transfer_id,
                committed_offset,
                prefix_hash,
            } => {
                self.inner.send(Job::Share {
                    source: source.clone(),
                    transfer_id: *transfer_id,
                    request_id: *request_id,
                    committed_offset: *committed_offset,
                    prefix_hash: *prefix_hash,
                });
            }
            Action::NearbyTransferRestart {
                request_id,
                transfer_id,
            } => {
                self.inner.send(Job::Restart {
                    source: source.clone(),
                    transfer_id: *transfer_id,
                    request_id: *request_id,
                });
            }
            Action::NearbyTransferCancel { transfer_id, .. } => {
                self.inner.send(Job::CancelIncoming {
                    transfer_id: *transfer_id,
                });
            }
            Action::NearbyTransferResult {
                transfer_id,
                accepted,
                ..
            } => {
                self.inner.send(Job::Settled {
                    source: source.clone(),
                    transfer_id: *transfer_id,
                    accepted: *accepted,
                });
            }
            _ => {}
        }
    }

    /// Bytes of a transfer arrived on this device's fabric.
    pub(crate) fn frame(&self, source: &DeviceId, frame: &PersonalNearbyTransferFrame) {
        if !self.inner.continuity.file_transfer_permitted() {
            return;
        }
        match frame {
            PersonalNearbyTransferFrame::Chunk {
                transfer_id,
                offset,
                bytes,
                ..
            } => {
                if bytes.is_empty() || bytes.len() > PERSONAL_TRANSFER_CHUNK_BYTES {
                    return;
                }
                {
                    self.inner.send(Job::Append {
                        source: source.clone(),
                        transfer_id: *transfer_id,
                        offset: *offset,
                        bytes: bytes.clone(),
                    });
                }
            }
            PersonalNearbyTransferFrame::Finish {
                transfer_id,
                request_id,
                ..
            } => {
                self.inner.send(Job::Finish {
                    source: source.clone(),
                    transfer_id: *transfer_id,
                    request_id: *request_id,
                });
            }
            // A transport abort is resumable: the last durable prefix stays,
            // and the sending device asks where it may start again.
            PersonalNearbyTransferFrame::Abort { .. } => {}
        }
    }

    /// Where one of this device's outgoing streams stands.
    pub(crate) fn progress(
        &self,
        transfer_id: [u8; 16],
        sent_bytes: u64,
        total_bytes: u64,
        status: PersonalTransferStatus,
    ) {
        {
            let mut state = self.inner.state.lock_recover();
            state.progress.insert(
                transfer_id,
                Stream {
                    sent_bytes,
                    total_bytes,
                    status,
                },
            );
            if matches!(status, PersonalTransferStatus::Failed) {
                state.offering.remove(&transfer_id);
            }
        }
        self.inner.changed();
        if matches!(
            status,
            PersonalTransferStatus::Completed | PersonalTransferStatus::Failed
        ) {
            (self.inner.wake)();
        }
    }

    /// A device of the person's came or went: what Files may offer moved.
    pub(crate) fn devices_changed(&self) {
        self.inner.changed();
    }

    /// One pass of the session's loop: withdraw offers nobody decided, offer again what no device
    /// acknowledged, forget the `AirDrop` attempts that ended, and say what moved.
    pub(crate) fn pump(&self) {
        if self.inner.continuity.file_transfer_permitted() {
            self.inner.expire_offers();
            self.inner.offer_again();
        }
        self.inner.state.lock_recover().prune(Instant::now());
        self.inner.observe();
    }

    /// The attempt that waits for this person now, if one does: what the portal sheet asks them to
    /// verify.
    pub(crate) fn air_drop_ask(&self) -> Option<AirDropAsk> {
        let state = self.inner.state.lock_recover();
        state
            .airdrops
            .iter()
            .filter(|(_, job)| {
                job.phase == NearbyTransferPhase::AwaitingConsent
                    && job.certificate.is_some()
                    && job.answer.is_none()
            })
            .map(|(id, job)| air_drop_ask(*id, job))
            .next()
    }

    /// The attempt `id` as it asks its person now, once it is ready to.
    pub(crate) fn air_drop_ask_of(&self, id: [u8; 16]) -> Option<AirDropAsk> {
        let state = self.inner.state.lock_recover();
        state
            .airdrops
            .get(&id)
            .filter(|job| {
                job.phase == NearbyTransferPhase::AwaitingConsent && job.certificate.is_some()
            })
            .map(|job| air_drop_ask(id, job))
    }

    /// Answer the attempt `id` with what the person did. A confirmation whose fingerprint differs
    /// from the receiver's is refused, and the attempt waits for them to try again.
    ///
    /// # Errors
    ///
    /// Nothing of the attempt is waiting for an answer, or the fingerprint is not the receiver's.
    pub(crate) fn answer_air_drop(
        &self,
        id: [u8; 16],
        answer: AirDropAnswer,
    ) -> Result<(), String> {
        let mut state = self.inner.state.lock_recover();
        let Some(job) = state.airdrops.get_mut(&id) else {
            return Err(crate::i18n::t("airdrop-error-fresh-consent"));
        };
        if job.phase != NearbyTransferPhase::AwaitingConsent {
            return Err(crate::i18n::t("airdrop-error-fresh-consent"));
        }
        let Some(certificate) = job.certificate else {
            return Err(crate::i18n::t("airdrop-error-unavailable"));
        };
        if let AirDropAnswer::Confirm { pin } = &answer
            && pin_bytes(pin) != Some(certificate)
        {
            return Err(crate::i18n::t("airdrop-error-verification"));
        }
        // The question is answered: it never stands again for this attempt, whichever way it
        // goes, and the attempt's thread takes the answer from what it finds here.
        job.phase = match answer {
            AirDropAnswer::Confirm { .. } => NearbyTransferPhase::Transferring,
            AirDropAnswer::Decline => {
                job.cancel.store(true, Ordering::Release);
                NearbyTransferPhase::Cancelled
            }
        };
        job.answer = Some(answer);
        drop(state);
        self.inner.changed();
        (self.inner.wake)();
        Ok(())
    }

    /// The files under `workspace` that Files may write into, widest grant first: where a received
    /// file can land.
    fn destinations(&self, workspace: &str) -> Vec<(PathBuf, String)> {
        destinations(self.inner.authority.lock_recover().as_ref(), workspace)
    }

    /// What `context` 's connection may see now, with a handle for each thing it may name.
    fn snapshot(&self, context: &NearbyTransferRequestContext) -> NearbyTransferSnapshot {
        let now = Instant::now();
        let peer_id = context.peer_id();
        let channel = context.channel().to_owned();
        let permitted = self.inner.continuity.file_transfer_permitted();
        let listed = if permitted {
            self.devices()
        } else {
            Vec::new()
        };
        let folders = if permitted {
            self.destinations(context.workspace_id())
        } else {
            Vec::new()
        };
        let labels: HashMap<DeviceId, String> = self
            .inner
            .link
            .as_ref()
            .map(|link| {
                link.devices()
                    .devices()
                    .iter()
                    .map(|(id, device)| (id.clone(), device.about.label.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut snapshot = NearbyTransferSnapshot::default();
        let mut state = self.inner.state.lock_recover();
        state.prune(now);
        for device in listed.into_iter().take(MAX_NEARBY_TRANSFER_TARGETS) {
            let Ok(id) = DeviceId::new(&device.id) else {
                continue;
            };
            let handle = state.bind_target(peer_id, &channel, &id, now);
            snapshot.targets.push(NearbyTransferTarget {
                handle,
                label: clamp(&device.label, MAX_NEARBY_TRANSFER_LABEL_BYTES),
                online: true,
            });
        }
        let remembered = state.remembered.get(&channel).cloned();
        let mut shown: Vec<(PathBuf, String)> = folders;
        shown.sort_by_key(|(path, _)| usize::from(remembered.as_deref() != Some(path.as_path())));
        for (grant, label) in shown.into_iter().take(MAX_NEARBY_TRANSFER_DESTINATIONS) {
            let remembered_for_session = remembered.as_deref() == Some(grant.as_path());
            let handle = state.bind_destination(peer_id, &channel, &grant, now);
            snapshot.destinations.push(NearbyTransferDestination {
                handle,
                label: clamp(&label, MAX_NEARBY_TRANSFER_LABEL_BYTES),
                remembered_for_session,
            });
        }
        for row in rows(&state) {
            if snapshot.transfers.len() >= MAX_NEARBY_TRANSFER_STATUSES {
                break;
            }
            let handle = state.bind_transfer(peer_id, &channel, row.transfer_id, now);
            let device_label = row
                .label
                .clone()
                .or_else(|| row.peer.as_ref().and_then(|peer| labels.get(peer).cloned()))
                .unwrap_or_else(|| "nearby device".to_owned());
            snapshot.transfers.push(NearbyTransferStatus {
                handle,
                name: clamp(&row.name, MAX_NEARBY_TRANSFER_NAME_BYTES),
                device_label: clamp(&device_label, MAX_NEARBY_TRANSFER_LABEL_BYTES),
                direction: row.direction,
                phase: row.phase,
                transferred_bytes: row.transferred_bytes,
                total_bytes: row.total_bytes,
            });
        }
        drop(state);
        snapshot
    }

    /// Offer the file `grant` -relative `path` names to `target` , as one more Files send: the
    /// handle Files holds names it before it is staged.
    fn send_file(
        &self,
        context: &NearbyTransferRequestContext,
        target: NearbyTargetHandle,
        path: &[u8],
    ) -> Result<NearbyTransferReply, String> {
        let device = {
            let state = self.inner.state.lock_recover();
            let Some(binding) = state.targets.get(&target) else {
                return Err("that device is no longer open to a send".to_owned());
            };
            if binding.peer_id != context.peer_id() || binding.channel != context.channel() {
                return Err(NOT_YOURS.to_owned());
            }
            let device = binding.target.clone();
            drop(state);
            device
        };
        let requested = PathBuf::from(std::ffi::OsStr::from_bytes(path));
        let source =
            source_from_grants(&requested, &self.inner.files_grants(context.workspace_id()))?;
        let bytes = source.metadata()?.len();
        if bytes == 0 {
            return Err("an empty file is not one Nearby Send carries".to_owned());
        }
        if bytes > MAX_TRANSFER_BYTES {
            return Err("that file is larger than one Nearby Send carries".to_owned());
        }
        let transfer_id = crate::platform::fabric::nonzero::<16>();
        let handle = self
            .inner
            .issue_transfer(context.peer_id(), context.channel(), transfer_id);
        self.inner.send(Job::Stage {
            transfer_id,
            target: device,
            source,
            stream_now: false,
        });
        Ok(NearbyTransferReply::Queued { handle })
    }

    /// Hand the file `path` names to the receiver at `address`:`port`, a device that is not one of
    /// the person's: the bytes are staged here, the receiver's leaf certificate is shown to the
    /// person for them to verify ([`Self::answer_air_drop`]), and only their answer sends anything.
    fn send_air_drop(
        &self,
        context: &NearbyTransferRequestContext,
        path: &[u8],
        address: &str,
        port: u16,
    ) -> Result<NearbyTransferReply, String> {
        if !self.inner.air_drop_available() {
            return Err(crate::i18n::t("airdrop-error-unavailable"));
        }
        let requested = PathBuf::from(std::ffi::OsStr::from_bytes(path));
        let source =
            source_from_grants(&requested, &self.inner.files_grants(context.workspace_id()))?;
        let metadata = source.metadata()?;
        if !metadata.is_file() || metadata.len() > AIRDROP_MAX_BYTES {
            return Err(crate::i18n::t("airdrop-error-source"));
        }
        let permit = ActiveAirDrop::reserve(&self.inner.air_drop_active)?;
        let id = crate::platform::fabric::nonzero::<16>();
        let cancel = Arc::new(AtomicBool::new(false));
        let name = source.name().to_owned();
        let job = AirDrop {
            peer_id: context.peer_id(),
            workspace: context.workspace_id().to_owned(),
            address: address.to_owned(),
            port,
            source_path: path.to_vec(),
            name,
            endpoint: format!("AirDrop · {address}:{port}"),
            bytes: metadata.len(),
            phase: NearbyTransferPhase::AwaitingConsent,
            created: Instant::now(),
            cancel: Arc::clone(&cancel),
            certificate: None,
            answer: None,
        };
        {
            let mut state = self.inner.state.lock_recover();
            state.prune(Instant::now());
            if state
                .airdrops
                .values()
                .filter(|job| !over(job.phase))
                .count()
                >= MAX_AIRDROP_JOBS
            {
                return Err(crate::i18n::t("airdrop-error-busy"));
            }
            state.airdrops.insert(id, job);
        }
        let handle = self
            .inner
            .issue_transfer(context.peer_id(), context.channel(), id);
        self.inner.changed();
        (self.inner.wake)();
        let inner = Arc::clone(&self.inner);
        let running = std::thread::Builder::new()
            .name("tilo-airdrop".into())
            .spawn(move || {
                let _permit = permit;
                let outcome = air_drop(&inner, id, source);
                if let Err(reason) = &outcome {
                    tracing::warn!(%reason, "an AirDrop attempt is not sent");
                }
                inner.air_drop_finished(id, &cancel, outcome.is_ok());
            });
        if let Err(error) = running {
            tracing::warn!(%error, "no thread takes an AirDrop attempt");
            self.inner.state.lock_recover().airdrops.remove(&id);
            return Err(crate::i18n::t("airdrop-error-unavailable"));
        }
        Ok(NearbyTransferReply::Queued { handle })
    }

    fn decide(
        &self,
        context: &NearbyTransferRequestContext,
        handle: NearbyTransferHandle,
        accept: bool,
        destination: Option<NearbyDestinationHandle>,
        remember_for_session: bool,
    ) -> Result<NearbyTransferReply, String> {
        let transfer_id = self.bindable(context, handle)?;
        let offer = {
            let state = self.inner.state.lock_recover();
            state.offers.get(&transfer_id).cloned()
        };
        let Some(offer) = offer else {
            return Err("that offer is no longer waiting for you".to_owned());
        };
        if !accept {
            self.inner.send(Job::CancelIncoming { transfer_id });
            if remember_for_session {
                self.inner
                    .state
                    .lock_recover()
                    .remember(context.channel(), None);
            }
            return Ok(NearbyTransferReply::Accepted);
        }
        let Some(destination) = destination else {
            return Err(NO_DESTINATION.to_owned());
        };
        let grant = {
            let state = self.inner.state.lock_recover();
            let Some(binding) = state.destinations.get(&destination) else {
                return Err(NO_DESTINATION.to_owned());
            };
            if binding.peer_id != context.peer_id() || binding.channel != context.channel() {
                return Err(NOT_YOURS.to_owned());
            }
            let grant = binding.grant.clone();
            drop(state);
            grant
        };
        let secured = SecureNearbyDestination::open_beneath(&grant, &grant)?;
        {
            let mut state = self.inner.state.lock_recover();
            state.remember(
                context.channel(),
                remember_for_session.then(|| grant.clone()),
            );
        }
        let mut manifest = offer.manifest();
        manifest.id = transfer_id;
        self.inner.send(Job::Accept {
            transfer_id,
            manifest,
            destination: secured,
        });
        Ok(NearbyTransferReply::Accepted)
    }

    fn cancel(
        &self,
        context: &NearbyTransferRequestContext,
        handle: NearbyTransferHandle,
    ) -> Result<NearbyTransferReply, String> {
        let transfer_id = self.bindable(context, handle)?;
        let (receiving, air_drop) = {
            let state = self.inner.state.lock_recover();
            (
                state.offers.contains_key(&transfer_id)
                    || state
                        .queues
                        .inbox
                        .iter()
                        .any(|entry| entry.id == transfer_id),
                state.airdrops.get(&transfer_id).cloned(),
            )
        };
        if let Some(job) = air_drop {
            // The attempt's own thread hears of it at its next check, and Files' list says so.
            job.cancel.store(true, Ordering::Release);
            (self.inner.wake)();
        } else if receiving {
            self.inner.send(Job::CancelIncoming { transfer_id });
        } else {
            self.inner.send(Job::Cancelled { transfer_id });
        }
        Ok(NearbyTransferReply::Accepted)
    }

    fn retry(
        &self,
        context: &NearbyTransferRequestContext,
        handle: NearbyTransferHandle,
    ) -> Result<NearbyTransferReply, String> {
        let transfer_id = self.bindable(context, handle)?;
        let staged = {
            let state = self.inner.state.lock_recover();
            state
                .queues
                .outbox
                .iter()
                .any(|entry| entry.id == transfer_id)
        };
        if !staged {
            return Err("that transfer has nothing left to offer".to_owned());
        }
        {
            let mut state = self.inner.state.lock_recover();
            state.offering.remove(&transfer_id);
            state.last_offer.remove(&transfer_id);
            state.progress.remove(&transfer_id);
        }
        self.inner.send(Job::Reoffer { transfer_id });
        Ok(NearbyTransferReply::Accepted)
    }

    /// The transfer `handle` names, once it is really this connection's.
    fn bindable(
        &self,
        context: &NearbyTransferRequestContext,
        handle: NearbyTransferHandle,
    ) -> Result<[u8; 16], String> {
        let state = self.inner.state.lock_recover();
        let Some(binding) = state.transfers.get(&handle) else {
            return Err("that transfer is no longer here".to_owned());
        };
        if binding.peer_id != context.peer_id() || binding.channel != context.channel() {
            return Err(NOT_YOURS.to_owned());
        }
        let transfer_id = binding.transfer_id;
        drop(state);
        Ok(transfer_id)
    }
}

impl NearbyTransferServices for NearbyTransfer {
    fn command(
        &self,
        context: &NearbyTransferRequestContext,
        command: NearbyTransferCommand,
    ) -> Result<NearbyTransferReply, String> {
        if context.app_id() != FILES_APP_ID {
            return Err("Nearby Send is served to Files only".to_owned());
        }
        match command {
            NearbyTransferCommand::Snapshot => Ok(NearbyTransferReply::Snapshot {
                snapshot: self.snapshot(context),
            }),
            NearbyTransferCommand::Send {
                target,
                source_path,
            } => self.send_file(context, target, &source_path),
            NearbyTransferCommand::Decide {
                handle,
                accept,
                destination,
                remember_for_session,
            } => self.decide(context, handle, accept, destination, remember_for_session),
            NearbyTransferCommand::Cancel { handle } => self.cancel(context, handle),
            NearbyTransferCommand::Retry { handle } => self.retry(context, handle),
            NearbyTransferCommand::AirDropAvailability => {
                Ok(NearbyTransferReply::AirDropAvailability {
                    available: self.inner.air_drop_available(),
                })
            }
            NearbyTransferCommand::SendAirDrop {
                source_path,
                address,
                port,
            } => self.send_air_drop(context, &source_path, &address, port),
        }
    }

    fn disconnect(&self, peer_id: u64) {
        let mut state = self.inner.state.lock_recover();
        state
            .targets
            .retain(|_, binding| binding.peer_id != peer_id);
        state
            .transfers
            .retain(|_, binding| binding.peer_id != peer_id);
        state
            .destinations
            .retain(|_, binding| binding.peer_id != peer_id);
        for job in state.airdrops.values().filter(|job| job.peer_id == peer_id) {
            job.cancel.store(true, Ordering::Release);
        }
        state.airdrops.retain(|_, job| job.peer_id != peer_id);
    }

    fn changed(
        &self,
        _context: &NearbyTransferRequestContext,
        after: u64,
    ) -> Option<NearbyTransferChange> {
        let revision = *self.inner.changes.borrow();
        if revision != after {
            return Some(Box::pin(std::future::ready(revision)));
        }
        let mut changes = self.inner.changes.subscribe();
        Some(Box::pin(async move {
            // Handles are renewed by a snapshot, so a wait that nothing else moves is still woken
            // before the ones its holder has lapse.
            let _ = tokio::time::timeout(HANDLE_REFRESH, changes.changed()).await;
            *changes.borrow_and_update()
        }))
    }
}

/// One transfer as Files' list shows it, from the live view and then the history behind it.
struct Row {
    transfer_id: [u8; 16],
    /// The device on the other end, when it is one of the person's.
    peer: Option<DeviceId>,
    /// What the other end is called, when it is not one of the person's devices: an `AirDrop`
    /// receiver, named by its endpoint.
    label: Option<String>,
    name: String,
    direction: NearbyTransferDirection,
    phase: NearbyTransferPhase,
    transferred_bytes: u64,
    total_bytes: u64,
}

fn rows(state: &State) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (transfer_id, offer) in &state.offers {
        rows.push(Row {
            transfer_id: *transfer_id,
            peer: Some(offer.source.clone()),
            label: None,
            name: offer.name.clone(),
            direction: NearbyTransferDirection::Receiving,
            phase: NearbyTransferPhase::AwaitingConsent,
            transferred_bytes: 0,
            total_bytes: offer.total_bytes,
        });
    }
    for entry in &state.queues.inbox {
        let Ok(peer) = DeviceId::new(&entry.source) else {
            continue;
        };
        rows.push(Row {
            transfer_id: entry.id,
            peer: Some(peer),
            label: None,
            name: entry.name.clone(),
            direction: NearbyTransferDirection::Receiving,
            phase: if entry.committed_offset == 0 {
                NearbyTransferPhase::Queued
            } else {
                NearbyTransferPhase::Transferring
            },
            transferred_bytes: entry.committed_offset,
            total_bytes: entry.total_bytes,
        });
    }
    for entry in &state.queues.outbox {
        let Ok(peer) = DeviceId::new(&entry.target) else {
            continue;
        };
        let stream = state.progress.get(&entry.id);
        rows.push(Row {
            transfer_id: entry.id,
            peer: Some(peer),
            label: None,
            name: entry.name.clone(),
            direction: NearbyTransferDirection::Sending,
            phase: match stream.map(|stream| stream.status) {
                Some(PersonalTransferStatus::Progress | PersonalTransferStatus::Completed) => {
                    NearbyTransferPhase::Transferring
                }
                _ => NearbyTransferPhase::Queued,
            },
            transferred_bytes: stream.map_or(0, |stream| stream.sent_bytes),
            total_bytes: entry.total_bytes,
        });
    }
    for (transfer_id, job) in &state.airdrops {
        rows.push(Row {
            transfer_id: *transfer_id,
            peer: None,
            label: Some(job.endpoint.clone()),
            name: job.name.clone(),
            direction: NearbyTransferDirection::Sending,
            phase: job.phase,
            transferred_bytes: if over(job.phase) { job.bytes } else { 0 },
            total_bytes: job.bytes,
        });
    }
    for entry in &state.history {
        rows.push(Row {
            transfer_id: entry.transfer_id,
            peer: Some(entry.peer.clone()),
            label: None,
            name: entry.name.clone(),
            direction: entry.direction,
            phase: entry.phase,
            transferred_bytes: 0,
            total_bytes: entry.total_bytes,
        });
    }
    let mut seen: Vec<[u8; 16]> = Vec::new();
    rows.retain(|row| {
        if seen.contains(&row.transfer_id) {
            return false;
        }
        seen.push(row.transfer_id);
        true
    });
    rows
}

/// Take every job the session hands over, keeping the spool on this thread.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the worker owns its end of the channel for the life of the thread"
)]
fn work(inner: &Arc<Inner>, jobs: Receiver<Job>) {
    let mut store: Option<NearbyTransferStore> = None;
    while let Ok(job) = jobs.recv() {
        take(inner, &mut store, job);
        let queues = store.as_ref().map_or_else(Queues::default, |store| Queues {
            inbox: store.inbox().to_vec(),
            outbox: store.outbox().to_vec(),
        });
        inner.state.lock_recover().queues = queues;
        inner.changed();
        (inner.wake)();
    }
}

fn take(inner: &Arc<Inner>, store: &mut Option<NearbyTransferStore>, job: Job) {
    match job {
        Job::Studio {
            target,
            label,
            path,
            reply,
        } => reply(studio(inner, store, &target, label, path)),
        Job::Stage {
            transfer_id,
            target,
            source,
            stream_now,
        } => {
            let name = source.name().to_owned();
            let bytes = source.metadata().map_or(0, |metadata| metadata.len());
            if let Err(reason) = stage(inner, store, transfer_id, &target, source, stream_now) {
                tracing::warn!(%reason, "a nearby send is refused");
                settle(
                    inner,
                    transfer_id,
                    &target,
                    name,
                    NearbyTransferDirection::Sending,
                    NearbyTransferPhase::Failed,
                    bytes,
                );
            }
        }
        Job::Share {
            source,
            transfer_id,
            request_id,
            committed_offset,
            prefix_hash,
        } => {
            if let Err(reason) = share(
                inner,
                store,
                &source,
                transfer_id,
                request_id,
                committed_offset,
                prefix_hash,
            ) {
                tracing::warn!(%reason, "a nearby stream is not started");
            }
        }
        Job::Reoffer { transfer_id } => reoffer(inner, store, transfer_id),
        Job::Accept {
            transfer_id,
            manifest,
            destination,
        } => {
            let name = manifest.name.clone();
            let bytes = manifest.total_bytes;
            let source = manifest.source.clone();
            if let Err(reason) = accept(inner, store, transfer_id, manifest, destination) {
                tracing::warn!(%reason, "a nearby receive is not accepted");
                if let Ok(peer) = DeviceId::new(source) {
                    settle(
                        inner,
                        transfer_id,
                        &peer,
                        name,
                        NearbyTransferDirection::Receiving,
                        NearbyTransferPhase::Failed,
                        bytes,
                    );
                }
            }
        }
        Job::CancelIncoming { transfer_id } => cancel_incoming(inner, store, transfer_id),
        Job::Append {
            source,
            transfer_id,
            offset,
            bytes,
        } => append(inner, store, &source, transfer_id, offset, &bytes),
        Job::Finish {
            source,
            transfer_id,
            request_id,
        } => finish(inner, store, &source, transfer_id, request_id),
        Job::Restart {
            source,
            transfer_id,
            request_id,
        } => restart(inner, store, &source, transfer_id, request_id),
        Job::Cancelled { transfer_id } => cancelled(inner, store, transfer_id),
        Job::Settled {
            source,
            transfer_id,
            accepted,
        } => settled(inner, store, &source, transfer_id, accepted),
    }
}

/// Remember how one transfer ended, for Files' list.
fn settle(
    inner: &Inner,
    transfer_id: [u8; 16],
    peer: &DeviceId,
    name: String,
    direction: NearbyTransferDirection,
    phase: NearbyTransferPhase,
    total_bytes: u64,
) {
    inner.state.lock_recover().record(Settled {
        transfer_id,
        peer: peer.clone(),
        name,
        direction,
        phase,
        total_bytes,
    });
    inner.changed();
}

fn open_store<'a>(
    inner: &Inner,
    slot: &'a mut Option<NearbyTransferStore>,
) -> Result<&'a mut NearbyTransferStore, String> {
    if slot.is_none() {
        let root = inner
            .spool
            .clone()
            .ok_or_else(|| "this session keeps no spool to stage a send in".to_owned())?;
        *slot = Some(NearbyTransferStore::open(root, tilo_clock::unix_now_ms())?);
    }
    slot.as_mut()
        .ok_or_else(|| "the transfer spool did not open".to_owned())
}

fn link_of(inner: &Inner) -> Result<&SharedLink, String> {
    inner
        .link
        .as_ref()
        .ok_or_else(|| "this session is no certified device of the person's".to_owned())
}

/// The folders the workspace lets Files reach with `access` , as its sandbox decides it now.
fn files_folders(workspace: &tilo_desktop_model::Workspace, access: Access) -> Vec<PathBuf> {
    let now = tilo_clock::unix_now();
    workspace
        .app_capabilities_at(FILES_APP_ID, now)
        .into_iter()
        .filter_map(|capability| match &capability.scope {
            Scope::FsPath(path) => Some(path),
            _ => None,
        })
        .filter(|path| {
            workspace.sandbox_decide_at(
                FILES_APP_ID,
                &ResourceRef::FsPath((*path).clone()),
                access,
                now,
            ) == CapDecision::Allow
        })
        .map(PathBuf::from)
        .collect()
}

/// Open the file `path` names beneath the Files grant that holds it. Files names files by raw
/// bytes: a name that is not UTF-8 is still one the person may send.
fn source_from_grants(requested: &Path, grants: &[PathBuf]) -> Result<SecureNearbySource, String> {
    let best = grants
        .iter()
        .filter(|path| requested.starts_with(path))
        .max_by_key(|path| path.components().count());
    let Some(grant) = best else {
        return Err(
            "that file is not one of the files this workspace shared with Files".to_owned(),
        );
    };
    SecureNearbySource::open_beneath(grant, requested)
}

/// The folders Files may write into in `workspace` , as paths and the names the person knows them
/// by.
fn destinations(
    authority: Option<&Arc<dyn WorkspaceAuthority>>,
    workspace: &str,
) -> Vec<(PathBuf, String)> {
    let Some(authority) = authority else {
        return Vec::new();
    };
    let mut grants: Vec<PathBuf> = Vec::new();
    let read = authority.with_workspace(workspace, &mut |workspace| {
        let Some(workspace) = workspace else {
            return;
        };
        for path in files_folders(workspace, Access::Write) {
            if !path.is_dir() {
                continue;
            }
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            if !grants.contains(&path) {
                grants.push(path);
            }
        }
    });
    if let Err(error) = read {
        tracing::warn!(%error, "a workspace's files are not read for a received file");
        return Vec::new();
    }
    grants
        .into_iter()
        .map(|path| {
            let label = folder_label(&path);
            (path, label)
        })
        .collect()
}

/// Whether `action` is one the transfer engine carries: the offer, a checkpoint, a withdrawal, a
/// restart or where a transfer ended.
#[must_use]
pub(crate) fn carried_by_engine(action: &PersonalFabricAction) -> bool {
    matches!(
        action,
        PersonalFabricAction::NearbyTransferOffer { .. }
            | PersonalFabricAction::NearbyTransferReady { .. }
            | PersonalFabricAction::NearbyTransferCancel { .. }
            | PersonalFabricAction::NearbyTransferRestart { .. }
            | PersonalFabricAction::NearbyTransferResult { .. }
    )
}

/// The name a grant's folder is offered to the person under.
fn folder_label(path: &Path) -> String {
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    if name.is_empty() {
        "Workspace files".to_owned()
    } else {
        name
    }
}

/// `text` , cut to `limit` bytes on a character boundary.
fn clamp(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let cut = text.get(..end).unwrap_or_default();
    if cut.is_empty() {
        "nearby".to_owned()
    } else {
        cut.to_owned()
    }
}

/// Progress in whole percent: a snapshot moves on with every chunk, and Files hears of it at most a
/// hundred times a transfer.
fn percent(done: u64, total: u64) -> u64 {
    done.min(total)
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
}

/// Stage `source` and offer it to `target` , streaming at once when the receiving device need not
/// choose a folder first.
fn stage(
    inner: &Inner,
    store: &mut Option<NearbyTransferStore>,
    transfer_id: [u8; 16],
    target: &DeviceId,
    source: SecureNearbySource,
    stream_now: bool,
) -> Result<(), String> {
    let link = link_of(inner)?;
    let store = open_store(inner, store)?;
    let staged = store.stage_secure_with_id(
        source,
        target.as_str().to_owned(),
        tilo_clock::unix_now_ms(),
        transfer_id,
    )?;
    let offer = PersonalFabricAction::NearbyTransferOffer {
        transfer_id: staged.id,
        name: staged.name.clone(),
        kind: staged.kind,
        total_bytes: staged.total_bytes,
        content_hash: staged.content_hash,
    };
    // The offer is recorded as out before it leaves, so a checkpoint that answers it at once finds
    // the transfer it speaks of.
    {
        let mut state = inner.state.lock_recover();
        state.offering.insert(
            staged.id,
            Offered {
                target: target.clone(),
                request_id: [0; 16],
            },
        );
        state.last_offer.insert(staged.id, Instant::now());
    }
    let request_id = match link.try_send_personal_fabric_action(target, offer) {
        Ok(request_id) => request_id,
        Err(error) => {
            inner.state.lock_recover().offering.remove(&staged.id);
            let _ = store.cancel_outgoing(staged.id);
            return Err(error.to_string());
        }
    };
    inner.state.lock_recover().offering.insert(
        staged.id,
        Offered {
            target: target.clone(),
            request_id,
        },
    );
    if stream_now {
        stream(
            inner,
            store,
            target,
            staged.id,
            request_id,
            0,
            *blake3::hash(&[]).as_bytes(),
        )?;
    }
    Ok(())
}

/// Stream the staged object `transfer_id` to `target` from the checkpoint it says it holds.
fn stream(
    inner: &Inner,
    store: &NearbyTransferStore,
    target: &DeviceId,
    transfer_id: [u8; 16],
    request_id: [u8; 16],
    committed_offset: u64,
    prefix_hash: [u8; 32],
) -> Result<(), String> {
    let link = link_of(inner)?;
    let Some(entry) = store.outbox().iter().find(|entry| entry.id == transfer_id) else {
        return Err("that transfer is no longer staged".to_owned());
    };
    if entry.target != target.as_str() {
        return Err("that transfer was staged for another device".to_owned());
    }
    if committed_offset > entry.total_bytes {
        return Err("the receiving device holds more than the transfer carries".to_owned());
    }
    let file =
        std::fs::File::open(store.object_path(transfer_id)).map_err(|error| error.to_string())?;
    let checkpoint = NearbyTransferCheckpoint {
        transfer_id,
        request_id,
        total_bytes: entry.total_bytes,
        resume_offset: committed_offset,
        prefix_hash,
        content_hash: entry.content_hash,
    };
    link.share_nearby_spool(target, NearbySpoolOffer { checkpoint, file })
        .map_err(|error| error.to_string())?;
    inner.state.lock_recover().offering.insert(
        transfer_id,
        Offered {
            target: target.clone(),
            request_id,
        },
    );
    Ok(())
}

/// Stream what the receiving device's checkpoint names, once the offer really was made to it.
fn share(
    inner: &Inner,
    store: &mut Option<NearbyTransferStore>,
    source: &DeviceId,
    transfer_id: [u8; 16],
    request_id: [u8; 16],
    committed_offset: u64,
    prefix_hash: [u8; 32],
) -> Result<(), String> {
    let offered = {
        let state = inner.state.lock_recover();
        state
            .offering
            .get(&transfer_id)
            .filter(|offered| &offered.target == source)
            .cloned()
    };
    if offered.is_none() {
        return Err("that transfer was not offered to this device".to_owned());
    }
    let store = open_store(inner, store)?;
    stream(
        inner,
        store,
        source,
        transfer_id,
        request_id,
        committed_offset,
        prefix_hash,
    )
}

/// Offer a staged object again to the device it was staged for.
fn reoffer(inner: &Inner, store: &mut Option<NearbyTransferStore>, transfer_id: [u8; 16]) {
    let Ok(link) = link_of(inner) else {
        return;
    };
    let Ok(store) = open_store(inner, store) else {
        return;
    };
    let Some(entry) = store
        .outbox()
        .iter()
        .find(|entry| entry.id == transfer_id)
        .cloned()
    else {
        return;
    };
    let Ok(target) = DeviceId::new(entry.target.clone()) else {
        return;
    };
    let offer = PersonalFabricAction::NearbyTransferOffer {
        transfer_id: entry.id,
        name: entry.name,
        kind: entry.kind,
        total_bytes: entry.total_bytes,
        content_hash: entry.content_hash,
    };
    {
        let mut state = inner.state.lock_recover();
        state.last_offer.insert(transfer_id, Instant::now());
        state.offering.insert(
            transfer_id,
            Offered {
                target: target.clone(),
                request_id: [0; 16],
            },
        );
    }
    match link.try_send_personal_fabric_action(&target, offer) {
        Ok(request_id) => {
            inner
                .state
                .lock_recover()
                .offering
                .insert(transfer_id, Offered { target, request_id });
        }
        Err(error) => {
            tracing::warn!(%error, "a staged send is not offered now");
            inner.state.lock_recover().offering.remove(&transfer_id);
        }
    }
}

fn accept(
    inner: &Inner,
    store: &mut Option<NearbyTransferStore>,
    transfer_id: [u8; 16],
    manifest: IncomingManifest,
    destination: SecureNearbyDestination,
) -> Result<(), String> {
    let link = link_of(inner)?.clone();
    let store = open_store(inner, store)?;
    let source = DeviceId::new(manifest.source.clone()).map_err(|error| error.to_string())?;
    let request_id = manifest.request_id;
    let (committed_offset, prefix_hash) =
        store.accept_incoming_offer(manifest, tilo_clock::unix_now_ms())?;
    let ready = PersonalFabricAction::NearbyTransferReady {
        request_id,
        transfer_id,
        committed_offset,
        prefix_hash,
    };
    link.try_send_personal_fabric_action(&source, ready)
        .map_err(|error| error.to_string())?;
    {
        let mut state = inner.state.lock_recover();
        state.offers.remove(&transfer_id);
        state.installing.insert(transfer_id, destination);
    }
    Ok(())
}

/// Drop a receive nobody took: tell its sending device where it stands, and remove what was kept of
/// it.
fn cancel_incoming(inner: &Inner, store: &mut Option<NearbyTransferStore>, transfer_id: [u8; 16]) {
    let offer = {
        let mut state = inner.state.lock_recover();
        state.installing.remove(&transfer_id);
        state.offers.remove(&transfer_id)
    };
    let Ok(link) = link_of(inner) else {
        return;
    };
    let Ok(store) = open_store(inner, store) else {
        return;
    };
    let entry = store
        .inbox()
        .iter()
        .find(|entry| entry.id == transfer_id)
        .cloned();
    let source = offer
        .as_ref()
        .map(|offer| (offer.source.clone(), offer.request_id))
        .or_else(|| {
            entry.as_ref().and_then(|entry| {
                DeviceId::new(entry.source.clone())
                    .ok()
                    .map(|source| (source, entry.request_id))
            })
        });
    let Some((source, request_id)) = source else {
        return;
    };
    let declined = PersonalFabricAction::NearbyTransferResult {
        request_id,
        transfer_id,
        accepted: false,
    };
    if let Err(error) = link.try_send_personal_fabric_action(&source, declined) {
        tracing::warn!(%error, "a device was not told a nearby transfer was dropped");
    }
    if entry.is_some()
        && let Err(error) = store.cancel_incoming(transfer_id)
    {
        tracing::warn!(%error, "a received transfer was not removed");
    }
    let name = offer
        .map(|offer| (offer.name, offer.total_bytes))
        .or_else(|| entry.map(|entry| (entry.name, entry.total_bytes)));
    if let Some((name, total_bytes)) = name {
        settle(
            inner,
            transfer_id,
            &source,
            name,
            NearbyTransferDirection::Receiving,
            NearbyTransferPhase::Cancelled,
            total_bytes,
        );
    }
}

fn append(
    inner: &Inner,
    store: &mut Option<NearbyTransferStore>,
    source: &DeviceId,
    transfer_id: [u8; 16],
    offset: u64,
    bytes: &[u8],
) {
    let Ok(store) = open_store(inner, store) else {
        return;
    };
    let accepted = store
        .inbox()
        .iter()
        .any(|entry| entry.id == transfer_id && entry.source == source.as_str());
    if !accepted {
        tracing::debug!(
            source = source.as_str(),
            "bytes of a transfer this device took no part in"
        );
        return;
    }
    if let Err(error) = store.append(transfer_id, offset, bytes) {
        tracing::warn!(%error, "a nearby chunk is not taken");
    }
}

fn finish(
    inner: &Inner,
    store: &mut Option<NearbyTransferStore>,
    source: &DeviceId,
    transfer_id: [u8; 16],
    request_id: [u8; 16],
) {
    let Ok(link) = link_of(inner).cloned() else {
        return;
    };
    let Ok(store) = open_store(inner, store) else {
        return;
    };
    let entry = store
        .inbox()
        .iter()
        .find(|entry| entry.id == transfer_id && entry.source == source.as_str())
        .cloned();
    let Some(entry) = entry else {
        return;
    };
    let destination = inner.state.lock_recover().installing.remove(&transfer_id);
    let Some(destination) = destination else {
        return;
    };
    let result = |accepted: bool| PersonalFabricAction::NearbyTransferResult {
        request_id,
        transfer_id,
        accepted,
    };
    match store.finish_secure(transfer_id, &destination) {
        Ok(installed) => {
            if let Err(error) = link.try_send_personal_fabric_action(source, result(true)) {
                tracing::warn!(%error, "a device was not told the file landed");
            }
            tracing::info!(path = %installed.display(), "a file arrived from one of the person's devices");
            settle(
                inner,
                transfer_id,
                source,
                entry.name,
                NearbyTransferDirection::Receiving,
                NearbyTransferPhase::Completed,
                entry.total_bytes,
            );
        }
        Err(reason) => {
            tracing::warn!(%reason, "a received file is not installed");
            let _ = store.cancel_incoming(transfer_id);
            let _ = link.try_send_personal_fabric_action(source, result(false));
            settle(
                inner,
                transfer_id,
                source,
                entry.name,
                NearbyTransferDirection::Receiving,
                NearbyTransferPhase::Failed,
                entry.total_bytes,
            );
        }
    }
}

fn restart(
    inner: &Inner,
    store: &mut Option<NearbyTransferStore>,
    source: &DeviceId,
    transfer_id: [u8; 16],
    request_id: [u8; 16],
) {
    let Ok(link) = link_of(inner).cloned() else {
        return;
    };
    let Ok(store) = open_store(inner, store) else {
        return;
    };
    match store.restart_incoming(transfer_id) {
        Ok(prefix_hash) => {
            let ready = PersonalFabricAction::NearbyTransferReady {
                request_id,
                transfer_id,
                committed_offset: 0,
                prefix_hash,
            };
            if let Err(error) = link.try_send_personal_fabric_action(source, ready) {
                tracing::warn!(%error, "a device was not told where to stream from");
            }
        }
        Err(reason) => tracing::warn!(%reason, "a received transfer is not restarted"),
    }
}

fn cancelled(inner: &Inner, store: &mut Option<NearbyTransferStore>, transfer_id: [u8; 16]) {
    let Ok(link) = link_of(inner).cloned() else {
        return;
    };
    let Ok(store) = open_store(inner, store) else {
        return;
    };
    let entry = store
        .outbox()
        .iter()
        .find(|entry| entry.id == transfer_id)
        .cloned();
    let offered = inner.state.lock_recover().offering.remove(&transfer_id);
    if let Err(error) = store.cancel_outgoing(transfer_id) {
        tracing::warn!(%error, "a staged send was not removed");
    }
    inner.state.lock_recover().progress.remove(&transfer_id);
    let Some(entry) = entry else {
        return;
    };
    let Ok(target) = DeviceId::new(entry.target.clone()) else {
        return;
    };
    if let Some(offered) = offered {
        let discard = PersonalFabricAction::NearbyTransferCancel {
            request_id: offered.request_id,
            transfer_id,
        };
        if let Err(error) = link.try_send_personal_fabric_action(&target, discard) {
            tracing::warn!(%error, "a device was not told a send was withdrawn");
        }
    }
    settle(
        inner,
        transfer_id,
        &target,
        entry.name,
        NearbyTransferDirection::Sending,
        NearbyTransferPhase::Cancelled,
        entry.total_bytes,
    );
}

fn settled(
    inner: &Inner,
    store: &mut Option<NearbyTransferStore>,
    source: &DeviceId,
    transfer_id: [u8; 16],
    accepted: bool,
) {
    let Ok(store) = open_store(inner, store) else {
        return;
    };
    let entry = store
        .outbox()
        .iter()
        .find(|entry| entry.id == transfer_id)
        .cloned();
    if let Some(entry) = &entry
        && entry.target != source.as_str()
    {
        return;
    }
    // The receiving device either installed the file or refused it: either way this device no
    // longer needs the staged copy.
    if let Err(error) = store.cancel_outgoing(transfer_id) {
        tracing::warn!(%error, "a staged send was not removed");
    }
    {
        let mut state = inner.state.lock_recover();
        state.offering.remove(&transfer_id);
        state.progress.remove(&transfer_id);
    }
    let Some(entry) = entry else {
        return;
    };
    settle(
        inner,
        transfer_id,
        source,
        entry.name,
        NearbyTransferDirection::Sending,
        if accepted {
            NearbyTransferPhase::Completed
        } else {
            NearbyTransferPhase::Failed
        },
        entry.total_bytes,
    );
}

/// Take one studio send: open the screenshot, stage it, offer it, stream it,
/// and answer.
fn studio(
    inner: &Inner,
    store: &mut Option<NearbyTransferStore>,
    target: &DeviceId,
    label: String,
    path: PathBuf,
) -> Sent {
    let Some(folder) = path.parent() else {
        return Sent::refused(label, Refusal::NoFolder { path });
    };
    if !path.is_file() {
        return Sent::refused(label, Refusal::Gone { path });
    }
    let source = match SecureNearbySource::open_beneath(folder, &path) {
        Ok(source) => source,
        Err(error) => return Sent::refused(label, Refusal::Refused(error)),
    };
    let bytes = match source.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => return Sent::refused(label, Refusal::Refused(error)),
    };
    if bytes > MAX_TRANSFER_BYTES {
        return Sent::refused(
            label,
            Refusal::TooLarge {
                bytes,
                limit: MAX_TRANSFER_BYTES,
            },
        );
    }
    let transfer_id = crate::platform::fabric::nonzero::<16>();
    match stage(inner, store, transfer_id, target, source, true) {
        Ok(()) => Sent::offered(label),
        Err(reason) => Sent::refused(label, Refusal::Refused(reason)),
    }
}

impl Inner {
    /// Hand `job` to the thread that owns the spool.
    fn send(&self, job: Job) -> bool {
        let sent = self.jobs.lock_recover().send(job);
        match sent {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "the nearby transfer worker stopped");
                false
            }
        }
    }

    /// Tell every waiting Files connection to ask for a snapshot again.
    fn changed(&self) {
        self.changes.send_modify(|revision| {
            *revision = revision.wrapping_add(1).max(1);
        });
    }

    /// A fresh handle bound to `peer_id` 's connection on `channel` , for one send this session
    /// starts itself.
    fn issue_transfer(
        &self,
        peer_id: u64,
        channel: &str,
        transfer_id: [u8; 16],
    ) -> NearbyTransferHandle {
        self.state
            .lock_recover()
            .bind_transfer(peer_id, channel, transfer_id, Instant::now())
    }

    /// What Files in `workspace` may read: the folders its workspace grants it.
    fn files_grants(&self, workspace: &str) -> Vec<PathBuf> {
        let authority = self.authority.lock_recover().clone();
        let Some(authority) = authority else {
            return Vec::new();
        };
        let mut grants: Vec<PathBuf> = Vec::new();
        let read = authority.with_workspace(workspace, &mut |workspace| {
            if let Some(workspace) = workspace {
                grants = files_folders(workspace, Access::Read);
            }
        });
        if let Err(error) = read {
            tracing::warn!(%error, "a workspace's grants are not read for a send");
            return Vec::new();
        }
        grants
    }

    /// Whether this session hands a file to a receiver that is not one of the person's: the host
    /// offers the path and the person allowed file transfer.
    fn air_drop_available(&self) -> bool {
        self.air_drop && self.continuity.file_transfer_permitted()
    }

    /// Whether the attempt `id` may still run: nobody stopped it, the host still offers the path
    /// and the person allows it, and the file Files named is still the one it named, inside the
    /// workspace's own Files grant.
    fn air_drop_admitted(&self, id: [u8; 16], identity: &std::fs::Metadata) -> bool {
        if !self.air_drop_available() {
            return false;
        }
        let job = self.state.lock_recover().airdrops.get(&id).cloned();
        let Some(job) = job else {
            return false;
        };
        if job.cancel.load(Ordering::Acquire) {
            return false;
        }
        let requested = PathBuf::from(std::ffi::OsStr::from_bytes(&job.source_path));
        source_from_grants(&requested, &self.files_grants(&job.workspace))
            .and_then(|source| source.metadata())
            .is_ok_and(|current| current.dev() == identity.dev() && current.ino() == identity.ino())
    }

    /// Say which way the attempt `id` stands, and turn the session's loop: Files asks for a new
    /// snapshot once a frame.
    fn air_drop_phase(&self, id: [u8; 16], phase: NearbyTransferPhase) {
        if let Some(job) = self.state.lock_recover().airdrops.get_mut(&id) {
            job.phase = phase;
        }
        self.changed();
        (self.wake)();
    }

    /// Withdraw the attempt `id` before anything is sent: Files' list says it was cancelled.
    fn air_drop_abandon(&self, id: [u8; 16]) {
        if let Some(job) = self.state.lock_recover().airdrops.get_mut(&id) {
            job.cancel.store(true, Ordering::Release);
        }
    }

    /// The attempt `id` is over: Files' list keeps it, with how it ended, for a while.
    fn air_drop_finished(&self, id: [u8; 16], cancel: &AtomicBool, sent: bool) {
        let phase = if cancel.load(Ordering::Acquire) {
            NearbyTransferPhase::Cancelled
        } else if sent {
            NearbyTransferPhase::Completed
        } else {
            NearbyTransferPhase::Failed
        };
        self.air_drop_phase(id, phase);
    }

    /// The offer this device received, once its sending device says so.
    fn offered(
        &self,
        source: &DeviceId,
        request_id: [u8; 16],
        transfer_id: [u8; 16],
        name: &str,
        kind: TransferKind,
        total_bytes: u64,
        content_hash: [u8; 32],
    ) {
        let refuse = |why: &str| {
            tracing::debug!(%why, source = source.as_str(), "a nearby offer is refused");
            if let Some(link) = &self.link {
                let _ = link.try_send_personal_fabric_action(
                    source,
                    PersonalFabricAction::NearbyTransferResult {
                        request_id,
                        transfer_id,
                        accepted: false,
                    },
                );
            }
        };
        if !self.continuity.file_transfer_permitted() {
            return refuse("file transfer is turned off on this device");
        }
        // A media handoff is admitted by the action that named it, which this session carries none
        // of: nothing of that kind lands here.
        if kind == TransferKind::MediaHandoff {
            return refuse("this session takes no media handoff");
        }
        if total_bytes == 0 || total_bytes > MAX_TRANSFER_BYTES {
            return refuse("that transfer is not one this device takes");
        }
        let queued = {
            let mut state = self.state.lock_recover();
            if state.offers.contains_key(&transfer_id) || state.offers.len() >= MAX_PENDING_OFFERS {
                false
            } else {
                state.offers.insert(
                    transfer_id,
                    Offer {
                        source: source.clone(),
                        transfer_id,
                        request_id,
                        name: name.to_owned(),
                        kind,
                        total_bytes,
                        content_hash,
                        seen: Instant::now(),
                    },
                );
                true
            }
        };
        if queued {
            self.changed();
            (self.wake)();
        } else {
            refuse("this device has no room for another offer");
        }
    }

    /// Offers nobody decided within [`CONSENT_TIMEOUT`] are withdrawn.
    fn expire_offers(&self) {
        let now = Instant::now();
        let expired: Vec<([u8; 16], Offer)> = {
            let mut state = self.state.lock_recover();
            let due: Vec<[u8; 16]> = state
                .offers
                .iter()
                .filter(|(_, offer)| now.saturating_duration_since(offer.seen) >= CONSENT_TIMEOUT)
                .map(|(transfer_id, _)| *transfer_id)
                .collect();
            let mut expired = Vec::new();
            for id in due {
                if let Some(offer) = state.offers.remove(&id) {
                    expired.push((id, offer));
                }
            }
            drop(state);
            expired
        };
        if expired.is_empty() {
            return;
        }
        for (transfer_id, offer) in expired {
            if let Some(link) = &self.link {
                let _ = link.try_send_personal_fabric_action(
                    &offer.source,
                    PersonalFabricAction::NearbyTransferResult {
                        request_id: offer.request_id,
                        transfer_id,
                        accepted: false,
                    },
                );
            }
            settle(
                self,
                transfer_id,
                &offer.source,
                offer.name,
                NearbyTransferDirection::Receiving,
                NearbyTransferPhase::Cancelled,
                offer.total_bytes,
            );
        }
    }

    /// Offer again every staged object no device acknowledged, and nothing while nothing is due.
    fn offer_again(&self) {
        let now = Instant::now();
        let due: Vec<[u8; 16]> = {
            let state = self.state.lock_recover();
            state
                .queues
                .outbox
                .iter()
                .filter(|entry| entry.kind != TransferKind::MediaHandoff)
                .filter(|entry| !state.offering.contains_key(&entry.id))
                .filter(|entry| {
                    state
                        .last_offer
                        .get(&entry.id)
                        .is_none_or(|last| now.saturating_duration_since(*last) >= OFFER_AGAIN)
                })
                .map(|entry| entry.id)
                .collect()
        };
        for transfer_id in due {
            self.state
                .lock_recover()
                .last_offer
                .insert(transfer_id, now);
            self.send(Job::Reoffer { transfer_id });
        }
    }

    /// Say what moved since the last pass, and nothing while nothing did.
    fn observe(&self) {
        let digest = self.digest();
        let changed = {
            let mut state = self.state.lock_recover();
            let changed = state.observed != Some(digest);
            if changed {
                state.observed = Some(digest);
            }
            changed
        };
        if changed {
            self.changed();
        }
    }

    /// A digest of everything a snapshot is built from. It may change when the snapshot would not
    /// (one extra ask is harmless); it never stays the same when the snapshot would change.
    fn digest(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for device in self.link.as_ref().map_or_else(Vec::new, devices) {
            device.id.hash(&mut hasher);
            device.label.hash(&mut hasher);
        }
        self.continuity.file_transfer_permitted().hash(&mut hasher);
        let state = self.state.lock_recover();
        for entry in &state.queues.inbox {
            (entry.id, &entry.name, &entry.source, entry.total_bytes).hash(&mut hasher);
            percent(entry.committed_offset, entry.total_bytes).hash(&mut hasher);
        }
        0xff_u8.hash(&mut hasher);
        for entry in &state.queues.outbox {
            (entry.id, &entry.name, &entry.target, entry.total_bytes).hash(&mut hasher);
        }
        0xff_u8.hash(&mut hasher);
        for (transfer_id, offer) in &state.offers {
            (transfer_id, &offer.name, &offer.source, offer.total_bytes).hash(&mut hasher);
        }
        0xff_u8.hash(&mut hasher);
        for (transfer_id, stream) in &state.progress {
            (transfer_id, percent(stream.sent_bytes, stream.total_bytes)).hash(&mut hasher);
            std::mem::discriminant(&stream.status).hash(&mut hasher);
        }
        0xff_u8.hash(&mut hasher);
        for entry in &state.history {
            (entry.transfer_id, &entry.peer, &entry.name).hash(&mut hasher);
            std::mem::discriminant(&entry.phase).hash(&mut hasher);
        }
        0xff_u8.hash(&mut hasher);
        for transfer_id in state.installing.keys() {
            transfer_id.hash(&mut hasher);
        }
        0xff_u8.hash(&mut hasher);
        for (channel, grant) in &state.remembered {
            (channel, grant).hash(&mut hasher);
        }
        0xff_u8.hash(&mut hasher);
        for (transfer_id, job) in &state.airdrops {
            (transfer_id, &job.name, &job.endpoint, job.bytes).hash(&mut hasher);
            std::mem::discriminant(&job.phase).hash(&mut hasher);
            job.certificate.is_some().hash(&mut hasher);
        }
        drop(state);
        hasher.finish()
    }
}

/// Whether this host offers the `AirDrop` path: a development build that asked for it, on a machine
/// where the wire helper is installed. A release session hands files to the person's own devices
/// only.
fn air_drop_lab() -> bool {
    cfg!(debug_assertions)
        && std::env::var("TILO_AIRDROP_LAB").as_deref() == Ok("1")
        && Path::new(AIRDROP_HELPER).is_file()
}

/// What the portal sheet shows of the attempt `job` waits for its person.
fn air_drop_ask(id: [u8; 16], job: &AirDrop) -> AirDropAsk {
    AirDropAsk {
        id,
        name: job.name.clone(),
        bytes: job.bytes,
        endpoint: job.endpoint.clone(),
    }
}

/// The 32 bytes a 64-digit hexadecimal fingerprint stands for, once it is one.
fn pin_bytes(value: &str) -> Option<[u8; 32]> {
    let value = value.trim();
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    hex::decode_to_slice(value, &mut bytes).ok()?;
    Some(bytes)
}

/// One attempt at a receiver that is not one of the person's devices: stage the approved file,
/// observe the receiver's leaf certificate, put the verification to the person and wait for their
/// answer, then hand the staged bytes over once and check what the receiver says it took.
fn air_drop(inner: &Arc<Inner>, id: [u8; 16], source: SecureNearbySource) -> Result<(), String> {
    let Some((address, port)) = inner
        .state
        .lock_recover()
        .airdrops
        .get(&id)
        .map(|job| (job.address.clone(), job.port))
    else {
        return Err(crate::i18n::t("airdrop-error-unavailable"));
    };
    let identity = source.metadata()?;
    let admitted = || inner.air_drop_admitted(id, &identity);
    let staged = StagedFile::staged(source, &admitted, AIRDROP_TIMEOUT)?;
    if !admitted() {
        return Err(crate::i18n::t("airdrop-error-unavailable"));
    }
    let observed = air_drop_helper(&address, port, None, None, &admitted)?
        .get("certificate_sha256")
        .and_then(serde_json::Value::as_str)
        .and_then(pin_bytes)
        .ok_or_else(|| crate::i18n::t("airdrop-error-failed"))?;
    if let Some(job) = inner.state.lock_recover().airdrops.get_mut(&id) {
        job.certificate = Some(observed);
    }
    inner.changed();
    (inner.wake)();
    let answer = air_drop_wait(inner, id, &admitted)?;
    match answer {
        AirDropAnswer::Decline => return Err(crate::i18n::t("airdrop-error-declined")),
        AirDropAnswer::Confirm { pin } => {
            if pin_bytes(&pin) != Some(observed) {
                return Err(crate::i18n::t("airdrop-error-verification"));
            }
        }
    }
    if !admitted() {
        return Err(crate::i18n::t("airdrop-error-unavailable"));
    }
    inner.air_drop_phase(id, NearbyTransferPhase::Transferring);
    let reply = air_drop_helper(
        &address,
        port,
        Some(&staged),
        Some(&hex::encode(observed)),
        &admitted,
    )?;
    if reply.get("bytes").and_then(serde_json::Value::as_u64) != Some(staged.bytes)
        || reply
            .get("content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(staged.hash.as_str())
    {
        return Err(crate::i18n::t("airdrop-error-failed"));
    }
    Ok(())
}

/// Wait for the person's answer on the verification of the attempt `id`, for at most
/// [`AIRDROP_CONSENT_TIMEOUT`], while the attempt stays admitted. An attempt that ends before it
/// is answered is withdrawn, not failed: nothing was sent.
fn air_drop_wait(
    inner: &Inner,
    id: [u8; 16],
    admitted: &impl Fn() -> bool,
) -> Result<AirDropAnswer, String> {
    let started = Instant::now();
    loop {
        if !admitted() {
            inner.air_drop_abandon(id);
            return Err(crate::i18n::t("airdrop-error-unavailable"));
        }
        if started.elapsed() >= AIRDROP_CONSENT_TIMEOUT {
            inner.air_drop_abandon(id);
            return Err(crate::i18n::t("airdrop-error-declined"));
        }
        let answer = inner
            .state
            .lock_recover()
            .airdrops
            .get_mut(&id)
            .and_then(|job| job.answer.take());
        if let Some(answer) = answer {
            return Ok(answer);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// One helper run against `address`:`port` : the observation the person verifies, or the one send
/// of the staged bytes that they approved.
fn air_drop_helper(
    address: &str,
    port: u16,
    source: Option<&StagedFile>,
    certificate: Option<&str>,
    admitted: &impl Fn() -> bool,
) -> Result<serde_json::Value, String> {
    let mut command = air_drop_command(AIRDROP_SENDER);
    command.args([address, &port.to_string()]);
    if let (Some(certificate), Some(source)) = (certificate, source) {
        command
            .args([
                "send",
                "--certificate-sha256",
                certificate,
                "--source-fd",
                "9",
            ])
            .arg(format!("--name={}", source.name));
        inherit_descriptors(&mut command, Some(source.file.as_raw_fd()), None);
    } else {
        command.arg("inspect");
        inherit_descriptors(&mut command, None, None);
    }
    air_drop_supervise(command, admitted, AIRDROP_TIMEOUT)
}

fn air_drop_command(script: &str) -> Command {
    let mut command = Command::new(AIRDROP_HELPER);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["-I", "-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Give the helper only the descriptors it needs, each duplicated to the number the script named.
fn inherit_descriptors(command: &mut Command, source: Option<i32>, staging: Option<i32>) {
    // SAFETY: only async-signal-safe syscalls run between fork and exec. All
    // inherited descriptors become CLOEXEC. The fixed helper receives only
    // a read-only source and (during staging only) its anonymous staging fd.
    unsafe {
        command.pre_exec(move || {
            if libc::syscall(
                libc::SYS_close_range,
                3_u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let handed = [(source, 9), (staging, 10)]
        .into_iter()
        .filter_map(|(fd, target)| Some((fd?, target)))
        .collect();
    launcher::inherit::hand_down(command, handed, false);
}

/// Run `command` while `admitted` holds, for at most `limit`, and read back its one JSON answer.
fn air_drop_supervise(
    mut command: Command,
    admitted: &impl Fn() -> bool,
    limit: Duration,
) -> Result<serde_json::Value, String> {
    if !admitted() {
        return Err(crate::i18n::t("airdrop-error-unavailable"));
    }
    let mut child = command
        .spawn()
        .map_err(|_| crate::i18n::t("airdrop-error-failed"))?;
    let started = Instant::now();
    loop {
        let withdrawn = !admitted();
        if withdrawn || started.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            // A helper that ran out of time failed; only a withdrawn
            // admission is the session, permission or Files access.
            return Err(crate::i18n::t(if withdrawn {
                "airdrop-error-unavailable"
            } else {
                "airdrop-error-failed"
            }));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(crate::i18n::t("airdrop-error-failed"));
                }
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(crate::i18n::t("airdrop-error-failed"));
            }
        }
    }
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| crate::i18n::t("airdrop-error-failed"))?
        .take(AIRDROP_REPLY_BYTES + 1)
        .read_to_end(&mut output)
        .map_err(|_| crate::i18n::t("airdrop-error-failed"))?;
    if u64::try_from(output.len()).unwrap_or(u64::MAX) > AIRDROP_REPLY_BYTES {
        return Err(crate::i18n::t("airdrop-error-failed"));
    }
    serde_json::from_slice(&output).map_err(|_| crate::i18n::t("airdrop-error-failed"))
}

/// One approved file, staged into a sealed anonymous object: what the person verified is what the
/// receiver is handed, whatever happens to the file they named meanwhile.
struct StagedFile {
    file: std::fs::File,
    // Keep the approved inode alive for every admission check. Closing it
    // after staging could let an unlink/recreate reuse its inode number.
    _source: std::fs::File,
    name: String,
    bytes: u64,
    hash: String,
}

impl StagedFile {
    fn staged(
        source: SecureNearbySource,
        admitted: &impl Fn() -> bool,
        limit: Duration,
    ) -> Result<Self, String> {
        let before = source.metadata()?;
        let name = source.name().to_owned();
        if !before.is_file() || before.len() > AIRDROP_MAX_BYTES {
            return Err(crate::i18n::t("airdrop-error-source"));
        }
        let input = source.into_file();
        // SAFETY: constant terminated name and flags create a new owned fd.
        let fd = unsafe {
            libc::memfd_create(
                c"tilo-airdrop".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(crate::i18n::t("airdrop-error-source"));
        }
        // SAFETY: memfd_create returned a descriptor exclusively owned here.
        let writable = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut command = air_drop_command(AIRDROP_SENDER);
        command
            .args([
                "127.0.0.1",
                "1",
                "stage",
                "--source-fd",
                "9",
                "--staged-fd",
                "10",
            ])
            .arg(format!("--name={name}"));
        inherit_descriptors(
            &mut command,
            Some(input.as_raw_fd()),
            Some(writable.as_raw_fd()),
        );
        let reply = air_drop_supervise(command, admitted, limit)?;
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        // SAFETY: fcntl operates only on our anonymous staging descriptor.
        if unsafe { libc::fcntl(writable.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
            return Err(crate::i18n::t("airdrop-error-source"));
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(format!("/proc/self/fd/{}", writable.as_raw_fd()))
            .map_err(|_| crate::i18n::t("airdrop-error-source"))?;
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(AIRDROP_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| crate::i18n::t("airdrop-error-source"))?;
        let hash = hex::encode(Sha256::digest(&bytes));
        if u64::try_from(bytes.len()).ok() != Some(before.len())
            || reply.get("bytes").and_then(serde_json::Value::as_u64) != Some(before.len())
            || reply
                .get("content_hash")
                .and_then(serde_json::Value::as_str)
                != Some(hash.as_str())
        {
            return Err(crate::i18n::t("airdrop-error-source"));
        }
        Ok(Self {
            file,
            _source: input,
            name,
            bytes: before.len(),
            hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn what_files_is_offered_is_named_measured_and_within_its_grants;

        phase progress_is_announced_in_whole_percent;
        {
            assert_eq!(percent(0, 0), 0, "an empty transfer has no progress");
            assert_eq!(percent(5, 1_000), 0);
            assert_eq!(percent(10, 1_000), 1);
            assert_eq!(percent(2_000, 1_000), 100, "never past the total");
        }

        phase a_name_is_cut_to_the_wire_budget_on_a_character_boundary;
        {
            assert_eq!(clamp("Screenshot 1.png", 512), "Screenshot 1.png");
            assert_eq!(clamp("ééé", 3), "é", "a character is never cut in two");
            assert_eq!(clamp("", 4), "");
        }

        phase a_folder_is_offered_under_its_own_name;
        {
            assert_eq!(folder_label(Path::new("/home/ada/Documents")), "Documents");
            assert_eq!(folder_label(Path::new("/")), "Workspace files");
        }

        phase a_file_outside_every_grant_is_not_sent;
        {
            let grants = [PathBuf::from("/home/ada/Documents")];
            let refused = source_from_grants(Path::new("/etc/shadow"), &grants);
            assert!(
                refused.is_err(),
                "a path the workspace shared no grant for is never opened"
            );
        }
    }
}

/// Two of the person's devices, on this machine's loopback with real certificates and the real peer
/// actor: what Files sends from one reaches the other as the same bytes, and both Files lists say
/// so.
#[cfg(all(test, target_os = "linux"))]
mod transfers_between_devices {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "a test fixture: a failed step is the assertion, and the \
                  sessions live until the whole transfer is over"
    )]

    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::platform::fabric::{FabricLink as _, SharedLink};
    use runtime_collab::{
        NearbyTransferCommand, NearbyTransferDestination, NearbyTransferPhase, NearbyTransferReply,
        NearbyTransferSnapshot, NearbyTransferStatus,
    };
    use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
    use tilo_network_actor::DeviceIdentity;
    use tilo_user_identity::DeviceCapabilities;
    use tilo_workspace_service::apps::services::{
        NearbyTransferRequestContext, NearbyTransferServices as _,
    };

    use super::{FILES_APP_ID, NearbyTransfer};
    use crate::platform::fabric::Continuity;
    use crate::sync::{Client, Entry, OwnPresence};

    /// How long the loopback sessions have to carry the whole transfer.
    const DEADLINE: Duration = Duration::from_secs(60);
    /// One pass of the session's loop, as the session runs it on its clock.
    const PASS: Duration = Duration::from_millis(20);
    /// The workspace both devices show, and the Files app in it.
    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    /// The name of the file one device sends the other.
    const NAME: &str = "notes.txt";

    /// One of the person's devices, its transfer engine, and its Files.
    struct Device {
        client: Client,
        id: crate::platform::fabric::DeviceId,
        nearby: Arc<NearbyTransfer>,
        /// Files, as the broker would admit it on its own connection.
        app: NearbyTransferRequestContext,
    }

    impl Device {
        /// Files' answer to a command on this device.
        fn command(&self, command: NearbyTransferCommand) -> NearbyTransferReply {
            self.nearby
                .command(&self.app, command)
                .expect("Files' call is answered")
        }

        /// What Files sees of this device now.
        fn snapshot(&self) -> NearbyTransferSnapshot {
            match self.command(NearbyTransferCommand::Snapshot) {
                NearbyTransferReply::Snapshot { snapshot } => snapshot,
                other => panic!("{other:?} is not a snapshot"),
            }
        }

        /// This device's way onto the person's fabric.
        fn fabric(&self) -> SharedLink {
            self.client
                .fabric()
                .expect("a certified device's session keeps a fabric")
        }
    }

    /// The person's root key: every device's certificate chains to it.
    fn person() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[21; 32])
    }

    /// The person's continuity switches with nothing turned off, as their settings say.
    pub(super) fn permitted() -> Continuity {
        let continuity = Continuity::default();
        let document: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({"continuity": {"enabled": true, "file_transfer": true}}),
        )
        .expect("a settings document");
        continuity.read(&document);
        continuity
    }

    /// The session this device decides against: one workspace, whose Files grant is `folder` .
    pub(super) struct Session {
        tree: Layout,
    }

    impl tilo_broker_core::services::WorkspaceAuthority for Session {
        fn with_tree(&self, read: &mut dyn FnMut(&Layout)) -> Result<(), String> {
            read(&self.tree);
            Ok(())
        }
    }

    /// The session of a device whose Files may read and write `folder` .
    pub(super) fn session(folder: &Path) -> Session {
        let workspace_id = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
        let mut workspace = Workspace::new(workspace_id, "Session");
        workspace.grant_sandbox(
            collab_types::caps::CapOverride::grant(
                tilo_action_policy::Grantee::person("ada"),
                collab_types::caps::Scope::FsPath(folder.to_string_lossy().into_owned()),
                collab_types::caps::Access::Write,
            )
            .through(FILES_APP_ID)
            .lasting(tilo_action_policy::Lifetime::Session),
        );
        Session {
            tree: Layout {
                workspaces: vec![workspace],
                active: workspace_id,
                ..Layout::default()
            },
        }
    }

    /// One of the person's devices, certified by `person` , on its own key, its Files granted
    /// `folder` and its transfers staged in `spool` .
    fn device(
        person: &ed25519_dalek::SigningKey,
        seed: u8,
        label: &str,
        folder: &Path,
        spool: &Path,
        peer_id: u64,
    ) -> Device {
        let key = test_server::signing_key(seed);
        let cert = test_server::certified(person, &key, label, DeviceCapabilities::member());
        let client = Client::start(
            Entry {
                identity: Some(DeviceIdentity::kept(cert, key)),
                own: OwnPresence::default(),
                hosts: Vec::new(),
                continuity: permitted(),
                wake: Arc::new(|| {}),
                relays: test_server::loopback(),
                lan: None,
                records: crate::platform::records::Changes::default(),
            },
            None,
            None,
            Vec::new(),
            None,
            tilo_workspace_service::service::ServiceSlot::default(),
        )
        .expect("a loopback endpoint binds");
        // The fabric names a device by the key its session proves.
        let id = crate::platform::fabric::DeviceId::new(hex::encode(client.publisher().as_bytes()))
            .expect("a device id");
        let nearby = NearbyTransfer::open(
            Some(
                client
                    .fabric()
                    .expect("a certified device's session keeps a fabric"),
            ),
            Some(spool.to_path_buf()),
            permitted(),
            Arc::new(|| {}),
        )
        .expect("a transfer engine");
        nearby.attach_authority(Arc::new(session(folder)));
        client.attach_nearby(Arc::clone(&nearby));
        let app = NearbyTransferRequestContext::for_test(
            peer_id,
            FILES_APP_ID,
            WORKSPACE,
            &format!("files-{peer_id}"),
            "Ada",
        );
        Device {
            client,
            id,
            nearby,
            app,
        }
    }

    /// One pass of each session's loop, as the session's own loop makes it: what the devices sent
    /// is carried out, the Nearby Sends by the engine.
    fn pass(devices: &[&Device]) {
        for device in devices {
            device.client.follow();
            let _ = device.client.fabric_actions();
        }
    }

    /// Run the sessions' passes until `done` holds, or the deadline passes: whether `done` held.
    fn until(devices: &[&Device], mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + DEADLINE;
        loop {
            pass(devices);
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(PASS);
        }
    }

    /// The file a snapshot lists for `name` , if it lists one.
    fn listed(snapshot: &NearbyTransferSnapshot, name: &str) -> Option<NearbyTransferStatus> {
        snapshot
            .transfers
            .iter()
            .find(|status| status.name == name)
            .cloned()
    }

    #[test]
    fn files_sends_a_file_from_one_device_and_the_other_receives_it_whole() {
        let person = person();
        let laptop_folder = tempfile::tempdir().expect("the laptop's files");
        let shell_folder = tempfile::tempdir().expect("the session's files");
        let laptop_spool = tempfile::tempdir().expect("the laptop's spool");
        let shell_spool = tempfile::tempdir().expect("the session's spool");
        // More than one chunk of the transport's, so the bytes really move.
        let bytes: Vec<u8> = (0..700_000_u32).map(|index| (index % 251) as u8).collect();
        let note = laptop_folder.path().join(NAME);
        std::fs::write(&note, &bytes).expect("the file Files sends");
        let laptop = device(
            &person,
            1,
            "Laptop",
            laptop_folder.path(),
            laptop_spool.path(),
            11,
        );
        let session = device(
            &person,
            2,
            "Session",
            shell_folder.path(),
            shell_spool.path(),
            12,
        );
        laptop.client.connect(session.client.endpoint_addr());

        assert!(
            until(&[&laptop, &session], || {
                laptop.fabric().online_device_ids().contains(&session.id)
                    && session.fabric().online_device_ids().contains(&laptop.id)
            }),
            "the two devices' sessions came up"
        );

        // Files on the laptop lists the session, and the session's own folder as where a file it receives
        // would land.
        let targets = laptop.snapshot().targets;
        let target = targets
            .iter()
            .find(|target| target.label == "Session")
            .expect("the session is a device Files may send to")
            .handle;
        let offered = session.snapshot().destinations;
        let destination: NearbyTransferDestination = offered
            .first()
            .expect("the session's Files has a folder to receive into")
            .clone();

        let reply = laptop.command(NearbyTransferCommand::Send {
            target,
            source_path: note.as_os_str().as_bytes().to_vec(),
        });
        assert!(
            matches!(reply, NearbyTransferReply::Queued { .. }),
            "the laptop takes the file to send: {reply:?}"
        );

        // The session's Files is asked, naming the file and nothing of its path.
        let ask = until(&[&laptop, &session], || {
            listed(&session.snapshot(), NAME).is_some_and(|status| {
                status.direction == runtime_collab::NearbyTransferDirection::Receiving
                    && status.phase == NearbyTransferPhase::AwaitingConsent
                    && status.total_bytes == bytes.len() as u64
            })
        });
        assert!(ask, "the session's Files is asked to take the file");
        let asked = listed(&session.snapshot(), NAME).expect("the offer the session lists");

        let reply = session.command(NearbyTransferCommand::Decide {
            handle: asked.handle,
            accept: true,
            destination: Some(destination.handle),
            remember_for_session: true,
        });
        assert_eq!(
            reply,
            NearbyTransferReply::Accepted,
            "the session takes the file into its folder"
        );

        let landed = shell_folder.path().join(NAME);
        assert!(
            until(&[&laptop, &session], || landed.is_file()),
            "the file landed on the session"
        );
        assert_eq!(
            std::fs::read(&landed).expect("the received file"),
            bytes,
            "what the session holds is what the laptop sent, byte for byte"
        );

        // Both Files lists say the transfer is done, each from its own side.
        let done = until(&[&laptop, &session], || {
            let sent = listed(&laptop.snapshot(), NAME).is_some_and(|status| {
                status.direction == runtime_collab::NearbyTransferDirection::Sending
                    && status.phase == NearbyTransferPhase::Completed
            });
            let received = listed(&session.snapshot(), NAME).is_some_and(|status| {
                status.direction == runtime_collab::NearbyTransferDirection::Receiving
                    && status.phase == NearbyTransferPhase::Completed
            });
            sent && received
        });
        assert!(done, "both devices' Files list the transfer as done");
    }
}

/// A receiver that is not one of the person's devices, on this machine's loopback behind a
/// certificate of its own: the real sender helper runs the `AirDrop` wire against it, and what it
/// takes is exactly what the person approved.
#[cfg(all(test, target_os = "linux"))]
mod a_receiver_that_is_not_one_of_yours {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::too_many_lines,
        reason = "a test fixture: a failed step is the assertion"
    )]

    use std::io::{BufRead, BufReader, Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use runtime_collab::{NearbyTransferCommand, NearbyTransferPhase, NearbyTransferReply};
    use sha2::{Digest as _, Sha256};
    use tilo_sheets::portal::{Body, Field, PortalEffect, PortalState};
    use tilo_test_env::SpawnUnderEnvLock as _;
    use tilo_workspace_service::apps::services::{
        NearbyTransferRequestContext, NearbyTransferServices as _,
    };

    use super::transfers_between_devices::{permitted, session};
    use super::{FILES_APP_ID, NearbyTransfer};
    use crate::nearby_airdrop::Consent;

    /// The workspace Files shows.
    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    /// What the file is called on both ends.
    const NAME: &str = "contract.txt";
    const DEADLINE: Duration = Duration::from_secs(60);
    const PASS: Duration = Duration::from_millis(10);

    /// Whether the receiver takes what it is offered or refuses it.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Answer {
        Accept,
        Refuse,
    }

    /// An `AirDrop` receiver of another person, in this process: an HTTPS endpoint that answers
    /// discovery, takes or refuses the ask, and records what it is posted.
    struct Receiver {
        port: u16,
        /// The leaf certificate digest it serves, as its trusted console would print it.
        pin: String,
        requests: Arc<std::sync::Mutex<Vec<String>>>,
        uploads: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
        stop: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Receiver {
        fn start(answer: Answer) -> Self {
            let material =
                rcgen::generate_simple_self_signed(vec!["airdrop-fixture.invalid".to_owned()])
                    .unwrap();
            let cert = material.cert.der().clone();
            let pin = hex::encode(Sha256::digest(&cert));
            let key =
                rustls::pki_types::PrivatePkcs8KeyDer::from(material.signing_key.serialize_der());
            let config = Arc::new(
                rustls::ServerConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert], key.into())
                .unwrap(),
            );
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
            let uploads = Arc::new(std::sync::Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let state = (
                Arc::clone(&requests),
                Arc::clone(&uploads),
                Arc::clone(&stop),
            );
            let worker = std::thread::spawn(move || {
                while !state.2.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((socket, _)) => receive(socket, &config, &state.0, &state.1, answer),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("fixture listener: {error}"),
                    }
                }
            });
            Self {
                port,
                pin,
                requests,
                uploads,
                stop,
                worker: Some(worker),
            }
        }

        fn paths(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        fn uploads(&self) -> Vec<Vec<u8>> {
            self.uploads.lock().unwrap().clone()
        }
    }

    impl Drop for Receiver {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    fn receive(
        socket: TcpStream,
        config: &rustls::ServerConfig,
        requests: &std::sync::Mutex<Vec<String>>,
        uploads: &std::sync::Mutex<Vec<Vec<u8>>>,
        answer: Answer,
    ) {
        let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = socket.set_write_timeout(Some(Duration::from_secs(2)));
        let connection = rustls::ServerConnection::new(Arc::new(config.clone())).unwrap();
        let mut stream = BufReader::new(rustls::StreamOwned::new(connection, socket));
        loop {
            let mut line = String::new();
            match stream.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let path = line.split_whitespace().nth(1).unwrap().to_owned();
            requests.lock().unwrap().push(path.clone());
            let mut length = 0;
            let mut chunked = false;
            loop {
                line.clear();
                stream.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                if line
                    .to_ascii_lowercase()
                    .contains("transfer-encoding: chunked")
                {
                    chunked = true;
                }
            }
            let body = if chunked {
                chunks(&mut stream)
            } else {
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                body
            };
            let (status, reason, reply): (u16, &str, &[u8]) = match path.as_str() {
                "/Ask" if answer == Answer::Refuse => (403, "Forbidden", b""),
                "/Upload" => {
                    uploads.lock().unwrap().push(body);
                    (200, "OK", b"")
                }
                "/Discover" => (
                    200,
                    "OK",
                    b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>ReceiverComputerName</key><string>Somebody else</string></dict></plist>",
                ),
                _ => (200, "OK", b""),
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                reply.len()
            );
            stream.get_mut().write_all(response.as_bytes()).unwrap();
            stream.get_mut().write_all(reply).unwrap();
            stream.get_mut().flush().unwrap();
        }
    }

    fn chunks(stream: &mut impl BufRead) -> Vec<u8> {
        let mut body = Vec::new();
        loop {
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            let length = usize::from_str_radix(line.trim(), 16).unwrap();
            if length == 0 {
                stream.read_line(&mut String::new()).unwrap();
                return body;
            }
            let mut chunk = vec![0; length];
            stream.read_exact(&mut chunk).unwrap();
            body.extend(chunk);
            let mut ending = [0; 2];
            stream.read_exact(&mut ending).unwrap();
            assert_eq!(&ending, b"\r\n");
        }
    }

    /// A port nothing serves: the receiver the person named is not there.
    fn a_port_nobody_serves() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    /// One engine under a Files grant over `folder` , and the connection Files asks through.
    fn engine(folder: &Path, spool: &Path) -> (Arc<NearbyTransfer>, NearbyTransferRequestContext) {
        let nearby = NearbyTransfer::open_with_air_drop(
            None,
            Some(spool.to_path_buf()),
            permitted(),
            Arc::new(|| {}),
            true,
        )
        .expect("a transfer engine");
        nearby.attach_authority(Arc::new(session(folder)));
        let app =
            NearbyTransferRequestContext::for_test(41, FILES_APP_ID, WORKSPACE, "files-41", "Ada");
        (nearby, app)
    }

    /// Run `done` until it holds, or the deadline passes: whether it held.
    fn until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(PASS);
        }
    }

    /// The transfer `name` as Files' snapshot lists it.
    fn listed(
        nearby: &Arc<NearbyTransfer>,
        app: &NearbyTransferRequestContext,
        name: &str,
    ) -> Option<runtime_collab::NearbyTransferStatus> {
        let NearbyTransferReply::Snapshot { snapshot } = nearby
            .command(app, NearbyTransferCommand::Snapshot)
            .unwrap()
        else {
            panic!("Files asks for a snapshot");
        };
        snapshot
            .transfers
            .into_iter()
            .find(|status| status.name == name)
    }

    /// Put the receiver's verification up on the sheet, as the session's pass does, and answer it
    /// with `pin` typed into its field.
    fn verify_with(
        nearby: &Arc<NearbyTransfer>,
        consent: &mut Consent,
        portal: &PortalState,
        pin: &str,
    ) {
        let asked = until(|| {
            consent.follow(Some(nearby), Some(portal));
            portal.dialog().is_some()
        });
        assert!(asked, "the receiver's verification is asked");
        let dialog = portal.dialog().expect("the verification is up");
        assert_eq!(
            dialog.body,
            Body::Entry {
                masked: false,
                required: true,
                choices: Vec::new(),
            },
            "the sheet asks for the fingerprint the person types"
        );
        assert!(
            dialog.detail.contains(NAME) && dialog.detail.contains("127.0.0.1"),
            "the sheet names the file and the receiver it goes to"
        );
        portal.set_text(Field::Answer, pin);
        portal.press(PortalEffect::Confirm {
            handle: dialog.handle,
        });
        consent.follow(Some(nearby), Some(portal));
    }

    /// What the receiver was posted, decoded as the one file the sender archived.
    fn extracted(upload: &[u8]) -> (String, Vec<u8>) {
        let mut child = std::process::Command::new("/usr/bin/python3")
            .args([
                "-I",
                "-c",
                "import gzip,sys;sys.stdout.buffer.write(gzip.decompress(sys.stdin.buffer.read()))",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap();
        child.stdin.take().unwrap().write_all(upload).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "the upload is one gzip archive");
        let bytes = output.stdout;
        assert_eq!(&bytes[..6], b"070701", "a newc cpio archive");
        let size = usize::from_str_radix(std::str::from_utf8(&bytes[54..62]).unwrap(), 16).unwrap();
        let name_size =
            usize::from_str_radix(std::str::from_utf8(&bytes[94..102]).unwrap(), 16).unwrap();
        let name_end = 110_usize.checked_add(name_size).unwrap();
        assert_eq!(&bytes[name_end - 1], &0, "the name ends its field");
        // The archive names its one entry by the path it lands at, which the sender roots at "./".
        let name = String::from_utf8(bytes[110..name_end - 1].to_vec()).unwrap();
        let name = name.strip_prefix("./").unwrap_or(&name).to_owned();
        let payload = name_end.checked_add(3).unwrap() & !3;
        (
            name,
            bytes[payload..payload.checked_add(size).unwrap()].to_vec(),
        )
    }

    /// The bytes both tests send, and the file holding them under the Files grant.
    fn the_file(folder: &Path) -> (std::path::PathBuf, Vec<u8>) {
        let bytes: Vec<u8> = (0..40_000_u32).map(|index| (index % 251) as u8).collect();
        let file = folder.join(NAME);
        std::fs::write(&file, &bytes).expect("the file Files sends");
        (file, bytes)
    }

    #[test]
    fn a_file_reaches_the_receiver_who_took_it_and_only_after_its_fingerprint() {
        assert!(
            Path::new("/usr/bin/python3").is_file(),
            "the AirDrop wire helper needs the system's own interpreter"
        );
        let folder = tempfile::tempdir().expect("the person's files");
        let spool = tempfile::tempdir().expect("the person's spool");
        let (file, bytes) = the_file(folder.path());
        let (nearby, app) = engine(folder.path(), spool.path());
        let portal = PortalState::default();
        let mut consent = Consent::default();
        let receiver = Receiver::start(Answer::Accept);

        assert_eq!(
            nearby
                .command(&app, NearbyTransferCommand::AirDropAvailability)
                .expect("Files asks whether it may offer the receiver"),
            NearbyTransferReply::AirDropAvailability { available: true },
            "the session offers the AirDrop path to Files"
        );
        let NearbyTransferReply::Queued { handle } = nearby
            .command(
                &app,
                NearbyTransferCommand::SendAirDrop {
                    source_path: file.as_os_str().as_bytes().to_vec(),
                    address: "127.0.0.1".into(),
                    port: receiver.port,
                },
            )
            .expect("Files hands the file over")
        else {
            panic!("an AirDrop send is queued");
        };
        assert!(handle.valid());
        let waiting = until(|| {
            listed(&nearby, &app, NAME)
                .is_some_and(|status| status.phase == NearbyTransferPhase::AwaitingConsent)
        });
        assert!(
            waiting,
            "Files lists the attempt while the receiver is verified"
        );
        assert_eq!(
            listed(&nearby, &app, NAME).unwrap().device_label,
            format!("AirDrop · 127.0.0.1:{}", receiver.port),
            "Files names the receiver by its endpoint"
        );

        // A fingerprint that is not the receiver's sends nothing, and the question stays up.
        verify_with(&nearby, &mut consent, &portal, &"00".repeat(32));
        assert!(
            portal.dialog().is_some_and(|dialog| dialog.error.is_some()),
            "the sheet says the fingerprint differs"
        );
        assert!(
            listed(&nearby, &app, NAME)
                .is_some_and(|status| status.phase == NearbyTransferPhase::AwaitingConsent),
            "the attempt still waits"
        );
        assert!(receiver.paths().is_empty(), "nothing was sent");

        // The fingerprint its console printed sends the staged bytes once.
        verify_with(&nearby, &mut consent, &portal, &receiver.pin);
        assert!(
            portal.dialog().is_none(),
            "the question is answered and taken down"
        );
        assert!(
            until(|| listed(&nearby, &app, NAME)
                .is_some_and(|status| status.phase == NearbyTransferPhase::Completed)),
            "Files lists the attempt as done"
        );
        assert_eq!(receiver.paths(), ["/Discover", "/Ask", "/Upload"]);
        let uploads = receiver.uploads();
        assert_eq!(uploads.len(), 1, "one upload");
        assert_eq!(
            extracted(&uploads[0]),
            (NAME.to_owned(), bytes),
            "the receiver holds exactly the bytes the person approved"
        );
    }

    #[test]
    fn a_receiver_who_declines_the_ask_gets_no_bytes_and_files_says_so() {
        let folder = tempfile::tempdir().expect("the person's files");
        let spool = tempfile::tempdir().expect("the person's spool");
        let (file, _bytes) = the_file(folder.path());
        let (nearby, app) = engine(folder.path(), spool.path());
        let portal = PortalState::default();
        let mut consent = Consent::default();
        let receiver = Receiver::start(Answer::Refuse);

        nearby
            .command(
                &app,
                NearbyTransferCommand::SendAirDrop {
                    source_path: file.as_os_str().as_bytes().to_vec(),
                    address: "127.0.0.1".into(),
                    port: receiver.port,
                },
            )
            .expect("Files hands the file over");
        verify_with(&nearby, &mut consent, &portal, &receiver.pin);
        assert!(
            until(|| listed(&nearby, &app, NAME)
                .is_some_and(|status| status.phase == NearbyTransferPhase::Failed)),
            "Files lists the attempt as failed"
        );
        assert_eq!(receiver.paths(), ["/Discover", "/Ask"]);
        assert!(receiver.uploads().is_empty(), "the receiver took nothing");
    }

    #[test]
    fn a_person_who_declines_the_verification_sends_nothing() {
        let folder = tempfile::tempdir().expect("the person's files");
        let spool = tempfile::tempdir().expect("the person's spool");
        let (file, _bytes) = the_file(folder.path());
        let (nearby, app) = engine(folder.path(), spool.path());
        let portal = PortalState::default();
        let mut consent = Consent::default();
        let receiver = Receiver::start(Answer::Accept);

        nearby
            .command(
                &app,
                NearbyTransferCommand::SendAirDrop {
                    source_path: file.as_os_str().as_bytes().to_vec(),
                    address: "127.0.0.1".into(),
                    port: receiver.port,
                },
            )
            .expect("Files hands the file over");
        assert!(
            until(|| {
                consent.follow(Some(&nearby), Some(&portal));
                portal.dialog().is_some()
            }),
            "the receiver's verification is asked"
        );
        let dialog = portal.dialog().expect("the verification is up");
        portal.press(PortalEffect::Cancel {
            handle: dialog.handle,
        });
        consent.follow(Some(&nearby), Some(&portal));
        assert!(
            until(|| listed(&nearby, &app, NAME)
                .is_some_and(|status| status.phase == NearbyTransferPhase::Cancelled)),
            "Files lists the attempt as cancelled"
        );
        assert!(receiver.paths().is_empty(), "nothing was sent");
    }

    #[test]
    fn a_receiver_that_is_not_there_fails_before_any_question() {
        let folder = tempfile::tempdir().expect("the person's files");
        let spool = tempfile::tempdir().expect("the person's spool");
        let (file, _bytes) = the_file(folder.path());
        let (nearby, app) = engine(folder.path(), spool.path());
        let portal = PortalState::default();
        let mut consent = Consent::default();

        nearby
            .command(
                &app,
                NearbyTransferCommand::SendAirDrop {
                    source_path: file.as_os_str().as_bytes().to_vec(),
                    address: "127.0.0.1".into(),
                    port: a_port_nobody_serves(),
                },
            )
            .expect("Files hands the file over");
        assert!(
            until(|| listed(&nearby, &app, NAME)
                .is_some_and(|status| status.phase == NearbyTransferPhase::Failed)),
            "Files lists the attempt as failed"
        );
        consent.follow(Some(&nearby), Some(&portal));
        assert!(
            portal.dialog().is_none(),
            "nothing is asked about a receiver that never answered"
        );
    }
}
