//! Per-route storage of the governor's checkpoint coverage and durability
//! receipts in the room CRDT.
//!
//! Each governed value is a map object keyed by route whose values are the
//! route's own canonical postcard bytes, so a commit rewrites only the routes
//! it changed instead of growing the room history by the whole map.
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use std::collections::BTreeMap;

/// The layout of one governed route map: its room map key.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub(crate) routes: &'static str,
}

fn routes_object(doc: &AutoCommit, layout: Layout) -> Result<Option<ObjId>, String> {
    match doc.get(ROOT, layout.routes).map_err(|e| e.to_string())? {
        None => Ok(None),
        Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
        Some(_) => Err(format!("governed {} is not a route map", layout.routes)),
    }
}

/// Raw per-route bytes of the map (winning values), if the map exists.
pub(crate) fn route_bytes(
    doc: &AutoCommit,
    layout: Layout,
) -> Result<Option<BTreeMap<String, Vec<u8>>>, String> {
    let Some(object) = routes_object(doc, layout)? else {
        return Ok(None);
    };
    let mut rows = BTreeMap::new();
    for key in doc.keys(&object) {
        match doc.get(&object, key.as_str()).map_err(|e| e.to_string())? {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => {
                    rows.insert(key, bytes.clone());
                }
                _ => return Err(format!("governed {} route is not bytes", layout.routes)),
            },
            _ => return Err(format!("governed {} route is not bytes", layout.routes)),
        }
    }
    Ok(Some(rows))
}

/// Deterministic identity of the whole governed value, for "did governance
/// change" comparisons. `None` when the map is absent.
pub(crate) fn identity(doc: &AutoCommit, layout: Layout) -> Option<Vec<u8>> {
    let routes = match route_bytes(doc, layout) {
        Ok(routes) => Ok(routes?),
        // A malformed map is its own (always different from valid) identity.
        Err(error) => Err(error),
    };
    postcard::to_allocvec(&routes).ok()
}

/// Write raw per-route values in one change: each `(layout, next)` pair
/// replaces that layout's complete route set with `next`, touching only
/// routes whose bytes differ and deleting routes absent from `next`. Commits
/// only when something changed; returns whether it did.
pub(crate) fn write_raw(
    doc: &mut AutoCommit,
    sets: &[(Layout, &BTreeMap<String, Vec<u8>>)],
) -> Result<bool, String> {
    let mut changed = false;
    for (layout, next) in sets {
        let current = route_bytes(doc, *layout)?.unwrap_or_default();
        if current == **next && routes_object(doc, *layout)?.is_some() {
            continue;
        }
        let object = match routes_object(doc, *layout)? {
            Some(object) => object,
            None => doc
                .put_object(ROOT, layout.routes, ObjType::Map)
                .map_err(|e| e.to_string())?,
        };
        for (key, bytes) in *next {
            if current.get(key) != Some(bytes) {
                doc.put(&object, key.as_str(), ScalarValue::Bytes(bytes.clone()))
                    .map_err(|e| e.to_string())?;
            }
        }
        for key in current.keys().filter(|key| !next.contains_key(*key)) {
            doc.delete(&object, key.as_str())
                .map_err(|e| e.to_string())?;
        }
        changed = true;
    }
    if changed {
        doc.commit();
    }
    Ok(changed)
}

/// Decode `bytes` as exactly one `T` (no trailing bytes).
pub(crate) fn exact<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    match postcard::take_from_bytes::<T>(bytes) {
        Ok((value, [])) => Some(value),
        _ => None,
    }
}
