//! Resources of apps this runtime does not run: what they print, and their readings.
//!
//! An app the platform publishes through the session (the sound, the displays, the desktop) is
//! not loaded here as code: the host registers its description ([`crate::Runtime::load_foreign`])
//! and hands the runtime the newest reading the session's watch saw
//! ([`crate::Runtime::set_reading`]). A view that takes such a resource as a parameter reads its
//! fields from that reading, typed by the description, and runs again when a new reading comes
//! in: the reading is one slot of view state per resource, so the boundaries that read it are
//! exactly the ones marked ([`crate::view_state`]).
//!
//! A resource of another app is written `<app>#<Type>` as its type ([`is_foreign`]), and its id
//! is the session's resource it stands for (`sound`, `desktop:overlays`).

use std::collections::BTreeMap;

use serde_json::Value as Json;

use crate::ids::ModuleHash;
use crate::meta::{Ty, TypeKey};
use crate::value::{EnumValue, RefValue, ResourceRef, Value};
use crate::view_state::StateKey;

/// How a resource of an app this runtime does not run printed when a program met it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Shown {
    /// How a program writes it: `@notes.Note:root-1/notes:n2`.
    pub literal: String,
    pub app: String,
    /// Its type's name.
    pub ty: String,
    /// Its id ([`ResourceRef::id_string`]).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The newest reading of a foreign resource, and the generation it was set with.
#[derive(Debug)]
struct Reading {
    json: Json,
    generation: u64,
}

/// Results a value carries that `show` prints for the resources of apps the runtime does not
/// hold: how each printed, and its reading.
#[derive(Debug, Default)]
pub struct Foreign {
    by_key: BTreeMap<String, Shown>,
    readings: BTreeMap<String, Reading>,
    /// Moves with every reading that changed.
    clock: u64,
}

impl Foreign {
    /// The key of a foreign resource: its type (`app#Type`) and id.
    pub(crate) fn key(target: &ResourceRef) -> String {
        format!("{}@{}", target.ty, target.id_string())
    }

    pub(crate) fn get(&self, target: &ResourceRef) -> Option<&Shown> {
        self.by_key.get(&Self::key(target))
    }

    /// Remember how `target` printed.
    pub(crate) fn show(&mut self, target: &ResourceRef, shown: Shown) {
        self.by_key.insert(Self::key(target), shown);
    }

    /// The generation of `target`'s reading: 0 until one was set, larger after each change.
    pub fn generation(&self, target: &ResourceRef) -> u64 {
        self.readings
            .get(&Self::key(target))
            .map_or(0, |held| held.generation)
    }

    /// Take `json` as `target`'s reading. Whether it is not the one held.
    pub(crate) fn set_reading(&mut self, target: &ResourceRef, json: Json) -> bool {
        let key = Self::key(target);
        if self
            .readings
            .get(&key)
            .is_some_and(|held| held.json == json)
        {
            return false;
        }
        self.clock = self.clock.saturating_add(1);
        let generation = self.clock;
        self.readings.insert(key, Reading { json, generation });
        true
    }

    /// The field `field` of `target` (its name, then its id) as its declared type, from the
    /// reading now.
    ///
    /// # Errors
    ///
    /// A message when no reading of it arrived yet, or the reading holds no value of the
    /// field's type: what a service publishes and what its surface declares differ.
    pub(crate) fn field(
        &self,
        target: &ResourceRef,
        field: &crate::registry::FieldInfo,
        author: Option<&ModuleHash>,
    ) -> Result<Value, String> {
        let reading = self.readings.get(&Self::key(target)).ok_or_else(|| {
            format!(
                "no reading of `{}` arrived yet: its app publishes it through the session",
                target.id_string()
            )
        })?;
        let held = reading
            .json
            .get(&field.name)
            .or_else(|| reading.json.get(&field.id));
        let Some(json) = held else {
            return crate::host_res::declared_default(&field.ty, field.default.as_ref())
                .map_err(|why| format!("`{}` has no `{}`: {why}", target.id_string(), field.name));
        };
        decode(json, &field.ty, author)
            .map_err(|why| format!("`{}` of `{}`: {why}", field.name, target.id_string()))
    }
}

/// Whether `target` is a resource of an app this runtime does not run.
#[must_use]
pub fn is_foreign(target: &ResourceRef) -> bool {
    target.ty.contains('#')
}

/// The type of a foreign resource as its description names it: the app and the type's name.
#[must_use]
pub fn foreign_type(ty: &str) -> Option<TypeKey> {
    let (app, name) = ty.split_once('#')?;
    Some(TypeKey {
        app: app.to_owned(),
        name: name.to_owned(),
    })
}

/// The view state slot a foreign resource's reading moves, so that a view which read it runs
/// again at the next poll. Its type and id name it, as [`Foreign::key`] does.
#[must_use]
pub fn reading_key(target: &ResourceRef) -> StateKey {
    StateKey {
        window: crate::platform::LENS_WINDOW.to_owned(),
        principal: None,
        state: format!("$reading:{}", Foreign::key(target)),
    }
}

