//! Delta coding for [`super::ALPN_CODED_DELTA_V1`].
//!
//! Two keystrokes of one author repeat most of their frame byte for byte:
//! the workspace and author keys, the retention channel, the app, cluster
//! and record ids, the Automerge actor, the signed op's artifact, author and
//! attestation. Only the timestamps, the change's own bytes and the
//! signature differ. Each session remembers the last delta frame it received
//! from each author on each route in each room (a *slot*), and the relay
//! writes the next one as [`DeltaSegment`]s: ranges copied from that frame and the bytes in
//! between. The frame the session rebuilds is the
//! [`super::EnvelopeV2::UnsignedDelta`] payload it would have received
//! uncoded, byte for byte, so nothing past the session reader changes and
//! every signature inside it verifies as before.
//!
//! [`DeltaEncoder`] is the relay's half and the session reader's
//! [`DeltaSlots`] the receiver's. Both see every coded frame of one session
//! in order, so their tables never diverge; a frame that is not coded
//! touches no slot.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tilo_relay_client::frame::FrameError;
use tilo_relay_protocol::transport::Envelope;

use super::{CODED_DELTA_KIND, UNSIGNED_DELTA_KIND};

/// Slots one session's delta coding remembers: the relay reuses the least
/// recently used one beyond this, and a reader refuses a higher index.
pub const MAX_DELTA_SLOTS: usize = 64;

/// Largest delta frame that is coded and kept in a slot. A larger delta
/// travels uncoded, so a session's slot table stays within
/// `MAX_DELTA_SLOTS * MAX_CODED_DELTA_BYTES` (256 KiB).
pub const MAX_CODED_DELTA_BYTES: usize = 4096;

/// Shortest run worth a [`DeltaSegment::Copy`]: a copy costs about five
/// bytes on the wire, so a shorter one saves nothing over sending it.
const MIN_COPY: usize = 8;
/// Bytes hashed to find copy candidates.
const WINDOW: usize = 4;
/// Buckets of the match index: 1,024, for frames of a few hundred bytes.
const HASH_BITS: u32 = 10;
/// Candidates followed per position, newest first.
const MAX_CANDIDATES: usize = 8;
const NO_POSITION: u16 = u16::MAX;

/// One piece of a [`super::EnvelopeV2::CodedDelta`]: bytes of the slot's
/// previous frame, or bytes carried in the frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeltaSegment {
    /// `len` bytes of the slot's previous frame, from `offset`.
    Copy { offset: u32, len: u32 },
    /// Bytes the previous frame does not hold.
    Literal(#[serde(with = "serde_bytes")] Vec<u8>),
}

/// Bytes at the start of a delta that name its route: a co-edit payload's
/// kind and 32-byte retention channel. The encoder reads them as opaque
/// bytes; deltas that start alike share a slot.
pub const DELTA_ROUTE_BYTES: usize = 33;

/// Whose deltas a slot holds on the relay: one author's on one route in one
/// room. Only the encoder sees it; the wire carries the session's slot index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeltaSlotKey {
    pub workspace_id: u64,
    pub author: [u8; 32],
    /// The delta's first [`DELTA_ROUTE_BYTES`], zero-padded.
    pub route: [u8; DELTA_ROUTE_BYTES],
}

/// The relay's half of one session's delta slots. It assigns each
/// [`DeltaSlotKey`] a session slot (reusing the least recently used one past
/// [`MAX_DELTA_SLOTS`]) and codes each delta frame against what the session
/// holds there. It belongs to exactly one session and must code every
/// [`super::EnvelopeV2::CodedDelta`] written to it, in the order written.
#[derive(Default)]
pub struct DeltaEncoder {
    index: HashMap<DeltaSlotKey, u16>,
    slots: Vec<EncodedSlot>,
    clock: u64,
    /// The match index, rebuilt for each frame in buffers kept across frames.
    matches: MatchIndex,
}

struct EncodedSlot {
    key: DeltaSlotKey,
    /// What the session holds in this slot now.
    last: Vec<u8>,
    used: u64,
}

