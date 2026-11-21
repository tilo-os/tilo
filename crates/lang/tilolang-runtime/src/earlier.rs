//! What an earlier build of an app kept, in documents of the platform's resource layout: the
//! records a native app of the same id wrote before its tilolang package took the id over.
//!
//! A package's `migration action` reads them with `earlier(r)` and writes what they hold into the
//! package's own resources, once, as the platform runs any migration. Nothing here writes:
//! the earlier documents stay as they are, so a migration that could not carry something leaves
//! it where it was rather than losing it.
//!
//! The layout read is the one every native resource is stored in (`tilo_resource`): a record's
//! fields at the document's root, and each collection a map of item ids, each item an order key
//! (`k`) and its fields (`v`). A field reads as what it holds, generically: words and numbers as
//! text, rich text as rich text, a set or a map as its keys, and a map of bytes (a native app's
//! pictures, by their digest) as the files they are. A document that is no record of
//! that layout (one written before it, which carries neither `tilo:version` nor `tilo:record`)
//! yields nothing: it is left where it is, unread.

use std::collections::BTreeMap;

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value as AmValue};

use crate::rich::RichValue;
use crate::value::Value;

/// The schema mark a rich-text field's map carries (`tilo_doc`'s).
const RICH_SCHEMA: &str = "tilo.richtext";

/// The key of the version a record of the resource layout is stored at.
const VERSION_KEY: &str = "tilo:version";

/// The key of what names a document of the resource layout: its id, its kind, its workspace.
const RECORD_KEY: &str = "tilo:record";

/// One item an earlier build kept, or one document's own fields: `host.Earlier` as code reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EarlierItem {
    /// The id it was kept under: the item's id in its collection, or the document's id for the
    /// document's own fields.
    pub id: String,
    /// The collection that kept it (`pages`); empty for a document's own fields.
    pub of: String,
    /// Its order key in that collection: the items of a collection read in the order of their
    /// keys. Empty for a document's own fields.
    pub order: String,
    /// The version of the layout the document holding it was stored at; `0` when the document
    /// does not say (a part of a chain).
    pub version: i64,
    /// Each field that reads as words: text, a name, a number, a flag.
    pub texts: BTreeMap<String, String>,
    /// Each field that holds several: a set or a map, as its keys; a list, as its entries.
    pub lists: BTreeMap<String, Vec<String>>,
    /// Each rich-text field.
    pub rich: BTreeMap<String, RichValue>,
    /// Each field that holds bytes by key (a native app's pictures, by their digest): the bytes
    /// under each key, which [`Self::value`] keeps as files.
    pub bytes: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
}

impl EarlierItem {
    /// The item as `host.Earlier` holds it: a tuple of its fields in the host's order. Each of
    /// its bytes is the file `keep` makes of it in the device's content store, named by its key
    /// and the media type its bytes say; bytes `keep` cannot keep are left out, which the
    /// migration reading them sees as a file that is not there.
    #[must_use]
    pub fn value(&self, keep: &dyn Fn(&str, &[u8]) -> Option<Value>) -> Value {
        let text = |s: &str| Value::Text(s.to_owned());
        Value::Tuple(vec![
            text(&self.id),
            text(&self.of),
            text(&self.order),
            Value::Int(self.version),
            Value::Map(
                self.texts
                    .iter()
                    .map(|(name, said)| (text(name), text(said)))
                    .collect(),
            ),
            Value::Map(
                self.lists
                    .iter()
                    .map(|(name, held)| {
                        (
                            text(name),
                            Value::List(held.iter().map(|s| text(s)).collect()),
                        )
                    })
                    .collect(),
            ),
            Value::Map(
                self.rich
                    .iter()
                    .map(|(name, rich)| (text(name), Value::Rich(rich.clone())))
                    .collect(),
            ),
            Value::Map(
                self.bytes
                    .iter()
                    .map(|(name, held)| {
                        let files = held
                            .iter()
                            .filter_map(|(key, bytes)| {
                                let file = format!("{key}.{}", extension_of(bytes));
                                Some((text(key), keep(&file, bytes)?))
                            })
                            .collect();
                        (text(name), Value::Map(files))
                    })
                    .collect(),
            ),
        ])
    }
}

