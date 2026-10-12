//! Count the actual future native representation before an original gets a receipt.
use super::{
    CommitBinding, Deserialize, MAX_BYTES, MAX_ENTRIES, NativeCommittedView, NeighbourMsg,
    Serialize, decode_entries, state,
};
use crate::{limits as store, record};
use serde::ser::{SerializeSeq, SerializeStruct};
use std::io::{self, Write};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredReservation {
    entries: usize,
    wire_bytes: usize,
    authority_bytes: usize,
    state_bytes: usize,
}
impl StoredReservation {
    pub(super) fn has_recovery(&self) -> bool {
        self.entries > 0
    }

    pub(crate) fn bounds(&self) -> Result<(), String> {
        if self.entries > MAX_ENTRIES
            || self.wire_bytes > MAX_BYTES
            || self.authority_bytes > store::MAX_AUTHORITY_BYTES
            || self.state_bytes > store::MAX_STATE_BYTES
        {
            return Err("native recovery reservation exceeds capacity".into());
        }
        Ok(())
    }
}

/// Serialize each worst-width byte without allocating a recovery payload.
struct VirtualBytes(usize);
impl Serialize for VirtualBytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0))?;
        for _ in 0..self.0 {
            seq.serialize_element(&u8::MAX)?;
        }
        seq.end()
    }
}
struct VirtualEntry<'a> {
    room: &'a str,
    app: &'a str,
    record: &'a str,
    bytes: usize,
}
impl Serialize for VirtualEntry<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let binding = CommitBinding {
            room: self.room.into(),
            target: tilo_peer_state::owner_commit::CommitTarget::Document {
                app: self.app.into(),
                record: self.record.into(),
            },
            request_digest: [u8::MAX; 32],
            basis: [u8::MAX; 32],
        };
        let mut value = serializer.serialize_struct("StoredOutgoing", 7)?;
        value.serialize_field("ordinal", &u64::MAX)?;
        value.serialize_field("id", &[u8::MAX; 32])?;
        value.serialize_field("bytes", &VirtualBytes(self.bytes))?;
        value.serialize_field("binding", &binding)?;
        value.serialize_field("timestamp_ns", &u64::MAX)?;
        value.serialize_field("channel", &[u8::MAX; 32])?;
        value.serialize_field("incremental", &true)?;
        value.end()
    }
}
struct VirtualProjection<'a> {
    room: &'a str,
    proof: &'a tilo_peer_state::LocalRecoveryReservation,
}
impl Serialize for VirtualProjection<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.proof.entries()))?;
        for (app, record, bytes) in self.proof.routes() {
            seq.serialize_element(&VirtualEntry {
                room: self.room,
                app,
                record,
                bytes,
            })?;
        }
        seq.end()
    }
}
/// A pretty-printed subtree has exactly `2 * depth` more spaces per structural
/// newline when embedded in the authority/file. Escaped string newlines are not
/// literal newlines in `serde_json` output, so they cannot falsify this accounting.
struct Counter {
    bytes: usize,
    depth: usize,
    limit: usize,
}
impl Write for Counter {
    #[allow(
        clippy::naive_bytecount,
        reason = "serializer chunks are small; not worth a bytecount dependency"
    )]
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
        let indentation = self
            .depth
            .checked_mul(2)
            .and_then(|per_line| newlines.checked_mul(per_line))
            .ok_or_else(|| io::Error::other("native reserve overflow"))?;
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .and_then(|n| n.checked_add(indentation))
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| io::Error::other("native encoded recovery reserve exceeds capacity"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn count(value: &impl Serialize, depth: usize, limit: usize) -> Result<usize, String> {
    let mut writer = Counter {
        bytes: 0,
        depth,
        limit,
    };
    serde_json::to_writer_pretty(&mut writer, value).map_err(|e| e.to_string())?;
    Ok(writer.bytes)
}
fn proof(
    view: &NativeCommittedView,
    originals: &[NeighbourMsg],
) -> Result<StoredReservation, String> {
    if originals.is_empty() {
        return Ok(StoredReservation::default());
    }
    let proof =
        view.peer
            .reserve_local_recovery(originals, None, view.device().device_public_key)?;
    if proof.entries() > MAX_ENTRIES || proof.max_encoded_bytes() > MAX_BYTES {
        return Err("native outgoing cannot reserve complete causal recovery".into());
    }
    let room = view.record.room.to_string();
    let virtual_projection = VirtualProjection {
        room: &room,
        proof: &proof,
    };
    // NativeAuthorityStorage.rooms[room].publication.projection has depth four;
    // state.json adds the surrounding WorkspaceTree object (depth five).
    // The peer's serialized reserved channel floor is separately charged below.
    // In a nonempty pretty JSON byte array, adding k binary bytes changes
    // punctuation/indentation by (4 + 2*depth)*k, plus exact decimal digit growth.
    // qps counts only the persisted channel-map fields changed by closed signing.
    let peer_extra = |depth: usize| -> Result<usize, String> {
        depth
            .checked_mul(2)
            .and_then(|indent| indent.checked_add(4))
            .and_then(|factor| proof.max_peer_growth_bytes().checked_mul(factor))
            .and_then(|size| size.checked_add(proof.max_peer_decimal_growth()))
            .ok_or_else(|| "native peer floor reserve overflow".into())
    };
    let authority_bytes = count(&virtual_projection, 4, store::MAX_AUTHORITY_BYTES)?
        .checked_add(peer_extra(3)?)
        .ok_or("native authority reserve overflow")?;
    let state_bytes = count(&virtual_projection, 5, store::MAX_STATE_BYTES)?
        .checked_add(peer_extra(4)?)
        .ok_or("native state reserve overflow")?;
    let result = StoredReservation {
        entries: proof.entries(),
        wire_bytes: proof.max_encoded_bytes(),
        authority_bytes,
        state_bytes,
    };
    result.bounds()?;
    Ok(result)
}
pub(crate) fn refresh(view: &mut NativeCommittedView) -> Result<(), String> {
    let publication = state(view)?;
    let originals = decode_entries(&publication.entries)?;
    let next = proof(view, &originals)?;
    let publication = view
        .record
        .publication
        .as_mut()
        .ok_or("publication absent")?;
    if publication.reservation != next {
        publication.reservation = next;
        publication.revision = publication
            .revision
            .checked_add(1)
            .ok_or("native reservation revision exhausted")?;
    }
    Ok(())
}
pub(crate) fn validate(
    view: &NativeCommittedView,
    originals: &[NeighbourMsg],
) -> Result<(), String> {
    if state(view)?.reservation != proof(view, originals)? {
        return Err("native recovery reservation differs from actual causal obligations".into());
    }
    Ok(())
}
fn digits(value: u64) -> usize {
    value.to_string().len()
}
fn extra(storage: &record::NativeAuthorityStorage, file: bool) -> Result<usize, String> {
    let mut total = 0usize;
    let mut active = false;
    for room in storage.rooms.values() {
        let Some(publication) = &room.publication else {
            continue;
        };
        publication.reservation.bounds()?;
        if publication.reservation.entries == 0 {
            continue;
        }
        active = true;
        let reserved = if file {
            publication.reservation.state_bytes
        } else {
            publication.reservation.authority_bytes
        };
        let actual = count(
            &publication.projection,
            if file { 5 } else { 4 },
            store::MAX_STATE_BYTES,
        )?;
        // A u64 has at most 20 digits and `bounds` keeps both reserves within
        // eight digits, so each slack term is exact and the sum is tiny.
        let numeric = [
            20_usize.saturating_sub(digits(publication.next_ordinal)),
            20_usize.saturating_sub(digits(publication.revision)),
            20_usize.saturating_sub(digits(room.generation)),
            8_usize.saturating_sub(digits(publication.reservation.authority_bytes as u64)),
            8_usize.saturating_sub(digits(publication.reservation.state_bytes as u64)),
        ]
        .into_iter()
        .fold(0_usize, usize::saturating_add);
        total = total
            .checked_add(reserved.saturating_sub(actual))
            .and_then(|sum| sum.checked_add(numeric))
            .ok_or("native aggregate recovery reserve overflow")?;
    }
    if active {
        total = total
            .checked_add(20_usize.saturating_sub(digits(storage.revision)))
            .ok_or("native storage revision reserve overflow")?;
    }
    Ok(total)
}

/// Refuse a record that leaves no room for the recovery its rooms reserved.
///
/// # Errors
/// `storage` does not fit its cap together with its reserve.
pub fn preflight_storage(storage: &record::NativeAuthorityStorage) -> Result<(), String> {
    let limit = store::MAX_AUTHORITY_BYTES
        .checked_sub(extra(storage, false)?)
        .ok_or("native authority recovery reserve exceeds capacity")?;
    store::preflight(storage, limit).map_err(|e| e.to_string())
}
/// Bytes a host file that embeds `storage` one level deeper than the record
/// itself (the desktop session's state file) must keep free for the
/// recovery its rooms reserved.
///
/// # Errors
/// A room's reserve is out of bounds.
pub fn reserved_file_bytes(storage: &record::NativeAuthorityStorage) -> Result<usize, String> {
    extra(storage, true)
}
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn check_actual_projection_bound(view: &NativeCommittedView) -> Result<(), String> {
    let state = state(view)?;
    let projection = count(&state.projection, 5, store::MAX_STATE_BYTES)?;
    if projection > state.reservation.state_bytes {
        return Err("actual projection exceeded admitted encoded reserve".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Serialize)]
    struct Entry {
        data: Vec<u8>,
        route: String,
    }
    #[test]
    #[allow(
        clippy::naive_bytecount,
        reason = "an obviously correct newline count is the oracle here; a byte-count crate \
                  would be a dependency for one test"
    )]
    fn native_outgoing_reserve_counter_matches_exact_pretty_embedding() {
        let value = vec![Entry {
            data: vec![0, 9, 10, 99, 100, 255],
            route: "quote\"\nbackslash\\".into(),
        }];
        let direct = serde_json::to_vec_pretty(&value).unwrap();
        for depth in [0, 3, 4, 5] {
            let embedded =
                direct.len() + direct.iter().filter(|byte| **byte == b'\n').count() * depth * 2;
            assert_eq!(count(&value, depth, usize::MAX).unwrap(), embedded);
            assert!(count(&value, depth, embedded - 1).is_err());
        }
    }
}
