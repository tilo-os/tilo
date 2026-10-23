//! Versioned governor assertions, published only by a completed native storage barrier.
//! This metadata leaves the frozen neighbour and checkpoint-coverage encodings intact.
use crate::coverage::{Coverage, route_key};
use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
/// Per-route map: a commit rewrites only the routes it changed.
pub const FIELD_V2: &str = "document_durability_receipts_v2";
pub(crate) const LAYOUT: crate::governed_routes::Layout =
    crate::governed_routes::Layout { routes: FIELD_V2 };
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableReceipt {
    pub version: u32,
    pub coverage: Coverage,
    pub generation: u64,
    pub receipt_id: String,
}
impl DurableReceipt {
    pub fn new(coverage: Coverage, generation: u64) -> Result<Self, String> {
        let mut receipt = Self {
            version: 1,
            coverage,
            generation,
            receipt_id: String::new(),
        };
        receipt.receipt_id = receipt.expected_id()?;
        Ok(receipt)
    }
    fn expected_id(&self) -> Result<String, String> {
        let bytes = postcard::to_allocvec(&(self.version, &self.coverage, self.generation))
            .map_err(|e| e.to_string())?;
        Ok(
            blake3::Hasher::new_derive_key("Tilo durable checkpoint receipt v1")
                .update(&bytes)
                .finalize()
                .to_hex()
                .to_string(),
        )
    }
    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.generation == 0 || self.receipt_id != self.expected_id()? {
            return Err("invalid durable receipt identity/version/generation".into());
        }
        Ok(())
    }
}
/// Identity of the whole governed receipt set for "unchanged"
/// comparisons; `None` when the room carries none.
pub fn governed_bytes(doc: &AutoCommit) -> Option<Vec<u8>> {
    crate::governed_routes::identity(doc, LAYOUT)
}

