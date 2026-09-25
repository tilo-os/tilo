//! Same-barrier native outgoing custody; relay queue acceptance is never an ACK.
use super::*;
use ed25519_dalek::SigningKey;
use qos_peer_state::{
    owner_commit::{CommitBinding, CommitOperation, OwnerCommitRequest},
    transport::Retention,
};
use serde::{Deserialize, Serialize};

mod reservation;
#[cfg(test)]
mod tests;
pub(crate) use reservation::{preflight_storage, preflight_tree};

pub(super) const MAX_ENTRIES: usize = 64;
pub(super) const MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_VISIBLE_ENTRIES: usize = MAX_ENTRIES * 2;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredPublication {
    version: u32,
    revision: u64,
    next_ordinal: u64,
    #[serde(deserialize_with = "read_entries")]
    entries: Vec<StoredOutgoing>,
    #[serde(deserialize_with = "read_entries")]
    projection: Vec<StoredOutgoing>,
    reservation: reservation::StoredReservation,
    legacy: Option<StoredHistory>,
}
impl Default for StoredPublication {
    fn default() -> Self {
        Self {
            version: 2,
            revision: 1,
            next_ordinal: 1,
            entries: Vec::new(),
            projection: Vec::new(),
            reservation: Default::default(),
            legacy: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredOutgoing {
    ordinal: u64,
    id: [u8; 32],
    #[serde(deserialize_with = "read_message_bytes")]
    bytes: Vec<u8>,
    binding: CommitBinding,
    timestamp_ns: u64,
    channel: [u8; 32],
    incremental: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredHistory {
    #[serde(deserialize_with = "read_history_bytes")]
    peer: Vec<u8>,
    digest: [u8; 32],
}
pub(crate) struct NativeOutgoing<'a> {
    state: &'a StoredPublication,
    allowed: [bool; MAX_VISIBLE_ENTRIES],
}
pub(crate) struct NativeOutgoingEntry<'a>(&'a StoredOutgoing);
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativeOutgoingStatus {
    pub pending: usize,
    pub blocked: usize,
    pub legacy_history: bool,
}
impl<'a> NativeOutgoing<'a> {
    pub(crate) fn revision(&self) -> u64 {
        self.state.revision
    }
    pub(crate) fn entries(&self) -> impl Iterator<Item = NativeOutgoingEntry<'a>> {
        let allowed = self.allowed;
        self.state
            .entries
            .iter()
            .chain(self.state.projection.iter())
            .enumerate()
            .filter(move |(index, _)| allowed[*index])
            .map(|(_, entry)| NativeOutgoingEntry(entry))
    }
}
impl<'a> NativeOutgoingEntry<'a> {
    pub(crate) fn id(&self) -> [u8; 32] {
        self.0.id
    }
    pub(crate) fn ordinal(&self) -> u64 {
        self.0.ordinal
    }
    pub(crate) fn bytes(&self) -> &'a [u8] {
        &self.0.bytes
    }
    pub(crate) fn channel(&self) -> [u8; 32] {
        self.0.channel
    }
    pub(crate) fn retention(&self) -> Retention {
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
fn read_history_bytes<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Vec<u8>, D::Error> {
    read_bytes(input, store::MAX_AUTHORITY_BYTES)
}
fn fresh(entry: &StoredOutgoing, now: SystemTime) -> bool {
    let timestamp = SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(entry.timestamp_ns);
    let skew = std::time::Duration::from_secs(300);
    timestamp >= now.checked_sub(skew).unwrap_or(SystemTime::UNIX_EPOCH)
        && timestamp <= now.checked_add(skew).unwrap_or(now)
}
fn state(view: &NativeCommittedView) -> Result<&StoredPublication, String> {
    view.record
        .publication
        .as_ref()
        .ok_or_else(|| "native publication migration is not committed".into())
}
fn status(view: &NativeCommittedView, now: SystemTime) -> Result<NativeOutgoingStatus, String> {
    let publication = state(view)?;
    let allowed = allowed(view, now)?;
    Ok(NativeOutgoingStatus {
        pending: publication.entries.len() + publication.projection.len(),
        blocked: publication.entries.len() + publication.projection.len()
            - allowed.iter().filter(|allowed| **allowed).count(),
        legacy_history: publication.legacy.is_some(),
    })
}
fn allowed(
    view: &NativeCommittedView,
    now: SystemTime,
) -> Result<[bool; MAX_VISIBLE_ENTRIES], String> {
    let publication = state(view)?;
    bounds(publication)?;
    let mut allowed = [false; MAX_VISIBLE_ENTRIES];
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
        allowed[index] = envelope.author.as_bytes() == &view.device().device_public_key
            && view
                .peer
                .lookup_exact_local_document(&app_id, &record_id, &envelope, now)
                .ok()
                .flatten()
                .is_some();
    }
    if !publication.projection.is_empty() {
        let originals = decode_entries(&publication.entries)?;
        let legacy = publication
            .legacy
            .as_ref()
            .map(|stored| history(view, stored, now))
            .transpose()?;
        let projection = decode_entries(&publication.projection)?;
        if view
            .peer
            .authorize_local_recovery_projection(
                &originals,
                legacy.as_ref(),
                &projection,
                view.device().device_public_key,
                now,
            )
            .is_ok()
        {
            for (index, entry) in publication.projection.iter().enumerate() {
                allowed[publication.entries.len() + index] = entry.incremental && fresh(entry, now);
            }
        }
    }
    Ok(allowed)
}
pub(crate) fn validate_file_layout(
    version: u32,
    storage: &record::NativeAuthorityStorage,
) -> Result<(), String> {
    for room in storage.rooms.values() {
        match (version, room.version, room.publication.is_some()) {
            (2, 1, false) | (3, 1, false) | (3, 2, true) => {}
            _ => return Err("native publication record/file layout mismatch".into()),
        }
        if let Some(publication) = &room.publication {
            bounds(publication)?;
        }
    }
    Ok(())
}
fn bounds(publication: &StoredPublication) -> Result<usize, String> {
    if publication.version != 2
        || publication.revision == 0
        || publication.next_ordinal == 0
        || publication.entries.len() > MAX_ENTRIES
        || publication.projection.len() > MAX_ENTRIES
    {
        return Err("invalid native outgoing publication or entry capacity".into());
    }
    let bytes = publication
        .entries
        .iter()
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
fn request(message: &NeighbourMsg) -> Result<OwnerCommitRequest, String> {
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
fn metadata(message: &NeighbourMsg) -> Result<(u64, [u8; 32], bool), String> {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        return Err("native outgoing requires document".into());
    };
    let (channel, kind) = qos_peer_state::transport::retention(message, false)
        .ok_or("unsupported native outgoing document payload")?;
    Ok((
        enrollment::ns(envelope.timestamp)?,
        channel,
        kind == Retention::Delta,
    ))
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
pub(super) fn encode(message: &NeighbourMsg, limit: usize) -> Result<Vec<u8>, String> {
    postcard::serialize_with_flavor(message, Count { bytes: 0, limit })
        .map_err(|e| e.to_string())?;
    postcard::to_allocvec(message).map_err(|e| e.to_string())
}
pub(super) fn prepare(request: &OwnerCommitRequest) -> Result<Vec<u8>, String> {
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
    let peer = qos_peer_state::peer_handle(envelope.author.as_bytes());
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
pub(super) fn reserve(view: &NativeCommittedView, bytes: &[u8]) -> Result<(), String> {
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
pub(super) fn append_local(
    view: &mut NativeCommittedView,
    bytes: Vec<u8>,
    now: SystemTime,
) -> Result<(), String> {
    append(view, bytes, false)?;
    reservation::refresh(view, now)
}
pub(super) fn has_exact(view: &NativeCommittedView, bytes: &[u8]) -> Result<bool, String> {
    Ok(state(view)?
        .entries
        .iter()
        .any(|entry| entry.bytes == bytes))
}
fn history(
    view: &NativeCommittedView,
    stored: &StoredHistory,
    now: SystemTime,
) -> Result<StatePeer, String> {
    if stored.digest != *blake3::hash(&stored.peer).as_bytes() {
        return Err("native legacy history digest mismatch".into());
    }
    StatePeer::restore(
        &stored.peer,
        &view.record.room.to_string(),
        view.record.governor.clone(),
        vec![view.record.device.clone()],
        tilo_user_identity::RevocationList::new(),
        now,
    )
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
fn history_retired(peer: &StatePeer, history: &StatePeer) -> Result<bool, String> {
    peer.retained_local_history_retired(history)
}
fn decode_entries(entries: &[StoredOutgoing]) -> Result<Vec<NeighbourMsg>, String> {
    entries
        .iter()
        .map(|entry| postcard::from_bytes(&entry.bytes).map_err(|e| e.to_string()))
        .collect()
}

pub(super) fn validate(view: &NativeCommittedView, now: SystemTime) -> Result<(), String> {
    if view.record.version == 1 {
        return if view.record.publication.is_none() {
            Ok(())
        } else {
            Err("legacy room unexpectedly contains publication fields".into())
        };
    }
    let publication = state(view)?;
    bounds(publication)?;
    let mut ids = BTreeSet::new();
    let mut ordinals = BTreeSet::new();
    for entry in publication
        .entries
        .iter()
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
        if encode(&message, MAX_BYTES)? != entry.bytes
            || request(&message)?.binding()? != entry.binding
            || metadata(&message)? != (entry.timestamp_ns, entry.channel, entry.incremental)
        {
            return Err("native outgoing bytes/binding mismatch".into());
        }
        if publication
            .entries
            .iter()
            .any(|original| original.id == entry.id)
        {
            validate_exact(&view.peer, &message, view.device().device_public_key)?;
        }
    }
    for entries in [&publication.entries, &publication.projection] {
        if entries
            .windows(2)
            .any(|pair| pair[0].ordinal >= pair[1].ordinal)
        {
            return Err("native publication ordinal order mismatch".into());
        }
    }
    let originals = decode_entries(&publication.entries)?;
    let legacy = publication
        .legacy
        .as_ref()
        .map(|stored| history(view, stored, now))
        .transpose()?;
    if !publication.projection.is_empty() {
        view.peer.validate_local_recovery_projection(
            &originals,
            legacy.as_ref(),
            &decode_entries(&publication.projection)?,
            view.device().device_public_key,
        )?;
    }
    reservation::validate(view, &originals, legacy.as_ref())?;
    if let Some(stored) = &publication.legacy {
        let _ = history(view, stored, now)?;
    }
    Ok(())
}
pub(super) fn migrate(view: &mut NativeCommittedView) -> Result<bool, String> {
    if view.record.version != 1 {
        return Ok(false);
    }
    let covered = view.peer.rows().iter().try_fold(true, |covered, row| {
        Ok::<_, String>(
            covered
                && view
                    .peer
                    .durable_covers_current(&row.app_id, &row.record_id)?,
        )
    })?;
    let mut publication = StoredPublication::default();
    if !covered {
        publication.legacy = Some(StoredHistory {
            peer: view.record.peer.clone(),
            digest: *blake3::hash(&view.record.peer).as_bytes(),
        });
    }
    view.record.version = 2;
    view.record.publication = Some(publication);
    reservation::refresh(view, SystemTime::now())?;
    Ok(true)
}
pub(super) fn retire(view: &mut NativeCommittedView, now: SystemTime) -> Result<(), String> {
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
    let remove_history = publication
        .legacy
        .as_ref()
        .map(|stored| {
            history(view, stored, now)
                .and_then(|history| history_retired(&view.peer, &history))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    // A projection has no user receipt/exact index. Empty obligations prove its
    // whole history is in the actual durable governor receipt, independently of
    // whether this retransmission sequence itself was observed by the governor.
    let remove_projection = !publication.projection.is_empty()
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
    if !remove.is_empty() || remove_history || remove_projection {
        let publication = view
            .record
            .publication
            .as_mut()
            .ok_or("publication absent")?;
        publication
            .entries
            .retain(|entry| !remove.contains(&entry.id));
        if remove_history {
            publication.legacy = None;
        }
        if remove_projection {
            publication.projection.clear();
        }
        publication.revision = publication
            .revision
            .checked_add(1)
            .ok_or("native publication revision exhausted")?;
    }
    reservation::refresh(view, now)?;
    Ok(())
}
pub(super) fn retry_covered(view: &NativeCommittedView, bytes: &[u8]) -> Result<bool, String> {
    let message: NeighbourMsg = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
    retired(&view.peer, &message, view.device().device_public_key)
}
impl NativeWorkspaceOwner {
    pub(crate) fn with_outgoing<T>(
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
    pub(crate) fn outgoing_status(
        &self,
        room: RoomId,
        now: SystemTime,
    ) -> Result<NativeOutgoingStatus, String> {
        self.with_current(room, |view| status(view, now))
    }
    pub(crate) fn reconcile_outgoing(
        &self,
        room: RoomId,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NativeOutgoingStatus, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if key.verifying_key().to_bytes() != candidate.device().device_public_key {
            return Err("native outgoing signer differs from selected device".into());
        }
        candidate
            .peer
            .permissions(candidate.device().device_public_key, now)?;
        let before = state(&candidate)?.revision;
        retire(&mut candidate, now)?;
        let originals = decode_entries(&state(&candidate)?.entries)?;
        let legacy = state(&candidate)?
            .legacy
            .as_ref()
            .map(|stored| history(&candidate, stored, now))
            .transpose()?;
        let old = decode_entries(&state(&candidate)?.projection)?;
        let replacement = candidate.peer.reconcile_local_recovery_projection(
            &originals,
            legacy.as_ref(),
            &old,
            key,
            now,
        )?;
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
        reservation::refresh(&mut candidate, now)?;
        if before != state(&candidate)?.revision {
            candidate.record.peer = candidate.peer.durable()?;
            candidate.record.generation = candidate
                .generation()
                .checked_add(1)
                .ok_or("native owner generation exhausted")?;
            self.publish_room(room, candidate, None)?;
        }
        self.outgoing_status(room, now)
    }
}
