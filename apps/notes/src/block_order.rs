//! The order a first-format page showed its blocks in: independent of the
//! list owning each block, by the fractional rank each block stored.
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use ui::tilo_resource::order::bytes::cmp as cmp_keys;
use uuid::Uuid;

pub(crate) const FIELD: &str = "notes_order";
pub(crate) const BASE_FIELD: &str = "notes_order_base";

pub(crate) fn stored(doc: &AutoCommit, block: &ObjId) -> Option<Vec<u8>> {
    let value = doc
        .get(block, FIELD)
        .ok()
        .flatten()
        .or_else(|| doc.get(block, BASE_FIELD).ok().flatten())?;
    match value.0 {
        Value::Scalar(value) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => Some(bytes.clone()),
            _ => None,
        },
        Value::Object(_) => None,
    }
}

fn identity(doc: &AutoCommit, block: &ObjId) -> Option<Uuid> {
    doc.get(block, "id")
        .ok()
        .flatten()?
        .0
        .as_str()?
        .parse()
        .ok()
}

pub(crate) fn initial(index: usize, id: Uuid) -> Vec<u8> {
    let mut rank = u64::try_from(index)
        .unwrap_or(u64::MAX)
        .to_be_bytes()
        .to_vec();
    rank.push(128);
    rank.extend_from_slice(id.as_bytes());
    rank.push(1);
    rank
}

pub(crate) fn valid(rank: &[u8]) -> bool {
    rank.len() >= 18 && rank.last() == Some(&1)
}

pub(crate) fn objects(doc: &AutoCommit, list: &ObjId) -> Vec<ObjId> {
    let mut records = (0..doc.length(list))
        .filter_map(|index| {
            let (Value::Object(ObjType::Map), object) = doc.get(list, index).ok().flatten()? else {
                return None;
            };
            let id = identity(doc, &object).unwrap_or(Uuid::nil());
            let rank = stored(doc, &object).unwrap_or_else(|| initial(index, id));
            Some((rank, id, object))
        })
        .collect::<Vec<_>>();
    records.sort_by(|a, b| cmp_keys(&a.0, &b.0).then(a.1.cmp(&b.1)));
    records.into_iter().map(|(_, _, object)| object).collect()
}
