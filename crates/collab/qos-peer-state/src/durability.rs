//! Versioned governor assertions, published only by a completed native storage barrier.
//! This metadata leaves the frozen neighbour and checkpoint-coverage encodings intact.
use crate::coverage::{Coverage, route_key};
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const FIELD: &str = "document_durability_receipts_v1";
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
            blake3::Hasher::new_derive_key("qOS durable checkpoint receipt v1")
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
pub fn governed_bytes(doc: &AutoCommit) -> Option<Vec<u8>> {
    match doc.get(ROOT, FIELD).ok().flatten() {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => Some(bytes.clone()),
            _ => Some(vec![]),
        },
        Some(_) => Some(vec![]),
        None => None,
    }
}
pub fn entries(doc: &AutoCommit) -> Result<BTreeMap<String, DurableReceipt>, String> {
    let rows: BTreeMap<String, DurableReceipt> = match governed_bytes(doc) {
        Some(bytes) => {
            postcard::from_bytes(&bytes).map_err(|e| format!("invalid durable receipts: {e}"))?
        }
        None => BTreeMap::new(),
    };
    for (key, receipt) in &rows {
        receipt.validate()?;
        if *key != route_key(&receipt.coverage.app, &receipt.coverage.record) {
            return Err("durable receipt route mismatch".into());
        }
    }
    Ok(rows)
}
pub fn put(doc: &mut AutoCommit, receipts: BTreeMap<String, DurableReceipt>) -> Result<(), String> {
    let bytes = postcard::to_allocvec(&receipts).map_err(|e| e.to_string())?;
    doc.put(ROOT, FIELD, ScalarValue::Bytes(bytes))
        .map_err(|e| e.to_string())?;
    doc.commit();
    Ok(())
}
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
        assert!(!crate::room::governance_change_allowed(
            Some(&before),
            &after,
            false,
            Some("authority"),
            "viewer"
        ));
        assert!(crate::room::governance_change_allowed(
            Some(&before),
            &after,
            false,
            Some("authority"),
            "authority"
        ));
    }
}
