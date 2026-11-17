//! The mounted workspace tree, published as the Files app's records.
//!
//! The tree is kept as several records ([`fs_protocol::tree`]): its own record, chunk 0, which
//! names the root and the chunks that hold its nodes, and one record per chunk. This module
//! joins each record's route on the session and publishes it: a device that mounts a tree
//! gives the session every record it holds — starting each from the empty genesis a record
//! nobody has yet is founded with — and every later change as an increment of the one record
//! it changed. A device that mounts nothing yet is given the records whole by one that holds
//! them, chunk 0 first, and joins each chunk the tree names.
//!
//! The filesystem daemon is the Files app's windowless endpoint, admitted by the session against
//! the launch it recorded for this process. A workspace that seals its records hands this
//! endpoint each record's content keys, and each change is sealed or opened under them.

use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result, bail};
use automerge::ChangeHash;
use collab_types::document_transfer::{DocumentFrame, DocumentReassembler};
use collab_types::{
    ContentRetention, DocumentMutation, EncryptionMetadata, OpKind, SignedOp, document_artifact_id,
    document_attestation_id, next_unordered_document_sequence, sign_op,
};
use fs_protocol::TreeChunk;
use key_group::ContentKeys;
use runtime_collab::{CertifiedJoinIdentity, ChannelHandle, CollabClient, OpEvent};
use tilo_identity_client::BrokerSigningIdentity;
use tilo_identity_client::wire::{
    HANDSHAKE_FRAMING, Handshake, KeyAccess, PRESENTATION_VERSION, PresentationHandshake,
};
use tilo_lock::MutexRecover as _;

use crate::identity::PeerCredentials;
use crate::namespace::{Namespace, ResourceWrite};

/// The app whose person record a workspace's tree is: it declares the kind, and the daemon is its
/// endpoint.
pub use fs_protocol::tree::FILES_APP;

/// The record a workspace's tree is, as the device that publishes it names it.
#[must_use]
pub fn record() -> String {
    fs_protocol::tree::document_id()
}

/// How long the writer waits before dialing the session again after its connection ended: the session
/// may restart under a daemon that outlives it.
const RECONNECT: Duration = Duration::from_secs(2);

/// The most changes of a sealed record this device holds unopened while it waits for the keys. Past it
/// the record is read whole instead, which settles every change at once.
const MAX_UNOPENED: usize = 16;

/// Publish `namespace` on `cluster` as the Files app's records, for as long as the daemon serves it, on
/// the collaboration socket at `socket`. The identity service at `identity` is where a workspace that
/// seals its records hands this endpoint their content keys.
///
/// The writer rejoins whenever that connection ends; a session that is not up yet is waited for, since
/// the daemon starts beside it.
#[must_use = "the handle is how a daemon stops publishing before it unmounts"]
pub fn spawn(
    namespace: Arc<Mutex<Namespace>>,
    cluster: String,
    credentials: PeerCredentials,
    socket: PathBuf,
    identity: PathBuf,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match serve_once(&namespace, &cluster, &credentials, &socket, &identity).await {
                Ok(()) => tracing::debug!(cluster, "the workspace tree's record connections ended"),
                Err(error) => {
                    tracing::warn!(cluster, ?error, "the workspace tree is not published");
                }
            }
            tokio::time::sleep(RECONNECT).await;
        }
    })
}

/// One connection to the session, joined to every record of `namespace` until the connection ends.
async fn serve_once(
    namespace: &Arc<Mutex<Namespace>>,
    cluster: &str,
    credentials: &PeerCredentials,
    path: &Path,
    identity: &Path,
) -> Result<()> {
    let mut stream = tilo_files::native::socket::connect(path)
        .with_context(|| format!("connect to the session at {}", path.display()))?;
    // The app and workspace, as the first thing on the connection: what the
    // session attests against the launch it recorded for this process.
    runtime_portal::launch_handoff::write(&mut stream, FILES_APP, cluster)
        .context("name this device's app on the session's connection")?;
    stream
        .set_nonblocking(true)
        .context("the session connection is ready for the runtime")?;
    let stream = tokio::net::UnixStream::from_std(stream).context("a runtime stream")?;
    let (reader, writer) = stream.into_split();
    let (client, carrying) = CollabClient::over(reader, writer);
    tokio::spawn(carrying);

    let mut publisher = TreePublisher::connect(&client, cluster, credentials, identity).await?;
    // The session's streams of each record reach this loop as the events they are, so a record
    // this device joins later starts its streams too.
    let (tell, mut told) = tokio::sync::mpsc::unbounded_channel();
    let mut changes = namespace.lock_recover().subscribe_changes();
    publisher.join_records(namespace, &tell).await?;
    // What this device holds goes out at once: a device that mounts a tree is a holder of its
    // records before anything changes them.
    publisher.publish(namespace, &tell).await?;
    loop {
        tokio::select! {
            moved = changes.changed() => {
                if moved.is_err() {
                    return Ok(());
                }
                publisher.publish(namespace, &tell).await?;
            }
            event = told.recv() => {
                let Some(event) = event else {
                    return Ok(());
                };
                publisher.handle(event, namespace, &tell).await?;
            }
        }
    }
}