impl DeltaEncoder {
    /// The [`super::EnvelopeV2::CodedDelta`] frame, length prefix included,
    /// that carries `envelope` to this session; `None` when `envelope` is
    /// not a zero-signature delta of at most [`MAX_CODED_DELTA_BYTES`], which
    /// is then written as before and changes no slot.
    ///
    /// # Errors
    ///
    /// Returns a postcard error if the frame cannot be encoded.
    pub fn encode(&mut self, envelope: &Envelope) -> Result<Option<Vec<u8>>, postcard::Error> {
        let Envelope::Delta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            delta,
            signature,
        } = envelope
        else {
            return Ok(None);
        };
        if *signature != [0; 64] {
            return Ok(None);
        }
        let payload = super::unsigned_delta_payload(
            *workspace_id,
            author_device_public_key,
            *authored_at_ns,
            delta,
        )?;
        if payload.len() > MAX_CODED_DELTA_BYTES {
            return Ok(None);
        }
        let mut route = [0; DELTA_ROUTE_BYTES];
        for (to, from) in route.iter_mut().zip(delta) {
            *to = *from;
        }
        let key = DeltaSlotKey {
            workspace_id: *workspace_id,
            author: *author_device_public_key,
            route,
        };
        self.clock = self.clock.saturating_add(1);
        let index = match self.index.get(&key) {
            Some(index) => usize::from(*index),
            None => self.assign(key),
        };
        let (Ok(slot), Some(entry)) = (u16::try_from(index), self.slots.get_mut(index)) else {
            return Ok(None);
        };
        let frame = encode_coded_delta(slot, &self.matches.segments(&entry.last, &payload))?;
        entry.last = payload;
        entry.used = self.clock;
        Ok(Some(frame))
    }

    /// A session slot for `key`: a fresh one while fewer than
    /// [`MAX_DELTA_SLOTS`] exist, else the least recently used. A reused
    /// slot keeps its frame as the base, since the session still holds it.
    fn assign(&mut self, key: DeltaSlotKey) -> usize {
        let index = if self.slots.len() < MAX_DELTA_SLOTS {
            self.slots.push(EncodedSlot {
                key,
                last: Vec::new(),
                used: self.clock,
            });
            self.slots.len().saturating_sub(1)
        } else {
            let index = self
                .slots
                .iter()
                .enumerate()
                .min_by_key(|(_, slot)| slot.used)
                .map_or(0, |(index, _)| index);
            if let Some(slot) = self.slots.get_mut(index) {
                self.index.remove(&slot.key);
                slot.key = key;
            }
            index
        };
        if let Ok(short) = u16::try_from(index) {
            self.index.insert(key, short);
        }
        index
    }
}

fn hash(window: &[u8]) -> usize {
    let mut bytes = [0; WINDOW];
    bytes.copy_from_slice(window.get(..WINDOW).unwrap_or(&[0; WINDOW]));
    let mixed = u32::from_le_bytes(bytes).wrapping_mul(0x9E37_79B1);
    usize::try_from(mixed >> (32 - HASH_BITS)).unwrap_or(0)
}

/// Where each [`WINDOW`]-byte sequence occurs in a base frame: the newest
/// position per hash bucket, and per position the next older one in its
/// bucket. Positions fit in a `u16`, since a base is at most
/// [`MAX_CODED_DELTA_BYTES`].
#[derive(Default)]
struct MatchIndex {
    head: Vec<u16>,
    chain: Vec<u16>,
}

impl MatchIndex {
    /// `next` as copies from `base` and literals: greedy, the longest run of
    /// at least [`MIN_COPY`] bytes at each position, among the newest
    /// [`MAX_CANDIDATES`] places in `base` with the same leading bytes.
    fn segments(&mut self, base: &[u8], next: &[u8]) -> Vec<DeltaSegment> {
        self.head.clear();
        self.head.resize(1 << HASH_BITS, NO_POSITION);
        self.chain.clear();
        self.chain.resize(base.len(), NO_POSITION);
        for (position, window) in base.windows(WINDOW).enumerate() {
            let bucket = hash(window);
            if let (Some(slot), Some(link), Ok(short)) = (
                self.head.get_mut(bucket),
                self.chain.get_mut(position),
                u16::try_from(position),
            ) {
                *link = *slot;
                *slot = short;
            }
        }
        segments(base, next, &self.head, &self.chain)
    }
}

