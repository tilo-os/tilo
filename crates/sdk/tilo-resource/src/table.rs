//! A model type's storage contract as data.
//!
//! What a record does with its fields — read them, write them, compare them,
//! edit, check and describe them — is the same walk for every record, over
//! each field's own type. `#[derive(Model)]` describes a record as a table
//! of its fields ([`RecordDesc`]) and three small functions reaching into it
//! (a field by position, the same mutably, and building one from its
//! fields), and every [`Model`] operation of a record is one function here,
//! compiled once into the runtime. A field's type is reached through its
//! [`ModelTable`]: the type's [`Model`] functions with the type erased,
//! built once per type, and for the types this crate defines, once.

use std::any::Any;
use std::marker::PhantomData;

use automerge::{ObjId, ObjType, Value};

use crate::action::{ActionCx, Refusal};
use crate::delta::{Delta, Touch};
use crate::items::{ItemCollection, ItemSource, ItemText};
use crate::model::{Doc, LocatedAt as _, Model, ModelError, Slot};
use crate::path::{Edit, EditOp, OpDescription, Segment};
use crate::resource::Place;

/// A model type's [`Model`] functions, its type erased. Each takes the table
/// it was reached through, which for a record carries its [`RecordDesc`].
/// A value is handed over as `&dyn Any` of the type; a value made is put
/// into an `&mut dyn Any` of `Option` of the type.
#[allow(
    clippy::type_complexity,
    reason = "each field is one Model function, with the same parameters"
)]
pub struct ModelOps {
    to_json: fn(&ModelTable, &dyn Any) -> serde_json::Value,
    from_json: fn(&ModelTable, &serde_json::Value, &mut dyn Any) -> Result<(), ModelError>,
    load: fn(&ModelTable, &Doc, &Value<'_>, &ObjId, &mut dyn Any) -> Result<(), ModelError>,
    missing: fn(&ModelTable, &mut dyn Any) -> Result<(), ModelError>,
    /// Read the value anew in place: from what a slot holds, or its missing
    /// value when the slot holds nothing.
    reload:
        fn(&ModelTable, &mut dyn Any, &Doc, Option<(&Value<'_>, &ObjId)>) -> Result<(), ModelError>,
    store: fn(&ModelTable, &dyn Any, &mut Doc, Slot<'_>) -> Result<(), ModelError>,
    stores_as: fn(&ModelTable, &dyn Any, &dyn Any) -> bool,
    store_changed:
        fn(&ModelTable, &dyn Any, &dyn Any, &mut Doc, Slot<'_>, &Delta) -> Result<(), ModelError>,
    refresh:
        fn(&ModelTable, &mut dyn Any, &Doc, &Value<'_>, &ObjId, &Delta) -> Result<(), ModelError>,
    read_whole: fn(&ModelTable, &mut dyn Any, &Doc) -> Result<(), ModelError>,
    edit: fn(
        &ModelTable,
        &mut Doc,
        Slot<'_>,
        &[Segment],
        &EditOp,
        &mut ActionCx,
    ) -> Result<(), ModelError>,
    diff: fn(&ModelTable, &dyn Any, &dyn Any, &[Segment], &mut Vec<Edit<()>>),
    collect_text: fn(&ModelTable, &dyn Any, &mut Vec<String>),
    check: fn(&ModelTable, &dyn Any) -> Result<(), Refusal>,
    get_at: for<'a> fn(&ModelTable, &'a dyn Any, &[Segment]) -> Option<&'a dyn Any>,
    op_coalesces:
        fn(&ModelTable, &[Segment], &serde_json::Value, &serde_json::Value) -> Option<bool>,
    op_continuable: fn(&ModelTable, &[Segment], &serde_json::Value) -> Option<bool>,
    op_describe: fn(&ModelTable, &[Segment], &serde_json::Value) -> Option<OpDescription>,
    op_lands: fn(&ModelTable, &dyn Any, &[Segment], &serde_json::Value) -> Option<Place>,
    op_schemas: fn(&ModelTable, &str, &mut Vec<(String, serde_json::Value)>),
}

/// A model type as data: its erased [`Model`] functions, what it is (a
/// record's fields), and what it holds (a collection's items).
pub struct ModelTable {
    ops: &'static ModelOps,
    record: Option<&'static RecordDesc>,
    /// Its [`Model::CHECKS`].
    pub checks: bool,
    /// Its [`Model::OPS`].
    pub has_ops: bool,
}

/// A collection type's [`ItemSource`] functions, its type erased.
pub struct ItemOps {
    item_collection: fn(&dyn Any, &'static str) -> Option<ItemCollection>,
    item_presence: fn(&dyn Any) -> usize,
    label_of_item: fn(&dyn Any, &crate::Id) -> Option<String>,
    item_texts: fn(&dyn Any, &'static str, &mut Vec<ItemText>),
    nested_items: fn(&dyn Any, &'static str, &mut Vec<ItemCollection>),
}

/// One field of a record, as [`RecordDesc`] lists it.
pub struct FieldDesc {
    /// The key it is stored under.
    pub key: &'static str,
    /// Its Rust name.
    pub name: &'static str,
    /// Its type's table.
    pub table: &'static ModelTable,
    /// Changed only by the record's verbs (`#[model(readonly)]`).
    pub readonly: bool,
    /// Read all of it whenever it is read (`#[model(whole)]`).
    pub whole: bool,
    /// Its items, for a collection whose items are published.
    pub items: Option<&'static ItemOps>,
}

/// Read a record, boxed, from the map it is stored in.
pub type ReadRecord = fn(&Doc, &ObjId) -> Result<Box<dyn Any>, ModelError>;

/// A record's own invariant (`#[model(check = path)]`).
pub struct OwnCheck {
    /// Check a record.
    pub check: fn(&dyn Any) -> Result<(), Refusal>,
    /// Read a record from the map it is stored in, to check it after an
    /// edit of one of its fields.
    pub read: ReadRecord,
}

/// A record type as `#[derive(Model)]` describes it. See the [module
/// docs](self).
pub struct RecordDesc {
    /// Its stored fields, in declaration order.
    pub fields: &'static [FieldDesc],
    /// The field at a position of a record.
    pub field: for<'a> fn(&'a dyn Any, usize) -> Option<&'a dyn Any>,
    /// The same, mutably.
    pub field_mut: for<'a> fn(&'a mut dyn Any, usize) -> Option<&'a mut dyn Any>,
    /// A record from its fields, in order, put into an `Option` of it.
    pub build: fn(&mut dyn FieldSource, &mut dyn Any) -> Result<(), ModelError>,
    /// An empty `Option` of the record, for [`Self::build`] to fill.
    pub empty: fn() -> Box<dyn Any>,
    /// The record in a filled `Option` of it.
    pub unwrap: fn(&dyn Any) -> Option<&dyn Any>,
    /// Move the record out of a filled `Option` (the second) over a record
    /// (the first).
    pub replace: fn(&mut dyn Any, &mut dyn Any) -> Result<(), ModelError>,
    /// Its own invariant.
    pub own_check: Option<OwnCheck>,
    /// The position of its `#[model(label)]` field.
    pub label: Option<usize>,
    /// The positions of the fields its label joins (`#[model(label = "..")]`).
    pub label_parts: &'static [usize],
    /// Its type's name: what a deferred check is known by.
    pub name: fn() -> &'static str,
    /// Its stored shape.
    pub shape: crate::Shape,
    /// Its [`Model::CHECKS`].
    pub checks: bool,
    /// Its [`Model::OPS`].
    pub has_ops: bool,
}

/// Where a record being built takes its fields from, one after the other.
pub trait FieldSource {
    /// The next field's value, put into `out`, an `Option` of its type.
    ///
    /// # Errors
    /// Why the field does not read, located at it.
    fn next(&mut self, out: &mut dyn Any) -> Result<(), ModelError>;

    /// The field just read: read all of it (`#[model(whole)]`) and move on.
    ///
    /// # Errors
    /// Why it does not read whole, located at it.
    fn read(&mut self, value: &mut dyn Any) -> Result<(), ModelError>;
}

/// The next field of `source`, a `T`.
///
/// # Errors
/// Why it does not read.
pub fn take<T: 'static>(source: &mut dyn FieldSource) -> Result<T, ModelError> {
    let mut out: Option<T> = None;
    source.next(&mut out)?;
    let Some(mut value) = out else {
        return Err(mismatch());
    };
    source.read(&mut value)?;
    Ok(value)
}

/// Put `value` into `out`, an `Option` of its type.
///
/// # Errors
/// When `out` is of another type.
pub fn put<T: 'static>(out: &mut dyn Any, value: T) -> Result<(), ModelError> {
    match out.downcast_mut::<Option<T>>() {
        Some(slot) => {
            *slot = Some(value);
            Ok(())
        }
        None => Err(mismatch()),
    }
}

/// An empty `Option` of `T`.
#[must_use]
pub fn empty_of<T: 'static>() -> Box<dyn Any> {
    Box::new(None::<T>)
}

/// The `T` in a filled `Option` of it.
#[must_use]
pub fn unwrap_of<T: 'static>(filled: &dyn Any) -> Option<&dyn Any> {
    match filled.downcast_ref::<Option<T>>()? {
        Some(value) => Some(value),
        None => None,
    }
}

/// Move the `T` out of `filled`, an `Option` of it, over `target`.
///
/// # Errors
/// When either is of another type, or `filled` is empty.
pub fn replace_of<T: 'static>(
    target: &mut dyn Any,
    filled: &mut dyn Any,
) -> Result<(), ModelError> {
    let value = filled
        .downcast_mut::<Option<T>>()
        .and_then(Option::take)
        .ok_or_else(mismatch)?;
    *target.downcast_mut::<T>().ok_or_else(mismatch)? = value;
    Ok(())
}

/// A value handed to a table of another type: a bug in the caller.
#[inline(never)]
fn mismatch() -> ModelError {
    ModelError::invalid("a value of another type")
}

// --- Tables of the types with their own `Model` functions ----------------

/// The table of `T`, from its own [`Model`] functions.
pub struct TableOf<T>(PhantomData<T>);

/// `TABLE` of a type this crate defines, inside its `impl Model`: built
/// here, once, rather than in every app holding a field of it.
#[doc(hidden)]
#[macro_export]
macro_rules! __own_table {
    ($ty:ty) => {
        const TABLE: &'static $crate::table::ModelTable = {
            static TABLE: $crate::table::ModelTable = $crate::table::TableOf::<$ty>::TABLE;
            &TABLE
        };
    };
}

