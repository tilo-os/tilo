//! Existing governed-value fingerprints, shared by legacy and paid readers.
use crate::room::read::{LegacyRead, ReadError, RoomRead};
use automerge::resource_limits::{
    NativeReadError, NativeReadSession, NativeResourceError, NativeScalar, NativeSelection,
    NativeValue,
};
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
enum Shape<'a> {
    Bytes(&'a [u8]),
    Str(&'a str),
    InvalidScalar,
    Object(ObjType),
}
fn fingerprint(
    shape: Shape<'_>,
    extra: impl FnOnce(&mut Hash) -> Result<(), NativeResourceError>,
) -> Result<[u8; 32], NativeResourceError> {
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
            write!(&mut hash, "{kind:?}").expect("hash sink is infallible");
        }
    }
    extra(&mut hash)?;
    Ok(*hash.0.finalize().as_bytes())
}
fn legacy(value: Value<'_>, object: &ObjId) -> [u8; 32] {
    match value {
        Value::Scalar(value) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => fingerprint(Shape::Bytes(bytes), |_| Ok(())),
            ScalarValue::Str(text) => fingerprint(Shape::Str(text), |_| Ok(())),
            value => fingerprint(Shape::InvalidScalar, |hash| {
                write!(hash, "{value:?}").expect("hash sink is infallible");
                Ok(())
            }),
        },
        Value::Object(kind) => fingerprint(Shape::Object(kind), |hash| {
            // Explicitly unqualified legacy adapter retains the original ID
            // codec. Native uses that same codec with paid caller storage.
            hash.0.update(&object.to_bytes());
            Ok(())
        }),
    }
    .expect("legacy fingerprint has no fallible resource work")
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
impl FingerprintRead for NativeReadSession<'_, '_> {
    fn visit_media_fingerprints<E>(
        &self,
        mut visitor: impl FnMut([u8; 32]) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>> {
        self.visit_property(
            self.root(),
            super::FIELD,
            NativeSelection::All,
            |value, id| {
                let hash = match value {
                    NativeValue::Scalar(NativeScalar::Bytes(bytes)) => {
                        fingerprint(Shape::Bytes(bytes), |_| Ok(()))
                    }
                    NativeValue::Scalar(NativeScalar::Str(text)) => {
                        fingerprint(Shape::Str(text), |_| Ok(()))
                    }
                    NativeValue::Scalar(value) => fingerprint(Shape::InvalidScalar, |hash| {
                        value
                            .write_legacy_debug(hash)
                            .expect("hash sink is infallible");
                        Ok(())
                    }),
                    NativeValue::Object { kind, .. } => fingerprint(Shape::Object(kind), |hash| {
                        let length = self.id_bytes_len(id)?;
                        let _retention = self.reserve_output(length)?;
                        let mut bytes = Vec::new();
                        // Pinned Global uses the requested exact byte layout.
                        // The reservation precedes allocation and outlives bytes.
                        bytes
                            .try_reserve_exact(length)
                            .map_err(|_| NativeResourceError::WorkBytes)?;
                        if bytes.capacity() > length {
                            return Err(NativeResourceError::Unaccounted);
                        }
                        bytes.resize(length, 0);
                        self.write_id_bytes(id, &mut bytes)?;
                        hash.0.update(&bytes);
                        Ok(())
                    }),
                }
                .map_err(ReadError::Resource)?;
                visitor(hash).map_err(ReadError::Visitor)
            },
        )
        .map_err(|error| match error {
            NativeReadError::Resource(error) => ReadError::Resource(error),
            NativeReadError::Visitor(error) => error,
        })
    }
}
#[cfg(test)]
mod tests;
