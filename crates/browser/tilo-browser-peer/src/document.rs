//! Durable documents on the wire: signed `NeighbourMsg::AppDocument`
//! envelopes, byte-for-byte what a native shell publishes through its
//! workspace host, optionally sealed with the route's channel content key.
//!
//! A browser is both the app and the shell for its documents, so it signs
//! envelopes with its certified device key and verifies inbound ones the way
//! a shell would before an app sees them: the envelope signature must verify,
//! its author must be the device the host authenticated, and the route and
//! artifact must match.

use std::collections::{BTreeMap, HashMap};

use collab_types::{
    ContentRetention, DocumentFrame, DocumentMutation, DocumentMutationKind, EncryptionMetadata,
    SignedOp, document_artifact_id, document_attestation_id,
};
use neighbour_wire::{NeighbourMsg, Retention};
use tilo_relay_client::DeviceSigner;

/// One document route: application, room (cluster) and record.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct DocumentRoute {
    /// Application id.
    pub app_id: String,
    /// Room uuid, as text.
    pub cluster_id: String,
    /// Record wire name; empty when the app announced none.
    pub record: String,
}

impl DocumentRoute {
    /// Relay workspace that carries this route's room.
    pub fn workspace_id(&self) -> Result<u64, DocumentError> {
        tilo_relay_protocol::room_workspace_id(&self.cluster_id)
            .map_err(|_| DocumentError::Route("room is not a uuid".into()))
    }

    fn artifact(&self) -> collab_types::ArtifactId {
        document_artifact_id(&self.app_id, &self.cluster_id, &self.record)
    }
}

/// End-to-end content encryption of one route (the browser as a
/// `channel-crypto` member device).
pub trait ContentCipher: Send {
    /// Seal one cleartext document frame.
    ///
    /// # Errors
    ///
    /// Returns a description when this member cannot seal (no key).
    fn seal(
        &mut self,
        artifact: collab_types::ArtifactId,
        retention: ContentRetention,
        cleartext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String>;

    /// Open one sealed payload.
    fn open(
        &mut self,
        artifact: collab_types::ArtifactId,
        metadata: &EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Vec<u8>, String>;

    /// Whether this member holds the key of content-key `epoch`. A payload
    /// of an epoch it does not hold yet (sealed by a member admitted after
    /// this one last caught up) waits for the keys instead of being refused.
    fn holds_epoch(&self, _epoch: u64) -> bool {
        true
    }
}

/// A read-only adapter over content keys the caller already owns.
/// It cannot seal a document, enroll a device, or obtain missing keys.
#[must_use]
pub fn content_key_reader(keys: channel_crypto::ChannelContentKeys) -> Box<dyn ContentCipher> {
    Box::new(ContentKeyReader(crate::channels::KeysCipher(keys)))
}

struct ContentKeyReader(crate::channels::KeysCipher);

impl ContentCipher for ContentKeyReader {
    fn seal(
        &mut self,
        _artifact: collab_types::ArtifactId,
        _retention: ContentRetention,
        _cleartext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        Err("a content-key reader cannot seal documents".into())
    }

    fn open(
        &mut self,
        artifact: collab_types::ArtifactId,
        metadata: &EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.0.open(artifact, metadata, payload)
    }

    fn holds_epoch(&self, epoch: u64) -> bool {
        self.0.holds_epoch(epoch)
    }
}

/// The installed content ciphers, by route. A route without one is
/// cleartext.
pub type Ciphers = std::collections::BTreeMap<DocumentRoute, Box<dyn ContentCipher>>;

/// Why a document envelope was not produced or accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    /// The route or artifact does not match.
    #[error("document route: {0}")]
    Route(String),
    /// Signing or signature verification failed.
    #[error("document signature: {0}")]
    Signature(String),
    /// The payload is not a document frame, or does not open.
    #[error("document payload: {0}")]
    Payload(String),
}

/// A document frame ready for the host: the encoded message and the
/// retention channel it is kept under.
#[derive(Clone, Debug)]
pub struct SignedDocumentFrame {
    /// Encoded `NeighbourMsg::AppDocument`.
    pub body: Vec<u8>,
    /// Retention channel digest.
    pub channel: [u8; 32],
    /// Whether the host keeps it as the channel's new checkpoint.
    pub checkpoint: bool,
}

/// A verified inbound document frame.
#[derive(Clone, Debug)]
pub struct OpenedDocument {
    pub route: DocumentRoute,
    pub envelope: SignedOp,
    /// The authenticated authoring device.
    pub author: [u8; 32],
    /// Cleartext when the envelope was sealed.
    pub decrypted_payload: Option<Vec<u8>>,
    /// The complete mutation when this is the last segment of a segmented
    /// transfer; `envelope` is then that segment's.
    whole: Option<DocumentMutation>,
    /// Retained entry ids of the frames this document came in.
    entries: Vec<[u8; 32]>,
}

impl OpenedDocument {
    /// The host's ids ([`tilo_relay_protocol::compaction::entry_id`]) of the
    /// retained frames this document came in: one, or every segment of a
    /// reassembled transfer.
    pub fn entries(&self) -> &[[u8; 32]] {
        &self.entries
    }

