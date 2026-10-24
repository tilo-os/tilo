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
use tilo_sort::CompactSort as _;

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
    pub(crate) to_json: fn(&ModelTable, &dyn Any) -> serde_json::Value,
    pub(crate) from_json:
        fn(&ModelTable, &serde_json::Value, &mut dyn Any) -> Result<(), ModelError>,
    pub(crate) load:
        fn(&ModelTable, &Doc, &Value<'_>, &ObjId, &mut dyn Any) -> Result<(), ModelError>,
    pub(crate) missing: fn(&ModelTable, &mut dyn Any) -> Result<(), ModelError>,
    /// Read the value anew in place: from what a slot holds, or its missing
    /// value when the slot holds nothing.
    pub(crate) reload:
        fn(&ModelTable, &mut dyn Any, &Doc, Option<(&Value<'_>, &ObjId)>) -> Result<(), ModelError>,
    pub(crate) store: fn(&ModelTable, &dyn Any, &mut Doc, Slot<'_>) -> Result<(), ModelError>,
    pub(crate) stores_as: fn(&ModelTable, &dyn Any, &dyn Any) -> bool,
    pub(crate) store_changed:
        fn(&ModelTable, &dyn Any, &dyn Any, &mut Doc, Slot<'_>, &Delta) -> Result<(), ModelError>,
    pub(crate) refresh:
        fn(&ModelTable, &mut dyn Any, &Doc, &Value<'_>, &ObjId, &Delta) -> Result<(), ModelError>,
    pub(crate) read_whole: fn(&ModelTable, &mut dyn Any, &Doc) -> Result<(), ModelError>,
    pub(crate) edit: fn(
        &ModelTable,
        &mut Doc,
        Slot<'_>,
        &[Segment],
        &EditOp,
        &mut ActionCx,
    ) -> Result<(), ModelError>,
    pub(crate) diff: fn(&ModelTable, &dyn Any, &dyn Any, &[Segment], &mut Vec<Edit<()>>),
    pub(crate) collect_text: fn(&ModelTable, &dyn Any, &mut Vec<String>),
    pub(crate) check: fn(&ModelTable, &dyn Any) -> Result<(), Refusal>,
    pub(crate) get_at: for<'a> fn(&ModelTable, &'a dyn Any, &[Segment]) -> Option<&'a dyn Any>,
    pub(crate) label: fn(&ModelTable, &dyn Any) -> Option<String>,
    pub(crate) nested_collections: fn(&ModelTable, &dyn Any) -> Vec<ItemCollection>,
    pub(crate) op_coalesces:
        fn(&ModelTable, &[Segment], &serde_json::Value, &serde_json::Value) -> Option<bool>,
    pub(crate) op_continuable: fn(&ModelTable, &[Segment], &serde_json::Value) -> Option<bool>,
    pub(crate) op_describe:
        fn(&ModelTable, &[Segment], &serde_json::Value) -> Option<OpDescription>,
    pub(crate) op_lands: fn(&ModelTable, &dyn Any, &[Segment], &serde_json::Value) -> Option<Place>,
    pub(crate) op_schemas: fn(&ModelTable, &str, &mut Vec<(String, serde_json::Value)>),
    /// What an edit that writes a new value carries of the value's JSON
    /// form ([`crate::shape::without_absent`]).
    pub(crate) without_absent: fn(&ModelTable, serde_json::Value) -> serde_json::Value,
}

/// A model type as data: its erased [`Model`] functions, what it is (a
/// record's fields), and what it holds (a collection's items).
pub struct ModelTable {
    pub(crate) ops: &'static ModelOps,
    desc: Desc,
    /// Its [`Model::CHECKS`].
    pub checks: bool,
    /// Its [`Model::OPS`].
    pub has_ops: bool,
    /// Its [`Model::SCALAR`].
    pub scalar: bool,
}

/// What a type's table knows of its shape, beyond its functions.
#[derive(Clone, Copy)]
enum Desc {
    /// Its own functions do everything.
    Own,
    /// A record of fields.
    Record(&'static RecordDesc),
    /// An `Option` of a value.
    Option(&'static OptionDesc),
    /// A collection of items.
    Collection(&'static crate::collection::CollectionDesc),
    /// An enum of unit variants.
    Unit(&'static UnitDesc),
    /// A value stored whole as JSON text.
    Json(&'static JsonDesc),
    /// A list of values.
    Vec(&'static VecDesc),
    /// A map of string keys.
    Map(&'static MapDesc),
    /// A set of elements stored as map keys.
    Set(&'static crate::storage::SetDesc),
}

impl ModelTable {
    pub(crate) const fn record(&self) -> Option<&'static RecordDesc> {
        match self.desc {
            Desc::Record(record) => Some(record),
            _ => None,
        }
    }

    pub(crate) const fn collection(&self) -> Option<&'static crate::collection::CollectionDesc> {
        match self.desc {
            Desc::Collection(collection) => Some(collection),
            _ => None,
        }
    }

    /// The table of a collection.
    #[must_use]
    pub const fn of_collection(
        collection: &'static crate::collection::CollectionDesc,
        checks: bool,
        has_ops: bool,
    ) -> Self {
        Self {
            ops: &crate::collection::COLLECTION_OPS,
            desc: Desc::Collection(collection),
            checks,
            has_ops,
            scalar: false,
        }
    }

    pub(crate) const fn set(&self) -> Option<&'static crate::storage::SetDesc> {
        match self.desc {
            Desc::Set(set) => Some(set),
            _ => None,
        }
    }

    /// The table of a set.
    #[must_use]
    pub const fn of_set(set: &'static crate::storage::SetDesc, checks: bool) -> Self {
        Self {
            ops: &crate::storage::SET_OPS,
            desc: Desc::Set(set),
            checks,
            has_ops: false,
            scalar: false,
        }
    }

    const fn option(&self) -> Option<&'static OptionDesc> {
        match self.desc {
            Desc::Option(option) => Some(option),
            _ => None,
        }
    }
}

/// A collection type's [`ItemSource`] functions, its type erased.
pub struct ItemOps {
    item_collection: fn(&dyn Any, &'static str) -> Option<ItemCollection>,
    item_presence: fn(&dyn Any) -> usize,
    label_of_item: fn(&dyn Any, &crate::Id) -> Option<String>,
    item_texts: fn(&dyn Any, &'static str, &mut Vec<ItemText>),
    nested_items: fn(&dyn Any, &'static str, &mut Vec<ItemCollection>),
    /// The items, whatever their type.
    list: for<'a> fn(&'a dyn Any) -> Option<&'a dyn crate::collection::ItemList>,
    /// The items' type's table.
    item_table: &'static ModelTable,
}

/// A collection whose items are published: its items' type's table.
pub trait PublishedItems: crate::collection::ItemList + ItemSource + 'static {
    const ITEM_TABLE: &'static ModelTable;
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
    /// What the check reads (`#[model(check_reads, check_items)]`); `None`
    /// when it may read anything, so every edit inside the record checks it.
    pub reads: Option<CheckReads>,
}

/// What a record's own check reads: an edit that touches none of it cannot
/// break the invariant, so the record is not read again to check it.
pub struct CheckReads {
    /// The keys of the fields it reads.
    pub fields: &'static [&'static str],
    /// The keys of the collections whose items it counts but does not read
    /// into: adding, removing or replacing an item touches it, an edit
    /// inside one does not.
    pub items: &'static [&'static str],
}

impl CheckReads {
    /// Whether an edit at `path` (from the record) can change what the
    /// check reads.
    #[must_use]
    pub fn touched_by(&self, path: &[Segment]) -> bool {
        match path.split_first() {
            Some((Segment::Field(key), rest)) => {
                self.fields.contains(&key.as_str())
                    || (self.items.contains(&key.as_str()) && rest.len() <= 1)
            }
            // The whole record, or a path it does not know: checked.
            None | Some(_) => true,
        }
    }
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

/// The next field of `source`, a `T`, or what `repair` derives for it when
/// the document holds none: a field declared `#[model(default = ..)]`, so a
/// record missing it (written by an older app, or half-made by an edit that
/// raced its deletion) still reads, and the next whole write stores it.
///
/// # Errors
/// Why it does not read, other than being absent.
pub fn take_or<T: 'static>(
    source: &mut dyn FieldSource,
    repair: impl FnOnce() -> T,
) -> Result<T, ModelError> {
    let mut out: Option<T> = None;
    match source.next(&mut out) {
        Ok(()) => {}
        Err(ModelError::Missing { .. }) => out = Some(repair()),
        Err(error) => return Err(error),
    }
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
pub(crate) fn mismatch() -> ModelError {
    ModelError::invalid("a value of another type")
}

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
        without_absent: crate::shape::kept_whole,
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
        label: label_of::<T>,
        nested_collections: nested_collections_of::<T>,
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
        desc: Desc::Own,
        checks: T::CHECKS,
        has_ops: T::OPS,
        scalar: T::SCALAR,
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

fn label_of<T: Model>(_: &ModelTable, value: &dyn Any) -> Option<String> {
    value.downcast_ref::<T>()?.label()
}

fn nested_collections_of<T: Model>(_: &ModelTable, value: &dyn Any) -> Vec<ItemCollection> {
    value
        .downcast_ref::<T>()
        .map_or_else(Vec::new, Model::nested_collections)
}

/// No label, no collections inside: what a value of a type with neither
/// answers.
pub(crate) fn no_label(_: &ModelTable, _: &dyn Any) -> Option<String> {
    None
}

pub(crate) fn no_nested(_: &ModelTable, _: &dyn Any) -> Vec<ItemCollection> {
    Vec::new()
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

impl<C: PublishedItems> ItemOpsOf<C> {
    pub const OPS: ItemOps = ItemOps {
        list: |value| Some(value.downcast_ref::<C>()? as &dyn crate::collection::ItemList),
        item_table: C::ITEM_TABLE,
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
    desc: Desc::Own,
    checks: false,
    has_ops: false,
    scalar: true,
};

static ATOM_STRING_OPS: ModelOps = ModelOps {
    without_absent: crate::shape::kept_whole,
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
    label: no_label,
    nested_collections: no_nested,
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

/// The functions of every record, over its [`RecordDesc`].
pub static RECORD_OPS: ModelOps = ModelOps {
    without_absent: crate::shape::record_without_absent,
    to_json: |table, value| match table.record() {
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
            .record()
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
        if let Some(record) = table.record() {
            diff(record, before, after, path, out);
        }
    },
    collect_text: |table, value, out| {
        if let Some(record) = table.record() {
            collect_text(record, value, out);
        }
    },
    check: |table, value| match table.record() {
        Some(record) => check(record, value),
        None => Ok(()),
    },
    get_at: |table, value, path| get_at(table.record()?, value, path),
    label: |table, value| label(table.record()?, value),
    nested_collections: |table, value| {
        table
            .record()
            .map_or_else(Vec::new, |record| item_collections(record, value))
    },
    op_coalesces: |table, path, previous, next| op_coalesces(table.record()?, path, previous, next),
    op_continuable: |table, path, op| op_continuable(table.record()?, path, op),
    op_describe: |table, path, op| op_describe(table.record()?, path, op),
    op_lands: |table, value, path, op| op_lands(table.record()?, value, path, op),
    op_schemas: |table, prefix, out| {
        if let Some(record) = table.record() {
            op_schemas(record, prefix, out);
        }
    },
};

impl ModelTable {
    /// The table of the record `record`.
    #[must_use]
    pub const fn of_record(record: &'static RecordDesc) -> Self {
        Self {
            ops: &RECORD_OPS,
            desc: Desc::Record(record),
            checks: record.checks,
            has_ops: record.has_ops,
            scalar: false,
        }
    }
}

fn described(table: &ModelTable) -> Result<&'static RecordDesc, ModelError> {
    table.record().ok_or_else(mismatch)
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

/// Read the record from the maps `objs`, field by field: each field from
/// the first map that holds it. Two replicas that made the same item
/// concurrently each made a map for it, and the document keeps both under
/// one key; this reads them as one item, so a field only the losing map
/// holds is not lost.
///
/// # Errors
/// Why a field does not read from any of them.
pub(crate) fn read_fields_merged(
    record: &RecordDesc,
    doc: &Doc,
    objs: &[ObjId],
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    let mut keys: Vec<&str> = Vec::with_capacity(record.fields.len());
    for field in record.fields {
        keys.push(field.key);
    }
    let mut slots = vec![None; keys.len()];
    for obj in objs {
        let mut found = vec![None; keys.len()];
        crate::model::fill_slots(doc, obj, &keys, &mut found);
        for (slot, found) in slots.iter_mut().zip(found) {
            if slot.is_none() {
                *slot = found;
            }
        }
    }
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
    write_fields_leaving(record, value, doc, obj, false)
}

/// [`write_fields`], leaving out a scalar field that holds what its absent
/// key reads as ([`Model::missing`]) where its key is absent: in a map made
/// just now (`fresh`), every such field; in one that was there, only those
/// whose key it does not hold, which read the same without a write.
fn write_fields_leaving(
    record: &RecordDesc,
    value: &dyn Any,
    doc: &mut Doc,
    obj: &ObjId,
    fresh: bool,
) -> Result<(), ModelError> {
    let mut staged = (record.empty)();
    let absent = missing(record, staged.as_mut())
        .ok()
        .and_then(|()| (record.unwrap)(staged.as_ref()));
    for (index, field) in record.fields.iter().enumerate() {
        let inner = field_of(record, value, index)?;
        if field.table.scalar
            && absent
                .and_then(|absent| (record.field)(absent, index))
                .is_some_and(|empty| (field.table.ops.stores_as)(field.table, inner, empty))
            && (fresh || Slot::Key(obj, field.key).current(doc)?.is_none())
        {
            continue;
        }
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
    let fresh = !matches!(slot.current(doc)?, Some((Value::Object(ObjType::Map), _)));
    let obj = slot.object(doc, ObjType::Map)?;
    write_fields_leaving(record, value, doc, &obj, fresh)
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
        write_key_changed(field.table, (now, was), doc, obj, field.key, dirty)?;
    }
    Ok(())
}

/// Write `now` under `key` of the map `obj`, which holds `was`: what
/// [`crate::model::write_key_changed`] writes, through `table`.
///
/// # Errors
/// The document's write error, located at `key`.
pub(crate) fn write_key_changed(
    table: &ModelTable,
    (now, was): (&dyn Any, &dyn Any),
    doc: &mut Doc,
    obj: &ObjId,
    key: &str,
    dirty: &Delta,
) -> Result<(), ModelError> {
    let ops = table.ops;
    match dirty.key(obj, key) {
        Touch::Written => (ops.store)(table, now, doc, Slot::Key(obj, key)).located(key),
        Touch::Untouched if (ops.stores_as)(table, now, was) => Ok(()),
        Touch::Untouched | Touch::Below => {
            (ops.store_changed)(table, now, was, doc, Slot::Key(obj, key), dirty).located(key)
        }
    }
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
    // A record whose map is gone (deleted by a change this one raced) is
    // written whole, as a whole write would: writing only the fields that
    // changed into a map made now would leave the others missing, an item
    // no replica can read.
    if !matches!(slot.current(doc)?, Some((Value::Object(ObjType::Map), _))) {
        return store(record, value, doc, slot);
    }
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
    refresh_slot(field.table, target, doc, obj, field.key, delta)?;
    if field.whole {
        (field.table.ops.read_whole)(field.table, target, doc).located(field.key)?;
    }
    Ok(())
}

/// Bring `target`, read earlier from `key` of the map `obj`, up to date
/// after `delta`, through `table`: what [`crate::model::refresh_key`] does.
///
/// # Errors
/// Why it does not read, located at `key`.
pub(crate) fn refresh_slot(
    table: &ModelTable,
    target: &mut dyn Any,
    doc: &Doc,
    obj: &ObjId,
    key: &str,
    delta: &Delta,
) -> Result<(), ModelError> {
    let ops = table.ops;
    match delta.key(obj, key) {
        Touch::Untouched => Ok(()),
        Touch::Written => {
            let slot = crate::model::get_slot(doc, obj, key).located(key)?;
            (ops.reload)(
                table,
                target,
                doc,
                slot.as_ref().map(|(value, id)| (value, id)),
            )
            .located(key)
        }
        Touch::Below => match crate::model::get_slot(doc, obj, key).located(key)? {
            Some((value, id)) => (ops.refresh)(table, target, doc, &value, &id, delta).located(key),
            None => (ops.reload)(table, target, doc, None).located(key),
        },
    }
}

/// Put what `slot` holds into `out`, an `Option` of `table`'s type: its
/// value, or its missing value when it holds nothing.
///
/// # Errors
/// Why it does not read.
pub(crate) fn load_slot(
    table: &ModelTable,
    doc: &Doc,
    slot: Option<(&Value<'_>, &ObjId)>,
    out: &mut dyn Any,
) -> Result<(), ModelError> {
    match slot {
        Some((value, id)) => (table.ops.load)(table, doc, value, id, out),
        None => (table.ops.missing)(table, out),
    }
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
    // Builds with debug assertions (tests, dev runs) check the record
    // whole after an edit its declared reads say cannot break it, so a
    // declaration that leaves out a field the check reads is caught.
    #[cfg(debug_assertions)]
    let unchecked_was_valid = skips_check(record, path).then(|| valid_at(record, doc, obj));
    match path.split_first() {
        Some((Segment::Field(key), rest)) => {
            let Some(field) = record.fields.iter().find(|field| field.key == key) else {
                let editable: Vec<&str> = record
                    .fields
                    .iter()
                    .filter(|field| !field.readonly)
                    .map(|field| field.key)
                    .collect();
                return Err(ModelError::invalid(format!(
                    "no field `{key}`; the fields here are {}",
                    editable.join(", ")
                )));
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
    if let Some(own) = &record.own_check
        && own
            .reads
            .as_ref()
            .is_none_or(|reads| reads.touched_by(path))
    {
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
    #[cfg(debug_assertions)]
    if unchecked_was_valid == Some(true) {
        guard_declared_reads(record, doc, obj, path)?;
    }
    Ok(())
}

/// Whether an edit at `path` skips `record`'s own check, by what the check
/// declares it reads.
#[cfg(debug_assertions)]
fn skips_check(record: &RecordDesc, path: &[Segment]) -> bool {
    record
        .own_check
        .as_ref()
        .and_then(|own| own.reads.as_ref())
        .is_some_and(|reads| !reads.touched_by(path))
}

/// Whether the record at `obj` passes its own check.
#[cfg(debug_assertions)]
fn valid_at(record: &RecordDesc, doc: &Doc, obj: &ObjId) -> bool {
    record.own_check.as_ref().is_none_or(|own| {
        (own.read)(doc, obj).is_ok_and(|current| (own.check)(current.as_ref()).is_ok())
    })
}

/// Check the record at `obj`, valid before an edit at `path` its declared
/// reads skip, once the edit (or its group) is done; a refusal means the
/// declaration leaves out a field the check reads.
#[cfg(debug_assertions)]
fn guard_declared_reads(
    record: &RecordDesc,
    doc: &Doc,
    obj: &ObjId,
    path: &[Segment],
) -> Result<(), ModelError> {
    let Some(own) = &record.own_check else {
        return Ok(());
    };
    let (read, own_check, name) = (own.read, own.check, record.name);
    let at = path
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("/");
    let checked = obj.clone();
    crate::group::check_or_defer(format!("{}@{:?}#reads", name(), obj), doc, move |doc| {
        if let Ok(current) = read(doc, &checked)
            && let Err(refusal) = own_check(current.as_ref())
        {
            undeclared_read(name(), &at, &refusal);
        }
        Ok(())
    })
    .map_err(ModelError::refused)
}

#[cfg(debug_assertions)]
#[allow(
    clippy::panic,
    reason = "a check that reads what it does not declare is a bug in the record's declaration, \
              which only builds with debug assertions look for"
)]
fn undeclared_read(name: &str, at: &str, refusal: &Refusal) -> ! {
    panic!(
        "`{name}`'s check refuses an edit at `{at}` ({refusal}), but its `check_reads` / \
         `check_items` say that edit cannot change what it reads: add the field it reads"
    )
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
/// key. A field that would store as it is ([`Model::stores_as`]) has
/// nothing to submit and is passed over, compared in memory: no path made
/// for it, nothing turned into JSON. The path of a field compared further
/// is made once, in one buffer.
pub fn diff(
    record: &RecordDesc,
    before: &dyn Any,
    after: &dyn Any,
    path: &[Segment],
    out: &mut Vec<Edit<()>>,
) {
    let mut at: Vec<Segment> = Vec::new();
    for (index, field) in record.fields.iter().enumerate() {
        if field.readonly {
            continue;
        }
        let (Some(was), Some(now)) = ((record.field)(before, index), (record.field)(after, index))
        else {
            continue;
        };
        if (field.table.ops.stores_as)(field.table, now, was) {
            continue;
        }
        if at.is_empty() {
            at.reserve_exact(path.len().saturating_add(1));
            at.extend_from_slice(path);
        } else {
            at.truncate(path.len());
        }
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

/// The value in a filled `Option` of its type.
pub(crate) type Unstage = for<'a> fn(&'a dyn Any) -> Option<&'a dyn Any>;

/// [`Model::edit`] of a value edited only as a whole, through its table:
/// `set` at its own path, checked before it is written. `staged` is an empty
/// `Option` of the type, which `unstage` reads the value back out of.
pub(crate) fn edit_whole(
    table: &ModelTable,
    (staged, unstage): (Box<dyn Any>, Unstage),
    nullable: bool,
    doc: &mut Doc,
    slot: Slot<'_>,
    path: &[Segment],
    op: &EditOp,
) -> Result<(), ModelError> {
    if let Some(segment) = path.first() {
        return Err(ModelError::invalid(format!(
            "nothing inside this value is called `{segment}`"
        )));
    }
    match op {
        EditOp::Set { value } if value.is_null() && !nullable => Err(ModelError::invalid(
            "`null` is no value of this field: set a value of its type",
        )),
        EditOp::Set { value } => {
            let mut staged = staged;
            (table.ops.from_json)(table, value, staged.as_mut())?;
            let made = unstage(staged.as_ref()).ok_or_else(mismatch)?;
            (table.ops.check)(table, made).map_err(ModelError::refused)?;
            (table.ops.store)(table, made, doc, slot)
        }
        other => Err(ModelError::unsupported(other)),
    }
}

/// What `Option<T>`'s table reaches into one with.
#[allow(
    clippy::type_complexity,
    reason = "each field reaches into the option one way"
)]
pub struct OptionDesc {
    /// `T`'s table.
    pub inner: &'static ModelTable,
    /// The present value of an option.
    pub get: for<'a> fn(&'a dyn Any) -> Option<&'a dyn Any>,
    /// The same, mutably.
    pub get_mut: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn Any>,
    /// Put an absent option into `out` (an `Option` of the option) and hand
    /// back that option, for `T`'s table to put a value into.
    pub slot: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn Any>,
    /// Make an option absent.
    pub clear: fn(&mut dyn Any),
    /// An empty `Option` of the option, and the option in a filled one.
    pub staged: (
        fn() -> Box<dyn Any>,
        for<'a> fn(&'a dyn Any) -> Option<&'a dyn Any>,
    ),
}

/// The [`OptionDesc`] of `Option<T>`.
pub struct OptionOf<T>(PhantomData<T>);

impl<T: Model> OptionOf<T> {
    pub const DESC: OptionDesc = OptionDesc {
        inner: T::TABLE,
        get: option_get::<T>,
        get_mut: option_get_mut::<T>,
        slot: option_slot::<T>,
        clear: option_clear::<T>,
        staged: (empty_of::<Option<T>>, unwrap_of::<Option<T>>),
    };

    /// `Option<T>`'s table.
    pub const TABLE: ModelTable = ModelTable {
        ops: &OPTION_OPS,
        desc: Desc::Option(&Self::DESC),
        checks: T::CHECKS,
        has_ops: T::OPS,
        scalar: T::SCALAR,
    };
}

fn option_get<T: 'static>(value: &dyn Any) -> Option<&dyn Any> {
    match value.downcast_ref::<Option<T>>()? {
        Some(inner) => Some(inner),
        None => None,
    }
}

fn option_get_mut<T: 'static>(value: &mut dyn Any) -> Option<&mut dyn Any> {
    match value.downcast_mut::<Option<T>>()? {
        Some(inner) => Some(inner),
        None => None,
    }
}

fn option_slot<T: 'static>(out: &mut dyn Any) -> Option<&mut dyn Any> {
    let out = out.downcast_mut::<Option<Option<T>>>()?;
    Some(out.insert(None))
}

fn option_clear<T: 'static>(value: &mut dyn Any) {
    if let Some(value) = value.downcast_mut::<Option<T>>() {
        *value = None;
    }
}

fn optional(table: &ModelTable) -> Result<&'static OptionDesc, ModelError> {
    table.option().ok_or_else(mismatch)
}

fn is_null(value: &Value<'_>) -> bool {
    matches!(value, Value::Scalar(scalar) if matches!(scalar.as_ref(), automerge::ScalarValue::Null))
}

/// The functions of every `Option`: `None` is an absent key (or a null list
/// element).
pub static OPTION_OPS: ModelOps = ModelOps {
    without_absent: crate::shape::kept_whole,
    to_json: |table, value| match table.option() {
        Some(option) => match (option.get)(value) {
            Some(inner) => (option.inner.ops.to_json)(option.inner, inner),
            None => serde_json::Value::Null,
        },
        None => serde_json::Value::Null,
    },
    from_json: |table, value, out| {
        let option = optional(table)?;
        let slot = (option.slot)(out).ok_or_else(mismatch)?;
        if value.is_null() {
            return Ok(());
        }
        (option.inner.ops.from_json)(option.inner, value, slot)
    },
    load: |table, doc, value, id, out| {
        let option = optional(table)?;
        let slot = (option.slot)(out).ok_or_else(mismatch)?;
        if is_null(value) {
            return Ok(());
        }
        (option.inner.ops.load)(option.inner, doc, value, id, slot)
    },
    missing: |table, out| {
        let option = optional(table)?;
        (option.slot)(out).ok_or_else(mismatch)?;
        Ok(())
    },
    reload: |table, target, doc, slot| {
        let option = optional(table)?;
        match slot {
            Some((value, id)) if !is_null(value) => {
                // The option is itself where `T`'s table puts a value.
                (option.inner.ops.load)(option.inner, doc, value, id, target)
            }
            _ => {
                (option.clear)(target);
                Ok(())
            }
        }
    },
    store: |table, value, doc, slot| {
        let option = optional(table)?;
        match (option.get)(value) {
            Some(inner) => (option.inner.ops.store)(option.inner, inner, doc, slot),
            None => slot.remove(doc),
        }
    },
    stores_as: |table, value, before| {
        let Some(option) = table.option() else {
            return false;
        };
        match ((option.get)(value), (option.get)(before)) {
            (Some(value), Some(before)) => {
                (option.inner.ops.stores_as)(option.inner, value, before)
            }
            (None, None) => true,
            _ => false,
        }
    },
    store_changed: |table, value, before, doc, slot, dirty| {
        let option = optional(table)?;
        match ((option.get)(value), (option.get)(before)) {
            (Some(value), Some(before)) => {
                (option.inner.ops.store_changed)(option.inner, value, before, doc, slot, dirty)
            }
            _ => (OPTION_OPS.store)(table, value, doc, slot),
        }
    },
    refresh: |table, target, doc, value, id, delta| {
        let option = optional(table)?;
        if !is_null(value)
            && let Some(inner) = (option.get_mut)(target)
        {
            return (option.inner.ops.refresh)(option.inner, inner, doc, value, id, delta);
        }
        (OPTION_OPS.reload)(table, target, doc, Some((value, id)))
    },
    read_whole: |_, _, _| Ok(()),
    edit: |table, doc, slot, path, op, cx| {
        let option = optional(table)?;
        if path.is_empty() && matches!(op, EditOp::Set { .. }) {
            let staged = ((option.staged.0)(), option.staged.1);
            return edit_whole(table, staged, true, doc, slot, path, op);
        }
        match slot.current(doc)? {
            Some((value, _)) if !is_null(&value) => {
                (option.inner.ops.edit)(option.inner, doc, slot, path, op, cx)
            }
            _ => Err(ModelError::invalid("the value is absent")),
        }
    },
    diff: |table, before, after, path, out| {
        let Some(option) = table.option() else {
            return;
        };
        match ((option.get)(before), (option.get)(after)) {
            (Some(before), Some(after)) => {
                (option.inner.ops.diff)(option.inner, before, after, path, out);
            }
            (None, None) => {}
            _ => out.push(Edit::new(
                path.to_vec(),
                EditOp::Set {
                    value: (OPTION_OPS.to_json)(table, after),
                },
            )),
        }
    },
    collect_text: |table, value, out| {
        if let Some(option) = table.option()
            && let Some(inner) = (option.get)(value)
        {
            (option.inner.ops.collect_text)(option.inner, inner, out);
        }
    },
    check: |table, value| match table.option() {
        Some(option) => match (option.get)(value) {
            Some(inner) => (option.inner.ops.check)(option.inner, inner),
            None => Ok(()),
        },
        None => Ok(()),
    },
    get_at: |table, value, path| {
        if path.is_empty() {
            return Some(value);
        }
        let option = table.option()?;
        (option.inner.ops.get_at)(option.inner, (option.get)(value)?, path)
    },
    label: |table, value| {
        let option = table.option()?;
        (option.inner.ops.label)(option.inner, (option.get)(value)?)
    },
    nested_collections: no_nested,
    op_coalesces: |table, path, previous, next| {
        let option = table.option()?;
        (option.inner.ops.op_coalesces)(option.inner, path, previous, next)
    },
    op_continuable: |table, path, op| {
        let option = table.option()?;
        (option.inner.ops.op_continuable)(option.inner, path, op)
    },
    op_describe: |table, path, op| {
        let option = table.option()?;
        (option.inner.ops.op_describe)(option.inner, path, op)
    },
    op_lands: |table, value, path, op| {
        let option = table.option()?;
        (option.inner.ops.op_lands)(option.inner, (option.get)(value)?, path, op)
    },
    op_schemas: |table, prefix, out| {
        if let Some(option) = table.option() {
            (option.inner.ops.op_schemas)(option.inner, prefix, out);
        }
    },
};

/// A value of a type whose table does the work ([`Model::to_json`] of an
/// `Option`, a `Collection`): the type's functions as the table runs them.
pub struct Run<'t>(pub &'t ModelTable);

impl Run<'_> {
    #[must_use]
    pub fn to_json(&self, value: &dyn Any) -> serde_json::Value {
        (self.0.ops.to_json)(self.0, value)
    }

    /// # Errors
    /// Why `value` is no JSON form of the type.
    pub fn from_json<T: 'static>(&self, value: &serde_json::Value) -> Result<T, ModelError> {
        let mut out: Option<T> = None;
        (self.0.ops.from_json)(self.0, value, &mut out)?;
        filled(out)
    }

    /// # Errors
    /// Why the stored value is not the type.
    pub fn load<T: 'static>(
        &self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
    ) -> Result<T, ModelError> {
        let mut out: Option<T> = None;
        (self.0.ops.load)(self.0, doc, value, id, &mut out)?;
        filled(out)
    }

    /// # Errors
    /// Why the type has no value without one stored.
    pub fn missing<T: 'static>(&self) -> Result<T, ModelError> {
        let mut out: Option<T> = None;
        (self.0.ops.missing)(self.0, &mut out)?;
        filled(out)
    }

    /// # Errors
    /// The document's write error.
    pub fn store(&self, value: &dyn Any, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        (self.0.ops.store)(self.0, value, doc, slot)
    }

    #[must_use]
    pub fn stores_as(&self, value: &dyn Any, before: &dyn Any) -> bool {
        (self.0.ops.stores_as)(self.0, value, before)
    }

    /// # Errors
    /// The document's write error.
    pub fn store_changed(
        &self,
        value: &dyn Any,
        before: &dyn Any,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        (self.0.ops.store_changed)(self.0, value, before, doc, slot, dirty)
    }

    /// # Errors
    /// Why the stored value is not the type.
    pub fn refresh(
        &self,
        target: &mut dyn Any,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        (self.0.ops.refresh)(self.0, target, doc, value, id, delta)
    }

    /// # Errors
    /// Why the path or operation does not fit, or a refusal.
    pub fn edit(
        &self,
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        (self.0.ops.edit)(self.0, doc, slot, path, op, cx)
    }

    pub fn diff<E>(
        &self,
        before: &dyn Any,
        after: &dyn Any,
        path: &[Segment],
        out: &mut Vec<Edit<E>>,
    ) {
        let mut found = Vec::new();
        (self.0.ops.diff)(self.0, before, after, path, &mut found);
        out.reserve(found.len());
        for edit in found {
            out.push(edit.retyped());
        }
    }

    pub fn collect_text(&self, value: &dyn Any, out: &mut Vec<String>) {
        (self.0.ops.collect_text)(self.0, value, out);
    }

    /// # Errors
    /// The first refusal.
    pub fn check(&self, value: &dyn Any) -> Result<(), Refusal> {
        (self.0.ops.check)(self.0, value)
    }

    #[must_use]
    pub fn get_at<'a>(&self, value: &'a dyn Any, path: &[Segment]) -> Option<&'a dyn Any> {
        (self.0.ops.get_at)(self.0, value, path)
    }

    #[must_use]
    pub fn op_coalesces(
        &self,
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        (self.0.ops.op_coalesces)(self.0, path, previous, next)
    }

    #[must_use]
    pub fn op_continuable(&self, path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        (self.0.ops.op_continuable)(self.0, path, op)
    }

    #[must_use]
    pub fn op_describe(&self, path: &[Segment], op: &serde_json::Value) -> Option<OpDescription> {
        (self.0.ops.op_describe)(self.0, path, op)
    }

    #[must_use]
    pub fn op_lands(
        &self,
        value: &dyn Any,
        path: &[Segment],
        op: &serde_json::Value,
    ) -> Option<Place> {
        (self.0.ops.op_lands)(self.0, value, path, op)
    }

    pub fn op_schemas(&self, prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        (self.0.ops.op_schemas)(self.0, prefix, out);
    }
}