/// `json` as the value `ty` declares, as a reading of another app publishes it.
///
/// A reading is plain JSON — what its service serialised (a number for an `Int`, a string for
/// a `Text`, an object for a record's fields) — and a value the runtime itself wrote is the
/// [`crate::value::to_json`] encoding of it, which is read too. A record field reads as a
/// reference to it, given either as an id string or as that encoding.
fn decode(json: &Json, ty: &Ty, author: Option<&ModuleHash>) -> Result<Value, String> {
    match ty {
        Ty::Unit
        | Ty::Int
        | Ty::Float
        | Ty::Bool
        | Ty::Text
        | Ty::Date
        | Ty::Time
        | Ty::Identifier
        | Ty::Principal
        | Ty::Length(_) => plain(json, ty),
        Ty::Option(inner) => match json {
            Json::Null => Ok(Value::none()),
            value => Ok(Value::some(decode(value, inner, author)?)),
        },
        Ty::List(inner) | Ty::Collection(inner) | Ty::Set(inner) => {
            let Some(items) = json.as_array() else {
                return Err(format!("{json} is not a list"));
            };
            Ok(Value::List(
                items
                    .iter()
                    .map(|item| decode(item, inner, author))
                    .collect::<Result<_, _>>()?,
            ))
        }
        Ty::Tuple(items) => tuple(json, items, author),
        Ty::Record(key) => reference(json, key, author),
        Ty::Union(keys) => one_of(json, keys, author),
        Ty::Enum(key) => match json {
            Json::String(variant) => Ok(Value::Enum(EnumValue {
                ty: format!("{}#{}", key.app, key.name),
                variant: variant.clone(),
                fields: BTreeMap::new(),
            })),
            encoded => crate::value::from_json(encoded),
        },
        Ty::Result(..) | Ty::RichText | Ty::Map(_) => crate::value::from_json(json),
        Ty::Action | Ty::View | Ty::Choice(_) | Ty::Error => Err(format!(
            "a value of type `{}` is not read from another app's reading",
            ty.meta_text("")
        )),
    }
}

/// A value of the scalar type `ty` as a reading writes it: a number, a flag, or the text it
/// is held as.
fn plain(json: &Json, ty: &Ty) -> Result<Value, String> {
    let value = match ty {
        Ty::Unit => {
            return match json {
                Json::Null => Ok(Value::Unit),
                other => Err(format!("{other} is not none")),
            };
        }
        Ty::Int => json.as_i64().map(Value::Int),
        Ty::Float | Ty::Length(_) => json.as_f64().map(Value::Float),
        Ty::Bool => json.as_bool().map(Value::Bool),
        Ty::Text | Ty::Date | Ty::Time | Ty::Identifier => json.as_str().map(Value::text),
        _ => json
            .as_str()
            .map(|who| Value::Principal(crate::ids::Principal::new(who))),
    };
    value.ok_or_else(|| format!("{json} is not a {}", ty.meta_text("")))
}

/// The values of the tuple's types, in order.
fn tuple(json: &Json, items: &[Ty], author: Option<&ModuleHash>) -> Result<Value, String> {
    let Some(given) = json.as_array() else {
        return Err(format!("{json} is not a tuple"));
    };
    if given.len() != items.len() {
        return Err(format!("{json} is not a tuple of {} values", items.len()));
    }
    Ok(Value::Tuple(
        items
            .iter()
            .zip(given)
            .map(|(ty, item)| decode(item, ty, author))
            .collect::<Result<_, _>>()?,
    ))
}

/// A reference to a resource of one of the union's types: the type the reading names, since an
/// id on its own does not say which of them it is.
fn one_of(json: &Json, keys: &[TypeKey], author: Option<&ModuleHash>) -> Result<Value, String> {
    let Some((app, name)) = encoded_ref_ty(json) else {
        return Err(format!(
            "{json} does not say which of a union's types it is"
        ));
    };
    let key = keys
        .iter()
        .find(|key| key.app == app && key.name == name)
        .ok_or_else(|| format!("`{app}#{name}` is not one of this field's types"))?;
    reference(json, key, author)
}

/// The app and type name an encoded reference names.
fn encoded_ref_ty(json: &Json) -> Option<(String, String)> {
    let ty = json.get("ty")?.as_str()?;
    let (app, name) = ty.split_once('#')?;
    Some((app.to_owned(), name.to_owned()))
}

/// `json` as a reference to a resource of `key`'s type: an id string, or an encoded reference.
fn reference(json: &Json, key: &TypeKey, author: Option<&ModuleHash>) -> Result<Value, String> {
    let ty = format!("{}#{}", key.app, key.name);
    let id = match json {
        Json::String(id) => id.clone(),
        Json::Object(fields) => match fields.get("ref").and_then(Json::as_str) {
            Some(id) => id.to_owned(),
            None => return Err(format!("{json} is not a reference")),
        },
        other => return Err(format!("{other} is not a reference")),
    };
    let target =
        ResourceRef::parse_id(&id, ty).ok_or_else(|| format!("`{id}` is not a resource id"))?;
    Ok(Value::Ref(RefValue {
        target: std::rc::Rc::new(target),
        author: author.cloned(),
    }))
}
