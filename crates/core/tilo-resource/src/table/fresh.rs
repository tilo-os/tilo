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
/// `store` writes once the map is made, with whether the field itself was
/// made with the map ([`Fresh::Made`]). A scalar field holding what its
/// absent key reads as ([`Model::missing`]) is left out: it reads the same.
pub(super) fn fresh_fields(
    desc: &ResourceDesc,
    value: &dyn Any,
    encoding: automerge::TextEncoding,
) -> Result<super::Fields, ModelError> {
    let mut staged = (desc.empty)();
    let absent = missing(desc, staged.as_mut())
        .ok()
        .and_then(|()| (desc.unwrap)(staged.as_ref()));
    let mut fields = std::collections::HashMap::with_capacity(desc.fields.len());
    let mut later: Vec<(usize, bool)> = Vec::new();
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
            Fresh::Made(made) => {
                fields.insert(field.key.to_owned(), made);
                later.push((index, true));
            }
            Fresh::Store => later.push((index, false)),
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
    write_later(desc, value, doc, &obj, &later)
}

/// Store the fields `later`, by position, of `value` into the map `obj` made
/// with them just now: each as a map made with it, which its own
/// store starts from.
pub(crate) fn write_later(
    desc: &ResourceDesc,
    value: &dyn Any,
    doc: &mut Doc,
    obj: &automerge::ObjId,
    later: &[(usize, bool)],
) -> Result<(), ModelError> {
    for &(index, made) in later {
        let field = desc
            .fields
            .get(index)
            .ok_or_else(|| ModelError::invalid("a field out of range"))?;
        let inner = field_of(desc, value, index)?;
        let mut store = || {
            (field.table.ops.store)(field.table, inner, doc, Slot::Key(obj, field.key))
                .located(field.key)
        };
        if made {
            crate::model::storing_made_around(store)?;
        } else {
            store()?;
        }
    }
    Ok(())
}

/// The fields `desc` describes of `value`, made with a map holding
/// it in one batch, and what its `store` writes into it after.
pub(crate) fn fields_made_of(
    desc: &ResourceDesc,
    value: &dyn Any,
    encoding: automerge::TextEncoding,
) -> Option<super::Fields> {
    fresh_fields(desc, value, encoding).ok()
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

/// [`Model::fresh`] of a list: made with its parent, holding its items, when
/// every item can be made so.
pub(super) fn list_made(
    table: &ModelTable,
    value: &dyn Any,
    encoding: automerge::TextEncoding,
) -> Fresh {
    let Some(list) = table.list() else {
        return Fresh::Store;
    };
    let Some(items) = (list.items)(value) else {
        return Fresh::Store;
    };
    let mut made = Vec::with_capacity(items.count());
    for at in 0..items.count() {
        let Some(item) = items.item_at(at) else {
            return Fresh::Store;
        };
        match (list.item.ops.fresh)(list.item, item, encoding) {
            Fresh::Value(value) => made.push(value),
            Fresh::Nothing | Fresh::Made(_) | Fresh::Store => return Fresh::Store,
        }
    }
    Fresh::Value(automerge::hydrate::Value::List(made.into()))
}

/// [`Model::fresh`] of a value stored as JSON: made with its parent as
/// `store` writes it, a structural value's maps, lists and scalars or its
/// JSON text.
pub(super) fn json_made(
    table: &ModelTable,
    value: &dyn Any,
    encoding: automerge::TextEncoding,
) -> Fresh {
    let Some(json) = table.json() else {
        return Fresh::Store;
    };
    if json.structural {
        return (json.try_value)(value).map_or(Fresh::Store, |value| value.fresh(encoding));
    }
    super::json_text(json, value).map_or(Fresh::Store, |text| {
        Fresh::scalar(automerge::ScalarValue::Str(text.into()))
    })
}
