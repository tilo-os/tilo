//! The mounted workspace tree, published as the Files app's record.
//!
//! The namespace is one Automerge document ([`crate::namespace`]), and
//! `fs_protocol::tree` reads that same document as a resource. This module is
//! the writer that joins the record's route on the session: a device that
//! mounts a tree publishes it whole as the record's first publication, and
//! every change the mount makes afterwards as an increment. A device that
//! mounts nothing yet is given the tree whole by a device that holds it,
//! rather than creating a second empty tree beside it, and answers the
//! session's ask when a device joins and needs the tree whole again.
//!
//! The filesystem daemon is the windowless endpoint of the Files app: the
//! launcher names the app and workspace on the connection it dials, and the
//! session admits it against the launch it recorded for this process. Files
//! change where the files are, so nothing here carries an action: a change
//! is the namespace's own edit, made by the mount.
//!
//! A shared workspace seals its records, so this endpoint seals and opens the
//! tree the way every app of the workspace does: it asks the session's
//! identity service for the record's content keys and seals each change under
//! them before the session admits it, and opens the changes others publish
//! under the same keys.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result, bail};
use automerge::ChangeHash;
use channel_crypto::ChannelContentKeys;
use collab_types::document_transfer::{DocumentFrame, DocumentReassembler};
use collab_types::{
    ContentRetention, DocumentMutation, EncryptionMetadata, OpKind, SignedOp, document_artifact_id,
    document_attestation_id, next_unordered_document_sequence, sign_op,
};
use runtime_collab::{CertifiedJoinIdentity, ChannelHandle, CollabClient, OpEvent};
use tilo_identity_client::BrokerSigningIdentity;
use tilo_identity_client::wire::{
    ChannelAccess, HANDSHAKE_FRAMING, Handshake, PRESENTATION_VERSION, PresentationHandshake,
};
use tilo_lock::MutexRecover as _;

use crate::identity::PeerCredentials;
use crate::namespace::Namespace;

/// The app whose person record a workspace's tree is: it declares the kind,
/// and the daemon is its endpoint.
pub const FILES_APP: &str = "so.tilo.file-explorer";

/// How long the writer waits before dialing the session again after its
/// connection ended: the session may restart under a daemon that outlives it.
const RECONNECT: Duration = Duration::from_secs(2);

/// The most changes of a sealed record this device holds unopened while it
/// waits for the record's keys. Past it the tree is read whole instead, which
/// settles every change at once.
const MAX_UNOPENED: usize = 16;

/// Publish `namespace` on `cluster` as the Files app's record, for as long as
/// the daemon serves it, on the collaboration socket at `socket`. The
/// identity service at `identity` is where a workspace that seals its records
/// hands this endpoint their content keys.
///
/// The writer rejoins whenever that connection ends; a session that is not up
/// yet is waited for, not given up on, since the daemon starts beside it.
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
                Ok(()) => tracing::debug!(cluster, "the workspace tree's record connection ended"),
                Err(error) => {
                    tracing::warn!(cluster, ?error, "the workspace tree is not published");
                }
            }
            tokio::time::sleep(RECONNECT).await;
        }
    })
}