/// [`MatchIndex::segments`] against the index `head` and `chain` of `base`.
fn segments(base: &[u8], next: &[u8], head: &[u16], chain: &[u16]) -> Vec<DeltaSegment> {
    let mut out = Vec::new();
    let mut literal = Vec::new();
    let mut at = 0;
    while at < next.len() {
        let rest = next.get(at..).unwrap_or_default();
        let (offset, len) = longest_match(base, rest, head, chain);
        if len >= MIN_COPY {
            if !literal.is_empty() {
                out.push(DeltaSegment::Literal(std::mem::take(&mut literal)));
            }
            if let (Ok(offset), Ok(len32)) = (u32::try_from(offset), u32::try_from(len)) {
                out.push(DeltaSegment::Copy { offset, len: len32 });
            }
            at = at.saturating_add(len);
        } else {
            if let Some(byte) = next.get(at) {
                literal.push(*byte);
            }
            at = at.saturating_add(1);
        }
    }
    if !literal.is_empty() {
        out.push(DeltaSegment::Literal(literal));
    }
    out
}

/// The longest prefix of `rest` found in `base`: its offset there and its
/// length, `(0, 0)` when none shares the first [`WINDOW`] bytes.
fn longest_match(base: &[u8], rest: &[u8], head: &[u16], chain: &[u16]) -> (usize, usize) {
    let Some(window) = rest.get(..WINDOW) else {
        return (0, 0);
    };
    let mut best = (0, 0);
    let mut candidate = head.get(hash(window)).copied().unwrap_or(NO_POSITION);
    for _ in 0..MAX_CANDIDATES {
        if candidate == NO_POSITION {
            break;
        }
        let offset = usize::from(candidate);
        let len = base
            .get(offset..)
            .unwrap_or_default()
            .iter()
            .zip(rest)
            .take_while(|(a, b)| a == b)
            .count();
        if len > best.1 {
            best = (offset, len);
        }
        candidate = chain.get(offset).copied().unwrap_or(NO_POSITION);
    }
    best
}

/// Encode one [`super::EnvelopeV2::CodedDelta`] frame, length prefix
/// included, without copying the segments.
///
/// # Errors
///
/// Returns a postcard error if the frame cannot be encoded.
pub fn encode_coded_delta(
    slot: u16,
    segments: &[DeltaSegment],
) -> Result<Vec<u8>, postcard::Error> {
    #[derive(Serialize)]
    struct Fields<'a> {
        slot: u16,
        segments: &'a [DeltaSegment],
    }
    // Room for the length prefix, written once the length is known.
    let mut out = vec![0, 0, 0, 0, CODED_DELTA_KIND];
    out = postcard::to_extend(&Fields { slot, segments }, out)?;
    let payload = out.len().saturating_sub(4);
    let len = u32::try_from(payload).map_err(|_| postcard::Error::SerializeBufferFull)?;
    if let Some(prefix) = out.get_mut(..4) {
        prefix.copy_from_slice(&len.to_le_bytes());
    }
    Ok(out)
}

/// The session reader's half: the last delta frame it received in each slot.
#[derive(Default)]
pub(super) struct DeltaSlots {
    slots: Vec<Vec<u8>>,
}

