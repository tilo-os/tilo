//! Durable documents on the wire: signed `NeighbourMsg::AppDocument`
//! envelopes, optionally sealed with the route's channel content key, and
//! how a device opens and reassembles the ones its replica admitted. Every
//! reader of retained or live document frames (a native device, a page, a
//! host serving a member's view) opens them here.

use std::collections::BTreeMap;

use crate::join::WorkspaceId;
use collab_types::{
    DocumentFrame, DocumentMutation, DocumentMutationKind, SignedOp, document_artifact_id,
};
use key_group::ContentKeys;
use neighbour_wire::NeighbourMsg;

/// One document route: application, workspace (cluster) and record.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct DocumentRoute {
    /// Application id.
    pub app_id: String,
    /// Workspace uuid, as text.
    pub cluster_id: String,
    /// Record wire name; empty when the app announced none.
    pub document: String,
}

impl DocumentRoute {
    /// The record an app document message writes to; `None` for any other message.
    #[must_use]
    pub fn of_message(message: &neighbour_wire::NeighbourMsg) -> Option<Self> {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            document_id,
            ..
        } = message
        else {
            return None;
        };
        Some(Self {
            app_id: app_id.clone(),
            cluster_id: cluster_id.clone(),
            document: document_id.clone(),
        })
    }
    /// The workspace that carries this route.
    pub fn workspace_id(&self) -> Result<WorkspaceId, DocumentError> {
        WorkspaceId::parse(&self.cluster_id)
            .ok_or_else(|| DocumentError::Route("workspace is not a uuid".into()))
    }

    /// The artifact every frame of this route is signed for.
    #[must_use]
    pub fn artifact(&self) -> collab_types::ArtifactId {
        document_artifact_id(&self.app_id, &self.cluster_id, &self.document)
    }

    /// The independent group for sealing this document.
    pub fn key_scope(&self) -> Result<neighbour_wire::KeyScope, DocumentError> {
        self.key_scope_for(None)
    }

    /// The independent key group for this route and its declared readers.
    pub fn key_scope_for(
        &self,
        readers: Option<&str>,
    ) -> Result<neighbour_wire::KeyScope, DocumentError> {
        neighbour_wire::KeyScope::document(
            self.workspace_id()?,
            &self.app_id,
            &self.document,
            readers,
        )
        .ok_or_else(|| DocumentError::Route("readers do not match the document".into()))
    }
}

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

/// A verified inbound document frame.
#[derive(Clone, Debug)]
pub struct OpenedDocument {
    pub route: DocumentRoute,
    pub envelope: SignedOp,
    /// The authenticated authoring device.
    pub author: [u8; 32],
    /// Cleartext when the envelope was sealed.
    pub decrypted_payload: Option<Vec<u8>>,
}

