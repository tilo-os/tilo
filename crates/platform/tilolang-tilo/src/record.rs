//! [`DynRecord`]: a tilolang resource as a `tilo_resource` model.
//!
//! The framework stores typed records through `Model`, which a Rust type implements once. A
//! tilolang type exists only at run time, so `DynRecord` is one hand-written implementation whose
//! schema is looked up from the `$type` id stored in each resource ([`crate::schema`]). Every
//! operation then delegates to the concrete model of the field's merge kind, so a tilolang
//! counter merges exactly as a Rust [`Counter`] does.
//!
//! Layout of one resource (implementation design 7.2): a map holding `$type`, `$app` and every
//! field by field id.

use std::collections::BTreeMap;

use automerge::{ObjType, ReadDoc, ScalarValue, Value as AmValue};
use serde_json::Value as Json;
use tilo_resource::__private::ObjId;
use tilo_resource::{
    ActionCx, Collection, Counter, Doc, EditOp, Fields, Map, Model, ModelError, Segment, Set,
    Settable, Slot, edit_whole, read_key, write_key,
};

use crate::schema::{APP_KEY, FieldSchema, MergeKind, TYPE_KEY, TypeSchema, lookup};

/// The value of one field of a [`DynRecord`], in the concrete model of its merge kind.
#[derive(Clone, Debug, PartialEq)]
pub enum DynValue {
    Register(Json),
    Counter(Counter),
    Text(String),
    Set(Set<Json>),
    Map(Map<Json>),
    Collection(Collection<DynRecord>),
    Record(Box<DynRecord>),
}

/// One resource: its type and its field values.
#[derive(Clone, Debug, PartialEq)]
pub struct DynRecord {
    schema: &'static TypeSchema,
    fields: BTreeMap<String, DynValue>,
}

fn unknown_type(id: &str) -> ModelError {
    ModelError::invalid(format!(
        "the type `{id}` is not registered: its package has not been loaded here"
    ))
}

fn mismatch(field: &FieldSchema) -> ModelError {
    ModelError::invalid(format!(
        "the value of field `{}` does not have its merge kind {:?}",
        field.id, field.kind
    ))
}

impl DynRecord {
    /// A record of `schema` whose every field holds its default.
    ///
    /// # Errors
    /// A nested record field whose type is not registered.
    pub fn empty(schema: &'static TypeSchema) -> Result<Self, ModelError> {
        let mut fields = BTreeMap::new();
        for field in &schema.fields {
            fields.insert(field.id.clone(), default_value(field)?);
        }
        Ok(Self { schema, fields })
    }

    /// The schema of this record's type.
    #[must_use]
    pub const fn schema(&self) -> &'static TypeSchema {
        self.schema
    }

    /// The value of the field `id`.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&DynValue> {
        self.fields.get(id)
    }

    /// The value of the field `id`, to change.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut DynValue> {
        self.fields.get_mut(id)
    }

    /// Set a register field.
    ///
    /// # Errors
    /// The field does not exist or is not a register.
    pub fn set_register(&mut self, id: &str, value: Json) -> Result<(), ModelError> {
        match self.fields.get_mut(id) {
            Some(DynValue::Register(slot)) => {
                *slot = value;
                Ok(())
            }
            _ => Err(ModelError::invalid(format!(
                "`{id}` is not a register field"
            ))),
        }
    }

    /// The type id stored at the map `obj`.
    fn type_at(doc: &Doc, obj: &ObjId) -> Result<&'static TypeSchema, ModelError> {
        match doc.get(obj, TYPE_KEY)? {
            Some((AmValue::Scalar(scalar), _)) => match scalar.as_ref() {
                ScalarValue::Str(id) => lookup(id).ok_or_else(|| unknown_type(id)),
                _ => Err(ModelError::shape("a type id")),
            },
            _ => Err(ModelError::Missing {
                path: TYPE_KEY.to_owned(),
            }),
        }
    }
}

