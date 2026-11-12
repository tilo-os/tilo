//! Same-barrier native outgoing custody; relay queue acceptance is never an ACK.
use crate::StatePeer;
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner, WorkspaceId, enrollment, record};
use crate::owner_commit::{CommitBinding, CommitOperation, OwnerCommitRequest};
use collab_types::{EncryptionMetadata, OpKind, SignedOp};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use neighbour_wire::Retention;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use web_time::SystemTime;

pub(crate) const MAX_ENTRIES: usize = 64;
/// What a commit is refused with while this device's custody holds as many writes as it
/// keeps: it may commit again once the host answered some of them.
pub const CUSTODY_FULL: &str = "native outgoing capacity exceeded; existing edits retained";
pub(crate) const MAX_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_VISIBLE_ENTRIES: usize = MAX_ENTRIES + 1;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredPublication {
    version: u32,
    revision: u64,
    next_ordinal: u64,
    #[serde(deserialize_with = "read_entries")]
    pub(crate) entries: Vec<StoredOutgoing>,
    /// Recovery projections an earlier build signed for its expiring
    /// frames. This build re-sends the signed originals instead, so the list
    /// is read and then dropped (`file_store::read_record`); it stays in the
    /// format so records that hold one still load.
    #[serde(
        default,
        deserialize_with = "read_entries",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub(crate) projection: Vec<StoredOutgoing>,
    #[serde(default, deserialize_with = "read_entries")]
    governance: Vec<StoredOutgoing>,
    /// What the workspace's relay host answered to this device's writes, per record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    answered: Vec<AnsweredRecord>,
}
/// The relay host's answers to this device's writes to one record, kept with the custody they
/// left.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AnsweredRecord {
    app: String,
    record: String,
    durable: u64,
    refused: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_order: Option<u64>,
}
/// What a workspace's relay host answered to one of this device's writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAnswer {
    /// Kept durably, at this place in the host's order.
    Durable(u64),
    /// Refused: it never becomes durable.
    Refused,
}
/// This device's writes to one record: those still in its custody, and what the workspace's
/// relay host answered to the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordWrites {
    pub pending: usize,
    pub durable: u64,
    pub refused: u64,
    /// The host's order of the latest write it kept.
    pub last_order: Option<u64>,
}
impl Default for StoredPublication {
    fn default() -> Self {
        Self {
            version: 3,
            revision: 1,
            next_ordinal: 1,
            entries: Vec::new(),
            projection: Vec::new(),
            governance: Vec::new(),
            answered: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredOutgoing {
    ordinal: u64,
    id: [u8; 32],
    /// Base64 text in the owner's JSON files, like the record's other
    /// opaque bytes; entries written before that held JSON arrays of
    /// numbers, one line per byte pretty-printed, and still read back.
    #[serde(
        serialize_with = "crate::owner::record::base64_bytes::serialize",
        deserialize_with = "read_message_bytes"
    )]
    pub(crate) bytes: Vec<u8>,
    binding: CommitBinding,
    timestamp_ns: u64,
    channel: [u8; 32],
    incremental: bool,
}
pub struct NativeOutgoing<'a> {
    state: &'a StoredPublication,
    allowed: [bool; MAX_VISIBLE_ENTRIES],
}
pub struct NativeOutgoingEntry<'a>(&'a StoredOutgoing);
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeOutgoingStatus {
    pub pending: usize,
    pub blocked: usize,
}
impl<'a> NativeOutgoing<'a> {
    /// Every entry still in custody, whether or not it may be sent now.
    pub fn queued(&self) -> impl Iterator<Item = [u8; 32]> + 'a {
        self.state
            .governance
            .iter()
            .chain(self.state.entries.iter())
            .map(|entry| entry.id)
    }
    pub fn entries(&self) -> impl Iterator<Item = NativeOutgoingEntry<'a>> {
        let allowed = self.allowed;
        self.state
            .governance
            .iter()
            .chain(self.state.entries.iter())
            .enumerate()
            .filter(move |(index, _)| allowed.get(*index).copied().unwrap_or(false))
            .map(|(_, entry)| NativeOutgoingEntry(entry))
    }
}
impl<'a> NativeOutgoingEntry<'a> {
    #[must_use]
    pub fn id(&self) -> [u8; 32] {
        self.0.id
    }
    #[must_use]
    pub fn bytes(&self) -> &'a [u8] {
        &self.0.bytes
    }
    #[must_use]
    pub fn channel(&self) -> [u8; 32] {
        self.0.channel
    }
    /// How the store keeps this entry: a write appends after the record's
    /// checkpoint, while a checkpoint of this author's own — the workspace's
    /// governance snapshot or its sealed compaction of a record — stands as
    /// the whole of what it retained there, so the store drops the frames it
    /// covers.
    #[must_use]
    pub fn retention(&self) -> Retention {
        if self.0.incremental {
            Retention::Delta
        } else {
            Retention::Checkpoint
        }
    }
}
fn read_entries<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<Vec<StoredOutgoing>, D::Error> {
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = Vec<StoredOutgoing>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded native publication entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut input: A,
        ) -> Result<Self::Value, A::Error> {
            if input.size_hint().is_some_and(|size| size > MAX_ENTRIES) {
                return Err(serde::de::Error::custom("native outgoing entry capacity"));
            }
            let mut entries = Vec::new();
            let mut bytes = 0usize;
            loop {
                if entries.len() == MAX_ENTRIES {
                    if input.next_element::<serde::de::IgnoredAny>()?.is_some() {
                        return Err(serde::de::Error::custom("native outgoing entry capacity"));
                    }
                    return Ok(entries);
                }
                let Some(entry) = input.next_element::<StoredOutgoing>()? else {
                    return Ok(entries);
                };
                bytes = bytes
                    .checked_add(entry.bytes.len())
                    .filter(|bytes| *bytes <= MAX_BYTES)
                    .ok_or_else(|| serde::de::Error::custom("native outgoing byte capacity"))?;
                entries.push(entry);
            }
        }
    }
    input.deserialize_seq(Entries)
}
fn read_bytes<'de, D: serde::Deserializer<'de>>(
    input: D,
    limit: usize,
) -> Result<Vec<u8>, D::Error> {
    struct Bytes(usize);
    impl<'de> serde::de::Visitor<'de> for Bytes {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded native publication bytes")
        }
        fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
            use base64::{Engine as _, engine::general_purpose::STANDARD};
            // Refuse oversized text before decoding any of it.
            if text.len() > self.0.div_ceil(3).saturating_mul(4) {
                return Err(E::custom("native publication bytes exceed limit"));
            }
            let bytes = STANDARD.decode(text).map_err(E::custom)?;
            if bytes.len() > self.0 {
                return Err(E::custom("native publication bytes exceed limit"));
            }
            Ok(bytes)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut input: A,
        ) -> Result<Self::Value, A::Error> {
            if input.size_hint().is_some_and(|size| size > self.0) {
                return Err(serde::de::Error::custom(
                    "native publication bytes exceed limit",
                ));
            }
            let mut bytes = Vec::new();
            while let Some(byte) = input.next_element::<u8>()? {
                if bytes.len() == self.0 {
                    return Err(serde::de::Error::custom(
                        "native publication bytes exceed limit",
                    ));
                }
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }
    // Text or the legacy array in JSON; the unchanged sequence in a binary
    // encoding, which writes one.
    if input.is_human_readable() {
        input.deserialize_any(Bytes(limit))
    } else {
        input.deserialize_seq(Bytes(limit))
    }
}
fn read_message_bytes<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Vec<u8>, D::Error> {
    read_bytes(input, MAX_BYTES)
}
fn fresh(entry: &StoredOutgoing, now: SystemTime) -> bool {
    let Some(timestamp) =
        SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_nanos(entry.timestamp_ns))
    else {
        return false;
    };
    let skew = std::time::Duration::from_secs(300);
    timestamp >= now.checked_sub(skew).unwrap_or(SystemTime::UNIX_EPOCH)
        && timestamp <= now.checked_add(skew).unwrap_or(now)
}
pub(crate) fn state(view: &NativeCommittedView) -> Result<&StoredPublication, String> {
    state_of(&view.record)
}