/// One route value. A receipt for exactly the route's current checkpoint
/// coverage (the normal case: the governor issues it right after publishing
/// that coverage) stores only its generation; the receipt, including its
/// derived id, is rebuilt from the coverage. Any other receipt is `Full`.
/// Coverage writes pin a `Current` receipt in full before changing the
/// route's coverage (`pin_before_coverage_change`), so a stored receipt
/// always names the coverage it was issued for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum RouteValue {
    Current { generation: u64 },
    Full(Box<DurableReceipt>),
}
fn encode_route(
    key: &str,
    receipt: &DurableReceipt,
    coverage: &BTreeMap<String, Coverage>,
) -> Result<Vec<u8>, String> {
    let current = coverage.get(key) == Some(&receipt.coverage)
        && DurableReceipt::new(receipt.coverage.clone(), receipt.generation)
            .ok()
            .as_ref()
            == Some(receipt);
    let value = if current {
        RouteValue::Current {
            generation: receipt.generation,
        }
    } else {
        RouteValue::Full(Box::new(receipt.clone()))
    };
    postcard::to_allocvec(&value).map_err(|e| e.to_string())
}
fn decode_route(
    key: &str,
    bytes: &[u8],
    coverage: &BTreeMap<String, Coverage>,
) -> Result<DurableReceipt, String> {
    let Some(value) = crate::governed_routes::exact::<RouteValue>(bytes) else {
        return Err(format!(
            "invalid durable receipts: {}",
            postcard::from_bytes::<RouteValue>(bytes)
                .err()
                .map_or_else(|| "trailing bytes".to_string(), |e| e.to_string())
        ));
    };
    match value {
        RouteValue::Full(receipt) => Ok(*receipt),
        RouteValue::Current { generation } => DurableReceipt::new(
            coverage
                .get(key)
                .ok_or("durable receipt names absent checkpoint coverage")?
                .clone(),
            generation,
        ),
    }
}
fn route_values(doc: &AutoCommit) -> Result<BTreeMap<String, Vec<u8>>, String> {
    Ok(crate::governed_routes::route_bytes(doc, LAYOUT)?.unwrap_or_default())
}
fn rows(doc: &AutoCommit) -> Result<BTreeMap<String, DurableReceipt>, String> {
    let mut rows = BTreeMap::new();
    let routes = route_values(doc)?;
    if !routes.is_empty() {
        let coverage = crate::coverage::entries(doc)?;
        for (key, bytes) in routes {
            let receipt = decode_route(&key, &bytes, &coverage)?;
            rows.insert(key, receipt);
        }
    }
    Ok(rows)
}
/// Every stored receipt encoding is canonical.
pub fn canonical(doc: &AutoCommit) -> bool {
    let (Ok(routes), Ok(coverage)) = (route_values(doc), crate::coverage::entries(doc)) else {
        return false;
    };
    routes.iter().all(|(key, bytes)| {
        decode_route(key, bytes, &coverage).is_ok_and(|receipt| {
            encode_route(key, &receipt, &coverage).ok().as_deref() == Some(bytes)
        })
    })
}
pub fn entries(doc: &AutoCommit) -> Result<BTreeMap<String, DurableReceipt>, String> {
    let rows = rows(doc)?;
    for (key, receipt) in &rows {
        receipt.validate()?;
        if *key != route_key(&receipt.coverage.app, &receipt.coverage.record) {
            return Err("durable receipt route mismatch".into());
        }
    }
    Ok(rows)
}
/// Raw route values of a complete receipt set against `coverage` rows.
pub(crate) fn encode_all(
    receipts: &BTreeMap<String, DurableReceipt>,
    coverage: &BTreeMap<String, Coverage>,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    receipts
        .iter()
        .map(|(key, receipt)| Ok((key.clone(), encode_route(key, receipt, coverage)?)))
        .collect()
}
/// Replace the receipt set with `receipts`, writing only changed routes.
///
/// # Errors
///
/// Returns an error if a receipt cannot be encoded or written.
#[expect(
    clippy::needless_pass_by_value,
    reason = "public API: callers hand over the complete replacement receipt set"
)]
pub fn put(doc: &mut AutoCommit, receipts: BTreeMap<String, DurableReceipt>) -> Result<(), String> {
    let next = encode_all(&receipts, &crate::coverage::entries(doc)?)?;
    crate::governed_routes::write_raw(doc, &[(LAYOUT, &next)])?;
    Ok(())
}
/// Before coverage routes change to `next_coverage` (raw route values), the
/// receipt values that must be rewritten in full in the same change: every
/// `Current` receipt whose route coverage changes. `None` when none must.
pub(crate) fn pin_before_coverage_change(
    doc: &AutoCommit,
    next_coverage: &BTreeMap<String, Vec<u8>>,
) -> Result<Option<BTreeMap<String, Vec<u8>>>, String> {
    let routes = route_values(doc)?;
    if routes.is_empty() {
        return Ok(None);
    }
    let old_coverage = crate::coverage::entries(doc)?;
    let old_raw = crate::coverage::route_values(doc)?;
    let mut changed = false;
    let mut next = BTreeMap::new();
    for (key, bytes) in routes {
        let current = matches!(
            crate::governed_routes::exact::<RouteValue>(&bytes),
            Some(RouteValue::Current { .. })
        );
        if current && old_raw.get(&key) != next_coverage.get(&key) {
            let receipt = decode_route(&key, &bytes, &old_coverage)?;
            next.insert(
                key,
                postcard::to_allocvec(&RouteValue::Full(Box::new(receipt)))
                    .map_err(|e| e.to_string())?,
            );
            changed = true;
        } else {
            next.insert(key, bytes);
        }
    }
    Ok(changed.then_some(next))
}
/// Check that `after` never regresses a receipt of `before`.
pub fn monotonic(before: Option<&AutoCommit>, after: &AutoCommit) -> Result<(), String> {
    let old = before.map(entries).transpose()?.unwrap_or_default();
    let new = entries(after)?;
    for (key, prior) in old {
        let next = new.get(&key).ok_or("durable receipt cannot be erased")?;
        if next.generation < prior.generation
            || next.coverage.sequence < prior.coverage.sequence
            || next.coverage.author != prior.coverage.author
            || next.coverage.room != prior.coverage.room
            || next.coverage.app != prior.coverage.app
            || next.coverage.record != prior.coverage.record
            || prior.coverage.watermarks.iter().any(|(author, seq)| {
                next.coverage.watermarks.get(author).copied().unwrap_or(0) < *seq
            })
            || (next.generation == prior.generation && next != &prior)
        {
            return Err("durable receipt regression".into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn receipt(generation: u64) -> DurableReceipt {
        DurableReceipt::new(
            Coverage {
                room: "room".into(),
                app: "app".into(),
                record: "record".into(),
                payload_hash: [3; 32],
                heads: vec!["head".into()],
                author: [1; 32],
                sequence: 2,
                watermarks: BTreeMap::from([("author".into(), 2)]),
            },
            generation,
        )
        .unwrap()
    }
    #[test]
    fn malformed_cross_route_and_regressing_receipts_are_rejected() {
        let mut before = AutoCommit::new();
        put(
            &mut before,
            BTreeMap::from([(route_key("app", "record"), receipt(7))]),
        )
        .unwrap();
        let mut after = before.clone();
        put(
            &mut after,
            BTreeMap::from([(route_key("app", "record"), receipt(6))]),
        )
        .unwrap();
        assert!(monotonic(Some(&before), &after).is_err());
        put(
            &mut after,
            BTreeMap::from([(route_key("other", "record"), receipt(8))]),
        )
        .unwrap();
        assert!(entries(&after).is_err());
        let mut bad = receipt(8);
        bad.coverage.payload_hash = [4; 32];
        put(
            &mut after,
            BTreeMap::from([(route_key("app", "record"), bad)]),
        )
        .unwrap();
        assert!(entries(&after).is_err());
    }
    #[test]
    fn receipt_metadata_is_pinned_governance() {
        let before = AutoCommit::new();
        let mut after = before.clone();
        put(
            &mut after,
            BTreeMap::from([(route_key("app", "record"), receipt(1))]),
        )
        .unwrap();
        assert!(!crate::room::governance_unchanged(Some(&before), &after));
    }
    fn compact_coverage(sequence: u64) -> Coverage {
        Coverage {
            room: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            app: "app".into(),
            record: "record".into(),
            payload_hash: [3; 32],
            heads: vec![hex::encode([5u8; 32])],
            author: [1; 32],
            sequence,
            watermarks: BTreeMap::from([(hex::encode([1u8; 32]), sequence)]),
        }
    }
    fn raw(doc: &AutoCommit, layout: crate::governed_routes::Layout) -> Vec<u8> {
        crate::governed_routes::route_bytes(doc, layout)
            .unwrap()
            .unwrap()
            .remove(&route_key("app", "record"))
            .unwrap()
    }
    #[test]
    fn v2_routes_store_compact_values_and_rebuild_exact_rows() {
        let mut doc = AutoCommit::new();
        let coverage = compact_coverage(1);
        crate::coverage::put_all_with_receipts(&mut doc, vec![coverage.clone()], |rows| {
            Ok(rows
                .iter()
                .map(|(key, row)| (key.clone(), DurableReceipt::new(row.clone(), 7).unwrap()))
                .collect())
        })
        .unwrap();
        let key = route_key("app", "record");
        assert_eq!(crate::coverage::entries(&doc).unwrap()[&key], coverage);
        assert_eq!(
            entries(&doc).unwrap()[&key],
            DurableReceipt::new(coverage, 7).unwrap()
        );
        // Compact coverage and a generation-only receipt.
        assert!(raw(&doc, crate::coverage::LAYOUT).len() < 200);
        assert!(raw(&doc, LAYOUT).len() < 16);
        assert!(crate::coverage::canonical(&doc) && canonical(&doc));
        // A non-hex head cannot be compacted: stored in full, read exactly.
        let mut odd = compact_coverage(2);
        odd.heads = vec!["not-a-hash".into()];
        crate::coverage::put(&mut doc, odd.clone()).unwrap();
        assert_eq!(crate::coverage::entries(&doc).unwrap()[&key], odd);
        assert!(crate::coverage::canonical(&doc));
    }
    #[test]
    fn coverage_change_pins_a_current_receipt_to_its_own_coverage() {
        let mut doc = AutoCommit::new();
        let first = compact_coverage(1);
        crate::coverage::put_all_with_receipts(&mut doc, vec![first.clone()], |rows| {
            Ok(rows
                .iter()
                .map(|(key, row)| (key.clone(), DurableReceipt::new(row.clone(), 7).unwrap()))
                .collect())
        })
        .unwrap();
        let before = doc.clone();
        // Coverage advances without a new receipt (a non-durable publish).
        crate::coverage::put(&mut doc, compact_coverage(2)).unwrap();
        let key = route_key("app", "record");
        assert_eq!(
            entries(&doc).unwrap()[&key],
            DurableReceipt::new(first, 7).unwrap(),
            "the receipt still names the coverage it was issued for"
        );
        monotonic(Some(&before), &doc).unwrap();
        crate::coverage::monotonic(Some(&before), &doc).unwrap();
        assert!(canonical(&doc));
        assert_ne!(governed_bytes(&before), governed_bytes(&doc));
    }
    #[test]
    fn malformed_route_values_are_refused() {
        use automerge::{ObjType, ROOT, ScalarValue, transaction::Transactable};
        let key = route_key("app", "record");
        let coverage = compact_coverage(1);
        let receipt = DurableReceipt::new(coverage.clone(), 5).unwrap();
        let mut doc = AutoCommit::new();
        crate::coverage::put_all_with_receipts(&mut doc, vec![coverage.clone()], |rows| {
            Ok(rows
                .iter()
                .map(|(key, row)| (key.clone(), DurableReceipt::new(row.clone(), 5).unwrap()))
                .collect())
        })
        .unwrap();
        assert_eq!(entries(&doc).unwrap()[&key], receipt);
        // A bare postcard row is not a route value.
        let mut bare = AutoCommit::new();
        let map = bare
            .put_object(ROOT, crate::coverage::FIELD_V2, ObjType::Map)
            .unwrap();
        bare.put(
            &map,
            key.as_str(),
            ScalarValue::Bytes(postcard::to_allocvec(&coverage).unwrap()),
        )
        .unwrap();
        bare.commit();
        assert!(crate::coverage::entries(&bare).is_err());
        let mut bad = doc.clone();
        let (_, object) = automerge::ReadDoc::get(&bad, ROOT, FIELD_V2)
            .unwrap()
            .unwrap();
        bad.put(
            &object,
            key.as_str(),
            ScalarValue::Bytes(vec![0xff, 0xff, 0xff]),
        )
        .unwrap();
        bad.commit();
        assert!(entries(&bad).is_err());
    }
}
