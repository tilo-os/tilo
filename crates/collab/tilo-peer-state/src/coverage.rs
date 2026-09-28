//! Governor-authenticated replay floors for exact retained checkpoints.
//! Carried in the existing room CRDT; this does not change neighbour v1 bytes.
pub(crate) mod read;
use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
/// Per-route map: a commit rewrites only the routes it changed.
pub const FIELD_V2: &str = "document_checkpoint_coverage_v2";
pub(crate) const LAYOUT: crate::governed_routes::Layout =
    crate::governed_routes::Layout { routes: FIELD_V2 };
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub room: String,
    pub app: String,
    pub record: String,
    pub payload_hash: [u8; 32],
    pub heads: Vec<String>,
    pub author: [u8; 32],
    pub sequence: u64,
    pub watermarks: BTreeMap<String, u64>,
}
/// Identity of the whole governed coverage for "unchanged"
/// comparisons; `None` when the room carries none.
pub fn governed_bytes(doc: &AutoCommit) -> Option<Vec<u8>> {
    crate::governed_routes::identity(doc, LAYOUT)
}

/// One route value. `Compact` drops what the route key and fixed-width
/// binary forms already determine (app/record, hex heads and watermark
/// authors, the textual room UUID); it is chosen only when it expands back
/// to exactly the same `Coverage`, otherwise the route stores `Full`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum RouteValue {
    Compact(CompactCoverage),
    Full(Coverage),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompactCoverage {
    room: [u8; 16],
    payload_hash: [u8; 32],
    heads: Vec<[u8; 32]>,
    author: [u8; 32],
    sequence: u64,
    watermarks: Vec<([u8; 32], u64)>,
}
impl RouteValue {
    pub(crate) fn payload_hash(&self) -> [u8; 32] {
        match self {
            Self::Compact(compact) => compact.payload_hash,
            Self::Full(coverage) => coverage.payload_hash,
        }
    }
    fn expand(self, key: &str) -> Result<Coverage, String> {
        match self {
            Self::Full(coverage) => Ok(coverage),
            Self::Compact(compact) => {
                let (app, record) = key
                    .split_once('\0')
                    .ok_or("compact checkpoint coverage without a route key")?;
                Ok(Coverage {
                    room: uuid::Uuid::from_bytes(compact.room).to_string(),
                    app: app.into(),
                    record: record.into(),
                    payload_hash: compact.payload_hash,
                    heads: compact.heads.iter().map(hex::encode).collect(),
                    author: compact.author,
                    sequence: compact.sequence,
                    watermarks: compact
                        .watermarks
                        .iter()
                        .map(|(author, seq)| (hex::encode(author), *seq))
                        .collect(),
                })
            }
        }
    }
    fn of(key: &str, coverage: &Coverage) -> Self {
        let hash = |text: &String| -> Option<[u8; 32]> { hex::decode(text).ok()?.try_into().ok() };
        let compact = (|| {
            Some(CompactCoverage {
                room: *uuid::Uuid::parse_str(&coverage.room).ok()?.as_bytes(),
                payload_hash: coverage.payload_hash,
                heads: coverage.heads.iter().map(hash).collect::<Option<_>>()?,
                author: coverage.author,
                sequence: coverage.sequence,
                watermarks: coverage
                    .watermarks
                    .iter()
                    .map(|(author, seq)| Some((hash(author)?, *seq)))
                    .collect::<Option<_>>()?,
            })
        })()
        .map(Self::Compact)
        .filter(|value| value.clone().expand(key).ok().as_ref() == Some(coverage));
        compact.unwrap_or_else(|| Self::Full(coverage.clone()))
    }
}
pub(crate) fn encode_route(key: &str, coverage: &Coverage) -> Result<Vec<u8>, String> {
    postcard::to_allocvec(&RouteValue::of(key, coverage)).map_err(|e| e.to_string())
}
/// One stored route value.
pub(crate) fn decode_value(bytes: &[u8]) -> Result<RouteValue, String> {
    if let Some(value) = crate::governed_routes::exact::<RouteValue>(bytes) {
        return Ok(value);
    }
    Err(format!(
        "invalid checkpoint coverage: {}",
        postcard::from_bytes::<RouteValue>(bytes)
            .err()
            .map_or_else(|| "trailing bytes".to_string(), |e| e.to_string())
    ))
}
fn decode_route(key: &str, bytes: &[u8]) -> Result<Coverage, String> {
    decode_value(bytes)?.expand(key)
}
/// Whether `bytes` is how a writer stores `row`.
fn stored_as(key: &str, row: &Coverage, bytes: &[u8]) -> bool {
    encode_route(key, row).ok().as_deref() == Some(bytes)
}
/// Raw route values of the room (empty without the map).
pub(crate) fn route_values(doc: &AutoCommit) -> Result<BTreeMap<String, Vec<u8>>, String> {
    Ok(crate::governed_routes::route_bytes(doc, LAYOUT)?.unwrap_or_default())
}
/// The coverage rows recorded in `doc`.
///
/// # Errors
///
/// Returns an error if a row is malformed.
pub fn entries(doc: &AutoCommit) -> Result<BTreeMap<String, Coverage>, String> {
    let mut rows = BTreeMap::new();
    for (key, bytes) in route_values(doc)? {
        let row = decode_route(&key, &bytes)?;
        rows.insert(key, row);
    }
    Ok(rows)
}
/// Every stored coverage encoding is canonical.
pub fn canonical(doc: &AutoCommit) -> bool {
    route_values(doc).is_ok_and(|routes| {
        routes.iter().all(|(key, bytes)| {
            decode_route(key, bytes).is_ok_and(|row| stored_as(key, &row, bytes))
        })
    })
}
pub fn route_key(app: &str, record: &str) -> String {
    format!("{app}\0{record}")
}
/// Write one coverage row.
///
/// # Errors
///
/// Returns an error if the row cannot be encoded or written.
pub fn put(doc: &mut AutoCommit, coverage: Coverage) -> Result<(), String> {
    put_all(doc, vec![coverage])
}
/// Publish `coverages` and, in the SAME room change, the complete durable
/// receipt set `receipts(new coverage rows)` (the governor's durable publish).
/// One change per durable publication instead of a coverage change plus a
/// receipt change, and receipts for the just-published coverage store only
/// their generation.
pub(crate) fn put_all_with_receipts(
    doc: &mut AutoCommit,
    coverages: Vec<Coverage>,
    receipts: impl FnOnce(
        &BTreeMap<String, Coverage>,
    ) -> Result<BTreeMap<String, crate::durability::DurableReceipt>, String>,
) -> Result<(), String> {
    let (rows, next) = next_rows(doc, coverages)?;
    let receipts = crate::durability::encode_all(&receipts(&rows)?, &rows)?;
    crate::governed_routes::write_raw(
        doc,
        &[(LAYOUT, &next), (crate::durability::LAYOUT, &receipts)],
    )?;
    Ok(())
}
/// Decoded coverage rows and their raw encoded values, by route.
type CoverageRows = (BTreeMap<String, Coverage>, BTreeMap<String, Vec<u8>>);
fn next_rows(doc: &AutoCommit, coverages: Vec<Coverage>) -> Result<CoverageRows, String> {
    let mut rows = entries(doc)?;
    let mut incoming = BTreeSet::new();
    for coverage in coverages {
        let route = route_key(&coverage.app, &coverage.record);
        if !incoming.insert(route.clone()) {
            return Err("duplicate checkpoint coverage route in batch".into());
        }
        rows.insert(route, coverage);
    }
    let next = rows
        .iter()
        .map(|(key, row)| Ok((key.clone(), encode_route(key, row)?)))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    Ok((rows, next))
}
pub(crate) fn put_all(doc: &mut AutoCommit, coverages: Vec<Coverage>) -> Result<(), String> {
    if coverages.is_empty() {
        return Ok(());
    }
    let (_, next) = next_rows(doc, coverages)?;
    // A durable receipt stored as "the route's current coverage" must keep
    // naming the coverage it was issued for: pin it in full, in the same
    // change, before that route's coverage changes.
    let pinned = crate::durability::pin_before_coverage_change(doc, &next)?;
    match pinned {
        Some(receipts) => crate::governed_routes::write_raw(
            doc,
            &[(LAYOUT, &next), (crate::durability::LAYOUT, &receipts)],
        )?,
        None => crate::governed_routes::write_raw(doc, &[(LAYOUT, &next)])?,
    };
    Ok(())
}
/// Check that `after` never regresses a coverage row of `before`.
///
/// # Errors
///
/// Returns an error if a row is removed or regresses.
pub fn monotonic(before: Option<&AutoCommit>, after: &AutoCommit) -> Result<(), String> {
    let old = before.map(entries).transpose()?.unwrap_or_default();
    let new = entries(after)?;
    for (key, prior) in old {
        let next = new
            .get(&key)
            .ok_or("checkpoint coverage cannot be erased")?;
        if next.sequence < prior.sequence
            || next.author != prior.author
            || next.room != prior.room
            || next.app != prior.app
            || next.record != prior.record
            || next
                .watermarks
                .iter()
                .any(|(key, value)| key.len() != 64 || *value == 0)
            || prior
                .watermarks
                .iter()
                .any(|(author, seq)| next.watermarks.get(author).copied().unwrap_or(0) < *seq)
        {
            return Err("checkpoint coverage regression".into());
        }
        if next.sequence == prior.sequence && *next != prior {
            return Err("checkpoint coverage changed without a new sequence".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    #[test]
    fn duplicate_batch_routes_leave_governance_unchanged() {
        let coverage = Coverage {
            room: "room".into(),
            app: "app".into(),
            record: "record".into(),
            payload_hash: [1; 32],
            heads: vec!["head".into()],
            author: [2; 32],
            sequence: 1,
            watermarks: BTreeMap::new(),
        };
        let mut doc = AutoCommit::new();
        put(&mut doc, coverage.clone()).unwrap();
        let heads = doc.get_heads();
        let rows = entries(&doc).unwrap();
        let mut next = coverage;
        next.sequence = 2;
        assert!(
            put_all(&mut doc, vec![next.clone(), next])
                .unwrap_err()
                .contains("duplicate")
        );
        assert_eq!(doc.get_heads(), heads);
        assert_eq!(entries(&doc).unwrap(), rows);
    }
}
