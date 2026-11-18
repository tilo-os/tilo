//! The compact form a request reaches a model module in.
//!
//! The device binds an action from its JSON arguments and hands the module
//! the result in this form: the same values as JSON (null, booleans, numbers,
//! text, lists, maps), each a tag byte and its content, so a module reads a
//! typed action through serde without carrying a JSON parser. The device
//! writes it from a JSON value ([`encode`]) with no knowledge of the app's
//! types; the module reads it into them ([`Reader`]), exactly as it would
//! have read the JSON.

use serde::de::{self, DeserializeSeed, IntoDeserializer as _, Visitor};

const NULL: u8 = 0;
const FALSE: u8 = 1;
const TRUE: u8 = 2;
/// A non-negative integer, as LEB128.
const UNSIGNED: u8 = 3;
/// A negative integer `n`, as the LEB128 of `!n`.
const NEGATIVE: u8 = 4;
/// Any other number, as the eight little-endian bytes of an `f64`.
const FLOAT: u8 = 5;
/// UTF-8 text: its length, then its bytes.
const TEXT: u8 = 6;
/// A list: how many values, then each.
const LIST: u8 = 7;
/// A map: how many entries, then each key (as text) and its value.
const MAP: u8 = 8;

/// Append `value` in the compact form.
pub fn encode(value: &serde_json::Value, out: &mut Vec<u8>) {
    match value {
        serde_json::Value::Null => out.push(NULL),
        serde_json::Value::Bool(false) => out.push(FALSE),
        serde_json::Value::Bool(true) => out.push(TRUE),
        serde_json::Value::Number(number) => {
            if let Some(unsigned) = number.as_u64() {
                out.push(UNSIGNED);
                length(unsigned, out);
            } else if let Some(negative) = number.as_i64() {
                out.push(NEGATIVE);
                length((!negative).cast_unsigned(), out);
            } else {
                out.push(FLOAT);
                out.extend_from_slice(&number.as_f64().unwrap_or_default().to_le_bytes());
            }
        }
        serde_json::Value::String(text) => {
            out.push(TEXT);
            self::text(text, out);
        }
        serde_json::Value::Array(values) => {
            out.push(LIST);
            length(values.len() as u64, out);
            for value in values {
                encode(value, out);
            }
        }
        serde_json::Value::Object(entries) => {
            out.push(MAP);
            length(entries.len() as u64, out);
            for (key, value) in entries {
                text(key, out);
                encode(value, out);
            }
        }
    }
}

/// Append `text`: its length, then its bytes.
pub fn text(text: &str, out: &mut Vec<u8>) {
    length(text.len() as u64, out);
    out.extend_from_slice(text.as_bytes());
}

/// Append `value` as LEB128.
pub fn length(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let low = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

/// Why a value did not read as the type asked for.
#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl de::Error for Error {
    fn custom<T: std::fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

/// Values read one after another from compact bytes.
pub struct Reader<'de> {
    bytes: &'de [u8],
}

impl<'de> Reader<'de> {
    /// A reader at the start of `bytes`.
    #[must_use]
    pub const fn new(bytes: &'de [u8]) -> Self {
        Self { bytes }
    }

    /// The next byte, taken.
    ///
    /// # Errors
    /// When the bytes end.
    pub fn byte(&mut self) -> Result<u8, Error> {
        let (&first, rest) = self
            .bytes
            .split_first()
            .ok_or_else(|| Error("the request ends early".into()))?;
        self.bytes = rest;
        Ok(first)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.first().copied()
    }

    /// The next LEB128 number, taken.
    ///
    /// # Errors
    /// When the bytes end inside it.
    pub fn length(&mut self) -> Result<u64, Error> {
        let mut value = 0_u64;
        let mut shift = 0_u32;
        loop {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f).checked_shl(shift).unwrap_or(0);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift = shift.saturating_add(7);
        }
    }

    /// The next text without its tag, taken.
    ///
    /// # Errors
    /// When the bytes end inside it or it is not UTF-8.
    pub fn text(&mut self) -> Result<&'de str, Error> {
        let length = usize::try_from(self.length()?).unwrap_or(usize::MAX);
        if length > self.bytes.len() {
            return Err(Error("the request ends inside a text".into()));
        }
        let (text, rest) = self.bytes.split_at(length);
        self.bytes = rest;
        std::str::from_utf8(text).map_err(|_| Error("a text is not UTF-8".into()))
    }

    /// The next value, read as `T`.
    ///
    /// # Errors
    /// Why it is not a `T`.
    pub fn read<T: de::Deserialize<'de>>(&mut self) -> Result<T, Error> {
        T::deserialize(self)
    }

    fn count(&mut self) -> Result<usize, Error> {
        Ok(usize::try_from(self.length()?).unwrap_or(usize::MAX))
    }

    /// Step over the next value.
    fn skip(&mut self) -> Result<(), Error> {
        match self.byte()? {
            NULL | FALSE | TRUE => {}
            UNSIGNED | NEGATIVE => {
                self.length()?;
            }
            FLOAT => {
                for _ in 0..8 {
                    self.byte()?;
                }
            }
            TEXT => {
                self.text()?;
            }
            LIST => {
                for _ in 0..self.count()? {
                    self.skip()?;
                }
            }
            MAP => {
                for _ in 0..self.count()? {
                    self.text()?;
                    self.skip()?;
                }
            }
            tag => return Err(Error(format!("no value has the tag {tag}"))),
        }
        Ok(())
    }

    /// The next value's head: a whole scalar, or how many items a list or a
    /// map holds.
    #[inline(never)]
    fn token(&mut self) -> Result<Token<'de>, Error> {
        Ok(match self.byte()? {
            NULL => Token::Unit,
            FALSE => Token::Bool(false),
            TRUE => Token::Bool(true),
            UNSIGNED => Token::Unsigned(self.length()?),
            NEGATIVE => Token::Negative(!self.length()?.cast_signed()),
            FLOAT => {
                let mut bytes = [0_u8; 8];
                for byte in &mut bytes {
                    *byte = self.byte()?;
                }
                Token::Float(f64::from_le_bytes(bytes))
            }
            TEXT => Token::Text(self.text()?),
            LIST => Token::List(self.count()?),
            MAP => Token::Map(self.count()?),
            tag => return Err(Error(format!("no value has the tag {tag}"))),
        })
    }
}