/// Whether `cluster` is the store of the person `root` belongs to: records there are kept in the store
/// and reach their other devices over the link those devices prove themselves to each other on, so no
/// workspace channel seals them.
fn in_person_store(root: &[u8; 32], cluster: &str) -> bool {
    uuid::Uuid::parse_str(cluster)
        .is_ok_and(|cluster| collab_types::is_personal_cluster(root, &cluster))
}

/// One record of the tree, as it reaches this device: a change another device published, the
/// session asking a holder for the record whole, its keys moving on, or the connection ending.
enum FromRecord {
    Change { chunk: TreeChunk, event: OpEvent },
    Asked { chunk: TreeChunk },
    Keys { chunk: TreeChunk },
    Ended,
}

enum Open {
    Opened(Vec<u8>),
    Later,
    Never,
}

/// The publisher of every record of one tree: the session's connection, this device's identity and
/// the keys of each record, and one channel per record.
struct TreePublisher {
    cluster: String,
    client: CollabClient,
    credentials: PeerCredentials,
    /// This device's proof of its identity on every record's route.
    presentation: Vec<u8>,
    /// The records this device is joined to: the tree's own (0) and each chunk.
    records: BTreeMap<TreeChunk, RecordPublisher>,
    /// Where this device asks for a record's content keys, when the workspace seals its records.
    identity: PathBuf,
    signer: Option<Arc<BrokerSigningIdentity>>,
}

/// This device's side of one record of the tree: its channel, what it published of the record, and
/// the keys its changes are sealed under.
struct RecordPublisher {
    /// The record's id, as the session names the channel it travels on.
    record: String,
    handle: ChannelHandle,
    /// The record's heads this device published: every change after them is the next increment.
    /// Empty until the record's content was published, which is how its first publication goes out.
    sent: Vec<ChangeHash>,
    /// Whether this device founded the record, with the empty genesis every record starts from.
    founded: bool,
    reassembler: DocumentReassembler,
    /// What this device may do with the record's content: seal it, wait for its keys, or write it
    /// unsealed.
    access: Access,
    keys: Option<ContentKeys>,
    /// The service generation the keys were answered at: what this device asks from when it asks
    /// again, so an unchanged answer means no new keys.
    generation: u64,
    /// Changes of a sealed record that arrived before its keys did, in the order they came.
    unopened: Vec<SignedOp>,
    /// Changes of the record that arrived before the record did, in the order they came: they
    /// change the record a holder publishes, and apply once this device holds it.
    waiting_for_the_record: Vec<SignedOp>,
}

/// What this device may do with one record's content.
///
/// A workspace that seals its records holds every change until it hands the keys over, as an app of
/// the workspace holds its edits. The person's own cluster seals nothing: its records are kept in the
/// person's store and reach their other devices over the link those devices authenticate with.
#[derive(Clone, Copy)]
enum Access {
    /// The workspace hands this endpoint the record's keys: every change it makes is sealed under
    /// them, and every change that arrives must open.
    Sealed,
    /// Its keys are not here yet: the record waits for them rather than going out in the clear into
    /// a workspace that seals.
    Waiting,
    /// The record is the person's own, kept in their store: no workspace channel seals it.
    Unsealed,
}

impl TreePublisher {
    /// The connection to the session as this device's endpoint for `cluster`'s tree.
    async fn connect(
        client: &CollabClient,
        cluster: &str,
        credentials: &PeerCredentials,
        identity: &Path,
    ) -> Result<Self> {
        let presentation = CertifiedJoinIdentity {
            workspace_id: cluster.to_owned(),
            device_certificate: serde_json::to_vec(credentials.cert())
                .context("this device's certificate is JSON")?,
            proof: credentials
                .signer()
                .sign(tilo_identity_client::collaboration_join_signing_bytes(
                    cluster,
                ))
                .await
                .context("this device signs its join")?
                .to_vec(),
        }
        .encode()
        .context("this device's join presentation encodes")?;
        Ok(Self {
            cluster: cluster.to_owned(),
            client: client.clone(),
            credentials: credentials.clone(),
            presentation,
            records: BTreeMap::new(),
            identity: identity.to_path_buf(),
            signer: None,
        })
    }

