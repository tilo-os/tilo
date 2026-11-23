//! Canonical compact JSON encoding for digests, signatures and content addresses.
//!
//! Object keys are sorted at every depth by the byte order of their UTF-8 text,
//! arrays keep their order, scalars and strings use `serde_json`'s compact
//! formatting, and no insignificant whitespace is emitted. That stays
//! deterministic even when `serde_json`'s `preserve_order` feature is enabled.
//!
//! Callers that feed a hash function can stream bytes straight into the hasher
//! with [`write_bytes`] or [`write_object_bytes`] through a fixed 8 KiB stack
//! buffer without allocating a cloned [`Value`] tree or a heap output buffer.

use std::fmt;

use serde_json::{Map, Value};

/// Objects up to this size sort in place on the stack on every target instead
/// of touching the heap.
const SMALL_OBJECT_LEN: usize = 8;

/// Fixed stack buffer size for [`write_bytes`] and [`write_object_bytes`].
/// Keeping flushes at 8 KiB lets streaming hashers like BLAKE3 stay on their
/// multi-chunk SIMD fast path without heap allocation.
const STREAM_BUF_LEN: usize = 8_192;
const _: () = assert!(STREAM_BUF_LEN.is_power_of_two());

static EMPTY_VALUE: Value = Value::Null;

/// Escapes for `0x00..=0x1f`, matching `serde_json::ser::CompactFormatter`.
const CONTROL_ESCAPES: [&str; 32] = [
    "\\u0000", "\\u0001", "\\u0002", "\\u0003", "\\u0004", "\\u0005", "\\u0006", "\\u0007", "\\b",
    "\\t", "\\n", "\\u000b", "\\f", "\\r", "\\u000e", "\\u000f", "\\u0010", "\\u0011", "\\u0012",
    "\\u0013", "\\u0014", "\\u0015", "\\u0016", "\\u0017", "\\u0018", "\\u0019", "\\u001a",
    "\\u001b", "\\u001c", "\\u001d", "\\u001e", "\\u001f",
];

/// Stream `value`'s canonical JSON bytes to `emit` in a single pass.
pub fn write_bytes(value: &Value, mut emit: impl FnMut(&[u8])) {
    let mut sink = FnSink::new(&mut emit);
    let mut scratch = Vec::new();
    write_value(&mut sink, value, &mut scratch);
    sink.flush();
}

/// Stream a JSON object's canonical JSON bytes to `emit` in a single pass,
/// without wrapping `object` in an owned [`Value::Object`].
pub fn write_object_bytes(object: &Map<String, Value>, mut emit: impl FnMut(&[u8])) {
    let mut sink = FnSink::new(&mut emit);
    let mut scratch = Vec::new();
    write_object(&mut sink, object, &mut scratch);
    sink.flush();
}

/// Append `value`'s canonical JSON bytes directly onto `out`.
pub fn append_vec(out: &mut Vec<u8>, value: &Value) {
    let mut sink = VecSink(out);
    let mut scratch = Vec::new();
    write_value(&mut sink, value, &mut scratch);
}

/// Encode `value` as canonical compact JSON bytes.
#[must_use]
pub fn to_vec(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    append_vec(&mut out, value);
    out
}

/// Encode `value` as a canonical compact JSON string.
#[must_use]
pub fn to_string(value: &Value) -> String {
    let mut out = String::new();
    let mut sink = StringSink(&mut out);
    let mut scratch = Vec::new();
    write_value(&mut sink, value, &mut scratch);
    out
}

trait Sink {
    fn write_str(&mut self, chunk: &str);
    fn write_ascii_byte(&mut self, byte: u8);
}

struct FnSink<'a> {
    emit: &'a mut dyn FnMut(&[u8]),
    buf: [u8; STREAM_BUF_LEN],
    len: usize,
}

impl<'a> FnSink<'a> {
    fn new(emit: &'a mut dyn FnMut(&[u8])) -> Self {
        Self {
            emit,
            buf: [0; STREAM_BUF_LEN],
            len: 0,
        }
    }

