//! What an item's address says of it.

/// The kind an item published under `resource_id` has, `<kind>.<field>`,
/// read from its id, `<kind>/<field>/<item>`; `None` for an id naming no
/// item of a top-level collection.
pub fn item_kind(resource_id: &str) -> Option<String> {
    let (collection, item) = resource_id.rsplit_once('/')?;
    let (kind, field) = collection.split_once('/')?;
    (!kind.is_empty() && !field.is_empty() && !field.contains('/') && !item.is_empty())
        .then(|| format!("{kind}.{field}"))
}
