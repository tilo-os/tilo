//! What a version changed of its record's fields, as one reader may see it: each field's value as
//! the change found it and as it left it, read from the record's own history when history is read,
//! so nothing beyond the record is stored or sent for it. A record kept on one device has its
//! values there alone, as it has the record.

use serde_json::Value as Json;

use crate::core::Core;
use crate::ids::Actor;
use crate::meta::{FieldKind, Ty};
use crate::store::{FieldSpec, Version};
use crate::value::{ResourceRef, Value};

/// One field a change wrote, as a reader may see it. `before` is there only for a reader who read
/// the record both before and after the change, so one the change let in learns nothing of what it
/// held before; `after` only for one who reads it after. A secret field has neither: history says
/// only that it changed.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldChange {
    /// The field's name, as the type declares it.
    pub field: String,
    pub before: Option<Json>,
    pub after: Option<Json>,
    pub secret: bool,
}

impl Core {
    /// The fields the version `v` changed, each with what `reader` may see of its values
    /// ([`FieldChange`]). A text, a number, a flag, a person or an enum's case has a value said;
    /// a collection or a record none. Nothing for a version not committed yet, or of a record
    /// this store does not hold.
    pub(crate) fn field_changes(&self, reader: &Actor, v: &Version) -> Vec<FieldChange> {
        if v.change.is_empty() || v.fields.is_empty() {
            return Vec::new();
        }
        let Some(at) = ResourceRef::parse_id(&v.resource, "") else {
            return Vec::new();
        };
        let at = ResourceRef {
            ty: self.stored_type(&at),
            ..at
        };
        let Ok((_, info)) = self.type_of_ref(&at.ty) else {
            return Vec::new();
        };
        let fields: Vec<&crate::registry::FieldInfo> = v
            .fields
            .iter()
            .filter_map(|id| info.field(id))
            .filter(|f| {
                matches!(
                    f.kind,
                    FieldKind::Register | FieldKind::Counter | FieldKind::Text
                ) && said_type(&f.ty)
            })
            .collect();
        let specs: Vec<FieldSpec> = fields
            .iter()
            .map(|f| FieldSpec {
                id: f.id.clone(),
                kind: f.kind,
            })
            .collect();
        let Ok(written) = self
            .store
            .borrow()
            .written(&at, &specs, (&v.change, &v.change))
        else {
            return Vec::new();
        };
        let after_read = self.read_around_change(reader, &at, &v.change, true);
        let before_read = after_read && self.read_around_change(reader, &at, &v.change, false);
        written
            .into_iter()
            .filter_map(|(id, before, after)| {
                let field = fields.iter().find(|f| f.id == id)?;
                let said = |held, shown: bool| {
                    if field.secret || !shown {
                        return None;
                    }
                    let value = self.field_value(&at, field, held, None).ok()?;
                    self.said_json(&value)
                };
                Some(FieldChange {
                    field: field.name.clone(),
                    before: said(before, before_read),
                    after: said(after, after_read),
                    secret: field.secret,
                })
            })
            .collect()
    }

    /// `value` as history's words read it: a flag, a number, text, a person or an enum's case by
    /// its name. `None` for what words do not say (a record, a list, a reference).
    fn said_json(&self, value: &Value) -> Option<Json> {
        Some(match value {
            Value::Bool(on) => Json::Bool(*on),
            Value::Int(n) => Json::from(*n),
            Value::Float(x) => serde_json::Number::from_f64(*x).map(Json::Number)?,
            Value::Text(text) => Json::String(text.clone()),
            Value::Actor(who) => Json::String(who.id().to_owned()),
            Value::Opt(Some(inner)) => return self.said_json(inner),
            Value::Opt(None) | Value::Unit => Json::Null,
            Value::Enum(case) => {
                let name = self
                    .registry
                    .borrow()
                    .enum_meta(&case.ty)
                    .and_then(|declared| {
                        declared
                            .variants
                            .iter()
                            .find(|variant| variant.id == case.variant)
                            .map(|variant| variant.name.clone())
                    })
                    .unwrap_or_else(|| case.variant.clone());
                Json::String(name)
            }
            _ => return None,
        })
    }
}

/// Whether a value of `ty` is one words say: a flag, a number, text, a moment, a person or an
/// enum's case, or an optional one of them.
fn said_type(ty: &Ty) -> bool {
    match ty {
        Ty::Int
        | Ty::Float
        | Ty::Bool
        | Ty::Text
        | Ty::Actor
        | Ty::Date
        | Ty::Time
        | Ty::Identifier
        | Ty::Enum(_) => true,
        Ty::Option(inner) => said_type(inner),
        _ => false,
    }
}