/// A `#[derive(Model)]` enum of unit variants, each stored as its name.
pub struct UnitDesc {
    /// The variants' stored names, in declaration order.
    pub names: &'static [&'static str],
    /// The `#[default]` variant's position.
    pub default: Option<usize>,
    /// The position of a value's variant.
    pub index: fn(&dyn Any) -> Option<usize>,
    /// Put the variant at a position into `out`, an `Option` of the enum.
    pub make: fn(usize, &mut dyn Any) -> Result<(), ModelError>,
    /// Set a value to the variant at a position.
    pub set: fn(usize, &mut dyn Any) -> Result<(), ModelError>,
    /// An empty `Option` of the enum and the enum in a filled one.
    pub staged: (fn() -> Box<dyn Any>, Unstage),
}

impl ModelTable {
    /// The table of an enum of unit variants.
    #[must_use]
    pub const fn of_unit(unit: &'static UnitDesc) -> Self {
        Self {
            ops: &UNIT_OPS,
            desc: Desc::Unit(unit),
            checks: false,
            has_ops: false,
            scalar: true,
        }
    }

    const fn unit(&self) -> Option<&'static UnitDesc> {
        match self.desc {
            Desc::Unit(unit) => Some(unit),
            _ => None,
        }
    }
}

fn units(table: &ModelTable) -> Result<&'static UnitDesc, ModelError> {
    table.unit().ok_or_else(mismatch)
}

