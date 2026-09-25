//! Explicit resource boundaries for native peer state.
//!
//! Outer serialization limits do not qualify native CRDT allocation or confer
//! readiness. Native reconstruction requires its separate checked witness.

mod encoding;
mod join;
pub use join::BoundedJoinResponse;
pub(crate) mod json;
pub(crate) mod native;
pub(crate) mod outer;
pub use encoding::BoundedSerialized;
pub(crate) use encoding::encode_known;

use automerge::resource_limits::NativeReservation;
pub use automerge::resource_limits::{NativeAllowance, NativeResourceError};

/// Conservative outer DTO refusal limits, separate from native CRDT limits.
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

/// A decoded outer peer message whose reserved allocation remains charged.
/// Opaque native document bytes have not been inspected or admitted. This type
/// confers neither native reconstruction qualification nor delivery readiness.
pub struct BoundedPeerMessage {
    held: Held<crate::NeighbourMsg>,
}

impl BoundedPeerMessage {
    /// The allowance must subdivide credit already held by the calling owner.
    /// The full conservative cap remains reserved until this value is dropped.
    pub fn decode(
        bytes: &[u8],
        limits: OuterLimits,
        allowance: &NativeAllowance,
    ) -> Result<Self, PeerResourceError> {
        Ok(Self {
            held: Held::decode(bytes, limits, allowance)?,
        })
    }
    pub fn message(&self) -> &crate::NeighbourMsg {
        &self.held.value
    }
}

/// Field order drops backing before releasing its reservation. There is no
/// public extraction method that could discard credit while retaining value.
pub(crate) struct Held<T> {
    pub(crate) value: T,
    _reservation: NativeReservation,
}
impl<T> Held<T> {
    pub(crate) fn decode_prefix_in_turn<'de>(
        bytes: &'de [u8],
        limits: OuterLimits,
        turn: &automerge::resource_limits::NativeWorkTurn,
    ) -> Result<Self, PeerResourceError>
    where
        T: outer::PrefixSchema<'de>,
    {
        let reservation = turn
            .reserve(limits.allocation_bytes)
            .map_err(|_| PeerResourceError::Capacity)?;
        let value = outer::decode_prefix(
            bytes,
            outer::Limits {
                allocation: limits.allocation_bytes,
                collection_entries: limits.collection_entries,
            },
        )
        .map_err(|error| match error {
            outer::Error::Capacity => PeerResourceError::Capacity,
            outer::Error::Invalid => PeerResourceError::Malformed,
        })?;
        Ok(Self {
            value,
            _reservation: reservation,
        })
    }
    pub(crate) fn decode_in_turn<'de>(
        bytes: &'de [u8],
        limits: OuterLimits,
        turn: &automerge::resource_limits::NativeWorkTurn,
    ) -> Result<Self, PeerResourceError>
    where
        T: outer::Schema<'de>,
    {
        let reservation = turn
            .reserve(limits.allocation_bytes)
            .map_err(|_| PeerResourceError::Capacity)?;
        Self::decode_reserved(bytes, limits, reservation)
    }
    pub(crate) fn decode<'de>(
        bytes: &'de [u8],
        limits: OuterLimits,
        allowance: &NativeAllowance,
    ) -> Result<Self, PeerResourceError>
    where
        T: outer::Schema<'de>,
    {
        let reservation = allowance
            .reserve(limits.allocation_bytes)
            .map_err(|_| PeerResourceError::Capacity)?;
        Self::decode_reserved(bytes, limits, reservation)
    }
    // The only two callers first reserve through a held allowance/current turn.
    // Kept private: no external transformation/extraction or Schema expansion.
    fn decode_reserved<'de>(
        bytes: &'de [u8],
        limits: OuterLimits,
        reservation: NativeReservation,
    ) -> Result<Self, PeerResourceError>
    where
        T: outer::Schema<'de>,
    {
        let value = outer::decode(
            bytes,
            outer::Limits {
                allocation: limits.allocation_bytes,
                collection_entries: limits.collection_entries,
            },
        )
        .map_err(|error| match error {
            outer::Error::Capacity => PeerResourceError::Capacity,
            outer::Error::Invalid => PeerResourceError::Malformed,
        })?;
        Ok(Self {
            value,
            _reservation: reservation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeighbourMsg;

    #[test]
    fn outer_resources_keep_one_shared_reservation_for_each_live_message() {
        let allowance = NativeAllowance::new(64 * 1024).unwrap();
        let initial = allowance.available_bytes();
        assert!(initial < 64 * 1024, "ledger backing is itself charged");
        let shared = allowance.clone();
        let limits = OuterLimits {
            allocation_bytes: 48 * 1024,
            collection_entries: 4096,
        };
        let message = NeighbourMsg::SyncResp {
            changes: vec![vec![1; 128]],
            room: "room".into(),
        };
        let bytes = postcard::to_allocvec(&message).unwrap();
        let held = BoundedPeerMessage::decode(&bytes, limits, &allowance).unwrap();
        assert_eq!(held.message(), &message);
        assert_eq!(allowance.available_bytes(), initial - 48 * 1024);
        assert!(matches!(
            BoundedPeerMessage::decode(&bytes, limits, &shared),
            Err(PeerResourceError::Capacity)
        ));
        drop(allowance);
        assert_eq!(shared.available_bytes(), initial - 48 * 1024);
        drop(held);
        assert_eq!(shared.available_bytes(), initial);
        assert!(BoundedPeerMessage::decode(&bytes, limits, &shared).is_ok());
    }

    #[test]
    fn outer_resources_failed_decode_releases_its_reservation() {
        let allowance = NativeAllowance::new(64 * 1024).unwrap();
        let initial = allowance.available_bytes();
        let limits = OuterLimits {
            allocation_bytes: 32 * 1024,
            collection_entries: 4096,
        };
        assert!(BoundedPeerMessage::decode(&[255], limits, &allowance).is_err());
        assert_eq!(allowance.available_bytes(), initial);
    }
}
