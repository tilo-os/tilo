//! JSON: text read into a value of a type the code names (`json.decode<T>`), a value written as
//! text (`json.encode`), and text read into a `json.Value` whose shape code asks later
//! (`json.parse`).
//!
//! None of it is an effect: it reaches nothing outside the text and the value it is given, so an
//! action that only reads JSON text declares no use. What can go wrong is a value: text that is
//! no JSON, and text whose shape is not the type's, each answer a `Result` saying where, by the
//! JSON path of the value that did not fit (`$.items[2].title`).
//!
//! A record read here is a record value, not a resource: it has no id until it is put in a
//! collection (`feed.items.insert(item)`), which is what a record value is for. A record field
//! that holds a resource (a record, a collection, a set) cannot be made from text, so reading it
//! from text says so at its path.

use std::collections::BTreeMap;

use serde_json::Value as Json;

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::meta::{Ty, TypeKey, parse_ty};
use crate::registry::Registry;
use crate::value::{EnumValue, RecordInit, Value};

impl Core {
    /// `json.parse`, `json.decode<T>` and `json.encode`.
    pub(crate) fn json_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::JsonParse => {
                let text = self.text(a.h(0)?)?;
                self.result(parse(&text))
            }
            Imp::JsonDecode => {
                let text = self.text(a.h(0)?)?;
                let ty = self.text(a.h(1)?)?;
                let app = self.text(a.h(2)?)?;
                let registry = self.registry.borrow();
                self.result(decode(&registry, &text, &ty, &app))
            }
            Imp::JsonEncode => {
                let value = self.val(a.h(0)?)?;
                let registry = self.registry.borrow();
                match encode(&registry, &value) {
                    Ok(json) => self.ret_h(Value::Text(json.to_string())),
                    Err(why) => trap(why),
                }
            }
            _ => trap("not a json import"),
        }
    }

    /// A `Result`: `.ok(value)`, or `.err(why)` saying what did not fit, and where.
    fn result(&self, outcome: Result<Value, String>) -> HResult<Option<Wasm>> {
        let mut fields = BTreeMap::new();
        let variant = match outcome {
            Ok(Value::Unit) => tilolang::RESULT_OK,
            Ok(value) => {
                fields.insert(tilolang::RESULT_VALUE.to_owned(), value);
                tilolang::RESULT_OK
            }
            Err(why) => {
                fields.insert(tilolang::RESULT_ERROR.to_owned(), Value::Text(why));
                tilolang::RESULT_ERR
            }
        };
        self.ret_h(Value::Enum(EnumValue {
            ty: tilolang::RESULT_TYPE.to_owned(),
            variant: variant.to_owned(),
            fields,
        }))
    }
}

/// The text as JSON, or the place it stops being JSON at.
fn parse(text: &str) -> Result<Value, String> {
    from_text(text).map(|json| Value::Json(Box::new(json)))
}

fn from_text(text: &str) -> Result<Json, String> {
    serde_json::from_str::<Json>(text)
        .map_err(|why| format!("not JSON: {}", why.to_string().to_lowercase()))
}

/// The text as a value of the type `ty_text` (as metadata writes it, of the package `app`).
fn decode(registry: &Registry, text: &str, ty_text: &str, app: &str) -> Result<Value, String> {
    let ty = parse_ty(ty_text, app)?;
    let json = from_text(text)?;
    Reader { registry }.read(&json, &ty, "$")
}

