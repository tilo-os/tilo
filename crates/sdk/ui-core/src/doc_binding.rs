//! Control bindings into the app's Automerge document: a value kept under a
//! key of the object the control is synced against.

use std::marker::PhantomData;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::widget::binding::{BoxedBinding, Pulled, ValueBinding};
use crate::widget::{AppModel, ModelPath};

/// How one kind of value is stored under a document key.
trait Codec: 'static {
    type Value;
    fn read(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<Self::Value>;
    fn write(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: &Self::Value);
}

fn scalar(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ScalarValue> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(scalar), _))) => Some(scalar.into_owned()),
        _ => None,
    }
}

struct Bool;

impl Codec for Bool {
    type Value = bool;

    fn read(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<bool> {
        match scalar(doc, obj, key)? {
            ScalarValue::Boolean(value) => Some(value),
            _ => None,
        }
    }

    fn write(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: &bool) {
        let _ = doc.put(obj, key, ScalarValue::Boolean(*value));
    }
}

struct Number;

impl Codec for Number {
    type Value = f64;

    #[allow(
        clippy::cast_precision_loss,
        reason = "integer document values are presented by f64 controls"
    )]
    fn read(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<f64> {
        match scalar(doc, obj, key)? {
            ScalarValue::F64(value) => Some(value),
            ScalarValue::Int(value) => Some(value as f64),
            ScalarValue::Uint(value) => Some(value as f64),
            _ => None,
        }
    }

    fn write(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: &f64) {
        let _ = doc.put(obj, key, ScalarValue::F64(*value));
    }
}

/// Collaborative text: an Automerge `Text` edited by splicing the changed
/// span. A plain string scalar reads as well.
struct Text;

impl Codec for Text {
    type Value = String;

    fn read(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((Value::Object(ObjType::Text), text))) => doc.text(&text).ok(),
            Ok(Some((Value::Scalar(scalar), _))) => match scalar.as_ref() {
                ScalarValue::Str(value) => Some(value.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn write(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: &String) {
        let text = match doc.get(obj, key) {
            Ok(Some((Value::Object(ObjType::Text), text))) => Some(text),
            _ => doc.put_object(obj, key, ObjType::Text).ok(),
        };
        if let Some(text) = text {
            crate::doc::splice_text_diffed(doc, &text, value);
        } else {
            let _ = doc.put(obj, key, value.as_str());
        }
    }
}

/// A whole string replaced on each write.
struct Str;

impl Codec for Str {
    type Value = String;

    fn read(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
        match scalar(doc, obj, key)? {
            ScalarValue::Str(value) => Some(value.to_string()),
            _ => None,
        }
    }

    fn write(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: &String) {
        let _ = doc.put(obj, key, value.as_str());
    }
}

struct DocKey<C> {
    key: String,
    codec: PhantomData<C>,
}

impl<C: Codec> ValueBinding<C::Value> for DocKey<C> {
    fn pull(&mut self, model: &mut AppModel, path: &ModelPath) -> Pulled<C::Value> {
        let (doc, obj) = crate::doc::document(model, path);
        C::read(doc, obj, &self.key).map_or(Pulled::Missing, Pulled::Value)
    }

    fn push(&mut self, model: &mut AppModel, path: &ModelPath, value: &C::Value) {
        let (doc, obj) = crate::doc::document(model, path);
        C::write(doc, obj, &self.key, value);
    }

    fn document_key(&self) -> Option<&str> {
        (!self.key.is_empty()).then_some(self.key.as_str())
    }
}

fn key<C: Codec>(key: &str) -> BoxedBinding<C::Value> {
    Box::new(DocKey::<C> {
        key: key.to_owned(),
        codec: PhantomData,
    })
}

/// A boolean under `key`.
pub fn bool_key(key_name: &str) -> BoxedBinding<bool> {
    key::<Bool>(key_name)
}

/// A number under `key`; integers read as numbers too.
pub fn number_key(key_name: &str) -> BoxedBinding<f64> {
    key::<Number>(key_name)
}

/// Collaborative text under `key`, spliced on each edit.
pub fn text_key(key_name: &str) -> BoxedBinding<String> {
    key::<Text>(key_name)
}

/// A string under `key`, replaced whole on each edit.
pub fn str_key(key_name: &str) -> BoxedBinding<String> {
    key::<Str>(key_name)
}
