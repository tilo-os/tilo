//! An item of a record found by the path a resource id names: whether the record holds it, and
//! what it reads as, without building the record's published tree.

use std::any::Any;

use super::{FieldDesc, ModelTable, ResourceDesc};

/// Whether the record `value` (described by `record`) holds an item at the
/// end of `path`: collection fields by their Rust names, the outer ones
/// looked into item by item over their first published page, as a
/// publication lists them.
#[must_use]
pub fn holds_nested_items(record: &ResourceDesc, value: &dyn Any, path: &[&str]) -> bool {
    let Some((name, rest)) = path.split_first() else {
        return false;
    };
    let Some((index, field)) = record
        .fields
        .iter()
        .enumerate()
        .find(|(_, field)| field.items.is_some() && field.name == *name)
    else {
        return false;
    };
    let (Some(items), Some(inner)) = (field.items, (record.field)(value, index)) else {
        return false;
    };
    if rest.is_empty() {
        return (items.item_presence)(inner) > 0;
    }
    let (Some(list), Some(item)) = ((items.list)(inner), items.item_table.record()) else {
        return false;
    };
    (0..list.count().min(crate::items::ITEM_PAGE)).any(|at| {
        list.item(at)
            .is_some_and(|(_, value)| holds_nested_items(item, value, rest))
    })
}

/// One step from a record toward a published part of it: into an item of a
/// collection, or into one of its fields.
enum Step<'a> {
    Item(&'a str, &'a str),
    Field(&'a str),
}

/// The steps `resource_id` takes into a record of the kind `kind`: pairs of
/// a collection and an item (`<kind>/<field>/<item>/<field>/<item>`), then
/// at most one field of the item reached (`…/<item>/<field>`, an issue's
/// body).
fn steps<'a>(kind: &str, resource_id: &'a str) -> Option<Vec<Step<'a>>> {
    let path = resource_id.strip_prefix(kind)?.strip_prefix('/')?;
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 2 || segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    let mut steps: Vec<Step<'a>> = segments
        .chunks(2)
        .filter_map(|pair| match pair {
            [field, item] => Some(Step::Item(field, item)),
            _ => None,
        })
        .collect();
    if segments.len() % 2 == 1 {
        steps.push(Step::Field(segments.last()?));
    }
    Some(steps)
}

/// The position and description of the field named `name` of `desc`.
fn field_named<'a>(desc: &'a ResourceDesc, name: &str) -> Option<(usize, &'a FieldDesc)> {
    desc.fields
        .iter()
        .enumerate()
        .find(|(_, desc)| desc.name == name)
}

/// What the published part of a record `resource_id` names holds, as its
/// own model's JSON ([`Model::to_json`]): the document read of an item of a
/// collection, or of a field of one (an issue's body), whatever their types,
/// found in `record` (of the table `table`) by the steps
/// `<kind>/<field>/<item>`, `…/<field>/<item>` for an item of a collection
/// inside an item, and a last `…/<field>` for a field of the item reached.
///
/// # Errors
/// Why there is nothing to answer: the id names no published part of a
/// `kind`, or the record holds no such item.
pub fn item_json(
    table: &'static ModelTable,
    record: &dyn Any,
    kind: &str,
    resource_id: &str,
) -> Result<serde_json::Value, String> {
    let steps = steps(kind, resource_id)
        .ok_or_else(|| format!("{resource_id} names no item of a {kind}"))?;
    let (mut table, mut value) = (table, record);
    for step in steps {
        let record = table
            .record()
            .ok_or_else(|| format!("{resource_id}: what it names a part of is no record"))?;
        match step {
            Step::Item(field, item) => {
                let (index, items) = field_named(record, field)
                    .and_then(|(index, desc)| Some((index, desc.items?)))
                    .ok_or_else(|| {
                        format!("{resource_id}: {field} is no collection of published items")
                    })?;
                let list = (record.field)(value, index)
                    .and_then(|collection| (items.list)(collection))
                    .ok_or_else(|| format!("{resource_id}: {field} is not readable"))?;
                value = (0..list.count())
                    .filter_map(|at| list.item(at))
                    .find_map(|(id, found)| (id.as_str() == item).then_some(found))
                    .ok_or_else(|| format!("{resource_id}: there is no such item"))?;
                table = items.item_table;
            }
            Step::Field(field) => {
                let (index, desc) = field_named(record, field)
                    .ok_or_else(|| format!("{resource_id}: there is no field {field}"))?;
                value = (record.field)(value, index)
                    .ok_or_else(|| format!("{resource_id}: {field} is not readable"))?;
                table = desc.table;
            }
        }
    }
    Ok((table.ops.to_json)(table, value))
}

/// Whether `resource_id` names a part [`item_json`] reads in a record of the
/// table `table` and kind `kind`: each step an item of a collection of
/// published items, or a last field, of the record the step before reached.
/// Decided from the tables alone, so offering the read costs no read.
#[must_use]
pub fn reads_item(table: &'static ModelTable, kind: &str, resource_id: &str) -> bool {
    let Some(steps) = steps(kind, resource_id) else {
        return false;
    };
    let mut table = table;
    for step in steps {
        let Some(record) = table.record() else {
            return false;
        };
        table = match step {
            Step::Item(field, _) => {
                match field_named(record, field).and_then(|(_, desc)| desc.items) {
                    Some(items) => items.item_table,
                    None => return false,
                }
            }
            Step::Field(field) => match field_named(record, field) {
                Some((_, desc)) => desc.table,
                None => return false,
            },
        };
    }
    true
}

/// Whether `resource_id` names an item of a collection of published items in
/// a record of the table `table` and kind `kind`, an item inside an item
/// included, rather than a part of an item (an issue's body, a channel's
/// call): what may be opened as an item.
#[must_use]
pub fn names_item(table: &'static ModelTable, kind: &str, resource_id: &str) -> bool {
    steps(kind, resource_id).is_some_and(|steps| matches!(steps.last(), Some(Step::Item(..))))
        && reads_item(table, kind, resource_id)
}
