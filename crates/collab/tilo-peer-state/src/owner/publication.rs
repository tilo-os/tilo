//! Same-barrier native outgoing custody; relay queue acceptance is never an ACK.
use crate::StatePeer;
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner, WorkspaceId, enrollment, record};
use crate::owner_commit::{CommitBinding, CommitOperation, OwnerCommitRequest};
use collab_types::signer::DeviceSigner;
use collab_types::{EncryptionMetadata, OpKind, SignedOp};
use neighbour_wire::NeighbourMsg;
use neighbour_wire::Retention;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use web_time::SystemTime;

/// Writes custody keeps in its record at its head, where the relay session sends them from,
/// and at most as many again at its tail. Every write past them waits in runs of the store's
/// beside the record ([`super::Store::put_custody`]): custody takes every write this device
/// makes, linked or not, however long its host is away. This crate's tests keep fewer, so a
/// test crosses the bound many times over in the writes it can afford.
#[cfg(not(test))]
pub(crate) const MAX_ENTRIES: usize = 64;
#[cfg(test)]
pub(crate) const MAX_ENTRIES: usize = 8;
pub(crate) const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Bytes of writes one run of custody's backlog gathers before the store keeps it.
const RUN_BYTES: usize = 1024 * 1024;
/// Most bytes a run the store keeps can hold: its writes, then one more that did not fit.
pub(crate) const MAX_RUN_BYTES: usize = RUN_BYTES + MAX_BYTES + 64 * 1024;
pub(crate) const MAX_VISIBLE_ENTRIES: usize = MAX_ENTRIES + 1;
/// Passes (writes, and asks of the workspace's holder) before a compaction that
/// drafted nothing is tried again ([`NativeWorkspaceOwner::compact_outgoing`]).
const COMPACTION_RETRY: usize = 256;

