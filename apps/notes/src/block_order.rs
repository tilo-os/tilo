//! Presentation order is independent of the list owning each collaborative Text.
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use ui::doc::fractional_order::{cmp_keys, key_between};
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
        .to_str()?
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

/// Distinct blocks inserted into the same gap get distinct, still-subdividable
/// fractions. Choose a short fractional cell contained in the gap, then append
/// the UUID inside that cell. Copying complete neighboring keys would grow the
/// position by another UUID on every insertion; appending directly to a midpoint
/// can instead overshoot the upper bound.
pub(crate) fn between(lo: Option<&[u8]>, hi: Option<&[u8]>, id: Uuid) -> Result<Vec<u8>, String> {
    if let (Some(lo), Some(hi)) = (lo, hi)
        && !cmp_keys(lo, hi).is_lt()
    {
        return Err("block order bounds are not distinct".into());
    }
    let mut rank = if hi.is_none() {
        // Round upward at the earliest possible digit. Appending a fresh UUID
        // after the previous key's final digit would grow every append by 17 bytes.
        let lo = lo.unwrap_or(&[]);
        let mut prefix = Vec::new();
        for &digit in lo {
            if digit < 255 {
                prefix.push(digit + 1);
                break;
            }
            prefix.push(255);
        }
        if prefix.len() == lo.len() && cmp_keys(&prefix, lo).is_eq() {
            prefix.push(128);
        }
        prefix
    } else if let Some(hi) = hi {
        let midpoint = key_between(lo, Some(hi));
        let limit = midpoint.len().max(hi.len()).max(lo.map_or(0, <[u8]>::len)) + 1;
        let mut chosen = None;
        for width in 1..=limit {
            let mut prefix = midpoint.iter().copied().take(width).collect::<Vec<_>>();
            prefix.resize(width, 0);
            if lo.is_some_and(|lo| cmp_keys(&prefix, lo).is_lt()) {
                continue;
            }
            let Some(upper) = cell_upper(&prefix) else {
                continue;
            };
            if !cmp_keys(&upper, hi).is_gt() {
                chosen = Some(prefix);
                break;
            }
        }
        chosen.ok_or("block order gap has no finite cell")?
    } else {
        return Err("block order upper bound is missing".into());
    };
    rank.extend_from_slice(id.as_bytes());
    rank.push(1);
    Ok(rank)
}

/// Upper edge of the base-256 cell having this prefix; None denotes 1.0.
fn cell_upper(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper = prefix.to_vec();
    let index = upper.iter().rposition(|digit| *digit < 255)?;
    let digit = upper.get_mut(index)?;
    *digit = digit.saturating_add(1);
    upper.truncate(index + 1);
    Some(upper)
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