/// The value as JSON text writes it: what the type declares, field by field, under the field's
/// own name.
fn encode(registry: &Registry, value: &Value) -> Result<Json, String> {
    match value {
        Value::Unit | Value::Opt(None) => Ok(Json::Null),
        Value::Int(i) => Ok(Json::from(*i)),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            .ok_or_else(|| format!("{f} is no number JSON writes")),
        Value::Bool(b) => Ok(Json::Bool(*b)),
        Value::Text(text) => Ok(Json::String(text.clone())),
        Value::Opt(Some(held)) => encode(registry, held),
        Value::List(items) => items
            .iter()
            .map(|item| encode(registry, item))
            .collect::<Result<Vec<_>, _>>()
            .map(Json::Array),
        Value::Map(entries) => {
            let mut out = serde_json::Map::new();
            for (key, held) in entries {
                let Some(key) = key.as_text() else {
                    return Err(format!(
                        "a {} is no JSON key: JSON names its keys in text",
                        key.kind_name()
                    ));
                };
                out.insert(key.to_owned(), encode(registry, held)?);
            }
            Ok(Json::Object(out))
        }
        Value::Enum(e) => encode_enum(registry, e),
        Value::Rec(rec) => {
            let Some((_, info)) = registry.type_info(&rec.ty) else {
                return Err(format!(
                    "`{}` is not declared by any loaded package",
                    rec.ty
                ));
            };
            let mut out = serde_json::Map::new();
            for field in &info.fields {
                // A field is written as what the value holds, or as what the type declares
                // for a field it does not.
                let held = match rec.fields.get(&field.id) {
                    Some(held) => held.clone(),
                    None => {
                        match crate::host_res::declared_default(&field.ty, field.default.as_ref()) {
                            Ok(value) => value,
                            Err(_) => continue,
                        }
                    }
                };
                out.insert(field.name.clone(), encode(registry, &held)?);
            }
            Ok(Json::Object(out))
        }
        Value::Json(json) => Ok((**json).clone()),
        // The values the language holds only while it runs, which no document is made of.
        Value::Ref(_) => {
            Err("a resource is not a value JSON writes: write the fields of it you want".to_owned())
        }
        other => Err(format!(
            "a {} is not a value JSON writes",
            other.kind_name()
        )),
    }
}

/// An enum value as JSON: a case carrying nothing as its name, a case carrying fields as
/// `{"<case>": {<field>: <value>}}`.
fn encode_enum(registry: &Registry, e: &EnumValue) -> Result<Json, String> {
    let names = registry.enum_meta(&e.ty);
    let case = names
        .as_ref()
        .and_then(|meta| meta.variants.iter().find(|v| v.id == e.variant))
        .map_or(e.variant.as_str(), |v| v.name.as_str());
    if e.fields.is_empty() {
        return Ok(Json::String(case.to_owned()));
    }
    let mut payload = serde_json::Map::new();
    for (field, held) in &e.fields {
        payload.insert(field.clone(), encode(registry, held)?);
    }
    let mut out = serde_json::Map::new();
    out.insert(case.to_owned(), Json::Object(payload));
    Ok(Json::Object(out))
}

/// A number as the Int it is, when it is a whole one that fits: `3` and `3.0` both read as 3,
/// `3.5` as none.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is a whole one that the i64 range holds"
)]
fn whole_int(json: &Json) -> Option<i64> {
    json.as_i64().or_else(|| {
        let f = json.as_f64()?;
        (f.fract() == 0.0
            && (-9_223_372_036_854_775_808.0..=9_223_372_036_854_775_807.0).contains(&f))
        .then_some(f as i64)
    })
}

/// The kind of a JSON value, as `json.Value`'s cases name it.
pub(crate) fn kind_of(json: &Json) -> &'static str {
    match json {
        Json::Object(_) => "object",
        Json::Array(_) => "array",
        Json::String(_) => "string",
        Json::Number(_) => "number",
        Json::Bool(_) => "bool",
        Json::Null => "null",
    }
}

/// The value a case of `json.Value` binds: `fields`, `items`, `text`, `number` or `flag`.
pub(crate) fn payload_of(json: &Json, field: &str) -> Option<Value> {
    match (json, field) {
        (Json::Object(fields), "fields") => Some(Value::Map(
            fields
                .iter()
                .map(|(key, held)| {
                    (
                        Value::text(key.clone()),
                        Value::Json(Box::new(held.clone())),
                    )
                })
                .collect(),
        )),
        (Json::Array(items), "items") => Some(Value::List(
            items
                .iter()
                .map(|item| Value::Json(Box::new(item.clone())))
                .collect(),
        )),
        (Json::String(text), "text") => Some(Value::Text(text.clone())),
        (Json::Number(number), "number") => number.as_f64().map(Value::Float),
        (Json::Bool(flag), "flag") => Some(Value::Bool(*flag)),
        _ => None,
    }
}