/// The stored name of `value`'s variant.
fn unit_name(table: &ModelTable, value: &dyn Any) -> Option<&'static str> {
    let unit = table.unit()?;
    unit.names.get((unit.index)(value)?).copied()
}

/// The position of the variant named `text`.
fn unit_named(unit: &UnitDesc, text: &str) -> Result<usize, ModelError> {
    unit.names
        .iter()
        .position(|name| *name == text)
        .ok_or_else(|| ModelError::invalid(format!("unknown variant `{text}`")))
}

/// [`Model::value_schema`] of an enum of the variants `names`.
#[must_use]
pub fn unit_schema(names: &[&str]) -> serde_json::Value {
    serde_json::json!({"type": "string", "enum": names})
}

/// The functions of every enum of unit variants.
pub static UNIT_OPS: ModelOps = ModelOps {
    without_absent: crate::shape::kept_whole,
    to_json: |table, value| {
        unit_name(table, value).map_or(serde_json::Value::Null, |name| {
            serde_json::Value::String(name.to_owned())
        })
    },
    from_json: |table, value, out| {
        let unit = units(table)?;
        match value {
            serde_json::Value::String(text) => (unit.make)(unit_named(unit, text)?, out),
            serde_json::Value::Null => (UNIT_OPS.missing)(table, out),
            _ => Err(ModelError::shape("a variant name")),
        }
    },
    load: |table, doc, value, id, out| {
        let unit = units(table)?;
        let crate::Atom(text) = crate::Atom::load(doc, value, id)?;
        (unit.make)(unit_named(unit, &text)?, out)
    },
    missing: |table, out| {
        let unit = units(table)?;
        match unit.default {
            Some(default) => (unit.make)(default, out),
            None => Err(ModelError::Missing {
                path: String::new(),
            }),
        }
    },
    reload: |table, target, doc, slot| {
        let unit = units(table)?;
        let index = match slot {
            Some((value, id)) => {
                let crate::Atom(text) = crate::Atom::load(doc, value, id)?;
                unit_named(unit, &text)?
            }
            None => unit.default.ok_or_else(|| ModelError::Missing {
                path: String::new(),
            })?,
        };
        (unit.set)(index, target)
    },
    store: |table, value, doc, slot| {
        let name = unit_name(table, value).ok_or_else(mismatch)?;
        slot.put_scalar(doc, automerge::ScalarValue::Str(name.into()))
    },
    stores_as: |table, value, before| {
        table.unit().is_some_and(|unit| {
            (unit.index)(value).is_some() && (unit.index)(value) == (unit.index)(before)
        })
    },
    store_changed: |table, value, _, doc, slot, _| (UNIT_OPS.store)(table, value, doc, slot),
    refresh: |table, target, doc, value, id, _| {
        (UNIT_OPS.reload)(table, target, doc, Some((value, id)))
    },
    read_whole: |_, _, _| Ok(()),
    edit: |table, doc, slot, path, op, _| {
        let unit = units(table)?;
        edit_whole(
            table,
            ((unit.staged.0)(), unit.staged.1),
            false,
            doc,
            slot,
            path,
            op,
        )
    },
    diff: |table, before, after, path, out| {
        let value = (UNIT_OPS.to_json)(table, after);
        if (UNIT_OPS.to_json)(table, before) != value {
            out.push(Edit::new(path.to_vec(), EditOp::Set { value }));
        }
    },
    collect_text: |table, value, out| {
        if let Some(name) = unit_name(table, value) {
            out.push(name.to_owned());
        }
    },
    check: |_, _| Ok(()),
    get_at: |_, value, path| path.is_empty().then_some(value),
    label: no_label,
    nested_collections: no_nested,
    op_coalesces: no_coalesces,
    op_continuable: no_continuable,
    op_describe: no_describe,
    op_lands: no_lands,
    op_schemas: no_schemas,
};

