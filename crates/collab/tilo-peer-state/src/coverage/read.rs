//! One governed-checkpoint coverage check for both storage modes.
use crate::resources::{OuterLimits, PeerResourceError, outer};
use crate::room::read::{ReadError, RoomRead, RoomValue, Selection};

pub(crate) trait CoverageRowView {
    fn payload_hash(&self) -> [u8; 32];
}
impl CoverageRowView for super::RouteValue {
    fn payload_hash(&self) -> [u8; 32] {
        super::RouteValue::payload_hash(self)
    }
}
pub(crate) trait CoverageDecode {
    type Error;
    type Row: CoverageRowView;
    /// One per-route value (`super::FIELD_V2`).
    fn decode_row(&self, bytes: &[u8]) -> Result<Self::Row, Self::Error>;
}
pub(crate) struct LegacyCoverage;
impl CoverageDecode for LegacyCoverage {
    type Error = postcard::Error;
    type Row = super::RouteValue;
    fn decode_row(&self, bytes: &[u8]) -> Result<Self::Row, Self::Error> {
        match super::decode_value(bytes) {
            Ok(value) => Ok(value),
            Err(_) => postcard::from_bytes(bytes),
        }
    }
}
impl outer::EncodeSchema for super::RouteValue {}
impl outer::Schema<'_> for super::RouteValue {}
/// Coverage decoded within [`OuterLimits`].
pub(crate) struct BoundedCoverage {
    pub(crate) limits: OuterLimits,
}
impl CoverageDecode for BoundedCoverage {
    type Error = PeerResourceError;
    type Row = super::RouteValue;
    fn decode_row(&self, bytes: &[u8]) -> Result<Self::Row, Self::Error> {
        crate::resources::decode(bytes, self.limits)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CoverageMatchError<E> {
    Decode(E),
    Capacity,
    NoMatch,
}
/// Check a stored checkpoint against its route's governed coverage row, if
/// any: a row whose payload hash does not match the checkpoint's signed
/// encoding is refused.
pub(crate) fn governed_snapshot_matches<R: RoomRead, D: CoverageDecode>(
    room: Option<&R>,
    app: &str,
    record: &str,
    snapshot: Option<&[u8]>,
    decoder: &D,
) -> Result<(), CoverageMatchError<D::Error>> {
    let (Some(snapshot), Some(room)) = (snapshot, room) else {
        return Ok(());
    };
    let resolve = |payload_hash: [u8; 32]| -> Result<(), CoverageMatchError<D::Error>> {
        if crate::document::snapshot_hash(snapshot).is_ok_and(|hash| hash == payload_hash) {
            Ok(())
        } else {
            Err(CoverageMatchError::NoMatch)
        }
    };
    // Matched against the stored keys without allocating the route key.
    let is_route = |key: &str| {
        key.as_bytes()
            .strip_prefix(app.as_bytes())
            .and_then(|tail| tail.strip_prefix(&[0]))
            == Some(record.as_bytes())
    };
    let mut routed = false;
    let v2 = room.visit_property(&room.root(), super::FIELD_V2, Selection::Winner, |value| {
        let RoomValue::Object { object, .. } = value else {
            return Ok(());
        };
        let keys = room.visit_keys(&object, |key| {
            if routed || !is_route(key) {
                return Ok(());
            }
            let inner = room.visit_property(&object, key, Selection::Winner, |entry| {
                let RoomValue::Bytes(bytes) = entry else {
                    return Ok(());
                };
                let row = decoder
                    .decode_row(bytes)
                    .map_err(CoverageMatchError::Decode)?;
                routed = true;
                resolve(row.payload_hash())
            });
            match inner {
                Ok(_) | Err(ReadError::Malformed) => Ok(()),
                Err(ReadError::Capacity) => Err(CoverageMatchError::Capacity),
                Err(ReadError::Visitor(error)) => Err(error),
            }
        });
        match keys {
            Ok(_) | Err(ReadError::Malformed) => Ok(()),
            Err(ReadError::Capacity) => Err(CoverageMatchError::Capacity),
            Err(ReadError::Visitor(error)) => Err(error),
        }
    });
    match v2 {
        Ok(_) | Err(ReadError::Malformed) => Ok(()),
        Err(ReadError::Capacity) => Err(CoverageMatchError::Capacity),
        Err(ReadError::Visitor(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests;