/// The outgoing custody of a record, published or in the making.
pub(crate) fn state_of(record: &record::StoredWorkspace) -> Result<&StoredPublication, String> {
    record
        .publication
        .as_ref()
        .ok_or_else(|| "native publication state is missing".into())
}
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn status(
    view: &NativeCommittedView,
    now: SystemTime,
) -> Result<NativeOutgoingStatus, String> {
    let publication = state(view)?;
    let allowed = allowed(view, now)?;
    // `bounds` caps both lists, so neither sum nor difference can wrap.
    let total = publication
        .governance
        .len()
        .saturating_add(publication.entries.len());
    Ok(NativeOutgoingStatus {
        pending: total,
        blocked: total.saturating_sub(allowed.iter().filter(|allowed| **allowed).count()),
    })
}
fn allowed(
    view: &NativeCommittedView,
    now: SystemTime,
) -> Result<[bool; MAX_VISIBLE_ENTRIES], String> {
    let publication = state(view)?;
    bounds(publication)?;
    let mut allowed = [false; MAX_VISIBLE_ENTRIES];
    for (index, entry) in publication.governance.iter().enumerate() {
        if let Some(slot) = allowed.get_mut(index) {
            *slot = validate_governance(view, entry).is_ok();
        }
    }
    let governance_len = publication.governance.len();
    for (index, entry) in publication.entries.iter().enumerate() {
        let message: NeighbourMsg =
            postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
        let NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } = message
        else {
            return Err("invalid native outgoing document".into());
        };
        // A sealed write expires with the channel epoch it was made under:
        // its key holder renews it ([`NativeWorkspaceOwner::renew_outgoing`]),
        // and the relay never re-sends the expired original beside a renewal.
        // A cleartext frame is re-sent as it was signed, however old it is.
        if envelope.encryption.is_some() && !fresh(entry, now) {
            continue;
        }
        if let Some(slot) = allowed.get_mut(governance_len.saturating_add(index)) {
            *slot = envelope.author.as_bytes() == &view.device().device_public_key
                && view
                    .peer
                    .lookup_exact_local_document(&app_id, &record_id, &envelope, now)
                    .ok()
                    .flatten()
                    .is_some();
        }
    }
    Ok(allowed)
}
/// Whether a host file of layout `version` may carry `storage`. Workspace
/// records came in with layout 4; later layouts carry them as it does.
pub fn validate_file_layout(
    version: u32,
    storage: &record::NativeAuthorityStorage,
) -> Result<(), String> {
    for workspace in storage.workspaces.values() {
        match (version, workspace.version, workspace.publication.is_some()) {
            (4.., 2, true) if workspace.effect_history_root.is_none() => {}
            (4.., 3, true) if workspace.effect_history_root.is_some() => {}
            _ => return Err("native publication record/file layout mismatch".into()),
        }
        if let Some(publication) = &workspace.publication {
            bounds(publication)?;
        }
    }
    Ok(())
}
fn bounds(publication: &StoredPublication) -> Result<usize, String> {
    if !matches!(publication.version, 2 | 3)
        || (publication.version == 2 && !publication.governance.is_empty())
        || publication.revision == 0
        || publication.next_ordinal == 0
        || publication.entries.len() > MAX_ENTRIES
        || publication.governance.len() > 1
    {
        return Err("invalid native outgoing publication or entry capacity".into());
    }
    let bytes = publication
        .governance
        .iter()
        .chain(publication.entries.iter())
        .try_fold(0usize, |bytes, entry| {
            bytes
                .checked_add(entry.bytes.len())
                .ok_or("native outgoing byte overflow")
        })?;
    if bytes > MAX_BYTES {
        return Err("native outgoing byte capacity exceeded".into());
    }
    Ok(bytes)
}
pub(crate) fn request(message: &NeighbourMsg) -> Result<OwnerCommitRequest, String> {
    let NeighbourMsg::AppDocument {
        app_id,
        cluster_id,
        record_id,
        envelope,
        ..
    } = message
    else {
        return Err("native outgoing entry must be an exact document".into());
    };
    Ok(OwnerCommitRequest {
        workspace: cluster_id.clone(),
        operation: CommitOperation::Document {
            app: app_id.clone(),
            record: record_id.clone(),
            envelope: envelope.clone(),
        },
    })
}
pub(crate) fn metadata(message: &NeighbourMsg) -> Result<(u64, [u8; 32], bool), String> {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        return Err("native outgoing requires document".into());
    };
    // The one host classifier (`neighbour_wire::retention`), shared with
    // browser peers and the relay. A single-use MLS application message has
    // no retention class and is refused here like any live frame.
    let (channel, kind) = neighbour_wire::retention(message, false)
        .ok_or("unsupported native outgoing document payload")?;
    Ok((
        enrollment::ns(envelope.timestamp)?,
        channel,
        member_publishes_as_delta(kind, envelope.encryption.is_some(), envelope.kind),
    ))
}
/// Whether a member publishes this document as a retained delta.
///
/// A cleartext snapshot keeps the checkpoint class the classifier gives it:
/// it is the record's whole state, and the class tells the store it stands as
/// the record's checkpoint rather than one more change after it.
///
/// A content-key ciphertext the classifier calls a checkpoint is published
/// as a delta instead, unless it is a compaction: the host then appends it
/// after the record's checkpoint rather than letting a member's ordinary
/// write replace it. A sealed compaction is the one member checkpoint that
/// leaves as itself, because it is the one that covers the sealed history
/// before it, which every store then drops.
fn member_publishes_as_delta(kind: Retention, sealed: bool, op: OpKind) -> bool {
    match kind {
        Retention::Delta => true,
        // A cleartext snapshot is the governor's baseline, whoever writes it.
        Retention::Checkpoint if !sealed => false,
        Retention::Checkpoint => op != OpKind::Compaction,
    }
}
struct Count {
    bytes: usize,
    limit: usize,
}
impl postcard::ser_flavors::Flavor for Count {
    type Output = usize;
    fn try_push(&mut self, _: u8) -> postcard::Result<()> {
        self.try_extend(&[0])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|bytes| *bytes <= self.limit)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.bytes)
    }
}
pub(crate) fn encode(message: &NeighbourMsg, limit: usize) -> Result<Vec<u8>, String> {
    postcard::serialize_with_flavor(message, Count { bytes: 0, limit })
        .map_err(|e| e.to_string())?;
    postcard::to_allocvec(message).map_err(|e| e.to_string())
}
pub(crate) fn prepare(request: &OwnerCommitRequest) -> Result<Vec<u8>, String> {
    let CommitOperation::Document {
        app,
        record,
        envelope,
    } = &request.operation
    else {
        return Err("native outgoing requires document".into());
    };
    // This is the same exact signed local envelope, with canonical transport routing.
    postcard::serialize_with_flavor(
        request,
        Count {
            bytes: 0,
            limit: MAX_BYTES,
        },
    )
    .map_err(|e| e.to_string())?;
    let peer = crate::peer_handle(envelope.author.as_bytes());
    encode(
        &NeighbourMsg::AppDocument {
            peer,
            app_id: app.clone(),
            cluster_id: request.workspace.clone(),
            record_id: record.clone(),
            envelope: envelope.clone(),
        },
        MAX_BYTES,
    )
}
/// Workspace for `bytes` in the outgoing custody.
fn reserve(view: &record::StoredWorkspace, bytes: &[u8]) -> Result<(), String> {
    let publication = state_of(view)?;
    let total = bounds(publication)?;
    if publication.entries.len() >= MAX_ENTRIES
        || total
            .checked_add(bytes.len())
            .is_none_or(|size| size > MAX_BYTES)
    {
        return Err(CUSTODY_FULL.into());
    }
    Ok(())
}