    /// The typed mutation: carried by a whole frame, or reassembled from
    /// every segment of a large one.
    pub fn mutation(&self) -> Option<DocumentMutation> {
        if let Some(whole) = &self.whole {
            return Some(whole.clone());
        }
        match DocumentFrame::decode(
            self.decrypted_payload
                .as_deref()
                .unwrap_or(&self.envelope.payload),
        )
        .ok()?
        {
            DocumentFrame::Whole(mutation) => Some(mutation),
            DocumentFrame::Segment(_) => None,
        }
    }

    /// Whether this document stands for a reassembled segmented transfer
    /// rather than for its own envelope's payload.
    pub fn is_reassembled(&self) -> bool {
        self.whole.is_some()
    }

    fn payload(&self) -> &[u8] {
        self.decrypted_payload
            .as_deref()
            .unwrap_or(&self.envelope.payload)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TransferKey {
    artifact: collab_types::ArtifactId,
    author: [u8; 32],
    digest: [u8; 32],
}

struct Transfer {
    kind: DocumentMutationKind,
    total_len: u64,
    segment_len: u32,
    segments: BTreeMap<u32, Vec<u8>>,
    entries: Vec<[u8; 32]>,
    bytes: usize,
    touched: u64,
}

/// Rebuilds segmented document transfers (a snapshot or change set larger
/// than one frame, see `collab_types::document_transfer`) from their
/// individually signed and verified segments. Memory stays within the
/// collaboration transport's reassembly limits by dropping the least
/// recently extended incomplete transfer; the page has no monotonic clock
/// `DocumentReassembler` accepts, so recency is an arrival counter.
#[derive(Default)]
pub struct Reassembly {
    transfers: HashMap<TransferKey, Transfer>,
    buffered: usize,
    arrivals: u64,
}

impl Reassembly {
    /// Pass a whole document through, hold a segment, or return the
    /// document completed by this final segment.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentError::Payload`] for a segment that contradicts
    /// its transfer or a complete transfer that fails its digest; the
    /// transfer is dropped.
    pub fn accept(
        &mut self,
        mut document: OpenedDocument,
    ) -> Result<Option<OpenedDocument>, DocumentError> {
        let segment = match DocumentFrame::decode(document.payload()) {
            Ok(DocumentFrame::Segment(segment)) => segment,
            Ok(DocumentFrame::Whole(_)) | Err(_) => return Ok(Some(document)),
        };
        self.arrivals = self.arrivals.saturating_add(1);
        let key = TransferKey {
            artifact: document.envelope.artifact,
            author: document.author,
            digest: segment.digest,
        };
        if let Some(transfer) = self.transfers.get(&key)
            && (transfer.kind != segment.kind
                || transfer.total_len != segment.total_len
                || transfer.segment_len != segment.segment_len)
        {
            self.remove(&key);
            return Err(DocumentError::Payload(
                "document segment contradicts its transfer".into(),
            ));
        }
        let len = segment.data.len();
        if self
            .transfers
            .get(&key)
            .is_some_and(|transfer| transfer.segments.contains_key(&segment.index))
        {
            return Ok(None);
        }
        self.make_room(len, &key);
        let transfer = self.transfers.entry(key).or_insert_with(|| Transfer {
            kind: segment.kind,
            total_len: segment.total_len,
            segment_len: segment.segment_len,
            segments: BTreeMap::new(),
            entries: Vec::new(),
            bytes: 0,
            touched: 0,
        });
        transfer.touched = self.arrivals;
        transfer.bytes = transfer.bytes.saturating_add(len);
        let count = segment.count() as usize;
        transfer.segments.insert(segment.index, segment.data);
        transfer.entries.extend_from_slice(&document.entries);
        self.buffered = self.buffered.saturating_add(len);
        if transfer.segments.len() < count {
            return Ok(None);
        }
        let Some(transfer) = self.remove(&key) else {
            return Ok(None);
        };
        let mut data = Vec::with_capacity(usize::try_from(transfer.total_len).unwrap_or(0));
        for chunk in transfer.segments.into_values() {
            data.extend_from_slice(&chunk);
        }
        if u64::try_from(data.len()).ok() != Some(transfer.total_len)
            || collab_types::document_transfer::transfer_digest(transfer.kind, &data) != key.digest
        {
            return Err(DocumentError::Payload(
                "reassembled document does not match its transfer digest".into(),
            ));
        }
        document.whole = Some(match transfer.kind {
            DocumentMutationKind::Snapshot => DocumentMutation::Snapshot(data),
            DocumentMutationKind::Incremental => DocumentMutation::Incremental(data),
        });
        document.entries = transfer.entries;
        Ok(Some(document))
    }

    /// Bytes held for incomplete transfers.
    pub fn buffered_bytes(&self) -> usize {
        self.buffered
    }

    fn make_room(&mut self, extra: usize, keep: &TransferKey) {
        loop {
            let slot = !self.transfers.contains_key(keep)
                && self.transfers.len()
                    >= collab_types::document_transfer::MAX_CONCURRENT_TRANSFERS;
            let bytes = self.buffered.saturating_add(extra)
                > collab_types::document_transfer::MAX_REASSEMBLY_BUFFER_BYTES;
            if !slot && !bytes {
                return;
            }
            let Some(victim) = self
                .transfers
                .iter()
                .filter(|(key, _)| *key != keep)
                .min_by_key(|(_, transfer)| transfer.touched)
                .map(|(key, _)| *key)
            else {
                return;
            };
            self.remove(&victim);
        }
    }

    fn remove(&mut self, key: &TransferKey) -> Option<Transfer> {
        let transfer = self.transfers.remove(key)?;
        self.buffered = self.buffered.saturating_sub(transfer.bytes);
        Some(transfer)
    }
}

/// Join each run of consecutive incremental changes into one change set.
/// Automerge loads concatenated change chunks as one, so a route's
/// checkpoint and its retained tail merge in two steps however long the
/// tail is. A snapshot stays on its own: its changes may depend on ones only
/// the local replica holds.
pub fn coalesce(mutations: Vec<DocumentMutation>) -> Vec<DocumentMutation> {
    let mut out: Vec<DocumentMutation> = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        match (out.last_mut(), mutation) {
            (Some(DocumentMutation::Incremental(run)), DocumentMutation::Incremental(bytes)) => {
                run.extend_from_slice(&bytes);
            }
            (_, mutation) => out.push(mutation),
        }
    }
    out
}

/// The changes a signed document body (a [`SignedDocumentFrame`]'s) claims
/// to carry, as their Automerge hashes: what its author withdraws when the
/// host refuses it. A snapshot claims none.
#[must_use]
pub fn claimed_changes(body: &[u8]) -> Vec<[u8; 32]> {
    match postcard::from_bytes::<NeighbourMsg>(body) {
        Ok(NeighbourMsg::AppDocument { envelope, .. }) => {
            envelope.claims.iter().map(|claim| claim.change).collect()
        }
        _ => Vec::new(),
    }
}

/// Stable `NeighbourMsg` peer handle of a device.
pub fn peer_id(device: &[u8; 32]) -> u64 {
    neighbour_wire::peer_handle(device)
}

/// Sign one cleartext document frame (one element of
/// `DocumentMutation::encode_frames`) as the device behind `signer`.
pub async fn sign_frame(
    route: &DocumentRoute,
    cleartext: Vec<u8>,
    signer: &dyn DeviceSigner,
    cipher: &mut Box<dyn ContentCipher>,
    seq: u64,
    now_ns: u64,
) -> Result<SignedDocumentFrame, DocumentError> {
    DocumentFrame::decode(&cleartext).map_err(|error| DocumentError::Payload(error.to_string()))?;
    let artifact = route.artifact();
    let author = collab_types::Ed25519PublicKey::from_bytes(signer.device_public_key());
    let attestation = document_attestation_id(&route.app_id, &route.cluster_id, &route.record);
    let timestamp = crate::clock::system_time(now_ns);
    // A document is only ever written sealed for its route's channel.
    let (metadata, sealed) = cipher
        .seal(artifact, ContentRetention::of_frame(&cleartext), &cleartext)
        .map_err(DocumentError::Payload)?;
    // A sealed op carries the claims of its changes for the relay, and names
    // the surface of the page's app they are decided against. Each claim names
    // the units its action may write by that surface, as a device's own
    // broker does: left on the whole record, a change to a part the app keeps
    // apart (a PDF's comments) is refused, since no action of it may write
    // the whole record.
    let claims = collab_types::DocumentMutation::decode(&cleartext)
        .ok()
        .and_then(|mutation| {
            collab_types::mutation_claims(&mutation, collab_types::record_unit).ok()
        })
        .unwrap_or_default();
    #[cfg(feature = "toolkit")]
    let (claims, surface) = (
        ui::app_surface::with_footprints(&route.app_id, claims),
        ui::app_surface::hash_of(&route.app_id),
    );
    #[cfg(not(feature = "toolkit"))]
    let surface = None;
    let mut op = SignedOp::new_unsigned_with_encryption(
        sealed,
        artifact,
        author,
        attestation,
        collab_types::OpKind::Change,
        timestamp,
        seq,
        metadata,
    )
    .with_claims(claims)
    .with_surface(surface);
    let signing_bytes = op
        .signing_bytes()
        .map_err(|error| DocumentError::Signature(error.to_string()))?;
    let signature = signer
        .sign(signing_bytes)
        .await
        .map_err(|error| DocumentError::Signature(error.0))?;
    op.signature = collab_types::Signature::from_bytes(signature);
    collab_types::verify_op(&op).map_err(|error| DocumentError::Signature(error.to_string()))?;
    let message = NeighbourMsg::AppDocument {
        peer: peer_id(&signer.device_public_key()),
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id.clone(),
        record_id: route.record.clone(),
        envelope: Box::new(op),
    };
    let (channel, retention) = neighbour_wire::retention(&message, false)
        .ok_or_else(|| DocumentError::Payload("document frame is not retainable".into()))?;
    Ok(SignedDocumentFrame {
        body: postcard::to_stdvec(&message)
            .map_err(|error| DocumentError::Payload(error.to_string()))?,
        channel,
        checkpoint: retention == Retention::Checkpoint,
    })
}

/// Verify one inbound host delta. `author` is the device the host stamped
/// from its authenticated connection. Returns `Ok(None)` for traffic that is
/// not an application document (shell tree, governance, continuity).
///
/// # Errors
///
/// Returns [`DocumentError`] for a document that must not reach an app.
pub fn open_frame(
    body: &[u8],
    author: [u8; 32],
    ciphers: &mut Ciphers,
) -> Result<Option<OpenedDocument>, DocumentError> {
    let Ok(NeighbourMsg::AppDocument {
        app_id,
        cluster_id,
        record_id,
        envelope,
        ..
    }) = postcard::from_bytes::<NeighbourMsg>(body)
    else {
        return Ok(None);
    };
    let route = DocumentRoute {
        app_id,
        cluster_id,
        record: record_id,
    };
    collab_types::verify_op(&envelope)
        .map_err(|error| DocumentError::Signature(error.to_string()))?;
    if *envelope.author.as_bytes() != author {
        return Err(DocumentError::Signature(
            "envelope author is not the authenticated device".into(),
        ));
    }
    if envelope.artifact != route.artifact() {
        return Err(DocumentError::Route("artifact does not match route".into()));
    }
    let decrypted_payload = match envelope.encryption {
        Some(metadata) => {
            let cipher = ciphers
                .get_mut(&route)
                .ok_or_else(|| DocumentError::Payload("route has no content key".into()))?;
            // A key holder's compaction of the record's sealed history
            // carries its sealed snapshot inside, and opens like one.
            let ciphertext = collab_types::sealed_ciphertext(&envelope)
                .map_err(|error| DocumentError::Payload(error.to_string()))?;
            Some(
                cipher
                    .open(route.artifact(), &metadata, &ciphertext)
                    .map_err(DocumentError::Payload)?,
            )
        }
        None => None,
    };
    DocumentFrame::decode(decrypted_payload.as_deref().unwrap_or(&envelope.payload))
        .map_err(|error| DocumentError::Payload(error.to_string()))?;
    Ok(Some(OpenedDocument {
        route,
        envelope: *envelope,
        author,
        decrypted_payload,
        whole: None,
        entries: vec![tilo_relay_protocol::compaction::entry_id(body)],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ReadDoc as _, transaction::Transactable as _};
    use tilo_automerge::ApplyIncoming as _;

    #[test]
    fn a_content_key_reader_opens_existing_content_but_cannot_seal() {
        let dir = tempfile::tempdir().unwrap();
        let channel = collab_types::document_channel_id("app", "room", "record");
        let steward = channel_crypto::ChannelSteward::open_or_create(
            dir.path(),
            channel,
            ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
            &channel_crypto::VaultKey::new([8; 32]),
        )
        .unwrap();
        let keys = steward.content_keys().unwrap();
        let artifact = collab_types::ArtifactId::new();
        let (metadata, payload) = keys
            .seal_content(artifact, ContentRetention::Delta, b"existing content")
            .unwrap();
        let mut reader = content_key_reader(keys);
        assert!(reader.holds_epoch(metadata.epoch));
        assert_eq!(
            reader.open(artifact, &metadata, &payload).unwrap(),
            b"existing content"
        );
        assert!(
            reader
                .open(collab_types::ArtifactId::new(), &metadata, &payload)
                .is_err()
        );
        assert_eq!(
            reader
                .seal(artifact, ContentRetention::Delta, b"attempted edit")
                .unwrap_err(),
            "a content-key reader cannot seal documents"
        );
    }

    /// A key holder (a room's governing node) compacts a record's sealed
    /// history into one sealed snapshot wrapped in a compaction; the page
    /// opens it as that snapshot.
    #[test]
    fn a_sealed_compaction_opens_as_its_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let route = DocumentRoute {
            app_id: "app".into(),
            cluster_id: "room".into(),
            record: "record".into(),
        };
        let steward = channel_crypto::ChannelSteward::open_or_create(
            dir.path(),
            collab_types::document_channel_id("app", "room", "record"),
            ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
            &channel_crypto::VaultKey::new([8; 32]),
        )
        .unwrap();
        let keys = steward.content_keys().unwrap();
        let node = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let author = collab_types::Ed25519PublicKey::from_bytes(node.verifying_key().to_bytes());
        let mut merged = AutoCommit::new();
        merged.put(automerge::ROOT, "title", "merged").unwrap();
        let snapshot = DocumentMutation::Snapshot(merged.save());
        let (metadata, ciphertext) = keys
            .seal_content(
                route.artifact(),
                ContentRetention::Checkpoint,
                &snapshot.encode().unwrap(),
            )
            .unwrap();
        let superseded = SignedOp::new_unsigned(
            vec![1],
            route.artifact(),
            author,
            document_attestation_id("app", "room", "record"),
            collab_types::OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        );
        let mut op = SignedOp::new_unsigned_with_encryption(
            collab_types::SealedCompaction::new(&[superseded], ciphertext)
                .unwrap()
                .encode()
                .unwrap(),
            route.artifact(),
            author,
            document_attestation_id("app", "room", "record"),
            collab_types::OpKind::Compaction,
            std::time::SystemTime::UNIX_EPOCH,
            2,
            metadata,
        );
        collab_types::sign_op(&mut op, &node).unwrap();
        let body = postcard::to_stdvec(&NeighbourMsg::AppDocument {
            peer: peer_id(author.as_bytes()),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            record_id: route.record.clone(),
            envelope: Box::new(op),
        })
        .unwrap();
        let mut ciphers = Ciphers::new();
        ciphers.insert(route, content_key_reader(keys));
        let opened = open_frame(&body, *author.as_bytes(), &mut ciphers)
            .unwrap()
            .unwrap();
        assert_eq!(opened.mutation(), Some(snapshot));
    }

    fn opened(
        artifact: collab_types::ArtifactId,
        author: [u8; 32],
        payload: Vec<u8>,
    ) -> OpenedDocument {
        let entry = tilo_relay_protocol::compaction::entry_id(&payload);
        OpenedDocument {
            route: DocumentRoute {
                app_id: "app".into(),
                cluster_id: "room".into(),
                record: "record".into(),
            },
            envelope: SignedOp::new_unsigned(
                payload,
                artifact,
                collab_types::Ed25519PublicKey::from_bytes(author),
                collab_types::AttestationId::new(),
                collab_types::OpKind::Change,
                std::time::SystemTime::UNIX_EPOCH,
                1,
            ),
            author,
            decrypted_payload: None,
            whole: None,
            entries: vec![entry],
        }
    }

    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect()
    }

    #[test]
    fn a_checkpoint_and_its_coalesced_tail_converge_like_one_change_at_a_time() {
        let mut author = AutoCommit::new();
        let text = author
            .put_object(automerge::ROOT, "text", automerge::ObjType::Text)
            .unwrap();
        author.splice_text(&text, 0, 0, "begin ").unwrap();
        let checkpoint = author.save();
        let mut tail = Vec::new();
        for round in 0..50 {
            author
                .splice_text(&text, 0, 0, &format!("round {round} "))
                .unwrap();
            tail.push(DocumentMutation::Incremental(author.save_incremental()));
        }
        let mut mutations = vec![DocumentMutation::Snapshot(checkpoint)];
        mutations.extend(tail.iter().cloned());
        let runs = coalesce(mutations);
        assert_eq!(runs.len(), 2, "one snapshot, one change set");

        let apply = |runs: &[DocumentMutation]| {
            let mut doc = AutoCommit::new();
            for run in runs {
                match run {
                    DocumentMutation::Snapshot(bytes) => {
                        doc.merge_incoming(&mut AutoCommit::load(bytes).unwrap())
                            .unwrap();
                    }
                    DocumentMutation::Incremental(bytes) => {
                        doc.load_incoming(bytes).unwrap();
                    }
                }
            }
            doc
        };
        let mut one_by_one = vec![runs[0].clone()];
        one_by_one.extend(tail);
        let mut batched = apply(&runs);
        let mut single = apply(&one_by_one);
        assert_eq!(batched.get_heads(), author.get_heads());
        assert_eq!(single.get_heads(), batched.get_heads());
        assert_eq!(batched.text(&text).unwrap(), author.text(&text).unwrap());
    }

    #[test]
    fn coalesce_keeps_snapshots_apart_and_joins_incremental_runs() {
        use DocumentMutation::{Incremental, Snapshot};
        let runs = coalesce(vec![
            Incremental(vec![1]),
            Incremental(vec![2, 3]),
            Snapshot(vec![4]),
            Snapshot(vec![5]),
            Incremental(vec![6]),
        ]);
        assert_eq!(
            runs,
            vec![
                Incremental(vec![1, 2, 3]),
                Snapshot(vec![4]),
                Snapshot(vec![5]),
                Incremental(vec![6]),
            ]
        );
    }

    #[test]
    fn segmented_snapshots_reassemble_in_any_order() {
        let mutation = DocumentMutation::Snapshot(noise(5 * 1024 * 1024 / 2));
        let mut frames = mutation.encode_frames().unwrap();
        assert_eq!(frames.len(), 3, "a 2.5 MiB snapshot travels as segments");
        frames.reverse();
        frames.insert(1, frames[0].clone());
        let artifact = collab_types::ArtifactId::new();
        let mut reassembly = Reassembly::default();
        let mut completed = Vec::new();
        for frame in frames {
            let document = opened(artifact, [7; 32], frame);
            assert!(
                document.mutation().is_none(),
                "a segment alone is no mutation"
            );
            if let Some(done) = reassembly.accept(document).unwrap() {
                completed.push(done);
            }
        }
        assert_eq!(completed.len(), 1, "the transfer completes exactly once");
        assert!(completed[0].is_reassembled());
        assert_eq!(
            completed[0].entries().len(),
            3,
            "a reassembled document names every segment it came in, once"
        );
        assert_eq!(completed[0].mutation(), Some(mutation));
        assert_eq!(reassembly.buffered_bytes(), 0);
    }

    #[test]
    fn whole_frames_pass_and_segments_of_other_authors_never_mix() {
        let artifact = collab_types::ArtifactId::new();
        let mut reassembly = Reassembly::default();
        let small = DocumentMutation::Incremental(vec![1, 2, 3]);
        let whole = reassembly
            .accept(opened(artifact, [1; 32], small.encode().unwrap()))
            .unwrap()
            .unwrap();
        assert!(!whole.is_reassembled());
        assert_eq!(whole.mutation(), Some(small));

        let frames = DocumentMutation::Incremental(noise(2 * 1024 * 1024 + 1))
            .encode_frames()
            .unwrap();
        let last = frames.len() - 1;
        for (index, frame) in frames.into_iter().enumerate() {
            let author = if index == last { [2; 32] } else { [1; 32] };
            assert!(
                reassembly
                    .accept(opened(artifact, author, frame))
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn a_tampered_segment_fails_the_transfer_digest() {
        let frames = DocumentMutation::Incremental(noise(2 * 1024 * 1024 + 5))
            .encode_frames()
            .unwrap();
        let artifact = collab_types::ArtifactId::new();
        let mut reassembly = Reassembly::default();
        let count = frames.len();
        let mut outcome = Ok(None);
        for (index, mut frame) in frames.into_iter().enumerate() {
            if index == 1 {
                let at = frame.len() - 3;
                frame[at] ^= 0xff;
            }
            outcome = reassembly.accept(opened(artifact, [3; 32], frame));
            if index + 1 < count {
                assert!(matches!(outcome, Ok(None)));
            }
        }
        assert!(matches!(outcome, Err(DocumentError::Payload(_))));
        assert_eq!(reassembly.buffered_bytes(), 0);
    }
}