/// What the earlier document `doc`, kept under the id `id`, holds: its own fields as one item,
/// then every item of each of its collections, in their order. Nothing for a document that is
/// no record of the resource layout.
#[must_use]
pub fn items(id: &str, doc: &AutoCommit) -> Vec<EarlierItem> {
    let Some(version) = version(doc) else {
        return Vec::new();
    };
    let mut own = EarlierItem {
        id: id.to_owned(),
        version,
        ..EarlierItem::default()
    };
    let mut held = Vec::new();
    for key in doc.keys(&automerge::ROOT) {
        if internal(&key) {
            continue;
        }
        let Ok(Some((value, obj))) = doc.get(&automerge::ROOT, key.as_str()) else {
            continue;
        };
        if let Some(mut items) = collection(doc, &key, &value, &obj, version) {
            held.append(&mut items);
        } else {
            read_field(doc, &mut own, &key, &value, &obj);
        }
    }
    let mut out = vec![own];
    out.append(&mut held);
    out
}

/// The documents `documents` names but does not hold: each part of a chain before the newest
/// one held (`derived:part.<n>@<chain>`, numbered from `0`), and the record a chain belongs to
/// (`<chain>` is `derived:<role>@<record>`). Empty when everything a held part implies is here.
/// A chain's newest part is the one a workspace sends first and follows live, so it is the part
/// an earlier one is missing beside, never the other way round.
#[must_use]
pub fn missing(documents: &[(String, AutoCommit)]) -> Vec<String> {
    let held: std::collections::BTreeSet<&str> =
        documents.iter().map(|(id, _)| id.as_str()).collect();
    let mut newest: BTreeMap<&str, u64> = BTreeMap::new();
    for (id, _) in documents {
        let Some((n, chain)) = id
            .strip_prefix("derived:part.")
            .and_then(|rest| rest.split_once('@'))
        else {
            continue;
        };
        let Ok(n) = n.parse::<u64>() else {
            continue;
        };
        let at = newest.entry(chain).or_insert(n);
        *at = (*at).max(n);
    }
    let mut out = Vec::new();
    for (chain, newest) in newest {
        if let Some((_, record)) = chain
            .strip_prefix("derived:")
            .and_then(|rest| rest.split_once('@'))
            && !held.contains(record)
        {
            out.push(record.to_owned());
        }
        for n in 0..newest {
            let part = format!("derived:part.{n}@{chain}");
            if !held.contains(part.as_str()) {
                out.push(part);
            }
        }
    }
    out
}

/// The layout version `doc` is a record of, when it is one: the version it says, or `0` for a
/// document of the layout that says none (a part of a chain, which its record's version covers).
fn version(doc: &AutoCommit) -> Option<i64> {
    let stated = match doc.get(&automerge::ROOT, VERSION_KEY).ok().flatten() {
        Some((AmValue::Scalar(s), _)) => s.as_i64(),
        _ => None,
    };
    let record = matches!(
        doc.get(&automerge::ROOT, RECORD_KEY),
        Ok(Some((AmValue::Object(ObjType::Map), _)))
    );
    stated.or_else(|| record.then_some(0))
}

/// Whether `key` is the platform's own, not the record's: its version, its address, how its
/// storage is cut.
fn internal(key: &str) -> bool {
    key.starts_with("tilo:") || key.starts_with('_')
}