impl<T: Model> TableOf<T> {
    /// `T`'s functions.
    pub const OPS: ModelOps = ModelOps {
        to_json: to_json_of::<T>,
        from_json: from_json_of::<T>,
        load: load_of::<T>,
        missing: missing_of::<T>,
        reload: reload_of::<T>,
        store: store_of::<T>,
        stores_as: stores_as_of::<T>,
        store_changed: store_changed_of::<T>,
        refresh: refresh_of::<T>,
        read_whole: read_whole_of::<T>,
        edit: edit_of::<T>,
        diff: diff_of::<T>,
        collect_text: collect_text_of::<T>,
        check: if T::CHECKS { check_of::<T> } else { no_check },
        get_at: get_at_of::<T>,
        op_coalesces: if T::OPS {
            op_coalesces_of::<T>
        } else {
            no_coalesces
        },
        op_continuable: if T::OPS {
            op_continuable_of::<T>
        } else {
            no_continuable
        },
        op_describe: if T::OPS {
            op_describe_of::<T>
        } else {
            no_describe
        },
        op_lands: if T::OPS { op_lands_of::<T> } else { no_lands },
        op_schemas: if T::OPS {
            op_schemas_of::<T>
        } else {
            no_schemas
        },
    };

    /// `T`'s table.
    pub const TABLE: ModelTable = ModelTable {
        ops: &Self::OPS,
        record: None,
        checks: T::CHECKS,
        has_ops: T::OPS,
    };
}

fn to_json_of<T: Model>(_: &ModelTable, value: &dyn Any) -> serde_json::Value {
    value
        .downcast_ref::<T>()
        .map_or(serde_json::Value::Null, Model::to_json)
}

fn from_json_of<T: Model>(
    _: &ModelTable,
    value: &serde_json::Value,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    put(out, T::from_json(value)?)
}

