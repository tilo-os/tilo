//! Guarded original durable DTO storage, before semantic peer restoration.
use super::*;
use crate::admission::BoundedExactAdmissions;
use crate::resources::{OuterLimits, PeerResourceError};
use automerge::resource_limits::NativeWorkTurn;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DurableDecodeError {
    Resource(PeerResourceError),
    UnsupportedVersion,
    Envelope,
    Pending(super::pending::PendingError),
}

// Keep each actual historical schema intact; especially V2's document vector
// is not converted by an uncharged collect. No native/identity authority here.
pub(super) enum DurableWire {
    V2(DurableV2),
    V3(Durable),
    V4(DurableV4),
}
pub(super) struct DecodedDurable {
    pub(super) wire: DurableWire,
    pub(super) exact: Option<BoundedExactAdmissions>,
    pub(super) version: u32,
    // All owned wire fields are destroyed before their retained reservation.
    pub(super) retention: NativeReservation,
}
impl DecodedDurable {
    pub(super) fn decode(
        bytes: &[u8],
        outer: OuterLimits,
        exact_limits: OuterLimits,
        turn: &NativeWorkTurn,
    ) -> Result<Self, DurableDecodeError> {
        let (version, _) = postcard::take_from_bytes::<u32>(bytes)
            .map_err(|_| DurableDecodeError::Resource(PeerResourceError::Malformed))?;
        if !matches!(version, 2..=5) {
            return Err(DurableDecodeError::UnsupportedVersion);
        }
        let retention = turn
            .reserve(outer.allocation_bytes)
            .map_err(|_| DurableDecodeError::Resource(PeerResourceError::Capacity))?;
        // Every path below is one of the already audited closed schemas. The
        // held reservation precedes the decoder and every resulting allocation.
        macro_rules! decode {
            ($ty:ty) => {
                crate::resources::outer::decode::<$ty>(
                    bytes,
                    crate::resources::outer::Limits {
                        allocation: outer.allocation_bytes,
                        collection_entries: outer.collection_entries,
                    },
                )
                .map_err(|error| {
                    DurableDecodeError::Resource(match error {
                        crate::resources::outer::Error::Capacity => PeerResourceError::Capacity,
                        crate::resources::outer::Error::Invalid => PeerResourceError::Malformed,
                    })
                })?
            };
        }
        let (wire, exact) = match version {
            2 => (DurableWire::V2(decode!(DurableV2)), None),
            3 => (DurableWire::V3(decode!(Durable)), None),
            4 => (DurableWire::V4(decode!(DurableV4)), None),
            5 => {
                let value = decode!(DurableV5<'_>);
                if value.version != 5 || value.base.version != 4 {
                    return Err(DurableDecodeError::Envelope);
                }
                // The original input borrow cannot escape. Decode its canonical
                // exact index now, under the SAME current work turn, then move
                // the owned V4 base without reallocating or cloning its fields.
                let exact = BoundedExactAdmissions::decode_in_turn(value.exact, exact_limits, turn)
                    .map_err(DurableDecodeError::Resource)?;
                (DurableWire::V4(value.base), Some(exact))
            }
            _ => unreachable!(),
        };
        match &wire {
            DurableWire::V2(value) if value.version != 2 => {
                return Err(DurableDecodeError::Envelope);
            }
            DurableWire::V3(value) if value.version != 3 => {
                return Err(DurableDecodeError::Envelope);
            }
            DurableWire::V4(value) => {
                if value.version != 4 || value.base.version != 3 {
                    return Err(DurableDecodeError::Envelope);
                }
                super::pending::validate(&value.pending, value.pending_bytes)
                    .map_err(DurableDecodeError::Pending)?;
            }
            _ => {}
        }
        Ok(Self {
            wire,
            exact,
            version,
            retention,
        })
    }
}

#[cfg(test)]
mod tests;
