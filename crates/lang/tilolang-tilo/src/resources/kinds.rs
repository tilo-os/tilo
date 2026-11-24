//! What the device's store calls a tilolang resource and its fields, and how a field's value is
//! held there.
//!
//! A resource of the type `T` of the app `a` is of the kind `tl.a.T`, as the app's surface
//! publishes it, and each field is held under its name, so a peer judges a change by the fields
//! the surface declares. A field the index can filter and sort by (a scalar, an enum without
//! payloads, a reference, a person, a set of them) is an indexed field, held as the surface
//! declares its values: an enum by its case's name, a reference by its id, nothing as nothing.
//! Everything else (rich text, counters, maps, composite values) is the resource's content.

use std::collections::{BTreeMap, HashMap};

use collab_types::store::Value;
use serde_json::Value as Json;
use tilolang_runtime::ids::{FieldId, TypeId};
use tilolang_runtime::meta::{FieldKind, FieldMeta, Meta, Ty, TypeKey, TypeMeta, parse_ty};
use tilolang_runtime::store::Scalar;
use tilolang_runtime::value::{self, EnumValue, ResourceRef};

/// One field of a type, as the store holds it.
#[derive(Clone, Debug)]
pub struct FieldEntry {
    pub name: String,
    /// The names an earlier version of the field was held under, the ids lock's renames: what
    /// it wrote is read where the field stands now.
    pub former: Vec<String>,
    pub ty: Ty,
    pub kind: FieldKind,
    /// The types a collection's items may be of: one, or each case of a union. Empty for a
    /// field that is no collection.
    pub items: Vec<TypeId>,
    /// The record type the field holds when its resource is made without it: an item of the
    /// holder placed under the field, with no order key ([`tilolang_runtime::held`]). `None` for
    /// any other field.
    pub held: Option<TypeId>,
    /// What the index holds of it on a resource made without it: the default the type declares,
    /// else its type's own (`false`, `0`, no text, none). `None` for a field with no default or
    /// none the index holds.
    pub initial: Option<Value>,
}

impl FieldEntry {
    /// The names it is held under: its own, then the ones an earlier version named it.
    pub fn names(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.name).chain(&self.former)
    }

    /// Whether the field is held in the index: a register or text whose values are scalars,
    /// or a set of them. Everything else is the resource's content.
    #[must_use]
    pub fn indexed(&self, kinds: &Kinds) -> bool {
        match self.kind {
            FieldKind::Register | FieldKind::Text => kinds.scalar(&self.ty),
            FieldKind::Set => true,
            FieldKind::Counter | FieldKind::RichText | FieldKind::Collection | FieldKind::Map => {
                false
            }
        }
    }
}

/// One type, as the store holds its resources.
#[derive(Clone, Debug)]
pub struct TypeEntry {
    pub id: TypeId,
    pub app: String,
    pub name: String,
    /// The kind its resources are of: `tl.<app>.<name>`.
    pub kind: String,
    /// The kinds an earlier version stored them as, the ids lock's renames: what it wrote is
    /// read as this type, which carries the same id.
    pub former: Vec<String>,
    pub fields: BTreeMap<FieldId, FieldEntry>,
    /// The field its resources are named by: the one it says, else its first text.
    pub title: Option<FieldId>,
    /// The texts, plain or rich, a resource is named by in turn while the ones before are
    /// empty: the field the type says names it, else each text as the type declares them.
    pub texts: Vec<FieldId>,
    /// Who reads its resources, when not every member (`readers: author`).
    pub readers: Option<tilolang::meta::ReaderRule>,
}

impl TypeEntry {
    /// Every kind its resources are of: the one it publishes now, then the ones an earlier
    /// version stored them as.
    pub fn kinds(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.kind).chain(&self.former)
    }
}

/// One enum's cases, by id and by name.
#[derive(Clone, Debug, Default)]
struct EnumEntry {
    id: String,
    /// `(variant id, name)`, for the cases that carry no payload.
    plain: Vec<(String, String)>,
}

/// Every loaded package's types and enums, as the store names them.
#[derive(Clone, Debug, Default)]
pub struct Kinds {
    types: HashMap<TypeId, TypeEntry>,
    by_kind: HashMap<String, TypeId>,
    by_key: HashMap<(String, String), TypeId>,
    enums: HashMap<(String, String), EnumEntry>,
    /// The action ids of every loaded package's scenarios.
    scenarios: std::collections::HashSet<String>,
}

/// The kind a type of an app is stored as.
#[must_use]
pub fn kind_of(app: &str, name: &str) -> String {
    format!("tl.{app}.{name}")
}