fn load_of<T: Model>(
    _: &ModelTable,
    doc: &Doc,
    value: &Value<'_>,
    id: &ObjId,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    put(out, T::load(doc, value, id)?)
}

fn missing_of<T: Model>(_: &ModelTable, out: &mut dyn Any) -> Result<(), ModelError> {
    put(out, T::missing()?)
}

fn reload_of<T: Model>(
    _: &ModelTable,
    target: &mut dyn Any,
    doc: &Doc,
    slot: Option<(&Value<'_>, &ObjId)>,
) -> Result<(), ModelError> {
    let Some(target) = target.downcast_mut::<T>() else {
        return Err(mismatch());
    };
    *target = match slot {
        Some((value, id)) => T::load(doc, value, id)?,
        None => T::missing()?,
    };
    Ok(())
}

fn store_of<T: Model>(
    _: &ModelTable,
    value: &dyn Any,
    doc: &mut Doc,
    slot: Slot<'_>,
) -> Result<(), ModelError> {
    match value.downcast_ref::<T>() {
        Some(value) => value.store(doc, slot),
        None => Err(mismatch()),
    }
}

fn stores_as_of<T: Model>(_: &ModelTable, value: &dyn Any, before: &dyn Any) -> bool {
    match (value.downcast_ref::<T>(), before.downcast_ref::<T>()) {
        (Some(value), Some(before)) => value.stores_as(before),
        _ => false,
    }
}

fn store_changed_of<T: Model>(
    _: &ModelTable,
    value: &dyn Any,
    before: &dyn Any,
    doc: &mut Doc,
    slot: Slot<'_>,
    dirty: &Delta,
) -> Result<(), ModelError> {
    match (value.downcast_ref::<T>(), before.downcast_ref::<T>()) {
        (Some(value), Some(before)) => value.store_changed(before, doc, slot, dirty),
        _ => Err(mismatch()),
    }
}

fn refresh_of<T: Model>(
    _: &ModelTable,
    target: &mut dyn Any,
    doc: &Doc,
    value: &Value<'_>,
    id: &ObjId,
    delta: &Delta,
) -> Result<(), ModelError> {
    match target.downcast_mut::<T>() {
        Some(target) => target.refresh(doc, value, id, delta),
        None => Err(mismatch()),
    }
}

fn read_whole_of<T: Model>(
    _: &ModelTable,
    target: &mut dyn Any,
    doc: &Doc,
) -> Result<(), ModelError> {
    match target.downcast_mut::<T>() {
        Some(target) => target.read_whole(doc),
        None => Err(mismatch()),
    }
}

fn edit_of<T: Model>(
    _: &ModelTable,
    doc: &mut Doc,
    slot: Slot<'_>,
    path: &[Segment],
    op: &EditOp,
    cx: &mut ActionCx,
) -> Result<(), ModelError> {
    T::edit(doc, slot, path, op, cx)
}

fn diff_of<T: Model>(
    _: &ModelTable,
    before: &dyn Any,
    after: &dyn Any,
    path: &[Segment],
    out: &mut Vec<Edit<()>>,
) {
    if let (Some(before), Some(after)) = (before.downcast_ref::<T>(), after.downcast_ref::<T>()) {
        before.diff(after, path, out);
    }
}

fn collect_text_of<T: Model>(_: &ModelTable, value: &dyn Any, out: &mut Vec<String>) {
    if let Some(value) = value.downcast_ref::<T>() {
        value.collect_text(out);
    }
}

fn check_of<T: Model>(_: &ModelTable, value: &dyn Any) -> Result<(), Refusal> {
    value.downcast_ref::<T>().map_or(Ok(()), Model::check)
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "a table function: every type's check returns a result"
)]
fn no_check(_: &ModelTable, _: &dyn Any) -> Result<(), Refusal> {
    Ok(())
}

fn get_at_of<'a, T: Model>(
    _: &ModelTable,
    value: &'a dyn Any,
    path: &[Segment],
) -> Option<&'a dyn Any> {
    value.downcast_ref::<T>()?.get_at(path)
}

fn op_coalesces_of<T: Model>(
    _: &ModelTable,
    path: &[Segment],
    previous: &serde_json::Value,
    next: &serde_json::Value,
) -> Option<bool> {
    T::op_coalesces(path, previous, next)
}

fn no_coalesces(
    _: &ModelTable,
    _: &[Segment],
    _: &serde_json::Value,
    _: &serde_json::Value,
) -> Option<bool> {
    None
}

fn op_continuable_of<T: Model>(
    _: &ModelTable,
    path: &[Segment],
    op: &serde_json::Value,
) -> Option<bool> {
    T::op_continuable(path, op)
}

fn no_continuable(_: &ModelTable, _: &[Segment], _: &serde_json::Value) -> Option<bool> {
    None
}

fn op_describe_of<T: Model>(
    _: &ModelTable,
    path: &[Segment],
    op: &serde_json::Value,
) -> Option<OpDescription> {
    T::op_describe(path, op)
}

fn no_describe(_: &ModelTable, _: &[Segment], _: &serde_json::Value) -> Option<OpDescription> {
    None
}

fn op_lands_of<T: Model>(
    _: &ModelTable,
    value: &dyn Any,
    path: &[Segment],
    op: &serde_json::Value,
) -> Option<Place> {
    value.downcast_ref::<T>()?.op_lands(path, op)
}

fn no_lands(_: &ModelTable, _: &dyn Any, _: &[Segment], _: &serde_json::Value) -> Option<Place> {
    None
}

fn op_schemas_of<T: Model>(
    _: &ModelTable,
    prefix: &str,
    out: &mut Vec<(String, serde_json::Value)>,
) {
    T::op_schemas(prefix, out);
}

fn no_schemas(_: &ModelTable, _: &str, _: &mut Vec<(String, serde_json::Value)>) {}

/// The [`ItemOps`] of a collection type.
pub struct ItemOpsOf<C>(PhantomData<C>);

impl<C: ItemSource + 'static> ItemOpsOf<C> {
    pub const OPS: ItemOps = ItemOps {
        item_collection: item_collection_of::<C>,
        item_presence: item_presence_of::<C>,
        label_of_item: label_of_item_of::<C>,
        item_texts: item_texts_of::<C>,
        nested_items: nested_items_of::<C>,
    };
}