/// Put `variant` into `out`, an `Option` of its enum.
///
/// # Errors
/// When there is no such variant or `out` is of another type.
pub fn put_variant<T: 'static>(variant: Option<T>, out: &mut dyn Any) -> Result<(), ModelError> {
    put(out, variant.ok_or_else(mismatch)?)
}

/// Set `target` to `variant`.
///
/// # Errors
/// When there is no such variant or `target` is of another type.
pub fn set_variant<T: 'static>(variant: Option<T>, target: &mut dyn Any) -> Result<(), ModelError> {
    *target.downcast_mut::<T>().ok_or_else(mismatch)? = variant.ok_or_else(mismatch)?;
    Ok(())
}

/// A type stored whole through its serde form: as one JSON string, last
/// writer winning ([`json_model!`](crate::json_model)), or as that form's
/// maps, lists and scalars ([`value_model!`](crate::value_model)).
#[allow(
    clippy::type_complexity,
    reason = "each field converts the value one way"
)]
pub struct JsonDesc {
    /// The value's JSON form.
    pub to_value: fn(&dyn Any) -> serde_json::Value,
    /// The value's JSON form, or why it has none: what its stored text is
    /// written from (`json_text`).
    pub try_value: fn(&dyn Any) -> Result<serde_json::Value, ModelError>,
    /// The value serialised, as compact JSON text: equal texts are equal
    /// JSON forms, so two values compare without building either form.
    pub bytes: fn(&dyn Any) -> Option<Vec<u8>>,
    /// Whether two values are equal in memory, for a type whose equal values
    /// have equal JSON forms (a derived `PartialEq`): a diff passes over an
    /// equal pair without serialising either. `None` when the type has no
    /// such equality; unequal values are still compared by their forms.
    pub equal: Option<fn(&dyn Any, &dyn Any) -> bool>,
    /// Put the value of a JSON form into `out`, an `Option` of the type. The
    /// stored text is read through it too (`json_text`).
    pub from_value: fn(&serde_json::Value, &mut dyn Any) -> Result<(), ModelError>,
    /// Put the type's default into `out`.
    pub default: fn(&mut dyn Any) -> Result<(), ModelError>,
    /// `null` reads as the default rather than through serde.
    pub null_default: bool,
    /// Stored as its JSON form's maps, lists and scalars
    /// ([`value_model!`](crate::value_model)) rather than as JSON text.
    pub structural: bool,
    /// An empty `Option` of the type, the value in a filled one, and moving
    /// it out over another.
    pub staged: (fn() -> Box<dyn Any>, Unstage),
    pub replace: fn(&mut dyn Any, &mut dyn Any) -> Result<(), ModelError>,
}