/// One connection to the session, joined and publishing `namespace` until the
/// connection ends.
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

    let mut publisher = TreePublisher::join(&client, cluster, credentials, identity).await?;
    // A workspace's keys arrive when a commit brings this device into the
    // record's group, possibly after the join: the tree is published, and the
    // changes that waited for them opened, on the change itself.
    let keys = publisher.handle.clone();
    let mut keys_after = 0;
    let mut changes = namespace.lock_recover().subscribe_changes();
    let events = publisher
        .handle
        .subscribe()
        .await
        .context("the session publishes the record's changes")?;
    let context = publisher
        .handle
        .subscribe_context()
        .await
        .context("the session tells this device what it is asked for")?;
    if namespace.lock_recover().holds_tree() {
        // This device mounts a tree: the record holds it whole from here on.
        publisher.publish_whole(namespace).await?;
    } else {
        // This device mounts nothing yet: it is given the tree a device that
        // has one publishes, rather than creating a second empty one.
        publisher.ask_for_the_tree().await;
    }
    loop {
        tokio::select! {
            moved = changes.changed() => {
                if moved.is_err() {
                    // The namespace is gone; nothing is left to publish.
                    return Ok(());
                }
                publisher.publish(namespace).await?;
            }
            event = events.read() => {
                let event = event.context("the record's changes reach this device")?;
                publisher.receive(event, namespace).await?;
            }
            asked = context.read() => {
                let asked = asked.context("the session's asks reach this device")?;
                if asked.snapshot_requested && namespace.lock_recover().holds_tree() {
                    // A device joined and the session asked a holder for the
                    // tree whole: this one holds it.
                    publisher.publish_whole(namespace).await?;
                }
            }
            keys_at = keys.changed(runtime_collab::wire::Changes::ChannelKeys, keys_after) => {
                keys_after = keys_at.context("the session says when the record's keys change")?;
                publisher.keys_changed(namespace).await?;
            }
        }
    }
}

/// Whether `cluster` is the store of the person `root` belongs to: records
/// there are kept in the store and reach their other devices over the link
/// those devices prove themselves to each other on, so no workspace channel
/// seals them.
fn in_person_store(root: &[u8; 32], cluster: &str) -> bool {
    uuid::Uuid::parse_str(cluster)
        .is_ok_and(|cluster| collab_types::is_personal_cluster(root, &cluster))
}

/// What became of one change of the record this device was handed.
enum Open {
    /// The change's payload, opened.
    Opened(Vec<u8>),
    /// This device's keys cannot open it yet: newer ones may.
    Later,
    /// It never opens here: it is not this record's, or not this channel's.
    Never,
}

/// This device's participation in one workspace's tree record.
struct TreePublisher {
    cluster: String,
    handle: ChannelHandle,
    credentials: PeerCredentials,
    /// The namespace heads this device has published: every change after them
    /// is the next increment. Empty until this device published the tree
    /// whole, which is how the record's first publication goes out.
    sent: Vec<ChangeHash>,
    /// The typed mutation a segmented transfer is arriving as.
    reassembler: DocumentReassembler,
    /// Where this device asks for the record's content keys, when the
    /// workspace seals its records.
    identity: PathBuf,
    /// The identity service connection, opened the first time keys are
    /// needed.
    signer: Option<Arc<BrokerSigningIdentity>>,
    /// What this device may do with the record's content: seal it, wait for
    /// its keys, or write it unsealed.
    access: Access,
    /// The record's content keys, while this device holds them.
    keys: Option<ChannelContentKeys>,
    /// The service generation the keys were answered at: what this device
    /// asks from when it asks again, so the answer is keys only when they
    /// changed.
    generation: u64,
    /// Changes of a sealed record that arrived before its keys did, in the
    /// order they came.
    unopened: Vec<SignedOp>,
    /// Changes of the record that arrived before the tree did, in the order
    /// they came: they change the tree a holder publishes, and apply once
    /// this device holds it.
    waiting_for_the_tree: Vec<SignedOp>,
}

/// What this device may do with the record's content.
///
/// A workspace that seals its records holds every change until it hands the
/// keys over, as an app of the workspace holds its edits. The person's own
/// cluster seals nothing: its records are kept in the person's store and reach
/// their other devices over the link those devices authenticate each other
/// with.
#[derive(Clone, Copy)]
enum Access {
    /// The workspace hands this endpoint the record's keys: every change it
    /// makes is sealed under them, and every change that arrives must open.
    Sealed,
    /// Its keys are not here yet: the tree waits for them rather than going
    /// out in the clear into a workspace that seals.
    Waiting,
    /// The tree is the person's own, kept in their store: their devices
    /// reach it over the link they prove themselves to each other on, and no
    /// workspace channel seals it.
    Unsealed,
}