    #[inline]
    fn write_slice(&mut self, bytes: &[u8]) {
        let available = STREAM_BUF_LEN.saturating_sub(self.len);
        if bytes.len() <= available {
            let end = self.len.saturating_add(bytes.len());
            if let Some(dst) = self.buf.get_mut(self.len..end) {
                dst.copy_from_slice(bytes);
                self.len = end;
            }
            if self.len == STREAM_BUF_LEN {
                self.flush();
            }
            return;
        }
        self.write_slice_slow(bytes);
    }

    #[cold]
    fn write_slice_slow(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let available = STREAM_BUF_LEN.saturating_sub(self.len);
            if bytes.len() <= available {
                let end = self.len.saturating_add(bytes.len());
                if let Some(dst) = self.buf.get_mut(self.len..end) {
                    dst.copy_from_slice(bytes);
                    self.len = end;
                }
                if self.len == STREAM_BUF_LEN {
                    self.flush();
                }
                return;
            }
            if self.len == 0 {
                // Buffer is empty and input exceeds 8 KiB: emit full 8 KiB
                // blocks straight to `emit` without copying through `self.buf`.
                let full = bytes.len() & !STREAM_BUF_LEN.saturating_sub(1);
                if let Some((head, tail)) = bytes.split_at_checked(full) {
                    (self.emit)(head);
                    bytes = tail;
                } else {
                    return;
                }
            } else if let Some((head, tail)) = bytes.split_at_checked(available) {
                if let Some(dst) = self.buf.get_mut(self.len..) {
                    dst.copy_from_slice(head);
                    self.len = STREAM_BUF_LEN;
                }
                self.flush();
                bytes = tail;
            } else {
                return;
            }
        }
    }

    #[inline(never)]
    fn flush(&mut self) {
        if self.len > 0 {
            if let Some(filled) = self.buf.get(..self.len) {
                (self.emit)(filled);
            }
            self.len = 0;
        }
    }
}

impl Sink for FnSink<'_> {
    #[inline]
    fn write_str(&mut self, chunk: &str) {
        self.write_slice(chunk.as_bytes());
    }

    #[inline]
    fn write_ascii_byte(&mut self, byte: u8) {
        if let Some(slot) = self.buf.get_mut(self.len) {
            *slot = byte;
            self.len = self.len.saturating_add(1);
            if self.len == STREAM_BUF_LEN {
                self.flush();
            }
        }
    }
}

struct VecSink<'a>(&'a mut Vec<u8>);

impl Sink for VecSink<'_> {
    #[inline]
    fn write_str(&mut self, chunk: &str) {
        self.0.extend_from_slice(chunk.as_bytes());
    }

    #[inline]
    fn write_ascii_byte(&mut self, byte: u8) {
        self.0.push(byte);
    }
}

struct StringSink<'a>(&'a mut String);

impl Sink for StringSink<'_> {
    #[inline]
    fn write_str(&mut self, chunk: &str) {
        self.0.push_str(chunk);
    }

    #[inline]
    fn write_ascii_byte(&mut self, byte: u8) {
        self.0.push(char::from(byte));
    }
}

struct SinkFmt<'a, S: Sink>(&'a mut S);

impl<S: Sink> fmt::Write for SinkFmt<'_, S> {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_str(s);
        Ok(())
    }
}

fn write_value<'a, S: Sink>(
    sink: &mut S,
    value: &'a Value,
    scratch: &mut Vec<(&'a str, &'a Value)>,
) {
    match value {
        Value::Null => sink.write_str("null"),
        Value::Bool(true) => sink.write_str("true"),
        Value::Bool(false) => sink.write_str("false"),
        Value::Number(number) => {
            // `SinkFmt::write_str` is infallible and `Number::fmt` formats via
            // `serde_json`'s own compact integer/float formatters.
            let _ = fmt::Write::write_fmt(&mut SinkFmt(sink), format_args!("{number}"));
        }
        Value::String(text) => write_escaped_str(sink, text),
        Value::Array(items) => {
            sink.write_ascii_byte(b'[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    sink.write_ascii_byte(b',');
                }
                write_value(sink, item, scratch);
            }
            sink.write_ascii_byte(b']');
        }
        Value::Object(object) => write_object(sink, object, scratch),
    }
}

