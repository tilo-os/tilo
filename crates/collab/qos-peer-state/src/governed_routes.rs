//! Per-route storage of the governor's checkpoint coverage and durability
//! receipts in the room CRDT (v2 layout).
//!
//! The v1 layout kept each whole route map as ONE bytes scalar that the
//! governor rewrote on every durable commit. Automerge retains every
//! overwritten value, so the room history (and with it every bootstrap,
//! checkpoint and saved Store) grew by the whole map per generation. The v2
//! layout is a map object keyed by route whose values are the route's own
//! canonical postcard bytes, so a commit rewrites only the routes it changed.
//!
//! Reading is the union of a legacy v1 scalar (older rooms) and the v2 map,
//! v2 winning per route. The governor's first v2 write migrates: it copies
//! every row into the map and deletes the v1 scalar in the same change, so
//! monotonicity checks over `entries` see one continuous route set.
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use std::collections::BTreeMap;

/// The layout of one governed route map: its legacy scalar and v2 map keys.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub(crate) legacy: &'static str,
    pub(crate) routes: &'static str,
}

/// Legacy whole-map bytes, exactly as the v1 accessor read them: a present
/// non-bytes value reads as empty bytes (which then fails to decode).
pub(crate) fn legacy_bytes(doc: &AutoCommit, layout: Layout) -> Option<Vec<u8>> {
    match doc.get(ROOT, layout.legacy).ok().flatten() {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => Some(bytes.clone()),
            _ => Some(vec![]),
        },
        Some(_) => Some(vec![]),
        None => None,
    }
}

fn routes_object(doc: &AutoCommit, layout: Layout) -> Result<Option<ObjId>, String> {
    match doc.get(ROOT, layout.routes).map_err(|e| e.to_string())? {
        None => Ok(None),
        Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
        Some(_) => Err(format!("governed {} is not a route map", layout.routes)),
    }
}

/// Raw per-route bytes of the v2 map (winning values), if the map exists.
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

/// Deterministic identity of the whole governed value (both layouts), for
/// "did governance change" comparisons. `None` when neither is present.
pub(crate) fn identity(doc: &AutoCommit, layout: Layout) -> Option<Vec<u8>> {
    let legacy = legacy_bytes(doc, layout);
    let routes = match route_bytes(doc, layout) {
        Ok(routes) => routes.map(Ok),
        // A malformed map is its own (always different from valid) identity.
        Err(error) => Some(Err(error)),
    };
    if legacy.is_none() && routes.is_none() {
        return None;
    }
    postcard::to_allocvec(&(legacy, routes)).ok()
}

/// Write raw per-route values in one change: each `(layout, next)` pair
/// replaces that layout's complete route set with `next`, touching only
/// routes whose bytes differ and deleting routes absent from `next`; a
/// legacy scalar is migrated (every route written, scalar deleted). Commits
/// only when something changed; returns whether it did.
pub(crate) fn write_raw(
    doc: &mut AutoCommit,
    sets: &[(Layout, &BTreeMap<String, Vec<u8>>)],
) -> Result<bool, String> {
    let mut changed = false;
    for (layout, next) in sets {
        let migrate = legacy_bytes(doc, *layout).is_some();
        let current = if migrate {
            BTreeMap::new()
        } else {
            route_bytes(doc, *layout)?.unwrap_or_default()
        };
        if !migrate && current == **next && routes_object(doc, *layout)?.is_some() {
            continue;
        }
        let object = match routes_object(doc, *layout)? {
            Some(object) => object,
            None => doc
                .put_object(ROOT, layout.routes, ObjType::Map)
                .map_err(|e| e.to_string())?,
        };
        for (key, bytes) in next.iter() {
            if current.get(key) != Some(bytes) {
                doc.put(&object, key.as_str(), ScalarValue::Bytes(bytes.clone()))
                    .map_err(|e| e.to_string())?;
            }
        }
        for key in current.keys().filter(|key| !next.contains_key(*key)) {
            doc.delete(&object, key.as_str())
                .map_err(|e| e.to_string())?;
        }
        if migrate {
            doc.delete(ROOT, layout.legacy).map_err(|e| e.to_string())?;
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
        Ok((value, rest)) if rest.is_empty() => Some(value),
        _ => None,
    }
}
