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
//! Files reaches it through the action door ([`NearbyTransferServices`]): the
//! broker binds every handle it is handed to the connection that asked and to
//! that connection's workspace, and this engine refuses a handle of another
//! connection. The spool belongs to a worker thread of the engine's own,
//! which stages, appends and installs off the session's pass; what a snapshot
//! shows is that worker's last projection, never a read that could wait on a
//! disk.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{Hash as _, Hasher as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::platform::fabric::{DeviceId, PersonalFabricAction};
use crate::platform::fabric::{FabricLink as _, NearbySpoolOffer, SharedLink};
use runtime_collab::{
    MAX_NEARBY_TRANSFER_DESTINATIONS, MAX_NEARBY_TRANSFER_LABEL_BYTES,
    MAX_NEARBY_TRANSFER_NAME_BYTES, MAX_NEARBY_TRANSFER_STATUSES, MAX_NEARBY_TRANSFER_TARGETS,
    NearbyDestinationHandle, NearbyTargetHandle, NearbyTransferCommand, NearbyTransferDestination,
    NearbyTransferDirection, NearbyTransferHandle, NearbyTransferPhase, NearbyTransferReply,
    NearbyTransferSnapshot, NearbyTransferStatus, NearbyTransferTarget,
};
use tilo_action_broker::services::{
    NearbyTransferChange, NearbyTransferRequestContext, NearbyTransferServices,
};
use tilo_broker_core::services::WorkspaceAuthority;
use tilo_capture_studio::{Refusal, Sent};
use tilo_desktop_model::capability::{Capability, FsMode};
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
use ui::widget::ScreenshotSendDevice;

use crate::platform::fabric::Continuity;

