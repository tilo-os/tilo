//! The bytes a document call and its answer travel as between a model module
//! and its host: one compact form both sides read and write with the same
//! code, so what the module asks is what the host does.
//!
//! Integers are LEB128 (signed ones zigzag first), byte strings and lists are
//! length-prefixed, and every enum is a tag byte followed by its fields.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Bound;

use crate::exid::ExId;
use crate::hydrate;
use crate::iter::Span;
use crate::marks::{ExpandMark, Mark, MarkSet};
use crate::{
    ActorId, ChangeHash, ChangeMetadata, Cursor, CursorPosition, MoveCursor, ObjType, Prop, ScalarValue,
    TextEncoding, Value,
};

/// What a call or an answer is written into.
#[derive(Debug, Default)]
pub struct Writer(pub Vec<u8>);

/// What a call or an answer is read from.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
}

/// The bytes ended or held something no writer writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put<T: Put + ?Sized>(&mut self, value: &T) -> &mut Self {
        value.put(self);
        self
    }

    pub fn byte(&mut self, byte: u8) {
        self.0.push(byte);
    }

    pub fn uint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }

    pub fn int(&mut self, value: i64) {
        self.uint(((value << 1) ^ (value >> 63)) as u64);
    }

    pub fn bytes(&mut self, bytes: &[u8]) {
        self.uint(bytes.len() as u64);
        self.0.extend_from_slice(bytes);
    }
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn take<T: Wire>(&mut self) -> Result<T, Malformed> {
        T::take(self)
    }

    pub fn byte(&mut self) -> Result<u8, Malformed> {
        let (first, rest) = self.bytes.split_first().ok_or(Malformed)?;
        self.bytes = rest;
        Ok(*first)
    }

    pub fn uint(&mut self) -> Result<u64, Malformed> {
        let mut value = 0_u64;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            if shift >= 64 {
                return Err(Malformed);
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    pub fn int(&mut self) -> Result<i64, Malformed> {
        let value = self.uint()?;
        Ok(((value >> 1) as i64) ^ -((value & 1) as i64))
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], Malformed> {
        let length = usize::try_from(self.uint()?).map_err(|_| Malformed)?;
        if length > self.bytes.len() {
            return Err(Malformed);
        }
        let (taken, rest) = self.bytes.split_at(length);
        self.bytes = rest;
        Ok(taken)
    }

    pub fn str(&mut self) -> Result<&'a str, Malformed> {
        std::str::from_utf8(self.bytes()?).map_err(|_| Malformed)
    }
}

/// What has a form on the wire.
pub trait Put {
    fn put(&self, w: &mut Writer);
}

/// A value with a form on the wire, read back as it was written.
pub trait Wire: Put + Sized {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed>;
}

impl Put for () {
    fn put(&self, _: &mut Writer) {}
}

impl Wire for () {
    fn take(_: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok(())
    }
}

impl Put for bool {
    fn put(&self, w: &mut Writer) {
        w.byte(u8::from(*self));
    }
}

impl Wire for bool {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok(r.byte()? != 0)
    }
}

impl Put for u8 {
    fn put(&self, w: &mut Writer) {
        w.byte(*self);
    }
}

impl Wire for u8 {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        r.byte()
    }
}

impl Put for u32 {
    fn put(&self, w: &mut Writer) {
        w.uint(u64::from(*self));
    }
}

impl Wire for u32 {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        u32::try_from(r.uint()?).map_err(|_| Malformed)
    }
}

impl Put for u64 {
    fn put(&self, w: &mut Writer) {
        w.uint(*self);
    }
}

impl Wire for u64 {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        r.uint()
    }
}

impl Put for usize {
    fn put(&self, w: &mut Writer) {
        w.uint(*self as u64);
    }
}

impl Wire for usize {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        usize::try_from(r.uint()?).map_err(|_| Malformed)
    }
}

impl Put for i64 {
    fn put(&self, w: &mut Writer) {
        w.int(*self);
    }
}

impl Wire for i64 {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        r.int()
    }
}

impl Put for isize {
    fn put(&self, w: &mut Writer) {
        w.int(*self as i64);
    }
}

