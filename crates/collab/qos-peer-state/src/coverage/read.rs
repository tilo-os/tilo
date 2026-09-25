//! One checkpoint wire-version resolver for both storage modes.
use super::Coverage;
use crate::resources::{OuterLimits, PeerResourceError, outer};
use crate::room::read::{ReadError, RoomRead, RoomValue, Selection};
use collab_types::DocumentWireVersion;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) trait CoverageView {
    fn route(&self, app: &str, record: &str) -> Option<&Coverage>;
}
fn route<'a>(
    rows: &'a BTreeMap<String, Coverage>,
    app: &str,
    record: &str,
) -> Option<&'a Coverage> {
    rows.iter()
        .find(|(key, _)| {
            key.as_bytes()
                .strip_prefix(app.as_bytes())
                .and_then(|tail| tail.strip_prefix(&[0]))
                == Some(record.as_bytes())
        })
        .map(|(_, value)| value)
}
impl CoverageView for BTreeMap<String, Coverage> {
    fn route(&self, app: &str, record: &str) -> Option<&Coverage> {
        route(self, app, record)
    }
}
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
    type Rows: CoverageView;
    type Row: CoverageRowView;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Rows, Self::Error>;
    /// One v2 per-route value (`super::FIELD_V2`).
    fn decode_row(&self, bytes: &[u8]) -> Result<Self::Row, Self::Error>;
}
pub(crate) struct LegacyCoverage;
impl CoverageDecode for LegacyCoverage {
    type Error = postcard::Error;
    type Rows = BTreeMap<String, Coverage>;
    type Row = super::RouteValue;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Rows, Self::Error> {
        postcard::from_bytes(bytes)
    }
    fn decode_row(&self, bytes: &[u8]) -> Result<Self::Row, Self::Error> {
        match super::decode_value(bytes) {
            Ok(value) => Ok(value),
            Err(_) => postcard::from_bytes(bytes),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct CoverageRows(BTreeMap<String, Coverage>);
impl outer::EncodeSchema for CoverageRows {}
impl outer::Schema<'_> for CoverageRows {}
impl outer::PrefixSchema<'_> for CoverageRows {}
impl outer::EncodeSchema for super::RouteValue {}
impl outer::Schema<'_> for super::RouteValue {}
/// The interim bare-`Coverage` route value (see `super::decode_value`).
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct BareCoverage(Coverage);
impl outer::EncodeSchema for BareCoverage {}
impl outer::Schema<'_> for BareCoverage {}
pub(crate) enum BoundedCoverageRow {
    Route(super::RouteValue),
    Bare(BareCoverage),
}
impl CoverageRowView for BoundedCoverageRow {
    fn payload_hash(&self) -> [u8; 32] {
        match self {
            Self::Route(value) => value.payload_hash(),
            Self::Bare(value) => value.0.payload_hash,
        }
    }
}
impl CoverageView for CoverageRows {
    fn route(&self, app: &str, record: &str) -> Option<&Coverage> {
        route(&self.0, app, record)
    }
}
/// Coverage decoded within [`OuterLimits`].
pub(crate) struct BoundedCoverage {
    pub(crate) limits: OuterLimits,
}
impl CoverageDecode for BoundedCoverage {
    type Error = PeerResourceError;
    type Rows = CoverageRows;
    type Row = BoundedCoverageRow;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Rows, Self::Error> {
        crate::resources::decode_prefix(bytes, self.limits)
    }
    fn decode_row(&self, bytes: &[u8]) -> Result<Self::Row, Self::Error> {
        match crate::resources::decode(bytes, self.limits) {
            Ok(value) => Ok(BoundedCoverageRow::Route(value)),
            Err(PeerResourceError::Malformed) => Ok(BoundedCoverageRow::Bare(
                crate::resources::decode(bytes, self.limits)?,
            )),
            Err(error) => Err(error),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WireVersionError<E> {
    Decode(E),
    Capacity,
    NoMatch,
    Ambiguous,
}
pub(crate) fn snapshot_wire_version<R: RoomRead, D: CoverageDecode>(
    room: Option<&R>,
    app: &str,
    record: &str,
    snapshot: Option<&[u8]>,
    decoder: &D,
) -> Result<Option<DocumentWireVersion>, WireVersionError<D::Error>> {
    let (Some(snapshot), Some(room)) = (snapshot, room) else {
        return Ok(None);
    };
    let resolve =
        |payload_hash: [u8; 32]| -> Result<DocumentWireVersion, WireVersionError<D::Error>> {
            let mut version = None;
            for candidate in DocumentWireVersion::ALL {
                if crate::document::snapshot_hash_for(snapshot, candidate)
                    .is_ok_and(|hash| hash == payload_hash)
                {
                    if version.is_some() {
                        return Err(WireVersionError::Ambiguous);
                    }
                    version = Some(candidate);
                }
            }
            version.ok_or(WireVersionError::NoMatch)
        };
    // v2 per-route map first: a present route there supersedes the legacy
    // whole-map scalar (which the governor deletes when it migrates).
    // Matched against the stored keys without allocating the route key.
    let is_route = |key: &str| {
        key.as_bytes()
            .strip_prefix(app.as_bytes())
            .and_then(|tail| tail.strip_prefix(&[0]))
            == Some(record.as_bytes())
    };
    let mut version = None;
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
                    .map_err(WireVersionError::Decode)?;
                routed = true;
                version = Some(resolve(row.payload_hash())?);
                Ok(())
            });
            match inner {
                Ok(_) | Err(ReadError::Malformed) => Ok(()),
                Err(ReadError::Capacity) => Err(WireVersionError::Capacity),
                Err(ReadError::Visitor(error)) => Err(error),
            }
        });
        match keys {
            Ok(_) | Err(ReadError::Malformed) => Ok(()),
            Err(ReadError::Capacity) => Err(WireVersionError::Capacity),
            Err(ReadError::Visitor(error)) => Err(error),
        }
    });
    match v2 {
        Ok(_) | Err(ReadError::Malformed) => {}
        Err(ReadError::Capacity) => return Err(WireVersionError::Capacity),
        Err(ReadError::Visitor(error)) => return Err(error),
    }
    if routed {
        return Ok(version);
    }
    let read = room.visit_property(&room.root(), super::FIELD, Selection::Winner, |value| {
        // The historical accessor decodes empty bytes for a present wrong
        // scalar/object shape, and ignores a failed property lookup.
        let bytes = match value {
            RoomValue::Bytes(bytes) => bytes,
            _ => &[],
        };
        let rows = decoder.decode(bytes).map_err(WireVersionError::Decode)?;
        let Some(coverage) = rows.route(app, record) else {
            return Ok(());
        };
        version = Some(resolve(coverage.payload_hash)?);
        Ok(())
    });
    match read {
        Ok(_) | Err(ReadError::Malformed) => Ok(version),
        Err(ReadError::Capacity) => Err(WireVersionError::Capacity),
        Err(ReadError::Visitor(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests;