/// The kind the store keeps a resource of the tilolang type `ty` as: the app and name a loaded
/// package declares it under, or that `<app>#<Name>` spells. Otherwise `ty` itself, which names
/// no app, so a dispatch that must know whose resource it is refuses it.
#[must_use]
pub fn kind_of_type(ty: &str) -> String {
    if let Some(schema) = crate::schema::lookup(ty) {
        return kind_of(&schema.app, &schema.name);
    }
    ty.split_once('#')
        .map_or_else(|| ty.to_owned(), |(app, name)| kind_of(app, name))
}

/// The tilolang type of a resource the store keeps as `kind`, where a loaded package declares
/// it ([`kind_of_type`] turned around).
#[must_use]
pub fn type_of_kind(kind: &str) -> Option<String> {
    let (app, name) = kind.strip_prefix("tl.")?.rsplit_once('.')?;
    crate::schema::lookup_in(app, name).map(|schema| schema.id.clone())
}

impl Kinds {
    /// Takes in the types and enums of `meta`, replacing what an earlier version declared.
    /// `former` is the retired declarations of the package's ids lock, `(name, id)` each: a
    /// type that keeps its id across a rename is read from what the earlier name wrote, and a
    /// field from the name it held its value under then.
    pub fn declare(&mut self, meta: &Meta, former: &[(String, String)]) {
        for declared in &meta.enums {
            let plain = declared
                .variants
                .iter()
                .filter(|variant| variant.fields.is_empty())
                .map(|variant| (variant.id.clone(), variant.name.clone()))
                .collect();
            self.enums.insert(
                (meta.app.clone(), declared.name.clone()),
                EnumEntry {
                    id: declared.id.clone(),
                    plain,
                },
            );
        }
        // Every type's id first: a collection names the types of its items by name.
        for declared in &meta.types {
            self.by_key.insert(
                (meta.app.clone(), declared.name.clone()),
                declared.id.clone(),
            );
        }
        for declared in &meta.types {
            let entry = self.entry(&meta.app, declared);
            self.by_kind.insert(entry.kind.clone(), declared.id.clone());
            self.types.insert(declared.id.clone(), entry);
        }
        self.scenarios.extend(
            meta.scenarios
                .iter()
                .map(|scenario| scenario.action.clone()),
        );
        self.take_former(&meta.app, former);
    }

    /// Whether `action` is a loaded package's scenario, whose writes are its seed data.
    #[must_use]
    pub fn is_scenario(&self, action: &str) -> bool {
        self.scenarios.contains(action)
    }