fn item_collection_of<C: ItemSource + 'static>(
    value: &dyn Any,
    field: &'static str,
) -> Option<ItemCollection> {
    Some(value.downcast_ref::<C>()?.item_collection(field))
}

fn item_presence_of<C: ItemSource + 'static>(value: &dyn Any) -> usize {
    value
        .downcast_ref::<C>()
        .map_or(0, ItemSource::item_presence)
}

fn label_of_item_of<C: ItemSource + 'static>(value: &dyn Any, id: &crate::Id) -> Option<String> {
    value.downcast_ref::<C>()?.label_of_item(id)
}

fn item_texts_of<C: ItemSource + 'static>(
    value: &dyn Any,
    field: &'static str,
    found: &mut Vec<ItemText>,
) {
    if let Some(value) = value.downcast_ref::<C>() {
        value.item_texts(field, found);
    }
}

fn nested_items_of<C: ItemSource + 'static>(
    value: &dyn Any,
    field: &'static str,
    out: &mut Vec<ItemCollection>,
) {
    if let Some(value) = value.downcast_ref::<C>() {
        value.nested_items(field, out);
    }
}

/// The table of a `String` stored whole, last writer winning
/// (`#[model(atom)]`): an id or a name two people setting at once must not
/// interleave.
pub static ATOM_STRING: ModelTable = ModelTable {
    ops: &ATOM_STRING_OPS,
    record: None,
    checks: false,
    has_ops: false,
};

static ATOM_STRING_OPS: ModelOps = ModelOps {
    to_json: atom_to_json,
    from_json: atom_from_json,
    load: atom_load,
    missing: atom_missing,
    reload: atom_reload,
    store: atom_store,
    stores_as: atom_stores_as,
    store_changed: atom_store_changed,
    refresh: atom_refresh,
    read_whole: atom_read_whole,
    edit: atom_edit,
    diff: atom_diff,
    collect_text: atom_collect_text,
    check: no_check,
    get_at: atom_get_at,
    op_coalesces: no_coalesces,
    op_continuable: no_continuable,
    op_describe: no_describe,
    op_lands: no_lands,
    op_schemas: no_schemas,
};

fn atom_text(value: &dyn Any) -> Result<&String, ModelError> {
    value.downcast_ref::<String>().ok_or_else(mismatch)
}

fn atom_of(value: &dyn Any) -> Result<crate::Atom, ModelError> {
    Ok(crate::Atom(atom_text(value)?.clone()))
}

// What the record shows of the field is the `String`'s; what it stores is
// the `Atom`'s.

fn atom_to_json(_: &ModelTable, value: &dyn Any) -> serde_json::Value {
    atom_text(value).map_or(serde_json::Value::Null, Model::to_json)
}

fn atom_from_json(
    _: &ModelTable,
    value: &serde_json::Value,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    put(out, String::from_json(value)?)
}

fn atom_load(
    _: &ModelTable,
    doc: &Doc,
    value: &Value<'_>,
    id: &ObjId,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    put(out, crate::Atom::load(doc, value, id)?.0)
}

fn atom_missing(_: &ModelTable, out: &mut dyn Any) -> Result<(), ModelError> {
    put(out, crate::Atom::missing()?.0)
}

fn atom_reload(
    _: &ModelTable,
    target: &mut dyn Any,
    doc: &Doc,
    slot: Option<(&Value<'_>, &ObjId)>,
) -> Result<(), ModelError> {
    let Some(target) = target.downcast_mut::<String>() else {
        return Err(mismatch());
    };
    *target = match slot {
        Some((value, id)) => crate::Atom::load(doc, value, id)?.0,
        None => crate::Atom::missing()?.0,
    };
    Ok(())
}

fn atom_store(
    _: &ModelTable,
    value: &dyn Any,
    doc: &mut Doc,
    slot: Slot<'_>,
) -> Result<(), ModelError> {
    atom_of(value)?.store(doc, slot)
}

fn atom_stores_as(_: &ModelTable, value: &dyn Any, before: &dyn Any) -> bool {
    match (atom_of(value), atom_of(before)) {
        (Ok(value), Ok(before)) => value.stores_as(&before),
        _ => false,
    }
}

fn atom_store_changed(
    _: &ModelTable,
    value: &dyn Any,
    before: &dyn Any,
    doc: &mut Doc,
    slot: Slot<'_>,
    dirty: &Delta,
) -> Result<(), ModelError> {
    atom_of(value)?.store_changed(&atom_of(before)?, doc, slot, dirty)
}

fn atom_refresh(
    _: &ModelTable,
    target: &mut dyn Any,
    doc: &Doc,
    value: &Value<'_>,
    id: &ObjId,
    delta: &Delta,
) -> Result<(), ModelError> {
    let Some(target) = target.downcast_mut::<String>() else {
        return Err(mismatch());
    };
    let mut atom = crate::Atom(std::mem::take(target));
    let refreshed = atom.refresh(doc, value, id, delta);
    *target = atom.0;
    refreshed
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "a table function: reading whole may fail for other types"
)]
fn atom_read_whole(_: &ModelTable, _: &mut dyn Any, _: &Doc) -> Result<(), ModelError> {
    Ok(())
}

fn atom_edit(
    _: &ModelTable,
    doc: &mut Doc,
    slot: Slot<'_>,
    path: &[Segment],
    op: &EditOp,
    cx: &mut ActionCx,
) -> Result<(), ModelError> {
    crate::Atom::edit(doc, slot, path, op, cx)
}

fn atom_diff(
    _: &ModelTable,
    before: &dyn Any,
    after: &dyn Any,
    path: &[Segment],
    out: &mut Vec<Edit<()>>,
) {
    if let (Ok(before), Ok(after)) = (atom_text(before), atom_text(after)) {
        before.diff(after, path, out);
    }
}

fn atom_collect_text(_: &ModelTable, value: &dyn Any, out: &mut Vec<String>) {
    if let Ok(text) = atom_text(value) {
        text.collect_text(out);
    }
}

fn atom_get_at<'a>(_: &ModelTable, value: &'a dyn Any, path: &[Segment]) -> Option<&'a dyn Any> {
    path.is_empty().then_some(value)
}

// --- Records ------------------------------------------------------------

