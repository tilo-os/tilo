//! Same-barrier native outgoing custody; relay queue acceptance is never an ACK.
use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId, enrollment, record};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use neighbour_wire::Retention;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, time::SystemTime};
use tilo_owner_commit::{CommitBinding, CommitOperation, OwnerCommitRequest};
use tilo_peer_state::{Reseal, StatePeer};

#[cfg(test)]
mod recovery_tests;
mod reservation;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use reservation::check_actual_projection_bound;
pub use reservation::{authority_limit, preflight_storage, reserved_file_bytes};

pub(crate) const MAX_ENTRIES: usize = 64;
pub(crate) const MAX_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_VISIBLE_ENTRIES: usize = MAX_ENTRIES * 2 + 2;
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
    /// Base64 text in the owner's JSON files, like the record's other
    /// opaque bytes; entries written before that held JSON arrays of
    /// numbers, one line per byte pretty-printed, and still read back.
    #[serde(
        serialize_with = "crate::record::base64_bytes::serialize",
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
            .chain(self.state.projection.iter())
            .map(|entry| entry.id)
    }
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
    #[must_use]
    pub fn retention(&self) -> Retention {
        match postcard::from_bytes::<NeighbourMsg>(&self.0.bytes) {
            Ok(NeighbourMsg::SanitizedRoomBaseline { .. }) => Retention::Checkpoint,
            _ => Retention::Delta,
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
            *slot = view.device().device_public_key == view.governor().device_public_key
                && validate_governance(view, entry).is_ok();
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
            .outbox()
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
            (3, 4 | 5, true) => {}
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
        || publication.governance.len() > 2
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
    let (channel, kind) = neighbour_wire::retention(message, false)
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

/// Only the selected governor's exact signed baseline may replace retained history.
pub(crate) fn authorize_baseline_checkpoint(
    view: &NativeCommittedView,
    bytes: &[u8],
    channel: [u8; 32],
) -> Result<(), String> {
    if view.device().device_public_key != view.governor().device_public_key {
        return Err("native member checkpoint publication refused".into());
    }
    let message: NeighbourMsg = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
    let NeighbourMsg::SanitizedRoomBaseline { room, baseline } = message else {
        return Err("native checkpoint is not a privacy baseline".into());
    };
    let (decoded, tail) =
        postcard::take_from_bytes::<tilo_peer_state::SanitizedRoomBaseline>(&baseline)
            .map_err(|e| e.to_string())?;
    let stored = view
        .record
        .sanitized_baseline
        .as_ref()
        .ok_or("missing native baseline")?;
    if !tail.is_empty() || room != view.record.room.to_string() || &decoded != stored {
        return Err("native checkpoint differs from selected baseline".into());
    }
    let entry = state(view)?
        .governance
        .iter()
        .find(|entry| entry.bytes == bytes && entry.channel == channel)
        .ok_or("native checkpoint is not in committed custody")?;
    validate_governance(view, entry)
}

fn baseline_message(view: &NativeCommittedView) -> Result<NeighbourMsg, String> {
    Ok(NeighbourMsg::SanitizedRoomBaseline {
        room: view.record.room.to_string(),
        baseline: postcard::to_allocvec(
            view.record
                .sanitized_baseline
                .as_ref()
                .ok_or("missing native baseline")?,
        )
        .map_err(|e| e.to_string())?,
    })
}

fn refresh_sanitized_checkpoint(view: &mut NativeCommittedView) -> Result<(), String> {
    let baseline = view
        .record
        .sanitized_baseline
        .as_ref()
        .ok_or("missing native baseline")?;
    let binding = crate::checkpoint::binding(view.record.room, baseline.anchor().document_hash);
    let messages = [baseline_message(view)?, view.peer.room_message()];
    let old = state(view)?.governance.clone();
    let mut governance = Vec::new();
    for message in messages {
        let bytes = encode(&message, MAX_BYTES)?;
        if let Some(entry) = old.iter().find(|entry| entry.bytes == bytes) {
            governance.push(entry.clone());
            continue;
        }
        let (channel, retention) =
            neighbour_wire::retention(&message, true).ok_or("unsupported sanitized checkpoint")?;
        if retention != Retention::Checkpoint {
            return Err("sanitized baseline must be a checkpoint".into());
        }
        let publication = view
            .record
            .publication
            .as_mut()
            .ok_or("publication absent")?;
        let ordinal = publication.next_ordinal;
        publication.next_ordinal = ordinal
            .checked_add(1)
            .ok_or("publication ordinal exhausted")?;
        governance.push(StoredOutgoing {
            ordinal,
            id: *blake3::hash(&bytes).as_bytes(),
            bytes,
            binding: binding.clone(),
            timestamp_ns: view.record.enrolled_at_ns,
            channel,
            incremental: false,
        });
    }
    let publication = view
        .record
        .publication
        .as_mut()
        .ok_or("publication absent")?;
    if publication.governance != governance {
        publication.version = 3;
        publication.revision = publication
            .revision
            .checked_add(1)
            .ok_or("publication revision exhausted")?;
        publication.governance = governance;
    }
    bounds(publication)?;
    Ok(())
}

pub(crate) fn replace_governance_checkpoint(
    view: &mut NativeCommittedView,
    binding: CommitBinding,
    now: SystemTime,
) -> Result<(), String> {
    if view.record.sanitized_baseline.is_some() {
        return refresh_sanitized_checkpoint(view);
    }
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
    let (channel, retention) = neighbour_wire::retention(&message, true)
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
    if view.record.sanitized_baseline.is_some() {
        return refresh_sanitized_checkpoint(view);
    }
    let Some(existing) = state(view)?.governance.first().cloned() else {
        return Ok(());
    };
    let bytes = encode(&view.peer.room_message(), MAX_BYTES)?;
    let binding = if matches!(
        existing.binding.target,
        tilo_owner_commit::CommitTarget::NativeCheckpointRoot { .. }
    ) {
        crate::checkpoint::binding(
            view.record.room,
            view.peer
                .workspace()
                .workspace_checkpoint_root()?
                .ok_or("native checkpoint root disappeared")?,
        )
    } else {
        existing.binding.clone()
    };
    if bytes == existing.bytes && binding == existing.binding {
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
        binding,
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
    validate_inner(view, false)
}

pub(crate) fn validate_private_recovery(
    view: &NativeCommittedView,
    key: &SigningKey,
) -> Result<(), String> {
    if key.verifying_key().to_bytes() != view.governor().device_public_key {
        return Err("private history recovery requires exact governor custody".into());
    }
    validate_inner(view, true)
}

/// Authenticate an old checkpoint for owner-local private-history recovery only.
/// This does not admit the checkpoint, authorize replay, or validate current ancestry.
/// The caller must prove ancestry before replacing history and retain extracted text.
/// # Errors
/// Refuses foreign signing custody, bad signatures or framing; only recognized
/// historical privacy-policy violations can pass the strict document check.
pub fn verify_private_recovery_baseline(
    baseline: &tilo_peer_state::SanitizedRoomBaseline,
    room: &str,
    governor: [u8; 32],
    key: &SigningKey,
) -> Result<automerge::AutoCommit, String> {
    if key.verifying_key().to_bytes() != governor {
        return Err("private history recovery requires exact governor custody".into());
    }
    recovery_checkpoint(baseline, room, governor)
}

// This exception is only for authenticated local recovery, never receive or replay.
// Verification checks signature, anchor and checkpoint framing before privacy.
pub(crate) fn recovery_checkpoint(
    baseline: &tilo_peer_state::SanitizedRoomBaseline,
    room: &str,
    governor: [u8; 32],
) -> Result<automerge::AutoCommit, String> {
    match baseline.verify_anchored_document(room, governor, &baseline.anchor()) {
        Ok(doc) => Ok(doc),
        Err(error) if recoverable_private_baseline(&error) => {
            tilo_automerge_isolate::load(&baseline.document).map_err(|e| e.to_string())
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn recoverable_private_baseline(error: &str) -> bool {
    matches!(
        error,
        "baseline contains private model authority contract"
            | "baseline contains private automatic task reports"
            | "baseline contains private task presentation bodies"
            | "baseline contains private task confirmation bodies"
    )
}

fn validate_inner(view: &NativeCommittedView, private_recovery: bool) -> Result<(), String> {
    let publication = state(view)?;
    bounds(publication)?;
    if matches!(view.record.version, 4 | 5) != view.record.sanitized_baseline.is_some()
        || view.peer.sanitized_room_generation()?
            != view
                .record
                .sanitized_baseline
                .as_ref()
                .map_or(0, |baseline| baseline.generation)
    {
        return Err("native room baseline custody or generation mismatch".into());
    }
    if view.record.sanitized_baseline.is_some() {
        let invalid = "native sanitized publication is missing its baseline or current deltas";
        let [baseline, current] = publication.governance.as_slice() else {
            return Err(invalid.into());
        };
        if baseline.bytes != encode(&baseline_message(view)?, MAX_BYTES)?
            || current.bytes != encode(&view.peer.room_message(), MAX_BYTES)?
        {
            return Err(invalid.into());
        }
    }
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
            validate_governance_inner(view, entry, private_recovery)?;
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
        view.peer.outbox().validate_local_recovery_projection(
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
    validate_governance_inner(view, entry, false)
}

fn validate_governance_inner(
    view: &NativeCommittedView,
    entry: &StoredOutgoing,
    private_recovery: bool,
) -> Result<(), String> {
    let message: NeighbourMsg = postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string())?;
    if let Some(baseline) = &view.record.sanitized_baseline {
        let mut checkpoint = if private_recovery {
            recovery_checkpoint(
                baseline,
                &view.record.room.to_string(),
                view.governor().device_public_key,
            )?
        } else {
            baseline.verify_anchored_document(
                &view.record.room.to_string(),
                view.governor().device_public_key,
                &baseline.anchor(),
            )?
        };
        let mut current = view
            .peer
            .admitted_room()
            .ok_or("native room absent")?
            .clone();
        tilo_peer_state::validate_sanitized_room_sync(Some(&checkpoint), &mut current)?;
        if checkpoint
            .get_heads()
            .iter()
            .any(|head| current.get_change_by_hash(head).is_none())
        {
            return Err("native baseline is not an ancestor of the admitted room".into());
        }
        if view.peer.sanitized_room_generation()? != baseline.generation {
            return Err("native baseline generation differs from admitted room".into());
        }
        let canonical = baseline_message(view)?;
        if entry.bytes != encode(&canonical, MAX_BYTES)?
            && entry.bytes != encode(&view.peer.room_message(), MAX_BYTES)?
        {
            return Err("native sanitized checkpoint differs from committed room".into());
        }
        let (channel, retention) =
            neighbour_wire::retention(&message, true).ok_or("unsupported sanitized checkpoint")?;
        if retention != Retention::Checkpoint || channel != entry.channel || entry.incremental {
            return Err("invalid sanitized checkpoint transport".into());
        }
        return Ok(());
    }
    if !matches!(&message, NeighbourMsg::SyncResp { room, .. } if room == &view.record.room.to_string())
        || encode(&view.peer.room_message(), MAX_BYTES)? != entry.bytes
        || entry.id != *blake3::hash(&entry.bytes).as_bytes()
        || entry.incremental
    {
        return Err("native governance checkpoint differs from canonical room policy".into());
    }
    let (channel, retention) = neighbour_wire::retention(&message, true)
        .ok_or("unsupported native governance checkpoint")?;
    if retention != Retention::Checkpoint || channel != entry.channel {
        return Err("native governance checkpoint transport mismatch".into());
    }
    if let tilo_owner_commit::CommitTarget::NativeCheckpointRoot { root } = entry.binding.target {
        if view.peer.workspace().workspace_checkpoint_root()? != Some(root)
            || entry.binding != crate::checkpoint::binding(view.record.room, root)
        {
            return Err("native checkpoint publication differs from signed root".into());
        }
        return Ok(());
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
    if publication
        .governance
        .iter()
        .any(|entry| validate_governance(view, entry).is_err())
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
            view.peer.outbox().reserve_local_recovery(
                &originals,
                None,
                view.device().device_public_key,
            )
        })
        .is_ok_and(|reservation| reservation.entries() > 0);
    // A projection has no user receipt/exact index. Empty obligations prove its
    // whole history is in the actual durable governor receipt, independently of
    // whether this retransmission sequence itself was observed by the governor.
    let remove_projection = !publication.projection.is_empty()
        && !owes_recovery
        && decode_entries(&publication.projection)
            .and_then(|projection| {
                view.peer.outbox().validate_local_recovery_projection(
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
/// Inspect retained task payloads, not merely the presence of task maps. Manual
/// coordination remains public; managed-model inputs and automatic prose do not.
/// Walk every frontier and conflict so overwritten/deleted secrets cannot escape.
fn has_private_task_history(
    room: &automerge::AutoCommit,
    references: &tilo_peer_state::workspace_task::NativeTaskPrivateReferences,
) -> Result<bool, String> {
    if private_task_values(room, references)? {
        return Ok(true);
    }
    let mut history = room.clone();
    let hashes: Vec<_> = history
        .get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .collect();
    for hash in hashes {
        let past = history.fork_at(&[hash]).map_err(|e| e.to_string())?;
        if private_task_values(&past, references)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn task_history_values(
    room: &automerge::AutoCommit,
    field: &str,
) -> Result<std::collections::BTreeMap<String, Vec<serde_json::Value>>, String> {
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value};
    let mut result = std::collections::BTreeMap::<String, Vec<serde_json::Value>>::new();
    for (value, object) in room.get_all(ROOT, field).map_err(|e| e.to_string())? {
        if value != Value::Object(ObjType::Map) {
            return Err("task history root is not a map".into());
        }
        for id in room.keys(&object) {
            for (value, _) in room.get_all(&object, &id).map_err(|e| e.to_string())? {
                let Value::Scalar(value) = value else {
                    return Err("task history value is not bytes".into());
                };
                let ScalarValue::Bytes(bytes) = value.as_ref() else {
                    return Err("task history value is not bytes".into());
                };
                match serde_json::from_slice(bytes) {
                    Ok(value) => result.entry(id.clone()).or_default().push(value),
                    // Like extraction, leave unknown non-schema task blobs alone.
                    Err(_) if field == tilo_desktop_model::projection::ROOM_TASKS_KEY => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
        }
    }
    Ok(result)
}

fn private_confirmation_action(what: &serde_json::Value, has_note: bool) -> Result<bool, String> {
    use serde_json::Value;
    if !what.is_object() {
        return Err("task audit action is not an object".into());
    }
    if matches!(
        what.pointer("/action/action").and_then(Value::as_str),
        Some("ask-confirmation" | "confirm-action" | "decline-action")
    ) && (has_note
        || what
            .get("args")
            .is_some_and(|args| args != &serde_json::json!({})))
    {
        return Ok(true);
    }
    for field in ["members", "distinct"] {
        if let Some(members) = what.get(field) {
            for member in members
                .as_array()
                .ok_or("task audit members are not a list")?
            {
                if private_confirmation_action(member, has_note)? {
                    return Ok(true);
                }
            }
        }
    }
    if let Some(previous) = what.pointer("/of/what") {
        return private_confirmation_action(previous, has_note);
    }
    Ok(false)
}

/// Read-only counterpart of task extraction. Use the same typed confirmation,
/// audit and presentation payloads and opaque contract bindings as the sanitizer.
fn private_task_values(
    room: &automerge::AutoCommit,
    references: &tilo_peer_state::workspace_task::NativeTaskPrivateReferences,
) -> Result<bool, String> {
    use serde_json::Value;
    let tasks = task_history_values(room, tilo_desktop_model::projection::ROOM_TASKS_KEY)?;
    for versions in tasks.values() {
        for task in versions {
            let automatic = matches!(
                task.get("route").and_then(Value::as_str),
                Some("model" | "rules_only")
            );
            if automatic
                && task
                    .pointer("/resource/agent_request")
                    .is_some_and(|value| !value.is_null())
            {
                return Ok(true);
            }
            if let Some(audit) = task.get("audit") {
                for event in audit.as_array().ok_or("task audit is not a list")? {
                    if private_task_audit(event, automatic)? {
                        return Ok(true);
                    }
                }
            }
        }
    }
    for (id, versions) in
        task_history_values(room, tilo_peer_state::workspace_task::TASK_CONTROL_KEY)?
    {
        // A conflicting automatic route must not inherit a manual exemption.
        let manual = tasks.get(&id).is_some_and(|tasks| {
            !tasks.is_empty()
                && tasks
                    .iter()
                    .all(|task| task.get("route").and_then(Value::as_str) == Some("manual"))
        });
        for control in versions {
            if private_task_control(&control, &id, manual, references)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn private_task_audit(event: &serde_json::Value, automatic: bool) -> Result<bool, String> {
    use serde_json::Value;
    if !event.is_object() {
        return Err("task audit event is not an object".into());
    }
    if let Some(what) = event.get("what")
        && private_confirmation_action(what, event.get("note").is_some())?
    {
        return Ok(true);
    }
    if automatic && event.get("note").is_some_and(|note| !note.is_null()) {
        let parsed: tilo_desktop_model::WorkspaceTaskAuditEvent =
            serde_json::from_value(event.clone()).map_err(|e| e.to_string())?;
        let neutral = match parsed.action() {
            "finish" => match event.pointer("/what/args/outcome").and_then(Value::as_str) {
                Some("done") => "done",
                Some("cancelled") => "cancelled",
                _ => "waiting",
            },
            "abandon" => "abandoned",
            _ => "progress",
        };
        return Ok(event.get("note").and_then(Value::as_str) != Some(neutral));
    }
    Ok(false)
}

fn private_task_control(
    control: &serde_json::Value,
    id: &str,
    manual: bool,
    references: &tilo_peer_state::workspace_task::NativeTaskPrivateReferences,
) -> Result<bool, String> {
    use serde_json::Value;
    if !control.is_object() {
        return Err("task control is not an object".into());
    }
    if let Some(model) = control
        .get("model_authority")
        .filter(|model| !model.is_null())
        && !model
            .get("contract")
            .is_some_and(tilo_peer_state::workspace_task::is_private_model_contract_reference)
    {
        return Ok(true);
    }
    if !manual {
        if control
            .get("instruction")
            .is_some_and(|value| !value.is_null())
        {
            return Ok(true);
        }
        if let Some(directions) = control.get("directions") {
            for direction in directions
                .as_array()
                .ok_or("task directions are not a list")?
            {
                let revision = direction
                    .get("revision")
                    .and_then(Value::as_u64)
                    .ok_or("task direction revision absent")?;
                let text = direction
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("task direction text absent")?;
                if references
                    .get(id)
                    .and_then(|known| known.get(&revision))
                    .map(String::as_str)
                    != Some(text)
                {
                    return Ok(true);
                }
            }
        }
        if let Some(presentations) = control.get("presentations") {
            for presentation in presentations
                .as_object()
                .ok_or("task presentations are not a map")?
                .values()
            {
                let parsed: collab_types::task_presentation::TaskPresentation =
                    serde_json::from_value(presentation.clone()).map_err(|e| e.to_string())?;
                if !parsed.text.is_empty()
                    || !parsed.reasoning.is_empty()
                    || !parsed.detail.is_empty()
                {
                    return Ok(true);
                }
            }
        }
    }
    if let Some(confirmation) = control.get("confirmation").filter(|value| !value.is_null()) {
        let full: tilo_desktop_model::WorkspaceTaskConfirmation =
            serde_json::from_value(confirmation.clone()).map_err(|e| e.to_string())?;
        let mut public = full.clone();
        public.draft = tilo_actions::Draft::default();
        match &mut public.state {
            tilo_desktop_model::WorkspaceTaskConfirmationState::Confirmed { edits, .. } => {
                edits.clear();
            }
            tilo_desktop_model::WorkspaceTaskConfirmationState::Declined { reason, .. } => {
                *reason = None;
            }
            tilo_desktop_model::WorkspaceTaskConfirmationState::Waiting => {}
        }
        if full != public {
            return Ok(true);
        }
    }
    Ok(false)
}

fn needs_private_task_migration(view: &NativeCommittedView) -> Result<bool, String> {
    let mut references = view.record.private_task_text.private_references();
    if let Some(baseline) = &view.record.sanitized_baseline {
        match baseline.verify_anchored_document(
            &view.record.room.to_string(),
            view.governor().device_public_key,
            &baseline.anchor(),
        ) {
            Ok(document) => {
                for (id, controls) in task_history_values(
                    &document,
                    tilo_peer_state::workspace_task::TASK_CONTROL_KEY,
                )? {
                    for control in controls {
                        if let Some(directions) = control.get("directions") {
                            for direction in directions
                                .as_array()
                                .ok_or("signed directions are not a list")?
                            {
                                let revision = direction
                                    .get("revision")
                                    .and_then(serde_json::Value::as_u64)
                                    .ok_or("signed direction revision absent")?;
                                let text = direction
                                    .get("text")
                                    .and_then(serde_json::Value::as_str)
                                    .ok_or("signed direction reference absent")?;
                                // A signature authenticates custody, not sanitization
                                // by an older producer. Plaintext is never a reference.
                                if !text.strip_prefix("private-instruction:").is_some_and(
                                    |digest| {
                                        digest.len() == 64
                                            && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                                    },
                                ) {
                                    return Ok(true);
                                }
                                let known = references.entry(id.clone()).or_default();
                                if known.get(&revision).is_some_and(|old| old != text) {
                                    return Err(
                                        "private direction differs from signed baseline".into()
                                    );
                                }
                                known.insert(revision, text.to_owned());
                            }
                        }
                    }
                }
            }
            Err(error) if recoverable_private_baseline(&error) => return Ok(true),
            Err(error) => return Err(error),
        }
    }
    view.peer
        .merge_native_task_private_direction_references(&mut references)?;
    has_private_task_history(
        view.peer.admitted_room().ok_or("publication room absent")?,
        &references,
    )
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
            if view.record.sanitized_baseline.is_none() && needs_private_task_migration(view)? {
                return Err("public history requires an authenticated privacy baseline".into());
            }
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
        // Public/manual task history needs no governor key. Only private leaves
        // require signed replacement, under the recovery projection's barrier.
        let migrated = if needs_private_task_migration(&candidate)? {
            enrollment::ensure_private_task_baseline(&mut candidate, key, now)?
        } else {
            false
        };
        let before = state(&candidate)?.revision;
        retire(&mut candidate)?;
        let originals = decode_entries(&state(&candidate)?.entries)?;
        let old = decode_entries(&state(&candidate)?.projection)?;
        let replacement = candidate
            .peer
            .outbox_mut()
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
        if migrated || before != state(&candidate)?.revision {
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
    /// (see [`PeerOutboxMut::renew_local_sealed`](tilo_peer_state::PeerOutboxMut::renew_local_sealed)). Each renewal replaces its
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
                .outbox_mut()
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
#[cfg(test)]
mod encoding_tests {
    use super::*;

    fn entry(bytes: Vec<u8>) -> StoredOutgoing {
        StoredOutgoing {
            ordinal: 3,
            id: [7; 32],
            bytes,
            binding: CommitBinding {
                room: "room".into(),
                target: tilo_owner_commit::CommitTarget::Document {
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