    /// Join every record the tree holds or lists that this device is not joined to yet, and ask a
    /// holder for the whole of each one this device holds nothing of.
    async fn join_records(
        &mut self,
        namespace: &Arc<Mutex<Namespace>>,
        tell: &tokio::sync::mpsc::UnboundedSender<FromRecord>,
    ) -> Result<()> {
        let (records, held, holds_tree) = {
            let namespace = namespace.lock_recover();
            (
                namespace.listed_records(),
                namespace.records(),
                namespace.holds_tree(),
            )
        };
        for chunk in records {
            if self.records.contains_key(&chunk) {
                continue;
            }
            self.join_record(chunk, tell).await?;
            let holds = if chunk == 0 {
                holds_tree
            } else {
                held.contains(&chunk)
            };
            if !holds {
                // This device holds nothing of the record: a device that holds it gives it whole.
                self.ask_for(chunk).await;
            }
        }
        Ok(())
    }

    /// Join `chunk`'s channel and start following its changes, its asks and its keys.
    async fn join_record(
        &mut self,
        chunk: TreeChunk,
        tell: &tokio::sync::mpsc::UnboundedSender<FromRecord>,
    ) -> Result<()> {
        let record = fs_protocol::tree::chunk_document_id(chunk);
        let channel = runtime_collab::route::channel_key::format(FILES_APP, &self.cluster, &record);
        let handle = self
            .client
            .join_channel_as_kind(
                &channel,
                &self.presentation,
                "",
                "person",
                fs_protocol::tree::TREE_KIND,
            )
            .await
            .with_context(|| format!("the session admits this device to {record}"))?;
        let events = handle
            .subscribe()
            .await
            .with_context(|| format!("the session publishes {record}'s changes"))?;
        let context = handle.subscribe_context().await.with_context(|| {
            format!("the session tells this device what it is asked for of {record}")
        })?;
        forward_changes(chunk, events, tell.clone());
        forward_asks(chunk, context, tell.clone());
        // A workspace's keys arrive when a commit brings this device into the record's group,
        // possibly after the join: the record is published, and the changes that waited for them
        // opened, on the change itself.
        if !in_person_store(&self.credentials.cert().user_public_key, &self.cluster) {
            forward_keys(chunk, handle.clone(), tell.clone());
        }
        let access = if in_person_store(&self.credentials.cert().user_public_key, &self.cluster) {
            Access::Unsealed
        } else {
            Access::Waiting
        };
        self.records.insert(
            chunk,
            RecordPublisher {
                record,
                handle,
                sent: Vec::new(),
                founded: false,
                reassembler: DocumentReassembler::default(),
                access,
                keys: None,
                generation: 0,
                unopened: Vec::new(),
                waiting_for_the_record: Vec::new(),
            },
        );
        self.refresh_keys(chunk).await;
        Ok(())
    }

    /// Ask a device that holds `chunk` to publish it whole, as a device joining the record with
    /// nothing of its own does.
    async fn ask_for(&self, chunk: TreeChunk) {
        let Some(record) = self.records.get(&chunk) else {
            return;
        };
        if let Err(error) = record.handle.request_document_snapshot().await {
            tracing::debug!(%error, "a device holding a record of the tree is not asked for it whole");
        }
    }

    /// Take one event of a record and answer it.
    async fn handle(
        &mut self,
        event: FromRecord,
        namespace: &Arc<Mutex<Namespace>>,
        tell: &tokio::sync::mpsc::UnboundedSender<FromRecord>,
    ) -> Result<()> {
        match event {
            FromRecord::Change { chunk, event } => {
                self.receive(chunk, event, namespace, tell).await
            }
            FromRecord::Asked { chunk } => self.publish_whole(chunk, namespace).await,
            FromRecord::Keys { chunk } => self.keys_changed(chunk, namespace, tell).await,
            FromRecord::Ended => bail!("a record of the tree's connection ended"),
        }
    }