/// The functions of every record, over its [`RecordDesc`].
pub static RECORD_OPS: ModelOps = ModelOps {
    to_json: |table, value| match table.record {
        Some(record) => to_json(record, value),
        None => serde_json::Value::Null,
    },
    from_json: |table, value, out| from_json(described(table)?, value, out),
    load: |table, doc, value, id, out| load(described(table)?, doc, value, id, out),
    missing: |table, out| missing(described(table)?, out),
    reload: |table, target, doc, slot| reload(described(table)?, target, doc, slot),
    store: |table, value, doc, slot| store(described(table)?, value, doc, slot),
    stores_as: |table, value, before| {
        table
            .record
            .is_some_and(|record| stores_as(record, value, before))
    },
    store_changed: |table, value, before, doc, slot, dirty| {
        store_changed(described(table)?, value, before, doc, slot, dirty)
    },
    refresh: |table, target, doc, value, id, delta| {
        refresh(described(table)?, target, doc, value, id, delta)
    },
    read_whole: |_, _, _| Ok(()),
    edit: |table, doc, slot, path, op, cx| edit(described(table)?, doc, slot, path, op, cx),
    diff: |table, before, after, path, out| {
        if let Some(record) = table.record {
            diff(record, before, after, path, out);
        }
    },
    collect_text: |table, value, out| {
        if let Some(record) = table.record {
            collect_text(record, value, out);
        }
    },
    check: |table, value| match table.record {
        Some(record) => check(record, value),
        None => Ok(()),
    },
    get_at: |table, value, path| get_at(table.record?, value, path),
    op_coalesces: |table, path, previous, next| op_coalesces(table.record?, path, previous, next),
    op_continuable: |table, path, op| op_continuable(table.record?, path, op),
    op_describe: |table, path, op| op_describe(table.record?, path, op),
    op_lands: |table, value, path, op| op_lands(table.record?, value, path, op),
    op_schemas: |table, prefix, out| {
        if let Some(record) = table.record {
            op_schemas(record, prefix, out);
        }
    },
};

impl ModelTable {
    /// The table of the record `record`.
    #[must_use]
    pub const fn record(record: &'static RecordDesc) -> Self {
        Self {
            ops: &RECORD_OPS,
            record: Some(record),
            checks: record.checks,
            has_ops: record.has_ops,
        }
    }
}

fn described(table: &ModelTable) -> Result<&'static RecordDesc, ModelError> {
    table.record.ok_or_else(mismatch)
}

/// The value of the field at `index` of `record`.
fn field_of<'a>(
    record: &RecordDesc,
    value: &'a dyn Any,
    index: usize,
) -> Result<&'a dyn Any, ModelError> {
    (record.field)(value, index).ok_or_else(mismatch)
}

/// The same, mutably.
fn field_of_mut<'a>(
    record: &RecordDesc,
    value: &'a mut dyn Any,
    index: usize,
) -> Result<&'a mut dyn Any, ModelError> {
    (record.field_mut)(value, index).ok_or_else(mismatch)
}

/// [`Model::to_json`] of a record.
#[must_use]
pub fn to_json(record: &RecordDesc, value: &dyn Any) -> serde_json::Value {
    let mut fields = serde_json::Map::new();
    for (index, field) in record.fields.iter().enumerate() {
        if let Some(inner) = (record.field)(value, index) {
            fields.insert(
                field.key.to_owned(),
                (field.table.ops.to_json)(field.table, inner),
            );
        }
    }
    serde_json::Value::Object(fields)
}

/// Fields read from a record's JSON form.
struct JsonSource<'a> {
    record: &'a RecordDesc,
    fields: &'a serde_json::Map<String, serde_json::Value>,
    index: usize,
}

impl FieldSource for JsonSource<'_> {
    fn next(&mut self, out: &mut dyn Any) -> Result<(), ModelError> {
        let field = self.record.fields.get(self.index).ok_or_else(mismatch)?;
        match self.fields.get(field.key) {
            Some(value) => (field.table.ops.from_json)(field.table, value, out),
            None => (field.table.ops.missing)(field.table, out),
        }
        .located(field.key)
    }

    fn read(&mut self, _: &mut dyn Any) -> Result<(), ModelError> {
        self.index = self.index.saturating_add(1);
        Ok(())
    }
}

/// The missing value of each field of a record.
struct MissingSource<'a> {
    record: &'a RecordDesc,
    index: usize,
}

impl FieldSource for MissingSource<'_> {
    fn next(&mut self, out: &mut dyn Any) -> Result<(), ModelError> {
        let field = self.record.fields.get(self.index).ok_or_else(mismatch)?;
        (field.table.ops.missing)(field.table, out).located(field.key)
    }

    fn read(&mut self, _: &mut dyn Any) -> Result<(), ModelError> {
        self.index = self.index.saturating_add(1);
        Ok(())
    }
}

/// Fields read from what each key of a record's map holds.
struct DocSource<'a> {
    record: &'a RecordDesc,
    doc: &'a Doc,
    slots: Vec<crate::model::MapSlot>,
    index: usize,
}

impl FieldSource for DocSource<'_> {
    fn next(&mut self, out: &mut dyn Any) -> Result<(), ModelError> {
        let field = self.record.fields.get(self.index).ok_or_else(mismatch)?;
        match crate::model::take_slot(&mut self.slots, self.index) {
            Some((value, id)) => (field.table.ops.load)(field.table, self.doc, &value, &id, out),
            None => (field.table.ops.missing)(field.table, out),
        }
        .located(field.key)
    }

    fn read(&mut self, value: &mut dyn Any) -> Result<(), ModelError> {
        let field = self.record.fields.get(self.index).ok_or_else(mismatch)?;
        self.index = self.index.saturating_add(1);
        if field.whole {
            (field.table.ops.read_whole)(field.table, value, self.doc).located(field.key)?;
        }
        Ok(())
    }
}

/// [`Model::from_json`] of a record, put into `out`.
///
/// # Errors
/// Why `value` is not the record's JSON form.
pub fn from_json(
    record: &RecordDesc,
    value: &serde_json::Value,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    match value {
        serde_json::Value::Object(fields) => (record.build)(
            &mut JsonSource {
                record,
                fields,
                index: 0,
            },
            out,
        ),
        serde_json::Value::Null => missing(record, out),
        _ => Err(ModelError::shape("an object")),
    }
}

