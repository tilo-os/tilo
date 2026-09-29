//! Guarded original durable DTO storage, before semantic peer restoration.
use super::DurableState;
use crate::admission::BoundedExactAdmissions;
use crate::resources::{OuterLimits, PeerResourceError};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DurableDecodeError {
    Resource(PeerResourceError),
    UnsupportedVersion,
    Envelope,
    Pending(super::pending::PendingError),
}

// The decoded outer schema, before any identity authority is applied.
pub(super) struct DecodedDurable {
    pub(super) wire: DurableState,
    pub(super) exact: Option<BoundedExactAdmissions>,
}
impl DecodedDurable {
    pub(super) fn decode(
        bytes: &[u8],
        outer: OuterLimits,
        exact_limits: OuterLimits,
    ) -> Result<Self, DurableDecodeError> {
        let (version, _) = postcard::take_from_bytes::<u32>(bytes)
            .map_err(|_| DurableDecodeError::Resource(PeerResourceError::Malformed))?;
        if version != super::DURABLE_VERSION {
            return Err(DurableDecodeError::UnsupportedVersion);
        }
        let wire = crate::resources::decode::<DurableState>(bytes, outer)
            .map_err(DurableDecodeError::Resource)?;
        // Bounded reads refuse encrypted records, so the sealed history is
        // only bounded here, never kept.
        if wire.exact.is_none() && !wire.sealed.is_empty() {
            return Err(DurableDecodeError::Envelope);
        }
        let exact = wire
            .exact
            .as_deref()
            .map(|exact| BoundedExactAdmissions::decode(exact, exact_limits))
            .transpose()
            .map_err(DurableDecodeError::Resource)?;
        super::pending::validate(&wire.pending, wire.pending_bytes)
            .map_err(DurableDecodeError::Pending)?;
        Ok(Self { wire, exact })
    }
}

#[cfg(test)]
mod tests;