/// The channel keys a device holds for the sealed records of its workspaces:
/// what its owner compacts their histories with
/// ([`NativeWorkspaceOwner::keep_sealed_histories`]).
pub trait SealedHistoryKeys: collab_types::signer::MaybeSendSync {
    /// Open `write`, an admitted write of a sealed record's history of
    /// `workspace` (a compaction included), under any epoch whose key this
    /// device holds.
    ///
    /// # Errors
    /// This device holds no key that opens it.
    fn open(&self, workspace: WorkspaceId, write: &SignedOp) -> Result<Vec<u8>, String>;
    /// Seal `snapshot`, the merged history of `app`'s `record`, as that
    /// record's checkpoint under its channel's current epoch.
    ///
    /// # Errors
    /// This device holds no current key of the record's channel.
    fn seal_checkpoint(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        snapshot: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String>;
    /// Seal `write` again under its channel's current epoch: one of this device's sealed writes
    /// of `workspace` that expired in custody before its host took it. Answers the encryption and
    /// payload its renewal carries.
    ///
    /// # Errors
    /// This device holds no key that opens it, or none of its channel's current epoch.
    fn renew(
        &self,
        workspace: WorkspaceId,
        write: &SignedOp,
    ) -> Result<(EncryptionMetadata, Vec<u8>), String>;
}
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
    /// Writes behind `entries` the store keeps beside the record, oldest first: they move to
    /// the head as it sends and the host answers what it holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spilled: Vec<SpilledRun>,
    /// The newest writes behind `spilled`, until they make a run of their own.
    #[serde(
        default,
        deserialize_with = "read_entries",
        skip_serializing_if = "Vec::is_empty"
    )]
    tail: Vec<StoredOutgoing>,
    /// The sealed records whose history here a compaction of this device's own folded ahead
    /// of the host's, each with the writes the host holds that it no longer holds as
    /// themselves: what this device knows of the host's history beside its own. All but its
    /// newest writes wait in runs the store keeps, so an answer never rewrites them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hosted: Vec<HostedHistory>,
    /// Custody's compactions not drafted yet. Each keeps its place, and the sequence it was
    /// committed under, until it is the next write custody sends: it is drafted then, against
    /// the history its host acknowledged ([`NativeWorkspaceOwner::compact_outgoing`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    slots: Vec<[u8; 32]>,
}
/// One of [`StoredPublication::hosted`]: the host's writes of `record` of `app` the record
/// holds here only inside this device's compaction, in the host's order: those of `runs`
/// ([`super::Store::put_custody`]), then `writes`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HostedHistory {
    app: String,
    record: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    runs: Vec<[u8; 32]>,
    writes: Vec<HostedWrite>,
    /// Each writer's highest write a compaction of the host's folded: the host holds none
    /// of them as themselves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    covered: Vec<([u8; 32], u64)>,
}
/// What this device knows of the host's history of a record whose history here a compaction of
/// its own folded ahead of the host's ([`StoredPublication::hosted`]).
#[derive(Clone, Debug, Default)]
struct Hosted {
    /// The host's writes the record holds here only inside that compaction, in its order.
    writes: Vec<SignedOp>,
    covered: BTreeMap<[u8; 32], u64>,
}
impl Hosted {
    /// Whether a compaction of the host's folded `write`.
    fn covers(&self, write: &SignedOp) -> bool {
        self.covered
            .get(write.author.as_bytes())
            .is_some_and(|seq| write.seq <= *seq)
    }
    /// Take `compaction`, now first in the host's history.
    fn take(&mut self, compaction: &SignedOp) -> Result<(), String> {
        let wrapper =
            collab_types::SealedCompaction::parse(compaction).map_err(|e| e.to_string())?;
        for (author, seq) in &wrapper.covers {
            let floor = self.covered.entry(*author.as_bytes()).or_default();
            *floor = (*floor).max(*seq);
        }
        let writes = std::mem::take(&mut self.writes);
        self.writes = std::iter::once(compaction.clone())
            .chain(
                writes
                    .into_iter()
                    .filter(|write| write.signature != compaction.signature && !self.covers(write)),
            )
            .collect();
        Ok(())
    }
}
/// A write of a [`HostedHistory`], as postcard bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HostedWrite(#[serde(with = "crate::owner::record::base64_bytes")] Vec<u8>);
impl HostedHistory {
    /// Keep all but the newest writes in runs, as custody keeps its backlog.
    fn spill(&mut self, (store, workspace): Runs<'_>) -> Result<(), String> {
        let runs = spill(&mut self.writes, |write| write.0.len(), store, workspace)?;
        self.runs.extend(runs.into_iter().map(|(hash, _)| hash));
        Ok(())
    }
}
/// The store that keeps a workspace's runs, and the workspace.
type Runs<'a> = (&'a dyn super::Store, WorkspaceId);
/// How many writes of what this device knows of its hosts' histories `view`'s record holds
/// itself, and how many runs beside it hold the rest.
#[cfg(test)]
pub(crate) fn hosted_in_record(view: &NativeCommittedView) -> (usize, usize) {
    state(view).map_or((0, 0), |publication| {
        publication
            .hosted
            .iter()
            .fold((0, 0), |(writes, runs), held| {
                (
                    writes.saturating_add(held.writes.len()),
                    runs.saturating_add(held.runs.len()),
                )
            })
    })
}
/// Writes of custody's backlog the store keeps as one run, named by the hash of its bytes
/// ([`super::Store::put_custody`]), and how many of them moved to custody's head already.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpilledRun {
    hash: [u8; 32],
    writes: u32,
    taken: u32,
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
            spilled: Vec::new(),
            tail: Vec::new(),
            hosted: Vec::new(),
            slots: Vec::new(),
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
    // The records with a compaction ahead in custody: none of their writes leaves before the host
    // answered it, so one it refuses is drafted again in its place.
    let mut compacting: Vec<(String, String)> = Vec::new();
    for (index, entry) in publication.entries.iter().enumerate() {
        // A compaction not drafted yet holds its place: nothing after it leaves before it.
        if publication.slots.contains(&entry.id) {
            break;
        }
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
        let route = (app_id, record_id);
        if compacting.contains(&route) {
            continue;
        }
        if envelope.kind == OpKind::Compaction {
            compacting.push(route.clone());
        }
        let (app_id, record_id) = route;
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
        || publication.spilled.iter().any(|run| {
            run.writes == 0
                || run.taken >= run.writes
                || usize::try_from(run.writes).map_or(true, |writes| writes > MAX_ENTRIES)
        })
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
/// The bytes custody keeps, and its relay session sends, for `request`, one of this device's
/// document writes as the owner committed it.
///
/// # Errors
/// `request` is not a document write, or it exceeds custody's cap.
pub fn outgoing_bytes(request: &OwnerCommitRequest) -> Result<Vec<u8>, String> {
    prepare(request)
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
    let binding = existing.binding.clone();
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
/// Take `bytes`, one of this device's writes, into custody: at its head while the head has room
/// and nothing waits behind it, else at its tail, which the store keeps in runs once it is long
/// ([`settle`]). Custody refuses no write for how many it holds.
fn append(view: &mut record::StoredWorkspace, bytes: Vec<u8>) -> Result<(), String> {
    let id = *blake3::hash(&bytes).as_bytes();
    let custody = state_of(view)?;
    if custody
        .entries
        .iter()
        .chain(&custody.tail)
        .any(|entry| entry.id == id && entry.bytes == bytes)
    {
        return Ok(());
    }
    let head = bounds(custody)?;
    let message: NeighbourMsg = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
    let (timestamp_ns, channel, incremental) = metadata(&message)?;
    let binding = request(&message)?.binding()?;
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    let at_head = publication.tail.is_empty()
        && publication.spilled.is_empty()
        && publication.entries.len() < MAX_ENTRIES
        && head
            .checked_add(bytes.len())
            .is_some_and(|size| size <= MAX_BYTES);
    let ordinal = publication.next_ordinal;
    publication.next_ordinal = ordinal
        .checked_add(1)
        .ok_or("native publication ordinal exhausted")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    let entry = StoredOutgoing {
        ordinal,
        id,
        bytes,
        binding,
        timestamp_ns,
        channel,
        incremental,
    };
    if at_head {
        publication.entries.push(entry);
    } else {
        publication.tail.push(entry);
    }
    Ok(())
}

/// Keep the oldest of `tail` as runs `store` keeps beside `workspace`'s record while it holds a
/// run's worth (`len` gives each one's bytes), and answer each run's hash and length.
fn spill<T: Serialize>(
    tail: &mut Vec<T>,
    len: impl Fn(&T) -> usize,
    store: &dyn super::Store,
    workspace: WorkspaceId,
) -> Result<Vec<([u8; 32], usize)>, String> {
    let mut runs = Vec::new();
    while tail.len() >= MAX_ENTRIES || tail.iter().map(&len).sum::<usize>() >= RUN_BYTES {
        let mut bytes = 0usize;
        let count = tail
            .iter()
            .take(MAX_ENTRIES)
            .take_while(|item| {
                let fits = bytes == 0 || bytes.saturating_add(len(item)) <= RUN_BYTES;
                bytes = bytes.saturating_add(len(item));
                fits
            })
            .count();
        let run: Vec<T> = tail.drain(..count).collect();
        let encoded = postcard::to_allocvec(&run).map_err(|e| e.to_string())?;
        runs.push((store.put_custody(workspace, &encoded)?, count));
    }
    Ok(runs)
}

/// Keep `workspace`'s custody in shape before its record is published: its head takes the
/// oldest writes waiting behind it while it has room, and a tail that grew long becomes a run
/// `store` keeps beside the record. Every write stays in the order it was committed.
///
/// # Errors
/// A run could not be kept or read back.
pub(crate) fn settle(
    view: &mut record::StoredWorkspace,
    store: &dyn super::Store,
) -> Result<(), String> {
    let workspace = view.workspace;
    let Some(publication) = view.publication.as_mut() else {
        return Ok(());
    };
    let mut held = bounds(publication)?;
    let mut changed = false;
    let mut run: Option<Vec<StoredOutgoing>> = None;
    while publication.entries.len() < MAX_ENTRIES {
        let next = match publication.spilled.first() {
            Some(front) => {
                let writes = match &mut run {
                    Some(writes) => writes,
                    None => run.insert(read_run(store, workspace, front)?),
                };
                writes
                    .get(usize::try_from(front.taken).map_err(|e| e.to_string())?)
                    .cloned()
                    .ok_or("a kept run of custody is shorter than its record says")?
            }
            None => match publication.tail.first() {
                Some(first) => first.clone(),
                None => break,
            },
        };
        let Some(after) = held
            .checked_add(next.bytes.len())
            .filter(|size| *size <= MAX_BYTES)
        else {
            break;
        };
        match publication.spilled.first_mut() {
            Some(front) => {
                front.taken = front.taken.saturating_add(1);
                if front.taken == front.writes {
                    publication.spilled.remove(0);
                    run = None;
                }
            }
            None => {
                publication.tail.remove(0);
            }
        }
        held = after;
        publication.entries.push(next);
        changed = true;
    }
    for (hash, count) in spill(
        &mut publication.tail,
        |entry| entry.bytes.len(),
        store,
        workspace,
    )? {
        publication.spilled.push(SpilledRun {
            hash,
            writes: u32::try_from(count).map_err(|e| e.to_string())?,
            taken: 0,
        });
        changed = true;
    }
    if changed {
        publication.revision = publication
            .revision
            .checked_add(1)
            .ok_or("native publication revision exhausted")?;
    }
    Ok(())
}

/// The runs of custody's backlog `view` names, which its store keeps while it does.
pub(crate) fn kept_runs(view: &record::StoredWorkspace) -> impl Iterator<Item = [u8; 32]> + '_ {
    view.publication.iter().flat_map(|publication| {
        publication.spilled.iter().map(|run| run.hash).chain(
            publication
                .hosted
                .iter()
                .flat_map(|held| held.runs.iter().copied()),
        )
    })
}

/// The name a store keeps the run of `workspace`'s custody named `hash` under
/// ([`super::Store::put_custody`]).
#[must_use]
pub fn custody_run_name(workspace: WorkspaceId, hash: [u8; 32]) -> String {
    format!(
        "{workspace}.custody.{}",
        blake3::Hash::from_bytes(hash).to_hex()
    )
}

impl record::NativeAuthorityStorage {
    /// The name of every run of outgoing custody this record names: what its store keeps
    /// beside it, and may let go of once a record it publishes no longer names it.
    pub fn custody_runs(&self) -> impl Iterator<Item = String> + '_ {
        self.workspaces.iter().flat_map(|(workspace, stored)| {
            kept_runs(stored).map(|hash| custody_run_name(*workspace, hash))
        })
    }
}

