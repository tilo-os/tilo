//! Product lifecycle around the Grayjay wire transport.
//!
//! The Noise key is deterministically derived from the app-scoped secret
//! returned by the desktop secret portal. Only public peer routes and durable
//! revocation tombstones are written to disk. All network work is serialized on
//! one bounded worker thread so redraws never dial a phone and shutdown is
//! bounded by the transport's socket deadlines.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use ui::time::{Duration, UNIX_EPOCH};

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
    ui::time::wall()
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
        let persistence = |error: tilo_files::FsError| SyncError::Persistence(error.to_string());
        let (dir, name) = tilo_files::file_at(&self.path).map_err(persistence)?;
        let metadata = match tilo_files::blocking::metadata(&dir, &name) {
            Ok(metadata) => metadata,
            Err(tilo_files::FsError::NotFound) => return Ok(Vec::new()),
            Err(error) => return Err(persistence(error)),
        };
        if metadata.kind != tilo_files::Kind::File || metadata.len > MAX_STORE_BYTES {
            return Err(SyncError::Persistence("invalid peer store".into()));
        }
        let bytes = tilo_files::blocking::read(&dir, &name).map_err(persistence)?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err(SyncError::Persistence("invalid peer store".into()));
        }
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
        let bytes = serde_json::to_vec_pretty(&PeerFile {
            version: STORE_VERSION,
            peers: peers.to_vec(),
        })
        .map_err(|error| SyncError::Persistence(error.to_string()))?;
        tilo_files::file_at(&self.path)
            .and_then(|(dir, name)| tilo_files::blocking::write_private(&dir, &name, &bytes))
            .map_err(|error| SyncError::Persistence(error.to_string()))
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

struct WorkerContext {
    transport: Box<dyn SyncTransport>,
    store: PeerStore,
    public_key: String,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
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
            summary: "One device is approved and one is waiting for your approval.".into(),
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
        let worker_public_key = public_key;
        let spawned = ui::rt::spawn_thread("tube-grayjay-sync", move || {
            worker(WorkerContext {
                transport,
                store,
                public_key: worker_public_key,
                commands: command_rx,
                events: event_tx,
            });
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

    /// Drain worker updates without mutating the render model.
    ///
    /// A pulled package is intentionally returned to `TubeApp`, which imports
    /// it through its canonical `TubeAction`/`ActionHost` boundary. Writing a
    /// remote package directly into `TubeModel` made the next Automerge
    /// reconciliation overwrite it, and made the device-only change invisible
    /// to activity history, collaborators, and a restart.
    ///
    /// The bool reports public status/fingerprint changes; the vector contains
    /// personal state that still needs canonical import.
    pub fn pump(&mut self) -> (bool, Vec<SyncState>) {
        let mut changed = false;
        let mut pulled = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::View(view) => {
                    if self.view != view {
                        self.view = view;
                        changed = true;
                    }
                }
                Event::Pulled(remote) => pulled.push(remote),
            }
        }
        (changed, pulled)
    }
}

impl Drop for DeviceSync {
    fn drop(&mut self) {
        let _ = self.try_send(Command::Stop);
    }
}

/// Tube's namespace in the system secret store, and the random seed its
/// Grayjay device key derives from.
const SECRETS_APP: &str = "so.tilo.tube";
const DEVICE_SEED: &str = "grayjay-device-seed";

/// This device's seed from the system secret store, created on first use.
fn device_seed() -> SyncResult<Zeroizing<Vec<u8>>> {
    let unavailable =
        |error: tilo_secrets::SecretError| SyncError::KeyUnavailable(error.to_string());
    let store = tilo_secrets::SecretStore::for_app(SECRETS_APP);
    if store
        .get_blocking(DEVICE_SEED)
        .map_err(unavailable)?
        .is_none()
    {
        let mut seed = Zeroizing::new(vec![0_u8; 32]);
        getrandom::fill(&mut seed)
            .map_err(|error| SyncError::KeyUnavailable(format!("no randomness: {error}")))?;
        let label = Some("Tube Grayjay device key".to_owned());
        store
            .put_blocking(DEVICE_SEED, &seed, tilo_secrets::PutOptions { label })
            .map_err(unavailable)?;
    }
    // Read back what the store kept, which a concurrent first run may have
    // replaced.
    let seed = store
        .get_blocking(DEVICE_SEED)
        .map_err(unavailable)?
        .ok_or_else(|| SyncError::KeyUnavailable("the secret store lost the device key".into()))?;
    if seed.expose().len() < 16 {
        return Err(SyncError::KeyUnavailable(
            "the secret store returned insufficient key material".into(),
        ));
    }
    Ok(Zeroizing::new(seed.expose().to_vec()))
}

