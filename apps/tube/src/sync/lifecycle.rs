//! Product lifecycle around the Grayjay wire transport.
//!
//! The Noise key is deterministically derived from the app-scoped secret
//! returned by the desktop secret portal. Only public peer routes and durable
//! revocation tombstones are written to disk. All network work is serialized on
//! one bounded worker thread so redraws never dial a phone and shutdown is
//! bounded by the transport's socket deadlines.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;
use zeroize::Zeroizing;

use super::transport::{DeviceKey, GrayjaySyncTransport};
use super::{PeerDescriptor, PeerId, SyncError, SyncResult, SyncState, SyncTransport};
use crate::model::TubeModel;

const STORE_VERSION: u32 = 1;
const MAX_STORE_BYTES: u64 = 1024 * 1024;
const MAX_PEERS: usize = 64;
const AUTO_SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

/// Durable, non-secret record for one locally approved Grayjay peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    pub id: String,
    pub name: String,
    pub descriptor: PeerDescriptor,
    pub approved_at: i64,
    /// Pairing authenticates the remote key, but local lifecycle approval is a
    /// separate explicit effect. Pending peers cannot be restored or synced.
    #[serde(default)]
    pub pending_approval: bool,
    #[serde(default)]
    pub revoked_at: Option<i64>,
    #[serde(default)]
    pub last_sync_at: Option<i64>,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl PeerRecord {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none() && !self.pending_approval
    }

    pub fn short_id(&self) -> String {
        self.id.chars().take(10).collect()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyncStatus {
    #[default]
    Unavailable,
    Ready,
    Pairing,
    Syncing,
    Error,
}

/// Render-safe sync state. It contains public fingerprints and peer metadata,
/// never pairing codes, private keys, or portal secret bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncView {
    pub status: SyncStatus,
    pub summary: String,
    pub device_public_key: Option<String>,
    pub peers: Vec<PeerRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PeerFile {
    version: u32,
    peers: Vec<PeerRecord>,
}

#[derive(Clone, Debug)]
struct PeerStore {
    path: PathBuf,
}

impl PeerStore {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> SyncResult<Vec<PeerRecord>> {
        let file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(SyncError::Persistence(error.to_string())),
        };
        let metadata = file
            .metadata()
            .map_err(|error| SyncError::Persistence(error.to_string()))?;
        if !metadata.is_file() || metadata.len() > MAX_STORE_BYTES {
            return Err(SyncError::Persistence("invalid peer store".into()));
        }
        let mut bytes = Vec::new();
        file.take(MAX_STORE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| SyncError::Persistence(error.to_string()))?;
        let file: PeerFile = serde_json::from_slice(&bytes)
            .map_err(|error| SyncError::Persistence(error.to_string()))?;
        if file.version != STORE_VERSION {
            return Err(SyncError::Persistence(
                "unsupported peer store version".into(),
            ));
        }
        if file.peers.len() > MAX_PEERS
            || file.peers.iter().any(|peer| {
                peer.id.len() > 256
                    || peer.name.len() > 256
                    || peer.descriptor.public_key.len() > 256
                    || peer.descriptor.addresses.len() > 8
                    || peer
                        .descriptor
                        .addresses
                        .iter()
                        .any(|address| address.is_empty() || address.len() > 255)
            })
        {
            return Err(SyncError::Persistence(
                "peer store exceeds its structural bounds".into(),
            ));
        }
        Ok(file.peers)
    }

    fn save(&self, peers: &[PeerRecord]) -> SyncResult<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| SyncError::Persistence("peer store has no parent".into()))?;
        fs::create_dir_all(parent).map_err(|error| SyncError::Persistence(error.to_string()))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| SyncError::Persistence(error.to_string()))?;
        let bytes = serde_json::to_vec_pretty(&PeerFile {
            version: STORE_VERSION,
            peers: peers.to_vec(),
        })
        .map_err(|error| SyncError::Persistence(error.to_string()))?;
        let temp = parent.join(format!(".peers-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| SyncError::Persistence(error.to_string()))?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)?;
            OpenOptions::new().read(true).open(parent)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|error: io::Error| SyncError::Persistence(error.to_string()))
    }
}

enum Command {
    Pair(Zeroizing<String>),
    Approve(String),
    Revoke(String),
    Sync(SyncState),
    Stop,
}

enum Event {
    View(SyncView),
    Pulled(SyncState),
}