pub(crate) fn replace_governance_checkpoint(
    live: &StatePeer,
    view: &mut record::StoredWorkspace,
    binding: CommitBinding,
    now: SystemTime,
) -> Result<(), String> {
    let message = live.workspace_message();
    let bytes = encode(&message, MAX_BYTES)?;
    let existing = state_of(view)?.governance.first();
    if existing.is_some_and(|entry| entry.bytes == bytes && entry.binding == binding) {
        return Ok(());
    }
    let total = bounds(state_of(view)?)?;
    let old = existing.map_or(0, |entry| entry.bytes.len());
    if total
        .checked_sub(old)
        .and_then(|size| size.checked_add(bytes.len()))
        .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("native governance checkpoint exceeds outgoing capacity".into());
    }
    let (channel, retention) = neighbour_wire::retention(&message, true)
        .ok_or("unsupported native governance checkpoint")?;
    if retention != Retention::Checkpoint {
        return Err("native governance publication is not a checkpoint".into());
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication.version = 3;
    let ordinal = publication.next_ordinal;
    publication.next_ordinal = ordinal
        .checked_add(1)
        .ok_or("native publication ordinal exhausted")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    publication.governance = vec![StoredOutgoing {
        ordinal,
        id: *blake3::hash(&bytes).as_bytes(),
        bytes,
        binding,
        timestamp_ns: enrollment::ns(now)?,
        channel,
        incremental: false,
    }];
    Ok(())
}

/// Publish `changes`, workspace changes this member's device made itself, for
/// the workspace's host to admit as it admits every member's: one retained
/// delta, joined with the changes still waiting from before, which stays
/// until the workspace's governance carries them all
/// ([`retire_member_changes`]).
pub(crate) fn add_member_changes(
    live: &StatePeer,
    view: &mut record::StoredWorkspace,
    changes: Vec<Vec<u8>>,
    binding: CommitBinding,
    now: SystemTime,
) -> Result<(), String> {
    let mut joined = match state_of(view)?.governance.first() {
        Some(existing) if existing.incremental => member_changes(&existing.bytes)?,
        _ => Vec::new(),
    };
    for change in changes {
        if !joined.contains(&change) {
            joined.push(change);
        }
    }
    let message = NeighbourMsg::SyncResp {
        changes: joined,
        room: view.workspace.to_string(),
    };
    let bytes = encode(&message, MAX_BYTES)?;
    let _ = live;
    let old = state_of(view)?
        .governance
        .first()
        .map_or(0, |entry| entry.bytes.len());
    let total = bounds(state_of(view)?)?;
    if total
        .checked_sub(old)
        .and_then(|size| size.checked_add(bytes.len()))
        .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("membership changes exceed outgoing capacity".into());
    }
    let (channel, retention) = neighbour_wire::retention(&message, false)
        .ok_or("unsupported native membership publication")?;
    if retention != Retention::Delta {
        return Err("native membership publication is not a delta".into());
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication.version = 3;
    let ordinal = publication.next_ordinal;
    publication.next_ordinal = ordinal
        .checked_add(1)
        .ok_or("native publication ordinal exhausted")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    publication.governance = vec![StoredOutgoing {
        ordinal,
        id: *blake3::hash(&bytes).as_bytes(),
        bytes,
        binding,
        timestamp_ns: enrollment::ns(now)?,
        channel,
        incremental: true,
    }];
    Ok(())
}