impl ModelTable {
    /// The table of a type stored as JSON text.
    #[must_use]
    pub const fn of_json(json: &'static JsonDesc) -> Self {
        Self {
            ops: &JSON_OPS,
            desc: Desc::Json(json),
            checks: false,
            has_ops: false,
            scalar: false,
        }
    }

    const fn json(&self) -> Option<&'static JsonDesc> {
        match self.desc {
            Desc::Json(json) => Some(json),
            _ => None,
        }
    }
}

fn jsons(table: &ModelTable) -> Result<&'static JsonDesc, ModelError> {
    table.json().ok_or_else(mismatch)
}

/// An error of serde as a model error.
#[allow(clippy::needless_pass_by_value, reason = "what `map_err` hands it")]
fn serde_invalid(error: serde_json::Error) -> ModelError {
    ModelError::invalid(error.to_string())
}

/// Put the value the stored JSON `text` holds into `out`: parsed once as
/// JSON, then read as the type through its one JSON reader.
fn json_from_text(json: &JsonDesc, text: &str, out: &mut dyn Any) -> Result<(), ModelError> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(serde_invalid)?;
    (json.from_value)(&value, out)
}

/// The JSON text a value of a type stored as JSON text is stored as.
fn json_text(json: &JsonDesc, value: &dyn Any) -> Result<String, ModelError> {
    (json.try_value)(value).map(|value| value.to_string())
}

/// The serde functions of a type stored as JSON text.
pub struct JsonOf<T>(PhantomData<T>);

impl<T: serde::Serialize + serde::de::DeserializeOwned + Default + 'static> JsonOf<T> {
    /// A type stored as itself, compared only by its JSON form.
    pub const PLAIN: JsonDesc = JsonDesc {
        to_value: |value| {
            value
                .downcast_ref::<T>()
                .and_then(|value| crate::__private::json_via_bytes(value).ok())
                .unwrap_or(serde_json::Value::Null)
        },
        try_value: |value| {
            crate::__private::json_via_bytes(value.downcast_ref::<T>().ok_or_else(mismatch)?)
                .map_err(serde_invalid)
        },
        bytes: |value| serde_json::to_vec(value.downcast_ref::<T>()?).ok(),
        equal: None,
        from_value: |value, out| {
            put::<T>(
                out,
                serde_json::from_value(value.clone()).map_err(serde_invalid)?,
            )
        },
        default: |out| put(out, T::default()),
        null_default: true,
        structural: false,
        staged: (empty_of::<T>, unwrap_of::<T>),
        replace: replace_of::<T>,
    };
}

impl<T> JsonOf<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default + PartialEq + 'static,
{
    /// A [`json_model!`](crate::json_model) type, compared in memory first:
    /// its `PartialEq` holds only between values of equal JSON forms, as a
    /// derived one does, so an equal pair needs neither serialised.
    pub const COMPARED: JsonDesc = JsonDesc {
        equal: Some(|value, other| {
            matches!(
                (value.downcast_ref::<T>(), other.downcast_ref::<T>()),
                (Some(value), Some(other)) if value == other
            )
        }),
        ..Self::PLAIN
    };
}

/// The serde functions of a [`value_model!`](crate::value_model) type,
/// stored as its JSON form's maps, lists and scalars.
pub struct ValueOf<T>(PhantomData<T>);

impl<T> ValueOf<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + 'static,
{
    /// A type with no value of its own for an absent slot.
    pub const DESC: JsonDesc = JsonDesc {
        to_value: |value| {
            value
                .downcast_ref::<T>()
                .and_then(|value| crate::__private::json_via_bytes(value).ok())
                .unwrap_or(serde_json::Value::Null)
        },
        try_value: |value| {
            crate::__private::json_via_bytes(value.downcast_ref::<T>().ok_or_else(mismatch)?)
                .map_err(serde_invalid)
        },
        bytes: |value| serde_json::to_vec(value.downcast_ref::<T>()?).ok(),
        equal: Some(|value, other| {
            matches!(
                (value.downcast_ref::<T>(), other.downcast_ref::<T>()),
                (Some(value), Some(other)) if value == other
            )
        }),
        from_value: |value, out| {
            put::<T>(
                out,
                serde_json::from_value(value.clone()).map_err(serde_invalid)?,
            )
        },
        default: |_| {
            Err(ModelError::Missing {
                path: String::new(),
            })
        },
        null_default: false,
        structural: true,
        staged: (empty_of::<T>, unwrap_of::<T>),
        replace: replace_of::<T>,
    };
}

