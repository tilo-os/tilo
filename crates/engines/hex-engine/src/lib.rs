//! The device's work for the hex editor: a file read as bytes, the edits a person typed applied
//! to them, a pattern found, and the bytes around the cursor read as the numbers they can be.
//!
//! An app runs sandboxed and can neither read a disk nor decode a number's bytes itself, so this
//! crate is what it reaches through `engine.call`. It registers as the engine of
//! `so.tilo.hex-editor` and answers that app alone; every answer is JSON the package reads into
//! its own records.
//!
//! The bytes a document stands on live in the workspace's content store, at the address
//! [`open`](HexEngine::call) answers, so every device reads the same file; the edits a person made
//! since are the document's own records, and this engine applies them to the base on each ask.
//!
//! Answers now (`engine.call`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `open` | `{path}` | `{name, address, length}` |
//! | `rows` | `{address, changes, first, count, columns, endian, cursor}` | `{length, edited, read, error, rows, findings}` |
//! | `find` | `{address, changes, query, kind, case, word, limit}` | `{hits, truncated}` |
//! | `apply` | `{address, changes, op, at, len, bytes}` | `{change, length}` |
//! | `replace` | `{address, changes, spans, bytes, mode}` | `{changes, count, length}` |
//! | `save` | `{address, changes, path}` | `{address, length}` |

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::json;

/// The app this engine answers.
pub const APP: &str = "so.tilo.hex-editor";

/// The most a file may hold to be opened: the buffer is held whole by the device and by every
/// content store the address reaches.
pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// The most matches one search keeps: a binary file can hold millions of one-byte hits.
pub const MAX_MATCHES: usize = 10_000;

/// The engine of the hex editor, as the session keeps it for the app.
#[derive(Default)]
pub struct HexEngine {
    /// The bytes of each open base as they stand with the edits applied, by address, so a
    /// keystroke touches only what it changed.
    buffers: Mutex<BTreeMap<String, Buffer>>,
}

/// One base file's bytes, and the edits already applied to them.
struct Buffer {
    base: Vec<u8>,
    bytes: Vec<u8>,
    applied: Vec<Change>,
}

/// One edit of the bytes: `remove` bytes at `at` replaced by `insert`, written as hexadecimal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub at: usize,
    pub remove: usize,
    /// The bytes spliced in, two hexadecimal digits each, no separators.
    pub insert: String,
}

impl Change {
    /// The change as records keep it: `at:remove:insert`.
    fn text(&self) -> String {
        format!("{}:{}:{}", self.at, self.remove, self.insert)
    }

    /// The change `text` writes, and nothing when it is none.
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.splitn(3, ':');
        let at = parts.next()?.parse().ok()?;
        let remove = parts.next()?.parse().ok()?;
        let insert = parts.next()?.to_owned();
        Some(Self { at, remove, insert })
    }

    fn inserted(&self) -> Vec<u8> {
        parse_hex(&self.insert).unwrap_or_default()
    }

    fn apply_to(&self, bytes: &mut Vec<u8>) {
        let end = (self.at.saturating_add(self.remove)).min(bytes.len());
        let start = self.at.min(end);
        bytes.splice(start..end, self.inserted());
    }
}

