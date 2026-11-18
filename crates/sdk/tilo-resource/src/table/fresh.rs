//! What a map made just now holds, made with it at once ([`Model::fresh`]).

use std::any::Any;

use super::{ModelTable, ResourceDesc, described, field_of, missing};
use crate::model::{Doc, Fresh, LocatedAt as _, Model, ModelError, Slot};

pub(super) fn fresh_of<T: Model>(
    _: &ModelTable,
    value: &dyn Any,
    encoding: automerge::TextEncoding,
) -> Fresh {
    value
        .downcast_ref::<T>()
        .map_or(Fresh::Store, |value| value.fresh(encoding))
}

/// What a type whose [`Model::fresh`] would not say stores: written by its
/// `store` once the map holding it is made.
pub(super) fn stored_after(_: &ModelTable, _: &dyn Any, _: automerge::TextEncoding) -> Fresh {
    Fresh::Store
}

/// What the fields of a map made just now hold, made with the map at once
/// ([`Model::fresh`]): the fields that can be, and, by position, those its
/// `store` writes once the map is made. A scalar field holding what its
/// absent key reads as ([`Model::missing`]) is left out: it reads the same.
pub(super) fn fresh_fields(
    desc: &ResourceDesc,
    value: &dyn Any,
    encoding: automerge::TextEncoding,
) -> Result<(automerge::hydrate::Map, Vec<usize>), ModelError> {
    let mut staged = (desc.empty)();
    let absent = missing(desc, staged.as_mut())
        .ok()
        .and_then(|()| (desc.unwrap)(staged.as_ref()));
    let mut fields = std::collections::HashMap::with_capacity(desc.fields.len());
    let mut later = Vec::new();
    for (index, field) in desc.fields.iter().enumerate() {
        let inner = field_of(desc, value, index)?;
        if field.table.scalar
            && absent
                .and_then(|absent| (desc.field)(absent, index))
                .is_some_and(|empty| (field.table.ops.stores_as)(field.table, inner, empty))
        {
            continue;
        }
        match (field.table.ops.fresh)(field.table, inner, encoding) {
            Fresh::Nothing => {}
            Fresh::Value(made) => {
                fields.insert(field.key.to_owned(), made);
            }
            Fresh::Store => later.push(index),
        }
    }
    Ok((fields.into(), later))
}

/// Write the fields of `desc` that `value` holds into a map made at `at` just
/// now: the map made holding every field that can be (its texts marked as
/// their writer's), then each other field stored into it.
pub(super) fn store_made(
    desc: &ResourceDesc,
    value: &dyn Any,
    doc: &mut Doc,
    at: Slot<'_>,
) -> Result<(), ModelError> {
    let (fields, later) = fresh_fields(desc, value, automerge::ReadDoc::text_encoding(doc))?;
    let obj = at.make(doc, &automerge::hydrate::Value::Map(fields))?;
    for index in later {
        let field = desc
            .fields
            .get(index)
            .ok_or_else(|| ModelError::invalid("a field out of range"))?;
        let inner = field_of(desc, value, index)?;
        (field.table.ops.store)(field.table, inner, doc, Slot::Key(&obj, field.key))
            .located(field.key)?;
    }
    Ok(())
}

/// [`Model::fresh`] of a nested map of fields: made with its parent when all
/// of them can be.
pub(super) fn fields_made(
    table: &ModelTable,
    value: &dyn Any,
    encoding: automerge::TextEncoding,
) -> Fresh {
    let Ok(desc) = described(table) else {
        return Fresh::Store;
    };
    match fresh_fields(desc, value, encoding) {
        Ok((fields, later)) if later.is_empty() => Fresh::Value(fields.into()),
        _ => Fresh::Store,
    }
}