impl OpenedDocument {
    /// The typed mutation a whole frame carries; `None` for one segment of a
    /// larger transfer, which a record's reader puts back together
    /// ([`Transfers`]).
    pub fn mutation(&self) -> Option<DocumentMutation> {
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
}

struct Transfer {
    kind: DocumentMutationKind,
    total_len: u64,
    segment_len: u32,
    segments: BTreeMap<u32, Vec<u8>>,
}

/// Puts segmented document transfers (a snapshot or change set larger than
/// one frame, see `collab_types::document_transfer`) back together from a
/// record's opened writes: each segment is a write of its own, signed and
/// admitted alone, and a reader takes the transfer once it holds all of them.
/// A transfer is its author's and digest's, so segments of different authors
/// or transfers never mix.
///
/// A writer sends a transfer's segments one after another, before anything
/// else it writes to the record. One that wrote anything else after some of
/// them stopped that transfer (it ended, or the record refused a segment), and
/// its other segments never come: the transfer is let go, and the record reads
/// without it, as the writer's own later writes do. Kept, every reader waited
/// for the rest for good and was refused the whole record as incomplete.
#[derive(Default)]
pub struct Transfers(BTreeMap<([u8; 32], [u8; 32]), Transfer>);

impl Transfers {
    /// The mutation `plaintext`, a write of `author`, carries whole or
    /// completes; `None` while its transfer waits for more segments.
    ///
    /// # Errors
    ///
    /// The payload does not decode, a segment contradicts its transfer, or
    /// the complete transfer fails its digest or does not read as its kind.
    pub fn take(
        &mut self,
        author: [u8; 32],
        plaintext: &[u8],
    ) -> Result<Option<DocumentMutation>, String> {
        let frame = DocumentFrame::decode(plaintext).map_err(|error| error.to_string())?;
        let going_on = match &frame {
            DocumentFrame::Whole(_) => None,
            DocumentFrame::Segment(segment) => Some(segment.digest),
        };
        self.0
            .retain(|(writer, digest), _| *writer != author || Some(*digest) == going_on);
        let segment = match frame {
            DocumentFrame::Whole(mutation) => return Ok(Some(mutation)),
            DocumentFrame::Segment(segment) => segment,
        };
        let key = (author, segment.digest);
        let transfer = self.0.entry(key).or_insert_with(|| Transfer {
            kind: segment.kind,
            total_len: segment.total_len,
            segment_len: segment.segment_len,
            segments: BTreeMap::new(),
        });
        if transfer.kind != segment.kind
            || transfer.total_len != segment.total_len
            || transfer.segment_len != segment.segment_len
        {
            self.0.remove(&key);
            return Err("document segment contradicts its transfer".into());
        }
        let count = segment.count() as usize;
        transfer
            .segments
            .entry(segment.index)
            .or_insert(segment.data);
        if transfer.segments.len() < count {
            return Ok(None);
        }
        let Some(transfer) = self.0.remove(&key) else {
            return Ok(None);
        };
        let data: Vec<u8> = transfer.segments.into_values().flatten().collect();
        if u64::try_from(data.len()).ok() != Some(transfer.total_len)
            || collab_types::document_transfer::transfer_digest(transfer.kind, &data) != key.1
        {
            return Err("reassembled document does not match its transfer digest".into());
        }
        // Joined, the segments are the body their writer cut up: collab-types reads it,
        // as it reads the body of a whole frame.
        DocumentMutation::of_transfer(transfer.kind, &data)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    /// Whether a transfer of changes still waits for segments: a record read
    /// without it lacks what it writes. A whole record (a key holder's
    /// checkpoint) only states again what the record's writes hold.
    pub fn waits_for_changes(&self) -> bool {
        self.0
            .values()
            .any(|transfer| transfer.kind == DocumentMutationKind::Incremental)
    }

    /// Whether a transfer still waits for segments.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
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
    match NeighbourMsg::decode(body) {
        Ok(NeighbourMsg::AppDocument { envelope, .. }) => {
            envelope.claims.iter().map(|claim| claim.change).collect()
        }
        _ => Vec::new(),
    }
}

/// Open one inbound host delta, a document its workspace's governance admitted
/// from `author`, the device the host stamped from its authenticated
/// connection, sealed content with its route's `keys`. Returns `Ok(None)` for
/// traffic that is not an application document (shell tree, governance,
/// continuity).
///
/// # Errors
///
/// Returns [`DocumentError`] for a payload that does not open.
pub fn open_frame(
    body: &[u8],
    author: [u8; 32],
    keys: Option<&ContentKeys>,
) -> Result<Option<OpenedDocument>, DocumentError> {
    let Ok(NeighbourMsg::AppDocument {
        app_id,
        cluster_id,
        document_id,
        envelope,
        ..
    }) = NeighbourMsg::decode(body)
    else {
        return Ok(None);
    };
    let route = DocumentRoute {
        app_id,
        cluster_id,
        document: document_id,
    };
    let decrypted_payload = match envelope.encryption {
        Some(metadata) => {
            collab_types::validate_document_payload_route(
                &route.app_id,
                &route.cluster_id,
                &route.document,
                &envelope,
            )
            .map_err(|error| DocumentError::Payload(error.to_string()))?;
            let keys =
                keys.ok_or_else(|| DocumentError::Payload("route has no content key".into()))?;
            // A key holder's compaction of the record's sealed history
            // carries its sealed snapshot inside, and opens like one.
            let ciphertext = collab_types::sealed_ciphertext(&envelope)
                .map_err(|error| DocumentError::Payload(error.to_string()))?;
            Some(
                keys.open_content(envelope.artifact, &metadata, &ciphertext)
                    .map_err(|error| DocumentError::Payload(error.to_string()))?
                    .to_vec(),
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
    }))
}

/// A record as its writer reads it: `base`, the record's saved bytes as the replica holds them
/// (`None` when it holds none of it, or cannot open it yet), with `frames`, writes of `author` the
/// replica has not taken in yet (they wait for the record's keys or for workspace held), merged
/// over it. Only the writer reads these: nobody else holds them until they land.
///
/// # Errors
/// A frame or the base does not decode as the record's document.
pub fn with_own_writes(
    base: Option<&[u8]>,
    author: [u8; 32],
    frames: &[Vec<u8>],
) -> Result<OwnRead, String> {
    let mut document = match base {
        Some(bytes) => {
            // The replica's read this base comes from kept the document these bytes were saved
            // from (`tilo_automerge::shared`): a copy, not a decode of the whole document again.
            tilo_automerge::shared::load(bytes).map_err(|error| error.to_string())?
        }
        None => automerge::AutoCommit::new(),
    };
    let mut transfers = Transfers::default();
    for frame in frames {
        if let Some(mutation) = transfers.take(author, frame)? {
            let (DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes)) =
                mutation;
            tilo_automerge::ApplyIncoming::load_incoming(&mut document, &bytes)
                .map_err(|error| error.to_string())?;
        }
    }
    let mut heads = document.get_heads();
    heads.sort();
    let revision = crate::committed_document_revision(&heads);
    Ok(OwnRead {
        bytes: document.save(),
        heads: heads.into_iter().map(|head| head.0).collect(),
        revision,
    })
}

/// The changes `frames`, writes of `author` the replica has not taken in yet, carry, in the order
/// they were written: what a writer's read adds over the replica's. `None` when one of them carries
/// the record whole (a snapshot), which only a whole read ([`with_own_writes`]) merges.
///
/// # Errors
/// A frame does not decode as the record's changes.
pub fn own_changes(author: [u8; 32], frames: &[Vec<u8>]) -> Result<Option<Vec<OwnChange>>, String> {
    let mut transfers = Transfers::default();
    let mut changes = Vec::new();
    for frame in frames {
        match transfers.take(author, frame)? {
            Some(DocumentMutation::Incremental(bytes)) => changes.extend(
                automerge::Automerge::parse_changes(&bytes)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|change| OwnChange {
                        hash: change.hash().0,
                        deps: change.deps().iter().map(|dep| dep.0).collect(),
                        bytes: change.raw_bytes().to_vec(),
                    }),
            ),
            Some(DocumentMutation::Snapshot(_)) => return Ok(None),
            None => {}
        }
    }
    Ok(Some(changes))
}