impl HexEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn buffers(&self) -> MutexGuard<'_, BTreeMap<String, Buffer>> {
        self.buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The bytes `address` holds with `changes` applied, read afresh only when the edits made
    /// since are not the ones already applied. Beside the bytes: how many of them differ from
    /// the base, and whether the base's bytes are on this device.
    fn derive(&self, address: &str, changes: &[String]) -> (Vec<u8>, usize, bool) {
        let changes: Vec<Change> = changes
            .iter()
            .filter_map(|text| Change::parse(text))
            .collect();
        let mut buffers = self.buffers();
        if let Some(buffer) = buffers.get_mut(address)
            && changes.starts_with(&buffer.applied)
        {
            for change in changes.get(buffer.applied.len()..).unwrap_or_default() {
                change.apply_to(&mut buffer.bytes);
                buffer.applied.push(change.clone());
            }
            let edited = differing(&buffer.base, &buffer.bytes);
            return (buffer.bytes.clone(), edited, true);
        }
        // A buffer with no file yet stands on no bytes, and reads as one.
        let base = if address.is_empty() {
            Vec::new()
        } else {
            let Some(base) = read_address(address) else {
                return (Vec::new(), 0, false);
            };
            base
        };
        let mut bytes = base.clone();
        for change in &changes {
            change.apply_to(&mut bytes);
        }
        let edited = differing(&base, &bytes);
        // A handful of documents are held at once: each is a file's whole bytes twice.
        if buffers.len() >= 3 {
            buffers.clear();
        }
        buffers.insert(
            address.to_owned(),
            Buffer {
                base,
                bytes: bytes.clone(),
                applied: changes.clone(),
            },
        );
        drop(buffers);
        (bytes, edited, true)
    }

    fn op_open(input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            path: String,
        }
        let ask: Ask = read_ask(input)?;
        let path = Path::new(&ask.path);
        let meta = std::fs::metadata(path).map_err(|error| error.to_string())?;
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!(
                "it is larger than {} MiB",
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(format!(
                "it is larger than {} MiB",
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        let address = keep(&bytes)?;
        let name = path.file_name().map_or_else(
            || ask.path.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        Ok(json!({ "name": name, "address": address, "length": bytes.len() }).to_string())
    }

    fn op_rows(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            address: String,
            #[serde(default)]
            changes: Vec<String>,
            #[serde(default)]
            first: usize,
            #[serde(default)]
            count: usize,
            #[serde(default)]
            columns: usize,
            #[serde(default)]
            endian: String,
            #[serde(default)]
            cursor: usize,
        }
        let ask: Ask = read_ask(input)?;
        let columns = ask.columns.clamp(1, 64);
        let (bytes, edited, read) = self.derive(&ask.address, &ask.changes);
        let base = self.base_of(&ask.address);
        let mut rows: Vec<String> = Vec::new();
        let mut changed_rows: Vec<String> = Vec::new();
        let mut row = ask.first;
        while rows.len() < ask.count.min(4096) {
            let start = row.saturating_mul(columns);
            if start >= bytes.len() {
                break;
            }
            let end = start.saturating_add(columns).min(bytes.len());
            let Some(slice) = bytes.get(start..end) else {
                break;
            };
            let mut hex = String::with_capacity(slice.len().saturating_mul(2));
            let mut changed = String::with_capacity(slice.len());
            for (index, &byte) in slice.iter().enumerate() {
                let _ = write!(hex, "{byte:02X}");
                let held = base
                    .as_ref()
                    .and_then(|base| base.get(start.saturating_add(index)))
                    .copied();
                changed.push(if held == Some(byte) { '0' } else { '1' });
            }
            rows.push(hex);
            changed_rows.push(changed);
            row = row.saturating_add(1);
        }
        let findings: Vec<String> = findings(&bytes, ask.cursor, &ask.endian)
            .into_iter()
            .map(|row| format!("{}\t{}", field(&row, "label"), field(&row, "value")))
            .collect();
        Ok(json!({
            "length": bytes.len(),
            "edited": edited,
            "read": read,
            "error": if read { "" } else { "The bytes are not on this device." },
            "rows": rows,
            "changed": changed_rows,
            "findings": findings,
        })
        .to_string())
    }

    fn base_of(&self, address: &str) -> Option<Vec<u8>> {
        self.buffers()
            .get(address)
            .map(|buffer| buffer.base.clone())
            .or_else(|| read_address(address))
    }

    fn op_find(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            address: String,
            #[serde(default)]
            changes: Vec<String>,
            query: String,
            #[serde(default)]
            kind: String,
            #[serde(default)]
            case: bool,
            #[serde(default)]
            word: bool,
            #[serde(default)]
            limit: usize,
        }
        let ask: Ask = read_ask(input)?;
        let (bytes, _, read) = self.derive(&ask.address, &ask.changes);
        if !read {
            return Err("The bytes are not on this device.".to_owned());
        }
        let needle = if ask.kind == "text" {
            if ask.query.is_empty() {
                Vec::new()
            } else {
                ask.query.as_bytes().to_vec()
            }
        } else {
            parse_hex_query(&ask.query)
                .ok_or_else(|| "Write the bytes as hexadecimal, like `de ad be ef`.".to_owned())?
        };
        let rules = Rules {
            match_case: ask.case && ask.kind == "text",
            whole_word: ask.word && ask.kind == "text",
        };
        let limit = ask.limit.clamp(1, MAX_MATCHES);
        let (hits, truncated) = find_all(&bytes, &needle, rules, limit);
        let hits: Vec<String> = hits
            .into_iter()
            .map(|start| format!("{start}:{}", start.saturating_add(needle.len())))
            .collect();
        Ok(json!({ "hits": hits, "truncated": truncated }).to_string())
    }

    fn op_apply(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            address: String,
            #[serde(default)]
            changes: Vec<String>,
            op: String,
            #[serde(default)]
            at: usize,
            #[serde(default)]
            len: usize,
            #[serde(default)]
            bytes: String,
        }
        let ask: Ask = read_ask(input)?;
        let (bytes, _, read) = self.derive(&ask.address, &ask.changes);
        if !read {
            return Err("The bytes are not on this device.".to_owned());
        }
        let change = checked(&ask.op, bytes.len(), ask.at, ask.len, &ask.bytes)?;
        let length = apply_length(bytes.len(), &change);
        Ok(json!({ "change": change.text(), "length": length }).to_string())
    }

    fn op_replace(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            address: String,
            #[serde(default)]
            changes: Vec<String>,
            #[serde(default)]
            spans: Vec<String>,
            #[serde(default)]
            bytes: String,
            #[serde(default)]
            mode: String,
        }
        let ask: Ask = read_ask(input)?;
        let (bytes, _, read) = self.derive(&ask.address, &ask.changes);
        if !read {
            return Err("The bytes are not on this device.".to_owned());
        }
        let insert = parse_bytes(&ask.bytes).ok_or_else(|| {
            "Write the replacement as hexadecimal bytes, like `de ad be ef`.".to_owned()
        })?;
        let want = insert.len();
        let mut kept: Vec<(usize, usize)> = Vec::new();
        for span in &ask.spans {
            let Some((start, end)) = span
                .split_once(':')
                .and_then(|(start, end)| Some((start.parse().ok()?, end.parse().ok()?)))
            else {
                continue;
            };
            if start >= end || end > bytes.len() {
                continue;
            }
            if kept.last().is_some_and(|&(_, last_end)| start < last_end) {
                continue;
            }
            kept.push((start, end));
        }
        if ask.mode != "insert"
            && kept
                .iter()
                .any(|&(start, end)| end.saturating_sub(start) != want)
        {
            return Err(
                "The replacement has another length than the match. Switch to Insert to change \
                 the file's length."
                    .to_owned(),
            );
        }
        // Last to first: each edit's offsets are the ones it still stands at.
        let mut changes = Vec::with_capacity(kept.len());
        let mut length = bytes.len();
        for &(start, end) in kept.iter().rev() {
            let keep = if ask.mode == "insert" {
                insert.len()
            } else {
                want.min(end.saturating_sub(start))
            };
            let change = Change {
                at: start,
                remove: end.saturating_sub(start),
                insert: hex_text(insert.get(..keep).unwrap_or_default()),
            };
            length = apply_length(length, &change);
            changes.push(change);
        }
        let count = changes.len();
        let changes: Vec<String> = changes.iter().map(Change::text).collect();
        Ok(json!({ "changes": changes, "count": count, "length": length }).to_string())
    }

    fn op_range(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            address: String,
            #[serde(default)]
            changes: Vec<String>,
            #[serde(default)]
            start: usize,
            #[serde(default)]
            end: usize,
            #[serde(default)]
            kind: String,
        }
        let ask: Ask = read_ask(input)?;
        let (bytes, _, read) = self.derive(&ask.address, &ask.changes);
        if !read {
            return Err("The bytes are not on this device.".to_owned());
        }
        let end = ask.end.min(bytes.len());
        let start = ask.start.min(end);
        let slice = bytes.get(start..end).unwrap_or_default();
        let text = if ask.kind == "text" {
            slice
                .iter()
                .map(|&byte| {
                    if (0x20..=0x7E).contains(&byte) {
                        char::from(byte)
                    } else {
                        '.'
                    }
                })
                .collect::<String>()
        } else {
            let mut text = String::with_capacity(slice.len().saturating_mul(3));
            for byte in slice {
                if !text.is_empty() {
                    text.push(' ');
                }
                let _ = write!(text, "{byte:02x}");
            }
            text
        };
        Ok(json!({ "text": text }).to_string())
    }

    fn op_bytes(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            address: String,
            #[serde(default)]
            changes: Vec<String>,
        }
        let ask: Ask = read_ask(input)?;
        let (bytes, _, read) = self.derive(&ask.address, &ask.changes);
        if !read {
            return Err("The bytes are not on this device.".to_owned());
        }
        Ok(json!({ "bytes": base64_text(&bytes) }).to_string())
    }

    fn op_save(&self, input: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ask {
            address: String,
            #[serde(default)]
            changes: Vec<String>,
            path: String,
        }
        let ask: Ask = read_ask(input)?;
        let (bytes, _, read) = self.derive(&ask.address, &ask.changes);
        if !read {
            return Err("The bytes are not on this device.".to_owned());
        }
        write_atomic(Path::new(&ask.path), &bytes)?;
        let address = keep(&bytes)?;
        Ok(json!({ "address": address, "length": bytes.len() }).to_string())
    }
}

