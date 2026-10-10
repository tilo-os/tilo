//! Presence slot coding for [`super::ALPN_PRESENCE_BATCH_V1`] (TIL-710).
//!
//! Successive presence messages in one slot — a pointer's moves, a caret's
//! positions — repeat almost all of their bytes: the author key, the user
//! and presence key, the colour. Each session remembers the last payload it
//! received in each of its slots, and the relay sends the next payload in that
//! slot as the bytes that changed between a kept prefix and a kept suffix.
//! [`PresenceEncoder`] is the relay's half and the session reader's
//! [`PresenceSlots`] the receiver's; both see every batch of one session in
//! order, so their tables never diverge.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tilo_relay_client::frame::FrameError;
use tilo_relay_protocol::transport::MAX_FRAME_BYTES;

use super::PRESENCE_BATCH_KIND;

/// Slots one session's presence coding remembers: the relay reuses the
/// least recently used one beyond this, and a reader refuses a higher index.
pub const MAX_PRESENCE_SLOTS: usize = 256;

/// Largest payload kept in a presence slot. A larger presence travels whole
/// and unslotted, so a session's slot table stays within
/// `MAX_PRESENCE_SLOTS * MAX_SLOTTED_PRESENCE_BYTES` (512 KiB).
pub const MAX_SLOTTED_PRESENCE_BYTES: usize = 2048;

/// One presence payload in a [`super::EnvelopeV2::PresenceBatch`].
///
/// A slotted item is the payload the session last received in `slot` with
/// its first `keep_prefix` and last `keep_suffix` bytes kept and `middle` in
/// between; the result becomes the slot's payload. An unslotted item is the
/// whole payload in `middle`, with both keeps zero, and is remembered nowhere.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceItem {
    pub slot: Option<u16>,
    pub keep_prefix: u32,
    pub keep_suffix: u32,
    #[serde(with = "serde_bytes")]
    pub middle: Vec<u8>,
}

/// Whose presence a slot holds on the relay: one author's slot in one room.
/// Only the encoder sees it; the wire carries the session's slot index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PresenceSlotKey {
    pub workspace_id: u64,
    pub author: [u8; 32],
    pub slot: u64,
}

/// The relay's half of one session's presence slots. It assigns each
/// [`PresenceSlotKey`] a session slot (reusing the least recently used one
/// past [`MAX_PRESENCE_SLOTS`]) and codes each payload against what the
/// session holds there. It belongs to exactly one session and must code
/// every item written to it, in the order written.
#[derive(Default)]
pub struct PresenceEncoder {
    index: HashMap<PresenceSlotKey, u16>,
    slots: Vec<EncodedSlot>,
    clock: u64,
}

struct EncodedSlot {
    key: PresenceSlotKey,
    /// What the session holds in this slot now.
    last: Arc<[u8]>,
    used: u64,
}

impl PresenceEncoder {
    /// Code `payload` for this session. `key` is `None` for presence that
    /// is not latest-state, which travels whole.
    pub fn item(&mut self, key: Option<PresenceSlotKey>, payload: &Arc<[u8]>) -> PresenceItem {
        let whole = || PresenceItem {
            slot: None,
            keep_prefix: 0,
            keep_suffix: 0,
            middle: payload.to_vec(),
        };
        let Some(key) = key.filter(|_| payload.len() <= MAX_SLOTTED_PRESENCE_BYTES) else {
            return whole();
        };
        self.clock = self.clock.saturating_add(1);
        let index = match self.index.get(&key) {
            Some(index) => usize::from(*index),
            None => self.assign(key),
        };
        let (Ok(slot), Some(entry)) = (u16::try_from(index), self.slots.get_mut(index)) else {
            return whole();
        };
        let (prefix, suffix) = common_ends(&entry.last, payload);
        let (Ok(keep_prefix), Ok(keep_suffix)) = (u32::try_from(prefix), u32::try_from(suffix))
        else {
            return whole();
        };
        let middle = payload
            .get(prefix..payload.len().saturating_sub(suffix))
            .unwrap_or_default()
            .to_vec();
        entry.last = payload.clone();
        entry.used = self.clock;
        PresenceItem {
            slot: Some(slot),
            keep_prefix,
            keep_suffix,
            middle,
        }
    }