/// The changes a member's published workspace delta carries.
fn member_changes(bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    match postcard::from_bytes(bytes).map_err(|e| e.to_string())? {
        NeighbourMsg::SyncResp { changes, .. } => Ok(changes),
        _ => Err("native membership publication is not a workspace delta".into()),
    }
}

/// The hashes of `changes`, encoded workspace changes.
pub(crate) fn change_hashes(
    changes: &[Vec<u8>],
) -> Result<BTreeSet<automerge::ChangeHash>, String> {
    changes
        .iter()
        .map(|bytes| {
            automerge::Change::from_bytes(bytes.clone())
                .map(|change| change.hash())
                .map_err(|e| e.to_string())
        })
        .collect()
}

/// Let go of this member's published workspace changes once the workspace's
/// governance, as one of the owner's devices sent it (`governed`), carries
/// every one of them: the host admitted them. A workspace its governor started
/// over from a newer base no longer holds them as they were made: that base
/// is what the workspace is now, and they go too.
///
/// `governed` is the changes as each delta carried them, a delta whose changes
/// do not all decode carrying none: they are decoded only when this member has
/// changes of its own waiting.
pub(crate) fn retire_member_changes(
    live: &StatePeer,
    view: &mut record::StoredWorkspace,
    governed: &[Vec<Vec<u8>>],
) -> Result<(), String> {
    let Some(existing) = state_of(view)?.governance.first() else {
        return Ok(());
    };
    if !existing.incremental {
        return Ok(());
    }
    let made = change_hashes(&member_changes(&existing.bytes)?)?;
    let governed: BTreeSet<automerge::ChangeHash> = governed
        .iter()
        .filter_map(|changes| change_hashes(changes).ok())
        .flatten()
        .collect();
    if !made.is_subset(&governed) && made.iter().all(|hash| live.workspace_holds_change(hash)) {
        return Ok(());
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication.governance.clear();
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    Ok(())
}

pub(crate) fn refresh_governance_checkpoint(
    live: &StatePeer,
    view: &mut record::StoredWorkspace,
) -> Result<(), String> {
    let Some(existing) = state_of(view)?.governance.first().cloned() else {
        return Ok(());
    };
    // A member's own changes stay as it made them until they are admitted.
    if existing.incremental {
        return Ok(());
    }
    let bytes = encode(&live.workspace_message(), MAX_BYTES)?;
    let binding = if matches!(
        existing.binding.target,
        crate::owner_commit::CommitTarget::NativeCheckpointRoot { .. }
    ) {
        crate::owner::checkpoint::binding(
            view.workspace,
            live.workspace()
                .workspace_checkpoint_root()?
                .ok_or("native checkpoint root disappeared")?,
        )
    } else {
        existing.binding.clone()
    };
    if bytes == existing.bytes && binding == existing.binding {
        return Ok(());
    }
    let total = bounds(state_of(view)?)?;
    if total
        .checked_sub(existing.bytes.len())
        .and_then(|size| size.checked_add(bytes.len()))
        .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("refreshed native governance checkpoint exceeds outgoing capacity".into());
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    publication.governance = vec![StoredOutgoing {
        id: *blake3::hash(&bytes).as_bytes(),
        bytes,
        binding,
        ..existing
    }];
    Ok(())
}
fn append(view: &mut record::StoredWorkspace, bytes: Vec<u8>) -> Result<(), String> {
    let id = *blake3::hash(&bytes).as_bytes();
    if state_of(view)?
        .entries
        .iter()
        .any(|entry| entry.id == id && entry.bytes == bytes)
    {
        return Ok(());
    }
    reserve(view, &bytes)?;
    let message: NeighbourMsg = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
    let (timestamp_ns, channel, incremental) = metadata(&message)?;
    let binding = request(&message)?.binding()?;
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    let ordinal = publication.next_ordinal;
    publication.next_ordinal = ordinal
        .checked_add(1)
        .ok_or("native publication ordinal exhausted")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    publication.entries.push(StoredOutgoing {
        ordinal,
        id,
        bytes,
        binding,
        timestamp_ns,
        channel,
        incremental,
    });
    Ok(())
}
pub(crate) fn append_local(
    view: &mut record::StoredWorkspace,
    bytes: Vec<u8>,
) -> Result<(), String> {
    if !view.hosted_here() {
        return append(view, bytes);
    }
    // The workspace's own device is its keeper: what it admits is already
    // where the workspace's records live, and no one else answers a publish.
    // Nothing waits here, and what an earlier record kept waiting is released
    // with this commit, so custody of a device-hosted workspace stays empty.
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    if publication.entries.is_empty() {
        return Ok(());
    }
    publication.entries.clear();
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    Ok(())
}
pub(crate) fn has_exact(view: &record::StoredWorkspace, bytes: &[u8]) -> Result<bool, String> {
    Ok(state_of(view)?
        .entries
        .iter()
        .any(|entry| entry.bytes == bytes))
}
// qps validates historical custody separately from current send/sign permission.
fn validate_exact(
    peer: &StatePeer,
    message: &NeighbourMsg,
    device: [u8; 32],
) -> Result<(), String> {
    peer.outbox()
        .lookup_retained_local_document(message, device)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "native outgoing entry lacks exact committed admission".to_owned())
        .map(|_| ())
}
fn retired(peer: &StatePeer, message: &NeighbourMsg, device: [u8; 32]) -> Result<bool, String> {
    peer.outbox()
        .retained_local_document_retired(message, device)
}
pub(crate) fn decode_entries(entries: &[StoredOutgoing]) -> Result<Vec<NeighbourMsg>, String> {
    entries
        .iter()
        .map(|entry| postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string()))
        .collect()
}