impl tilolang_tilo::engine::Provider for HexEngine {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "open" => Self::op_open(input),
            "rows" => self.op_rows(input),
            "find" => self.op_find(input),
            "range" => self.op_range(input),
            "bytes" => self.op_bytes(input),
            "apply" => self.op_apply(input),
            "replace" => self.op_replace(input),
            "save" => self.op_save(input),
            other => Err(format!("`{other}` is no op this engine runs")),
        }
    }
}

/// Register the hex editor's engine for the session's lifetime.
pub fn register() {
    tilolang_tilo::engine::register(APP, std::sync::Arc::new(HexEngine::new()));
}

fn read_ask<T: serde::de::DeserializeOwned>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))
}

/// The bytes of a `tilo-blob://` address, when this device holds them. The language names a
/// file's content so; the content store names the same bytes by the reference under it.
fn read_address(address: &str) -> Option<Vec<u8>> {
    let reference = address.strip_prefix("tilo-blob://").unwrap_or(address);
    tilo_resource::content::read_hash(reference).ok()
}

/// The address the workspace's content store keeps `bytes` at: the window whose code runs keeps
/// them, so every device of the workspace reads them by it.
fn keep(bytes: &[u8]) -> Result<String, String> {
    let store = tilo_resource::content::current()
        .ok_or_else(|| "This device keeps no content store for the window.".to_owned())?;
    let reference = store.put(bytes).map_err(|error| error.to_string())?;
    Ok(format!("tilo-blob://{reference}"))
}