/// UI-side handle for the bounded background worker.
pub struct DeviceSync {
    commands: Option<mpsc::SyncSender<Command>>,
    events: mpsc::Receiver<Event>,
    pub pairing_input: Arc<Mutex<String>>,
    view: SyncView,
}

impl Default for DeviceSync {
    fn default() -> Self {
        let (_tx, events) = mpsc::channel();
        Self {
            commands: None,
            events,
            pairing_input: Arc::new(Mutex::new(String::new())),
            view: SyncView {
                status: SyncStatus::Unavailable,
                summary: "Start the live app to initialize device sync.".into(),
                ..SyncView::default()
            },
        }
    }
}

impl DeviceSync {
    /// Initialize the shipped backend. Failure to reach the host secret portal
    /// is visible and fail-closed: no random replacement identity is generated.
    pub fn production(device_name: &str) -> Self {
        match production_parts(device_name) {
            Ok((transport, store, public_key)) => {
                Self::spawn(Box::new(transport), store, public_key)
            }
            Err(error) => Self::unavailable(error.to_string()),
        }
    }

    fn unavailable(summary: String) -> Self {
        let mut out = Self::default();
        out.view.summary = summary;
        out
    }

    #[cfg(test)]
    pub(crate) fn with_test_view(view: SyncView) -> Self {
        let (_tx, events) = mpsc::channel();
        Self {
            commands: None,
            events,
            pairing_input: Arc::new(Mutex::new(String::new())),
            view,
        }
    }

    /// Deterministic, secret-free lifecycle state for the offscreen visual
    /// review mode. It never starts a worker or opens a socket.
    #[doc(hidden)]
    pub fn visual_sample() -> Self {
        Self::with_inert_view(SyncView {
            status: SyncStatus::Ready,
            summary: "Two devices are ready; one awaits your approval.".into(),
            device_public_key: Some("q4w8Jf3mP7k2V9nR6xT1".into()),
            peers: vec![
                PeerRecord {
                    id: "6A:1C:92:7E:4B:80".into(),
                    name: "Pixel phone".into(),
                    descriptor: PeerDescriptor {
                        public_key: "visual-active-key".into(),
                        addresses: vec!["192.0.2.10".into()],
                        port: 12315,
                    },
                    approved_at: 1,
                    pending_approval: false,
                    revoked_at: None,
                    last_sync_at: Some(now() - 180),
                    last_error: None,
                },
                PeerRecord {
                    id: "B5:73:0D:11:9A:CE".into(),
                    name: "Living-room tablet".into(),
                    descriptor: PeerDescriptor {
                        public_key: "visual-pending-key".into(),
                        addresses: vec!["192.0.2.11".into()],
                        port: 12315,
                    },
                    approved_at: 0,
                    pending_approval: true,
                    revoked_at: None,
                    last_sync_at: None,
                    last_error: None,
                },
            ],
        })
    }

    fn with_inert_view(view: SyncView) -> Self {
        let (_tx, events) = mpsc::channel();
        Self {
            commands: None,
            events,
            pairing_input: Arc::new(Mutex::new(String::new())),
            view,
        }
    }

    fn spawn(transport: Box<dyn SyncTransport>, store: PeerStore, public_key: String) -> Self {
        let (command_tx, command_rx) = mpsc::sync_channel(8);
        let (event_tx, event_rx) = mpsc::channel();
        let worker_public_key = public_key.clone();
        let spawned = thread::Builder::new()
            .name("tube-grayjay-sync".into())
            .spawn(move || {
                worker(transport, store, worker_public_key, command_rx, event_tx);
            })
            .is_ok();
        if !spawned {
            return Self::unavailable("Could not start the device-sync worker.".into());
        }
        Self {
            commands: Some(command_tx),
            events: event_rx,
            pairing_input: Arc::new(Mutex::new(String::new())),
            view: SyncView {
                status: SyncStatus::Ready,
                summary: "Loading approved devices…".into(),
                ..SyncView::default()
            },
        }
    }

    pub fn view(&self) -> &SyncView {
        &self.view
    }

    /// Move the local text field into a `Secret` action argument and wipe the
    /// field. The returned value must only be placed in that redacted argument.
    pub fn take_pairing_input(&self) -> String {
        let mut input = self
            .pairing_input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let blob = input.trim().to_string();
        input.zeroize();
        input.clear();
        blob
    }

    pub fn pair(&self, invite: String) -> bool {
        !invite.is_empty() && self.try_send(Command::Pair(Zeroizing::new(invite)))
    }