impl DeltaSlots {
    /// The [`super::EnvelopeV2::UnsignedDelta`] payload `segments` code
    /// against `slot`, remembered there. Refuses a slot past
    /// [`MAX_DELTA_SLOTS`], a copy outside the slot's frame, a frame larger
    /// than [`MAX_CODED_DELTA_BYTES`] and anything but an unsigned delta.
    pub(super) fn expand(
        &mut self,
        slot: u16,
        segments: &[DeltaSegment],
    ) -> Result<Vec<u8>, FrameError> {
        let bad = || FrameError::Decode(postcard::Error::DeserializeBadEncoding);
        let slot = usize::from(slot);
        if slot >= MAX_DELTA_SLOTS {
            return Err(bad());
        }
        if self.slots.len() <= slot {
            self.slots.resize_with(slot.saturating_add(1), Vec::new);
        }
        let base = self.slots.get(slot).ok_or_else(bad)?;
        let mut payload = Vec::new();
        for segment in segments {
            let piece = match segment {
                DeltaSegment::Copy { offset, len } => {
                    let start = usize::try_from(*offset).map_err(|_| bad())?;
                    let len = usize::try_from(*len).map_err(|_| bad())?;
                    let end = start.checked_add(len).ok_or_else(bad)?;
                    base.get(start..end).ok_or_else(bad)?
                }
                DeltaSegment::Literal(bytes) => bytes.as_slice(),
            };
            if payload.len().saturating_add(piece.len()) > MAX_CODED_DELTA_BYTES {
                return Err(bad());
            }
            payload.extend_from_slice(piece);
        }
        if payload.first() != Some(&UNSIGNED_DELTA_KIND) {
            return Err(bad());
        }
        if let Some(held) = self.slots.get_mut(slot) {
            held.clone_from(&payload);
        }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A keystroke's delta as its author publishes it: a fixed head (the
    /// channel and routing ids), the change, and a fixed tail around a
    /// fresh signature.
    fn keystroke(author: u8, n: u8) -> Envelope {
        routed_keystroke(author, 0x11, n)
    }

    fn routed_keystroke(author: u8, channel: u8, n: u8) -> Envelope {
        let mut delta = vec![1];
        delta.extend_from_slice(&[channel; 32]);
        delta.extend_from_slice(
            b"so.tilo.word-processor 5e11f00d-7110-4625-8000-000000000001 tab:1",
        );
        delta.extend_from_slice(&[author; 32]);
        delta.extend_from_slice(&[n, n.wrapping_mul(3), 0x7f, n.wrapping_add(9)]);
        delta.extend_from_slice(&[0x55; 70]);
        delta.extend_from_slice(&[n.wrapping_mul(7); 64]);
        delta.extend_from_slice(&[0x22; 40]);
        Envelope::Delta {
            workspace_id: 755,
            author_device_public_key: [author; 32],
            authored_at_ns: 1_800_000_000_000_000_000 + u64::from(n),
            delta,
            signature: [0; 64],
        }
    }

    fn plain_payload(envelope: &Envelope) -> Vec<u8> {
        super::super::encode_state_framed(envelope, super::super::StateFraming::Compact)
            .unwrap()
            .split_off(4)
    }

    fn coded(frame: &[u8]) -> (u16, Vec<DeltaSegment>) {
        match super::super::decode(frame.get(4..).unwrap()).unwrap() {
            super::super::EnvelopeV2::CodedDelta { slot, segments } => (slot, segments),
            other => panic!("not a coded delta: {other:?}"),
        }
    }

    #[test]
    fn a_second_keystroke_travels_as_what_changed_and_rebuilds_byte_for_byte() {
        let mut encoder = DeltaEncoder::default();
        let mut reader = DeltaSlots::default();
        let first = keystroke(1, 1);
        let frame = encoder.encode(&first).unwrap().unwrap();
        let plain = plain_payload(&first);
        assert!(
            frame.len() <= plain.len() + 4 + 8,
            "a first frame costs a few bytes over plain: {} vs {}",
            frame.len(),
            plain.len() + 4
        );
        let (slot, segments) = coded(&frame);
        assert_eq!(reader.expand(slot, &segments).unwrap(), plain);
        for n in 2..10 {
            let next = keystroke(1, n);
            let frame = encoder.encode(&next).unwrap().unwrap();
            let plain = plain_payload(&next);
            assert!(
                frame.len() * 2 <= plain.len(),
                "keystroke {n}: {} coded vs {} plain",
                frame.len(),
                plain.len()
            );
            let (slot, segments) = coded(&frame);
            assert_eq!(reader.expand(slot, &segments).unwrap(), plain);
        }
    }

    #[test]
    fn authors_and_rooms_take_their_own_slots() {
        let mut encoder = DeltaEncoder::default();
        let mut reader = DeltaSlots::default();
        let mut seen = std::collections::HashSet::new();
        for n in 0..6 {
            for author in [1, 2, 3] {
                let envelope = keystroke(author, n);
                let (slot, segments) = coded(&encoder.encode(&envelope).unwrap().unwrap());
                seen.insert(slot);
                assert_eq!(
                    reader.expand(slot, &segments).unwrap(),
                    plain_payload(&envelope)
                );
            }
        }
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn each_route_of_an_author_keeps_its_own_base() {
        let mut encoder = DeltaEncoder::default();
        let mut reader = DeltaSlots::default();
        let mut slots = std::collections::HashSet::new();
        for n in 0..8 {
            for channel in [0x11, 0x22, 0x33, 0x44] {
                let envelope = routed_keystroke(1, channel, n);
                let frame = encoder.encode(&envelope).unwrap().unwrap();
                let plain = plain_payload(&envelope);
                if n > 0 {
                    assert!(
                        frame.len() * 2 <= plain.len(),
                        "{} vs {}",
                        frame.len(),
                        plain.len()
                    );
                }
                let (slot, segments) = coded(&frame);
                slots.insert(slot);
                assert_eq!(reader.expand(slot, &segments).unwrap(), plain);
            }
        }
        assert_eq!(slots.len(), 4, "one slot per route");
    }

    #[test]
    fn slots_past_the_limit_reuse_the_least_recent_and_stay_in_step() {
        let mut encoder = DeltaEncoder::default();
        let mut reader = DeltaSlots::default();
        let authors = u8::try_from(MAX_DELTA_SLOTS + 20).unwrap();
        for round in 0..3 {
            for author in 0..authors {
                let envelope = keystroke(author, round);
                let (slot, segments) = coded(&encoder.encode(&envelope).unwrap().unwrap());
                assert!(usize::from(slot) < MAX_DELTA_SLOTS);
                assert_eq!(
                    reader.expand(slot, &segments).unwrap(),
                    plain_payload(&envelope)
                );
            }
        }
    }

    #[test]
    fn only_small_unsigned_deltas_are_coded() {
        let mut encoder = DeltaEncoder::default();
        assert_eq!(encoder.encode(&Envelope::Ping).unwrap(), None);
        let Envelope::Delta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            delta,
            ..
        } = keystroke(1, 1)
        else {
            unreachable!()
        };
        let signed = Envelope::Delta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            delta,
            signature: [9; 64],
        };
        assert_eq!(encoder.encode(&signed).unwrap(), None);
        let large = Envelope::Delta {
            workspace_id: 1,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            delta: vec![3; MAX_CODED_DELTA_BYTES],
            signature: [0; 64],
        };
        assert_eq!(encoder.encode(&large).unwrap(), None);
        assert!(encoder.slots.is_empty(), "an uncoded frame takes no slot");
    }

