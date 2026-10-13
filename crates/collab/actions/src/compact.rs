//! The compact form a history entry takes in a change message.
//!
//! An entry is a tree of JSON values; written as JSON text it repeats every
//! key name, quotes every string and spells ids and times out in decimal and
//! hex. Here every value is a one-byte tag and its body:
//!
//! | tag | value |
//! |---|---|
//! | `n` `f` `t` | null, false, true |
//! | `u` *n* / `i` *n* | the integer *n* / −1 − *n* |
//! | `d` *len* *text* | any other number, as its JSON text |
//! | `s` *len* *bytes* | a string, UTF-8 |
//! | `k` *n* | the *n*th of [`WORDS`] (keys and values every entry uses) |
//! | `r` *n* | the *n*th string written with `s`, `U` or `H` before it |
//! | `U` 22 digits | a lowercase hyphenated UUID |
//! | `H` 22 digits | 32 lowercase hex digits (a gesture) |
//! | `[` *n* | an array of *n* values |
//! | `{` *n* | an object of *n* keys (string values) each followed by its value |
//!
//! Numbers *n* and lengths are written six bits a byte, low bits first: a
//! byte below `0x40` ends the number, `0x40 + bits` continues it. A digit is
//! `'0' + bits` (six bits). Every byte but those inside strings is below
//! `0x80`, so the whole is UTF-8 and travels as a change message does. Keys
//! keep their order; a value decodes to exactly the tree it was written
//! from.
//!
//! [`WORDS`] only grows: a word's index is part of every stored entry.

use serde_json::Value;

/// Strings entries use so often that each is written as two bytes. Append
/// only.
const WORDS: &[&str] = &[
    // The entry's own keys.
    "p",
    "r",
    "i",
    "w",
    "wp",
    "k",
    "pu",
    "gr",
    "f",
    "by",
    "s",
    "a",
    "u",
    "g",
    "c",
    // What it did.
    "did",
    "edit",
    "verb",
    "group",
    "history",
    "ty",
    "field",
    "path",
    "op",
    "old",
    "new",
    "args",
    "members",
    "count",
    "distinct",
    "item",
    "key",
    "cell",
    "action",
    "steps",
    "of",
    "set",
    "splice",
    "replace",
    "add",
    "insert",
    "remove",
    "move-to",
    "set-cell",
    "insert-row",
    "insert-column",
    "remove-row",
    "remove-column",
    // Who.
    "actor",
    "kind",
    "on_behalf_of",
    "participant",
    "id",
    "label",
    // The actions resources carry.
    "at",
    "ops",
    "field-edit",
    "value",
    "row",
    "column",
    "raw",
    "text",
    "index",
    // A bot run's cause.
    "cb",
    "installation",
    "event",
    "depth",
    // A bot's appearance.
    "appearance",
    "name",
    "avatar",
    "persona",
    "service",
    "account",
];

/// The message form of `value`.
#[must_use]
pub fn encode(value: &Value) -> String {
    let mut writer = Writer::default();
    writer.value(value);
    // Every byte written is below 0x80 but those copied from strings.
    String::from_utf8(writer.out).unwrap_or_default()
}

/// The JSON text of the value `text` was written from; `None` when it is
/// not one whole value.
#[must_use]
pub fn decode(text: &str) -> Option<String> {
    String::from_utf8(json_of(text)?).ok()
}

/// [`decode`] as bytes: what an entry's reader parses, straight from the
/// message without building a value tree first.
pub(crate) fn json_of(text: &str) -> Option<Vec<u8>> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        at: 0,
        strings: Vec::new(),
        out: Vec::with_capacity(text.len().saturating_mul(2)),
    };
    reader.value(0)?;
    (reader.at == reader.bytes.len()).then_some(reader.out)
}

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    strings: Vec<String>,
}

impl Writer {
    fn number(&mut self, mut n: u64) {
        while n >= 0x40 {
            self.out.push(0x40 | low_bits(n));
            n >>= 6;
        }
        self.out.push(low_bits(n));
    }

    fn value(&mut self, value: &Value) {
        match value {
            Value::Null => self.out.push(b'n'),
            Value::Bool(false) => self.out.push(b'f'),
            Value::Bool(true) => self.out.push(b't'),
            Value::Number(number) => {
                if let Some(n) = number.as_u64() {
                    self.out.push(b'u');
                    self.number(n);
                } else if let Some(n) = number.as_i64() {
                    self.out.push(b'i');
                    self.number(!n.cast_unsigned());
                } else {
                    let text = number.to_string();
                    self.out.push(b'd');
                    self.number(text.len() as u64);
                    self.out.extend_from_slice(text.as_bytes());
                }
            }
            Value::String(text) => self.string(text),
            Value::Array(items) => {
                self.out.push(b'[');
                self.number(items.len() as u64);
                for item in items {
                    self.value(item);
                }
            }
            Value::Object(fields) => {
                self.out.push(b'{');
                self.number(fields.len() as u64);
                for (key, value) in fields {
                    self.string(key);
                    self.value(value);
                }
            }
        }
    }