impl TreePublisher {
    /// Join `cluster`'s tree record as this device.
    async fn join(
        client: &CollabClient,
        cluster: &str,
        credentials: &PeerCredentials,
        identity: &Path,
    ) -> Result<Self> {
        let record = collab_types::person_record_id(fs_protocol::tree::TREE_KIND);
        let channel = runtime_collab::route::channel_key::format(FILES_APP, cluster, &record);
        let presentation = CertifiedJoinIdentity {
            workspace_id: cluster.to_owned(),
            device_certificate: serde_json::to_vec(credentials.cert())
                .context("this device's certificate is JSON")?,
            proof: ed25519_dalek::Signer::sign(
                credentials.signing_key(),
                &tilo_identity_client::collaboration_join_signing_bytes(cluster),
            )
            .to_bytes()
            .to_vec(),
        }
        .encode()
        .context("this device's join presentation encodes")?;
        let handle = client
            .join_channel_as_kind(
                &channel,
                &presentation,
                "",
                "person",
                fs_protocol::tree::TREE_KIND,
            )
            .await
            .context("the session admits this device to the workspace tree")?;
        // The kind the join named is the record the session founds for this
        // endpoint's first write, as it founds every app's record on open.
        let person = credentials.cert().user_public_key;
        let mut publisher = Self {
            cluster: cluster.to_owned(),
            handle,
            credentials: credentials.clone(),
            sent: Vec::new(),
            reassembler: DocumentReassembler::default(),
            identity: identity.to_path_buf(),
            signer: None,
            access: if in_person_store(&person, cluster) {
                Access::Unsealed
            } else {
                Access::Waiting
            },
            keys: None,
            generation: 0,
            unopened: Vec::new(),
            waiting_for_the_tree: Vec::new(),
        };
        publisher.refresh_keys().await;
        Ok(publisher)
    }

    /// The record's keys changed: take them, publish the tree this device
    /// holds if the wait is over, and open the changes that waited with it.
    async fn keys_changed(&mut self, namespace: &Arc<Mutex<Namespace>>) -> Result<()> {
        self.refresh_keys().await;
        if !matches!(self.access, Access::Sealed) {
            return Ok(());
        }
        self.take_unopened(namespace).await?;
        if namespace.lock_recover().holds_tree() {
            self.publish(namespace).await?;
        }
        Ok(())
    }

    /// Whether the tree may go out: the person's own store takes it as it is,
    /// and a workspace that seals its records hands this device their keys
    /// when it is admitted. Until they are here the tree waits rather than
    /// going out in the clear.
    fn keys_here(&self) -> bool {
        !matches!(self.access, Access::Waiting)
    }

    /// Ask a device that holds the tree to publish it whole, as a device
    /// joining it with nothing of its own does.
    async fn ask_for_the_tree(&self) {
        if let Err(error) = self.handle.request_document_snapshot().await {
            tracing::debug!(%error, "the record's holders are not asked for the tree whole");
        }
    }

    /// Publish the tree whole: what the record holds from this device from now
    /// on, and what a device joining it gets.
    async fn publish_whole(&mut self, namespace: &Arc<Mutex<Namespace>>) -> Result<()> {
        if !self.keys_here() {
            return Ok(());
        }
        let (bytes, heads) = {
            let mut namespace = namespace.lock_recover();
            let bytes = namespace.save();
            (bytes, namespace.heads())
        };
        self.send(DocumentMutation::Snapshot(bytes)).await?;
        self.sent = heads;
        Ok(())
    }

    /// Publish what this device changed: the tree whole when it has published
    /// nothing yet, everything since it last did afterwards.
    async fn publish(&mut self, namespace: &Arc<Mutex<Namespace>>) -> Result<()> {
        if !self.keys_here() {
            return Ok(());
        }
        if self.sent.is_empty() {
            return self.publish_whole(namespace).await;
        }
        let (bytes, heads) = {
            let mut namespace = namespace.lock_recover();
            let bytes = namespace.changes_since(&self.sent);
            (bytes, namespace.heads())
        };
        if bytes.is_empty() {
            return Ok(());
        }
        self.send(DocumentMutation::Incremental(bytes)).await?;
        self.sent = heads;
        Ok(())
    }