impl Wire for isize {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        isize::try_from(r.int()?).map_err(|_| Malformed)
    }
}

impl Put for str {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.as_bytes());
    }
}

impl Put for String {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.as_bytes());
    }
}

impl Wire for String {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok(r.str()?.to_owned())
    }
}

impl<T: Put> Put for Option<T> {
    fn put(&self, w: &mut Writer) {
        match self {
            None => w.byte(0),
            Some(value) => {
                w.byte(1);
                value.put(w);
            }
        }
    }
}

impl<T: Wire> Wire for Option<T> {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(None),
            1 => Ok(Some(T::take(r)?)),
            _ => Err(Malformed),
        }
    }
}

impl<T: Put> Put for Vec<T> {
    fn put(&self, w: &mut Writer) {
        self.as_slice().put(w);
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        let length = usize::try_from(r.uint()?).map_err(|_| Malformed)?;
        // Every item takes at least one byte, so a length beyond what is
        // left is a lie, refused before anything is allocated for it.
        if length > r.bytes.len() {
            return Err(Malformed);
        }
        (0..length).map(|_| T::take(r)).collect()
    }
}

impl<T: Put> Put for [T] {
    fn put(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        for item in self {
            item.put(w);
        }
    }
}

impl<A: Put, B: Put> Put for (A, B) {
    fn put(&self, w: &mut Writer) {
        self.0.put(w);
        self.1.put(w);
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok((A::take(r)?, B::take(r)?))
    }
}

impl<A: Put, B: Put, C: Put> Put for (A, B, C) {
    fn put(&self, w: &mut Writer) {
        self.0.put(w);
        self.1.put(w);
        self.2.put(w);
    }
}

impl<A: Wire, B: Wire, C: Wire> Wire for (A, B, C) {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok((A::take(r)?, B::take(r)?, C::take(r)?))
    }
}

impl<A: Put, B: Put, C: Put, D: Put> Put for (A, B, C, D) {
    fn put(&self, w: &mut Writer) {
        self.0.put(w);
        self.1.put(w);
        self.2.put(w);
        self.3.put(w);
    }
}

impl<A: Wire, B: Wire, C: Wire, D: Wire> Wire for (A, B, C, D) {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok((A::take(r)?, B::take(r)?, C::take(r)?, D::take(r)?))
    }
}

impl<T: Put> Put for Bound<T> {
    fn put(&self, w: &mut Writer) {
        match self {
            Bound::Unbounded => w.byte(0),
            Bound::Included(value) => {
                w.byte(1);
                value.put(w);
            }
            Bound::Excluded(value) => {
                w.byte(2);
                value.put(w);
            }
        }
    }
}

impl<T: Wire> Wire for Bound<T> {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(Bound::Unbounded),
            1 => Ok(Bound::Included(T::take(r)?)),
            2 => Ok(Bound::Excluded(T::take(r)?)),
            _ => Err(Malformed),
        }
    }
}

impl Put for ChangeHash {
    fn put(&self, w: &mut Writer) {
        w.0.extend_from_slice(&self.0);
    }
}

impl Wire for ChangeHash {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        let mut hash = [0_u8; 32];
        for byte in &mut hash {
            *byte = r.byte()?;
        }
        Ok(ChangeHash(hash))
    }
}

impl Put for ActorId {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.to_bytes());
    }
}

impl Wire for ActorId {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok(ActorId::from(r.bytes()?))
    }
}

/// An object id as the host's document knows it: the actor's index is the
/// document's own, so the host takes it back without a lookup.
impl Put for ExId {
    fn put(&self, w: &mut Writer) {
        match self {
            ExId::Root => w.byte(0),
            ExId::Id(counter, actor, index) => {
                w.byte(1);
                w.uint(*counter);
                actor.put(w);
                w.uint(*index as u64);
            }
        }
    }
}

impl Wire for ExId {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(ExId::Root),
            1 => Ok(ExId::Id(r.uint()?, ActorId::take(r)?, usize::take(r)?)),
            _ => Err(Malformed),
        }
    }
}

