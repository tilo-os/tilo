//! Per-route storage of the workspace's checkpoint coverage and durability
//! receipts in the workspace CRDT.
//!
//! Each stored value is a map object keyed by route whose values are the
//! route's own canonical postcard bytes, so a commit rewrites only the routes
//! it changed instead of growing the workspace history by the whole map.
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use std::collections::BTreeMap;

/// The layout of one route map: its workspace map key.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub(crate) routes: &'static str,
}

fn routes_object(doc: &AutoCommit, layout: Layout) -> Result<Option<ObjId>, String> {
    match doc.get(ROOT, layout.routes).map_err(|e| e.to_string())? {
        None => Ok(None),
        Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
        Some(_) => Err(format!("{} is not a route map", layout.routes)),
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
                _ => return Err(format!("{} route is not bytes", layout.routes)),
            },
            _ => return Err(format!("{} route is not bytes", layout.routes)),
        }
    }
    Ok(Some(rows))
}

/// The raw bytes of the route `key` (its winning value), if the map holds it.
pub(crate) fn route(
    doc: &AutoCommit,
    layout: Layout,
    key: &str,
) -> Result<Option<Vec<u8>>, String> {
    let Some(object) = routes_object(doc, layout)? else {
        return Ok(None);
    };
    match doc.get(&object, key).map_err(|e| e.to_string())? {
        None => Ok(None),
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => Ok(Some(bytes.clone())),
            _ => Err(format!("{} route is not bytes", layout.routes)),
        },
        Some(_) => Err(format!("{} route is not bytes", layout.routes)),
    }
}

/// Write the routes of `routes` leaving them pending, as [`write_raw`] does, and leave every
/// other route as it is: a writer that changes a few routes reads and writes only those.
///
/// # Errors
///
/// The workspace was founded without the layout's map, or the document refuses
/// the write.
pub(crate) fn put_routes(
    doc: &mut AutoCommit,
    layout: Layout,
    routes: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    if routes.is_empty() {
        return Ok(());
    }
    let object = routes_object(doc, layout)?.ok_or_else(|| {
        format!(
            "the workspace was founded without its `{}` routes",
            layout.routes
        )
    })?;
    for (key, bytes) in routes {
        if route(doc, layout, key)?.as_ref() != Some(bytes) {
            doc.put(&object, key.as_str(), ScalarValue::Bytes(bytes.clone()))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Deterministic identity of the whole map, for "did governance
/// change" comparisons. `None` when the map is absent.
pub(crate) fn identity(doc: &AutoCommit, layout: Layout) -> Option<Vec<u8>> {
    let routes = match route_bytes(doc, layout) {
        Ok(routes) => Ok(routes?),
        // A malformed map is its own (always different from valid) identity.
        Err(error) => Err(error),
    };
    postcard::to_allocvec(&routes).ok()
}

/// Write raw per-route values leaving them pending, for the writer to commit as one governance
/// change: each `(layout, next)` pair replaces that layout's complete route set with `next`,
/// touching only routes whose bytes differ and deleting routes absent from `next`. Returns whether
/// something changed.
///
/// # Errors
///
/// The workspace was founded without the layout's map, or the document refuses
/// the write.
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
        let object = routes_object(doc, *layout)?.ok_or_else(|| {
            format!(
                "the workspace was founded without its `{}` routes",
                layout.routes
            )
        })?;
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
    Ok(changed)
}

/// Decode `bytes` as exactly one `T` (no trailing bytes).
pub(crate) fn exact<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    match postcard::take_from_bytes::<T>(bytes) {
        Ok((value, [])) => Some(value),
        _ => None,
    }
}