    fn string(&mut self, text: &str) {
        if let Some(word) = WORDS.iter().position(|word| *word == text) {
            self.out.push(b'k');
            self.number(word as u64);
            return;
        }
        if let Some(earlier) = self.strings.iter().position(|earlier| earlier == text) {
            self.out.push(b'r');
            self.number(earlier as u64);
            return;
        }
        if let Some(bits) = uuid_bits(text) {
            self.out.push(b'U');
            self.digits(bits);
        } else if let Some(bits) = hex_bits(text) {
            self.out.push(b'H');
            self.digits(bits);
        } else {
            self.out.push(b's');
            self.number(text.len() as u64);
            self.out.extend_from_slice(text.as_bytes());
        }
        self.strings.push(text.to_owned());
    }

    fn digits(&mut self, mut bits: u128) {
        for _ in 0..DIGITS {
            self.out.push(b'0'.wrapping_add(low_bits(bits)));
            bits >>= 6;
        }
    }
}

/// The low six bits of `n`.
fn low_bits(n: impl Into<u128>) -> u8 {
    u8::try_from(n.into() & 0x3f).unwrap_or_default()
}

/// How many six-bit digits hold 128 bits.
const DIGITS: usize = 22;

/// The bits of a lowercase hyphenated UUID, which it is written back as.
fn uuid_bits(text: &str) -> Option<u128> {
    let uuid = uuid::Uuid::try_parse(text).ok()?;
    (text.len() == 36 && uuid.hyphenated().to_string() == text).then(|| uuid.as_u128())
}

/// The bits of 32 lowercase hex digits.
fn hex_bits(text: &str) -> Option<u128> {
    let uuid = uuid::Uuid::try_parse(text).ok()?;
    (text.len() == 32 && uuid.simple().to_string() == text).then(|| uuid.as_u128())
}

/// Deepest nesting a message is read to.
const MAX_DEPTH: usize = 64;

/// Reads a message and writes the JSON text of its value.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    /// Where each string written with `s`, `U` or `H` sits in `out`, quoted.
    strings: Vec<(usize, usize)>,
    out: Vec<u8>,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at = self.at.checked_add(1)?;
        Some(byte)
    }

    fn number(&mut self) -> Option<u64> {
        let mut n = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.byte()?;
            if byte >= 0x80 || shift >= 64 {
                return None;
            }
            n |= u64::from(byte & 0x3f).checked_shl(shift)?;
            if byte < 0x40 {
                return Some(n);
            }
            shift = shift.checked_add(6)?;
        }
    }

    fn length(&mut self) -> Option<usize> {
        let n = usize::try_from(self.number()?).ok()?;
        (n <= self.bytes.len().checked_sub(self.at)?).then_some(n)
    }

    fn text(&mut self) -> Option<&str> {
        let len = self.length()?;
        let end = self.at.checked_add(len)?;
        let text = std::str::from_utf8(self.bytes.get(self.at..end)?).ok()?;
        self.at = end;
        Some(text)
    }

    fn digits(&mut self) -> Option<u128> {
        let mut bits = 0u128;
        for at in (0..DIGITS).map(|at| at.checked_mul(6)) {
            let digit = self
                .byte()?
                .checked_sub(b'0')
                .filter(|digit| *digit < 0x40)?;
            bits |= u128::from(digit).checked_shl(u32::try_from(at?).ok()?)?;
        }
        Some(bits)
    }

    fn decimal(&mut self, n: impl std::fmt::Display) {
        use std::io::Write as _;
        let _ = write!(self.out, "{n}");
    }

    /// Write the string tagged `tag` as a JSON string.
    fn string(&mut self, tag: u8) -> Option<()> {
        let start = self.out.len();
        match tag {
            b'k' => {
                let word = WORDS.get(usize::try_from(self.number()?).ok()?)?;
                self.out.push(b'"');
                self.out.extend_from_slice(word.as_bytes());
                self.out.push(b'"');
                return Some(());
            }
            b'r' => {
                let earlier = usize::try_from(self.number()?).ok()?;
                let (from, to) = *self.strings.get(earlier)?;
                self.out.extend_from_within(from..to);
                return Some(());
            }
            b'U' => {
                let mut buffer = uuid::Uuid::encode_buffer();
                let uuid = uuid::Uuid::from_u128(self.digits()?);
                let text = uuid.hyphenated().encode_lower(&mut buffer);
                self.out.push(b'"');
                self.out.extend_from_slice(text.as_bytes());
                self.out.push(b'"');
            }
            b'H' => {
                let mut buffer = uuid::Uuid::encode_buffer();
                let uuid = uuid::Uuid::from_u128(self.digits()?);
                let text = uuid.simple().encode_lower(&mut buffer);
                self.out.push(b'"');
                self.out.extend_from_slice(text.as_bytes());
                self.out.push(b'"');
            }
            b's' => {
                let len = self.length()?;
                let end = self.at.checked_add(len)?;
                let text = std::str::from_utf8(self.bytes.get(self.at..end)?).ok()?;
                self.at = end;
                quote(&mut self.out, text);
            }
            _ => return None,
        }
        self.strings.push((start, self.out.len()));
        Some(())
    }

    fn value(&mut self, depth: usize) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.byte()? {
            b'n' => self.out.extend_from_slice(b"null"),
            b'f' => self.out.extend_from_slice(b"false"),
            b't' => self.out.extend_from_slice(b"true"),
            b'u' => {
                let n = self.number()?;
                self.decimal(n);
            }
            b'i' => {
                let n = (!self.number()?).cast_signed();
                self.decimal(n);
            }
            b'd' => {
                let text = self.text()?;
                // Only a JSON number, so the text stays one value.
                let number: serde_json::Number = text.parse().ok()?;
                self.decimal(number);
            }
            b'[' => {
                let count = self.length()?;
                self.out.push(b'[');
                for index in 0..count {
                    if index > 0 {
                        self.out.push(b',');
                    }
                    self.value(depth.checked_add(1)?)?;
                }
                self.out.push(b']');
            }
            b'{' => {
                let count = self.length()?;
                self.out.push(b'{');
                for index in 0..count {
                    if index > 0 {
                        self.out.push(b',');
                    }
                    let tag = self.byte()?;
                    self.string(tag)?;
                    self.out.push(b':');
                    self.value(depth.checked_add(1)?)?;
                }
                self.out.push(b'}');
            }
            tag => self.string(tag)?,
        }
        Some(())
    }
}