    /// Publish what this device changed, record by record: the record whose changes moved since it
    /// last published them. A record this device holds nothing of yet is not published, and neither
    /// is a record whose keys are not here.
    async fn publish(
        &mut self,
        namespace: &Arc<Mutex<Namespace>>,
        tell: &tokio::sync::mpsc::UnboundedSender<FromRecord>,
    ) -> Result<()> {
        self.join_records(namespace, tell).await?;
        let (writes, holds_tree) = {
            let mut namespace = namespace.lock_recover();
            let holds_tree = namespace.holds_tree();
            // What each record's holders have of it, as this device published it: every change
            // after these heads is the next increment.
            let known: BTreeMap<TreeChunk, Vec<ChangeHash>> = self
                .records
                .iter()
                // The tree's own record carries a root identity this device founds on its own; it
                // goes out once the tree does, so a device that mounts nothing publishes nothing.
                .filter(|(chunk, _)| **chunk != 0 || holds_tree)
                .map(|(chunk, record)| (*chunk, record.sent.clone()))
                .collect();
            (namespace.document_writes(&known), holds_tree)
        };
        for write in writes {
            if write.chunk == 0 && !holds_tree {
                continue;
            }
            let Some(record) = self.records.get(&write.chunk) else {
                continue;
            };
            if !record.keys_here() {
                continue;
            }
            self.publish_write(write).await?;
        }
        Ok(())
    }

    /// Publish one record's changes: the empty genesis first for a record nobody has yet, then the
    /// changes themselves as an increment.
    async fn publish_write(&mut self, write: ResourceWrite) -> Result<()> {
        let chunk = write.chunk;
        let founded = self
            .records
            .get(&chunk)
            .is_some_and(|record| record.founded);
        if !founded {
            // A record is founded empty: its first publication is the empty genesis every device
            // of a record starts from, which a host that has the record already takes as nothing.
            self.send(
                chunk,
                DocumentMutation::Snapshot(automerge::AutoCommit::new().save()),
            )
            .await?;
            if let Some(record) = self.records.get_mut(&chunk) {
                record.founded = true;
            }
        }
        if !write.changes.is_empty() {
            self.send(chunk, DocumentMutation::Incremental(write.changes))
                .await?;
        }
        if let Some(record) = self.records.get_mut(&chunk) {
            record.sent = write.heads;
        }
        Ok(())
    }

    /// Publish `chunk` whole: what the record holds from this device from now on, and what a
    /// device joining it gets.
    async fn publish_whole(
        &mut self,
        chunk: TreeChunk,
        namespace: &Arc<Mutex<Namespace>>,
    ) -> Result<()> {
        let Some(record) = self.records.get(&chunk) else {
            return Ok(());
        };
        if !record.keys_here() {
            return Ok(());
        }
        let (bytes, heads) = {
            let mut namespace = namespace.lock_recover();
            // A device that holds no tree of its own has no tree to give: only a holder answers.
            if chunk == 0 && !namespace.holds_tree() {
                return Ok(());
            }
            let Some(bytes) = namespace.save_record(chunk) else {
                return Ok(());
            };
            (bytes, namespace.record_heads())
        };
        self.send(chunk, DocumentMutation::Snapshot(bytes)).await?;
        if let Some(record) = self.records.get_mut(&chunk) {
            record.founded = true;
            record.sent = heads.get(&chunk).cloned().unwrap_or_default();
        }
        Ok(())
    }

    /// `chunk`'s keys changed: take them, open the changes that waited with them, and publish what
    /// this device holds of the record now that it may.
    async fn keys_changed(
        &mut self,
        chunk: TreeChunk,
        namespace: &Arc<Mutex<Namespace>>,
        tell: &tokio::sync::mpsc::UnboundedSender<FromRecord>,
    ) -> Result<()> {
        self.refresh_keys(chunk).await;
        if !matches!(
            self.records.get(&chunk).map(|record| record.access),
            Some(Access::Sealed)
        ) {
            return Ok(());
        }
        self.take_unopened(chunk, namespace, tell).await?;
        self.publish(namespace, tell).await
    }

