//! The mounted workspace tree, published as the Files app's person record.
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

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result};
use automerge::ChangeHash;
use collab_types::document_transfer::{DocumentFrame, DocumentReassembler};
use collab_types::{
    DocumentMutation, OpKind, SignedOp, document_artifact_id, document_attestation_id,
    next_unordered_document_sequence, sign_op,
};
use runtime_collab::{CertifiedJoinIdentity, ChannelHandle, CollabClient, OpEvent};
use tilo_lock::MutexRecover as _;

use crate::identity::PeerCredentials;
use crate::namespace::Namespace;

/// The app whose person record a workspace's tree is: it declares the kind,
/// and the daemon is its endpoint.
pub const FILES_APP: &str = "so.tilo.file-explorer";

/// How long the writer waits before dialing the session again after its
/// connection ended: the session may restart under a daemon that outlives it.
const RECONNECT: Duration = Duration::from_secs(2);

/// Publish `namespace` on `cluster` as the Files app's record, for as long as
/// the daemon serves it.
///
/// The writer dials the session's collaboration socket and rejoins whenever
/// that connection ends; a session that is not up yet is waited for, not
/// given up on, since the daemon starts beside it.
#[must_use = "the handle is how a daemon stops publishing before it unmounts"]
pub fn spawn(
    namespace: Arc<Mutex<Namespace>>,
    cluster: String,
    credentials: PeerCredentials,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match serve_once(&namespace, &cluster, &credentials).await {
                Ok(()) => tracing::debug!(cluster, "the workspace tree's record connection ended"),
                Err(error) => {
                    tracing::warn!(cluster, %error, "the workspace tree is not published");
                }
            }
            tokio::time::sleep(RECONNECT).await;
        }
    })
}

/// Where the session's collaboration broker listens.
fn socket() -> std::path::PathBuf {
    tilo_paths::xdg::service_socket("collab")
}

/// One connection to the session, joined and publishing `namespace` until the
/// connection ends.
async fn serve_once(
    namespace: &Arc<Mutex<Namespace>>,
    cluster: &str,
    credentials: &PeerCredentials,
) -> Result<()> {
    let path = socket();
    let mut stream = tilo_files::native::socket::connect(&path)
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

    let mut publisher = TreePublisher::join(&client, cluster, credentials).await?;
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
        }
    }
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
}

impl TreePublisher {
    /// Join `cluster`'s tree record as this device.
    async fn join(
        client: &CollabClient,
        cluster: &str,
        credentials: &PeerCredentials,
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
            .join_channel(&channel, &presentation)
            .await
            .context("the session admits this device to the workspace tree")?;
        Ok(Self {
            cluster: cluster.to_owned(),
            handle,
            credentials: credentials.clone(),
            sent: Vec::new(),
            reassembler: DocumentReassembler::default(),
        })
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

    /// Apply one change another device published, and publish what resolving
    /// it wrote.
    async fn receive(&mut self, event: OpEvent, namespace: &Arc<Mutex<Namespace>>) -> Result<()> {
        let op: SignedOp = postcard::from_bytes(&event.op).context("a signed change decodes")?;
        if op.author.as_bytes() == &self.credentials.device_public_key() {
            // This device's own change: the namespace holds it already.
            return Ok(());
        }
        let Some(mutation) = self.mutation_of(&op)? else {
            return Ok(());
        };
        if matches!(mutation, DocumentMutation::Incremental(_))
            && !namespace.lock_recover().holds_tree()
        {
            // A change of a tree this device was never given: it asks for the
            // tree whole rather than merging one root beside another.
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

    /// The change `op` carries, once every segment of it arrived. `None` for
    /// a segment of a transfer still arriving.
    fn mutation_of(&mut self, op: &SignedOp) -> Result<Option<DocumentMutation>> {
        match DocumentFrame::decode(&op.payload)
            .context("a published change is a document frame")?
        {
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

    /// Sign `mutation` as this device and publish every frame of it.
    async fn send(&self, mutation: DocumentMutation) -> Result<()> {
        let frames = mutation
            .encode_frames()
            .context("the change fits the collaboration wire")?;
        for frame in frames {
            let now = SystemTime::now();
            let mut op = SignedOp::new_unsigned(
                frame,
                document_artifact_id(FILES_APP, &self.cluster, &Self::record()),
                collab_types::Ed25519PublicKey::from_bytes(self.credentials.device_public_key()),
                document_attestation_id(FILES_APP, &self.cluster, &Self::record()),
                OpKind::Change,
                now,
                next_unordered_document_sequence(now),
            );
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

    /// The record this device publishes.
    fn record() -> String {
        collab_types::person_record_id(fs_protocol::tree::TREE_KIND)
    }
}