/// [`Model::missing`] of a record, put into `out`.
///
/// # Errors
/// Why a field has no value without one stored.
pub fn missing(record: &RecordDesc, out: &mut dyn Any) -> Result<(), ModelError> {
    (record.build)(&mut MissingSource { record, index: 0 }, out)
}

/// [`Model::load`] of a record, put into `out`.
///
/// # Errors
/// Why the stored value is not the record.
pub fn load(
    record: &RecordDesc,
    doc: &Doc,
    value: &Value<'_>,
    id: &ObjId,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    match value {
        Value::Object(ObjType::Map) => read_fields(record, doc, id, out),
        _ => Err(ModelError::shape("a map")),
    }
}

/// [`Fields::read_fields`](crate::Fields::read_fields), put into `out`:
/// every stored field read in one walk of the map.
///
/// # Errors
/// Why a field does not read.
pub fn read_fields(
    record: &RecordDesc,
    doc: &Doc,
    obj: &ObjId,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    let mut keys: Vec<&str> = Vec::with_capacity(record.fields.len());
    for field in record.fields {
        keys.push(field.key);
    }
    let mut slots = vec![None; keys.len()];
    crate::model::fill_slots(doc, obj, &keys, &mut slots);
    (record.build)(
        &mut DocSource {
            record,
            doc,
            slots,
            index: 0,
        },
        out,
    )
}