    /// Take one change another device published of `chunk`, and publish what resolving it wrote.
    async fn receive(
        &mut self,
        chunk: TreeChunk,
        event: OpEvent,
        namespace: &Arc<Mutex<Namespace>>,
        tell: &tokio::sync::mpsc::UnboundedSender<FromRecord>,
    ) -> Result<()> {
        let op: SignedOp = postcard::from_bytes(&event.op).context("a signed change decodes")?;
        if op.author.as_bytes() == &self.credentials.device_public_key() {
            return Ok(());
        }
        let Some(record) = self.records.get_mut(&chunk) else {
            return Ok(());
        };
        record.unopened.push(op);
        if record.unopened.len() > MAX_UNOPENED {
            // The keys still are not here: read the record whole from a holder instead of holding
            // changes this device cannot open.
            record.unopened.clear();
            self.ask_for(chunk).await;
            return Ok(());
        }
        self.take_unopened(chunk, namespace, tell).await
    }

    /// Apply every change of `chunk` this device holds the keys for, in the order they arrived,
    /// asking for the record's keys once when one of them needs them.
    async fn take_unopened(
        &mut self,
        chunk: TreeChunk,
        namespace: &Arc<Mutex<Namespace>>,
        tell: &tokio::sync::mpsc::UnboundedSender<FromRecord>,
    ) -> Result<()> {
        // A change that lands may bring the record changes that waited for it build on, so the
        // pass runs while it applies something.
        while self.apply_unopened(chunk, namespace, tell).await? {}
        Ok(())
    }

    /// Apply the changes of `chunk` whose keys are here, in the order they arrived; the rest wait
    /// for the keys that open them. Whether any change was applied or dropped.
    async fn apply_unopened(
        &mut self,
        chunk: TreeChunk,
        namespace: &Arc<Mutex<Namespace>>,
        tell: &tokio::sync::mpsc::UnboundedSender<FromRecord>,
    ) -> Result<bool> {
        let mut applied = false;
        let mut asked = false;
        loop {
            let mut waiting = Vec::new();
            while let Some(op) = self
                .records
                .get(&chunk)
                .and_then(|record| record.unopened.first().cloned())
            {
                match self.opened(chunk, &op) {
                    Open::Opened(payload) => {
                        if let Some(record) = self.records.get_mut(&chunk) {
                            record.unopened.remove(0);
                        }
                        applied = true;
                        self.apply(chunk, &op, payload, namespace, tell).await?;
                    }
                    Open::Later => {
                        if let Some(record) = self.records.get_mut(&chunk) {
                            waiting.push(record.unopened.remove(0));
                        }
                    }
                    Open::Never => {
                        if let Some(record) = self.records.get_mut(&chunk) {
                            record.unopened.remove(0);
                        }
                        applied = true;
                        tracing::debug!("a change handed to this device is not the tree's to open");
                    }
                }
            }
            let Some(record) = self.records.get_mut(&chunk) else {
                return Ok(applied);
            };
            if waiting.is_empty() {
                return Ok(applied);
            }
            record.unopened = waiting;
            if asked {
                return Ok(applied);
            }
            // The keys this device holds may have moved on since the changes arrived, as a workspace
            // that re-keys hands out new ones: ask once, and open what they reach.
            asked = true;
            self.refresh_keys(chunk).await;
        }
    }

    fn opened(&self, chunk: TreeChunk, op: &SignedOp) -> Open {
        let Some(record) = self.records.get(&chunk) else {
            return Open::Never;
        };
        match (&op.encryption, record.access) {
            (None, Access::Unsealed) => Open::Opened(op.payload.clone()),
            (_, Access::Waiting) => Open::Later,
            // A change that does not go as this record's route does is another workspace's:
            // cleartext into a workspace that seals is the downgrade the check refuses, and sealed
            // content on a route with no keys to open it is not this tree's.
            (None, Access::Sealed) | (Some(_), Access::Unsealed) => Open::Never,
            (Some(metadata), Access::Sealed) => {
                let Some(keys) = &record.keys else {
                    return Open::Later;
                };
                if !keys.holds_key_for(metadata) {
                    // Sealed under an epoch this device's keys have moved past: newer keys open it.
                    return Open::Later;
                }
                let artifact = document_artifact_id(FILES_APP, &self.cluster, &record.record);
                match keys.open_content(artifact, metadata, &op.payload) {
                    Ok(opened) => Open::Opened(opened.to_vec()),
                    Err(_) => {
                        // Sealed under a key from another branch of the record's group: the ones
                        // that open it are newer than these.
                        Open::Later
                    }
                }
            }
        }
    }