    /// Take one change another device published, and publish what resolving
    /// it wrote.
    async fn receive(&mut self, event: OpEvent, namespace: &Arc<Mutex<Namespace>>) -> Result<()> {
        let op: SignedOp = postcard::from_bytes(&event.op).context("a signed change decodes")?;
        if op.author.as_bytes() == &self.credentials.device_public_key() {
            // This device's own change: the namespace holds it already.
            return Ok(());
        }
        self.unopened.push(op);
        if self.unopened.len() > MAX_UNOPENED {
            // The keys still are not here: read the tree whole from a holder
            // instead of holding changes this device cannot open.
            self.unopened.clear();
            self.ask_for_the_tree().await;
            return Ok(());
        }
        self.take_unopened(namespace).await
    }

    /// Apply every change this device holds the keys for, in the order they
    /// arrived, asking for the record's keys once when one of them needs
    /// them; then the changes that waited for the tree, once it is here.
    async fn take_unopened(&mut self, namespace: &Arc<Mutex<Namespace>>) -> Result<()> {
        loop {
            self.apply_unopened(namespace).await?;
            if self.waiting_for_the_tree.is_empty() || !namespace.lock_recover().holds_tree() {
                return Ok(());
            }
            // The tree came: the changes that waited for it apply now, in the
            // order they came.
            self.unopened = std::mem::take(&mut self.waiting_for_the_tree);
        }
    }

    /// Apply the changes whose keys are here, in the order they arrived; the
    /// rest wait for the keys that open them ([`Self::keys_changed`]).
    async fn apply_unopened(&mut self, namespace: &Arc<Mutex<Namespace>>) -> Result<()> {
        let mut asked = false;
        loop {
            let mut waiting = Vec::new();
            while let Some(op) = self.unopened.first() {
                let payload = match self.opened(op) {
                    Open::Opened(payload) => payload,
                    Open::Later => {
                        let op = self.unopened.remove(0);
                        waiting.push(op);
                        continue;
                    }
                    Open::Never => {
                        self.unopened.remove(0);
                        tracing::debug!("a change handed to this device is not the tree's to open");
                        continue;
                    }
                };
                let op = self.unopened.remove(0);
                self.apply(&op, payload, namespace).await?;
            }
            if waiting.is_empty() {
                return Ok(());
            }
            self.unopened = waiting;
            if asked {
                return Ok(());
            }
            // The keys this device holds may have moved on since the changes
            // arrived, as a workspace that re-keys hands out new ones: ask
            // once, and open what they reach.
            asked = true;
            self.refresh_keys().await;
        }
    }

    /// The change `op` carries, opened when the workspace seals it.
    fn opened(&self, op: &SignedOp) -> Open {
        match (&op.encryption, self.access) {
            (None, Access::Unsealed) => Open::Opened(op.payload.clone()),
            // Sealed or not, the workspace's answer is not here yet.
            (_, Access::Waiting) => Open::Later,
            // A change that does not go as this device's route does is another
            // workspace's: cleartext into a workspace that seals is the
            // downgrade the gate refuses, and sealed content on a route with
            // no keys to open it is not this tree's.
            (None, Access::Sealed) | (Some(_), Access::Unsealed) => Open::Never,
            (Some(metadata), Access::Sealed) => {
                let Some(keys) = &self.keys else {
                    return Open::Later;
                };
                if keys.channel_id() != metadata.channel {
                    // Sealed under another of the record's channels, as a
                    // workspace that re-keys hands out: the keys that open
                    // it are newer than the ones here.
                    return Open::Later;
                }
                if !keys.holds_epoch(metadata.epoch) {
                    // Sealed under an epoch this device's keys have moved
                    // past: newer keys open it.
                    return Open::Later;
                }
                match keys.open_content(self.artifact(), metadata, &op.payload) {
                    Ok(opened) => Open::Opened(opened.to_vec()),
                    Err(_) => {
                        // Sealed under a key from another branch of the
                        // record's group, as a workspace that re-keys hands
                        // out: the ones that open it are newer than these.
                        Open::Later
                    }
                }
            }
        }
    }