/// What a reader holding `heads` of a record lacks of it as its writer reads it: the replica's
/// changes since what the reader holds of the replica, then those of `own`, the writer's changes
/// the replica has not taken in yet, the reader does not hold. `replica` reads the replica's
/// changes since the sorted heads it is handed, with the heads it then stands at, or `None` where
/// only a whole read answers. Answers the changes and the sorted heads the record then stands at;
/// `None` where only a whole read answers.
pub fn read_after_own(
    heads: &[[u8; 32]],
    own: &[OwnChange],
    replica: impl FnOnce(&[[u8; 32]]) -> Option<(Vec<u8>, Vec<[u8; 32]>)>,
) -> Option<(Vec<u8>, Vec<[u8; 32]>)> {
    use std::collections::{BTreeSet, HashMap, HashSet};
    let written: HashMap<[u8; 32], &OwnChange> =
        own.iter().map(|change| (change.hash, change)).collect();
    // What the reader holds of the replica: its heads, followed down through the writes it holds
    // to what they build on.
    let mut base: BTreeSet<[u8; 32]> = BTreeSet::new();
    let mut reached: HashSet<[u8; 32]> = HashSet::new();
    let mut stack = heads.to_vec();
    while let Some(head) = stack.pop() {
        match written.get(&head) {
            Some(change) => {
                if reached.insert(head) {
                    stack.extend(change.deps.iter().copied());
                }
            }
            None => {
                base.insert(head);
            }
        }
    }
    let base: Vec<[u8; 32]> = base.into_iter().collect();
    if base.is_empty() {
        return None;
    }
    let (mut bytes, replica_heads) = replica(&base)?;
    // The writes build on the replica and on each other: what they build on is a head no more.
    let built_on: HashSet<[u8; 32]> = own
        .iter()
        .flat_map(|change| change.deps.iter().copied())
        .collect();
    let mut now: BTreeSet<[u8; 32]> = replica_heads
        .into_iter()
        .filter(|head| !built_on.contains(head))
        .collect();
    for change in own {
        if !built_on.contains(&change.hash) {
            now.insert(change.hash);
        }
        if !reached.contains(&change.hash) {
            bytes.extend_from_slice(&change.bytes);
        }
    }
    Some((bytes, now.into_iter().collect()))
}

