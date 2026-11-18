//! The items a resource's collections hold, whatever their type, and what
//! each takes stored: what a store bounding the resources it keeps by the
//! items they hold counts and measures them by.

use std::any::Any;

use crate::Id;
use crate::table::ModelTable;

/// Each item the collections of the resource whose type `table` describes
/// hold: its id, its value and its type's table. Nothing is built per item
/// beyond its id.
pub fn each_item(
    table: &'static ModelTable,
    value: &dyn Any,
    found: &mut dyn FnMut(Id, &dyn Any, &'static ModelTable),
) {
    each_entry(table, value, &mut |_, id, item, table| {
        found(id, item, table);
    });
}

/// [`each_item`] with the name of the collection field holding each item,
/// in the order each collection keeps them.
pub fn each_entry<'a>(
    table: &'static ModelTable,
    value: &'a dyn Any,
    found: &mut dyn FnMut(&'static str, Id, &'a dyn Any, &'static ModelTable),
) {
    let Some(desc) = table.record() else {
        return;
    };
    for (index, field) in desc.fields.iter().enumerate() {
        let (Some(items), Some(inner)) = (field.items, (desc.field)(value, index)) else {
            continue;
        };
        let Some(list) = (items.list)(inner) else {
            continue;
        };
        for at in 0..list.count() {
            if let Some((id, item)) = list.item(at) {
                found(field.key, id, item, items.item_table);
            }
        }
    }
}

/// What `value`, of the type `table` describes, takes stored, in bytes, as
/// near as its JSON form says.
#[must_use]
pub fn stored_size(table: &'static ModelTable, value: &dyn Any) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(buf.len());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    let json = (table.ops.to_json)(table, value);
    match serde_json::to_writer(&mut count, &json) {
        Ok(()) => count.0,
        Err(_) => 0,
    }
}

/// `value`, of the type `table` describes, in its JSON form
/// ([`crate::Model::to_json`]).
#[must_use]
pub fn json(table: &'static ModelTable, value: &dyn Any) -> serde_json::Value {
    (table.ops.to_json)(table, value)
}