    /// Apply one opened change, and publish what resolving it wrote.
    async fn apply(
        &mut self,
        op: &SignedOp,
        payload: Vec<u8>,
        namespace: &Arc<Mutex<Namespace>>,
    ) -> Result<()> {
        let Some(mutation) = self.mutation_of(op, &payload)? else {
            return Ok(());
        };
        if matches!(mutation, DocumentMutation::Incremental(_))
            && !namespace.lock_recover().holds_tree()
        {
            // A change of a tree this device was never given: it waits for
            // the whole of it rather than merging one root beside another,
            // and applies once the tree here is that one.
            if self.waiting_for_the_tree.len() > MAX_UNOPENED {
                // The tree is not coming: this device reads it whole instead.
                self.waiting_for_the_tree.clear();
            }
            self.waiting_for_the_tree.push(op.clone());
            self.ask_for_the_tree().await;
            return Ok(());
        }
        let resolution = {
            let mut namespace = namespace.lock_recover();
            match mutation {
                DocumentMutation::Snapshot(bytes) => namespace
                    .adopt_or_merge_remote_and_resolve(&bytes)
                    .context("the published tree becomes this device's")?,
                DocumentMutation::Incremental(bytes) => namespace
                    .apply_remote_and_resolve(&bytes)
                    .context("a published tree change applies to this one")?,
            }
        };
        if !resolution.is_empty() {
            self.send(DocumentMutation::Incremental(resolution)).await?;
        }
        self.sent = namespace.lock_recover().heads();
        Ok(())
    }