/// The writes of `run`, read back from `store` and checked against its record.
fn read_run(
    store: &dyn super::Store,
    workspace: WorkspaceId,
    run: &SpilledRun,
) -> Result<Vec<StoredOutgoing>, String> {
    let bytes = store.read_custody(workspace, run.hash)?;
    if bytes.len() > MAX_RUN_BYTES || *blake3::hash(&bytes).as_bytes() != run.hash {
        return Err("a kept run of custody differs from its record".into());
    }
    let writes: Vec<StoredOutgoing> = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
    if usize::try_from(run.writes).ok() != Some(writes.len())
        || writes
            .iter()
            .any(|write| write.id != *blake3::hash(&write.bytes).as_bytes())
    {
        return Err("a kept run of custody differs from its record".into());
    }
    Ok(writes)
}

/// The writes of every run `publication` keeps beside its record that are not at its head yet.
fn spilled_writes(
    publication: &StoredPublication,
    store: &dyn super::Store,
    workspace: WorkspaceId,
) -> Result<Vec<StoredOutgoing>, String> {
    let mut writes = Vec::new();
    for run in &publication.spilled {
        let taken = usize::try_from(run.taken).map_err(|e| e.to_string())?;
        writes.extend(read_run(store, workspace, run)?.into_iter().skip(taken));
    }
    Ok(writes)
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
    if publication.entries.is_empty()
        && publication.tail.is_empty()
        && publication.spilled.is_empty()
    {
        return Ok(());
    }
    publication.entries.clear();
    publication.tail.clear();
    publication.spilled.clear();
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    Ok(())
}
pub(crate) fn has_exact(
    view: &record::StoredWorkspace,
    bytes: &[u8],
    store: &dyn super::Store,
) -> Result<bool, String> {
    let publication = state_of(view)?;
    if publication
        .entries
        .iter()
        .chain(&publication.tail)
        .any(|entry| entry.bytes == bytes)
    {
        return Ok(true);
    }
    Ok(spilled_writes(publication, store, view.workspace)?
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
        .chain(publication.tail.iter())
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
        if !publication
            .governance
            .iter()
            .any(|item| item.id == entry.id)
        {
            validate_exact(&view.peer, &message, view.device().device_public_key)?;
        }
    }
    let written: Vec<u64> = publication
        .entries
        .iter()
        .chain(&publication.tail)
        .map(|entry| entry.ordinal)
        .collect();
    if written
        .windows(2)
        .any(|pair| matches!(pair, [first, second] if first >= second))
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
            || entry.binding != checkpoint_root_binding(view.record.workspace, root)
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

/// The binding a publication of a workspace's checkpoint root carried. No
/// device anchors new checkpoint content, but a workspace whose root was
/// anchored before still publishes it as it was signed.
fn checkpoint_root_binding(workspace: WorkspaceId, address: [u8; 32]) -> CommitBinding {
    CommitBinding {
        workspace: workspace.to_string(),
        target: crate::owner_commit::CommitTarget::NativeCheckpointRoot { root: address },
        request_digest: *blake3::Hasher::new_derive_key(
            "Tilo native checkpoint root publication v1",
        )
        .update(&address)
        .finalize()
        .as_bytes(),
        basis: address,
    }
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
    /// What the host holds of `record` of `app` that the record holds here only inside a
    /// compaction of this device's own; `None` while the record's history is the host's.
    /// `(store, workspace)` keep its runs.
    fn hosted(
        &self,
        (store, workspace): Runs<'_>,
        (app, record): (&str, &str),
    ) -> Result<Option<Hosted>, String> {
        let Some(held) = self
            .hosted
            .iter()
            .find(|held| held.app == app && held.record == record)
        else {
            return Ok(None);
        };
        let mut writes = Vec::new();
        for hash in &held.runs {
            let run: Vec<HostedWrite> =
                postcard::from_bytes(&store.read_custody(workspace, *hash)?)
                    .map_err(|e| e.to_string())?;
            writes.extend(run);
        }
        Ok(Some(Hosted {
            writes: writes
                .iter()
                .chain(&held.writes)
                .map(|write| postcard::from_bytes(&write.0).map_err(|e| e.to_string()))
                .collect::<Result<_, String>>()?,
            covered: held.covered.iter().copied().collect(),
        }))
    }
    fn set_hosted(
        &mut self,
        runs: Runs<'_>,
        (app, record): (&str, &str),
        hosted: Option<&Hosted>,
    ) -> Result<(), String> {
        self.hosted
            .retain(|held| held.app != app || held.record != record);
        if let Some(hosted) = hosted {
            let writes = hosted
                .writes
                .iter()
                .map(|write| {
                    postcard::to_stdvec(write)
                        .map(HostedWrite)
                        .map_err(|e| e.to_string())
                })
                .collect::<Result<_, _>>()?;
            let mut held = HostedHistory {
                app: app.to_owned(),
                record: record.to_owned(),
                runs: Vec::new(),
                writes,
                covered: hosted.covered.iter().map(|(a, s)| (*a, *s)).collect(),
            };
            held.spill(runs)?;
            self.hosted.push(held);
        }
        Ok(())
    }
    /// Take `write`, one of this device's that the host now holds, into what this device knows
    /// of the host's history of `record` of `app`, unless the record holds it as itself or a
    /// compaction of the host's covers it.
    fn hosted_write(
        &mut self,
        live: &StatePeer,
        runs: Runs<'_>,
        (app, record): (&str, &str),
        write: &SignedOp,
    ) -> Result<(), String> {
        let Some(held) = self
            .hosted
            .iter_mut()
            .find(|held| held.app == app && held.record == record)
        else {
            return Ok(());
        };
        if held
            .covered
            .iter()
            .any(|(author, seq)| author == write.author.as_bytes() && write.seq <= *seq)
            || live.publication().holds_sealed(app, record, write)
        {
            return Ok(());
        }
        held.writes.push(HostedWrite(
            postcard::to_stdvec(write).map_err(|e| e.to_string())?,
        ));
        held.spill(runs)
    }
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
    /// How many of this device's committed writes to `workspace` are still in
    /// its outgoing custody: not yet admitted by the workspace's governor.
    pub fn unadmitted_writes(&self, workspace: WorkspaceId) -> Result<usize, String> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            Ok(publication
                .spilled
                .iter()
                .map(|run| run.writes.saturating_sub(run.taken) as usize)
                .fold(
                    publication
                        .entries
                        .len()
                        .saturating_add(publication.tail.len()),
                    usize::saturating_add,
                ))
        })
    }
    /// Every write of this device to `workspace` still in its outgoing custody, sendable now or
    /// not, by id, in the order it sends them: what a caller waits on to know all of them reached
    /// the workspace's host.
    ///
    /// # Errors
    /// The workspace is not held here, or custody's backlog could not be read.
    pub fn owed_writes(&self, workspace: WorkspaceId) -> Result<Vec<[u8; 32]>, String> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            let spilled = spilled_writes(publication, self.0.store.as_ref(), workspace)?;
            Ok(publication
                .entries
                .iter()
                .chain(&spilled)
                .chain(&publication.tail)
                .map(|entry| entry.id)
                .collect())
        })
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
    /// The snapshots are signed by `signer`, this device's custody of its key,
    /// outside the owner's turn.
    ///
    /// # Errors
    /// The workspace is not held here, `signer` is not this device's, signing
    /// failed, or the commit failed.
    pub async fn commit_bootstrap(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        crate::owner::write::signed_turn(signer, |signed| {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            if signed.device_public_key() != candidate.device().device_public_key {
                return Err("native bootstrap signer differs from selected device".into());
            }
            // A device that does not host the workspace and holds no record in the
            // clear signs nothing for a catch-up and takes no number: there is
            // nothing to make durable, and a link costs no write of its record.
            if !candidate.record.hosted_here() {
                let mut live = candidate.live();
                if !live.holds_clear_records() {
                    let mut next = live.clone();
                    let messages = next.publication_mut().bootstrap_messages(signed, now)?;
                    signed.settled()?;
                    *live = next;
                    drop(live);
                    return Ok(messages);
                }
                drop(live);
            }
            let (messages, stored) = candidate.change(|live, stored| {
                let generation = stored
                    .generation
                    .checked_add(1)
                    .ok_or("native owner generation exhausted")?;
                let messages = if stored.hosted_here() {
                    live.publication_mut()
                        .durable_bootstrap_messages(signed, now, generation)?
                } else {
                    live.publication_mut().bootstrap_messages(signed, now)?
                };
                signed.settled()?;
                stored.generation = generation;
                Ok(messages)
            })?;
            self.publish_record(workspace, &candidate, stored, None)?;
            Ok(messages)
        })
        .await
    }
    /// Retire every retained frame the workspace has refused or a durable
    /// receipt names, and commit the retirement.
    pub fn retire_outgoing(&self, workspace: WorkspaceId) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let now = tilo_clock::wall();
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
        let now = tilo_clock::wall();
        let runs = (self.0.store.as_ref(), workspace);
        let (changed, mut record) = candidate.change(|live, record| {
            let publication = record.publication.as_mut().ok_or("publication absent")?;
            // A refused compaction keeps its place, and is drafted again there against what
            // the host holds now: no later write of its record has left before it.
            let again: Vec<[u8; 32]> = publication
                .entries
                .iter()
                .filter(|entry| {
                    answers.get(&entry.id) == Some(&HostAnswer::Refused) && is_compaction(entry)
                })
                .map(|entry| entry.id)
                .collect();
            let (answered, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut publication.entries)
                .into_iter()
                .partition(|entry| answers.contains_key(&entry.id) && !again.contains(&entry.id));
            publication.entries = kept;
            publication.slots.extend(&again);
            if answered.is_empty() && again.is_empty() {
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
                let route = (app_id.as_str(), record_id.as_str());
                match (envelope.kind, answer) {
                    (OpKind::Compaction, _) => {
                        let owed = owed_to(publication, runs, route)?;
                        take_hosted(live, publication, runs, route, &envelope, &owed, now)?;
                    }
                    (_, HostAnswer::Refused) => {
                        live.withdraw_local_document(&app_id, &record_id, &envelope)?;
                    }
                    (_, HostAnswer::Durable(_)) => {
                        publication.hosted_write(live, runs, route, &envelope)?;
                    }
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
            let spilled = spilled_writes(publication, self.0.store.as_ref(), workspace)?;
            for entry in publication
                .entries
                .iter()
                .chain(&spilled)
                .chain(&publication.tail)
            {
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
    /// governor retired them, with the keys [`Self::keep_sealed_histories`] gave
    /// (see [`PeerOutbox::draft_sealed_renewals`](crate::PeerOutbox::draft_sealed_renewals)).
    /// Each renewal replaces its original in one durable commit and moves behind every other
    /// entry, so an author's writes still send in sequence order. A write the keys do not renew
    /// stays exactly as it was. Answers how many writes were renewed.
    ///
    /// # Errors
    /// As [`Self::commit_outgoing`].
    pub async fn renew_outgoing(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let Some(keys) = self.0.history_keys.lock().clone() else {
            return Ok(0);
        };
        let mut reseal = |write: &SignedOp| keys.renew(workspace, write);
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
                .filter(|((_, original), _)| {
                    // A compaction is drafted when it is sent, under the epoch of that moment.
                    !matches!(original, NeighbourMsg::AppDocument { envelope, .. }
                        if envelope.kind == OpKind::Compaction)
                })
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
        let admit =
            |live: &mut StatePeer, _: &mut StoredPublication, original: NeighbourMsg, renewal| {
                live.outbox_mut()
                    .admit_sealed_renewal(&original, renewal, now)
            };
        self.commit_outgoing(workspace, signer, now, draft, admit)
            .await
    }

    /// Keep every sealed record history this owner holds within its bound
    /// with `keys`, the channel keys this device holds: before each of this
    /// device's writes, and whenever its holder asks
    /// ([`Self::compact_outgoing`]). Compaction does not wait for a link, so a
    /// device alone or offline goes on writing however long a record's
    /// history grows.
    pub fn keep_sealed_histories(&self, keys: Arc<dyn SealedHistoryKeys>) {
        *self.0.history_keys.lock() = Some(keys);
    }

    /// Compact every sealed record history of `workspace` that has used most of
    /// its bound into one sealed checkpoint, and commit the compactions
    /// into this device's outgoing custody, where they publish as the
    /// checkpoints they are, after the writes they cover. The keys
    /// [`Self::keep_sealed_histories`] gave open any write of a history this
    /// device can read and seal a record's merged snapshot under its channel's
    /// current epoch
    /// ([`PeerPublication::draft_sealed_compaction`](crate::PeerPublication::draft_sealed_compaction)).
    /// A record whose keys are not here, or whose history does not merge yet,
    /// waits: a workspace none of whose due histories compacted is tried again
    /// only after [`COMPACTION_RETRY`] more passes, as merging a whole history
    /// on every keystroke would cost more than the write.
    ///
    /// Answers how many histories were compacted. Every device taking part
    /// maintains its own copy this way; no host compacts for it.
    ///
    /// # Errors
    /// As [`Self::commit_outgoing`].
    pub async fn compact_outgoing(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        self.compact(workspace, None, signer, now).await
    }

    /// [`Self::compact_outgoing`], with `record` of `app` compacted now whatever share of its
    /// bound its history used: the checkpoint a workspace's host asks of a member is this
    /// compaction, which every replica takes in place of exactly the writes it covers, so a
    /// record's history never holds a checkpoint beside the writes it re-states.
    ///
    /// # Errors
    /// As [`Self::commit_outgoing`].
    pub async fn compact_record(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let route = (app.to_owned(), record.to_owned());
        self.compact(workspace, Some(route), signer, now).await
    }

    /// Custody's entry of this device's latest compaction of `record` of `app` in `workspace`,
    /// once it is drafted to send: `None` while it waits for its place, or when custody holds
    /// none.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub fn drafted_compaction(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
    ) -> Result<Option<Vec<u8>>, String> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            let latest = publication.entries.iter().rev().find(|entry| {
                postcard::from_bytes::<NeighbourMsg>(&entry.bytes).is_ok_and(|message| {
                    matches!(&message, NeighbourMsg::AppDocument { app_id, record_id, envelope, .. }
                        if app_id == app && record_id == record && envelope.kind == OpKind::Compaction)
                })
            });
            Ok(latest
                .filter(|entry| !publication.slots.contains(&entry.id))
                .map(|entry| entry.bytes.clone()))
        })
    }

    async fn compact(
        &self,
        workspace: WorkspaceId,
        asked: Option<(String, String)>,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let Some(keys) = self.0.history_keys.lock().clone() else {
            return Ok(0);
        };
        self.draft_next(workspace, signer, keys.as_ref(), now)
            .await?;
        // A record asked for is compacted at once, whatever share of its bound it used.
        if asked.is_none()
            && self.with_current(workspace, |view| {
                Ok(view
                    .peer
                    .publication()
                    .sealed_records_to_compact()
                    .is_empty())
            })?
        {
            return Ok(0);
        }
        if asked.is_none()
            && let Some(wait) = self.0.compaction_wait.lock().get_mut(&workspace)
            && *wait > 0
        {
            *wait = wait.saturating_sub(1);
            return Ok(0);
        }
        let runs = (self.0.store.as_ref(), workspace);
        let draft = |view: &NativeCommittedView| {
            let device = view.device().device_public_key;
            let mut due = view.peer.publication().sealed_records_to_compact();
            if let Some(route) = asked
                && !due.contains(&route)
            {
                due.push(route);
            }
            let publication = state(view)?;
            let mut open = |write: &SignedOp| keys.open(workspace, write);
            due.into_iter()
                .filter_map(|(app, record)| {
                    let mut seal =
                        |snapshot: &[u8]| keys.seal_checkpoint(workspace, &app, &record, snapshot);
                    let route = (app.as_str(), record.as_str());
                    let drafted = view.peer.publication().draft_sealed_compaction(
                        route, None, &mut open, &mut seal, device, now,
                    );
                    let op = drafted
                        .inspect_err(|error| {
                            tracing::warn!(%error, %app, %record, "sealed record history stays uncompacted");
                        })
                        .ok()?;
                    Some(
                        owed_to(publication, runs, route)
                            .map(|owed| (None, (app, record, owed), op)),
                    )
                })
                .collect()
        };
        // The record's history here becomes the compaction; the host's writes it holds only
        // inside it now are what this device knows of the host's history beside it.
        let admit = |live: &mut StatePeer,
                     publication: &mut StoredPublication,
                     (app, record, owed): (String, String, Vec<SignedOp>),
                     op| {
            compacted_here(live, publication, runs, (&app, &record), &owed, op, now)
        };
        let compacted = self
            .commit_outgoing(workspace, signer, now, draft, admit)
            .await?;
        if compacted == 0 {
            self.0
                .compaction_wait
                .lock()
                .insert(workspace, COMPACTION_RETRY);
        } else {
            self.0.compaction_wait.lock().remove(&workspace);
        }
        Ok(compacted)
    }

    /// Draft the compaction custody of `workspace` sends next, against the history its host
    /// acknowledged: every write custody held before it is answered. It takes the place and
    /// sequence of the compaction this device made of its own history, and the record's own
    /// history takes it too where it was that compaction. One the host holds nothing for, or
    /// that cannot be drafted, is let go.
    async fn draft_next(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        keys: &dyn SealedHistoryKeys,
        now: SystemTime,
    ) -> Result<(), String> {
        let runs = (self.0.store.as_ref(), workspace);
        let next = self.with_current(workspace, |view| next_drafted(view, runs, keys, now))?;
        let Some((id, message, drafted)) = next else {
            return Ok(());
        };
        let Some((op, history)) = drafted else {
            return self.drop_slot(workspace, id);
        };
        let NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } = &message
        else {
            return Err("a compaction in custody is a document write".into());
        };
        let route = (app_id.as_str(), record_id.as_str());
        let draft = |_: &NativeCommittedView| Ok(vec![(Some(id), message.clone(), op)]);
        let admit = |live: &mut StatePeer,
                     publication: &mut StoredPublication,
                     message: NeighbourMsg,
                     op: SignedOp| {
            drafted_here(live, publication, runs, (route, &history), message, op, now)
        };
        // A compaction that cannot go as drafted is let go: custody never waits on it.
        match self
            .commit_outgoing(workspace, signer, now, draft, admit)
            .await
        {
            Ok(1..) => Ok(()),
            Ok(0) => self.drop_slot(workspace, id),
            Err(error) => {
                tracing::warn!(%error, "a compaction stays unsent");
                self.drop_slot(workspace, id)
            }
        }
    }

    /// Let go of `id`, a compaction custody holds the place of: custody never waits on one
    /// that cannot go.
    fn drop_slot(&self, workspace: WorkspaceId, id: [u8; 32]) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let (changed, mut record) = candidate.change(|_, record| {
            let publication = record.publication.as_mut().ok_or("publication absent")?;
            if !publication.slots.contains(&id) {
                return Ok(false);
            }
            publication.slots.retain(|slot| *slot != id);
            publication.entries.retain(|entry| entry.id != id);
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

    /// Commit the writes `draft` drafts of `workspace`, each with the custody entry it replaces,
    /// into the outgoing custody in one durable commit of a signed turn of `signer`, this device's
    /// custody of its key: what the first run drafted is kept, and signed between the runs. A
    /// write `admit` refuses, or one whose entry left custody meanwhile, stays out. A write that
    /// replaces a compaction custody holds the place of takes that place; any other goes behind
    /// every write custody holds, and a compaction keeps its place there until it is drafted.
    /// Answers how many writes were taken.
    ///
    /// # Errors
    /// `signer` is not the enrolled device, it may no longer write, the workspace is fenced,
    /// signing failed, or what was taken cannot be committed.
    async fn commit_outgoing<T: Clone>(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
        draft: impl FnOnce(&NativeCommittedView) -> Result<Vec<(Option<[u8; 32]>, T, SignedOp)>, String>,
        mut admit: impl FnMut(
            &mut StatePeer,
            &mut StoredPublication,
            T,
            SignedOp,
        ) -> Result<NeighbourMsg, String>,
    ) -> Result<usize, String> {
        let mut draft = Some(draft);
        let mut drafted = None;
        crate::owner::write::signed_turn(signer, |signed| {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            let device = candidate.device().device_public_key;
            if signed.device_public_key() != device {
                return Err("native outgoing signer differs from selected device".into());
            }
            candidate
                .peer
                .permissions(device, now)
                .map_err(|error| error.to_string())?;
            let drafted = crate::owner::write::drafted_once(&mut drafted, &mut draft, &candidate)?;
            if drafted.is_empty() {
                return Ok(0);
            }
            let mut writes = Vec::with_capacity(drafted.len());
            for (replaces, with, op) in drafted.iter() {
                let mut op = op.clone();
                collab_types::sign_op(&mut op, signed).map_err(|error| error.to_string())?;
                writes.push((*replaces, with.clone(), op));
            }
            signed.settled()?;
            let (taken, mut record) = candidate.change(|live, record| {
                let mut taken = 0usize;
                for (replaces, with, op) in writes {
                    let publication = record.publication.as_mut().ok_or("publication absent")?;
                    if replaces
                        .is_some_and(|id| !publication.entries.iter().any(|kept| kept.id == id))
                    {
                        continue;
                    }
                    let compaction = op.kind == OpKind::Compaction;
                    // Admitted on copies: a write that cannot be committed as one custody entry
                    // leaves the replica and custody as they were.
                    let mut attempt = live.clone();
                    let mut kept = publication.clone();
                    match admit(&mut attempt, &mut kept, with, op)
                        .and_then(|message| encode(&message, MAX_BYTES))
                    {
                        Ok(bytes) => {
                            *publication = kept;
                            *live = attempt;
                            match replaces {
                                Some(id) if publication.slots.contains(&id) => {
                                    in_place(record, id, bytes)?;
                                }
                                _ => {
                                    publication.entries.retain(|kept| Some(kept.id) != replaces);
                                    let id = *blake3::hash(&bytes).as_bytes();
                                    append(record, bytes)?;
                                    if compaction {
                                        record
                                            .publication
                                            .as_mut()
                                            .ok_or("publication absent")?
                                            .slots
                                            .push(id);
                                    }
                                }
                            }
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
        })
        .await
    }

    /// Take `message` into `publication` when it is a compaction the host holds of a record this
    /// device compacted ahead of the host: the record's history here does not line up with it,
    /// so it goes into what this device knows of the host's history, and the record's history
    /// takes it once it holds everything the record's own compaction folded. `None` for any
    /// other input, which is admitted as it is.
    pub(crate) fn receive_hosted(
        &self,
        candidate: &NativeCommittedView,
        publication: &mut StoredPublication,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Option<Result<crate::AdmissionReport, String>> {
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = message
        else {
            return None;
        };
        let route = (app_id.as_str(), record_id.as_str());
        if envelope.kind != OpKind::Compaction
            || !publication
                .hosted
                .iter()
                .any(|held| held.app == route.0 && held.record == route.1)
        {
            return None;
        }
        let runs = (self.0.store.as_ref(), candidate.record.workspace);
        let mut live = candidate.live();
        let taken = (|| {
            live.publication()
                .check_sealed_compaction(route, envelope, now)?;
            let owed = owed_to(publication, runs, route)?;
            let mut peer = live.clone();
            let mut next = publication.clone();
            // This device's compactions the host took before this one, whose answers are still
            // on their way.
            for sent in owed
                .iter()
                .filter(|sent| sent.kind == OpKind::Compaction && covers(envelope, sent))
            {
                take_hosted(&mut peer, &mut next, runs, route, sent, &owed, now)?;
            }
            take_hosted(&mut peer, &mut next, runs, route, envelope, &owed, now)?;
            let admission = crate::admission::DocumentAdmission::from_op(
                cluster_id, route.0, route.1, envelope,
            )
            .map_err(|error| error.to_string())?;
            *live = peer;
            *publication = next;
            Ok(crate::AdmissionReport::new(
                crate::InputDisposition::Accepted(admission),
                crate::ReceiveOutcome {
                    changed: true,
                    metadata_changed: true,
                    ..Default::default()
                },
            ))
        })();
        Some(taken)
    }
}
/// Commit `op`, a compaction of this device's own history of `record` of `app`, into the
/// record: its history becomes the compaction, and the host's writes it holds only inside it now
/// are what this device knows of the host's history beside it. `owed` are the writes custody
/// still owes the host.
fn compacted_here(
    live: &mut StatePeer,
    publication: &mut StoredPublication,
    runs: Runs<'_>,
    (app, record): (&str, &str),
    owed: &[SignedOp],
    op: SignedOp,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    let history = host_history(live, publication, runs, (app, record), owed)?;
    let mut hosted = if let Some(hosted) = publication.hosted(runs, (app, record))? {
        hosted
    } else {
        // The record's history was the host's: its compaction, if any, is first.
        let mut hosted = Hosted::default();
        if let Some(first) = history
            .first()
            .filter(|first| first.kind == OpKind::Compaction)
        {
            hosted.take(first)?;
        }
        hosted
    };
    let message = live
        .publication_mut()
        .admit_sealed_compaction(app, record, op, now)?;
    hosted.writes = history
        .into_iter()
        .filter(|write| !live.publication().holds_sealed(app, record, write))
        .collect();
    publication.set_hosted(runs, (app, record), Some(&hosted))?;
    Ok(message)
}
/// The compaction custody of `view` sends next, when custody holds its place at its head:
/// drafted against `history`, the host's history as it acknowledged it, under the sequence of
/// the place, as the record's custody message, or `None` when it cannot be drafted.
type NextDrafted = ([u8; 32], NeighbourMsg, Option<(SignedOp, Vec<SignedOp>)>);
fn next_drafted(
    view: &NativeCommittedView,
    runs: Runs<'_>,
    keys: &dyn SealedHistoryKeys,
    now: SystemTime,
) -> Result<Option<NextDrafted>, String> {
    let workspace = view.record.workspace;
    let publication = state(view)?;
    let Some(entry) = publication
        .entries
        .first()
        .filter(|entry| publication.slots.contains(&entry.id))
    else {
        return Ok(None);
    };
    let message: NeighbourMsg = postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
    let NeighbourMsg::AppDocument {
        app_id,
        record_id,
        envelope,
        ..
    } = &message
    else {
        return Err("a compaction in custody is a document write".into());
    };
    let route = (app_id.as_str(), record_id.as_str());
    let owed = owed_to(publication, runs, route)?;
    let history = host_history(&view.peer, publication, runs, route, &owed)?;
    let mut open = |write: &SignedOp| keys.open(workspace, write);
    let mut seal = |snapshot: &[u8]| keys.seal_checkpoint(workspace, route.0, route.1, snapshot);
    let drafted = view.peer.publication().draft_sealed_compaction_of(
        route,
        history.clone(),
        &mut open,
        &mut seal,
        (view.device().device_public_key, envelope.seq),
        now,
    );
    let drafted = drafted
        .inspect_err(|error| tracing::warn!(%error, "a compaction stays unsent"))
        .ok();
    Ok(Some((entry.id, message, drafted.map(|op| (op, history)))))
}
/// Commit `op`, drafted against `history` (the host's history of `record` of `app` as it
/// acknowledged it) in place of `message`, custody's compaction of this device's own history:
/// where the record's history is that compaction, it takes `op` in its place. Answers the
/// custody message `op` goes as.
fn drafted_here(
    live: &mut StatePeer,
    publication: &mut StoredPublication,
    runs: Runs<'_>,
    (route, history): ((&str, &str), &[SignedOp]),
    message: NeighbourMsg,
    op: SignedOp,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    let NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        record_id,
        envelope,
    } = message
    else {
        return Err("a compaction in custody is a document write".into());
    };
    let first = live
        .publication()
        .sealed_history(route.0, route.1)
        .first()
        .map(|first| first.signature);
    if first == Some(envelope.signature) {
        let mut hosted = publication.hosted(runs, route)?.unwrap_or_default();
        live.publication_mut().take_sealed_compaction(
            route,
            &op,
            &|write| hosted.covers(write),
            now,
        )?;
        hosted.writes = history
            .iter()
            .filter(|write| !live.publication().holds_sealed(route.0, route.1, write))
            .cloned()
            .collect();
        publication.set_hosted(runs, route, Some(&hosted))?;
    } else {
        live.publication_mut()
            .record_sealed_compaction(route.0, route.1, &op)?;
    }
    Ok(NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        record_id,
        envelope: Box::new(op),
    })
}
/// Put `bytes`, a compaction drafted for the host, where the compaction custody held the
/// place of as `id` stands: it goes in that compaction's turn.
fn in_place(
    view: &mut record::StoredWorkspace,
    id: [u8; 32],
    bytes: Vec<u8>,
) -> Result<(), String> {
    let message: NeighbourMsg = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
    let (timestamp_ns, channel, incremental) = metadata(&message)?;
    let binding = request(&message)?.binding()?;
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    let entry = publication
        .entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or("the compaction custody held left it")?;
    *entry = StoredOutgoing {
        ordinal: entry.ordinal,
        id: *blake3::hash(&bytes).as_bytes(),
        bytes,
        binding,
        timestamp_ns,
        channel,
        incremental,
    };
    publication.slots.retain(|slot| *slot != id);
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    Ok(())
}
/// Whether `entry` of custody is a compaction.
fn is_compaction(entry: &StoredOutgoing) -> bool {
    matches!(postcard::from_bytes(&entry.bytes),
        Ok(NeighbourMsg::AppDocument { envelope, .. }) if envelope.kind == OpKind::Compaction)
}
/// Whether `compaction` covers `write`.
fn covers(compaction: &SignedOp, write: &SignedOp) -> bool {
    compaction.kind == OpKind::Compaction
        && collab_types::SealedCompaction::parse(compaction)
            .is_ok_and(|wrapper| wrapper.covers(write))
}
/// This device's writes of `record` of `app` its custody still owes the host, in the order it
/// sends them.
fn owed_to(
    publication: &StoredPublication,
    (store, workspace): Runs<'_>,
    (app, record): (&str, &str),
) -> Result<Vec<SignedOp>, String> {
    let spilled = spilled_writes(publication, store, workspace)?;
    Ok(publication
        .entries
        .iter()
        .chain(&spilled)
        .chain(&publication.tail)
        .filter_map(|entry| match postcard::from_bytes(&entry.bytes) {
            Ok(NeighbourMsg::AppDocument {
                app_id,
                record_id,
                envelope,
                ..
            }) if app_id == app && record_id == record => Some(*envelope),
            _ => None,
        })
        .collect())
}
/// The history the host holds of `record` of `app` as far as it acknowledged it: what this
/// device holds of it, but `owed`, the writes custody still owes the host.
fn host_history(
    live: &StatePeer,
    publication: &StoredPublication,
    runs: Runs<'_>,
    (app, record): (&str, &str),
    owed: &[SignedOp],
) -> Result<Vec<SignedOp>, String> {
    let held = live.publication().sealed_history(app, record);
    let history: Vec<SignedOp> = match publication.hosted(runs, (app, record))? {
        // The record's own compaction is first here, and the host never held it; nor does it
        // hold the writes here a compaction of its own folded.
        Some(hosted) => {
            let rest: Vec<SignedOp> = held
                .into_iter()
                .skip(1)
                .filter(|write| !hosted.covers(write))
                .collect();
            hosted.writes.into_iter().chain(rest).collect()
        }
        None => held,
    };
    Ok(history
        .into_iter()
        .filter(|write| !owed.iter().any(|owed| owed.signature == write.signature))
        .collect())
}
/// Take `compaction`, which the host now holds first in its history of `record` of `app`, into
/// what this device knows of that history. Once that holds nothing beside it and custody owes
/// none of the writes the record's own compaction folded, the record's history takes it in
/// that compaction's place: it is the host's again. `owed` are the writes custody still owes.
fn take_hosted(
    live: &mut StatePeer,
    publication: &mut StoredPublication,
    runs: Runs<'_>,
    route: (&str, &str),
    compaction: &SignedOp,
    owed: &[SignedOp],
    now: SystemTime,
) -> Result<(), String> {
    let held = live.publication().sealed_history(route.0, route.1);
    let first = held.first().map(|first| first.signature);
    let Some(mut hosted) = publication.hosted(runs, route)? else {
        // Taken already: a later compaction of the host's that covers it came first.
        if first != Some(compaction.signature)
            && !held.first().is_some_and(|first| covers(first, compaction))
        {
            live.publication_mut()
                .take_sealed_compaction(route, compaction, &|_| false, now)?;
        }
        return Ok(());
    };
    if hosted.covers(compaction)
        || hosted
            .writes
            .iter()
            .any(|write| write.signature == compaction.signature)
    {
        return Ok(());
    }
    hosted.take(compaction)?;
    let folded = owed.iter().any(|write| {
        write.kind != OpKind::Compaction
            && !held.iter().any(|kept| kept.signature == write.signature)
    });
    if hosted.writes.len() > 1 || folded {
        return publication.set_hosted(runs, route, Some(&hosted));
    }
    if first != Some(compaction.signature) {
        live.publication_mut().take_sealed_compaction(
            route,
            compaction,
            &|write| hosted.covers(write),
            now,
        )?;
    }
    publication.set_hosted(runs, route, None)
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