fn reload(
    record: &RecordDesc,
    target: &mut dyn Any,
    doc: &Doc,
    slot: Option<(&Value<'_>, &ObjId)>,
) -> Result<(), ModelError> {
    // A record is replaced by one read anew through its own table.
    let mut fresh = (record.empty)();
    if let Some((value, id)) = slot {
        load(record, doc, value, id, fresh.as_mut())?;
    } else {
        missing(record, fresh.as_mut())?;
    }
    (record.replace)(target, fresh.as_mut())
}

/// [`Fields::write_fields`](crate::Fields::write_fields) of a record.
///
/// # Errors
/// The document's write error, located at the field.
pub fn write_fields(
    record: &RecordDesc,
    value: &dyn Any,
    doc: &mut Doc,
    obj: &ObjId,
) -> Result<(), ModelError> {
    for (index, field) in record.fields.iter().enumerate() {
        let inner = field_of(record, value, index)?;
        (field.table.ops.store)(field.table, inner, doc, Slot::Key(obj, field.key))
            .located(field.key)?;
    }
    Ok(())
}

/// [`Model::store`] of a record.
///
/// # Errors
/// The document's write error.
pub fn store(
    record: &RecordDesc,
    value: &dyn Any,
    doc: &mut Doc,
    slot: Slot<'_>,
) -> Result<(), ModelError> {
    let obj = slot.object(doc, ObjType::Map)?;
    write_fields(record, value, doc, &obj)
}

/// [`Model::stores_as`] of a record.
#[must_use]
pub fn stores_as(record: &RecordDesc, value: &dyn Any, before: &dyn Any) -> bool {
    for (index, field) in record.fields.iter().enumerate() {
        let (Some(now), Some(was)) = ((record.field)(value, index), (record.field)(before, index))
        else {
            return false;
        };
        if !(field.table.ops.stores_as)(field.table, now, was) {
            return false;
        }
    }
    true
}

/// [`Fields::write_changed_fields`](crate::Fields::write_changed_fields) of
/// a record: each field written as `write_key_changed` writes it.
///
/// # Errors
/// The document's write error, located at the field.
pub fn write_changed_fields(
    record: &RecordDesc,
    value: &dyn Any,
    before: &dyn Any,
    doc: &mut Doc,
    obj: &ObjId,
    dirty: &Delta,
) -> Result<(), ModelError> {
    for (index, field) in record.fields.iter().enumerate() {
        let now = field_of(record, value, index)?;
        let was = field_of(record, before, index)?;
        let ops = field.table.ops;
        match dirty.key(obj, field.key) {
            Touch::Written => {
                (ops.store)(field.table, now, doc, Slot::Key(obj, field.key)).located(field.key)?;
            }
            Touch::Untouched if (ops.stores_as)(field.table, now, was) => {}
            Touch::Untouched | Touch::Below => {
                (ops.store_changed)(field.table, now, was, doc, Slot::Key(obj, field.key), dirty)
                    .located(field.key)?;
            }
        }
    }
    Ok(())
}

/// [`Model::store_changed`] of a record.
///
/// # Errors
/// The document's write error.
pub fn store_changed(
    record: &RecordDesc,
    value: &dyn Any,
    before: &dyn Any,
    doc: &mut Doc,
    slot: Slot<'_>,
    dirty: &Delta,
) -> Result<(), ModelError> {
    let obj = slot.object(doc, ObjType::Map)?;
    write_changed_fields(record, value, before, doc, &obj, dirty)
}

/// [`Model::refresh`] of a record.
///
/// # Errors
/// Why a touched field does not read.
pub fn refresh(
    record: &RecordDesc,
    target: &mut dyn Any,
    doc: &Doc,
    value: &Value<'_>,
    id: &ObjId,
    delta: &Delta,
) -> Result<(), ModelError> {
    match value {
        Value::Object(ObjType::Map) => refresh_fields(record, target, doc, id, delta),
        _ => Err(ModelError::shape("a map")),
    }
}

/// [`Fields::refresh_fields`](crate::Fields::refresh_fields) of a record:
/// a map changed all over is read anew, else each touched field.
///
/// # Errors
/// Why a touched field does not read.
pub fn refresh_fields(
    record: &RecordDesc,
    target: &mut dyn Any,
    doc: &Doc,
    obj: &ObjId,
    delta: &Delta,
) -> Result<(), ModelError> {
    if delta.under(obj).is_all() {
        let mut fresh = (record.empty)();
        read_fields(record, doc, obj, fresh.as_mut())?;
        return (record.replace)(target, fresh.as_mut());
    }
    for (index, field) in record.fields.iter().enumerate() {
        let inner = field_of_mut(record, target, index)?;
        refresh_key(field, inner, doc, obj, delta)?;
    }
    Ok(())
}

/// Bring the field `field`, read earlier from the map `obj`, up to date
/// after `delta`: untouched keeps it, a written slot reads it again, a
/// change inside what it holds refreshes it in place.
fn refresh_key(
    field: &FieldDesc,
    target: &mut dyn Any,
    doc: &Doc,
    obj: &ObjId,
    delta: &Delta,
) -> Result<(), ModelError> {
    let ops = field.table.ops;
    match delta.key(obj, field.key) {
        Touch::Untouched => {}
        Touch::Written => {
            let slot = crate::model::get_slot(doc, obj, field.key).located(field.key)?;
            (ops.reload)(
                field.table,
                target,
                doc,
                slot.as_ref().map(|(value, id)| (value, id)),
            )
            .located(field.key)?;
        }
        Touch::Below => match crate::model::get_slot(doc, obj, field.key).located(field.key)? {
            Some((value, id)) => {
                (ops.refresh)(field.table, target, doc, &value, &id, delta).located(field.key)?;
            }
            None => (ops.reload)(field.table, target, doc, None).located(field.key)?,
        },
    }
    if field.whole {
        (ops.read_whole)(field.table, target, doc).located(field.key)?;
    }
    Ok(())
}

/// [`Model::edit`] of a record.
///
/// # Errors
/// Why the path or operation does not fit, or a refusal.
pub fn edit(
    record: &RecordDesc,
    doc: &mut Doc,
    slot: Slot<'_>,
    path: &[Segment],
    op: &EditOp,
    cx: &mut ActionCx,
) -> Result<(), ModelError> {
    let obj = slot.object(doc, ObjType::Map)?;
    edit_fields(record, doc, &obj, path, op, cx)
}

/// [`Fields::edit_fields`](crate::Fields::edit_fields) of a record: a field
/// segment routes the edit to its field; a `set` of the record replaces it
/// whole; the record's own check runs after.
///
/// # Errors
/// Why the path or operation does not fit, or a refusal.
pub fn edit_fields(
    record: &RecordDesc,
    doc: &mut Doc,
    obj: &ObjId,
    path: &[Segment],
    op: &EditOp,
    cx: &mut ActionCx,
) -> Result<(), ModelError> {
    match path.split_first() {
        Some((Segment::Field(key), rest)) => {
            let Some(field) = record.fields.iter().find(|field| field.key == key) else {
                return Err(ModelError::invalid(format!("no field `{key}`")));
            };
            // The outside settling an outbox entry, or forgetting a settled
            // one, is no edit of the field's content: a read-only outbox
            // takes them.
            if field.readonly && !matches!(op, EditOp::Settle { .. } | EditOp::Forget { .. }) {
                return Err(ModelError::Refused(format!(
                    "`{}` is changed only by the record's actions",
                    field.name
                )));
            }
            (field.table.ops.edit)(field.table, doc, Slot::Key(obj, field.key), rest, op, cx)
                .located(field.key)?;
        }
        None => match op {
            EditOp::Set { value } => {
                if record.fields.iter().any(|field| field.readonly) {
                    return Err(ModelError::Refused(String::from(
                        "a record with read-only fields is not replaced whole",
                    )));
                }
                let mut whole = (record.empty)();
                from_json(record, value, whole.as_mut())?;
                let whole = (record.unwrap)(whole.as_ref()).ok_or_else(mismatch)?;
                check(record, whole).map_err(ModelError::refused)?;
                write_fields(record, whole, doc, obj)?;
            }
            other => return Err(ModelError::unsupported(other)),
        },
        Some((segment, _)) => {
            return Err(ModelError::invalid(format!(
                "nothing in this record is called `{segment}`"
            )));
        }
    }
    if let Some(own) = &record.own_check {
        let checked = obj.clone();
        let (read, own_check) = (own.read, own.check);
        crate::group::check_or_defer(format!("{}@{:?}", (record.name)(), obj), doc, move |doc| {
            match read(doc, &checked) {
                Ok(current) => own_check(current.as_ref()),
                Err(error) => Err(Refusal::new(error.to_string())),
            }
        })
        .map_err(ModelError::refused)?;
    }
    Ok(())
}

/// [`Model::check`] of a record: its own invariant, then its fields'.
///
/// # Errors
/// The first refusal.
pub fn check(record: &RecordDesc, value: &dyn Any) -> Result<(), Refusal> {
    if !record.checks {
        return Ok(());
    }
    if let Some(own) = &record.own_check {
        (own.check)(value)?;
    }
    for (index, field) in record.fields.iter().enumerate() {
        if field.table.checks
            && let Some(inner) = (record.field)(value, index)
        {
            (field.table.ops.check)(field.table, inner)?;
        }
    }
    Ok(())
}

/// The first non-empty line of the text the field at `index` carries.
fn first_line(record: &RecordDesc, value: &dyn Any, index: usize) -> Option<String> {
    let field = record.fields.get(index)?;
    let inner = (record.field)(value, index)?;
    let mut text = Vec::new();
    (field.table.ops.collect_text)(field.table, inner, &mut text);
    crate::__private::first_line_of(text)
}

/// [`Model::label`] of a record: the texts of its label fields joined by
/// spaces, else its `#[model(label)]` field.
#[must_use]
pub fn label(record: &RecordDesc, value: &dyn Any) -> Option<String> {
    if !record.label_parts.is_empty() {
        let mut parts = Vec::new();
        for index in record.label_parts {
            if let Some(part) = first_line(record, value, *index) {
                parts.push(part);
            }
        }
        if !parts.is_empty() {
            return Some(parts.join(" "));
        }
    }
    first_line(record, value, record.label?)
}

/// The field `key` of `record`, with `rest`, when `path` starts at a field
/// whose type has a vocabulary of its own.
fn ops_field<'p>(
    record: &RecordDesc,
    path: &'p [Segment],
) -> Option<(usize, &'static FieldDesc, &'p [Segment])> {
    let Some((Segment::Field(key), rest)) = path.split_first() else {
        return None;
    };
    let (index, field) = record
        .fields
        .iter()
        .enumerate()
        .find(|(_, field)| field.key == key)?;
    field.table.has_ops.then_some((index, field, rest))
}

/// [`Model::op_coalesces`] of a record.
#[must_use]
pub fn op_coalesces(
    record: &RecordDesc,
    path: &[Segment],
    previous: &serde_json::Value,
    next: &serde_json::Value,
) -> Option<bool> {
    let (_, field, rest) = ops_field(record, path)?;
    (field.table.ops.op_coalesces)(field.table, rest, previous, next)
}