/// Reads JSON into the value a type declares, saying by its path where a value does not fit.
struct Reader<'a> {
    registry: &'a Registry,
}

impl Reader<'_> {
    fn read(&self, json: &Json, ty: &Ty, at: &str) -> Result<Value, String> {
        match ty {
            Ty::Unit => match json {
                Json::Null => Ok(Value::Unit),
                other => Err(not(at, other, "null")),
            },
            Ty::Int => whole_int(json)
                .map(Value::Int)
                .ok_or_else(|| not(at, json, "a whole number")),
            Ty::Float => json
                .as_f64()
                .map(Value::Float)
                .ok_or_else(|| not(at, json, "a number")),
            Ty::Bool => json
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| not(at, json, "a Bool")),
            Ty::Text | Ty::Date | Ty::Time | Ty::Identifier => json
                .as_str()
                .map(|text| Value::Text(text.to_owned()))
                .ok_or_else(|| not(at, json, &format!("a `{}`, as text", ty.display("")))),
            Ty::Option(held) => {
                if json.is_null() {
                    return Ok(Value::none());
                }
                Ok(Value::some(self.read(json, held, at)?))
            }
            Ty::List(held) => {
                let Some(items) = json.as_array() else {
                    return Err(not(at, json, "a list"));
                };
                let mut values = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    values.push(self.read(item, held, &format!("{at}[{index}]"))?);
                }
                Ok(Value::List(values))
            }
            Ty::Map(held) => {
                let Some(fields) = json.as_object() else {
                    return Err(not(at, json, "an object"));
                };
                let mut entries = Vec::with_capacity(fields.len());
                for (key, item) in fields {
                    entries.push((
                        Value::text(key.clone()),
                        self.read(item, held, &format!("{at}.{key}"))?,
                    ));
                }
                Ok(Value::Map(entries))
            }
            Ty::Enum(key) => self.enum_value(json, key, at),
            Ty::Record(key) => self.record(json, key, at),
            Ty::Collection(_) | Ty::Set(_) | Ty::Filter(_) => Err(format!(
                "{at}: a `{}` holds resources, and JSON text makes none: read a `List`, then insert what it holds",
                ty.display("")
            )),
            other => Err(format!(
                "{at}: `{}` is no type JSON text reads; `json.decode` reads Int, Float, Bool, Text, a list, an option, a map, an enum or a record",
                other.display("")
            )),
        }
    }

    /// A record value: the fields the text holds, by name. A field the text leaves out is none
    /// or its type's own default (a required one missing is an error at its path); names the
    /// type has no field for are ignored, so a service may add one without breaking its readers.
    fn record(&self, json: &Json, key: &TypeKey, at: &str) -> Result<Value, String> {
        let Some(fields) = json.as_object() else {
            return Err(not(at, json, "an object"));
        };
        let Some(id) = self.registry.type_for(key) else {
            return Err(format!(
                "`{}` of `{}` is not declared by any loaded package",
                key.name, key.app
            ));
        };
        let Some((pkg, info)) = self.registry.type_info(&id) else {
            return Err(format!(
                "`{}` is not declared by any loaded package",
                key.name
            ));
        };
        let mut held = BTreeMap::new();
        for field in &info.fields {
            let field_at = format!("{at}.{}", field.name);
            let Some(value) = fields.get(&field.name) else {
                if matches!(field.ty, Ty::Option(_)) || field.default.is_some() {
                    continue;
                }
                return Err(format!(
                    "{field_at} is missing; `{}` has {}. The object has {}",
                    info.name,
                    info.field_names(),
                    keys(json)
                ));
            };
            held.insert(
                field.id.clone(),
                self.field(value, &field.ty, &field_at, &pkg.meta.app)?,
            );
        }
        Ok(Value::Rec(RecordInit {
            ty: info.id.clone(),
            fields: held,
        }))
    }

    /// A record's field, which the store keeps as a value or as a reference to a resource: text
    /// makes the value, never the reference.
    fn field(&self, json: &Json, ty: &Ty, at: &str, app: &str) -> Result<Value, String> {
        if holds_resources(ty) {
            return Err(format!(
                "{at}: a `{}` holds resources, and JSON text makes none: read a `List` of them and insert what it holds",
                ty.display(app)
            ));
        }
        self.read(json, ty, at)
    }

    /// A case of a declared enum: its name as text when it carries nothing, or
    /// `{"<case>": {<field>: <value>}}` when it carries fields.
    fn enum_value(&self, json: &Json, key: &TypeKey, at: &str) -> Result<Value, String> {
        let Some(declared) = self.registry.enum_of(key) else {
            return Err(format!(
                "`{}` of `{}` is not declared by any loaded package",
                key.name, key.app
            ));
        };
        let naming = |case: &str| {
            declared
                .variants
                .iter()
                .find(|variant| variant.name == case || variant.id == case)
                .cloned()
                .ok_or_else(|| {
                    let cases: Vec<&str> = declared
                        .variants
                        .iter()
                        .map(|variant| variant.name.as_str())
                        .collect();
                    format!("{at} is no case of `{}`: {}", key.name, cases.join(", "))
                })
        };
        match json {
            Json::String(case) => {
                let variant = naming(case)?;
                if !variant.fields.is_empty() {
                    let fields: Vec<&str> =
                        variant.fields.iter().map(|f| f.name.as_str()).collect();
                    return Err(format!(
                        "{at} names `{}`, which carries {}: write {{\"{}\": {{{}: ...}}}}",
                        variant.name,
                        fields.join(", "),
                        variant.name,
                        fields.join(", ")
                    ));
                }
                Ok(Value::Enum(EnumValue {
                    ty: declared.id.clone(),
                    variant: variant.id,
                    fields: BTreeMap::new(),
                }))
            }
            Json::Object(given) if given.len() == 1 => {
                let Some((case, payload)) = given.iter().next() else {
                    return Err(not(at, json, "a case of the enum"));
                };
                let variant = naming(case)?;
                let Some(payload) = payload.as_object() else {
                    return Err(not(at, payload, "an object of the case's fields"));
                };
                let mut fields = BTreeMap::new();
                for field in &variant.fields {
                    let field_at = format!("{at}.{case}.{}", field.name);
                    let ty = parse_ty(&field.ty, &key.app)?;
                    match payload.get(&field.name) {
                        Some(value) => {
                            fields.insert(
                                field.id.clone(),
                                self.field(value, &ty, &field_at, &key.app)?,
                            );
                        }
                        None => {
                            match crate::host_res::declared_default(&ty, field.default.as_ref()) {
                                Ok(value) => {
                                    fields.insert(field.id.clone(), value);
                                }
                                Err(_) => return Err(format!("{field_at} is missing")),
                            }
                        }
                    }
                }
                Ok(Value::Enum(EnumValue {
                    ty: declared.id.clone(),
                    variant: variant.id,
                    fields,
                }))
            }
            other => {
                let cases: Vec<&str> = declared
                    .variants
                    .iter()
                    .map(|variant| variant.name.as_str())
                    .collect();
                Err(not(
                    at,
                    other,
                    &format!(
                        "a case of `{}`: {} (as its name, or as {{\"<case>\": {{...}}}})",
                        key.name,
                        cases.join(", ")
                    ),
                ))
            }
        }
    }
}

/// Whether a value of the type is a resource (a record, a collection, a set), or holds one
/// where a stored value can: text cannot make one.
fn holds_resources(ty: &Ty) -> bool {
    match ty {
        Ty::Record(_) | Ty::Collection(_) | Ty::Set(_) | Ty::Filter(_) => true,
        Ty::Option(t) | Ty::List(t) | Ty::Map(t) | Ty::Result(t, _) => holds_resources(t),
        _ => false,
    }
}

/// The value at `at` is not what the type declares.
fn not(at: &str, json: &Json, what: &str) -> String {
    let written = json.to_string();
    let shown = if written.chars().count() > 48 {
        let cut: String = written.chars().take(45).collect();
        format!("{cut}...")
    } else {
        written
    };
    format!("{at} is not {what}: {shown}")
}

/// The keys an object has, for a message saying what a record was given instead.
fn keys(json: &Json) -> String {
    match json {
        Json::Object(fields) if !fields.is_empty() => fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        _ => "none".to_owned(),
    }
}
