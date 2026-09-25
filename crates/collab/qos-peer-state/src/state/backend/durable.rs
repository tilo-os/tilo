//! Guarded original durable DTO storage, before semantic peer restoration.
use super::{Durable, DurableV2, DurableV4, DurableV5};
use crate::admission::BoundedExactAdmissions;
use crate::resources::{OuterLimits, PeerResourceError};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DurableDecodeError {
    Resource(PeerResourceError),
    UnsupportedVersion,
    Envelope,
    Pending(super::pending::PendingError),
}

// Keep each actual historical schema intact. No identity authority here.
pub(super) enum DurableWire {
    V2(DurableV2),
    V3(Durable),
    V4(DurableV4),
}
pub(super) struct DecodedDurable {
    pub(super) wire: DurableWire,
    pub(super) exact: Option<BoundedExactAdmissions>,
    pub(super) version: u32,
}
impl DecodedDurable {
    pub(super) fn decode(
        bytes: &[u8],
        outer: OuterLimits,
        exact_limits: OuterLimits,
    ) -> Result<Self, DurableDecodeError> {
        let (version, _) = postcard::take_from_bytes::<u32>(bytes)
            .map_err(|_| DurableDecodeError::Resource(PeerResourceError::Malformed))?;
        if !matches!(version, 2..=5) {
            return Err(DurableDecodeError::UnsupportedVersion);
        }
        // Every path below is one of the closed schemas.
        macro_rules! decode {
            ($ty:ty) => {
                crate::resources::decode::<$ty>(bytes, outer)
                    .map_err(DurableDecodeError::Resource)?
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
                // The input borrow cannot escape: decode the canonical exact
                // index now, then move the owned V4 base.
                let exact = BoundedExactAdmissions::decode(value.exact, exact_limits)
                    .map_err(DurableDecodeError::Resource)?;
                (DurableWire::V4(value.base), Some(exact))
            }
            _ => return Err(DurableDecodeError::UnsupportedVersion),
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
        })
    }
}

#[cfg(test)]
mod tests;