fn default_value(field: &FieldSchema) -> Result<DynValue, ModelError> {
    Ok(match field.kind {
        MergeKind::Register => DynValue::Register(field.default.clone()),
        MergeKind::Counter => DynValue::Counter(Counter::default()),
        MergeKind::Text => DynValue::Text(String::new()),
        MergeKind::Set => DynValue::Set(Set::new()),
        MergeKind::Map => DynValue::Map(Map::new()),
        MergeKind::Collection => DynValue::Collection(Collection::default()),
        MergeKind::Record => {
            let id = field.record.as_deref().unwrap_or_default();
            let schema = lookup(id).ok_or_else(|| unknown_type(id))?;
            DynValue::Record(Box::new(DynRecord::empty(schema)?))
        }
    })
}

fn read_field(doc: &Doc, obj: &ObjId, field: &FieldSchema) -> Result<DynValue, ModelError> {
    let id = field.id.as_str();
    Ok(match field.kind {
        MergeKind::Register => {
            let value: Json = read_key(doc, obj, id)?;
            DynValue::Register(if value.is_null() {
                field.default.clone()
            } else {
                value
            })
        }
        MergeKind::Counter => DynValue::Counter(read_key(doc, obj, id)?),
        MergeKind::Text => DynValue::Text(read_key(doc, obj, id)?),
        MergeKind::Set => DynValue::Set(read_key(doc, obj, id)?),
        MergeKind::Map => DynValue::Map(read_key(doc, obj, id)?),
        MergeKind::Collection => DynValue::Collection(read_key(doc, obj, id)?),
        MergeKind::Record => DynValue::Record(Box::new(read_key(doc, obj, id)?)),
    })
}

fn write_field(
    doc: &mut Doc,
    obj: &ObjId,
    field: &FieldSchema,
    value: &DynValue,
) -> Result<(), ModelError> {
    let id = field.id.as_str();
    match (field.kind, value) {
        (MergeKind::Register, DynValue::Register(value)) => write_key(doc, obj, id, value),
        (MergeKind::Counter, DynValue::Counter(value)) => write_key(doc, obj, id, value),
        (MergeKind::Text, DynValue::Text(value)) => write_key(doc, obj, id, value),
        (MergeKind::Set, DynValue::Set(value)) => write_key(doc, obj, id, value),
        (MergeKind::Map, DynValue::Map(value)) => write_key(doc, obj, id, value),
        (MergeKind::Collection, DynValue::Collection(value)) => write_key(doc, obj, id, value),
        (MergeKind::Record, DynValue::Record(value)) => write_key(doc, obj, id, value.as_ref()),
        _ => Err(mismatch(field)),
    }
}

fn value_json(value: &DynValue) -> Json {
    match value {
        DynValue::Register(value) => value.clone(),
        DynValue::Counter(value) => value.to_json(),
        DynValue::Text(value) => value.to_json(),
        DynValue::Set(value) => value.to_json(),
        DynValue::Map(value) => value.to_json(),
        DynValue::Collection(value) => value.to_json(),
        DynValue::Record(value) => value.to_json(),
    }
}

fn value_from_json(field: &FieldSchema, json: &Json) -> Result<DynValue, ModelError> {
    Ok(match field.kind {
        MergeKind::Register => DynValue::Register(if json.is_null() {
            field.default.clone()
        } else {
            json.clone()
        }),
        MergeKind::Counter => DynValue::Counter(Counter::from_json(json)?),
        MergeKind::Text => DynValue::Text(String::from_json(json)?),
        MergeKind::Set => DynValue::Set(Set::from_json(json)?),
        MergeKind::Map => DynValue::Map(Map::from_json(json)?),
        MergeKind::Collection => DynValue::Collection(Collection::from_json(json)?),
        MergeKind::Record => DynValue::Record(Box::new(DynRecord::from_json(json)?)),
    })
}

impl Settable for DynRecord {}

impl Model for DynRecord {
    tilo_resource::leaf_paths!();

    fn to_json(&self) -> Json {
        let mut object = serde_json::Map::new();
        object.insert(TYPE_KEY.to_owned(), Json::String(self.schema.id.clone()));
        object.insert(APP_KEY.to_owned(), Json::String(self.schema.app.clone()));
        for (id, value) in &self.fields {
            object.insert(id.clone(), value_json(value));
        }
        Json::Object(object)
    }