fn write_object<'a, S: Sink>(
    sink: &mut S,
    object: &'a Map<String, Value>,
    scratch: &mut Vec<(&'a str, &'a Value)>,
) {
    if object.is_empty() {
        sink.write_str("{}");
        return;
    }
    if object.len() == 1 || is_sorted_keys(object) {
        sink.write_ascii_byte(b'{');
        for (position, (key, item)) in object.iter().enumerate() {
            if position > 0 {
                sink.write_ascii_byte(b',');
            }
            write_escaped_str(sink, key);
            sink.write_ascii_byte(b':');
            write_value(sink, item, scratch);
        }
        sink.write_ascii_byte(b'}');
        return;
    }
    if object.len() <= SMALL_OBJECT_LEN {
        let mut stack: [(&str, &Value); SMALL_OBJECT_LEN] = [("", &EMPTY_VALUE); SMALL_OBJECT_LEN];
        let mut len = 0usize;
        for (key, item) in object {
            if let Some(slot) = stack.get_mut(len) {
                *slot = (key.as_str(), item);
                len = len.saturating_add(1);
            }
        }
        if let Some(entries) = stack.get_mut(..len) {
            insertion_sort_entries(entries);
            sink.write_ascii_byte(b'{');
            for (position, &(key, item)) in entries.iter().enumerate() {
                if position > 0 {
                    sink.write_ascii_byte(b',');
                }
                write_escaped_str(sink, key);
                sink.write_ascii_byte(b':');
                write_value(sink, item, scratch);
            }
            sink.write_ascii_byte(b'}');
        }
        return;
    }
    let base = scratch.len();
    scratch.extend(object.iter().map(|(key, item)| (key.as_str(), item)));
    let end = scratch.len();
    if let Some(entries) = scratch.get_mut(base..end) {
        sort_entries(entries);
    }
    sink.write_ascii_byte(b'{');
    for index in base..end {
        let Some(&(key, item)) = scratch.get(index) else {
            break;
        };
        if index > base {
            sink.write_ascii_byte(b',');
        }
        write_escaped_str(sink, key);
        sink.write_ascii_byte(b':');
        write_value(sink, item, scratch);
    }
    sink.write_ascii_byte(b'}');
    scratch.truncate(base);
}

fn is_sorted_keys(object: &Map<String, Value>) -> bool {
    let mut previous: Option<&[u8]> = None;
    for key in object.keys() {
        let bytes = key.as_bytes();
        if let Some(prev) = previous
            && prev > bytes
        {
            return false;
        }
        previous = Some(bytes);
    }
    true
}

/// In-place insertion sort for small stack-allocated entry slices, avoiding
/// heap allocations on both native and `wasm32` targets.
fn insertion_sort_entries(entries: &mut [(&str, &Value)]) {
    for end in 1..entries.len() {
        let mut cursor = end;
        while cursor > 0 {
            let prev = cursor.saturating_sub(1);
            let needs_swap = matches!(
                (entries.get(prev), entries.get(cursor)),
                (Some(left), Some(right)) if left.0.as_bytes() > right.0.as_bytes()
            );
            if !needs_swap {
                break;
            }
            entries.swap(prev, cursor);
            cursor = prev;
        }
    }
}

fn sort_entries(entries: &mut [(&str, &Value)]) {
    #[cfg(target_arch = "wasm32")]
    tilo_sort::CompactSort::compact_sort_by(entries, |left, right| {
        left.0.as_bytes().cmp(right.0.as_bytes())
    });
    #[cfg(not(target_arch = "wasm32"))]
    entries.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
}

/// Returns a bitmask with bit `8 * k + 7` set for the first lane `k` (in
/// little-endian byte order) containing a byte requiring JSON escaping
/// (`0x00..=0x1f`, `b'"'`, or `b'\\'`), or `0` when all eight bytes pass
/// through unchanged.
#[inline]
const fn escape_mask_word(word: u64) -> u64 {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    const CTRL: u64 = 0x2020_2020_2020_2020;
    const QUOTE: u64 = 0x2222_2222_2222_2222;
    const BACKSLASH: u64 = 0x5c5c_5c5c_5c5c_5c5c;

    let ctrl = word.wrapping_sub(CTRL) & !word;
    let quote_xor = word ^ QUOTE;
    let quote = quote_xor.wrapping_sub(LO) & !quote_xor;
    let backslash_xor = word ^ BACKSLASH;
    let backslash = backslash_xor.wrapping_sub(LO) & !backslash_xor;
    (ctrl | quote | backslash) & HI
}