/// The head of one value.
enum Token<'de> {
    Unit,
    Bool(bool),
    Unsigned(u64),
    Negative(i64),
    Float(f64),
    Text(&'de str),
    List(usize),
    Map(usize),
}

impl<'de> de::Deserializer<'de> for &mut Reader<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        // What is read is decided once, outside the code each type's visitor
        // instantiates, which only hands the token on.
        match self.token()? {
            Token::Unit => visitor.visit_unit(),
            Token::Bool(value) => visitor.visit_bool(value),
            Token::Unsigned(value) => visitor.visit_u64(value),
            Token::Negative(value) => visitor.visit_i64(value),
            Token::Float(value) => visitor.visit_f64(value),
            Token::Text(text) => visitor.visit_borrowed_str(text),
            Token::List(left) => visitor.visit_seq(Items { reader: self, left }),
            Token::Map(left) => visitor.visit_map(Items { reader: self, left }),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.peek() == Some(NULL) {
            self.byte()?;
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.byte()? {
            TEXT => visitor.visit_enum(self.text()?.into_deserializer()),
            MAP if self.count()? == 1 => visitor.visit_enum(self),
            _ => Err(Error("an enum is a text or a map of one entry".into())),
        }
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.skip()?;
        visitor.visit_unit()
    }

    fn is_human_readable(&self) -> bool {
        true
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier
    }
}

/// The entries of a list or a map still to read.
struct Items<'a, 'de> {
    reader: &'a mut Reader<'de>,
    left: usize,
}

impl<'de> de::SeqAccess<'de> for Items<'_, 'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left = self.left.saturating_sub(1);
        seed.deserialize(&mut *self.reader).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.left)
    }
}

impl<'de> de::MapAccess<'de> for Items<'_, 'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left = self.left.saturating_sub(1);
        let key = self.reader.text()?;
        seed.deserialize(de::value::BorrowedStrDeserializer::new(key))
            .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        seed.deserialize(&mut *self.reader)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.left)
    }
}

impl<'de> de::EnumAccess<'de> for &mut Reader<'de> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self), Error> {
        let name = self.text()?;
        let variant = seed.deserialize(de::value::BorrowedStrDeserializer::<Error>::new(name))?;
        Ok((variant, self))
    }
}

impl<'de> de::VariantAccess<'de> for &mut Reader<'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        self.skip()
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self)
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_any(self, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_any(self, visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(tag = "action", content = "args", rename_all = "kebab-case")]
    enum Verb {
        CreateArea { name: String, color: Option<String> },
        Toggle(Toggle),
        Clear,
    }

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Toggle {
        id: u64,
        by: i32,
        weight: f64,
        tags: Vec<Tag>,
    }

    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum Tag {
        Plain,
        Named(String),
        Pair { left: bool, right: Option<bool> },
    }

    fn read<T: de::DeserializeOwned>(value: &serde_json::Value) -> T {
        let mut bytes = Vec::new();
        encode(value, &mut bytes);
        Reader::new(&bytes).read().expect("reads")
    }

    #[test]
    fn a_value_reads_as_its_json_reads() {
        for value in [
            serde_json::json!({"action": "create-area", "args": {"name": "Ship it", "extra": [1, {"a": null}]}}),
            serde_json::json!({"action": "toggle", "args": {"id": 3, "by": -7, "weight": 1, "tags": ["Plain", {"Named": "x"}, {"Pair": {"left": true, "right": null}}]}}),
            serde_json::json!({"action": "clear"}),
        ] {
            let expected: Verb = serde_json::from_value(value.clone()).expect("json reads");
            assert_eq!(read::<Verb>(&value), expected);
        }
        let any = serde_json::json!({"n": -1, "f": 2.5e300, "t": "é", "l": [true, false, null]});
        assert_eq!(read::<serde_json::Value>(&any), any);
    }
}