    async fn apply(
        &mut self,
        chunk: TreeChunk,
        op: &SignedOp,
        payload: Vec<u8>,
        namespace: &Arc<Mutex<Namespace>>,
        tell: &tokio::sync::mpsc::UnboundedSender<FromRecord>,
    ) -> Result<()> {
        let Some(mutation) = self.mutation_of(chunk, op, &payload)? else {
            return Ok(());
        };
        if let DocumentMutation::Incremental(bytes) = &mutation
            && !applies(chunk, namespace, bytes)?
        {
            // A change of a record this device was never given waits for the whole of it rather
            // than merging one root beside another.
            let over = self
                .records
                .get(&chunk)
                .is_some_and(|record| record.waiting_for_the_record.len() > MAX_UNOPENED);
            if let Some(record) = self.records.get_mut(&chunk) {
                if over {
                    // The record is not coming: this device reads it whole instead.
                    record.waiting_for_the_record.clear();
                }
                record.waiting_for_the_record.push(op.clone());
            }
            self.ask_for(chunk).await;
            return Ok(());
        }
        let (resolutions, heads) = {
            let (bytes, whole) = match mutation {
                DocumentMutation::Snapshot(bytes) => (bytes, true),
                DocumentMutation::Incremental(bytes) => (bytes, false),
            };
            let mut namespace = namespace.lock_recover();
            let resolutions = namespace
                .take_records_and_resolve(vec![(chunk, bytes)])
                .with_context(|| {
                    if whole {
                        "a published record becomes this device's"
                    } else {
                        "a published change applies to this device's record"
                    }
                })?;
            (
                resolutions,
                namespace
                    .record_heads()
                    .get(&chunk)
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        // What the record holds now is what the session sent: only the changes settling it are
        // this device's to publish.
        if let Some(record) = self.records.get_mut(&chunk) {
            record.sent = heads;
        }
        for write in resolutions {
            self.publish_write(write).await?;
        }
        // What waited for this record may apply now that the record moved on, in the order it came.
        if let Some(record) = self.records.get_mut(&chunk)
            && !record.waiting_for_the_record.is_empty()
        {
            let mut waited = std::mem::take(&mut record.waiting_for_the_record);
            waited.append(&mut record.unopened);
            record.unopened = waited;
        }
        // The tree's own record may name chunks this device joined nothing of yet.
        self.join_records(namespace, tell).await
    }

    /// The change `payload` carries, once every segment of it arrived; `None` for a segment of a
    /// transfer still arriving.
    fn mutation_of(
        &mut self,
        chunk: TreeChunk,
        op: &SignedOp,
        payload: &[u8],
    ) -> Result<Option<DocumentMutation>> {
        let Some(record) = self.records.get_mut(&chunk) else {
            return Ok(None);
        };
        match DocumentFrame::decode(payload).context("a published change is a document frame")? {
            DocumentFrame::Whole(mutation) => Ok(Some(mutation)),
            DocumentFrame::Segment(segment) => {
                let complete = record
                    .reassembler
                    .accept(op, segment, Instant::now())
                    .context("a segmented change reassembles")?;
                complete
                    .map(|document| {
                        DocumentMutation::decode(&document.payload)
                            .context("a reassembled change is a typed document mutation")
                    })
                    .transpose()
            }
        }
    }

    /// Sign `mutation` as this device and publish every frame of it to record `chunk`'s channel,
    /// sealed under its content keys when the workspace seals its records.
    async fn send(&mut self, chunk: TreeChunk, mutation: DocumentMutation) -> Result<()> {
        let frames = mutation
            .encode_frames()
            .context("the change fits the collaboration wire")?;
        for frame in frames {
            let (payload, encryption) = self.seal(chunk, frame).await?;
            let Some(record) = self.records.get(&chunk) else {
                bail!("the record's channel is not joined");
            };
            let artifact = document_artifact_id(FILES_APP, &self.cluster, &record.record);
            let attestation = document_attestation_id(FILES_APP, &self.cluster, &record.record);
            let now = SystemTime::now();
            let mut op = match encryption {
                Some(metadata) => SignedOp::new_unsigned_with_encryption(
                    payload,
                    artifact,
                    collab_types::Ed25519PublicKey::from_bytes(
                        self.credentials.device_public_key(),
                    ),
                    attestation,
                    OpKind::Change,
                    now,
                    next_unordered_document_sequence(now),
                    metadata,
                ),
                None => SignedOp::new_unsigned(
                    payload,
                    artifact,
                    collab_types::Ed25519PublicKey::from_bytes(
                        self.credentials.device_public_key(),
                    ),
                    attestation,
                    OpKind::Change,
                    now,
                    next_unordered_document_sequence(now),
                ),
            };
            sign_op(&mut op, self.credentials.signer())
                .context("this device signs the change it publishes")?;
            let bytes = postcard::to_stdvec(&op).context("a signed change encodes")?;
            record
                .handle
                .send(&bytes)
                .await
                .context("the session admits this device's change")?;
        }
        Ok(())
    }

    /// `frame` as the session admits it: sealed under record `chunk`'s keys when the workspace
    /// seals its records, as it is otherwise.
    async fn seal(
        &mut self,
        chunk: TreeChunk,
        frame: Vec<u8>,
    ) -> Result<(Vec<u8>, Option<EncryptionMetadata>)> {
        if matches!(
            self.records.get(&chunk).map(|record| record.access),
            Some(Access::Waiting)
        ) {
            // The record's keys may have been handed over since the join.
            self.refresh_keys(chunk).await;
        }
        if matches!(
            self.records.get(&chunk).map(|record| record.access),
            Some(Access::Unsealed)
        ) {
            return Ok((frame, None));
        }
        for attempt in 0..2 {
            let sealed = {
                let Some(record) = self.records.get(&chunk) else {
                    bail!("the record's channel is not joined");
                };
                let Access::Sealed = record.access else {
                    bail!("the workspace has not handed this device the record's keys yet");
                };
                let Some(keys) = &record.keys else {
                    bail!("this device holds no keys of the record's");
                };
                let artifact = document_artifact_id(FILES_APP, &self.cluster, &record.record);
                // A whole record is a checkpoint of it, which is what a device holding none of it
                // takes in first.
                let retention = ContentRetention::of_frame(&frame);
                keys.seal_content(artifact, retention, &frame)
            };
            match sealed {
                Ok((metadata, sealed)) => return Ok((sealed, Some(metadata))),
                Err(key_group::Error::MissingContentKey(_)) if attempt == 0 => {
                    // The channel moved to an epoch this device has not been handed yet: ask for
                    // the keys again and seal under them.
                    self.refresh_keys(chunk).await;
                }
                Err(error) => {
                    return Err(error).context("the change seals under the record's keys");
                }
            }
        }
        bail!("the record's keys do not cover the channel's current epoch")
    }

    /// Ask the session's identity service for record `chunk`'s content keys, at the generation this
    /// device holds: it is answered the keys when they changed, and with none when they did not.
    ///
    /// A service that is not there yet, or has not handed the keys over, keeps them waiting: a
    /// workspace that seals its records never has this tree published in the clear.
    async fn refresh_keys(&mut self, chunk: TreeChunk) {
        let Some(access) = self.records.get(&chunk).map(|record| record.access) else {
            return;
        };
        if matches!(access, Access::Unsealed) {
            return;
        }
        let Some(signer) = self.signer() else {
            return;
        };
        let cluster = self.cluster.clone();
        let record = fs_protocol::tree::chunk_document_id(chunk);
        let generation = self.records.get(&chunk).map_or(0, |held| held.generation);
        let asked_record = record.clone();
        let asked = tokio::task::spawn_blocking(move || {
            signer.group_keys(&cluster, FILES_APP, &asked_record, generation)
        })
        .await;
        let Some(held) = self.records.get_mut(&chunk) else {
            return;
        };
        match asked {
            Ok(Ok(reply)) => match (reply.access, reply.keys.as_deref()) {
                (KeyAccess::Ready, Some(bytes)) => match ContentKeys::from_bytes(bytes) {
                    Ok(keys) => {
                        held.keys = Some(keys);
                        held.generation = reply.generation;
                        held.access = Access::Sealed;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "a tree record's content keys do not decode");
                    }
                },
                // Keys reported unchanged: the ones held still open the record.
                (KeyAccess::Ready, None) => {
                    held.access = if held.keys.is_some() {
                        Access::Sealed
                    } else {
                        Access::Waiting
                    };
                }
                (access, _) => {
                    // Waiting, removed, unavailable, or no secret store: the
                    // workspace seals, and the record waits to be sealed.
                    held.access = Access::Waiting;
                    tracing::debug!(
                        ?access,
                        %record,
                        "the workspace has not handed this device the record's keys yet"
                    );
                }
            },
            Ok(Err(error)) => {
                // The service answered and refused: it may answer differently once this device is
                // admitted, so the record waits rather than going out in the clear.
                tracing::debug!(%error, %record, "the identity service withholds the record's keys");
            }
            Err(error) => {
                tracing::debug!(%error, %record, "the record's keys were not asked for");
            }
        }
    }

    /// This device's connection to the session's identity service, bound to the workspace whose tree
    /// it publishes, as an app's connection is; opened once for every record of the tree.
    fn signer(&mut self) -> Option<Arc<BrokerSigningIdentity>> {
        if let Some(signer) = &self.signer {
            return Some(Arc::clone(signer));
        }
        match self.connect_identity() {
            Ok(signer) => {
                let signer = Arc::new(signer);
                self.signer = Some(Arc::clone(&signer));
                Some(signer)
            }
            Err(error) => {
                // Not there yet, or refused this device: the workspace seals this record either
                // way, so the record waits rather than going out in the clear.
                tracing::debug!(%error, "the identity service is not here for the tree's keys yet");
                None
            }
        }
    }

    /// This device's connection to the session's identity service, bound to the workspace whose tree
    /// it publishes, as an app's connection is.
    fn connect_identity(&self) -> Result<BrokerSigningIdentity> {
        let mut stream =
            tilo_files::native::socket::connect(&self.identity).with_context(|| {
                format!(
                    "connect to the identity service at {}",
                    self.identity.display()
                )
            })?;
        let handshake = PresentationHandshake {
            handshake: Handshake {
                workspace_id: self.cluster.clone(),
                app_id: FILES_APP.to_owned(),
                surface: None,
            },
            presentation_version: PRESENTATION_VERSION,
        };
        let bytes =
            serde_json::to_vec(&handshake).context("this connection's handshake is JSON")?;
        HANDSHAKE_FRAMING
            .write(&mut stream, &bytes)
            .context("name this device's app and workspace on the identity connection")?;
        stream
            .flush()
            .context("the identity service reads this connection's handshake")?;
        BrokerSigningIdentity::from_connected_io(stream)
            .context("the identity service answers this device")
    }
}

/// Whether the changes `bytes` of record `chunk` build on something this device holds, in the
/// record or in the changes themselves: what a change delivered before the record it changes does
/// not.
fn applies(chunk: TreeChunk, namespace: &Arc<Mutex<Namespace>>, bytes: &[u8]) -> Result<bool> {
    if bytes.is_empty() {
        return Ok(true);
    }
    let changes = collab_types::parse_changes(bytes).context("a published change set parses")?;
    let carried: HashSet<ChangeHash> = changes.iter().map(automerge::Change::hash).collect();
    let mut namespace = namespace.lock_recover();
    Ok(changes.iter().all(|change| {
        change
            .deps()
            .iter()
            .all(|dep| carried.contains(dep) || namespace.holds_change(chunk, dep))
    }))
}

impl RecordPublisher {
    /// Whether the record may go out: the person's own store takes it as it is, and a workspace
    /// that seals its records hands this device their keys when it is admitted. Until then the
    /// record waits rather than going out in the clear.
    fn keys_here(&self) -> bool {
        !matches!(self.access, Access::Waiting)
    }
}

/// Follow `chunk`'s changes as the session publishes them.
fn forward_changes(
    chunk: TreeChunk,
    events: runtime_collab::OpStream,
    tell: tokio::sync::mpsc::UnboundedSender<FromRecord>,
) {
    tokio::spawn(async move {
        while let Ok(event) = events.read().await {
            if tell.send(FromRecord::Change { chunk, event }).is_err() {
                return;
            }
        }
        let _ = tell.send(FromRecord::Ended);
    });
}

/// Follow the session's asks of `chunk`'s holders.
fn forward_asks(
    chunk: TreeChunk,
    context: runtime_collab::ContextStream,
    tell: tokio::sync::mpsc::UnboundedSender<FromRecord>,
) {
    tokio::spawn(async move {
        while let Ok(context) = context.read().await {
            if context.snapshot_requested && tell.send(FromRecord::Asked { chunk }).is_err() {
                return;
            }
        }
        let _ = tell.send(FromRecord::Ended);
    });
}

/// Follow changes to `chunk`'s content keys.
fn forward_keys(
    chunk: TreeChunk,
    handle: ChannelHandle,
    tell: tokio::sync::mpsc::UnboundedSender<FromRecord>,
) {
    tokio::spawn(async move {
        let mut after = 0;
        while let Ok(at) = handle
            .changed(runtime_collab::wire::Changes::GroupKeys, after)
            .await
        {
            after = at;
            if tell.send(FromRecord::Keys { chunk }).is_err() {
                return;
            }
        }
        let _ = tell.send(FromRecord::Ended);
    });
}