    #[test]
    fn a_reader_refuses_segments_its_slots_cannot_hold() {
        let literal = |bytes: &[u8]| DeltaSegment::Literal(bytes.to_vec());
        let mut reader = DeltaSlots::default();
        let index = u16::try_from(MAX_DELTA_SLOTS).unwrap();
        assert!(
            reader
                .expand(index, &[literal(&[UNSIGNED_DELTA_KIND])])
                .is_err()
        );
        assert!(
            reader
                .expand(0, &[DeltaSegment::Copy { offset: 0, len: 1 }])
                .is_err(),
            "nothing to copy yet"
        );
        assert!(
            reader.expand(0, &[literal(&[0, 6])]).is_err(),
            "only an unsigned delta is coded"
        );
        let too_big = vec![UNSIGNED_DELTA_KIND; MAX_CODED_DELTA_BYTES + 1];
        assert!(reader.expand(0, &[literal(&too_big)]).is_err());
        let base = [UNSIGNED_DELTA_KIND, 1, 2, 3];
        assert_eq!(reader.expand(0, &[literal(&base)]).unwrap(), base);
        assert!(
            reader
                .expand(0, &[DeltaSegment::Copy { offset: 2, len: 3 }])
                .is_err(),
            "a copy past the slot's frame"
        );
        assert!(
            reader
                .expand(
                    0,
                    &[DeltaSegment::Copy {
                        offset: u32::MAX,
                        len: u32::MAX
                    }]
                )
                .is_err()
        );
        assert_eq!(
            reader
                .expand(
                    0,
                    &[
                        DeltaSegment::Copy { offset: 0, len: 2 },
                        literal(&[9]),
                        DeltaSegment::Copy { offset: 3, len: 1 },
                    ]
                )
                .unwrap(),
            [UNSIGNED_DELTA_KIND, 1, 9, 3]
        );
        // Repeated copies cannot grow a frame past the bound either.
        let base = vec![UNSIGNED_DELTA_KIND; MAX_CODED_DELTA_BYTES];
        assert!(reader.expand(1, &[literal(&base)]).is_ok());
        let len = u32::try_from(MAX_CODED_DELTA_BYTES).unwrap();
        assert!(
            reader
                .expand(
                    1,
                    &[
                        DeltaSegment::Copy { offset: 0, len },
                        DeltaSegment::Copy { offset: 0, len: 1 },
                    ]
                )
                .is_err()
        );
    }
}