/// The one app the action door serves: the certified Files application.
pub(crate) const FILES_APP_ID: &str = "so.tilo.file-explorer";

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
    let fabric = link.device_fabric();
    let mut devices: Vec<ScreenshotSendDevice> = fabric
        .online_devices()
        .filter(|presence| online.contains(&presence.device))
        .filter(|presence| local.as_ref() != Some(&presence.device))
        .map(|presence| ScreenshotSendDevice {
            id: presence.device.as_str().to_owned(),
            label: presence.label.clone(),
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
        tracing::info!(
            fabric = link.is_some(),
            staging = spool.is_some(),
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
        });
        let working = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("tilo-nearby-transfer".into())
            .spawn(move || work(&working, mailbox))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(Self { inner }))
    }

    /// The workspaces' grants are read from the desk `authority` decides against.
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
    /// acknowledged, and say what moved.
    pub(crate) fn pump(&self) {
        if self.inner.continuity.file_transfer_permitted() {
            self.inner.expire_offers();
            self.inner.offer_again();
        }
        self.inner.observe();
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
                link.device_fabric()
                    .snapshots()
                    .iter()
                    .map(|(device, snapshot)| (device.clone(), snapshot.presence.label.clone()))
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
            let device_label = labels
                .get(&row.peer)
                .cloned()
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
        let receiving = {
            let state = self.inner.state.lock_recover();
            state.offers.contains_key(&transfer_id)
                || state
                    .queues
                    .inbox
                    .iter()
                    .any(|entry| entry.id == transfer_id)
        };
        if receiving {
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
                Ok(NearbyTransferReply::AirDropAvailability { available: false })
            }
            NearbyTransferCommand::SendAirDrop { .. } => Err(
                "this session hands files to the person's own devices, not to a lab receiver"
                    .to_owned(),
            ),
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
    peer: DeviceId,
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
            peer: offer.source.clone(),
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
            peer,
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
            peer,
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
    for entry in &state.history {
        rows.push(Row {
            transfer_id: entry.transfer_id,
            peer: entry.peer.clone(),
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

/// Open the file `path` names beneath the Files grant that holds it. Files names files by raw
/// bytes: a name that is not UTF-8 is still one the person may send.
fn source_from_grants(
    requested: &Path,
    grants: &[Capability],
) -> Result<SecureNearbySource, String> {
    let mut best: Option<&Path> = None;
    for capability in grants {
        let Capability::Filesystem { path, .. } = capability else {
            continue;
        };
        if !requested.starts_with(path) {
            continue;
        }
        if best.is_none_or(|grant| path.components().count() > grant.components().count()) {
            best = Some(path);
        }
    }
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
        let now = tilo_clock::unix_now();
        for capability in workspace
            .app_grants_snapshot_at(now)
            .get(FILES_APP_ID)
            .into_iter()
            .flatten()
        {
            let Capability::Filesystem { path, mode, .. } = capability else {
                continue;
            };
            if *mode != FsMode::ReadWrite || !path.is_dir() {
                continue;
            }
            let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
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

    /// What Files in `workspace` may read: the filesystem grants its workspace carries for it.
    fn files_grants(&self, workspace: &str) -> Vec<Capability> {
        let authority = self.authority.lock_recover().clone();
        let Some(authority) = authority else {
            return Vec::new();
        };
        let mut grants: Vec<Capability> = Vec::new();
        let read = authority.with_workspace(workspace, &mut |workspace| {
            let Some(workspace) = workspace else {
                return;
            };
            let now = tilo_clock::unix_now();
            for capability in workspace
                .app_grants_snapshot_at(now)
                .get(FILES_APP_ID)
                .into_iter()
                .flatten()
            {
                if matches!(capability, Capability::Filesystem { .. }) {
                    grants.push(capability.clone());
                }
            }
        });
        if let Err(error) = read {
            tracing::warn!(%error, "a workspace's grants are not read for a send");
            return Vec::new();
        }
        grants
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
        drop(state);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_announced_in_whole_percent() {
        assert_eq!(percent(0, 0), 0, "an empty transfer has no progress");
        assert_eq!(percent(5, 1_000), 0);
        assert_eq!(percent(10, 1_000), 1);
        assert_eq!(percent(2_000, 1_000), 100, "never past the total");
    }

    #[test]
    fn a_name_is_cut_to_the_wire_budget_on_a_character_boundary() {
        assert_eq!(clamp("Screenshot 1.png", 512), "Screenshot 1.png");
        assert_eq!(clamp("ééé", 3), "é", "a character is never cut in two");
        assert_eq!(clamp("", 4), "");
    }

    #[test]
    fn a_folder_is_offered_under_its_own_name() {
        assert_eq!(folder_label(Path::new("/home/ada/Documents")), "Documents");
        assert_eq!(folder_label(Path::new("/")), "Workspace files");
    }

    #[test]
    fn a_file_outside_every_grant_is_not_sent() {
        let grants = [Capability::Filesystem {
            path: PathBuf::from("/home/ada/Documents"),
            mode: FsMode::Read,
            scope: tilo_desktop_model::capability::GRANT_SESSION,
        }];
        let refused = source_from_grants(Path::new("/etc/shadow"), &grants);
        assert!(
            refused.is_err(),
            "a path the workspace shared no grant for is never opened"
        );
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
    use tilo_action_broker::services::{NearbyTransferRequestContext, NearbyTransferServices as _};
    use tilo_desktop_model::{
        Capability, DesktopTree, FsMode, GRANT_SESSION, Workspace, WorkspaceId, WorkspaceSlot,
    };
    use tilo_network_actor::HostIdentity;
    use tilo_user_identity::DeviceCapabilities;

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
    fn permitted() -> Continuity {
        let continuity = Continuity::default();
        let document: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({"continuity": {"enabled": true, "file_transfer": true}}),
        )
        .expect("a settings document");
        continuity.read(&document);
        continuity
    }

    /// The desk this device decides against: one workspace, whose Files grant is `folder` .
    struct Desk {
        tree: DesktopTree,
    }

    impl tilo_broker_core::services::WorkspaceAuthority for Desk {
        fn with_tree(&self, read: &mut dyn FnMut(&DesktopTree)) -> Result<(), String> {
            read(&self.tree);
            Ok(())
        }
    }

    /// The desk of a device whose Files may read and write `folder` .
    fn desk(folder: &Path) -> Desk {
        let workspace_id = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
        let slot = WorkspaceSlot::of_workspace(workspace_id);
        let mut workspace = Workspace::in_workspace(slot, workspace_id, "Desk");
        workspace.grant_app(
            FILES_APP_ID,
            &Capability::Filesystem {
                path: folder.to_path_buf(),
                mode: FsMode::ReadWrite,
                scope: GRANT_SESSION,
            },
        );
        Desk {
            tree: DesktopTree {
                workspaces: vec![workspace],
                active: slot,
                ..DesktopTree::default()
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
        let key = test_relay::signing_key(seed);
        let cert = test_relay::certified(person, &key, label, DeviceCapabilities::member());
        let client = Client::start(
            Entry {
                identity: Some(HostIdentity::kept(cert, key)),
                own: OwnPresence::default(),
                hosts: Vec::new(),
                continuity: permitted(),
                wake: Arc::new(|| {}),
                fabric: None,
                relays: test_relay::loopback(),
            },
            None,
            None,
            Vec::new(),
            None,
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
        nearby.attach_authority(Arc::new(desk(folder)));
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
        let desk_folder = tempfile::tempdir().expect("the desk's files");
        let laptop_spool = tempfile::tempdir().expect("the laptop's spool");
        let desk_spool = tempfile::tempdir().expect("the desk's spool");
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
        let desk = device(
            &person,
            2,
            "Desk",
            desk_folder.path(),
            desk_spool.path(),
            12,
        );
        laptop.client.connect(desk.client.endpoint_addr());

        assert!(
            until(&[&laptop, &desk], || {
                laptop.fabric().online_device_ids().contains(&desk.id)
                    && desk.fabric().online_device_ids().contains(&laptop.id)
            }),
            "the two devices' sessions came up"
        );

        // Files on the laptop lists the desk, and the desk's own folder as where a file it receives
        // would land.
        let targets = laptop.snapshot().targets;
        let target = targets
            .iter()
            .find(|target| target.label == "Desk")
            .expect("the desk is a device Files may send to")
            .handle;
        let offered = desk.snapshot().destinations;
        let destination: NearbyTransferDestination = offered
            .first()
            .expect("the desk's Files has a folder to receive into")
            .clone();

        let reply = laptop.command(NearbyTransferCommand::Send {
            target,
            source_path: note.as_os_str().as_bytes().to_vec(),
        });
        assert!(
            matches!(reply, NearbyTransferReply::Queued { .. }),
            "the laptop takes the file to send: {reply:?}"
        );

        // The desk's Files is asked, naming the file and nothing of its path.
        let ask = until(&[&laptop, &desk], || {
            listed(&desk.snapshot(), NAME).is_some_and(|status| {
                status.direction == runtime_collab::NearbyTransferDirection::Receiving
                    && status.phase == NearbyTransferPhase::AwaitingConsent
                    && status.total_bytes == bytes.len() as u64
            })
        });
        assert!(ask, "the desk's Files is asked to take the file");
        let asked = listed(&desk.snapshot(), NAME).expect("the offer the desk lists");

        let reply = desk.command(NearbyTransferCommand::Decide {
            handle: asked.handle,
            accept: true,
            destination: Some(destination.handle),
            remember_for_session: true,
        });
        assert_eq!(
            reply,
            NearbyTransferReply::Accepted,
            "the desk takes the file into its folder"
        );

        let landed = desk_folder.path().join(NAME);
        assert!(
            until(&[&laptop, &desk], || landed.is_file()),
            "the file landed on the desk"
        );
        assert_eq!(
            std::fs::read(&landed).expect("the received file"),
            bytes,
            "what the desk holds is what the laptop sent, byte for byte"
        );

        // Both Files lists say the transfer is done, each from its own side.
        let done = until(&[&laptop, &desk], || {
            let sent = listed(&laptop.snapshot(), NAME).is_some_and(|status| {
                status.direction == runtime_collab::NearbyTransferDirection::Sending
                    && status.phase == NearbyTransferPhase::Completed
            });
            let received = listed(&desk.snapshot(), NAME).is_some_and(|status| {
                status.direction == runtime_collab::NearbyTransferDirection::Receiving
                    && status.phase == NearbyTransferPhase::Completed
            });
            sent && received
        });
        assert!(done, "both devices' Files list the transfer as done");
    }
}
