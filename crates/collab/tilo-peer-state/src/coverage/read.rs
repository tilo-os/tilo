//! One governed-checkpoint coverage check for both storage modes.
use crate::resources::{OuterLimits, PeerResourceError, outer};
use crate::room::read::{ReadError, RoomRead, RoomValue, Selection};
use std::collections::BTreeMap;

pub(crate) trait CoverageRowView {
    fn payload_hash(&self) -> [u8; 32];
    fn admitted_in(&self, covered: &BTreeMap<String, u64>) -> bool;
}
impl CoverageRowView for super::RouteValue {
    fn payload_hash(&self) -> [u8; 32] {
        super::RouteValue::payload_hash(self)
    }
    fn admitted_in(&self, covered: &BTreeMap<String, u64>) -> bool {
        super::RouteValue::admitted_in(self, covered)
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
/// How a stored checkpoint stands against its route's governed coverage.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Governed {
    /// No coverage or no checkpoint to compare, or the checkpoint is the
    /// covered one.
    Current,
    /// The coverage names a checkpoint the document has not admitted yet:
    /// coverage and checkpoint arrive as separate frames.
    Arriving,
}
/// Check a stored checkpoint against its route's governed coverage row, if
/// any. A checkpoint older than coverage whose body the document has not
/// admitted (`covered`, per hex author) is arriving; one that differs from
/// coverage it already admitted is refused.
pub(crate) fn governed_snapshot_matches<R: RoomRead, D: CoverageDecode>(
    room: Option<&R>,
    (app, record): (&str, &str),
    covered: &BTreeMap<String, u64>,
    snapshot: Option<&[u8]>,
    decoder: &D,
) -> Result<Governed, CoverageMatchError<D::Error>> {
    let (Some(snapshot), Some(room)) = (snapshot, room) else {
        return Ok(Governed::Current);
    };
    let mut governed = Governed::Current;
    let mut resolve = |row: &D::Row| -> Result<(), CoverageMatchError<D::Error>> {
        if crate::document::snapshot_hash(snapshot).is_ok_and(|hash| hash == row.payload_hash()) {
            Ok(())
        } else if row.admitted_in(covered) {
            Err(CoverageMatchError::NoMatch)
        } else {
            governed = Governed::Arriving;
            Ok(())
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
                resolve(&row)
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
        Ok(_) | Err(ReadError::Malformed) => Ok(governed),
        Err(ReadError::Capacity) => Err(CoverageMatchError::Capacity),
        Err(ReadError::Visitor(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests;
