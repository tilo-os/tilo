//! Explicit limits for decoding peer wire messages.
//!
//! These bound the outer serialization only. Automerge bytes carried inside a
//! message are admitted separately, through `tilo_automerge_isolate`.

mod guard;
mod join;
pub use join::BoundedJoinResponse;
pub(crate) mod json;
pub(crate) mod native;
pub(crate) mod outer;
pub(crate) use outer::decode;

/// Refusal limits for one decoded outer message.
#[derive(Clone, Copy, Debug)]
pub struct OuterLimits {
    pub allocation_bytes: usize,
    pub collection_entries: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerResourceError {
    Capacity,
    Malformed,
}

/// A peer message decoded within [`OuterLimits`]. Opaque document bytes inside
/// it have not been admitted.
pub struct BoundedPeerMessage {
    message: crate::NeighbourMsg,
}

impl BoundedPeerMessage {
    /// # Errors
    ///
    /// [`PeerResourceError::Capacity`] if the limits are exceeded and
    /// [`PeerResourceError::Malformed`] if the bytes do not decode.
    pub fn decode(bytes: &[u8], limits: OuterLimits) -> Result<Self, PeerResourceError> {
        Ok(Self {
            message: decode(bytes, limits)?,
        })
    }
    pub fn message(&self) -> &crate::NeighbourMsg {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeighbourMsg;

    #[test]
    fn outer_limits_refuse_oversized_messages_and_keep_valid_ones() {
        let message = NeighbourMsg::SyncResp {
            changes: vec![vec![1; 128]],
            room: "room".into(),
        };
        let bytes = postcard::to_allocvec(&message).unwrap();
        let roomy = OuterLimits {
            allocation_bytes: 48 * 1024,
            collection_entries: 4096,
        };
        assert_eq!(
            BoundedPeerMessage::decode(&bytes, roomy).unwrap().message(),
            &message
        );
        let tight = OuterLimits {
            allocation_bytes: 64,
            ..roomy
        };
        assert!(matches!(
            BoundedPeerMessage::decode(&bytes, tight),
            Err(PeerResourceError::Capacity)
        ));
        assert!(matches!(
            BoundedPeerMessage::decode(&[255], roomy),
            Err(PeerResourceError::Malformed)
        ));
    }
}