    fn from_json(value: &Json) -> Result<Self, ModelError> {
        let Json::Object(object) = value else {
            return Err(ModelError::shape("a record object"));
        };
        let id =
            object
                .get(TYPE_KEY)
                .and_then(Json::as_str)
                .ok_or_else(|| ModelError::Missing {
                    path: TYPE_KEY.to_owned(),
                })?;
        let schema = lookup(id).ok_or_else(|| unknown_type(id))?;
        let mut fields = BTreeMap::new();
        for field in &schema.fields {
            let value = match object.get(&field.id) {
                Some(json) => value_from_json(field, json).map_err(|error| error.at(&field.id))?,
                None => default_value(field)?,
            };
            fields.insert(field.id.clone(), value);
        }
        Ok(Self { schema, fields })
    }

    fn value_schema() -> Json {
        serde_json::json!({"type": "object"})
    }

    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        if path.is_empty() {
            return edit_whole::<Self>(doc, slot, path, op);
        }
        match slot.current(doc)? {
            Some((AmValue::Object(ObjType::Map), obj)) => {
                Self::edit_fields(doc, &obj, path, op, cx)
            }
            _ => Err(ModelError::shape("a record")),
        }
    }

    fn load(doc: &Doc, value: &AmValue<'_>, id: &ObjId) -> Result<Self, ModelError> {
        match value {
            AmValue::Object(ObjType::Map) => Self::read_fields(doc, id),
            _ => Err(ModelError::shape("a record")),
        }
    }

    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let obj = slot.object(doc, ObjType::Map)?;
        self.write_fields(doc, &obj)
    }

    fn collect_text(&self, out: &mut Vec<String>) {
        for value in self.fields.values() {
            match value {
                DynValue::Text(text) if !text.is_empty() => out.push(text.clone()),
                DynValue::Collection(items) => {
                    for (_, item) in items.iter() {
                        item.collect_text(out);
                    }
                }
                DynValue::Record(record) => record.collect_text(out),
                _ => {}
            }
        }
    }
}

impl Fields for DynRecord {
    fn read_fields(doc: &Doc, obj: &ObjId) -> Result<Self, ModelError> {
        let schema = Self::type_at(doc, obj)?;
        let mut fields = BTreeMap::new();
        for field in &schema.fields {
            let value = read_field(doc, obj, field).map_err(|error| error.at(&field.id))?;
            fields.insert(field.id.clone(), value);
        }
        Ok(Self { schema, fields })
    }

    fn write_fields(&self, doc: &mut Doc, obj: &ObjId) -> Result<(), ModelError> {
        write_key(
            doc,
            obj,
            TYPE_KEY,
            &tilo_resource::Atom(self.schema.id.clone()),
        )?;
        write_key(
            doc,
            obj,
            APP_KEY,
            &tilo_resource::Atom(self.schema.app.clone()),
        )?;
        for field in &self.schema.fields {
            let value = match self.fields.get(&field.id) {
                Some(value) => value.clone(),
                None => default_value(field)?,
            };
            write_field(doc, obj, field, &value)?;
        }
        Ok(())
    }

    fn edit_fields(
        doc: &mut Doc,
        obj: &ObjId,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        let Some((Segment::Field(id), rest)) = path.split_first() else {
            return Err(ModelError::unsupported(op));
        };
        let schema = Self::type_at(doc, obj)?;
        let field = schema
            .field(id)
            .ok_or_else(|| ModelError::invalid(format!("the type has no field `{id}`")))?;
        let slot = Slot::Key(obj, id);
        match field.kind {
            MergeKind::Register => Json::edit(doc, slot, rest, op, cx),
            MergeKind::Counter => Counter::edit(doc, slot, rest, op, cx),
            MergeKind::Text => String::edit(doc, slot, rest, op, cx),
            MergeKind::Set => Set::<Json>::edit(doc, slot, rest, op, cx),
            MergeKind::Map => Map::<Json>::edit(doc, slot, rest, op, cx),
            MergeKind::Collection => Collection::<Self>::edit(doc, slot, rest, op, cx),
            MergeKind::Record => Self::edit(doc, slot, rest, op, cx),
        }
        .map_err(|error| error.at(id))
    }
}