/// [`Model::op_continuable`] of a record.
#[must_use]
pub fn op_continuable(
    record: &RecordDesc,
    path: &[Segment],
    op: &serde_json::Value,
) -> Option<bool> {
    let (_, field, rest) = ops_field(record, path)?;
    (field.table.ops.op_continuable)(field.table, rest, op)
}

/// [`Model::op_describe`] of a record.
#[must_use]
pub fn op_describe(
    record: &RecordDesc,
    path: &[Segment],
    op: &serde_json::Value,
) -> Option<OpDescription> {
    let (_, field, rest) = ops_field(record, path)?;
    (field.table.ops.op_describe)(field.table, rest, op)
}

/// [`Model::op_lands`] of a record.
#[must_use]
pub fn op_lands(
    record: &RecordDesc,
    value: &dyn Any,
    path: &[Segment],
    op: &serde_json::Value,
) -> Option<Place> {
    let (index, field, rest) = ops_field(record, path)?;
    let inner = (record.field)(value, index)?;
    (field.table.ops.op_lands)(field.table, inner, rest, op)
}

/// [`Model::op_schemas`] of a record: only a field whose type has a
/// vocabulary of its own is visited.
pub fn op_schemas(record: &RecordDesc, prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
    // A type nested in itself has no vocabulary of its own deeper than
    // this.
    if prefix.matches('.').count() > 16 {
        return;
    }
    for field in record.fields {
        if field.table.has_ops {
            (field.table.ops.op_schemas)(field.table, &format!("{prefix}{}.", field.key), out);
        }
    }
}

/// [`Model::get_at`] of a record.
#[must_use]
pub fn get_at<'a>(
    record: &RecordDesc,
    value: &'a dyn Any,
    path: &[Segment],
) -> Option<&'a dyn Any> {
    match path.split_first() {
        None => Some(value),
        Some((Segment::Field(key), rest)) => {
            let (index, field) = record
                .fields
                .iter()
                .enumerate()
                .find(|(_, field)| field.key == key)?;
            let inner = (record.field)(value, index)?;
            (field.table.ops.get_at)(field.table, inner, rest)
        }
        Some(_) => None,
    }
}

/// [`Model::collect_text`] of a record.
pub fn collect_text(record: &RecordDesc, value: &dyn Any, out: &mut Vec<String>) {
    for (index, field) in record.fields.iter().enumerate() {
        if let Some(inner) = (record.field)(value, index) {
            (field.table.ops.collect_text)(field.table, inner, out);
        }
    }
}

/// [`Model::diff`] of a record: each field that is not read-only, at its
/// key.
pub fn diff(
    record: &RecordDesc,
    before: &dyn Any,
    after: &dyn Any,
    path: &[Segment],
    out: &mut Vec<Edit<()>>,
) {
    for (index, field) in record.fields.iter().enumerate() {
        if field.readonly {
            continue;
        }
        let (Some(was), Some(now)) = ((record.field)(before, index), (record.field)(after, index))
        else {
            continue;
        };
        let mut at = path.to_vec();
        at.push(Segment::Field(field.key.to_owned()));
        (field.table.ops.diff)(field.table, was, now, &at, out);
    }
}

/// [`Fields::item_collections`](crate::Fields::item_collections) of a
/// record: each collection field's first page, then the collections inside
/// their items.
#[must_use]
pub fn item_collections(record: &RecordDesc, value: &dyn Any) -> Vec<ItemCollection> {
    let mut out = Vec::new();
    for (index, field) in record.fields.iter().enumerate() {
        if let (Some(items), Some(inner)) = (field.items, (record.field)(value, index))
            && let Some(collection) = (items.item_collection)(inner, field.name)
        {
            out.push(collection);
        }
    }
    for (index, field) in record.fields.iter().enumerate() {
        if let (Some(items), Some(inner)) = (field.items, (record.field)(value, index)) {
            (items.nested_items)(inner, field.name, &mut out);
        }
    }
    out
}

/// [`Fields::item_collection_sizes`](crate::Fields::item_collection_sizes)
/// of a record.
#[must_use]
pub fn item_collection_sizes(record: &RecordDesc, value: &dyn Any) -> Vec<(&'static str, usize)> {
    let mut out = Vec::new();
    for (index, field) in record.fields.iter().enumerate() {
        if let Some(items) = field.items {
            let presence = (record.field)(value, index).map_or(0, items.item_presence);
            out.push((field.name, presence));
        }
    }
    out
}

/// [`Fields::item_label`](crate::Fields::item_label) of a record.
#[must_use]
pub fn item_label(
    record: &RecordDesc,
    value: &dyn Any,
    name: &str,
    id: &crate::Id,
) -> Option<String> {
    let (index, field) = record
        .fields
        .iter()
        .enumerate()
        .find(|(_, field)| field.items.is_some() && field.name == name)?;
    let items = field.items?;
    (items.label_of_item)((record.field)(value, index)?, id)
}

/// [`Fields::item_search`](crate::Fields::item_search) of a record.
#[must_use]
pub fn item_search(record: &RecordDesc, value: &dyn Any) -> Vec<ItemText> {
    let mut found = Vec::new();
    for (index, field) in record.fields.iter().enumerate() {
        if let (Some(items), Some(inner)) = (field.items, (record.field)(value, index)) {
            (items.item_texts)(inner, field.name, &mut found);
        }
    }
    found
}

/// [`Model::diff`] of a record into edits of the record `E`.
pub fn diff_into<E>(
    record: &RecordDesc,
    before: &dyn Any,
    after: &dyn Any,
    path: &[Segment],
    out: &mut Vec<Edit<E>>,
) {
    let mut found = Vec::new();
    diff(record, before, after, path, &mut found);
    out.reserve(found.len());
    for edit in found {
        out.push(edit.retyped());
    }
}

/// The value a record function put into `out`.
///
/// # Errors
/// When it put none: a bug in the record's description.
pub fn filled<T>(out: Option<T>) -> Result<T, ModelError> {
    match out {
        Some(value) => Ok(value),
        None => Err(mismatch()),
    }
}