pub(crate) fn validate(view: &NativeCommittedView) -> Result<(), String> {
    let publication = state(view)?;
    bounds(publication)?;
    let mut ids = BTreeSet::new();
    let mut ordinals = BTreeSet::new();
    for entry in publication
        .governance
        .iter()
        .chain(publication.entries.iter())
    {
        if entry.ordinal == 0
            || entry.ordinal >= publication.next_ordinal
            || !ids.insert(entry.id)
            || !ordinals.insert(entry.ordinal)
            || entry.id != *blake3::hash(&entry.bytes).as_bytes()
        {
            return Err("native outgoing identity/order mismatch".into());
        }
        let message: NeighbourMsg =
            postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
        if publication
            .governance
            .iter()
            .any(|item| item.id == entry.id)
        {
            validate_governance(view, entry)?;
        } else if encode(&message, MAX_BYTES)? != entry.bytes
            || request(&message)?.binding()? != entry.binding
            || metadata(&message)? != (entry.timestamp_ns, entry.channel, entry.incremental)
        {
            return Err("native outgoing bytes/binding mismatch".into());
        }
        if publication.entries.iter().any(|item| item.id == entry.id) {
            validate_exact(&view.peer, &message, view.device().device_public_key)?;
        }
    }
    if publication
        .entries
        .windows(2)
        .any(|pair| matches!(pair, [first, second] if first.ordinal >= second.ordinal))
    {
        return Err("native publication ordinal order mismatch".into());
    }
    Ok(())
}

fn validate_governance(view: &NativeCommittedView, entry: &StoredOutgoing) -> Result<(), String> {
    if entry.incremental {
        return validate_member_changes(view, entry);
    }
    let message: NeighbourMsg = postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
    if !matches!(&message, NeighbourMsg::SyncResp { room, .. } if room == &view.record.workspace.to_string())
        || encode(&view.peer.workspace_message(), MAX_BYTES)? != entry.bytes
        || entry.id != *blake3::hash(&entry.bytes).as_bytes()
        || entry.incremental
    {
        return Err("native governance checkpoint differs from canonical workspace policy".into());
    }
    let (channel, retention) = neighbour_wire::retention(&message, true)
        .ok_or("unsupported native governance checkpoint")?;
    if retention != Retention::Checkpoint || channel != entry.channel {
        return Err("native governance checkpoint transport mismatch".into());
    }
    if let crate::owner_commit::CommitTarget::NativeCheckpointRoot { root } = entry.binding.target {
        if view.peer.workspace().workspace_checkpoint_root()? != Some(root)
            || entry.binding != crate::owner::checkpoint::binding(view.record.workspace, root)
        {
            return Err("native checkpoint publication differs from signed root".into());
        }
        return Ok(());
    }
    if !receipted(view, &entry.binding) {
        return Err("native governance checkpoint lacks its owner receipt".into());
    }
    Ok(())
}

/// Whether `binding` is one this device receipted: a membership command, a
/// broker effect, a shortcut change or a join admission it committed.
fn receipted(view: &NativeCommittedView, binding: &CommitBinding) -> bool {
    view.record
        .workspace_memberships
        .values()
        .any(|receipt| &receipt.receipt.binding == binding)
        || view
            .record
            .effect_receipts
            .values()
            .any(|receipt| &receipt.receipt.binding == binding)
        || view
            .record
            .shortcut_change
            .as_ref()
            .is_some_and(|receipt| &receipt.binding == binding)
        || view
            .record
            .join_admissions
            .values()
            .any(|receipt| &receipt.receipt.binding == binding)
}
/// A member's published workspace changes: a delta of changes this device made,
/// every one of them in the workspace it holds, bound to its membership receipt.
fn validate_member_changes(
    view: &NativeCommittedView,
    entry: &StoredOutgoing,
) -> Result<(), String> {
    let message: NeighbourMsg = postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
    let NeighbourMsg::SyncResp { changes, room } = &message else {
        return Err("native membership publication is not a workspace delta".into());
    };
    if room != &view.record.workspace.to_string()
        || encode(&message, MAX_BYTES)? != entry.bytes
        || entry.id != *blake3::hash(&entry.bytes).as_bytes()
    {
        return Err("native membership publication differs from its workspace".into());
    }
    // The actor this replica writes under: who wrote a change is never
    // read from its actor, only which copy of the workspace made it.
    let made_here = view
        .peer
        .workspace_actor_for(&view.device().device_public_key);
    for bytes in changes {
        let change = automerge::Change::from_bytes(bytes.clone()).map_err(|e| e.to_string())?;
        if change.actor_id() != &made_here || !view.peer.workspace_holds_change(&change.hash()) {
            return Err("native membership publication carries a change not made here".into());
        }
    }
    if neighbour_wire::retention(&message, false) != Some((entry.channel, Retention::Delta)) {
        return Err("native membership publication transport mismatch".into());
    }
    if !receipted(view, &entry.binding) {
        return Err("native membership publication lacks its receipt".into());
    }
    Ok(())
}
/// An exact replay changes no peer state; reuse its barrier only when there
/// is no retirement or checkpoint refresh to publish.
pub(crate) fn exact_replay_unchanged(
    live: &StatePeer,
    view: &record::StoredWorkspace,
) -> Result<bool, String> {
    let publication = state_of(view)?;
    let device = view.device.device_public_key;
    let originals = decode_entries(&publication.entries)?;
    for message in &originals {
        if retired(live, message, device).unwrap_or(false) {
            return Ok(false);
        }
    }
    // A member's own workspace changes wait for the governance that retires
    // them, which any replay may bring.
    if let Some(checkpoint) = publication.governance.first()
        && (checkpoint.incremental
            || encode(&live.workspace_message(), MAX_BYTES)? != checkpoint.bytes)
    {
        return Ok(false);
    }
    Ok(true)
}