/// One change a writer's held write carries ([`own_changes`]).
pub struct OwnChange {
    /// Its hash.
    pub hash: [u8; 32],
    /// The changes it builds on.
    pub deps: Vec<[u8; 32]>,
    /// It, encoded as a record's history keeps it.
    pub bytes: Vec<u8>,
}

/// A record as its writer reads it ([`with_own_writes`]).
pub struct OwnRead {
    /// The document, saved.
    pub bytes: Vec<u8>,
    /// Its heads, sorted.
    pub heads: Vec<[u8; 32]>,
    /// The revision those heads name.
    pub revision: [u8; 32],
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ReadDoc as _, transaction::Transactable as _};
    use collab_types::document_attestation_id;
    use tilo_automerge::ApplyIncoming as _;

    tilo_test_env::scenario! {
        fn a_documents_frames_open_to_the_document_they_carry;

        phase a_sealed_compaction_opens_as_its_snapshot;
        {
            let dir = tempfile::tempdir().unwrap();
            let route = DocumentRoute {
                app_id: "app".into(),
                cluster_id: "workspace".into(),
                document: "record".into(),
            };
            let creator = key_group::GroupPeer::create_now(
                Some(std::sync::Arc::new(key_group::Files::new(dir.path()))),
                collab_types::WorkspaceId::new(),
                Box::new(ed25519_dalek::SigningKey::from_bytes(&[7; 32])),
                &key_group::VaultKey::new([8; 32]),
            )
            .unwrap();
            let keys = creator.content_keys().unwrap();
            let node = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let author = collab_types::Ed25519PublicKey::from_bytes(node.verifying_key().to_bytes());
            let mut merged = AutoCommit::new();
            merged.put(automerge::ROOT, "title", "merged").unwrap();
            let snapshot = DocumentMutation::Snapshot(merged.save());
            let (metadata, ciphertext) = keys
                .seal_content(
                    route.artifact(),
                    collab_types::ContentRetention::Checkpoint,
                    &snapshot.encode().unwrap(),
                )
                .unwrap();
            let superseded = SignedOp::new_unsigned(
                vec![1],
                route.artifact(),
                author,
                document_attestation_id("app", "workspace", "record"),
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
                document_attestation_id("app", "workspace", "record"),
                collab_types::OpKind::Compaction,
                std::time::SystemTime::UNIX_EPOCH,
                2,
                metadata,
            );
            collab_types::sign_op(&mut op, &node).unwrap();
            let body = postcard::to_stdvec(&NeighbourMsg::AppDocument {
                peer: neighbour_wire::peer_handle(author.as_bytes()),
                app_id: route.app_id.clone(),
                cluster_id: route.cluster_id.clone(),
                document_id: route.document,
                envelope: Box::new(op),
                readers: None,
            })
            .unwrap();
            let opened = open_frame(&body, *author.as_bytes(), Some(&keys))
                .unwrap()
                .unwrap();
            assert_eq!(opened.mutation(), Some(snapshot));
        }

        phase a_checkpoint_and_its_coalesced_tail_converge_like_one_change_at_a_time;
        {
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

        phase a_writer_reads_its_held_writes_over_what_the_replica_holds;
        {
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "kept", "landed").unwrap();
            let base = doc.save();
            doc.put(automerge::ROOT, "held", "waiting for keys")
                .unwrap();
            let frames = DocumentMutation::Incremental(doc.save_incremental())
                .encode_frames()
                .unwrap();
            let read = with_own_writes(Some(&base), [1; 32], &frames).unwrap();
            let read_back = AutoCommit::load(&read.bytes).unwrap();
            assert!(read_back.get(automerge::ROOT, "kept").unwrap().is_some());
            assert!(read_back.get(automerge::ROOT, "held").unwrap().is_some());
            let alone = with_own_writes(None, [1; 32], &frames).unwrap();
            assert_ne!(alone.revision, read.revision);
        }

        phase a_base_the_replica_kept_a_document_of_reads_as_that_document;
        {
            // A sealed document's read hands out the bytes it saved and keeps the document they
            // were saved from. A writer's read over them takes that document: the copy held with
            // these bytes says so by holding a change they do not.
            let mut landing = AutoCommit::new();
            let text = landing
                .put_object(automerge::ROOT, "body", automerge::ObjType::Text)
                .unwrap();
            // Long enough that its save is one of the documents the cache keeps. Varied words
            // do not compress: a repetition of one would save to a few kilobytes.
            let words: Vec<String> = noise(60_000)
                .iter()
                .map(u8::to_string)
                .collect();
            landing.splice_text(&text, 0, 0, &words.join(" ")).unwrap();
            let base = landing.save();
            let mut kept = landing.fork();
            kept.put(automerge::ROOT, "kept", "the document the bytes were saved from")
                .unwrap();
            kept.commit();
            tilo_automerge::shared::hold(&base, &kept);
            let read = with_own_writes(Some(&base), [1; 32], &[]).unwrap();
            let read_back = AutoCommit::load(&read.bytes).unwrap();
            assert_eq!(
                read_back
                    .get(automerge::ROOT, "kept")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("the document the bytes were saved from"),
                "the document kept with the bytes, not a decode of them"
            );
            tilo_automerge::shared::clear();
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

    tilo_test_env::scenario! {
        fn frames_and_segments_reassemble_only_as_they_were_sent;

        phase coalesce_keeps_snapshots_apart_and_joins_incremental_runs;
        {
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

        phase segmented_snapshots_reassemble_in_any_order;
        {
            let mutation = DocumentMutation::Snapshot(noise(5 * 1024 * 1024 / 2));
            let mut frames = mutation.encode_frames().unwrap();
            assert_eq!(frames.len(), 3, "a 2.5 MiB snapshot travels as segments");
            frames.reverse();
            frames.insert(1, frames[0].clone());
            let mut transfers = Transfers::default();
            let completed: Vec<_> = frames
                .iter()
                .filter_map(|frame| transfers.take([7; 32], frame).unwrap())
                .collect();
            assert_eq!(
                completed,
                vec![mutation],
                "the transfer completes exactly once"
            );
            assert!(transfers.is_empty());
        }

        phase whole_frames_pass_and_segments_of_other_authors_never_mix;
        {
            let mut transfers = Transfers::default();
            let small = DocumentMutation::Incremental(vec![1, 2, 3]);
            assert_eq!(
                transfers.take([1; 32], &small.encode().unwrap()).unwrap(),
                Some(small)
            );
            let frames = DocumentMutation::Incremental(noise(2 * 1024 * 1024 + 1))
                .encode_frames()
                .unwrap();
            let last = frames.len() - 1;
            for (index, frame) in frames.iter().enumerate() {
                let author = if index == last { [2; 32] } else { [1; 32] };
                assert_eq!(transfers.take(author, frame).unwrap(), None);
            }
            assert!(!transfers.is_empty());
        }

        phase a_tampered_segment_fails_the_transfer_digest;
        {
            let frames = DocumentMutation::Incremental(noise(2 * 1024 * 1024 + 5))
                .encode_frames()
                .unwrap();
            let mut transfers = Transfers::default();
            let count = frames.len();
            let mut outcome = Ok(None);
            for (index, mut frame) in frames.into_iter().enumerate() {
                if index == 1 {
                    let at = frame.len() - 3;
                    frame[at] ^= 0xff;
                }
                outcome = transfers.take([3; 32], &frame);
                if index + 1 < count {
                    assert_eq!(outcome, Ok(None));
                }
            }
            assert!(outcome.is_err());
            assert!(transfers.is_empty());
        }

        phase segmented_changes_reassemble_as_the_changes_written;
        {
            // A file put into a document: changes that do not compress, larger than one segment.
            // A reader takes their segments joined as the changes were written.
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "title", "before the file").unwrap();
            let base = doc.save();
            doc.put(automerge::ROOT, "file", noise(3 * 1024 * 1024 / 2))
                .unwrap();
            let mutation = DocumentMutation::Incremental(doc.save_incremental());
            let frames = mutation.encode_frames().unwrap();
            assert!(frames.len() > 1, "the changes travel as segments");
            let mut transfers = Transfers::default();
            let completed: Vec<_> = frames
                .iter()
                .filter_map(|frame| transfers.take([7; 32], frame).unwrap())
                .collect();
            assert!(
                completed.len() == 1 && completed.first() == Some(&mutation),
                "the transfer completes once, as the changes that were written"
            );
            assert!(transfers.is_empty());
            // A document that holds such a write opens with it.
            let read = with_own_writes(Some(&base), [7; 32], &frames).unwrap();
            let mut read_back = AutoCommit::load(&read.bytes).unwrap();
            assert_eq!(read_back.get_heads(), doc.get_heads());
            assert!(read_back.get(automerge::ROOT, "file").unwrap().is_some());
        }

        phase segmented_changes_that_are_not_changes_are_refused_where_they_are_read;
        {
            // Segments a writer signed over bytes that are no changes: the digest is of those
            // bytes, so the transfer completes, and what it carries does not read as the
            // document's changes.
            let frames = DocumentMutation::Incremental(noise(2 * 1024 * 1024 + 5))
                .encode_frames()
                .unwrap();
            let segments: Vec<collab_types::DocumentSegment> = frames
                .iter()
                .map(|frame| match DocumentFrame::decode(frame).unwrap() {
                    DocumentFrame::Segment(segment) => segment,
                    DocumentFrame::Whole(_) => panic!("the changes travel as segments"),
                })
                .collect();
            let kind = segments[0].kind;
            let unreadable =
                vec![0xff_u8; segments.iter().map(|segment| segment.data.len()).sum()];
            let digest = collab_types::document_transfer::transfer_digest(kind, &unreadable);
            let mut transfers = Transfers::default();
            let mut corrupted = Vec::with_capacity(frames.len());
            let mut outcome = Ok(None);
            for (frame, segment) in frames.iter().zip(&segments) {
                let mut frame = frame.clone();
                let header = frame.len() - segment.data.len();
                let at = frame[..header]
                    .windows(32)
                    .position(|window| window == segment.digest.as_slice())
                    .expect("a segment names its transfer's digest");
                frame[at..at + 32].copy_from_slice(&digest);
                frame[header..].fill(0xff);
                outcome = transfers.take([5; 32], &frame);
                corrupted.push(frame);
            }
            assert_eq!(
                outcome,
                Ok(Some(DocumentMutation::Incremental(unreadable))),
                "the transfer completes as the bytes its writer signed"
            );
            assert!(transfers.is_empty());
            assert!(
                own_changes([5; 32], &corrupted).is_err(),
                "bytes that are no changes do not read as the document's changes"
            );
        }
    }

    /// A record's heads, sorted.
    fn sorted_heads(doc: &mut AutoCommit) -> Vec<[u8; 32]> {
        let mut heads: Vec<[u8; 32]> = doc.get_heads().iter().map(|head| head.0).collect();
        heads.sort_unstable();
        heads
    }

    /// A writer's changes since `heads` of `doc`, as its held writes carry them.
    fn changes_of(doc: &mut AutoCommit, heads: &[automerge::ChangeHash]) -> Vec<OwnChange> {
        doc.get_changes(heads)
            .iter()
            .map(|change| OwnChange {
                hash: change.hash().0,
                deps: change.deps().iter().map(|dep| dep.0).collect(),
                bytes: change.raw_bytes().to_vec(),
            })
            .collect()
    }

    /// A reader holding a record anywhere along its writer's held writes is brought to the record
    /// as its writer reads it with the replica's changes since and the writes it lacks: the same
    /// document a whole read gives, never read whole.
    #[test]
    fn a_reader_is_brought_past_a_writers_held_writes_without_a_whole_read() {
        let mut replica = AutoCommit::new();
        replica.set_actor(automerge::ActorId::from(b"replica".as_slice()));
        replica.put(automerge::ROOT, "title", "Tracker").unwrap();
        replica.commit();
        let at_start = replica.clone();
        let start = replica.get_heads();

        // The writer writes three changes the replica has not taken in yet ...
        let mut writer = replica.fork();
        writer.set_actor(automerge::ActorId::from(b"writer".as_slice()));
        for at in 0..3 {
            writer
                .put(automerge::ROOT, format!("own-{at}"), at)
                .unwrap();
            writer.commit();
        }
        let own = changes_of(&mut writer, &start);
        // ... while the replica takes in two from elsewhere.
        for at in 0..2 {
            replica
                .put(automerge::ROOT, format!("other-{at}"), at)
                .unwrap();
            replica.commit();
        }
        let mut whole = replica.clone();
        for change in &own {
            whole.load_incoming(&change.bytes).unwrap();
        }
        let whole_heads = sorted_heads(&mut whole);

        let read = |heads: &[[u8; 32]]| {
            let mut asked = 0;
            let answer = read_after_own(heads, &own, |base| {
                asked += 1;
                let base: Vec<automerge::ChangeHash> =
                    base.iter().copied().map(automerge::ChangeHash).collect();
                let mut replica = replica.clone();
                let bytes = replica
                    .get_changes(&base)
                    .iter()
                    .flat_map(|change| change.raw_bytes().to_vec())
                    .collect();
                Some((bytes, sorted_heads(&mut replica)))
            });
            assert_eq!(asked, 1, "the replica is asked once");
            answer.expect("the reader's heads are the record's")
        };

        // A reader at the start, one that read the writer's first write, and one that read the
        // writer's own copy: each lands on the whole read.
        let mut first = at_start.clone();
        let mut readers = vec![at_start];
        first.load_incoming(&own[0].bytes).unwrap();
        readers.push(first);
        readers.push(writer);
        for mut reader in readers {
            let (bytes, heads) = read(&sorted_heads(&mut reader));
            reader.load_incoming(&bytes).unwrap();
            assert_eq!(heads, whole_heads, "the heads the record then stands at");
            assert_eq!(sorted_heads(&mut reader), whole_heads);
            assert_eq!(
                reader.keys(automerge::ROOT).collect::<Vec<_>>(),
                whole.keys(automerge::ROOT).collect::<Vec<_>>()
            );
        }

        // A reader already there is handed nothing.
        let (bytes, heads) = read(&whole_heads);
        assert!(bytes.is_empty(), "{} bytes", bytes.len());
        assert_eq!(heads, whole_heads);

        // Heads the replica and the writes do not hold answer only whole.
        let mut asked = false;
        let none = read_after_own(&[], &own, |_| {
            asked = true;
            None
        });
        assert!(none.is_none() && !asked, "no heads, no read of the replica");
        assert!(read_after_own(&whole_heads, &own, |_| None).is_none());
    }
}
