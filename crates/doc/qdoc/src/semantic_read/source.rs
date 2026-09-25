//! Closed mechanical sources. Neither variant is a caller/record authority witness.
use super::{
    AutoCommit, Basis, Limits, ObjId, ObjType, ReadDoc, ReadError, ScalarValueRef, TextPosition,
    Transactable, ValueRef, Work, escaped_bytes, field, object_fingerprint, validate_limits,
};
use automerge::resource_limits::{
    NativeObject, NativeReadError, NativeReadSession, NativeScalar, NativeSelection, NativeValue,
};
use std::ops::ControlFlow;

pub struct Source<'a, 'doc, 'turn>(Kind<'a, 'doc, 'turn>);
enum Kind<'a, 'doc, 'turn> {
    Legacy {
        doc: &'doc AutoCommit,
        heads: &'a [automerge::ChangeHash],
    },
    Native(&'a NativeReadSession<'doc, 'turn>),
}
#[derive(Clone, Debug)]
pub struct Object<'doc>(ObjectKind<'doc>);
#[derive(Clone, Debug)]
enum ObjectKind<'doc> {
    Legacy(ObjId),
    Native(NativeObject<'doc>),
}
pub enum Value<'value, 'doc> {
    Object(ObjType, Object<'doc>),
    Scalar(NativeScalar<'value>),
}
impl<'doc> Value<'_, 'doc> {
    pub fn object(self, wanted: ObjType) -> Option<Object<'doc>> {
        match self {
            Self::Object(kind, object) if kind == wanted => Some(object),
            _ => None,
        }
    }
}
pub struct Entry<'value, 'doc> {
    pub index: usize,
    pub is_mark: bool,
    pub value: Value<'value, 'doc>,
}
fn native_value<'v, 'doc>(value: NativeValue<'v, 'doc>) -> Value<'v, 'doc> {
    match value {
        NativeValue::Object { kind, object } => {
            Value::Object(kind, Object(ObjectKind::Native(object)))
        }
        NativeValue::Scalar(value) => Value::Scalar(value),
    }
}
fn legacy_value<'v, 'doc>(value: &'v ValueRef<'_>, object: ObjId) -> Value<'v, 'doc> {
    use ScalarValueRef as S;
    let value = match value {
        ValueRef::Object(kind) => return Value::Object(*kind, Object(ObjectKind::Legacy(object))),
        ValueRef::Scalar(value) => value,
    };
    Value::Scalar(match value {
        S::Bytes(v) => NativeScalar::Bytes(v),
        S::Str(v) => NativeScalar::Str(v),
        S::Int(v) => NativeScalar::Int(*v),
        S::Uint(v) => NativeScalar::Uint(*v),
        S::F64(v) => NativeScalar::F64(*v),
        S::Counter(v) => NativeScalar::Counter(*v),
        S::Timestamp(v) => NativeScalar::Timestamp(*v),
        S::Boolean(v) => NativeScalar::Boolean(*v),
        S::Unknown { type_code, bytes } => NativeScalar::Unknown {
            type_code: *type_code,
            bytes,
        },
        S::Null => NativeScalar::Null,
    })
}
/// Lowercase hexadecimal digit of a nibble (`0..16`).
fn hex_digit(nibble: u8) -> char {
    char::from_digit(u32::from(nibble), 16).unwrap_or('0')
}
fn read_error(error: NativeReadError<ReadError>) -> ReadError {
    match error {
        NativeReadError::Resource(e) => e.into(),
        NativeReadError::Visitor(e) => e,
    }
}
impl<'a, 'doc, 'turn> Source<'a, 'doc, 'turn> {
    /// Legacy mechanics only; supplied heads are data, not a binding witness.
    pub fn legacy(doc: &'doc AutoCommit, heads: &'a [automerge::ChangeHash]) -> Self {
        Self(Kind::Legacy { doc, heads })
    }
    /// Uses only an actual retained session and its complete native frontier.
    pub fn native(read: &'a NativeReadSession<'doc, 'turn>) -> Self {
        Self(Kind::Native(read))
    }
    pub fn root(&self) -> Object<'doc> {
        match self.0 {
            Kind::Legacy { .. } => Object(ObjectKind::Legacy(automerge::ROOT)),
            Kind::Native(read) => Object(ObjectKind::Native(read.root())),
        }
    }
    /// Wrap a legacy object id.
    ///
    /// # Errors
    ///
    /// [`ReadError::Malformed`] when this is a native source.
    pub fn legacy_object(&self, object: &ObjId) -> Result<Object<'doc>, ReadError> {
        match self.0 {
            Kind::Legacy { .. } => Ok(Object(ObjectKind::Legacy(object.clone()))),
            Kind::Native(_) => Err(ReadError::Malformed),
        }
    }
    /// Wrap a native object after checking it resolves in this session.
    ///
    /// # Errors
    ///
    /// [`ReadError::Malformed`] when this is a legacy source, or the session's resource error.
    pub fn native_object(&self, object: NativeObject<'doc>) -> Result<Object<'doc>, ReadError> {
        match self.0 {
            Kind::Native(read) => {
                read.visit_id_parts(object.id(), |_| ())?;
                Ok(Object(ObjectKind::Native(object)))
            }
            Kind::Legacy { .. } => Err(ReadError::Malformed),
        }
    }
    /// Bind a projection basis to the source's complete head set.
    ///
    /// # Errors
    ///
    /// [`ReadError::Pending`] for uncommitted operations, [`ReadError::Limit`] for invalid limits,
    /// oversized documents or more than 64 heads, and [`ReadError::Malformed`] for duplicate heads.
    pub fn begin(&self, limits: Limits, max_items: usize) -> Result<(Basis, Work), ReadError> {
        validate_limits(limits, max_items)?;
        let mut heads = [automerge::ChangeHash([0; 32]); 64];
        let count = match self.0 {
            Kind::Legacy { doc, heads: input } => {
                if doc.pending_ops() != 0 {
                    return Err(ReadError::Pending);
                }
                if doc.stats().num_ops > 1_000_000 || input.len() > 64 {
                    return Err(ReadError::Limit);
                }
                heads
                    .get_mut(..input.len())
                    .ok_or(ReadError::Limit)?
                    .copy_from_slice(input);
                input.len()
            }
            Kind::Native(read) => {
                if read.pending_ops() != 0 {
                    return Err(ReadError::Pending);
                }
                if read.operation_count() > 1_000_000 {
                    return Err(ReadError::Limit);
                }
                let mut index = 0;
                read.visit_heads(64, |head| {
                    // More heads than slots leaves `index` past the end, which
                    // the bounded slice below refuses.
                    if let Some(slot) = heads.get_mut(index) {
                        *slot = head;
                    }
                    index = index.saturating_add(1);
                })?;
                index
            }
        };
        let heads = heads.get_mut(..count).ok_or(ReadError::Limit)?;
        heads.sort_unstable();
        if heads
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left == right))
        {
            return Err(ReadError::Malformed);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"qdoc-semantic-basis-v1\0");
        let mut encoded = Vec::with_capacity(count);
        for head in heads {
            hasher.update(&head.0);
            let mut text = String::with_capacity(64);
            for byte in head.0 {
                text.push(hex_digit(byte >> 4));
                text.push(hex_digit(byte & 15));
            }
            encoded.push(text);
        }
        Ok((
            Basis {
                heads: encoded,
                fingerprint: *hasher.finalize().as_bytes(),
            },
            Work {
                steps: automerge::resource_limits::NativeReadSteps::new(limits.scan_entries)?,
            },
        ))
    }
    /// Canonical cursor scope of `object` (see [`object_fingerprint`]).
    ///
    /// # Errors
    ///
    /// [`ReadError::Malformed`] when `object` belongs to the other source kind, and
    /// [`ReadError::Limit`] for an actor id over 256 bytes.
    pub fn fingerprint(&self, object: &Object<'doc>) -> Result<[u8; 32], ReadError> {
        match (&self.0, &object.0) {
            (Kind::Legacy { .. }, ObjectKind::Legacy(object)) => object_fingerprint(object),
            (Kind::Native(read), ObjectKind::Native(object)) => {
                read.visit_id_parts(object.id(), |parts| {
                    let mut hash = blake3::Hasher::new();
                    hash.update(b"qdoc-semantic-object-v1\0");
                    match parts {
                        None => {
                            hash.update(&[0]);
                        }
                        Some((counter, actor)) => {
                            if actor.len() > 256 {
                                return Err(ReadError::Limit);
                            }
                            hash.update(&[1]);
                            hash.update(&counter.to_le_bytes());
                            hash.update(actor);
                        }
                    }
                    Ok(*hash.finalize().as_bytes())
                })?
            }
            _ => Err(ReadError::Malformed),
        }
    }
    /// Visit the winning value under `key`, if any.
    ///
    /// # Errors
    ///
    /// [`ReadError::Limit`] when `key` exceeds 256 bytes, [`ReadError::Malformed`] for an object of
    /// the other source kind, or any error returned by `visitor`.
    pub fn field(
        &self,
        object: &Object<'doc>,
        key: &str,
        work: &Work,
        mut visitor: impl for<'v> FnMut(Value<'v, 'doc>) -> Result<(), ReadError>,
    ) -> Result<(), ReadError> {
        if key.len() > 256 {
            return Err(ReadError::Limit);
        }
        match (&self.0, &object.0) {
            (Kind::Legacy { doc, .. }, ObjectKind::Legacy(object)) => {
                if let Some(item) = field(doc, object, key)? {
                    visitor(legacy_value(&item.value, item.id()))?;
                }
            }
            (Kind::Native(read), ObjectKind::Native(object)) => {
                read.visit_property_with_steps(
                    *object,
                    key,
                    NativeSelection::Winner,
                    &work.steps,
                    |value, _| visitor(native_value(value)),
                )
                .map_err(read_error)?;
            }
            _ => return Err(ReadError::Malformed),
        }
        Ok(())
    }
    /// The winning object of `kind` under `key`, if any.
    ///
    /// # Errors
    ///
    /// As [`Source::field`].
    pub fn object(
        &self,
        object: &Object<'doc>,
        key: &str,
        kind: ObjType,
        work: &Work,
    ) -> Result<Option<Object<'doc>>, ReadError> {
        let mut result = None;
        self.field(object, key, work, |value| {
            result = value.object(kind);
            Ok(())
        })?;
        Ok(result)
    }
    /// The winning string under `key`, or empty when it is absent or not a string.
    ///
    /// # Errors
    ///
    /// As [`Source::field`], and [`ReadError::Limit`] when the string exceeds `maximum` bytes.
    pub fn string(
        &self,
        object: &Object<'doc>,
        key: &str,
        maximum: usize,
        work: &Work,
    ) -> Result<String, ReadError> {
        let mut result = String::new();
        self.field(object, key, work, |value| {
            if let Value::Scalar(NativeScalar::Str(text)) = value {
                if text.len() > maximum {
                    return Err(ReadError::Limit);
                }
                result.push_str(text);
            }
            Ok(())
        })?;
        Ok(result)
    }
    /// Visit the winning byte payload under `key`, if any.
    ///
    /// # Errors
    ///
    /// As [`Source::field`].
    pub fn bytes(
        &self,
        object: &Object<'doc>,
        key: &str,
        work: &Work,
        mut visitor: impl for<'v> FnMut(&'v [u8]) -> Result<(), ReadError>,
    ) -> Result<(), ReadError> {
        self.field(object, key, work, |value| {
            if let Value::Scalar(NativeScalar::Bytes(bytes)) = value {
                visitor(bytes)?;
            }
            Ok(())
        })
    }
    /// Visit every visible map entry until `visitor` breaks.
    ///
    /// # Errors
    ///
    /// [`ReadError::Malformed`] for an object of the other source kind, [`ReadError::Limit`] when
    /// the scan budget is exhausted, or any error returned by `visitor`.
    pub fn map(
        &self,
        object: &Object<'doc>,
        work: &Work,
        mut visitor: impl for<'k, 'v> FnMut(
            &'k str,
            Value<'v, 'doc>,
        ) -> Result<ControlFlow<()>, ReadError>,
    ) -> Result<(), ReadError> {
        match (&self.0, &object.0) {
            (Kind::Legacy { doc, .. }, ObjectKind::Legacy(object)) => {
                for item in doc.map_range(object, ..) {
                    work.visit()?;
                    if visitor(&item.key, legacy_value(&item.value, item.id()))?.is_break() {
                        break;
                    }
                }
            }
            (Kind::Native(read), ObjectKind::Native(native)) => {
                read.visit_keys_with_steps(*native, &work.steps, |key| {
                    work.visit()?;
                    let mut flow = ControlFlow::Continue(());
                    // Map keys remain borrowed. Let the app inspect the actual
                    // winner before enforcing metadata limits: malformed rows
                    // with long keys must retain legacy skip semantics.
                    read.visit_property_with_steps(
                        *native,
                        key,
                        NativeSelection::Winner,
                        &work.steps,
                        |value, _| {
                            flow = visitor(key, native_value(value))?;
                            Ok(())
                        },
                    )
                    .map_err(read_error)?;
                    Ok(flow)
                })
                .map_err(read_error)?;
            }
            _ => return Err(ReadError::Malformed),
        }
        Ok(())
    }
    /// Visit every visible list element until `visitor` breaks.
    ///
    /// # Errors
    ///
    /// As [`Source::map`].
    pub fn list(
        &self,
        object: &Object<'doc>,
        work: &Work,
        mut visitor: impl for<'v> FnMut(Entry<'v, 'doc>) -> Result<ControlFlow<()>, ReadError>,
    ) -> Result<(), ReadError> {
        match (&self.0, &object.0) {
            (Kind::Legacy { doc, .. }, ObjectKind::Legacy(object)) => {
                for item in doc.list_range(object, ..) {
                    work.visit()?;
                    let entry = Entry {
                        index: item.index,
                        is_mark: item.is_mark(),
                        value: legacy_value(&item.value, item.id()),
                    };
                    if visitor(entry)?.is_break() {
                        break;
                    }
                }
            }
            (Kind::Native(read), ObjectKind::Native(object)) => {
                read.visit_list(*object, &work.steps, |entry| {
                    work.visit()?;
                    visitor(Entry {
                        index: entry.index,
                        is_mark: entry.is_mark,
                        value: native_value(entry.value),
                    })
                })
                .map_err(read_error)?;
            }
            _ => return Err(ReadError::Malformed),
        }
        Ok(())
    }
    /// A bounded text fragment from `position` (see [`super::text_fragment`]).
    ///
    /// # Errors
    ///
    /// [`ReadError::InvalidCursor`] for a cursor outside the text and [`ReadError::Limit`] when the
    /// scan or escaped budget cannot make progress.
    pub fn text_fragment(
        &self,
        text: Option<&Object<'doc>>,
        position: TextPosition,
        escaped_budget: usize,
        work: &Work,
    ) -> Result<(String, TextPosition, bool), ReadError> {
        let Some(text) = text else {
            return if position == TextPosition::default() {
                Ok((String::new(), position, true))
            } else {
                Err(ReadError::InvalidCursor)
            };
        };
        let mut output = String::new();
        let mut spent = 0usize;
        let mut next = TextPosition::default();
        let mut found = position == TextPosition::default();
        let mut complete = true;
        self.list(text, work, |item| {
            if item.index < position.element || item.is_mark {
                return Ok(ControlFlow::Continue(()));
            }
            let value = match item.value {
                Value::Scalar(NativeScalar::Str(s)) => s,
                _ => "\u{fffc}",
            };
            let start = if item.index == position.element {
                position.byte
            } else {
                0
            };
            if !value.is_char_boundary(start) {
                return Err(ReadError::InvalidCursor);
            }
            found = true;
            let suffix = value.get(start..).ok_or(ReadError::InvalidCursor)?;
            for (offset, c) in suffix.char_indices() {
                work.visit()?;
                let size = escaped_bytes(c);
                let Some(total) = spent.checked_add(size).filter(|sum| *sum <= escaped_budget)
                else {
                    if output.is_empty() {
                        return Err(ReadError::Limit);
                    }
                    next = TextPosition {
                        element: item.index,
                        byte: start.checked_add(offset).ok_or(ReadError::Limit)?,
                    };
                    complete = false;
                    return Ok(ControlFlow::Break(()));
                };
                output.push(c);
                spent = total;
            }
            next = TextPosition {
                element: item.index.checked_add(1).ok_or(ReadError::Limit)?,
                byte: 0,
            };
            Ok(ControlFlow::Continue(()))
        })?;
        if !found && position != next {
            return Err(ReadError::InvalidCursor);
        }
        Ok((output, next, complete))
    }
}
