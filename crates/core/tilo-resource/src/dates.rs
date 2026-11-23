//! Dates and times as model values: written whole as their ISO
//! 8601 text, last writer wins.

use automerge::{ObjId, ScalarValue, Value};
use jiff::civil::{Date, DateTime, Time};

use crate::model::{Doc, Model, ModelError, Slot, json_shape};
use crate::path::Settable;

macro_rules! date_model {
    ($($ty:ty => $schema:literal, $what:literal, $field:ident;)*) => {$(
        impl Settable for $ty {}
        impl Model for $ty {
            crate::__own_table!($ty);
            crate::model::leaf_paths!();
            fn field_type(types: &mut crate::TypeSet) -> crate::FieldType {
                let _ = types;
                crate::FieldType::$field
            }
            fn to_json(&self) -> serde_json::Value {
                serde_json::Value::String(self.to_string())
            }
            fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
                match value {
                    serde_json::Value::String(text) => text
                        .parse()
                        .map_err(|_| ModelError::invalid(format!("not {}: {text}", $what))),
                    _ => Err(json_shape($what)),
                }
            }
            fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
                match value {
                    Value::Scalar(scalar) => match scalar.as_ref() {
                        ScalarValue::Str(text) => text
                            .parse()
                            .map_err(|_| ModelError::invalid(format!("not {}: {text}", $what))),
                        _ => Err(ModelError::shape($what)),
                    },
                    Value::Object(_) => Err(ModelError::shape($what)),
                }
            }
            fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
                slot.put_scalar(doc, ScalarValue::Str(self.to_string().into()))
            }
            fn collect_text(&self, out: &mut Vec<String>) {
                out.push(self.to_string());
            }
        }
        impl crate::JsonSchema for $ty {
            fn json_schema() -> serde_json::Value {
                serde_json::json!({"type": "string", "pattern": $schema})
            }
            fn field_type(_: &mut crate::TypeSet) -> crate::FieldType {
                crate::FieldType::$field
            }
        }
    )*};
}

date_model! {
    Date => r"^\d{4}-\d{2}-\d{2}$", "a date", Date;
    Time => r"^\d{2}:\d{2}:\d{2}(\.\d+)?$", "a time", Text;
    DateTime =>
        r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?$", "a date and time", Time;
}
