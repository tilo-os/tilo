//! Guarded original durable DTO storage, before semantic peer restoration.
use super::{DurableV4, DurableV5, DurableV6};
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
    pub(super) wire: DurableV4,
    pub(super) exact: Option<BoundedExactAdmissions>,
    pub(super) version: u32,
    pub(super) sealed: super::super::SealedHistory,
}
impl DecodedDurable {
    pub(super) fn decode(
        bytes: &[u8],
        outer: OuterLimits,
        exact_limits: OuterLimits,
    ) -> Result<Self, DurableDecodeError> {
        let (version, _) = postcard::take_from_bytes::<u32>(bytes)
            .map_err(|_| DurableDecodeError::Resource(PeerResourceError::Malformed))?;
        if !matches!(version, 4..=6) {
            return Err(DurableDecodeError::UnsupportedVersion);
        }
        // Every path below is one of the closed schemas.
        macro_rules! decode {
            ($ty:ty) => {
                crate::resources::decode::<$ty>(bytes, outer)
                    .map_err(DurableDecodeError::Resource)?
            };
        }
        let (wire, exact, sealed) = match version {
            4 => (decode!(DurableV4), None, Vec::new()),
            5 => {
                let value = decode!(DurableV5<'_>);
                if value.version != 5 || value.base.version != 4 {
                    return Err(DurableDecodeError::Envelope);
                }
                // The input borrow cannot escape: decode the canonical exact
                // index now, then move the owned V4 base.
                let exact = BoundedExactAdmissions::decode(value.exact, exact_limits)
                    .map_err(DurableDecodeError::Resource)?;
                (value.base, Some(exact), Vec::new())
            }
            // Retain the bounded sealed history for authenticated replay.
            6 => {
                let value = decode!(DurableV6<'_>);
                if value.version != 6 || value.base.version != 4 || value.sealed.is_empty() {
                    return Err(DurableDecodeError::Envelope);
                }
                let exact = BoundedExactAdmissions::decode(value.exact, exact_limits)
                    .map_err(DurableDecodeError::Resource)?;
                (value.base, Some(exact), value.sealed)
            }
            _ => return Err(DurableDecodeError::UnsupportedVersion),
        };
        if wire.version != 4 || wire.base.version != 3 {
            return Err(DurableDecodeError::Envelope);
        }
        super::pending::validate(&wire.pending, wire.pending_bytes)
            .map_err(DurableDecodeError::Pending)?;
        Ok(Self {
            wire,
            exact,
            version,
            sealed,
        })
    }
}

#[cfg(test)]
mod tests;
