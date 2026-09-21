//! Governor-authenticated replay floors for exact retained checkpoints.
//! Carried in the existing room CRDT; this does not change neighbour v1 bytes.
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const FIELD: &str = "document_checkpoint_coverage_v1";
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
pub fn entries(doc: &AutoCommit) -> Result<BTreeMap<String, Coverage>, String> {
    match governed_bytes(doc) {
        Some(bytes) => {
            postcard::from_bytes(&bytes).map_err(|e| format!("invalid checkpoint coverage: {e}"))
        }
        None => Ok(BTreeMap::new()),
    }
}
pub fn route_key(app: &str, record: &str) -> String {
    format!("{app}\0{record}")
}
pub fn put(doc: &mut AutoCommit, coverage: Coverage) -> Result<(), String> {
    put_all(doc, vec![coverage])
}
pub(crate) fn put_all(doc: &mut AutoCommit, coverages: Vec<Coverage>) -> Result<(), String> {
    if coverages.is_empty() {
        return Ok(());
    }
    let mut rows = entries(doc)?;
    let mut incoming = BTreeSet::new();
    for coverage in coverages {
        let route = route_key(&coverage.app, &coverage.record);
        if !incoming.insert(route.clone()) {
            return Err("duplicate checkpoint coverage route in batch".into());
        }
        rows.insert(route, coverage);
    }
    let bytes = postcard::to_allocvec(&rows).map_err(|e| e.to_string())?;
    doc.put(ROOT, FIELD, ScalarValue::Bytes(bytes))
        .map_err(|e| e.to_string())?;
    doc.commit();
    Ok(())
}
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