/// Write `bytes` to `path` whole or not at all: a temporary file beside it, then a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().map_or_else(
        || "untitled.bin".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temporary = folder.join(format!(".{name}.{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&temporary).map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
    }
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        error.to_string()
    })
}

/// How many bytes differ between the base and the bytes with the edits applied.
fn differing(base: &[u8], bytes: &[u8]) -> usize {
    let common = base.len().min(bytes.len());
    let inside = base
        .iter()
        .zip(bytes)
        .take(common)
        .filter(|(a, b)| a != b)
        .count();
    inside
        .saturating_add(bytes.len().saturating_sub(common))
        .saturating_add(base.len().saturating_sub(common))
}

/// The length of the bytes after `change`.
fn apply_length(length: usize, change: &Change) -> usize {
    let removed = change
        .remove
        .min(length.saturating_sub(change.at.min(length)));
    length
        .saturating_sub(removed)
        .saturating_add(change.inserted().len())
}

/// The change `op` asks for, refused where it cannot stand.
fn checked(op: &str, length: usize, at: usize, len: usize, bytes: &str) -> Result<Change, String> {
    let insert = parse_bytes(bytes)
        .ok_or_else(|| "Write the bytes as hexadecimal, like `de ad be ef`.".to_owned())?;
    match op {
        "insert" => {
            if at > length {
                return Err("That offset is past the end of the bytes.".to_owned());
            }
            Ok(Change {
                at,
                remove: 0,
                insert: hex_text(&insert),
            })
        }
        "overwrite" => {
            if at >= length {
                return Err("That offset is past the end of the bytes.".to_owned());
            }
            let fits = insert.len().min(length.saturating_sub(at));
            Ok(Change {
                at,
                remove: fits,
                insert: hex_text(insert.get(..fits).unwrap_or_default()),
            })
        }
        "delete" => {
            if at > length || (at == length && len > 0) {
                return Err("That offset is past the end of the bytes.".to_owned());
            }
            let take = len.min(length.saturating_sub(at));
            Ok(Change {
                at,
                remove: take,
                insert: String::new(),
            })
        }
        other => Err(format!(
            "`{other}` is no edit of bytes: insert, overwrite or delete"
        )),
    }
}