/// The items of the collection `key` holds, when it is one: a map whose every entry is an order
/// key and the item's fields. `None` for any other field.
fn collection(
    doc: &AutoCommit,
    key: &str,
    value: &AmValue<'_>,
    obj: &ObjId,
    version: i64,
) -> Option<Vec<EarlierItem>> {
    if !matches!(value, AmValue::Object(ObjType::Map)) {
        return None;
    }
    let ids: Vec<String> = doc.keys(obj).collect();
    if ids.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let (AmValue::Object(ObjType::Map), entry) = doc.get(obj, id.as_str()).ok()?? else {
            return None;
        };
        let order = words(doc, &entry, "k")?;
        let (AmValue::Object(ObjType::Map), fields) = doc.get(&entry, "v").ok()?? else {
            return None;
        };
        let mut item = EarlierItem {
            id,
            of: key.to_owned(),
            order,
            version,
            ..EarlierItem::default()
        };
        for name in doc.keys(&fields) {
            if internal(&name) {
                continue;
            }
            if let Ok(Some((value, at))) = doc.get(&fields, name.as_str()) {
                read_field(doc, &mut item, &name, &value, &at);
            }
        }
        out.push(item);
    }
    out.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    Some(out)
}

/// The field `name` of `item`, holding `value` (at `obj` when it is an object), as it reads.
fn read_field(
    doc: &AutoCommit,
    item: &mut EarlierItem,
    name: &str,
    value: &AmValue<'_>,
    obj: &ObjId,
) {
    match value {
        AmValue::Scalar(s) => {
            if let Some(said) = scalar_words(s) {
                item.texts.insert(name.to_owned(), said);
            }
        }
        AmValue::Object(ObjType::Text) => {
            if let Ok(said) = doc.text(obj) {
                item.texts.insert(name.to_owned(), said);
            }
        }
        AmValue::Object(ObjType::Map | ObjType::Table) => {
            if words(doc, obj, "schema").as_deref() == Some(RICH_SCHEMA) {
                item.rich
                    .insert(name.to_owned(), crate::rich::read_under(doc, obj));
            } else if let Some(held) = bytes_by_key(doc, obj) {
                item.bytes.insert(name.to_owned(), held);
            } else {
                item.lists.insert(name.to_owned(), doc.keys(obj).collect());
            }
        }
        AmValue::Object(ObjType::List) => {
            let held = (0..doc.length(obj))
                .filter_map(|at| match doc.get(obj, at).ok()?? {
                    (AmValue::Scalar(s), _) => scalar_words(&s),
                    (AmValue::Object(ObjType::Text), text) => doc.text(&text).ok(),
                    (AmValue::Object(_), _) => None,
                })
                .collect();
            item.lists.insert(name.to_owned(), held);
        }
    }
}

/// The bytes the map `obj` holds under each of its keys, when it holds some and nothing else.
fn bytes_by_key(doc: &AutoCommit, obj: &ObjId) -> Option<BTreeMap<String, Vec<u8>>> {
    let mut held = BTreeMap::new();
    for key in doc.keys(obj) {
        let (AmValue::Scalar(s), _) = doc.get(obj, key.as_str()).ok()?? else {
            return None;
        };
        let ScalarValue::Bytes(bytes) = s.as_ref() else {
            return None;
        };
        held.insert(key, bytes.clone());
    }
    (!held.is_empty()).then_some(held)
}

/// The extension of a file holding `bytes`, as the picture formats a note shows mark their
/// first bytes; `bin` for any other content.
fn extension_of(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        "webp"
    } else if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        "svg"
    } else {
        "bin"
    }
}

/// The words the field `key` of `obj` says, when it holds words.
fn words(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key).ok()?? {
        (AmValue::Scalar(s), _) => scalar_words(&s),
        (AmValue::Object(ObjType::Text), text) => doc.text(&text).ok(),
        (AmValue::Object(_), _) => None,
    }
}

/// A scalar as words; `None` for one that says none (nothing, bytes).
fn scalar_words(s: &ScalarValue) -> Option<String> {
    Some(match s {
        ScalarValue::Str(text) => text.to_string(),
        ScalarValue::Int(v) | ScalarValue::Timestamp(v) => v.to_string(),
        ScalarValue::Uint(v) => v.to_string(),
        ScalarValue::F64(v) => v.to_string(),
        ScalarValue::Counter(_) => s.as_i64()?.to_string(),
        ScalarValue::Boolean(v) => v.to_string(),
        ScalarValue::Null | ScalarValue::Bytes(_) | ScalarValue::Unknown { .. } => return None,
    })
}
