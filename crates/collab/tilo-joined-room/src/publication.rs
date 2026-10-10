//! Same-barrier native outgoing custody; relay queue acceptance is never an ACK.
use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId, enrollment, record};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, time::SystemTime};
use tilo_peer_state::{NeighbourMsg, Reseal, StatePeer};
use tilo_peer_state::{
    owner_commit::{CommitBinding, CommitOperation, OwnerCommitRequest},
    transport::Retention,
};

mod reservation;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use reservation::check_actual_projection_bound;
pub use reservation::{preflight_storage, reserved_file_bytes};

pub(crate) const MAX_ENTRIES: usize = 64;
pub(crate) const MAX_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_VISIBLE_ENTRIES: usize = MAX_ENTRIES * 2 + 1;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredPublication {
    version: u32,
    revision: u64,
    next_ordinal: u64,
    #[serde(deserialize_with = "read_entries")]
    pub(crate) entries: Vec<StoredOutgoing>,
    #[serde(deserialize_with = "read_entries")]
    pub(crate) projection: Vec<StoredOutgoing>,
    #[serde(default, deserialize_with = "read_entries")]
    governance: Vec<StoredOutgoing>,
    reservation: reservation::StoredReservation,
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
            reservation: reservation::StoredReservation::default(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredOutgoing {
    ordinal: u64,
    id: [u8; 32],
    #[serde(deserialize_with = "read_message_bytes")]
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
    pub fn entries(&self) -> impl Iterator<Item = NativeOutgoingEntry<'a>> {
        let allowed = self.allowed;
        self.state
            .governance
            .iter()
            .chain(self.state.entries.iter())
            .chain(self.state.projection.iter())
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
    #[allow(
        clippy::unused_self,
        reason = "a per-entry accessor; every native outgoing entry is currently a delta"
    )]
    #[must_use]
    pub fn retention(&self) -> Retention {
        Retention::Delta
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
    input.deserialize_seq(Bytes(limit))
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
    view.record
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
        .saturating_add(publication.entries.len())
        .saturating_add(publication.projection.len());
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
        if !entry.incremental || !fresh(entry, now) {
            continue;
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
    if !publication.projection.is_empty() {
        let originals = decode_entries(&publication.entries)?;
        let projection = decode_entries(&publication.projection)?;
        if view
            .peer
            .authorize_local_recovery_projection(
                &originals,
                None,
                &projection,
                view.device().device_public_key,
                now,
            )
            .is_ok()
        {
            for (index, entry) in publication.projection.iter().enumerate() {
                if let Some(slot) = publication
                    .governance
                    .len()
                    .checked_add(publication.entries.len())
                    .and_then(|slot| slot.checked_add(index))
                    .and_then(|slot| allowed.get_mut(slot))
                {
                    *slot = entry.incremental && fresh(entry, now);
                }
            }
        }
    }
    Ok(allowed)
}
/// Whether a host file of layout `version` may carry `storage`.
///
/// # Errors
/// A room record or its publication does not belong in that layout.
pub fn validate_file_layout(
    version: u32,
    storage: &record::NativeAuthorityStorage,
) -> Result<(), String> {
    for room in storage.rooms.values() {
        match (version, room.version, room.publication.is_some()) {
            (3, 2, true) if room.effect_history_root.is_none() => {}
            (3, 3, true) if room.effect_history_root.is_some() => {}
            _ => return Err("native publication record/file layout mismatch".into()),
        }
        if let Some(publication) = &room.publication {
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
        || publication.projection.len() > MAX_ENTRIES
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
    if bytes > MAX_BYTES
        || publication
            .projection
            .iter()
            .try_fold(0usize, |sum, entry| sum.checked_add(entry.bytes.len()))
            .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("native outgoing byte capacity exceeded".into());
    }
    publication.reservation.bounds()?;
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
        room: cluster_id.clone(),
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
    let (channel, kind) = tilo_peer_state::transport::retention(message, false)
        .ok_or("unsupported native outgoing document payload")?;
    Ok((
        enrollment::ns(envelope.timestamp)?,
        channel,
        member_publishes_as_delta(kind, envelope.encryption.is_some()),
    ))
}
/// Whether a member publishes this document as a retained delta.
///
/// A member never replaces a record's retained checkpoint under its own
/// relay author: a cleartext snapshot is the governor's baseline and stays a
/// checkpoint, which [`allowed`] never releases and the session refuses.
///
/// A content-key ciphertext the classifier calls a checkpoint (a sealed
/// snapshot or compaction) is published as a delta instead: the host appends
/// it after the record's checkpoint rather than letting a member's write
/// replace it. This is the conservative choice and keeps the class
/// every sealed write had before the classifiers were unified, so stored
/// outgoing entries still validate. Letting a member's sealed checkpoint
/// compact the host's tail is a protocol decision this does not make.
fn member_publishes_as_delta(kind: Retention, sealed: bool) -> bool {
    kind == Retention::Delta || sealed
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
    let peer = tilo_peer_state::peer_handle(envelope.author.as_bytes());
    encode(
        &NeighbourMsg::AppDocument {
            peer,
            app_id: app.clone(),
            cluster_id: request.room.clone(),
            record_id: record.clone(),
            envelope: envelope.clone(),
        },
        MAX_BYTES,
    )
}
pub(crate) fn reserve(view: &NativeCommittedView, bytes: &[u8]) -> Result<(), String> {
    let publication = state(view)?;
    let total = bounds(publication)?;
    if publication.entries.len() >= MAX_ENTRIES
        || total
            .checked_add(bytes.len())
            .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("native outgoing capacity exceeded; existing edits retained".into());
    }
    Ok(())
}

pub(crate) fn replace_governance_checkpoint(
    view: &mut NativeCommittedView,
    binding: CommitBinding,
    now: SystemTime,
) -> Result<(), String> {
    let message = view.peer.room_message();
    let bytes = encode(&message, MAX_BYTES)?;
    let existing = state(view)?.governance.first();
    if existing.is_some_and(|entry| entry.bytes == bytes && entry.binding == binding) {
        return Ok(());
    }
    let total = bounds(state(view)?)?;
    let old = existing.map_or(0, |entry| entry.bytes.len());
    if total
        .checked_sub(old)
        .and_then(|size| size.checked_add(bytes.len()))
        .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("native governance checkpoint exceeds outgoing capacity".into());
    }
    let (channel, retention) = tilo_peer_state::transport::retention(&message, true)
        .ok_or("unsupported native governance checkpoint")?;
    if retention != Retention::Checkpoint {
        return Err("native governance publication is not a checkpoint".into());
    }
    let publication = view
        .record
        .publication
        .as_mut()
        .ok_or("publication absent")?;
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

pub(crate) fn refresh_governance_checkpoint(view: &mut NativeCommittedView) -> Result<(), String> {
    let Some(existing) = state(view)?.governance.first().cloned() else {
        return Ok(());
    };
    let bytes = encode(&view.peer.room_message(), MAX_BYTES)?;
    if bytes == existing.bytes {
        return Ok(());
    }
    let total = bounds(state(view)?)?;
    if total
        .checked_sub(existing.bytes.len())
        .and_then(|size| size.checked_add(bytes.len()))
        .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("refreshed native governance checkpoint exceeds outgoing capacity".into());
    }
    let publication = view
        .record
        .publication
        .as_mut()
        .ok_or("publication absent")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    publication.governance = vec![StoredOutgoing {
        id: *blake3::hash(&bytes).as_bytes(),
        bytes,
        ..existing
    }];
    Ok(())
}
fn append(view: &mut NativeCommittedView, bytes: Vec<u8>, projected: bool) -> Result<(), String> {
    let id = *blake3::hash(&bytes).as_bytes();
    if state(view)?
        .entries
        .iter()
        .chain(state(view)?.projection.iter())
        .any(|entry| entry.id == id && entry.bytes == bytes)
    {
        return Ok(());
    }
    if projected {
        let projection = &state(view)?.projection;
        if projection.len() >= MAX_ENTRIES
            || projection
                .iter()
                .map(|entry| entry.bytes.len())
                .sum::<usize>()
                .checked_add(bytes.len())
                .is_none_or(|size| size > MAX_BYTES)
        {
            return Err("native recovery projection capacity exceeded".into());
        }
    } else {
        reserve(view, &bytes)?;
    }
    let message: NeighbourMsg = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
    let (timestamp_ns, channel, incremental) = metadata(&message)?;
    let binding = request(&message)?.binding()?;
    let publication = view
        .record
        .publication
        .as_mut()
        .ok_or("publication absent")?;
    let ordinal = publication.next_ordinal;
    publication.next_ordinal = ordinal
        .checked_add(1)
        .ok_or("native publication ordinal exhausted")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    let target = if projected {
        &mut publication.projection
    } else {
        &mut publication.entries
    };
    target.push(StoredOutgoing {
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
pub(crate) fn append_local(view: &mut NativeCommittedView, bytes: Vec<u8>) -> Result<(), String> {
    append(view, bytes, false)?;
    reservation::refresh(view)
}
pub(crate) fn has_exact(view: &NativeCommittedView, bytes: &[u8]) -> Result<bool, String> {
    Ok(state(view)?
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
    peer.lookup_retained_local_document(message, device)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "native outgoing entry lacks exact committed admission".to_owned())
        .map(|_| ())
}
fn retired(peer: &StatePeer, message: &NeighbourMsg, device: [u8; 32]) -> Result<bool, String> {
    peer.retained_local_document_retired(message, device)
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
        .chain(publication.projection.iter())
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
    for entries in [&publication.entries, &publication.projection] {
        if entries
            .windows(2)
            .any(|pair| matches!(pair, [first, second] if first.ordinal >= second.ordinal))
        {
            return Err("native publication ordinal order mismatch".into());
        }
    }
    let originals = decode_entries(&publication.entries)?;
    if !publication.projection.is_empty() {
        view.peer.validate_local_recovery_projection(
            &originals,
            None,
            &decode_entries(&publication.projection)?,
            view.device().device_public_key,
        )?;
    }
    reservation::validate(view, &originals)?;
    Ok(())
}

fn validate_governance(view: &NativeCommittedView, entry: &StoredOutgoing) -> Result<(), String> {
    let message: NeighbourMsg = postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
    if !matches!(&message, NeighbourMsg::SyncResp { room, .. } if room == &view.record.room.to_string())
        || encode(&view.peer.room_message(), MAX_BYTES)? != entry.bytes
        || entry.id != *blake3::hash(&entry.bytes).as_bytes()
        || entry.incremental
    {
        return Err("native governance checkpoint differs from canonical room policy".into());
    }
    let (channel, retention) = tilo_peer_state::transport::retention(&message, true)
        .ok_or("unsupported native governance checkpoint")?;
    if retention != Retention::Checkpoint || channel != entry.channel {
        return Err("native governance checkpoint transport mismatch".into());
    }
    if !view
        .record
        .workspace_memberships
        .values()
        .any(|receipt| receipt.receipt.binding == entry.binding)
        && !view
            .record
            .effect_receipts
            .values()
            .any(|receipt| receipt.receipt.binding == entry.binding)
    {
        return Err("native governance checkpoint lacks its owner receipt".into());
    }
    Ok(())
}
/// An exact replay changes no peer state; reuse its barrier only when there
/// is no retirement, reservation repair or checkpoint refresh to publish.
pub(crate) fn exact_replay_unchanged(view: &NativeCommittedView) -> Result<bool, String> {
    let publication = state(view)?;
    let originals = decode_entries(&publication.entries)?;
    for message in &originals {
        if retired(&view.peer, message, view.device().device_public_key).unwrap_or(false) {
            return Ok(false);
        }
    }
    if reservation::validate(view, &originals).is_err()
        || (!publication.projection.is_empty() && !publication.reservation.has_recovery())
    {
        return Ok(false);
    }
    if let Some(checkpoint) = publication.governance.first()
        && encode(&view.peer.room_message(), MAX_BYTES)? != checkpoint.bytes
    {
        return Ok(false);
    }
    Ok(true)
}

pub(crate) fn retire(view: &mut NativeCommittedView) -> Result<(), String> {
    let publication = state(view)?;
    let mut remove = BTreeSet::new();
    for entry in &publication.entries {
        let message: NeighbourMsg =
            postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
        // A refused query cannot discard newly admitted revocations or prerequisites.
        if retired(&view.peer, &message, view.device().device_public_key).unwrap_or(false) {
            remove.insert(entry.id);
        }
    }
    // A projection also carries the sequence whose durable watermark retires
    // its originals. While any original still owes recovery, keep it: the next
    // reconciliation reuses it while it is current. Clearing it here signed a
    // fresh retransmission at every reconciliation, without bound, for each
    // original the governor already holds but has not yet acknowledged.
    let remaining = publication
        .entries
        .iter()
        .filter(|entry| !remove.contains(&entry.id))
        .cloned()
        .collect::<Vec<_>>();
    let owes_recovery = decode_entries(&remaining)
        .and_then(|originals| {
            view.peer
                .reserve_local_recovery(&originals, None, view.device().device_public_key)
        })
        .is_ok_and(|reservation| reservation.entries() > 0);
    // A projection has no user receipt/exact index. Empty obligations prove its
    // whole history is in the actual durable governor receipt, independently of
    // whether this retransmission sequence itself was observed by the governor.
    let remove_projection = !publication.projection.is_empty()
        && !owes_recovery
        && decode_entries(&publication.projection)
            .and_then(|projection| {
                view.peer.validate_local_recovery_projection(
                    &[],
                    None,
                    &projection,
                    view.device().device_public_key,
                )
            })
            .is_ok();
    if !remove.is_empty() || remove_projection {
        let publication = view
            .record
            .publication
            .as_mut()
            .ok_or("publication absent")?;
        publication
            .entries
            .retain(|entry| !remove.contains(&entry.id));
        if remove_projection {
            publication.projection.clear();
        }
        publication.revision = publication
            .revision
            .checked_add(1)
            .ok_or("native publication revision exhausted")?;
    }
    reservation::refresh(view)?;
    Ok(())
}
pub(crate) fn retry_covered(view: &NativeCommittedView, bytes: &[u8]) -> Result<bool, String> {
    let message: NeighbourMsg = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
    retired(&view.peer, &message, view.device().device_public_key)
}
impl NativeWorkspaceOwner {
    /// Read `room`'s outgoing custody: what the relay session may send now.
    ///
    /// # Errors
    /// The room is fenced, this device may not publish, or `read` fails.
    pub fn with_outgoing<T>(
        &self,
        room: RoomId,
        now: SystemTime,
        read: impl FnOnce(&NativeCommittedView, NativeOutgoing<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_current(room, |view| {
            view.peer
                .permissions(view.device().device_public_key, now)?;
            read(
                view,
                NativeOutgoing {
                    state: state(view)?,
                    allowed: allowed(view, now)?,
                },
            )
        })
    }
    /// # Errors
    /// The room is fenced.
    #[cfg(any(test, feature = "test-support"))]
    pub fn outgoing_status(
        &self,
        room: RoomId,
        now: SystemTime,
    ) -> Result<NativeOutgoingStatus, String> {
        self.with_current(room, |view| status(view, now))
    }
    /// Retire what the governor admitted and replace expired increments by
    /// a fresh recovery projection signed with `key`.
    ///
    /// # Errors
    /// `key` is not the enrolled device, the room is fenced or the record
    /// cannot be published.
    pub fn reconcile_outgoing(
        &self,
        room: RoomId,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if key.verifying_key().to_bytes() != candidate.device().device_public_key {
            return Err("native outgoing signer differs from selected device".into());
        }
        candidate
            .peer
            .permissions(candidate.device().device_public_key, now)?;
        let before = state(&candidate)?.revision;
        retire(&mut candidate)?;
        let originals = decode_entries(&state(&candidate)?.entries)?;
        let old = decode_entries(&state(&candidate)?.projection)?;
        let replacement = candidate
            .peer
            .reconcile_local_recovery_projection(&originals, None, &old, key, now)?;
        if replacement != old {
            let publication = candidate
                .record
                .publication
                .as_mut()
                .ok_or("publication absent")?;
            publication.projection.clear();
            publication.revision = publication
                .revision
                .checked_add(1)
                .ok_or("native projection revision exhausted")?;
            for message in replacement {
                append(&mut candidate, encode(&message, MAX_BYTES)?, true)?;
            }
        }
        reservation::refresh(&mut candidate)?;
        if before != state(&candidate)?.revision {
            candidate.record.peer = candidate.peer.durable()?;
            candidate.record.generation = candidate
                .generation()
                .checked_add(1)
                .ok_or("native owner generation exhausted")?;
            self.publish_room(room, candidate, None)?;
        }
        // Re-read the committed room so a publication that cannot be served is reported.
        self.with_current(room, |view| {
            state(view)?;
            allowed(view, now)?;
            Ok(())
        })
    }
    /// Renew this device's sealed writes that expired in custody before the
    /// governor retired them, through `reseal`, the channels' key holder
    /// (see [`StatePeer::renew_local_sealed`]). Each renewal replaces its
    /// original in one durable commit and moves behind every other entry,
    /// so an author's writes still send in sequence order. A write `reseal`
    /// refuses stays exactly as it was.
    ///
    /// # Errors
    /// `key` is not the enrolled device, it may no longer write, the room is
    /// fenced or the renewals cannot be committed.
    pub fn renew_outgoing(
        &self,
        room: RoomId,
        key: &SigningKey,
        reseal: &mut Reseal<'_>,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if key.verifying_key().to_bytes() != candidate.device().device_public_key {
            return Err("native outgoing signer differs from selected device".into());
        }
        candidate
            .peer
            .permissions(candidate.device().device_public_key, now)?;
        let mut renewed = false;
        for entry in state(&candidate)?.entries.clone() {
            let original: NeighbourMsg =
                postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
            let renewal = match candidate
                .peer
                .renew_local_sealed(&original, reseal, key, now)
            {
                Ok(Some(renewal)) => renewal,
                Ok(None) => continue,
                Err(error) => {
                    tracing::debug!(%error, "sealed write stays unrenewed in custody");
                    continue;
                }
            };
            candidate
                .record
                .publication
                .as_mut()
                .ok_or("publication absent")?
                .entries
                .retain(|kept| kept.id != entry.id);
            append(&mut candidate, encode(&renewal, MAX_BYTES)?, false)?;
            renewed = true;
        }
        if !renewed {
            return Ok(());
        }
        reservation::refresh(&mut candidate)?;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(room, candidate, None)
    }
}