    /// A session slot for `key`: a fresh one while fewer than
    /// [`MAX_PRESENCE_SLOTS`] exist, else the least recently used. A reused
    /// slot keeps its payload as the base, since the session still holds it.
    fn assign(&mut self, key: PresenceSlotKey) -> usize {
        let index = if self.slots.len() < MAX_PRESENCE_SLOTS {
            self.slots.push(EncodedSlot {
                key,
                last: Arc::from(Vec::new()),
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

/// The longest common prefix of `base` and `next`, then the longest common
/// suffix of what is left of both.
fn common_ends(base: &[u8], next: &[u8]) -> (usize, usize) {
    let prefix = base.iter().zip(next).take_while(|(a, b)| a == b).count();
    let room = base.len().min(next.len()).saturating_sub(prefix);
    let suffix = base
        .iter()
        .rev()
        .zip(next.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    (prefix, suffix)
}

/// The session reader's half: what it last received in each slot.
#[derive(Default)]
pub(super) struct PresenceSlots {
    slots: Vec<Vec<u8>>,
}

impl PresenceSlots {
    /// The payload `item` codes, remembered in its slot.
    pub(super) fn expand(&mut self, item: PresenceItem) -> Result<Vec<u8>, FrameError> {
        let bad = || FrameError::Decode(postcard::Error::DeserializeBadEncoding);
        let keep_prefix = usize::try_from(item.keep_prefix).map_err(|_| bad())?;
        let keep_suffix = usize::try_from(item.keep_suffix).map_err(|_| bad())?;
        let Some(slot) = item.slot else {
            if keep_prefix != 0 || keep_suffix != 0 {
                return Err(bad());
            }
            return Ok(item.middle);
        };
        let slot = usize::from(slot);
        if slot >= MAX_PRESENCE_SLOTS {
            return Err(bad());
        }
        if self.slots.len() <= slot {
            self.slots.resize_with(slot.saturating_add(1), Vec::new);
        }
        let base = self.slots.get_mut(slot).ok_or_else(bad)?;
        let kept = keep_prefix.checked_add(keep_suffix).ok_or_else(bad)?;
        let len = kept.checked_add(item.middle.len()).ok_or_else(bad)?;
        if kept > base.len() || len > MAX_SLOTTED_PRESENCE_BYTES {
            return Err(bad());
        }
        let mut payload = Vec::with_capacity(len);
        payload.extend_from_slice(base.get(..keep_prefix).ok_or_else(bad)?);
        payload.extend_from_slice(&item.middle);
        payload.extend_from_slice(
            base.get(base.len().saturating_sub(keep_suffix)..)
                .ok_or_else(bad)?,
        );
        base.clone_from(&payload);
        Ok(payload)
    }
}

/// Encode one [`super::EnvelopeV2::PresenceBatch`] frame, length prefix
/// included, without copying the items.
///
/// # Errors
///
/// Returns a postcard error if the batch exceeds the state frame bound.
pub fn encode_presence_batch(
    workspace_id: u64,
    items: &[PresenceItem],
) -> Result<Vec<u8>, postcard::Error> {
    #[derive(Serialize)]
    struct Fields<'a> {
        workspace_id: u64,
        items: &'a [PresenceItem],
    }
    // Room for the length prefix, written once the length is known.
    let mut out = vec![0, 0, 0, 0, PRESENCE_BATCH_KIND];
    out = postcard::to_extend(
        &Fields {
            workspace_id,
            items,
        },
        out,
    )?;
    let payload = out.len().saturating_sub(4);
    if payload > MAX_FRAME_BYTES + 1 {
        return Err(postcard::Error::SerializeBufferFull);
    }
    let len = u32::try_from(payload).map_err(|_| postcard::Error::SerializeBufferFull)?;
    if let Some(prefix) = out.get_mut(..4) {
        prefix.copy_from_slice(&len.to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(slot: u64) -> PresenceSlotKey {
        PresenceSlotKey {
            workspace_id: 1,
            author: [7; 32],
            slot,
        }
    }

    fn payload(bytes: &[u8]) -> Arc<[u8]> {
        Arc::from(bytes.to_vec())
    }

    #[test]
    fn a_slot_sends_only_what_changed_and_the_reader_rebuilds_it() {
        let mut encoder = PresenceEncoder::default();
        let mut reader = PresenceSlots::default();
        let first = payload(b"author|user|key|x=0100,y=0200|ts=1|teal");
        let second = payload(b"author|user|key|x=0103,y=0202|ts=2|teal");
        let item = encoder.item(Some(key(9)), &first);
        assert_eq!(item.middle.len(), first.len(), "nothing to keep yet");
        assert_eq!(reader.expand(item).unwrap(), first.to_vec());
        let item = encoder.item(Some(key(9)), &second);
        assert_eq!(item.slot, Some(0));
        assert!(
            item.middle.len() < 15,
            "{} changed bytes",
            item.middle.len()
        );
        assert_eq!(reader.expand(item).unwrap(), second.to_vec());
        // A shorter and a longer payload in the same slot.
        for next in [
            &b"author|x|teal"[..],
            b"author|user|key|x=1|y=2|z=3|ts=33|teal",
        ] {
            let item = encoder.item(Some(key(9)), &payload(next));
            assert_eq!(reader.expand(item).unwrap(), next.to_vec());
        }
        // Identical again: nothing in the middle.
        let same = payload(b"author|x|teal");
        let _ = reader.expand(encoder.item(Some(key(9)), &same)).unwrap();
        let item = encoder.item(Some(key(9)), &same);
        assert!(item.middle.is_empty());
        assert_eq!(reader.expand(item).unwrap(), same.to_vec());
    }

    #[test]
    fn unslotted_and_oversized_presence_travels_whole() {
        let mut encoder = PresenceEncoder::default();
        let mut reader = PresenceSlots::default();
        let event = payload(b"heartbeat");
        let item = encoder.item(None, &event);
        assert_eq!(
            (item.slot, item.keep_prefix, item.keep_suffix),
            (None, 0, 0)
        );
        assert_eq!(reader.expand(item).unwrap(), event.to_vec());
        let big = payload(&vec![3; MAX_SLOTTED_PRESENCE_BYTES + 1]);
        let item = encoder.item(Some(key(1)), &big);
        assert_eq!(item.slot, None);
        assert_eq!(reader.expand(item).unwrap(), big.to_vec());
    }

    #[test]
    fn slots_past_the_limit_reuse_the_least_recent_and_stay_in_step() {
        let mut encoder = PresenceEncoder::default();
        let mut reader = PresenceSlots::default();
        let total = u64::try_from(MAX_PRESENCE_SLOTS).unwrap() + 40;
        for round in 0..3u8 {
            for slot in 0..total {
                let body = payload(format!("slot {slot} round {round}").as_bytes());
                let item = encoder.item(Some(key(slot)), &body);
                assert!(usize::from(item.slot.unwrap()) < MAX_PRESENCE_SLOTS);
                assert_eq!(reader.expand(item).unwrap(), body.to_vec());
            }
        }
    }

    #[test]
    fn a_reader_refuses_items_its_slots_cannot_hold() {
        let item = |slot, keep_prefix, keep_suffix, middle: &[u8]| PresenceItem {
            slot,
            keep_prefix,
            keep_suffix,
            middle: middle.to_vec(),
        };
        let mut reader = PresenceSlots::default();
        let index = u16::try_from(MAX_PRESENCE_SLOTS).unwrap();
        assert!(reader.expand(item(Some(index), 0, 0, b"x")).is_err());
        assert!(
            reader.expand(item(Some(0), 1, 0, b"x")).is_err(),
            "nothing to keep"
        );
        assert!(reader.expand(item(None, 1, 0, b"x")).is_err());
        let too_big = vec![0; MAX_SLOTTED_PRESENCE_BYTES + 1];
        assert!(reader.expand(item(Some(0), 0, 0, &too_big)).is_err());
        assert!(reader.expand(item(Some(0), 0, 0, b"abc")).is_ok());
        assert!(
            reader.expand(item(Some(0), 2, 2, b"")).is_err(),
            "keeps overlap"
        );
        assert_eq!(reader.expand(item(Some(0), 1, 1, b"Z")).unwrap(), b"aZc");
    }
}