#[inline]
fn find_escape(bytes: &[u8]) -> Option<usize> {
    let mut offset = 0usize;
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        if let Ok(array) = <[u8; 8]>::try_from(chunk) {
            let mask = escape_mask_word(u64::from_le_bytes(array));
            if mask != 0 {
                let lane = usize::try_from(mask.trailing_zeros() >> 3).unwrap_or(0);
                return Some(offset.saturating_add(lane));
            }
        }
        offset = offset.saturating_add(8);
    }
    for &byte in chunks.remainder() {
        if byte < 0x20 || byte == b'"' || byte == b'\\' {
            return Some(offset);
        }
        offset = offset.saturating_add(1);
    }
    None
}

fn write_escaped_str<S: Sink>(sink: &mut S, value: &str) {
    sink.write_ascii_byte(b'"');
    let mut rest = value;
    while let Some(pos) = find_escape(rest.as_bytes()) {
        if pos > 0
            && let Some(run) = rest.get(..pos)
        {
            sink.write_str(run);
        }
        let escaped = match rest.as_bytes().get(pos).copied() {
            Some(b'"') => "\\\"",
            Some(b'\\') => "\\\\",
            Some(byte) => CONTROL_ESCAPES
                .get(usize::from(byte))
                .copied()
                .unwrap_or(""),
            None => break,
        };
        sink.write_str(escaped);
        let next = pos.saturating_add(1);
        match rest.get(next..) {
            Some(tail) => rest = tail,
            None => {
                rest = "";
                break;
            }
        }
    }
    if !rest.is_empty() {
        sink.write_str(rest);
    }
    sink.write_ascii_byte(b'"');
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use super::{append_vec, is_sorted_keys, to_string, to_vec, write_bytes, write_object_bytes};

    const CORPUS: &[(&str, &str, &[u8])] = &[
        (
            "nested_unsorted",
            r#"{"z":{"b":2,"a":1},"a":{"d":{"y":true,"x":false},"c":[3,2,1]},"m":0}"#,
            br#"{"a":{"c":[3,2,1],"d":{"x":false,"y":true}},"m":0,"z":{"a":1,"b":2}}"#,
        ),
        (
            "utf8_vs_utf16_key_order",
            r#"{"\uD83D\uDE00":4,"\uE000":3,"é":2,"z":1}"#,
            b"{\"z\":1,\"\xc3\xa9\":2,\"\xee\x80\x80\":3,\"\xf0\x9f\x98\x80\":4}",
        ),
        (
            "non_bmp_vs_bmp_high_key_order",
            r#"{"\uD83D\uDE80":1,"\uFFFF":2,"\uE000":3}"#,
            b"{\"\xee\x80\x80\":3,\"\xef\xbf\xbf\":2,\"\xf0\x9f\x9a\x80\":1}",
        ),
        (
            "ascii_empty_prefix_key_order",
            r#"{"b":1,"":2,"a":3,"ab":4,"B":5,"A":6}"#,
            br#"{"":2,"A":6,"B":5,"a":3,"ab":4,"b":1}"#,
        ),
        (
            "nested_ten_keys",
            r#"{"k9":9,"k8":8,"k7":{"j9":9,"j8":8,"j7":7,"j6":6,"j5":5,"j4":4,"j3":3,"j2":2,"j1":1,"j0":0},"k6":6,"k5":5,"k4":4,"k3":3,"k2":2,"k1":1,"k0":0}"#,
            br#"{"k0":0,"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":{"j0":0,"j1":1,"j2":2,"j3":3,"j4":4,"j5":5,"j6":6,"j7":7,"j8":8,"j9":9},"k8":8,"k9":9}"#,
        ),
        ("empty_object", "{}", b"{}"),
        ("empty_array", "[]", b"[]"),
        ("empty_string", r#""""#, br#""""#),
        (
            "line_seps_fffd_and_solidus",
            r#""\u2028\u2029\ufffd/""#,
            b"\"\xe2\x80\xa8\xe2\x80\xa9\xef\xbf\xbd/\"",
        ),
        (
            "all_32_control_bytes_value",
            r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
            br#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
        ),
        (
            "all_32_control_bytes_key",
            r#"{"a":2,"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1}"#,
            br#"{"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1,"a":2}"#,
        ),
        (
            "escaped_and_unicode_string",
            r#""q\"b\\c\u0000\u0001\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u001f\u007f café 日本語 🚀""#,
            b"\"q\\\"b\\\\c\\u0000\\u0001\\u0007\\b\\t\\n\\u000b\\f\\r\\u000e\\u001f\x7f caf\xc3\xa9 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e \xf0\x9f\x9a\x80\"",
        ),
        (
            "escaped_keys_object",
            r#"{"q\"b\\c\u0000\b\t\n\f\r\u001f\u007f":2,"a":1}"#,
            b"{\"a\":1,\"q\\\"b\\\\c\\u0000\\b\\t\\n\\f\\r\\u001f\x7f\":2}",
        ),
        ("i64_min", "-9223372036854775808", b"-9223372036854775808"),
        ("i64_max", "9223372036854775807", b"9223372036854775807"),
        (
            "u64_above_i64_max",
            "9223372036854775808",
            b"9223372036854775808",
        ),
        ("u64_max", "18446744073709551615", b"18446744073709551615"),
        ("zero", "0", b"0"),
        ("negative_zero", "-0.0", b"-0.0"),
        ("float_whole", "1.0", b"1.0"),
        ("float_tenth", "0.1", b"0.1"),
        ("float_1e20", "1e20", b"1e+20"),
        ("float_1e21", "1e21", b"1e+21"),
        ("float_1e_neg5", "1e-5", b"0.00001"),
        ("float_1e_neg7", "1e-7", b"1e-7"),
        ("float_min_subnormal", "5e-324", b"5e-324"),
        (
            "float_max_finite",
            "1.7976931348623157e308",
            b"1.7976931348623157e+308",
        ),
        ("float_1e15", "1e15", b"1000000000000000.0"),
        ("float_0_3_ulp", "0.30000000000000004", b"0.30000000000000004"),
        ("bool_true", "true", b"true"),
        ("bool_false", "false", b"false"),
        ("null", "null", b"null"),
        (
            "mixed_array",
            r#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"b":2,"a":1}]"#,
            br#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"a":1,"b":2}]"#,
        ),
    ];

    #[test]
    fn encodes_shared_corpus_across_all_entry_points() {
        for &(name, input, expected) in CORPUS {
            let value: Value = serde_json::from_str(input).unwrap();
            assert_eq!(to_vec(&value), expected, "to_vec row {name}");
            assert_eq!(
                to_string(&value).as_bytes(),
                expected,
                "to_string row {name}"
            );

            let mut streamed = Vec::new();
            write_bytes(&value, |chunk| streamed.extend_from_slice(chunk));
            assert_eq!(streamed, expected, "write_bytes row {name}");

            let mut prefixed = b"prefix:".to_vec();
            append_vec(&mut prefixed, &value);
            assert_eq!(
                prefixed,
                [b"prefix:".as_slice(), expected].concat(),
                "append_vec row {name}"
            );

            if let Value::Object(map) = &value {
                let mut obj_streamed = Vec::new();
                write_object_bytes(map, |chunk| obj_streamed.extend_from_slice(chunk));
                assert_eq!(obj_streamed, expected, "write_object_bytes row {name}");
            }
        }
    }

    #[test]
    fn string_escaping_matches_serde_json_for_all_ascii_bytes_and_unicode() {
        for byte in 0u8..=0x7f {
            let text = String::from(char::from(byte));
            let value = Value::String(text);
            assert_eq!(
                to_string(&value),
                serde_json::to_string(&value).unwrap(),
                "ascii byte 0x{byte:02x}"
            );
        }
        let mixed = Value::String("héllo\x00world\x1f\x7f\"\\\u{e000}\u{1f680}".to_owned());
        assert_eq!(to_string(&mixed), serde_json::to_string(&mixed).unwrap());
    }

    #[test]
    fn sorts_small_unsorted_objects_up_to_eight_keys() {
        // Exercises the <= 8-key stack sort path with insertion order preserved.
        let eight: Value =
            serde_json::from_str(r#"{"h":8,"c":3,"g":7,"a":1,"f":6,"b":2,"e":5,"d":4}"#).unwrap();
        let map = eight.as_object().unwrap();
        assert_eq!(map.len(), 8);
        assert!(
            !is_sorted_keys(map),
            "preserve_order must keep the 8-key map unsorted so the stack sort runs"
        );
        let expected = r#"{"a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7,"h":8}"#;
        assert_eq!(to_string(&eight), expected);
        let mut streamed = Vec::new();
        write_object_bytes(map, |chunk| streamed.extend_from_slice(chunk));
        assert_eq!(streamed, expected.as_bytes());

        let six: Value = serde_json::from_str(r#"{"b":1,"":2,"a":3,"ab":4,"B":5,"A":6}"#).unwrap();
        assert!(!is_sorted_keys(six.as_object().unwrap()));
        assert_eq!(to_string(&six), r#"{"":2,"A":6,"B":5,"a":3,"ab":4,"b":1}"#);

        let reversed_eight: Value =
            serde_json::from_str(r#"{"h":8,"g":7,"f":6,"e":5,"d":4,"c":3,"b":2,"a":1}"#).unwrap();
        assert!(!is_sorted_keys(reversed_eight.as_object().unwrap()));
        assert_eq!(to_string(&reversed_eight), expected);
    }

    #[test]
    fn sorts_large_unsorted_objects_over_eight_keys() {
        // Exercises the > 8-key scratch sort path with insertion order preserved.
        let mut map = Map::new();
        for key in ["m", "b", "k", "a", "j", "c", "i", "d", "h", "e", "g", "f"] {
            map.insert(key.to_owned(), Value::from(key));
        }
        assert_eq!(map.len(), 12);
        assert!(
            !is_sorted_keys(&map),
            "preserve_order must keep the 12-key map unsorted so the scratch sort runs"
        );
        let expected = r#"{"a":"a","b":"b","c":"c","d":"d","e":"e","f":"f","g":"g","h":"h","i":"i","j":"j","k":"k","m":"m"}"#;
        let value = Value::Object(map.clone());
        assert_eq!(to_string(&value), expected);
        let mut streamed = Vec::new();
        write_object_bytes(&map, |chunk| streamed.extend_from_slice(chunk));
        assert_eq!(streamed, expected.as_bytes());
    }

    #[test]
    fn sorts_nested_ten_key_objects_sharing_scratch() {
        // Exercises recursive scratch slicing (base..end and truncate) with ten keys
        // k9 down to k0 where k7 holds ten keys j9 down to j0.
        let input = r#"{"k9":9,"k8":8,"k7":{"j9":9,"j8":8,"j7":7,"j6":6,"j5":5,"j4":4,"j3":3,"j2":2,"j1":1,"j0":0},"k6":6,"k5":5,"k4":4,"k3":3,"k2":2,"k1":1,"k0":0}"#;
        let expected = r#"{"k0":0,"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":{"j0":0,"j1":1,"j2":2,"j3":3,"j4":4,"j5":5,"j6":6,"j7":7,"j8":8,"j9":9},"k8":8,"k9":9}"#;
        let value: Value = serde_json::from_str(input).unwrap();
        let outer = value.as_object().unwrap();
        let inner = outer.get("k7").unwrap().as_object().unwrap();
        assert!(!is_sorted_keys(outer));
        assert!(!is_sorted_keys(inner));
        assert_eq!(to_string(&value), expected);
        let mut streamed = Vec::new();
        write_object_bytes(outer, |chunk| streamed.extend_from_slice(chunk));
        assert_eq!(streamed, expected.as_bytes());
    }

    #[test]
    fn handles_sorted_small_and_large_nested_objects() {
        // With `serde_json/preserve_order` enabled in dev-dependencies, `outer` and each
        // `inner` map stay in reverse insertion order so this checks both the unsorted
        // > 8-key scratch-reuse path and the already-sorted / single-key fast paths.
        let mut outer = Map::new();
        for i in (0u32..12).rev() {
            let mut inner = Map::new();
            for j in (0u32..10).rev() {
                inner.insert(format!("k{j:02}"), Value::from(j));
            }
            outer.insert(format!("o{i:02}"), Value::Object(inner));
        }
        assert!(!is_sorted_keys(&outer));
        let encoded = to_string(&Value::Object(outer));

        let mut expected_outer = Map::new();
        for i in 0u32..12 {
            let mut inner = Map::new();
            for j in 0u32..10 {
                inner.insert(format!("k{j:02}"), Value::from(j));
            }
            expected_outer.insert(format!("o{i:02}"), Value::Object(inner));
        }
        assert!(is_sorted_keys(&expected_outer));
        let expected_value = Value::Object(expected_outer);
        let expected = serde_json::to_string(&expected_value).unwrap();
        assert_eq!(encoded, expected);
        // Already-sorted fast path must produce identical bytes.
        assert_eq!(to_string(&expected_value), expected);

        // Single-key object fast path wrapping an unsorted 2-key child.
        let single: Value = serde_json::from_str(r#"{"only":{"b":2,"a":1}}"#).unwrap();
        assert!(!is_sorted_keys(
            single
                .as_object()
                .unwrap()
                .get("only")
                .unwrap()
                .as_object()
                .unwrap()
        ));
        assert_eq!(to_string(&single), r#"{"only":{"a":1,"b":2}}"#);
    }

    #[test]
    fn streaming_sink_batches_8kib_chunks_across_boundaries() {
        // 64 KiB of 80-byte newline-terminated lines, matching the Order 3 workload.
        let line = format!("{}\n", "a".repeat(79));
        let mut text = String::with_capacity(65_536);
        for _ in 0..819 {
            text.push_str(&line);
        }
        text.push_str(&"b".repeat(15));
        text.push('\n');
        assert_eq!(text.len(), 65_536);

        let doc = serde_json::json!({ "text": text });
        let expected = to_vec(&doc);
        let mut streamed = Vec::with_capacity(expected.len());
        let mut chunk_lens = Vec::new();
        write_bytes(&doc, |chunk| {
            assert!(
                !chunk.is_empty(),
                "buffered sink must never emit empty chunks"
            );
            chunk_lens.push(chunk.len());
            streamed.extend_from_slice(chunk);
        });
        assert_eq!(streamed, expected);
        assert!(chunk_lens.len() >= 8);
        for &len in &chunk_lens[..chunk_lens.len() - 1] {
            assert!(
                len >= super::STREAM_BUF_LEN,
                "non-final flush was {len} bytes, expected >= {}",
                super::STREAM_BUF_LEN
            );
        }

        // Exact 8 KiB boundary (8191, 8192, 8193) and a 20 KiB unescaped string
        // crossing direct-emit and buffered-tail paths.
        for target_total in [8_191usize, 8_192, 8_193, 20_480] {
            let payload_len = target_total - 2; // surrounding quotes
            let value = Value::String("x".repeat(payload_len));
            let expected = to_vec(&value);
            assert_eq!(expected.len(), target_total);
            let mut out = Vec::with_capacity(target_total);
            let mut sizes = Vec::new();
            write_bytes(&value, |chunk| {
                assert!(!chunk.is_empty());
                sizes.push(chunk.len());
                out.extend_from_slice(chunk);
            });
            assert_eq!(out, expected, "total {target_total}");
            if target_total == 8_192 {
                assert_eq!(sizes, vec![8_192]);
            }
        }
    }

    #[test]
    fn swar_find_escape_matches_naive_scan_for_every_byte_and_lane() {
        let needs_escape = |b: u8| b < 0x20 || b == b'"' || b == b'\\';
        for len in 1..=16 {
            for pos in 0..len {
                for byte in 0u8..=0xff {
                    let mut buf = vec![b'a'; len];
                    buf[pos] = byte;
                    let expected = needs_escape(byte).then_some(pos);
                    assert_eq!(
                        super::find_escape(&buf),
                        expected,
                        "len={len} pos={pos} byte=0x{byte:02x}"
                    );
                }
            }
        }
        // High-bit bytes mixed with every safe ASCII byte never trigger false positives.
        let all_safe: Vec<u8> = (0u8..=0xff).filter(|&b| !needs_escape(b)).collect();
        assert_eq!(super::find_escape(&all_safe), None);
    }
}