impl<T> ValueOf<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + Default + 'static,
{
    /// A type whose `Default` is the value of an absent slot.
    pub const DEFAULTED: JsonDesc = JsonDesc {
        default: |out| put(out, T::default()),
        null_default: true,
        ..Self::DESC
    };
}

/// The functions of every type stored as JSON text.
pub static JSON_OPS: ModelOps = ModelOps {
    without_absent: crate::shape::kept_whole,
    to_json: |table, value| match table.json() {
        Some(json) => (json.to_value)(value),
        None => serde_json::Value::Null,
    },
    from_json: |table, value, out| {
        let json = jsons(table)?;
        if json.null_default && value.is_null() {
            return (json.default)(out);
        }
        (json.from_value)(value, out)
    },
    load: |table, doc, value, id, out| {
        let json = jsons(table)?;
        if json.structural {
            let read = <serde_json::Value as Model>::load(doc, value, id)?;
            if read.is_null() && json.null_default {
                return (json.default)(out);
            }
            return (json.from_value)(&read, out);
        }
        match value {
            Value::Scalar(scalar) => match scalar.as_ref() {
                automerge::ScalarValue::Str(text) => json_from_text(json, text, out),
                automerge::ScalarValue::Null => (json.default)(out),
                _ => Err(ModelError::shape("a JSON string")),
            },
            Value::Object(_) => Err(ModelError::shape("a JSON string")),
        }
    },
    missing: |table, out| (jsons(table)?.default)(out),
    reload: |table, target, doc, slot| {
        let json = jsons(table)?;
        let mut staged = (json.staged.0)();
        match slot {
            Some((value, id)) => (JSON_OPS.load)(table, doc, value, id, staged.as_mut())?,
            None => (json.default)(staged.as_mut())?,
        }
        (json.replace)(target, staged.as_mut())
    },
    store: |table, value, doc, slot| {
        let json = jsons(table)?;
        if json.structural {
            return (json.try_value)(value)?.store(doc, slot);
        }
        let text = json_text(json, value)?;
        slot.put_scalar(doc, automerge::ScalarValue::Str(text.into()))
    },
    // Equal values of a structural type store the same maps, lists and
    // scalars; JSON text is written again, as it always was.
    stores_as: |table, value, before| {
        table.json().is_some_and(|json| {
            json.structural && json.equal.is_some_and(|equal| equal(value, before))
        })
    },
    store_changed: |table, value, _, doc, slot, _| (JSON_OPS.store)(table, value, doc, slot),
    refresh: |table, target, doc, value, id, _| {
        (JSON_OPS.reload)(table, target, doc, Some((value, id)))
    },
    read_whole: |_, _, _| Ok(()),
    edit: |table, doc, slot, path, op, _| {
        let json = jsons(table)?;
        edit_whole(
            table,
            ((json.staged.0)(), json.staged.1),
            true,
            doc,
            slot,
            path,
            op,
        )
    },
    diff: |table, before, after, path, out| {
        // Values equal in memory have equal forms; the same text is the same
        // form; a different text may still be (a map's keys serialised in
        // another order), so the forms decide.
        if let Some(json) = table.json() {
            if json.equal.is_some_and(|equal| equal(before, after)) {
                return;
            }
            if let Some(was) = (json.bytes)(before)
                && (json.bytes)(after).is_some_and(|now| now == was)
            {
                return;
            }
        }
        let value = (JSON_OPS.to_json)(table, after);
        if (JSON_OPS.to_json)(table, before) != value {
            out.push(Edit::new(path.to_vec(), EditOp::Set { value }));
        }
    },
    collect_text: |_, _, _| {},
    check: |_, _| Ok(()),
    get_at: |_, value, path| path.is_empty().then_some(value),
    label: no_label,
    nested_collections: no_nested,
    op_coalesces: no_coalesces,
    op_continuable: no_continuable,
    op_describe: no_describe,
    op_lands: no_lands,
    op_schemas: no_schemas,
};

/// A list's items, whatever their type.
pub trait ListItems {
    fn count(&self) -> usize;
    fn item_at(&self, at: usize) -> Option<&dyn Any>;
    /// Append an item, put by `fill` into an `Option` of the item type.
    ///
    /// # Errors
    /// What `fill` refused.
    fn push_filled(
        &mut self,
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError>;
}

impl<T: 'static> ListItems for Vec<T> {
    fn count(&self) -> usize {
        self.len()
    }

    fn item_at(&self, at: usize) -> Option<&dyn Any> {
        self.get(at).map(|item| item as &dyn Any)
    }

    fn push_filled(
        &mut self,
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let mut slot: Option<T> = None;
        fill(&mut slot)?;
        self.push(filled(slot)?);
        Ok(())
    }
}

/// What `Vec<T>`'s table reaches into one with.
#[allow(
    clippy::type_complexity,
    reason = "each field reaches into the list one way"
)]
pub struct VecDesc {
    item: &'static ModelTable,
    items: for<'a> fn(&'a dyn Any) -> Option<&'a dyn ListItems>,
    /// Put an empty list into `out` (an `Option` of it), and hand it back.
    fresh: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn ListItems>,
    staged: (fn() -> Box<dyn Any>, Unstage),
    replace: fn(&mut dyn Any, &mut dyn Any) -> Result<(), ModelError>,
}

/// The [`VecDesc`] of `Vec<T>`.
pub struct VecOf<T>(PhantomData<T>);

impl<T: Model> VecOf<T> {
    pub const DESC: VecDesc = VecDesc {
        item: T::TABLE,
        items: |value| Some(value.downcast_ref::<Vec<T>>()? as &dyn ListItems),
        fresh: |out| {
            let out = out.downcast_mut::<Option<Vec<T>>>()?;
            Some(out.insert(Vec::new()) as &mut dyn ListItems)
        },
        staged: (empty_of::<Vec<T>>, unwrap_of::<Vec<T>>),
        replace: replace_of::<Vec<T>>,
    };

    pub const TABLE: ModelTable = ModelTable {
        ops: &VEC_OPS,
        desc: Desc::Vec(&Self::DESC),
        checks: T::CHECKS,
        has_ops: T::OPS,
        scalar: false,
    };
}

impl ModelTable {
    const fn list(&self) -> Option<&'static VecDesc> {
        match self.desc {
            Desc::Vec(list) => Some(list),
            _ => None,
        }
    }
}

fn lists(table: &ModelTable) -> Result<&'static VecDesc, ModelError> {
    table.list().ok_or_else(mismatch)
}

/// The functions of every `Vec`: a list of values.
pub static VEC_OPS: ModelOps = ModelOps {
    without_absent: crate::shape::kept_whole,
    to_json: |table, value| {
        let (Some(list), Some(items)) = (
            table.list(),
            table.list().and_then(|list| (list.items)(value)),
        ) else {
            return serde_json::Value::Null;
        };
        let mut out = Vec::with_capacity(items.count());
        for at in 0..items.count() {
            if let Some(item) = items.item_at(at) {
                out.push((list.item.ops.to_json)(list.item, item));
            }
        }
        serde_json::Value::Array(out)
    },
    from_json: |table, value, out| {
        let list = lists(table)?;
        let fresh = (list.fresh)(out).ok_or_else(mismatch)?;
        match value {
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    fresh.push_filled(&mut |into| {
                        (list.item.ops.from_json)(list.item, item, into).located(&index.to_string())
                    })?;
                }
                Ok(())
            }
            serde_json::Value::Null => Ok(()),
            _ => Err(ModelError::shape("a list")),
        }
    },
    load: |table, doc, value, id, out| {
        let list = lists(table)?;
        if !matches!(value, Value::Object(ObjType::List)) {
            return Err(ModelError::shape("a list"));
        }
        let fresh = (list.fresh)(out).ok_or_else(mismatch)?;
        for index in 0..automerge::ReadDoc::length(doc, id) {
            let slot = crate::model::get_item(doc, id, index)?;
            fresh.push_filled(&mut |into| {
                load_slot(list.item, doc, slot.as_ref().map(|(v, i)| (v, i)), into)
                    .located(&index.to_string())
            })?;
        }
        Ok(())
    },
    missing: |table, out| {
        (lists(table)?.fresh)(out).ok_or_else(mismatch)?;
        Ok(())
    },
    reload: |table, target, doc, slot| {
        let list = lists(table)?;
        let mut staged = (list.staged.0)();
        match slot {
            Some((value, id)) => (VEC_OPS.load)(table, doc, value, id, staged.as_mut())?,
            None => (VEC_OPS.missing)(table, staged.as_mut())?,
        }
        (list.replace)(target, staged.as_mut())
    },
    store: |table, value, doc, slot| {
        let list = lists(table)?;
        let items = (list.items)(value).ok_or_else(mismatch)?;
        let obj = slot.object(doc, ObjType::List)?;
        let existing = automerge::ReadDoc::length(doc, &obj);
        for index in 0..items.count() {
            let Some(item) = items.item_at(index) else {
                continue;
            };
            let slot = if index < existing {
                Slot::Index(&obj, index)
            } else {
                Slot::Insert(&obj, index)
            };
            (list.item.ops.store)(list.item, item, doc, slot).located(&index.to_string())?;
        }
        for index in (items.count()..existing).rev() {
            automerge::transaction::Transactable::delete(doc, &obj, index)?;
        }
        Ok(())
    },
    stores_as: |table, value, before| {
        let Some(list) = table.list() else {
            return false;
        };
        let (Some(now), Some(was)) = ((list.items)(value), (list.items)(before)) else {
            return false;
        };
        now.count() == was.count()
            && (0..now.count()).all(|at| match (now.item_at(at), was.item_at(at)) {
                (Some(now), Some(was)) => (list.item.ops.stores_as)(list.item, now, was),
                _ => false,
            })
    },
    store_changed: |table, value, _, doc, slot, _| (VEC_OPS.store)(table, value, doc, slot),
    refresh: |table, target, doc, value, id, _| {
        (VEC_OPS.reload)(table, target, doc, Some((value, id)))
    },
    read_whole: |_, _, _| Ok(()),
    edit: |table, doc, slot, path, op, _| {
        let list = lists(table)?;
        edit_whole(
            table,
            ((list.staged.0)(), list.staged.1),
            false,
            doc,
            slot,
            path,
            op,
        )
    },
    diff: |table, before, after, path, out| {
        let value = (VEC_OPS.to_json)(table, after);
        if (VEC_OPS.to_json)(table, before) != value {
            out.push(Edit::new(path.to_vec(), EditOp::Set { value }));
        }
    },
    collect_text: |table, value, out| {
        if let Some(list) = table.list()
            && let Some(items) = (list.items)(value)
        {
            for at in 0..items.count() {
                if let Some(item) = items.item_at(at) {
                    (list.item.ops.collect_text)(list.item, item, out);
                }
            }
        }
    },
    check: |table, value| {
        let Some(list) = table.list() else {
            return Ok(());
        };
        if !list.item.checks {
            return Ok(());
        }
        if let Some(items) = (list.items)(value) {
            for at in 0..items.count() {
                if let Some(item) = items.item_at(at) {
                    (list.item.ops.check)(list.item, item)?;
                }
            }
        }
        Ok(())
    },
    get_at: |_, value, path| path.is_empty().then_some(value),
    label: no_label,
    nested_collections: no_nested,
    op_coalesces: no_coalesces,
    op_continuable: no_continuable,
    op_describe: no_describe,
    op_lands: no_lands,
    op_schemas: no_schemas,
};