impl StoredPublication {
    /// Count `answer` toward the writes to `app`'s `record`.
    fn answer(&mut self, app: &str, record: &str, answer: HostAnswer) {
        if !self
            .answered
            .iter()
            .any(|held| held.app == app && held.record == record)
        {
            self.answered.push(AnsweredRecord {
                app: app.to_owned(),
                record: record.to_owned(),
                durable: 0,
                refused: 0,
                last_order: None,
            });
        }
        let Some(held) = self
            .answered
            .iter_mut()
            .find(|held| held.app == app && held.record == record)
        else {
            return;
        };
        match answer {
            HostAnswer::Durable(order) => {
                held.durable = held.durable.saturating_add(1);
                held.last_order = Some(order);
            }
            HostAnswer::Refused => held.refused = held.refused.saturating_add(1),
        }
    }
}
pub(crate) fn retire(live: &StatePeer, view: &mut record::StoredWorkspace) -> Result<bool, String> {
    let publication = state_of(view)?;
    let mut remove = BTreeSet::new();
    for entry in &publication.entries {
        let message: NeighbourMsg =
            postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
        // A refused query cannot discard newly admitted revocations or prerequisites.
        if retired(live, &message, view.device.device_public_key).unwrap_or(false) {
            remove.insert(entry.id);
        }
    }
    if remove.is_empty() {
        return Ok(false);
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication
        .entries
        .retain(|entry| !remove.contains(&entry.id));
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    Ok(true)
}
pub(crate) fn retry_covered(
    live: &StatePeer,
    view: &record::StoredWorkspace,
    bytes: &[u8],
) -> Result<bool, String> {
    let message: NeighbourMsg = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
    retired(live, &message, view.device.device_public_key)
}
impl NativeWorkspaceOwner {
    /// Read `workspace`'s outgoing custody: what the relay session may send now.
    pub fn with_outgoing<T>(
        &self,
        workspace: WorkspaceId,
        now: SystemTime,
        read: impl FnOnce(&NativeCommittedView, NativeOutgoing<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_current(workspace, |view| {
            view.peer
                .permissions(view.device().device_public_key, now)
                .map_err(|error| error.to_string())?;
            read(
                view,
                NativeOutgoing {
                    state: state(view)?,
                    allowed: allowed(view, now)?,
                },
            )
        })
    }
    /// How many more writes, and how many more bytes of them, this device's
    /// outgoing custody of `workspace` takes before it is full.
    pub fn custody_room(&self, workspace: WorkspaceId) -> Result<(usize, usize), String> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            Ok((
                MAX_ENTRIES.saturating_sub(publication.entries.len()),
                MAX_BYTES.saturating_sub(bounds(publication)?),
            ))
        })
    }
    /// How many of this device's committed writes to `workspace` are still in
    /// its outgoing custody: not yet admitted by the workspace's governor.
    pub fn unadmitted_writes(&self, workspace: WorkspaceId) -> Result<usize, String> {
        self.with_current(workspace, |view| Ok(state(view)?.entries.len()))
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn outgoing_status(
        &self,
        workspace: WorkspaceId,
        now: SystemTime,
    ) -> Result<NativeOutgoingStatus, String> {
        self.with_current(workspace, |view| status(view, now))
    }
    /// The workspace's governance and a signed snapshot of each record, for a
    /// link's catch-up of `workspace`, committed as this device's write. The
    /// snapshots are signed with `key`, this device's key, and take numbers
    /// of its own sequence: signed in the live replica alone, they left the
    /// durable view behind it, and the device's next commit, numbered from
    /// that view, took a number a snapshot already held. Committed, they are
    /// as durable as the replica, and a restart reuses no number a member
    /// already holds.
    ///
    /// A durable receipt is the governor's assertion about the workspace, and
    /// its generation is the workspace's: only a device that hosts this
    /// workspace writes one, as its own commits do elsewhere
    /// ([`super::effect_receipt`]). Every other device of the workspace's
    /// person publishes the snapshots alone: their generations are each
    /// device's own count, and two of them stamping the same receipts would
    /// leave every member with a receipt no other member can merge.
    ///
    /// # Errors
    /// The workspace is not held here, `key` is not this device's, or the
    /// commit failed.
    pub fn commit_bootstrap(
        &self,
        workspace: WorkspaceId,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        if key.verifying_key().to_bytes() != candidate.device().device_public_key {
            return Err("native bootstrap signer differs from selected device".into());
        }
        // A device that does not host the workspace and holds no record in the
        // clear signs nothing for a catch-up and takes no number: there is
        // nothing to make durable, and a link costs no write of its record.
        if !candidate.record.hosted_here() {
            let mut live = candidate.live();
            if !live.holds_clear_records() {
                return live.publication_mut().bootstrap_messages(key, now);
            }
        }
        let (messages, stored) = candidate.change(|live, stored| {
            let generation = stored
                .generation
                .checked_add(1)
                .ok_or("native owner generation exhausted")?;
            let messages = if stored.hosted_here() {
                live.publication_mut()
                    .durable_bootstrap_messages(key, now, generation)?
            } else {
                live.publication_mut().bootstrap_messages(key, now)?
            };
            stored.generation = generation;
            Ok(messages)
        })?;
        self.publish_record(workspace, &candidate, stored, None)?;
        Ok(messages)
    }
    /// Retire every retained frame the workspace has refused or a durable
    /// receipt names, and commit the retirement.
    pub fn retire_outgoing(&self, workspace: WorkspaceId) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let now = SystemTime::now();
        candidate
            .peer
            .permissions(candidate.device().device_public_key, now)
            .map_err(|error| error.to_string())?;
        let (changed, mut record) = candidate.change(|live, record| retire(live, record))?;
        if !changed {
            return Ok(());
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_record(workspace, &candidate, record, None)
    }
    /// Retire the custody entries `answers` names, which the workspace's relay host answered:
    /// a relay host acknowledges each commit, durable or refused, rather than receipting it in
    /// the workspace's governance, and an answered entry is never sent again. Each answer
    /// counts toward its record's [`RecordWrites`], and a refused write leaves this device's
    /// replica as well: the workspace never holds it, so neither does this device.
    ///
    /// # Errors
    /// The workspace is not held here, or the retirement could not be committed.
    pub fn retire_answered(
        &self,
        workspace: WorkspaceId,
        answers: &BTreeMap<[u8; 32], HostAnswer>,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let (changed, mut record) = candidate.change(|live, record| {
            let publication = record.publication.as_mut().ok_or("publication absent")?;
            let (answered, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut publication.entries)
                .into_iter()
                .partition(|entry| answers.contains_key(&entry.id));
            publication.entries = kept;
            if answered.is_empty() {
                return Ok(false);
            }
            for entry in answered {
                // ignored-failure: custody holds governance entries too, which belong to no record
                let Ok(NeighbourMsg::AppDocument {
                    app_id,
                    record_id,
                    envelope,
                    ..
                }) = postcard::from_bytes::<NeighbourMsg>(&entry.bytes)
                else {
                    continue;
                };
                let answer = answers
                    .get(&entry.id)
                    .copied()
                    .unwrap_or(HostAnswer::Refused);
                if answer == HostAnswer::Refused {
                    live.withdraw_local_document(&app_id, &record_id, &envelope)?;
                }
                publication.answer(&app_id, &record_id, answer);
            }
            publication.revision = publication
                .revision
                .checked_add(1)
                .ok_or("native publication revision exhausted")?;
            Ok(true)
        })?;
        if !changed {
            return Ok(());
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_record(workspace, &candidate, record, None)
    }
    /// This device's writes to each record of `workspace` it wrote, by app and record: those
    /// in its custody, and the relay host's answers to the others.
    pub fn record_writes(
        &self,
        workspace: WorkspaceId,
    ) -> BTreeMap<(String, String), RecordWrites> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            let mut writes: BTreeMap<(String, String), RecordWrites> = publication
                .answered
                .iter()
                .map(|answered| {
                    (
                        (answered.app.clone(), answered.record.clone()),
                        RecordWrites {
                            pending: 0,
                            durable: answered.durable,
                            refused: answered.refused,
                            last_order: answered.last_order,
                        },
                    )
                })
                .collect();
            for entry in &publication.entries {
                // ignored-failure: custody holds governance entries too, which belong to no record
                if let Ok(NeighbourMsg::AppDocument {
                    app_id, record_id, ..
                }) = postcard::from_bytes::<NeighbourMsg>(&entry.bytes)
                {
                    let writes = writes.entry((app_id, record_id)).or_default();
                    writes.pending = writes.pending.saturating_add(1);
                }
            }
            Ok(writes)
        })
        .unwrap_or_default()
    }
    /// Renew this device's sealed writes that expired in custody before the
    /// governor retired them, through `reseal`, the channels' key holder
    /// (see [`PeerOutbox::draft_sealed_renewals`](crate::PeerOutbox::draft_sealed_renewals)).
    /// Each renewal replaces its original in one durable commit and moves behind every other
    /// entry, so an author's writes still send in sequence order. A write `reseal` refuses stays
    /// exactly as it was.
    ///
    /// # Errors
    /// As [`Self::sign_and_commit`].
    pub async fn renew_outgoing(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        mut reseal: impl FnMut(&SignedOp) -> Result<(EncryptionMetadata, Vec<u8>), String>,
        now: SystemTime,
    ) -> Result<(), String> {
        let draft = |view: &NativeCommittedView| {
            let entries = &state_of(&view.record)?.entries;
            let originals = entries
                .iter()
                .map(|entry| postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string()))
                .collect::<Result<Vec<NeighbourMsg>, String>>()?;
            let device = view.device().device_public_key;
            let drafts =
                view.peer
                    .outbox()
                    .draft_sealed_renewals(&originals, &mut reseal, device, now);
            Ok(entries
                .iter()
                .zip(originals)
                .zip(drafts)
                .filter_map(|((entry, original), draft)| {
                    // The write stays valid only until its own expiry: a person whose change
                    // does not reach the workspace needs this in the journal.
                    let renewal = draft
                        .inspect_err(|error| tracing::warn!(%error, "sealed write stays unrenewed"))
                        .ok()??;
                    Some((Some(entry.id), original, renewal))
                })
                .collect())
        };
        let admit = |live: &mut StatePeer, original: NeighbourMsg, renewal| {
            live.outbox_mut()
                .admit_sealed_renewal(&original, renewal, now)
        };
        self.sign_and_commit(workspace, signer, now, draft, admit)
            .await
            .map(drop)
    }

    /// Compact every sealed record history of `workspace` that has used most of
    /// its bound into one sealed checkpoint, and commit the compactions
    /// into this device's outgoing custody, where they publish as the
    /// checkpoints they are. `open` opens any write of a history this
    /// device can read, and `seal` seals a record's merged snapshot under
    /// its channel's current epoch, both from the channel keys this device
    /// holds as a writer of the record
    /// ([`PeerPublication::draft_sealed_compaction`](crate::PeerPublication::draft_sealed_compaction)).
    /// A record whose keys are not here, or whose history does not merge yet, waits for a later
    /// call.
    ///
    /// Answers how many histories were compacted. Every device taking part
    /// maintains its own copy this way; no host compacts for it.
    ///
    /// # Errors
    /// As [`Self::sign_and_commit`].
    pub async fn compact_outgoing(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        mut open: impl FnMut(&SignedOp) -> Result<Vec<u8>, String>,
        mut seal: impl FnMut(&str, &str, &[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String>,
        now: SystemTime,
    ) -> Result<usize, String> {
        if self.with_current(workspace, |view| {
            Ok(view
                .peer
                .publication()
                .sealed_records_to_compact()
                .is_empty())
        })? {
            return Ok(0);
        }
        let draft = |view: &NativeCommittedView| {
            let device = view.device().device_public_key;
            let due = view.peer.publication().sealed_records_to_compact();
            Ok(due
                .into_iter()
                .filter_map(|(app, record)| {
                    let mut seal = |snapshot: &[u8]| seal(&app, &record, snapshot);
                    let route = (app.as_str(), record.as_str());
                    let drafted = view.peer.publication().draft_sealed_compaction(
                        route, None, &mut open, &mut seal, device, now,
                    );
                    let op = drafted
                        .inspect_err(|error| {
                            tracing::warn!(%error, %app, %record, "sealed record history stays uncompacted");
                        })
                        .ok()?;
                    Some((None, (app, record), op))
                })
                .collect())
        };
        let admit = |live: &mut StatePeer, (app, record): (String, String), op| {
            live.publication_mut()
                .admit_sealed_compaction(&app, &record, op, now)
        };
        self.sign_and_commit(workspace, signer, now, draft, admit)
            .await
    }

    /// Draft writes of `workspace` in one owner turn, each with the custody entry it replaces,
    /// sign each with `signer`, this device's custody of its key, outside it, and commit those
    /// `admit` takes into the outgoing custody in one durable commit of a second turn. One write
    /// of this device runs at a time, so a draft's sequence is still past its record's floor when
    /// it is admitted. A write `admit` refuses, or one whose entry left custody meanwhile, stays
    /// out. Answers how many writes were taken.
    ///
    /// # Errors
    /// `signer` is not the enrolled device, it may no longer write, the workspace is fenced,
    /// signing failed, or what was taken cannot be committed.
    async fn sign_and_commit<T>(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
        draft: impl FnOnce(&NativeCommittedView) -> Result<Vec<(Option<[u8; 32]>, T, SignedOp)>, String>,
        mut admit: impl FnMut(&mut StatePeer, T, SignedOp) -> Result<NeighbourMsg, String>,
    ) -> Result<usize, String> {
        let _writing = self.0.writes.lock().await;
        let mut drafted = {
            let _turn = self.0.mutation.lock();
            let view = self.view(workspace)?;
            let device = view.device().device_public_key;
            if signer.device_public_key() != device {
                return Err("native outgoing signer differs from selected device".into());
            }
            view.peer
                .permissions(device, now)
                .map_err(|error| error.to_string())?;
            draft(&view)?
        };
        if drafted.is_empty() {
            return Ok(0);
        }
        for (_, _, op) in &mut drafted {
            crate::owner::write::sign(signer, op).await?;
        }
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let (taken, mut record) = candidate.change(|live, record| {
            let mut taken = 0usize;
            for (replaces, with, op) in drafted {
                let entries = &mut record
                    .publication
                    .as_mut()
                    .ok_or("publication absent")?
                    .entries;
                if replaces.is_some_and(|id| !entries.iter().any(|kept| kept.id == id)) {
                    continue;
                }
                // Admitted on a copy: a write that cannot be committed as one custody entry
                // leaves the replica as it was.
                let mut attempt = live.clone();
                match admit(&mut attempt, with, op).and_then(|message| encode(&message, MAX_BYTES))
                {
                    Ok(bytes) => {
                        entries.retain(|kept| Some(kept.id) != replaces);
                        *live = attempt;
                        append(record, bytes)?;
                        taken = taken.saturating_add(1);
                    }
                    Err(error) => tracing::warn!(%error, "an outgoing write stays as it was"),
                }
            }
            Ok(taken)
        })?;
        if taken == 0 {
            return Ok(0);
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_record(workspace, &candidate, record, None)?;
        Ok(taken)
    }
}
#[cfg(test)]
mod encoding_tests {
    use super::*;

    fn entry(bytes: Vec<u8>) -> StoredOutgoing {
        StoredOutgoing {
            ordinal: 3,
            id: [7; 32],
            bytes,
            binding: CommitBinding {
                workspace: "room".into(),
                target: crate::owner_commit::CommitTarget::Document {
                    app: "so.tilo.code".into(),
                    record: "a".into(),
                },
                request_digest: [1; 32],
                basis: [2; 32],
            },
            timestamp_ns: 5,
            channel: [9; 32],
            incremental: true,
        }
    }

    /// Custody leaves each entry as the class it was published by: a write
    /// appends after the record's checkpoint, while a member's sealed
    /// compaction replaces that author's retained history with itself.
    #[test]
    fn an_entry_leaves_as_the_class_it_was_published_by() {
        let delta = entry(Vec::new());
        assert_eq!(NativeOutgoingEntry(&delta).retention(), Retention::Delta);
        let checkpoint = StoredOutgoing {
            incremental: false,
            ..entry(Vec::new())
        };
        assert_eq!(
            NativeOutgoingEntry(&checkpoint).retention(),
            Retention::Checkpoint
        );
    }

    /// An entry's bytes are base64 text in JSON, an entry written as the
    /// legacy array of numbers still reads back the same, and a binary
    /// encoding keeps its unchanged sequence.
    #[test]
    fn outgoing_bytes_are_text_in_json_and_legacy_arrays_still_read() {
        let stored = entry((0..=255).collect());
        let json = serde_json::to_value(&stored).unwrap();
        assert!(json["bytes"].is_string(), "{json}");
        assert_eq!(
            serde_json::from_value::<StoredOutgoing>(json).unwrap(),
            stored
        );
        let mut legacy = serde_json::to_value(&stored).unwrap();
        legacy["bytes"] = serde_json::to_value(&stored.bytes).unwrap();
        assert!(legacy["bytes"].is_array());
        assert_eq!(
            serde_json::from_value::<StoredOutgoing>(legacy).unwrap(),
            stored
        );
        let pretty = serde_json::to_vec_pretty(&stored).unwrap();
        assert_eq!(
            serde_json::from_slice::<StoredOutgoing>(&pretty).unwrap(),
            stored
        );
        let binary = postcard::to_allocvec(&stored).unwrap();
        assert_eq!(
            binary,
            postcard::to_allocvec(&(
                stored.ordinal,
                stored.id,
                &stored.bytes,
                &stored.binding,
                stored.timestamp_ns,
                stored.channel,
                stored.incremental
            ))
            .unwrap(),
            "the binary encoding is unchanged"
        );
        assert_eq!(
            postcard::from_bytes::<StoredOutgoing>(&binary).unwrap(),
            stored
        );
    }

    /// A record an earlier build kept recovery projections in still reads, and
    /// this build writes it back without them: it sends the signed originals.
    #[test]
    fn a_projected_record_still_reads_and_is_written_without_its_projection() {
        let projected = entry(vec![1, 2, 3]);
        let mut stored = serde_json::to_value(StoredPublication::default()).unwrap();
        stored["projection"] = serde_json::to_value([&projected]).unwrap();
        let mut read: StoredPublication = serde_json::from_value(stored).unwrap();
        assert_eq!(read.projection, vec![projected]);
        // What a record read from disk does with one (`file_store`).
        read.projection.clear();
        let written = serde_json::to_value(&read).unwrap();
        assert!(
            written.get("projection").is_none(),
            "a record written now carries no projection: {written}"
        );
    }

    /// Bytes past the cap are refused in either JSON form, text before it is
    /// decoded.
    #[test]
    fn outgoing_bytes_past_the_cap_are_refused_as_text_and_as_arrays() {
        let read = |json: &str| read_bytes(&mut serde_json::Deserializer::from_str(json), 4);
        assert_eq!(read(r#""AAECAw==""#).unwrap(), [0, 1, 2, 3]);
        assert_eq!(read("[0, 1, 2, 3]").unwrap(), [0, 1, 2, 3]);
        assert!(read(r#""AAECAwQ=""#).is_err(), "five bytes");
        assert!(
            read(r#""AAECAwQFBg==""#).is_err(),
            "text longer than four bytes'"
        );
        assert!(read("[0, 1, 2, 3, 4]").is_err());
        assert!(read(r#""not base64""#).is_err());
        let fits = entry(vec![0; MAX_BYTES]);
        assert_eq!(
            serde_json::from_value::<StoredOutgoing>(serde_json::to_value(&fits).unwrap()).unwrap(),
            fits
        );
        let past = entry(vec![0; MAX_BYTES + 1]);
        assert!(
            serde_json::from_value::<StoredOutgoing>(serde_json::to_value(&past).unwrap()).is_err()
        );
    }
}