fn production_parts(device_name: &str) -> SyncResult<(GrayjaySyncTransport, PeerStore, String)> {
    let seed = device_seed()?;
    let mut digest = Sha256::new();
    digest.update(b"tilo.tube.grayjay-noise-static-v1\0");
    digest.update(seed.as_slice());
    let private = Zeroizing::new(digest.finalize().to_vec());
    let key = DeviceKey::from_private(private.as_slice())?;
    let public_key = key.public_base64();
    let transport = GrayjaySyncTransport::with_device(key, device_name);
    let base = tilo_paths::env_dir("XDG_STATE_HOME")
        .or_else(tilo_paths::state_dir)
        .ok_or_else(|| SyncError::Persistence("no private state directory is available".into()))?;
    Ok((
        transport,
        PeerStore::new(base.join("tilo-tube").join("grayjay-peers.json")),
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

fn worker(context: WorkerContext) {
    let WorkerContext {
        transport,
        store,
        public_key,
        commands,
        events,
    } = context;
    let peers = match store.load() {
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
    let mut worker = SyncWorker {
        transport,
        store,
        public_key,
        events,
        peers,
        latest_state: None,
    };
    if !worker.restore_active_peers() {
        return;
    }
    worker.publish_ready();
    worker.run(&commands);
}

struct SyncWorker {
    transport: Box<dyn SyncTransport>,
    store: PeerStore,
    public_key: String,
    events: mpsc::Sender<Event>,
    peers: Vec<PeerRecord>,
    latest_state: Option<SyncState>,
}

impl SyncWorker {
    fn publish(&self, status: SyncStatus, summary: impl Into<String>) {
        publish(&self.events, status, summary, &self.public_key, &self.peers);
    }
    fn restore_active_peers(&mut self) -> bool {
        for peer in self.peers.iter().filter(|peer| peer.is_active()) {
            if let Err(error) = self.transport.restore_peer(&peer.descriptor) {
                self.publish(
                    SyncStatus::Error,
                    format!("Could not restore {}: {error}", peer.name),
                );
                return false;
            }
        }
        true
    }
    fn publish_ready(&self) {
        self.publish(
            SyncStatus::Ready,
            if self.peers.iter().any(PeerRecord::is_active) {
                "Approved devices are ready to sync."
            } else {
                "Paste a Grayjay pairing invite to add your first device."
            },
        );
    }
    fn run(&mut self, commands: &mpsc::Receiver<Command>) {
        loop {
            let command = match commands.recv_timeout(AUTO_SYNC_INTERVAL) {
                Ok(command) => command,
                Err(mpsc::RecvTimeoutError::Timeout) => match self.latest_state.clone() {
                    Some(state) => Command::Sync(state),
                    None => continue,
                },
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            if !self.handle(command) {
                return;
            }
        }
    }
    fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::Stop => false,
            Command::Pair(invite) => {
                self.pair(&invite);
                true
            }
            Command::Approve(id) => {
                self.approve(&id);
                true
            }
            Command::Revoke(id) => {
                self.revoke(&id);
                true
            }
            Command::Sync(state) => {
                self.sync(&state);
                true
            }
        }
    }
    fn pair(&mut self, invite: &Zeroizing<String>) {
        self.publish(
            SyncStatus::Pairing,
            "Waiting for approval on the other device…",
        );
        let result = self
            .transport
            .pair(invite.as_str())
            .and_then(|id| self.persist_paired_peer(&id));
        match result {
            Ok(()) => self.publish(
                SyncStatus::Ready,
                "Device authenticated. Review its fingerprint and approve access.",
            ),
            Err(error) => self.publish(SyncStatus::Error, error.to_string()),
        }
    }
    fn persist_paired_peer(&mut self, id: &PeerId) -> SyncResult<()> {
        if self
            .peers
            .iter()
            .any(|peer| peer.id == id.0 && !peer.is_active())
        {
            self.transport.forget_peer(id);
            return Err(SyncError::Revoked);
        }
        let descriptor = self.transport.peer_descriptor(id).ok_or_else(|| {
            SyncError::Persistence("transport did not expose a public peer route".into())
        })?;
        let mut candidate = self.peers.clone();
        if let Some(existing) = candidate.iter_mut().find(|peer| peer.id == id.0) {
            existing.descriptor = descriptor;
            existing.pending_approval = true;
            existing.last_error = None;
        } else {
            if candidate.len() >= MAX_PEERS {
                self.transport.forget_peer(id);
                return Err(SyncError::Persistence(
                    "the approved-device limit has been reached".into(),
                ));
            }
            candidate.push(PeerRecord {
                id: id.0.clone(),
                name: format!("Grayjay {}", id.0.chars().take(8).collect::<String>()),
                descriptor,
                approved_at: now(),
                pending_approval: true,
                revoked_at: None,
                last_sync_at: None,
                last_error: None,
            });
        }
        self.store.save(&candidate)?;
        self.peers = candidate;
        Ok(())
    }
    fn approve(&mut self, id: &str) {
        let mut candidate = self.peers.clone();
        let Some(peer) = candidate
            .iter_mut()
            .find(|peer| peer.id == id && peer.revoked_at.is_none())
        else {
            return;
        };
        peer.pending_approval = false;
        peer.approved_at = now();
        let descriptor = peer.descriptor.clone();
        match self
            .store
            .save(&candidate)
            .and_then(|()| self.transport.restore_peer(&descriptor))
        {
            Ok(()) => {
                self.peers = candidate;
                self.publish(SyncStatus::Ready, "Device approved. Sync is ready.");
            }
            Err(error) => self.publish(SyncStatus::Error, error.to_string()),
        }
    }
    fn revoke(&mut self, id: &str) {
        let mut candidate = self.peers.clone();
        let Some(peer) = candidate.iter_mut().find(|peer| peer.id == id) else {
            return;
        };
        peer.revoked_at = Some(now());
        peer.last_error = None;
        match self.store.save(&candidate) {
            Ok(()) => {
                self.peers = candidate;
                self.transport.forget_peer(&PeerId(id.to_string()));
                self.publish(
                    SyncStatus::Ready,
                    "Device revoked. Its key can no longer sync.",
                );
            }
            Err(error) => self.publish(SyncStatus::Error, error.to_string()),
        }
    }
    fn sync(&mut self, state: &SyncState) {
        self.latest_state = Some(state.clone());
        self.publish(SyncStatus::Syncing, "Syncing approved devices…");
        let active = self
            .peers
            .iter()
            .filter(|peer| peer.is_active())
            .map(|peer| PeerId(peer.id.clone()))
            .collect::<Vec<_>>();
        let mut pulled = SyncState::default();
        let mut failures = 0;
        for id in active {
            if let Err(error) = self.sync_peer(&id, state, &mut pulled) {
                self.mark_error(&id.0, &error);
                failures += 1;
            }
        }
        if let Err(error) = self.store.save(&self.peers) {
            failures += 1;
            if let Some(peer) = self.peers.first_mut() {
                peer.last_error = Some(error.to_string());
            }
        }
        if pulled != SyncState::default() {
            let _ = self.events.send(Event::Pulled(pulled));
        }
        self.publish(
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
        );
    }
    fn sync_peer(
        &mut self,
        id: &PeerId,
        state: &SyncState,
        pulled: &mut SyncState,
    ) -> SyncResult<()> {
        merge_sync_states(pulled, self.transport.pull(id)?);
        self.transport.push(id, state)?;
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.id == id.0) {
            peer.last_sync_at = Some(now());
            peer.last_error = None;
        }
        Ok(())
    }
    fn mark_error(&mut self, id: &str, error: &SyncError) {
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.id == id) {
            peer.last_error = Some(error.to_string());
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
        .filter_map(|id| {
            // A paired device can know about a creator before this device has
            // fetched its catalogue record. Preserve the vetted external URL
            // verbatim so the subscription round-trips rather than silently
            // disappearing from the next package we push.
            model.channel(id).map_or_else(
                || super::normalized_subscription_url(id),
                |channel| {
                    if channel.handle.starts_with("http") {
                        super::normalized_subscription_url(&channel.handle)
                    } else {
                        Some(format!(
                            "https://youtube.com/{}",
                            channel.handle.trim_start_matches('/')
                        ))
                    }
                },
            )
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
        .filter_map(|url| url.split('/').rfind(|part| !part.is_empty()))
        .collect::<HashSet<_>>();
    for channel in &model.channels {
        if (remote_channels.contains(channel.id.as_str())
            || remote_channels.contains(channel.handle.trim_start_matches('@'))
            || remote_channels.contains(channel.handle.as_str()))
            && !model.subscriptions.contains(&channel.id)
        {
            model.subscriptions.push(channel.id.clone());
            changed = true;
        }
    }
    for remote_id in &remote.watch_history {
        let id = remote_id
            .split(['/', '='])
            .rfind(|part| !part.is_empty())
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
    use std::fs;

    fn descriptor(id: &str) -> PeerDescriptor {
        PeerDescriptor {
            public_key: id.into(),
            addresses: vec!["127.0.0.1".into()],
            port: 12315,
        }
    }

    #[test]
    fn device_sync_lifecycle_persists_keys_peers_and_merges_additively() {
        // Case `peer_store_survives_restart_and_keeps_revocation_tombstone`.
        {
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
        // Case `independently_derived_device_keys_are_stable_and_distinct`.
        {
            let a = DeviceKey::from_private(&[1; 32]).unwrap();
            let a_restart = DeviceKey::from_private(&[1; 32]).unwrap();
            let b = DeviceKey::from_private(&[2; 32]).unwrap();
            assert_eq!(a.public_base64(), a_restart.public_base64());
            assert_ne!(a.public_base64(), b.public_base64());
        }
        // Case `collaborative_merge_is_additive_and_never_adds_unknown_entities`.
        {
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
}