/// Bytes written as base64 text, as a file's content travels.
fn base64_text(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Bytes written as hexadecimal text, two digits each.
fn hex_text(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(text, "{byte:02X}");
    }
    text
}

/// Bytes as a person writes them: `ff`, `0xFF`, `de ad be ef`, `DE-AD`.
#[must_use]
pub fn parse_bytes(text: &str) -> Option<Vec<u8>> {
    let digits: String = text
        .chars()
        .filter(char::is_ascii_hexdigit)
        .collect::<String>();
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(digits.get(i..i.saturating_add(2))?, 16).ok())
        .collect()
}

/// A search written as hexadecimal: whitespace and `-`, `:`, `,` between the digits are noise.
#[must_use]
pub fn parse_hex_query(query: &str) -> Option<Vec<u8>> {
    let kept: String = query
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != ':' && *c != ',')
        .collect();
    if kept.is_empty() {
        return None;
    }
    parse_hex(&kept)
}

fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if body.is_empty() {
        return Some(Vec::new());
    }
    if !body.chars().all(|c| c.is_ascii_hexdigit()) || !body.len().is_multiple_of(2) {
        return None;
    }
    parse_bytes(body)
}

/// How a text query matches: letter case and word boundaries. A hex query is always exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    pub match_case: bool,
    pub whole_word: bool,
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Where `needle` stands in `haystack`, up to `limit` of them, and whether there were more.
#[must_use]
pub fn find_all(haystack: &[u8], needle: &[u8], rules: Rules, limit: usize) -> (Vec<usize>, bool) {
    if needle.is_empty() || needle.len() > haystack.len() {
        return (Vec::new(), false);
    }
    let mut hits = Vec::new();
    let mut truncated = false;
    for (offset, window) in haystack.windows(needle.len()).enumerate() {
        let same = if rules.match_case {
            window == needle
        } else {
            window.eq_ignore_ascii_case(needle)
        };
        if !same {
            continue;
        }
        if rules.whole_word {
            let before = offset.checked_sub(1).and_then(|i| haystack.get(i));
            let after = haystack.get(offset.saturating_add(needle.len()));
            if before.copied().is_some_and(is_word_byte) || after.copied().is_some_and(is_word_byte)
            {
                continue;
            }
        }
        if hits.len() == limit {
            truncated = true;
            break;
        }
        hits.push(offset);
    }
    (hits, truncated)
}

/// The bytes at `cursor` read as the numbers they can be, in `endian`.
fn findings(bytes: &[u8], cursor: usize, endian: &str) -> Vec<serde_json::Value> {
    let mut rows: Vec<(String, String, usize)> = Vec::new();
    let big = endian == "big";
    let read = |n: usize| bytes.get(cursor..cursor.saturating_add(n));
    match bytes.get(cursor) {
        Some(&b) => {
            rows.push(("i8".to_owned(), b.cast_signed().to_string(), 1));
            rows.push(("u8".to_owned(), b.to_string(), 1));
            rows.push(("bin".to_owned(), format!("{b:08b}"), 1));
            rows.push(("oct".to_owned(), format!("0o{b:o}"), 1));
            rows.push(("char".to_owned(), printable_char(b), 1));
        }
        None => {
            for label in ["i8", "u8", "bin", "oct", "char"] {
                rows.push(not_read(label));
            }
        }
    }
    for (label, n) in [
        ("i16", 2),
        ("u16", 2),
        ("i32", 4),
        ("u32", 4),
        ("i64", 8),
        ("u64", 8),
    ] {
        rows.push(int_row(label, read(n), big, n));
    }
    rows.push(float_row("f32", read(4), big, 4));
    rows.push(float_row("f64", read(8), big, 8));
    let sub = |at: usize| bytes.get(cursor.saturating_add(at)).copied();
    let uleb = leb128(&sub, false);
    rows.push(match uleb {
        Some((v, n)) if v >= 0 => ("uleb128".to_owned(), v.to_string(), n),
        _ => not_read("uleb128"),
    });
    let sleb = leb128(&sub, true);
    rows.push(match sleb {
        Some((v, n)) => ("sleb128".to_owned(), v.to_string(), n),
        None => not_read("sleb128"),
    });
    rows.into_iter()
        .map(|(label, value, width)| json!({ "label": label, "value": value, "width": width }))
        .collect()
}

