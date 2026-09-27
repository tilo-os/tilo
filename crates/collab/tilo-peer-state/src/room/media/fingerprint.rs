//! Existing governed-value fingerprints.
use crate::room::read::{LegacyRead, ReadError, RoomRead};
use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use std::fmt::Write;

pub(crate) trait FingerprintRead: RoomRead {
    fn visit_media_fingerprints<E>(
        &self,
        visitor: impl FnMut([u8; 32]) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>>;
}
struct Hash(blake3::Hasher);
impl std::fmt::Write for Hash {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0.update(text.as_bytes());
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum Shape<'a> {
    Bytes(&'a [u8]),
    Str(&'a str),
    InvalidScalar,
    Object(ObjType),
}
fn fingerprint(shape: Shape<'_>, extra: impl FnOnce(&mut Hash)) -> [u8; 32] {
    let mut hash = Hash(blake3::Hasher::new());
    match shape {
        Shape::Bytes(bytes) => {
            hash.0.update(b"bytes\0");
            hash.0.update(bytes);
        }
        Shape::Str(text) => {
            hash.0.update(b"string\0");
            hash.0.update(text.as_bytes());
        }
        Shape::InvalidScalar => {
            hash.0.update(b"invalid-scalar\0");
        }
        Shape::Object(kind) => {
            hash.0.update(b"invalid-object\0");
            // The hash sink never fails.
            let _ = write!(&mut hash, "{kind:?}");
        }
    }
    extra(&mut hash);
    *hash.0.finalize().as_bytes()
}
fn legacy(value: Value<'_>, object: &ObjId) -> [u8; 32] {
    match value {
        Value::Scalar(value) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => fingerprint(Shape::Bytes(bytes), |_| ()),
            ScalarValue::Str(text) => fingerprint(Shape::Str(text), |_| ()),
            value => fingerprint(Shape::InvalidScalar, |hash| {
                let _ = write!(hash, "{value:?}");
            }),
        },
        Value::Object(kind) => fingerprint(Shape::Object(kind), |hash| {
            hash.0.update(&object.to_bytes());
        }),
    }
}
impl<D: ReadDoc> FingerprintRead for LegacyRead<'_, D> {
    fn visit_media_fingerprints<E>(
        &self,
        mut visitor: impl FnMut([u8; 32]) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>> {
        let values = self
            .0
            .get_all(ROOT, super::FIELD)
            .map_err(|_| ReadError::Malformed)?;
        let count = values.len();
        for (value, object) in values {
            visitor(legacy(value, &object)).map_err(ReadError::Visitor)?;
        }
        Ok(count)
    }
}
#[cfg(test)]
mod tests;