/// Append `text` as a JSON string.
fn quote(out: &mut Vec<u8>, text: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let hex = |nibble: u8| HEX.get(usize::from(nibble)).copied().unwrap_or(b'0');
    let bytes = text.as_bytes();
    out.push(b'"');
    let mut plain = 0;
    for (at, &byte) in bytes.iter().enumerate() {
        let escape: &[u8] = match byte {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0..0x20 => b"",
            _ => continue,
        };
        out.extend_from_slice(bytes.get(plain..at).unwrap_or_default());
        if escape.is_empty() {
            out.extend_from_slice(b"\\u00");
            out.push(hex(byte >> 4));
            out.push(hex(byte & 0xf));
        } else {
            out.extend_from_slice(escape);
        }
        plain = at.saturating_add(1);
    }
    out.extend_from_slice(bytes.get(plain..).unwrap_or_default());
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_keys_are_known_words() {
        for word in [
            "appearance",
            "name",
            "avatar",
            "persona",
            "service",
            "account",
        ] {
            // A word reference: `k` and the word's index, never the text.
            let encoded = encode(&serde_json::json!(word));
            assert!(
                encoded.starts_with('k') && encoded.len() <= 3,
                "{word}: {encoded:?}"
            );
        }
    }

    fn round_trip(value: &Value) -> String {
        let text = encode(value);
        let json = decode(&text).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&json).ok().as_ref(),
            Some(value),
            "{text:?}"
        );
        text
    }

    #[test]
    fn every_kind_of_value_reads_back_as_written() {
        let value = serde_json::json!({
            "did": "edit",
            "n": [null, true, false, 0, 63, 64, 4095, u64::MAX, -1, i64::MIN, 1.5, -0.25, 1e300],
            "s": ["", "plain", "größe ✓", "with \"quotes\" and \n", "\u{1}\t\\ \u{1f}", "a".repeat(5000)],
            "id": "00000000-0000-4000-8000-000000000001",
            "upper": "00000000-0000-4000-8000-00000000000A",
            "g": "3637f0e9bb384e6baa8092a0224f4267",
            "nested": {"z": 1, "a": {"b": [[], {}]}},
            "again": ["plain", "00000000-0000-4000-8000-000000000001"],
        });
        let text = round_trip(&value);
        assert!(
            text.bytes().filter(|b| *b >= 0x80).count() < 20,
            "only string content leaves ASCII"
        );
    }

    #[test]
    fn repeated_strings_and_known_words_take_two_bytes() {
        let once = encode(&serde_json::json!(["sheets.workbook"]));
        let twice = encode(&serde_json::json!(["sheets.workbook", "sheets.workbook"]));
        assert_eq!(twice.len(), once.len() + 2);
        assert_eq!(encode(&serde_json::json!("field-edit")).len(), 2);
        assert_eq!(
            encode(&serde_json::json!("3637f0e9bb384e6baa8092a0224f4267")).len(),
            23
        );
    }

    #[test]
    fn what_is_not_one_whole_value_is_refused() {
        let text = encode(&serde_json::json!({"a": [1, "two"]}));
        for end in 0..text.len() {
            if text.is_char_boundary(end) {
                assert_eq!(decode(&text[..end]), None, "{end}");
            }
        }
        assert_eq!(decode(&format!("{text}n")), None);
        assert_eq!(decode("k\x68\x0f"), None, "no such word");
        assert_eq!(decode("r\x00"), None, "no earlier string");
        assert_eq!(decode("Z"), None);
        assert_eq!(decode(&"[\x01".repeat(100)), None, "nested too deep");
        assert_eq!(
            decode("s\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x03"),
            None
        );
    }
}