/// The row of a number the bytes at the cursor do not hold enough of.
fn not_read(label: &str) -> (String, String, usize) {
    (label.to_owned(), "—".to_owned(), 0)
}

/// `raw` as `N` bytes, in the order little-endian reads them: largest byte last.
fn ordered<const N: usize>(raw: &[u8], big: bool) -> Option<[u8; N]> {
    let mut bytes: [u8; N] = raw.try_into().ok()?;
    if big {
        bytes.reverse();
    }
    Some(bytes)
}

/// The row for `label`, `raw` read as a `width`-byte integer, signed when the label starts
/// with `i`.
fn int_row(label: &str, raw: Option<&[u8]>, big: bool, width: usize) -> (String, String, usize) {
    let Some(raw) = raw else {
        return not_read(label);
    };
    let text = if label.starts_with('i') {
        signed(raw, big, width).map(|value| value.to_string())
    } else {
        unsigned(raw, big, width).map(|value| value.to_string())
    };
    let Some(text) = text else {
        return not_read(label);
    };
    (label.to_owned(), text, width)
}

/// `raw` as an unsigned `width`-byte number, read in the asked endianness.
fn unsigned(raw: &[u8], big: bool, width: usize) -> Option<u64> {
    match width {
        2 => ordered(raw, big).map(|b| u64::from(u16::from_le_bytes(b))),
        4 => ordered(raw, big).map(|b| u64::from(u32::from_le_bytes(b))),
        _ => ordered(raw, big).map(u64::from_le_bytes),
    }
}

/// `raw` as a signed `width`-byte number, read in the asked endianness.
fn signed(raw: &[u8], big: bool, width: usize) -> Option<i64> {
    match width {
        2 => ordered(raw, big).map(|b| i64::from(i16::from_le_bytes(b))),
        4 => ordered(raw, big).map(|b| i64::from(i32::from_le_bytes(b))),
        _ => ordered(raw, big).map(i64::from_le_bytes),
    }
}

/// The row for `label`, `raw` read as a `width`-byte float in the asked endianness.
fn float_row(label: &str, raw: Option<&[u8]>, big: bool, width: usize) -> (String, String, usize) {
    let value = raw.and_then(|raw| match width {
        4 => ordered(raw, big).map(|b| f64::from(f32::from_le_bytes(b))),
        _ => ordered(raw, big).map(f64::from_le_bytes),
    });
    let Some(value) = value else {
        return not_read(label);
    };
    (label.to_owned(), format_float(value), width)
}

/// The `name` field of a row `findings` answered, or nothing when it is not there.
fn field<'a>(row: &'a serde_json::Value, name: &str) -> &'a str {
    row.get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// An unsigned or signed LEB128 varint at a byte source: the value and how many bytes it took.
fn leb128(byte: &dyn Fn(usize) -> Option<u8>, signed: bool) -> Option<(i64, usize)> {
    let mut result: i64 = 0;
    let mut shift: u32 = 0;
    let mut consumed = 0usize;
    loop {
        let b = byte(consumed)?;
        consumed = consumed.saturating_add(1);
        if shift >= 64 {
            return None;
        }
        let low = i64::from(b & 0x7F);
        result |= low << shift;
        shift = shift.saturating_add(7);
        if b & 0x80 == 0 {
            if signed && shift < 64 && (b & 0x40) != 0 {
                result |= -1i64 << shift;
            }
            return Some((result, consumed));
        }
    }
}

/// A float, compact: enough precision to read a buffer, without noise.
fn format_float(v: f64) -> String {
    if v == 0.0 {
        "0".to_owned()
    } else if v.is_nan() {
        "NaN".to_owned()
    } else if v.is_infinite() {
        if v > 0.0 {
            "+inf".to_owned()
        } else {
            "-inf".to_owned()
        }
    } else if v.abs() >= 1e-4 && v.abs() < 1e9 {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        format!("{v:e}")
    }
}

