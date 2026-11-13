//! Presence batches for [`super::EnvelopeV2::PresenceBatch`].
//!
//! A session reads a batch's items in order and each item is a whole payload.
//! Items still decode the slot coding an older relay wrote — a payload with
//! its first and last bytes kept from the one before it in the same slot — so
//! a session reads what a peer running an older build sends.

use serde::{Deserialize, Serialize};
use tilo_relay_client::frame::FrameError;
use tilo_relay_protocol::transport::MAX_FRAME_BYTES;

use super::PRESENCE_BATCH_KIND;
use collab_types::WorkspaceId;

/// Slots a session's presence reader remembers what an older relay coded
/// against; a higher slot index is refused.
pub const MAX_PRESENCE_SLOTS: usize = 256;

/// Largest payload an older relay kept in a presence slot; a coded item can
/// never rebuild a larger one.
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
pub fn encode_presence_batch(
    workspace_id: WorkspaceId,
    items: &[PresenceItem],
) -> Result<Vec<u8>, postcard::Error> {
    #[derive(Serialize)]
    struct Fields<'a> {
        workspace_id: WorkspaceId,
        items: &'a [PresenceItem],
    }
    // Workspace for the length prefix, written once the length is known.
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

    fn item(slot: Option<u16>, keep_prefix: u32, keep_suffix: u32, middle: &[u8]) -> PresenceItem {
        PresenceItem {
            slot,
            keep_prefix,
            keep_suffix,
            middle: middle.to_vec(),
        }
    }

    /// An older relay sent a slot's payload as the bytes that changed: the
    /// reader rebuilds it from what it holds in that slot.
    #[test]
    fn a_coded_item_rebuilds_the_payload_it_names() {
        let mut reader = PresenceSlots::default();
        assert_eq!(
            reader.expand(item(Some(0), 0, 0, b"author|x=01")).unwrap(),
            b"author|x=01"
        );
        assert_eq!(
            reader.expand(item(Some(0), 10, 0, b"7")).unwrap(),
            b"author|x=07"
        );
        assert_eq!(
            reader.expand(item(Some(0), 6, 2, b"y=")).unwrap(),
            b"authory=07"
        );
    }

    /// A whole payload is read as it stands, with no slot remembered.
    #[test]
    fn a_whole_payload_reads_as_itself() {
        let mut reader = PresenceSlots::default();
        assert_eq!(
            reader.expand(item(None, 0, 0, b"heartbeat")).unwrap(),
            b"heartbeat"
        );
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