impl Put for Prop {
    fn put(&self, w: &mut Writer) {
        match self {
            Prop::Map(key) => {
                w.byte(0);
                key.put(w);
            }
            Prop::Seq(index) => {
                w.byte(1);
                index.put(w);
            }
        }
    }
}

impl Wire for Prop {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(Prop::Map(String::take(r)?)),
            1 => Ok(Prop::Seq(usize::take(r)?)),
            _ => Err(Malformed),
        }
    }
}

impl Put for ObjType {
    fn put(&self, w: &mut Writer) {
        w.byte(match self {
            ObjType::Map => 0,
            ObjType::Table => 1,
            ObjType::List => 2,
            ObjType::Text => 3,
        });
    }
}

impl Wire for ObjType {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(ObjType::Map),
            1 => Ok(ObjType::Table),
            2 => Ok(ObjType::List),
            3 => Ok(ObjType::Text),
            _ => Err(Malformed),
        }
    }
}

impl Put for ScalarValue {
    fn put(&self, w: &mut Writer) {
        match self {
            ScalarValue::Bytes(bytes) => {
                w.byte(0);
                w.bytes(bytes);
            }
            ScalarValue::Str(text) => {
                w.byte(1);
                w.bytes(text.as_bytes());
            }
            ScalarValue::Int(value) => {
                w.byte(2);
                w.int(*value);
            }
            ScalarValue::Uint(value) => {
                w.byte(3);
                w.uint(*value);
            }
            ScalarValue::F64(value) => {
                w.byte(4);
                w.0.extend_from_slice(&value.to_le_bytes());
            }
            ScalarValue::Counter(counter) => {
                w.byte(5);
                w.int(i64::from(counter));
            }
            ScalarValue::Timestamp(value) => {
                w.byte(6);
                w.int(*value);
            }
            ScalarValue::Boolean(value) => {
                w.byte(7);
                value.put(w);
            }
            ScalarValue::Unknown { type_code, bytes } => {
                w.byte(8);
                w.byte(*type_code);
                w.bytes(bytes);
            }
            ScalarValue::Null => w.byte(9),
        }
    }
}

impl Wire for ScalarValue {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok(match r.byte()? {
            0 => ScalarValue::Bytes(r.bytes()?.to_vec()),
            1 => ScalarValue::Str(r.str()?.into()),
            2 => ScalarValue::Int(r.int()?),
            3 => ScalarValue::Uint(r.uint()?),
            4 => {
                let mut bytes = [0_u8; 8];
                for byte in &mut bytes {
                    *byte = r.byte()?;
                }
                ScalarValue::F64(f64::from_le_bytes(bytes))
            }
            5 => ScalarValue::counter(r.int()?),
            6 => ScalarValue::Timestamp(r.int()?),
            7 => ScalarValue::Boolean(bool::take(r)?),
            8 => ScalarValue::Unknown {
                type_code: r.byte()?,
                bytes: r.bytes()?.to_vec(),
            },
            9 => ScalarValue::Null,
            _ => return Err(Malformed),
        })
    }
}

impl Put for Value<'_> {
    fn put(&self, w: &mut Writer) {
        match self {
            Value::Object(kind) => {
                w.byte(0);
                kind.put(w);
            }
            Value::Scalar(scalar) => {
                w.byte(1);
                scalar.as_ref().put(w);
            }
        }
    }
}

impl Wire for Value<'_> {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(Value::Object(ObjType::take(r)?)),
            1 => Ok(Value::Scalar(Cow::Owned(ScalarValue::take(r)?))),
            _ => Err(Malformed),
        }
    }
}

impl Put for ExpandMark {
    fn put(&self, w: &mut Writer) {
        w.byte(match self {
            ExpandMark::Before => 0,
            ExpandMark::After => 1,
            ExpandMark::Both => 2,
            ExpandMark::None => 3,
        });
    }
}

impl Wire for ExpandMark {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(ExpandMark::Before),
            1 => Ok(ExpandMark::After),
            2 => Ok(ExpandMark::Both),
            3 => Ok(ExpandMark::None),
            _ => Err(Malformed),
        }
    }
}

