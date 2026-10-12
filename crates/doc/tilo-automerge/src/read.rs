//! Typed reads of one map entry or list element.
//!
//! Each reader answers `None` when the entry is missing, is a different type,
//! or cannot be read, so callers state the type they expect instead of
//! matching `Ok(Some((Value::Scalar(..), _)))` themselves. Integer readers
//! take either integer encoding when the value fits, so a count one writer
//! stored as `Uint` reads the same through [`ReadTyped::int_at`]; floats are
//! never truncated into integers.

use automerge::{ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value};

/// Typed readers for every [`ReadDoc`].
pub trait ReadTyped: ReadDoc {
    /// The string at `prop` of `obj`.
    fn str_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<String> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The integer at `prop` of `obj`, when it fits an `i64`.
    fn int_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<i64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Int(number) => Some(*number),
                ScalarValue::Uint(number) => i64::try_from(*number).ok(),
                _ => None,
            },
            _ => None,
        }
    }

    /// The integer at `prop` of `obj`, when it fits a `u64`.
    fn uint_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<u64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Uint(number) => Some(*number),
                ScalarValue::Int(number) => u64::try_from(*number).ok(),
                _ => None,
            },
            _ => None,
        }
    }

    /// The number at `prop` of `obj` as a float.
    #[allow(
        clippy::cast_precision_loss,
        reason = "document numbers read as floats are measures well below 2^53"
    )]
    fn f64_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<f64> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::F64(number) => Some(*number),
                ScalarValue::Int(number) => Some(*number as f64),
                ScalarValue::Uint(number) => Some(*number as f64),
                _ => None,
            },
            _ => None,
        }
    }

    /// The boolean at `prop` of `obj`.
    fn bool_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<bool> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Boolean(flag) => Some(*flag),
                _ => None,
            },
            _ => None,
        }
    }

    /// The bytes at `prop` of `obj`.
    fn bytes_at(&self, obj: &ObjId, prop: impl Into<Prop>) -> Option<Vec<u8>> {
        match self.get(obj, prop) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => Some(bytes.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The object of type `kind` at `prop` of `obj`.
    fn obj_at(&self, obj: &ObjId, prop: impl Into<Prop>, kind: ObjType) -> Option<ObjId> {
        match self.get(obj, prop) {
            Ok(Some((Value::Object(found), id))) if found == kind => Some(id),
            _ => None,
        }
    }
}

impl<T: ReadDoc + ?Sized> ReadTyped for T {}