/// A map's entries, whatever their value type.
pub trait MapEntries {
    fn count(&self) -> usize;
    /// Each entry in key order.
    fn each(&self, visit: &mut dyn FnMut(&str, &dyn Any));
    fn value(&self, key: &str) -> Option<&dyn Any>;
    fn value_mut(&mut self, key: &str) -> Option<&mut dyn Any>;
    fn remove_key(&mut self, key: &str);
    /// Insert (or replace) `key`, its value put by `fill` into an `Option`
    /// of the value type.
    ///
    /// # Errors
    /// What `fill` refused.
    fn insert_filled(
        &mut self,
        key: String,
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError>;
    /// Insert (or replace) `key` with the value moved out of `staged`, a
    /// filled `Option` of the value type.
    ///
    /// # Errors
    /// When `staged` is of another type or empty.
    fn insert_moved(&mut self, key: String, staged: &mut dyn Any) -> Result<(), ModelError>;
    /// The keys whose entries may differ from those of `before`, a map of
    /// this same type, in key order, once each: under any other key both
    /// hold the same entry. `None`, the default, when the map cannot tell,
    /// and every entry is compared.
    fn differing_keys<'a>(&'a self, before: &'a dyn Any) -> Option<Vec<&'a str>> {
        let _ = before;
        None
    }
}

impl<T: 'static> MapEntries for std::collections::BTreeMap<String, T> {
    fn count(&self) -> usize {
        self.len()
    }

    fn each(&self, visit: &mut dyn FnMut(&str, &dyn Any)) {
        for (key, value) in self {
            visit(key, value);
        }
    }

    fn value(&self, key: &str) -> Option<&dyn Any> {
        self.get(key).map(|value| value as &dyn Any)
    }

    fn value_mut(&mut self, key: &str) -> Option<&mut dyn Any> {
        self.get_mut(key).map(|value| value as &mut dyn Any)
    }

    fn remove_key(&mut self, key: &str) {
        self.remove(key);
    }

    fn insert_filled(
        &mut self,
        key: String,
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let mut slot: Option<T> = None;
        fill(&mut slot)?;
        self.insert(key, filled(slot)?);
        Ok(())
    }

    fn insert_moved(&mut self, key: String, staged: &mut dyn Any) -> Result<(), ModelError> {
        let value = staged
            .downcast_mut::<Option<T>>()
            .and_then(Option::take)
            .ok_or_else(mismatch)?;
        self.insert(key, value);
        Ok(())
    }
}

/// What the table of a map of string keys reaches into one with.
#[allow(
    clippy::type_complexity,
    reason = "each field reaches into the map one way"
)]
pub struct MapDesc {
    item: &'static ModelTable,
    item_nullable: bool,
    entries: for<'a> fn(&'a dyn Any) -> Option<&'a dyn MapEntries>,
    entries_mut: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn MapEntries>,
    /// Put an empty map into `out` (an `Option` of it), and hand it back.
    fresh: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn MapEntries>,
    staged: (fn() -> Box<dyn Any>, Unstage),
    replace: fn(&mut dyn Any, &mut dyn Any) -> Result<(), ModelError>,
    item_staged: (fn() -> Box<dyn Any>, Unstage),
}

/// The [`MapDesc`] of the map `M` of string keys to `T`s: a
/// `BTreeMap<String, T>` ([`MapOf`]) or a [`Map`](crate::Map), stored
/// alike.
pub struct MapIn<T, M>(PhantomData<fn() -> (T, M)>);

/// The [`MapDesc`] of `BTreeMap<String, T>`.
pub type MapOf<T> = MapIn<T, std::collections::BTreeMap<String, T>>;

impl<T: Model, M: MapEntries + Default + 'static> MapIn<T, M> {
    pub const DESC: MapDesc = MapDesc {
        item: T::TABLE,
        item_nullable: T::NULLABLE,
        entries: |value| Some(value.downcast_ref::<M>()? as &dyn MapEntries),
        entries_mut: |value| Some(value.downcast_mut::<M>()? as &mut dyn MapEntries),
        fresh: |out| {
            let out = out.downcast_mut::<Option<M>>()?;
            Some(out.insert(M::default()) as &mut dyn MapEntries)
        },
        staged: (empty_of::<M>, unwrap_of::<M>),
        replace: replace_of::<M>,
        item_staged: (empty_of::<T>, unwrap_of::<T>),
    };

    pub const TABLE: ModelTable = ModelTable {
        ops: &MAP_OPS,
        desc: Desc::Map(&Self::DESC),
        checks: T::CHECKS,
        has_ops: T::OPS,
        scalar: false,
    };
}

impl ModelTable {
    const fn map(&self) -> Option<&'static MapDesc> {
        match self.desc {
            Desc::Map(map) => Some(map),
            _ => None,
        }
    }
}

fn maps(table: &ModelTable) -> Result<&'static MapDesc, ModelError> {
    table.map().ok_or_else(mismatch)
}

/// The value at the key `path` starts at, and the rest of the path, when
/// the map's values have a vocabulary of their own.
fn keyed<'p>(
    table: &ModelTable,
    path: &'p [Segment],
) -> Option<(&'static MapDesc, &'p str, &'p [Segment])> {
    match path.split_first() {
        Some((Segment::Key(key), rest)) if table.has_ops => Some((table.map()?, key, rest)),
        _ => None,
    }
}