    pub fn revoke(&self, id: &str) -> bool {
        self.try_send(Command::Revoke(id.to_string()))
    }

    pub fn approve(&self, id: &str) -> bool {
        self.try_send(Command::Approve(id.to_string()))
    }

    pub fn sync_now(&self, state: SyncState) -> bool {
        self.try_send(Command::Sync(state))
    }

    fn try_send(&self, command: Command) -> bool {
        self.commands
            .as_ref()
            .is_some_and(|sender| sender.try_send(command).is_ok())
    }

    /// Drain worker updates and merge pulled personal state. Returns true when
    /// the app should rebuild/persist its non-secret collaborative document.
    pub fn pump(&mut self, model: &mut TubeModel) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::View(view) => {
                    if self.view != view {
                        self.view = view;
                        changed = true;
                    }
                }
                Event::Pulled(remote) => changed |= merge_remote_state(model, &remote),
            }
        }
        changed
    }
}

impl Drop for DeviceSync {
    fn drop(&mut self) {
        let _ = self.try_send(Command::Stop);
    }
}

fn production_parts(device_name: &str) -> SyncResult<(GrayjaySyncTransport, PeerStore, String)> {
    let portal_secret = Zeroizing::new(
        runtime_portal_xdg::Secret::retrieve()
            .map_err(|error| SyncError::KeyUnavailable(error.to_string()))?,
    );
    if portal_secret.len() < 16 {
        return Err(SyncError::KeyUnavailable(
            "host secret broker returned insufficient key material".into(),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"qos.tube.grayjay-noise-static-v1\0");
    digest.update(portal_secret.as_slice());
    let private = Zeroizing::new(digest.finalize().to_vec());
    let key = DeviceKey::from_private(private.as_slice())?;
    let public_key = key.public_base64();
    let transport = GrayjaySyncTransport::with_device(key, device_name);
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::state_dir())
        .ok_or_else(|| SyncError::Persistence("no private state directory is available".into()))?;
    Ok((
        transport,
        PeerStore::new(base.join("qos-tube").join("grayjay-peers.json")),
        public_key,
    ))
}

fn publish(
    events: &mpsc::Sender<Event>,
    status: SyncStatus,
    summary: impl Into<String>,
    public_key: &str,
    peers: &[PeerRecord],
) {
    let _ = events.send(Event::View(SyncView {
        status,
        summary: summary.into(),
        device_public_key: Some(public_key.to_string()),
        peers: peers.to_vec(),
    }));
}

fn worker(
    mut transport: Box<dyn SyncTransport>,
    store: PeerStore,
    public_key: String,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
) {
    let mut peers = match store.load() {
        Ok(peers) => peers,
        Err(error) => {
            publish(
                &events,
                SyncStatus::Error,
                error.to_string(),
                &public_key,
                &[],
            );
            return;
        }
    };
    for peer in peers.iter().filter(|peer| peer.is_active()) {
        if let Err(error) = transport.restore_peer(&peer.descriptor) {
            publish(
                &events,
                SyncStatus::Error,
                format!("Could not restore {}: {error}", peer.name),
                &public_key,
                &peers,
            );
            return;
        }
    }
    publish(
        &events,
        SyncStatus::Ready,
        if peers.iter().any(PeerRecord::is_active) {
            "Approved devices are ready to sync."
        } else {
            "Paste a Grayjay pairing invite to add your first device."
        },
        &public_key,
        &peers,
    );

    let mut latest_state = None;
    loop {
        let command = match commands.recv_timeout(AUTO_SYNC_INTERVAL) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => match latest_state.clone() {
                Some(state) => Command::Sync(state),
                None => continue,
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        match command {
            Command::Stop => return,
            Command::Pair(blob) => {
                publish(
                    &events,
                    SyncStatus::Pairing,
                    "Waiting for approval on the other device…",
                    &public_key,
                    &peers,
                );
                match transport.pair(blob.as_str()).and_then(|id| {
                    if peers
                        .iter()
                        .any(|peer| peer.id == id.0 && !peer.is_active())
                    {
                        transport.forget_peer(&id);
                        return Err(SyncError::Revoked);
                    }
                    let descriptor = transport.peer_descriptor(&id).ok_or_else(|| {
                        SyncError::Persistence(
                            "transport did not expose a public peer route".into(),
                        )
                    })?;
                    let name = format!("Grayjay {}", id.0.chars().take(8).collect::<String>());
                    let mut candidate = peers.clone();
                    if let Some(existing) = candidate.iter_mut().find(|peer| peer.id == id.0) {
                        existing.descriptor = descriptor;
                        existing.pending_approval = true;
                        existing.last_error = None;
                    } else {
                        if candidate.len() >= MAX_PEERS {
                            transport.forget_peer(&id);
                            return Err(SyncError::Persistence(
                                "the approved-device limit has been reached".into(),
                            ));
                        }
                        candidate.push(PeerRecord {
                            id: id.0,
                            name,
                            descriptor,
                            approved_at: now(),
                            pending_approval: true,
                            revoked_at: None,
                            last_sync_at: None,
                            last_error: None,
                        });
                    }
                    store.save(&candidate)?;
                    peers = candidate;
                    Ok(())
                }) {
                    Ok(()) => publish(
                        &events,
                        SyncStatus::Ready,
                        "Device authenticated. Review its fingerprint and approve access.",
                        &public_key,
                        &peers,
                    ),
                    Err(error) => publish(
                        &events,
                        SyncStatus::Error,
                        error.to_string(),
                        &public_key,
                        &peers,
                    ),
                }
            }
            Command::Approve(id) => {
                let mut candidate = peers.clone();
                if let Some(peer) = candidate
                    .iter_mut()
                    .find(|peer| peer.id == id && peer.revoked_at.is_none())
                {
                    peer.pending_approval = false;
                    peer.approved_at = now();
                    let descriptor = peer.descriptor.clone();
                    let result = store.save(&candidate).and_then(|()| {
                        transport.restore_peer(&descriptor)?;
                        peers = candidate;
                        Ok(())
                    });
                    publish(
                        &events,
                        if result.is_ok() {
                            SyncStatus::Ready
                        } else {
                            SyncStatus::Error
                        },
                        result.map_or_else(
                            |error| error.to_string(),
                            |_| "Device approved. Sync is ready.".into(),
                        ),
                        &public_key,
                        &peers,
                    );
                }
            }
            Command::Revoke(id) => {
                let mut candidate = peers.clone();
                if let Some(peer) = candidate.iter_mut().find(|peer| peer.id == id) {
                    peer.revoked_at = Some(now());
                    peer.last_error = None;
                    let result = store.save(&candidate).map(|()| {
                        peers = candidate;
                        transport.forget_peer(&PeerId(id));
                    });
                    publish(
                        &events,
                        if result.is_ok() {
                            SyncStatus::Ready
                        } else {
                            SyncStatus::Error
                        },
                        result.map_or_else(
                            |error| error.to_string(),
                            |_| "Device revoked. Its key can no longer sync.".into(),
                        ),
                        &public_key,
                        &peers,
                    );
                }
            }
            Command::Sync(state) => {
                latest_state = Some(state.clone());
                publish(
                    &events,
                    SyncStatus::Syncing,
                    "Syncing approved devices…",
                    &public_key,
                    &peers,
                );
                let active = peers
                    .iter()
                    .enumerate()
                    .filter(|(_, peer)| peer.is_active())
                    .map(|(index, peer)| (index, PeerId(peer.id.clone())))
                    .collect::<Vec<_>>();
                let mut pulled = SyncState::default();
                let mut failures = 0usize;
                for (index, id) in active {
                    match transport.pull(&id) {
                        Ok(remote) => merge_sync_states(&mut pulled, remote),
                        Err(error) => {
                            peers[index].last_error = Some(error.to_string());
                            failures += 1;
                            continue;
                        }
                    }
                    match transport.push(&id, &state) {
                        Ok(()) => {
                            peers[index].last_sync_at = Some(now());
                            peers[index].last_error = None;
                        }
                        Err(error) => {
                            peers[index].last_error = Some(error.to_string());
                            failures += 1;
                        }
                    }
                }
                if let Err(error) = store.save(&peers) {
                    failures += 1;
                    if let Some(peer) = peers.first_mut() {
                        peer.last_error = Some(error.to_string());
                    }
                }
                if pulled != SyncState::default() {
                    let _ = events.send(Event::Pulled(pulled));
                }
                publish(
                    &events,
                    if failures == 0 {
                        SyncStatus::Ready
                    } else {
                        SyncStatus::Error
                    },
                    if failures == 0 {
                        "Device sync is up to date.".into()
                    } else {
                        format!("{failures} device operation(s) failed.")
                    },
                    &public_key,
                    &peers,
                );
            }
        }
    }
}

fn merge_sync_states(target: &mut SyncState, incoming: SyncState) {
    for subscription in incoming.subscriptions {
        if !target.subscriptions.contains(&subscription) {
            target.subscriptions.push(subscription);
        }
    }
    for video in incoming.watch_history {
        if !target.watch_history.contains(&video) {
            target.watch_history.push(video);
        }
    }
    if incoming.settings.is_some() {
        target.settings = incoming.settings;
    }
}

/// Snapshot the syncable personal state. Public catalogue ids and preferences
/// are included; local device records and all key material are not.
pub fn state_from_model(model: &TubeModel) -> SyncState {
    let subscriptions = model
        .subscriptions
        .iter()
        .filter_map(|id| model.channel(id))
        .map(|channel| {
            if channel.handle.starts_with("http") {
                channel.handle.clone()
            } else {
                format!(
                    "https://youtube.com/{}",
                    channel.handle.trim_start_matches('/')
                )
            }
        })
        .collect();
    SyncState {
        subscriptions,
        watch_history: model.history.clone(),
        settings: Some(model.settings.clone()),
    }
}

/// Deterministic, additive merge for personal state arriving from devices.
/// Automerge remains the collaboration authority after this is persisted by
/// `TubeApp`; transport/device metadata is never inserted into that document.
pub fn merge_remote_state(model: &mut TubeModel, remote: &SyncState) -> bool {
    let mut changed = false;
    let remote_channels = remote
        .subscriptions
        .iter()
        .filter_map(|url| url.split('/').filter(|part| !part.is_empty()).next_back())
        .collect::<HashSet<_>>();
    for channel in &model.channels {
        if remote_channels.contains(channel.id.as_str())
            || remote_channels.contains(channel.handle.trim_start_matches('@'))
            || remote_channels.contains(channel.handle.as_str())
        {
            if !model.subscriptions.contains(&channel.id) {
                model.subscriptions.push(channel.id.clone());
                changed = true;
            }
        }
    }
    for remote_id in &remote.watch_history {
        let id = remote_id
            .split(['/', '='])
            .filter(|part| !part.is_empty())
            .next_back()
            .unwrap_or(remote_id);
        if model.video(id).is_some() && !model.history.iter().any(|local| local == id) {
            model.history.push(id.to_string());
            changed = true;
        }
    }
    if let Some(settings) = &remote.settings
        && &model.settings != settings
    {
        model.settings = settings.clone();
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(id: &str) -> PeerDescriptor {
        PeerDescriptor {
            public_key: id.into(),
            addresses: vec!["127.0.0.1".into()],
            port: 12315,
        }
    }

    #[test]
    fn peer_store_survives_restart_and_keeps_revocation_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let store = PeerStore::new(dir.path().join("peers.json"));
        let record = PeerRecord {
            id: "public-key".into(),
            name: "Phone".into(),
            descriptor: descriptor("public-key"),
            approved_at: 7,
            pending_approval: false,
            revoked_at: Some(9),
            last_sync_at: None,
            last_error: None,
        };
        store.save(std::slice::from_ref(&record)).unwrap();
        assert_eq!(store.load().unwrap(), vec![record]);
        let text = fs::read_to_string(dir.path().join("peers.json")).unwrap();
        assert!(!text.contains("private"));
        assert!(!text.contains("pairingCode"));
    }

    #[test]
    fn independently_derived_device_keys_are_stable_and_distinct() {
        let a = DeviceKey::from_private(&[1; 32]).unwrap();
        let a_restart = DeviceKey::from_private(&[1; 32]).unwrap();
        let b = DeviceKey::from_private(&[2; 32]).unwrap();
        assert_eq!(a.public_base64(), a_restart.public_base64());
        assert_ne!(a.public_base64(), b.public_base64());
    }

    #[test]
    fn collaborative_merge_is_additive_and_never_adds_unknown_entities() {
        let mut model = crate::sample::model();
        let channel = model.channels[0].clone();
        let video = model.videos[0].clone();
        model.subscriptions.clear();
        model.history.clear();
        let remote = SyncState {
            subscriptions: vec![format!("https://youtube.com/{}", channel.handle)],
            watch_history: vec![video.id.clone(), "unknown-secret-looking-value".into()],
            settings: None,
        };
        assert!(merge_remote_state(&mut model, &remote));
        assert_eq!(model.subscriptions, vec![channel.id]);
        assert_eq!(model.history, vec![video.id]);
    }
}