    /// The type `declared` of `app`, as the store names it and its fields.
    fn entry(&self, app: &str, declared: &TypeMeta) -> TypeEntry {
        let mut fields = BTreeMap::new();
        for field in &declared.fields {
            let Ok(ty) = parse_ty(field.ty.trim(), app) else {
                continue;
            };
            let kind = tilolang_runtime::meta::field_kind(field, &ty);
            let items = match &ty {
                Ty::Collection(inner) => match inner.as_ref() {
                    Ty::Record(key) => self.type_id(key).into_iter().collect(),
                    Ty::Union(keys) => keys.iter().filter_map(|key| self.type_id(key)).collect(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            let held = tilolang_runtime::held::held_key(kind, &ty, field.default.is_some())
                .and_then(|key| self.type_id(key));
            let initial = match kind {
                FieldKind::Register | FieldKind::Text => self.initial(&ty, field.default.as_ref()),
                _ => None,
            };
            fields.insert(
                field.id.clone(),
                FieldEntry {
                    name: field.name.clone(),
                    former: Vec::new(),
                    ty,
                    kind,
                    items,
                    held,
                    initial,
                },
            );
        }
        // A text kept as one value (a key, an account) names nothing: only text as written does.
        let is_text = |field: &&FieldMeta| {
            matches!(field.ty.trim(), "Text" | "RichText")
                && field.merge != tilolang::types::MergeKind::Register
        };
        let title = declared
            .title
            .as_deref()
            .and_then(|name| declared.fields.iter().find(|field| field.name == name))
            // Else its first text, plain or rich: what a message is called by is what it says.
            .or_else(|| declared.fields.iter().find(is_text))
            .map(|field| field.id.clone());
        // A type that says what names it is named by that alone; one that does not, by its
        // first text that holds any.
        let texts = if declared.title.is_some() {
            title.iter().cloned().collect()
        } else {
            declared
                .fields
                .iter()
                .filter(is_text)
                .map(|field| field.id.clone())
                .collect()
        };
        TypeEntry {
            id: declared.id.clone(),
            app: app.to_owned(),
            name: declared.name.clone(),
            kind: kind_of(app, &declared.name),
            former: Vec::new(),
            fields,
            title,
            texts,
            readers: declared.readers.clone(),
        }
    }

    /// What the earlier names of kept ids wrote: a type under the kind its name made then, a
    /// field under the name it held its value under. A retired name of another app's, or of a
    /// declaration the ids lock no longer names a type or a field of, tells nothing.
    fn take_former(&mut self, app: &str, former: &[(String, String)]) {
        for (name, id) in former {
            let Some((_, field)) = name.split_once('.') else {
                let Some(entry) = self.types.get_mut(id) else {
                    continue;
                };
                if entry.app != app {
                    continue;
                }
                let kind = kind_of(app, name);
                if !entry.former.contains(&kind) {
                    entry.former.push(kind.clone());
                }
                self.by_kind.entry(kind).or_insert_with(|| id.clone());
                continue;
            };
            for entry in self.types.values_mut().filter(|entry| entry.app == app) {
                let Some(held) = entry.fields.get_mut(id) else {
                    continue;
                };
                if !held.former.iter().any(|held| held == field) {
                    held.former.push(field.to_owned());
                }
                break;
            }
        }
    }

    /// Every type declared.
    pub fn types(&self) -> impl Iterator<Item = &TypeEntry> {
        self.types.values()
    }

    /// The name of the case `variant` (its id) of a plain enum.
    #[must_use]
    pub fn variant_name(&self, variant: &str) -> Option<String> {
        self.enums.values().find_map(|entry| {
            entry
                .plain
                .iter()
                .find(|(id, _)| id == variant)
                .map(|(_, name)| name.clone())
        })
    }

    #[must_use]
    pub fn get(&self, ty: &str) -> Option<&TypeEntry> {
        self.types.get(ty)
    }

    /// The type a kind is of.
    #[must_use]
    pub fn of_kind(&self, kind: &str) -> Option<&TypeEntry> {
        self.by_kind.get(kind).and_then(|id| self.types.get(id))
    }

    fn type_id(&self, key: &TypeKey) -> Option<TypeId> {
        self.by_key
            .get(&(key.app.clone(), key.name.clone()))
            .cloned()
    }

    fn plain_enum(&self, key: &TypeKey) -> Option<&EnumEntry> {
        self.enums
            .get(&(key.app.clone(), key.name.clone()))
            .filter(|entry| !entry.plain.is_empty())
    }

    /// Whether a value of `ty` is one scalar the index compares.
    #[must_use]
    pub fn scalar(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Int
            | Ty::Float
            | Ty::Bool
            | Ty::Text
            | Ty::Date
            | Ty::Time
            | Ty::Identifier
            | Ty::Actor
            | Ty::Record(_) => true,
            Ty::Enum(key) => self.plain_enum(key).is_some(),
            Ty::Option(inner) => self.scalar(inner),
            _ => false,
        }
    }

    /// What the index holds of a field of `ty` a resource was made without: `default`, the
    /// default it declares, else the type's own, as a read of the field answers it.
    fn initial(&self, ty: &Ty, default: Option<&Json>) -> Option<Value> {
        match (ty, default) {
            (Ty::Option(_), None) => Some(Value::Null),
            (Ty::Bool, None) => Some(Value::Bool(false)),
            (Ty::Int, None) => Some(Value::Int(0)),
            (Ty::Float, None) => Some(Value::Float(0.0)),
            (Ty::Text | Ty::Date | Ty::Time | Ty::Identifier, None) => {
                Some(Value::Text(String::new()))
            }
            (_, Some(default)) => self.encode_json(ty, default),
            _ => None,
        }
    }

    /// The scalar `scalar` of a field of `ty` as the index holds it; `None` when the index
    /// cannot hold it, and it goes to the resource's content.
    #[must_use]
    pub fn encode(&self, ty: &Ty, scalar: &Scalar) -> Option<Value> {
        match (ty, scalar) {
            (_, Scalar::Bool(flag)) => Some(Value::Bool(*flag)),
            (_, Scalar::Int(number)) => Some(Value::Int(*number)),
            (_, Scalar::Float(number)) => Some(Value::Float(*number)),
            (_, Scalar::Text(text)) => Some(Value::Text(text.clone())),
            (ty, Scalar::Json(json)) => self.encode_json(ty, json),
        }
    }

    fn encode_json(&self, ty: &Ty, json: &Json) -> Option<Value> {
        match ty {
            Ty::Option(inner) => match json.as_object() {
                Some(tagged) if tagged.contains_key("none") => Some(Value::Null),
                Some(tagged) => match tagged.get("some") {
                    Some(inner_json) => self.encode_scalar_json(inner, inner_json),
                    None => None,
                },
                None if json.is_null() => Some(Value::Null),
                None => None,
            },
            other => self.encode_scalar_json(other, json),
        }
    }

    /// A value of `ty`, written as the runtime writes it, as the index holds it.
    fn encode_scalar_json(&self, ty: &Ty, json: &Json) -> Option<Value> {
        match ty {
            Ty::Bool => json.as_bool().map(Value::Bool),
            Ty::Int => json.as_i64().map(Value::Int),
            Ty::Float => json.as_f64().map(Value::Float),
            Ty::Text | Ty::Date | Ty::Time | Ty::Identifier => {
                json.as_str().map(|text| Value::Text(text.to_owned()))
            }
            Ty::Actor => match value::from_json(json).ok()? {
                value::Value::Actor(actor) => {
                    Some(Value::Text(tilolang_runtime::ids::actor_text(&actor)))
                }
                _ => None,
            },
            Ty::Record(_) => match value::from_json(json).ok()? {
                value::Value::Ref(reference) => Some(Value::Text(reference.target.id_string())),
                _ => None,
            },
            Ty::Enum(key) => {
                let entry = self.plain_enum(key)?;
                let variant = json.get("variant").and_then(Json::as_str)?;
                entry
                    .plain
                    .iter()
                    .find(|(id, _)| id == variant)
                    .map(|(_, name)| Value::Text(name.clone()))
            }
            _ => None,
        }
    }

    /// What a field of `ty` holds, read back from the index's `value`.
    #[must_use]
    pub fn decode(&self, ty: &Ty, value: &Value) -> Option<Scalar> {
        match ty {
            Ty::Option(inner) => {
                if matches!(value, Value::Null) {
                    return Some(Scalar::Json(serde_json::json!({"none": null})));
                }
                let inner_json = self.decode_json(inner, value)?;
                Some(Scalar::Json(serde_json::json!({ "some": inner_json })))
            }
            Ty::Bool => match value {
                Value::Bool(flag) => Some(Scalar::Bool(*flag)),
                _ => None,
            },
            Ty::Int => match value {
                Value::Int(number) => Some(Scalar::Int(*number)),
                _ => None,
            },
            Ty::Float => match value {
                Value::Float(number) => Some(Scalar::Float(*number)),
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a float field written as a whole number"
                )]
                Value::Int(number) => Some(Scalar::Float(*number as f64)),
                _ => None,
            },
            Ty::Text | Ty::Date | Ty::Time | Ty::Identifier => {
                value.as_text().map(|text| Scalar::Text(text.to_owned()))
            }
            other => self.decode_json(other, value).map(Scalar::Json),
        }
    }

