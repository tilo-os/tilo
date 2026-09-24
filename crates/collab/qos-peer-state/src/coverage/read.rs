//! One checkpoint wire-version resolver for legacy and retained native reads.
use super::Coverage;
use crate::resources::{Held, OuterLimits, PeerResourceError, outer};
use crate::room::read::{ReadError, RoomRead, RoomValue, Selection};
use automerge::resource_limits::{NativeResourceError, NativeWorkTurn};
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
pub(crate) trait CoverageDecode {
    type Error;
    type Rows: CoverageView;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Rows, Self::Error>;
}
pub(crate) struct LegacyCoverage;
impl CoverageDecode for LegacyCoverage {
    type Error = postcard::Error;
    type Rows = BTreeMap<String, Coverage>;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Rows, Self::Error> {
        postcard::from_bytes(bytes)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct CoverageRows(BTreeMap<String, Coverage>);
impl outer::EncodeSchema for CoverageRows {}
impl<'de> outer::Schema<'de> for CoverageRows {}
impl<'de> outer::PrefixSchema<'de> for CoverageRows {}
pub(crate) struct BoundedCoverageRows {
    held: Held<CoverageRows>,
}
impl CoverageView for BoundedCoverageRows {
    fn route(&self, app: &str, record: &str) -> Option<&Coverage> {
        route(&self.held.value.0, app, record)
    }
}
pub(crate) struct BoundedCoverage<'a> {
    pub(crate) turn: &'a NativeWorkTurn,
    pub(crate) limits: OuterLimits,
}
impl CoverageDecode for BoundedCoverage<'_> {
    type Error = PeerResourceError;
    type Rows = BoundedCoverageRows;
    fn decode(&self, bytes: &[u8]) -> Result<Self::Rows, Self::Error> {
        Ok(BoundedCoverageRows {
            held: Held::decode_prefix_in_turn(bytes, self.limits, self.turn)?,
        })
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WireVersionError<E> {
    Decode(E),
    Resource(NativeResourceError),
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
    let mut version = None;
    let read = room.visit_property(&room.root(), super::FIELD, Selection::Winner, |value| {
        // The historical accessor decodes empty bytes for a present wrong
        // scalar/object shape, and ignores a failed native property lookup.
        let bytes = match value {
            RoomValue::Bytes(bytes) => bytes,
            _ => &[],
        };
        let rows = decoder.decode(bytes).map_err(WireVersionError::Decode)?;
        let Some(coverage) = rows.route(app, record) else {
            return Ok(());
        };
        for candidate in DocumentWireVersion::ALL {
            if crate::document::snapshot_hash_for(snapshot, candidate)
                .is_ok_and(|hash| hash == coverage.payload_hash)
            {
                if version.is_some() {
                    return Err(WireVersionError::Ambiguous);
                }
                version = Some(candidate);
            }
        }
        if version.is_none() {
            return Err(WireVersionError::NoMatch);
        }
        Ok(())
    });
    match read {
        Ok(_) | Err(ReadError::Malformed) => Ok(version),
        Err(ReadError::Resource(error)) => Err(WireVersionError::Resource(error)),
        Err(ReadError::Visitor(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests;
