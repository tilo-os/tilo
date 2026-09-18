//! Governor-authenticated replay floors for exact retained checkpoints.
//! Carried in the existing room CRDT; this does not change neighbour v1 bytes.
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    let mut rows = entries(doc)?;
    rows.insert(route_key(&coverage.app, &coverage.record), coverage);
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