/// The functions of every map of string keys: a map, one key per entry.
pub static MAP_OPS: ModelOps = ModelOps {
    without_absent: crate::shape::kept_whole,
    to_json: |table, value| {
        let mut out = serde_json::Map::new();
        if let Some(map) = table.map()
            && let Some(entries) = (map.entries)(value)
        {
            entries.each(&mut |key, value| {
                out.insert(key.to_owned(), (map.item.ops.to_json)(map.item, value));
            });
        }
        serde_json::Value::Object(out)
    },
    from_json: |table, value, out| {
        let map = maps(table)?;
        match value {
            serde_json::Value::Object(entries) => {
                let fresh = (map.fresh)(out).ok_or_else(mismatch)?;
                for (key, value) in entries {
                    fresh.insert_filled(key.clone(), &mut |into| {
                        (map.item.ops.from_json)(map.item, value, into).located(key)
                    })?;
                }
                Ok(())
            }
            serde_json::Value::Null => {
                (map.fresh)(out).ok_or_else(mismatch)?;
                Ok(())
            }
            _ => Err(ModelError::shape("a map")),
        }
    },
    load: |table, doc, value, id, out| {
        let map = maps(table)?;
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("a map"));
        }
        let fresh = (map.fresh)(out).ok_or_else(mismatch)?;
        // One walk of the map rather than a lookup per key.
        for (key, slot) in crate::model::map_slots(doc, id) {
            fresh.insert_filled(key.clone(), &mut |into| {
                load_slot(map.item, doc, slot.as_ref().map(|(v, i)| (v, i)), into).located(&key)
            })?;
        }
        Ok(())
    },
    missing: |table, out| {
        (maps(table)?.fresh)(out).ok_or_else(mismatch)?;
        Ok(())
    },
    reload: |table, target, doc, slot| {
        let map = maps(table)?;
        let mut staged = (map.staged.0)();
        match slot {
            Some((value, id)) => (MAP_OPS.load)(table, doc, value, id, staged.as_mut())?,
            None => (MAP_OPS.missing)(table, staged.as_mut())?,
        }
        (map.replace)(target, staged.as_mut())
    },
    store: |table, value, doc, slot| {
        let map = maps(table)?;
        let entries = (map.entries)(value).ok_or_else(mismatch)?;
        let obj = slot.object(doc, ObjType::Map)?;
        let stale: Vec<String> = automerge::ReadDoc::keys(doc, &obj)
            .filter(|key| entries.value(key).is_none())
            .collect();
        for key in stale {
            automerge::transaction::Transactable::delete(doc, &obj, key.as_str())?;
        }
        let mut written = Ok(());
        entries.each(&mut |key, value| {
            if written.is_ok() {
                written =
                    (map.item.ops.store)(map.item, value, doc, Slot::Key(&obj, key)).located(key);
            }
        });
        written
    },
    stores_as: |table, value, before| {
        let Some(map) = table.map() else {
            return false;
        };
        let (Some(now), Some(was)) = ((map.entries)(value), (map.entries)(before)) else {
            return false;
        };
        if now.count() != was.count() {
            return false;
        }
        let stores_as = |key: &str, value: &dyn Any| {
            was.value(key)
                .is_some_and(|before| (map.item.ops.stores_as)(map.item, value, before))
        };
        // Under any other key both hold the same entry.
        if let Some(keys) = now.differing_keys(before) {
            return keys
                .into_iter()
                .all(|key| now.value(key).is_some_and(|value| stores_as(key, value)));
        }
        let mut same = true;
        now.each(&mut |key, value| same = same && stores_as(key, value));
        same
    },
    store_changed: |table, value, before, doc, slot, dirty| {
        let map = maps(table)?;
        let now = (map.entries)(value).ok_or_else(mismatch)?;
        let was = (map.entries)(before).ok_or_else(mismatch)?;
        let Some(differing) = now.differing_keys(before) else {
            return (MAP_OPS.store)(table, value, doc, slot);
        };
        // A slot holding no map yet is written whole, as is a map someone
        // else changed otherwise than key by key.
        let Some((Value::Object(ObjType::Map), obj)) = slot.current(doc)? else {
            return (MAP_OPS.store)(table, value, doc, slot);
        };
        let touched = dirty.under(&obj);
        if touched.is_all() {
            return (MAP_OPS.store)(table, value, doc, slot);
        }
        // Under any other key the entry is the one `before` holds, and the
        // map holds it as a store left it: storing it writes nothing.
        let mut keys = differing;
        keys.extend(touched.keys());
        keys.compact_sort();
        keys.dedup();
        // What `store` writes: the stale keys deleted, then each entry
        // written, both in key order.
        for key in &keys {
            if now.value(key).is_none() && automerge::ReadDoc::get(doc, &obj, *key)?.is_some() {
                automerge::transaction::Transactable::delete(doc, &obj, *key)?;
            }
        }
        for key in &keys {
            let Some(next) = now.value(key) else {
                continue;
            };
            match was.value(key) {
                Some(held) => write_key_changed(map.item, (next, held), doc, &obj, key, dirty)?,
                None => {
                    (map.item.ops.store)(map.item, next, doc, Slot::Key(&obj, key)).located(key)?;
                }
            }
        }
        Ok(())
    },
    refresh: |table, target, doc, value, id, delta| {
        let map = maps(table)?;
        let touched = delta.under(id);
        if touched.is_all() {
            return (MAP_OPS.reload)(table, target, doc, Some((value, id)));
        }
        let entries = (map.entries_mut)(target).ok_or_else(mismatch)?;
        for key in touched.keys() {
            match crate::model::get_slot(doc, id, key)? {
                None => entries.remove_key(key),
                Some((value, slot)) => match entries.value_mut(key) {
                    Some(existing) => refresh_slot(map.item, existing, doc, id, key, delta)?,
                    None => entries.insert_filled(key.to_owned(), &mut |into| {
                        (map.item.ops.load)(map.item, doc, &value, &slot, into).located(key)
                    })?,
                },
            }
        }
        Ok(())
    },
    read_whole: |_, _, _| Ok(()),
    edit: |table, doc, slot, path, op, cx| {
        let map = maps(table)?;
        let item = map.item;
        match (path.split_first(), op) {
            (None, EditOp::RemoveKey { key }) => {
                let obj = slot.object(doc, ObjType::Map)?;
                Slot::Key(&obj, key).remove(doc)
            }
            (Some((Segment::Key(key), rest)), op) => {
                let obj = slot.object(doc, ObjType::Map)?;
                if rest.is_empty() && matches!(op, EditOp::Set { .. }) {
                    let staged = ((map.item_staged.0)(), map.item_staged.1);
                    return edit_whole(
                        item,
                        staged,
                        map.item_nullable,
                        doc,
                        Slot::Key(&obj, key),
                        rest,
                        op,
                    )
                    .located(key);
                }
                // An edit inside an entry that is not there yet starts it
                // from its value's empty value (a rule for an app that had
                // none).
                if automerge::ReadDoc::get(doc, &obj, key.as_str())?.is_none() {
                    let mut empty = (map.item_staged.0)();
                    (item.ops.missing)(item, empty.as_mut())
                        .map_err(|_| ModelError::invalid(format!("no entry `{key}`")))?;
                    let empty = (map.item_staged.1)(empty.as_ref()).ok_or_else(mismatch)?;
                    (item.ops.store)(item, empty, doc, Slot::Key(&obj, key)).located(key)?;
                }
                (item.ops.edit)(item, doc, Slot::Key(&obj, key), rest, op, cx).located(key)
            }
            _ => {
                let staged = ((map.staged.0)(), map.staged.1);
                edit_whole(table, staged, false, doc, slot, path, op)
            }
        }
    },
    diff: |table, before, after, path, out| {
        let Some(map) = table.map() else {
            return;
        };
        let (Some(was), Some(now)) = ((map.entries)(before), (map.entries)(after)) else {
            return;
        };
        let at = |key: &str| {
            let mut at = path.to_vec();
            at.push(Segment::Key(key.to_owned()));
            at
        };
        // An entry `before` holds: changed, or removed.
        let kept = |key: &str, before: &dyn Any, out: &mut Vec<Edit<()>>| match now.value(key) {
            Some(next) => (map.item.ops.diff)(map.item, before, next, &at(key), out),
            None => out.push(Edit::new(
                path.to_vec(),
                EditOp::RemoveKey {
                    key: key.to_owned(),
                },
            )),
        };
        // An entry `after` holds: added, when `before` had none.
        let added = |key: &str, next: &dyn Any, out: &mut Vec<Edit<()>>| {
            if was.value(key).is_none() {
                out.push(Edit::new(
                    at(key),
                    EditOp::Set {
                        value: (map.item.ops.to_json)(map.item, next),
                    },
                ));
            }
        };
        // Under any other key both hold the same entry: no edit.
        if let Some(keys) = now.differing_keys(before) {
            for key in &keys {
                if let Some(before) = was.value(key) {
                    kept(key, before, out);
                }
            }
            for key in &keys {
                if let Some(next) = now.value(key) {
                    added(key, next, out);
                }
            }
        } else {
            was.each(&mut |key, before| kept(key, before, out));
            now.each(&mut |key, next| added(key, next, out));
        }
    },
    collect_text: |table, value, out| {
        if let Some(map) = table.map()
            && let Some(entries) = (map.entries)(value)
        {
            entries.each(&mut |_, value| (map.item.ops.collect_text)(map.item, value, out));
        }
    },
    check: |table, value| {
        let Some(map) = table.map() else {
            return Ok(());
        };
        if !map.item.checks {
            return Ok(());
        }
        let Some(entries) = (map.entries)(value) else {
            return Ok(());
        };
        let mut checked = Ok(());
        entries.each(&mut |_, value| {
            if checked.is_ok() {
                checked = (map.item.ops.check)(map.item, value);
            }
        });
        checked
    },
    get_at: |table, value, path| match path.split_first() {
        None => Some(value),
        Some((Segment::Key(key), rest)) => {
            let map = table.map()?;
            (map.item.ops.get_at)(map.item, (map.entries)(value)?.value(key)?, rest)
        }
        Some(_) => None,
    },
    label: no_label,
    nested_collections: no_nested,
    op_coalesces: |table, path, previous, next| {
        let (map, _, rest) = keyed(table, path)?;
        (map.item.ops.op_coalesces)(map.item, rest, previous, next)
    },
    op_continuable: |table, path, op| {
        let (map, _, rest) = keyed(table, path)?;
        (map.item.ops.op_continuable)(map.item, rest, op)
    },
    op_describe: |table, path, op| {
        let (map, _, rest) = keyed(table, path)?;
        (map.item.ops.op_describe)(map.item, rest, op)
    },
    op_lands: |table, value, path, op| {
        let (map, key, rest) = keyed(table, path)?;
        (map.item.ops.op_lands)(map.item, (map.entries)(value)?.value(key)?, rest, op)
    },
    op_schemas: |table, prefix, out| {
        if table.has_ops
            && let Some(map) = table.map()
        {
            (map.item.ops.op_schemas)(map.item, &format!("{prefix}*."), out);
        }
    },
};

/// Whether the record `value` (described by `record`) holds an item at the
/// end of `path`: collection fields by their Rust names, the outer ones
/// looked into item by item over their first published page, as a
/// publication lists them.
#[must_use]
pub fn holds_nested_items(record: &RecordDesc, value: &dyn Any, path: &[&str]) -> bool {
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