    /// The change `payload` carries, once every segment of it arrived. `None`
    /// for a segment of a transfer still arriving.
    fn mutation_of(&mut self, op: &SignedOp, payload: &[u8]) -> Result<Option<DocumentMutation>> {
        match DocumentFrame::decode(payload).context("a published change is a document frame")? {
            DocumentFrame::Whole(mutation) => Ok(Some(mutation)),
            DocumentFrame::Segment(segment) => {
                let complete = self
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

    /// Sign `mutation` as this device and publish every frame of it, sealed
    /// under the record's content keys when the workspace seals its records.
    async fn send(&mut self, mutation: DocumentMutation) -> Result<()> {
        let frames = mutation
            .encode_frames()
            .context("the change fits the collaboration wire")?;
        for frame in frames {
            let (payload, encryption) = self.seal(frame).await?;
            let now = SystemTime::now();
            let mut op = match encryption {
                Some(metadata) => SignedOp::new_unsigned_with_encryption(
                    payload,
                    self.artifact(),
                    collab_types::Ed25519PublicKey::from_bytes(
                        self.credentials.device_public_key(),
                    ),
                    self.attestation(),
                    OpKind::Change,
                    now,
                    next_unordered_document_sequence(now),
                    metadata,
                ),
                None => SignedOp::new_unsigned(
                    payload,
                    self.artifact(),
                    collab_types::Ed25519PublicKey::from_bytes(
                        self.credentials.device_public_key(),
                    ),
                    self.attestation(),
                    OpKind::Change,
                    now,
                    next_unordered_document_sequence(now),
                ),
            };
            sign_op(&mut op, self.credentials.signing_key())
                .context("this device signs the change it publishes")?;
            let bytes = postcard::to_stdvec(&op).context("a signed change encodes")?;
            self.handle
                .send(&bytes)
                .await
                .context("the session admits this device's change")?;
        }
        Ok(())
    }

    /// `frame` as the session admits it: sealed under the record's keys when
    /// the workspace seals its records, and as it is otherwise.
    async fn seal(&mut self, frame: Vec<u8>) -> Result<(Vec<u8>, Option<EncryptionMetadata>)> {
        if matches!(self.access, Access::Waiting) {
            // The record's keys may have been handed over since the join.
            self.refresh_keys().await;
        }
        if matches!(self.access, Access::Unsealed) {
            return Ok((frame, None));
        }
        for attempt in 0..2 {
            let Access::Sealed = self.access else {
                bail!("the workspace has not handed this device the tree's keys yet");
            };
            let Some(keys) = &self.keys else {
                bail!("this device holds no keys of the tree's");
            };
            // A whole tree is a checkpoint of the record, which is what a
            // device holding none of it takes in first.
            let retention = ContentRetention::of_frame(&frame);
            match keys.seal_content(self.artifact(), retention, &frame) {
                Ok((metadata, sealed)) => return Ok((sealed, Some(metadata))),
                Err(channel_crypto::Error::MissingContentKey(_)) if attempt == 0 => {
                    // The channel moved to an epoch this device has not been
                    // handed yet: ask for the keys again and seal under them.
                    self.refresh_keys().await;
                }
                Err(error) => {
                    return Err(error).context("the change seals under the record's keys");
                }
            }
        }
        bail!("the record's keys do not cover the channel's current epoch")
    }

    /// Ask the session's identity service for the record's content keys, at
    /// the generation this device holds: it is answered the keys when they
    /// changed, and with none when they did not.
    ///
    /// A service that is not there yet, or has not handed the keys over,
    /// keeps them waiting: a workspace that seals its records never has this
    /// tree published in the clear.
    async fn refresh_keys(&mut self) {
        if matches!(self.access, Access::Unsealed) {
            return;
        }
        let signer = match &self.signer {
            Some(signer) => Arc::clone(signer),
            None => match self.connect_identity() {
                Ok(signer) => {
                    let signer = Arc::new(signer);
                    self.signer = Some(Arc::clone(&signer));
                    signer
                }
                Err(error) => {
                    // Not there yet, or refused this device: the workspace
                    // seals this record either way, so the tree waits rather
                    // than going out in the clear.
                    tracing::debug!(%error, "the identity service is not here for the tree's keys yet");
                    return;
                }
            },
        };
        let cluster = self.cluster.clone();
        let record = Self::record();
        let generation = self.generation;
        let asked = tokio::task::spawn_blocking(move || {
            signer.channel_keys(&cluster, FILES_APP, &record, generation)
        })
        .await;
        match asked {
            Ok(Ok(reply)) => match (reply.access, reply.keys.as_deref()) {
                (ChannelAccess::Ready, Some(bytes)) => {
                    match ChannelContentKeys::from_bytes(bytes) {
                        Ok(keys) => {
                            self.keys = Some(keys);
                            self.generation = reply.generation;
                            self.access = Access::Sealed;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "the tree's content keys do not decode");
                        }
                    }
                }
                // Keys reported unchanged: the ones held still open the
                // record, and a caller holding none never gets this answer.
                (ChannelAccess::Ready, None) => {
                    self.access = if self.keys.is_some() {
                        Access::Sealed
                    } else {
                        Access::Waiting
                    };
                }
                (access, _) => {
                    // Waiting, removed, unavailable, or no secret store: the
                    // workspace seals, and the tree waits to be sealed.
                    self.access = Access::Waiting;
                    tracing::debug!(
                        ?access,
                        cluster = %self.cluster,
                        "the workspace has not handed this device the tree's keys yet"
                    );
                }
            },
            Ok(Err(error)) => {
                // The service answered and refused: it may answer differently
                // once this device is admitted, so the tree waits rather than
                // going out in the clear.
                tracing::debug!(%error, "the identity service withholds the tree's keys");
            }
            Err(error) => {
                tracing::debug!(%error, "the tree's keys were not asked for");
            }
        }
    }

    /// This device's connection to the session's identity service, bound to
    /// the workspace whose tree it publishes, as an app's connection is.
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

    /// The artifact this device's changes are of.
    fn artifact(&self) -> collab_types::ArtifactId {
        document_artifact_id(FILES_APP, &self.cluster, &Self::record())
    }

    /// The attestation this device's changes are made under.
    fn attestation(&self) -> collab_types::AttestationId {
        document_attestation_id(FILES_APP, &self.cluster, &Self::record())
    }

    /// The record this device publishes.
    fn record() -> String {
        collab_types::person_record_id(fs_protocol::tree::TREE_KIND)
    }
}