/// A byte as a quoted printable character, or its escape for control bytes.
fn printable_char(b: u8) -> String {
    match b {
        0x20..=0x7E => format!("'{}'", b as char),
        b'\n' => "'\\n'".to_owned(),
        b'\r' => "'\\r'".to_owned(),
        b'\t' => "'\\t'".to_owned(),
        0 => "'\\0'".to_owned(),
        _ => format!("\\x{b:02x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn the_engine_applies_edits_finds_bytes_and_reads_them_as_numbers;

        phase edits_apply_at_their_offsets_and_nothing_changes_away_from_them;
        {
            let mut bytes = vec![0u8, 1, 2, 3];
            Change { at: 1, remove: 1, insert: "AB".to_owned() }.apply_to(&mut bytes);
            assert_eq!(bytes, [0x00, 0xAB, 2, 3]);
            Change { at: 0, remove: 0, insert: "0A0B".to_owned() }.apply_to(&mut bytes);
            assert_eq!(bytes, [0x0A, 0x0B, 0x00, 0xAB, 2, 3]);
            Change { at: 2, remove: 9, insert: String::new() }.apply_to(&mut bytes);
            assert_eq!(bytes, [0x0A, 0x0B]);
            assert_eq!(differing(&[1, 2, 3], &[1, 9, 3]), 1);
            assert_eq!(differing(&[1, 2], &[1, 2, 3]), 1);
        }

        phase a_typed_edit_is_read_as_bytes_and_refused_past_the_end;
        {
            assert_eq!(parse_bytes("de ad be ef").unwrap(), [0xDE, 0xAD, 0xBE, 0xEF]);
            assert!(parse_bytes("abc").is_none());
            assert_eq!(checked("insert", 3, 3, 0, "FF").unwrap().insert, "FF");
            assert!(checked("insert", 3, 4, 0, "FF").is_err());
            assert!(checked("overwrite", 3, 3, 0, "FF").is_err());
            assert_eq!(checked("overwrite", 3, 2, 0, "0809").unwrap().insert, "08");
            assert!(checked("delete", 3, 3, 1, "").is_err());
            assert_eq!(checked("delete", 3, 1, 9, "").unwrap().remove, 2);
            assert_eq!(apply_length(5, &Change { at: 2, remove: 2, insert: "AABBCC".to_owned() }), 6);
        }

        phase a_search_finds_hex_and_text_under_its_rules;
        {
            let hay = b"IHDR ihdr IHDRx _IHDR";
            let any = Rules { match_case: false, whole_word: false };
            assert_eq!(find_all(hay, b"ihdr", any, 10).0, vec![0, 5, 10, 17]);
            let exact = Rules { match_case: true, whole_word: false };
            assert_eq!(find_all(hay, b"ihdr", exact, 10).0, vec![5]);
            let word = Rules { match_case: false, whole_word: true };
            assert_eq!(find_all(hay, b"ihdr", word, 10).0, vec![0, 5]);
            assert_eq!(find_all(hay, b"IHDR", word, 1), (vec![0], true));
            assert_eq!(parse_hex_query("de-ad:be").unwrap(), [0xDE, 0xAD, 0xBE]);
            assert!(parse_hex_query("zz").is_none());
        }

        phase the_bytes_around_a_place_read_as_numbers_in_either_order;
        {
            let rows = findings(&[0x34, 0x12, 0, 0, 0, 0, 0, 0], 0, "little");
            let value = |label: &str| {
                rows.iter()
                    .find(|row| row["label"] == label)
                    .map(|row| row["value"].as_str().unwrap_or("").to_owned())
                    .unwrap_or_default()
            };
            assert_eq!(value("u16"), "4660");
            assert_eq!(value("i8"), "52");
            assert_eq!(value("u8"), "52");
            assert_eq!(value("oct"), "0o64");
            let big = findings(&[0x34, 0x12], 0, "big");
            assert_eq!(
                big.iter().find(|row| row["label"] == "u16").map(|row| row["value"].as_str()),
                Some(Some("13330"))
            );
            let none = findings(&[], 0, "little");
            assert_eq!(none[0]["value"], "—");
            assert_eq!(leb128(&|at| [0xE5, 0x8E, 0x26].get(at).copied(), false), Some((624_485, 3)));
            assert_eq!(leb128(&|at| [0xC0, 0xBB, 0x78].get(at).copied(), true), Some((-123_456, 3)));
        }
    }
}

#[cfg(test)]
mod scope_test {
    use super::*;

    #[test]
    fn a_kept_address_reads_back_inside_the_window_scope() {
        let store = tilo_content::ContentStore::memory_in_workspace();
        let scope = tilo_wake::WakeScope::new();
        tilo_resource::content::register(&scope.handle(), store);
        let _guard = scope.enter();
        let address = keep(&[1, 2, 3]).expect("kept");
        let read = read_address(&address);
        assert_eq!(read, Some(vec![1, 2, 3]), "{address}");
    }
}