impl Put for Mark {
    fn put(&self, w: &mut Writer) {
        self.start.put(w);
        self.end.put(w);
        self.name().put(w);
        self.value().put(w);
    }
}

impl Wire for Mark {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        let start = usize::take(r)?;
        let end = usize::take(r)?;
        let name = String::take(r)?;
        Ok(Mark::new(name, ScalarValue::take(r)?, start, end))
    }
}

impl Put for MarkSet {
    fn put(&self, w: &mut Writer) {
        let marks: Vec<(String, ScalarValue)> = self
            .iter()
            .map(|(name, value)| (name.to_owned(), value.clone()))
            .collect();
        marks.put(w);
    }
}

impl Wire for MarkSet {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok(Vec::<(String, ScalarValue)>::take(r)?.into_iter().collect())
    }
}

impl Put for TextEncoding {
    fn put(&self, w: &mut Writer) {
        w.byte(match self {
            TextEncoding::UnicodeCodePoint => 0,
            TextEncoding::Utf8CodeUnit => 1,
            TextEncoding::Utf16CodeUnit => 2,
            TextEncoding::GraphemeCluster => 3,
        });
    }
}

impl Wire for TextEncoding {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(TextEncoding::UnicodeCodePoint),
            1 => Ok(TextEncoding::Utf8CodeUnit),
            2 => Ok(TextEncoding::Utf16CodeUnit),
            3 => Ok(TextEncoding::GraphemeCluster),
            _ => Err(Malformed),
        }
    }
}

impl Put for Cursor {
    fn put(&self, w: &mut Writer) {
        w.bytes(&self.to_bytes());
    }
}

impl Wire for Cursor {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Cursor::try_from(r.bytes()?).map_err(|_| Malformed)
    }
}

impl Put for CursorPosition {
    fn put(&self, w: &mut Writer) {
        match self {
            CursorPosition::Start => w.byte(0),
            CursorPosition::End => w.byte(1),
            CursorPosition::Index(index) => {
                w.byte(2);
                index.put(w);
            }
        }
    }
}

impl Wire for CursorPosition {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(CursorPosition::Start),
            1 => Ok(CursorPosition::End),
            2 => Ok(CursorPosition::Index(usize::take(r)?)),
            _ => Err(Malformed),
        }
    }
}

impl Put for MoveCursor {
    fn put(&self, w: &mut Writer) {
        w.byte(match self {
            MoveCursor::Before => 0,
            MoveCursor::After => 1,
        });
    }
}

impl Wire for MoveCursor {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(MoveCursor::Before),
            1 => Ok(MoveCursor::After),
            _ => Err(Malformed),
        }
    }
}

/// A marks map as hydrated text and list items carry it.
fn put_marks(marks: &HashMap<String, ScalarValue>, w: &mut Writer) {
    let mut marks: Vec<(String, ScalarValue)> = marks
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    marks.sort_by(|a, b| a.0.cmp(&b.0));
    marks.put(w);
}

fn take_marks(r: &mut Reader<'_>) -> Result<HashMap<String, ScalarValue>, Malformed> {
    Ok(Vec::<(String, ScalarValue)>::take(r)?.into_iter().collect())
}

impl Put for hydrate::Value {
    fn put(&self, w: &mut Writer) {
        match self {
            hydrate::Value::Scalar(scalar) => {
                w.byte(0);
                scalar.put(w);
            }
            hydrate::Value::Map(map) => {
                w.byte(1);
                map.put(w);
            }
            hydrate::Value::List(list) => {
                w.byte(2);
                w.uint(list.len() as u64);
                for item in list.iter() {
                    item.value.put(w);
                    put_marks(&item.marks, w);
                    item.conflict.put(w);
                }
            }
            hydrate::Value::Text(text) => {
                w.byte(3);
                text.put(w);
            }
        }
    }
}

impl Wire for hydrate::Value {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok(match r.byte()? {
            0 => hydrate::Value::Scalar(ScalarValue::take(r)?),
            1 => hydrate::Value::Map(hydrate::Map::take(r)?),
            2 => {
                let length = usize::take(r)?;
                let mut items = Vec::new();
                for _ in 0..length {
                    let value = hydrate::Value::take(r)?;
                    let marks = take_marks(r)?;
                    let conflict = bool::take(r)?;
                    items.push(hydrate::ListValue {
                        value,
                        marks,
                        conflict,
                    });
                }
                hydrate::Value::List(hydrate::List::from_items(items))
            }
            3 => hydrate::Value::Text(hydrate::Text::take(r)?),
            _ => return Err(Malformed),
        })
    }
}