    /// The runtime's JSON of a value of `ty` the index holds as `value`.
    fn decode_json(&self, ty: &Ty, value: &Value) -> Option<Json> {
        match (ty, value) {
            (Ty::Bool, Value::Bool(flag)) => Some(Json::Bool(*flag)),
            (Ty::Int, Value::Int(number)) => Some(Json::from(*number)),
            (Ty::Float, Value::Float(number)) => {
                serde_json::Number::from_f64(*number).map(Json::Number)
            }
            (Ty::Text | Ty::Date | Ty::Time | Ty::Identifier, Value::Text(text)) => {
                Some(Json::String(text.clone()))
            }
            (Ty::Actor, Value::Text(text)) => {
                let actor = tilolang_runtime::ids::actor_of_text(text)?;
                value::to_json(&value::Value::Actor(actor)).ok()
            }
            (Ty::Record(key), Value::Text(id)) => {
                let ty = self
                    .type_id(key)
                    .unwrap_or_else(|| format!("{}#{}", key.app, key.name));
                let target = ResourceRef::parse_id(id, ty)?;
                value::to_json(&value::Value::resource(&target)).ok()
            }
            (Ty::Enum(key), Value::Text(name)) => {
                let entry = self.plain_enum(key)?;
                let (variant, _) = entry.plain.iter().find(|(_, held)| held == name)?;
                value::to_json(&value::Value::Enum(EnumValue {
                    ty: entry.id.clone(),
                    variant: variant.clone(),
                    fields: BTreeMap::new(),
                }))
                .ok()
            }
            _ => None,
        }
    }

    /// A set's member `key` as the index holds it: a reference or a person as written, a case
    /// of an enum by its name.
    #[must_use]
    pub fn member(key: &str) -> Value {
        Value::Text(key.to_owned())
    }
}