impl Put for hydrate::Map {
    fn put(&self, w: &mut Writer) {
        let mut entries: Vec<_> = self.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        w.uint(entries.len() as u64);
        for (key, entry) in entries {
            key.put(w);
            entry.value.put(w);
            entry.conflict.put(w);
        }
    }
}

impl Wire for hydrate::Map {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        let length = usize::take(r)?;
        let mut entries = HashMap::new();
        for _ in 0..length {
            let key = String::take(r)?;
            let value = hydrate::Value::take(r)?;
            let conflict = bool::take(r)?;
            entries.insert(key, hydrate::MapValue { value, conflict });
        }
        Ok(hydrate::Map::from_entries(entries))
    }
}

impl Put for hydrate::Text {
    fn put(&self, w: &mut Writer) {
        let (text, marks) = self.parts();
        text.put(w);
        put_marks(marks, w);
    }
}

impl Wire for hydrate::Text {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        let text = String::take(r)?;
        let marks = take_marks(r)?;
        Ok(hydrate::Text::from_parts(&text, marks))
    }
}

impl Put for Span {
    fn put(&self, w: &mut Writer) {
        match self {
            Span::Text { text, marks } => {
                w.byte(0);
                text.put(w);
                marks.as_deref().put(w);
            }
            Span::Block(block) => {
                w.byte(1);
                block.put(w);
            }
        }
    }
}

impl Wire for Span {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        match r.byte()? {
            0 => Ok(Span::Text {
                text: String::take(r)?,
                marks: <Option<MarkSet> as Wire>::take(r)?.map(std::sync::Arc::new),
            }),
            1 => Ok(Span::Block(hydrate::Map::take(r)?)),
            _ => Err(Malformed),
        }
    }
}

/// A change's metadata, as reading its entry asks for it. A module has no
/// authors.
impl Put for ChangeMetadata<'_> {
    fn put(&self, w: &mut Writer) {
        self.actor.as_ref().put(w);
        self.seq.put(w);
        self.start_op.put(w);
        self.max_op.put(w);
        self.timestamp.put(w);
        self.message.as_deref().put(w);
        self.deps.put(w);
        self.hash.put(w);
        w.bytes(&self.extra);
    }
}

impl Wire for ChangeMetadata<'_> {
    fn take(r: &mut Reader<'_>) -> Result<Self, Malformed> {
        Ok(ChangeMetadata {
            actor: Cow::Owned(ActorId::take(r)?),
            author: None,
            seq: r.take()?,
            start_op: r.take()?,
            max_op: r.take()?,
            timestamp: r.take()?,
            message: <Option<String> as Wire>::take(r)?.map(Cow::Owned),
            deps: r.take()?,
            hash: r.take()?,
            extra: Cow::Owned(r.bytes()?.to_vec()),
        })
    }
}

impl<T: Put + ?Sized> Put for &T {
    fn put(&self, w: &mut Writer) {
        (**self).put(w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round<T: Wire + PartialEq + std::fmt::Debug>(value: T) {
        let mut w = Writer::new();
        w.put(&value);
        let mut r = Reader::new(&w.0);
        assert_eq!(r.take::<T>().unwrap(), value);
        assert!(r.bytes.is_empty());
    }

    #[test]
    fn values_come_back_as_they_went() {
        round(ScalarValue::Str("é".into()));
        round(ScalarValue::Int(-5));
        round(ScalarValue::F64(1.5));
        round(ScalarValue::counter(-3));
        round(ExId::Id(7, ActorId::from(&[1, 2][..]), 3));
        round(Prop::Seq(300));
        round((Bound::Excluded("a".to_string()), Bound::<String>::Unbounded));
        round(Some(vec![1_u64, u64::MAX]));
        round(i64::MIN);
    }
}
