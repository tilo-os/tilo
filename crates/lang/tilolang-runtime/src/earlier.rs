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
//! text, rich text as rich text, a set or a map as its keys (and a map of words as what each key
//! holds too), and a map of bytes (a native app's
//! pictures, by their digest) as the files they are. A field that holds a record of its own (a
//! draft's sender, its account and address) reads as that record's fields too, each under the
//! field's name and its own (`sender.email`); one at a document's root (the draft a composer
//! writes, beside the collection of the others) is besides an item of its own, `of` the field.
//! A field that keeps records of its own, in a collection or a list (a skill's examples), reads in
//! `records`, the records in the order they stood, each record's fields by their names.
//! A document that is no record of that layout (one written before it, which carries neither
//! `tilo:version` nor `tilo:record` nor any field kept by key, only bare words) yields nothing:
//! it is left where it is, unread.
//!
//! Who wrote an item there is read from the document's own history: each change names the
//! action that made it and who invoked it, so an item is the person whose change last wrote it,
//! or the person an agent wrote it for (`by`). A carry that makes a record under the id the item
//! had makes it as theirs ([`EarlierItem::by`]).

use std::collections::BTreeMap;

use automerge::{ActorId, AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value as AmValue};

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
    /// Each field that maps keys to words (a native app's watch times, by video): what each key
    /// holds, as words. Its keys are in [`Self::lists`] too.
    pub entries: BTreeMap<String, BTreeMap<String, String>>,
    /// Each field that keeps records of its own, in a collection or a list (a skill's examples):
    /// the records in the order they stood, each record's fields as words.
    pub records: BTreeMap<String, Vec<BTreeMap<String, String>>>,
    /// Who wrote it there, as the document's history names them: the person whose change last
    /// wrote the item, or the person an agent wrote it for. `None` where no person did (the
    /// platform's own writes), and for a document's own fields.
    pub by: Option<crate::ids::MemberId>,
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
            Value::Map(
                self.entries
                    .iter()
                    .map(|(name, held)| {
                        let said = held
                            .iter()
                            .map(|(key, said)| (text(key), text(said)))
                            .collect();
                        (text(name), Value::Map(said))
                    })
                    .collect(),
            ),
            Value::Map(
                self.texts
                    .iter()
                    .filter_map(|(name, said)| {
                        let named = members_named(said)?;
                        let named = named
                            .into_iter()
                            .map(|member| Value::Actor(crate::ids::Actor::Member(member)))
                            .collect();
                        Some((text(name), Value::List(named)))
                    })
                    .collect(),
            ),
            Value::Map(
                self.records
                    .iter()
                    .map(|(name, records)| {
                        let records = records
                            .iter()
                            .map(|record| {
                                Value::Map(
                                    record
                                        .iter()
                                        .map(|(field, said)| (text(field), text(said)))
                                        .collect(),
                                )
                            })
                            .collect();
                        (text(name), Value::List(records))
                    })
                    .collect(),
            ),
            Value::Opt(
                self.by
                    .clone()
                    .map(|by| Box::new(Value::Actor(crate::ids::Actor::Member(by)))),
            ),
        ])
    }
}

/// Who wrote each change of a document: the ops of each change, by the actor that wrote them,
/// and the person the change was made by or for.
struct Writers(BTreeMap<ActorId, Vec<(u64, u64, Option<crate::ids::MemberId>)>>);

impl Writers {
    /// The changes of `doc` and who made each.
    fn of(doc: &AutoCommit) -> Self {
        let mut doc = doc.clone();
        let mut by: BTreeMap<ActorId, Vec<(u64, u64, Option<crate::ids::MemberId>)>> =
            BTreeMap::new();
        for change in doc.get_changes_meta(&[]) {
            let person = change.message.as_deref().and_then(person_of);
            by.entry(change.actor.into_owned()).or_default().push((
                change.start_op,
                change.max_op,
                person,
            ));
        }
        Self(by)
    }

    /// The person whose change wrote the newest of the ops `ops` that a person wrote: who last
    /// wrote what they are.
    fn last_of(&self, ops: &[automerge::ObjId]) -> Option<crate::ids::MemberId> {
        let mut written: Vec<(u64, &ActorId)> = ops
            .iter()
            .filter_map(|op| match op {
                automerge::ObjId::Id(counter, actor, _) => Some((*counter, actor)),
                automerge::ObjId::Root => None,
            })
            .collect();
        written.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(a.1)));
        written.into_iter().find_map(|(counter, actor)| {
            self.0
                .get(actor)?
                .iter()
                .find(|(start, max, _)| (*start..=*max).contains(&counter))?
                .2
                .clone()
        })
    }
}

/// The person a change's message says it was made by, or made for when an agent made it; `None`
/// for a change no person made or that names nobody.
fn person_of(message: &str) -> Option<crate::ids::MemberId> {
    let context = tilo_actions::ActionContext::from_change_message(message)?;
    let invoker = context.invoker?;
    let person = invoker
        .acting
        .person_id()
        .or_else(|| invoker.acting.acted_for_person())?;
    Some(crate::ids::MemberId::person(person))
}

/// The ops that wrote the record `obj` and its fields: the record's making, and each field's
/// last write.
fn ops_of(doc: &AutoCommit, obj: &ObjId) -> Vec<ObjId> {
    let mut ops = vec![obj.clone()];
    ops.extend(
        doc.keys(obj)
            .filter_map(|key| doc.get(obj, key.as_str()).ok().flatten())
            .map(|(_, op)| op),
    );
    ops
}

/// The members the words `said` name, as a native app kept them: one member (`{"person":"<id>"}`),
/// or several, as the id of a direct conversation names its people
/// (`direct:[{"person":"<id>"},...]`). None for words that name no member.
fn members_named(said: &str) -> Option<Vec<crate::ids::MemberId>> {
    let said = said.strip_prefix("direct:").unwrap_or(said);
    if let Ok(one) = serde_json::from_str::<crate::ids::MemberId>(said) {
        return Some(vec![one]);
    }
    serde_json::from_str::<Vec<crate::ids::MemberId>>(said)
        .ok()
        .filter(|several| !several.is_empty())
}

/// What the earlier document `doc`, kept under the id `id`, holds: its own fields as one item,
/// then every item of each of its collections, in their order. Nothing for a document that is
/// no record of the resource layout.
#[must_use]
pub fn items(id: &str, doc: &AutoCommit) -> Vec<EarlierItem> {
    let Some(version) = version(doc) else {
        return Vec::new();
    };
    let writers = Writers::of(doc);
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
        if let Some(mut items) = collection(doc, &key, &value, &obj, version, Some(&writers)) {
            held.append(&mut items);
        } else {
            if record(doc, &value, &obj) {
                let mut item = EarlierItem {
                    id: key.clone(),
                    of: key.clone(),
                    version,
                    by: writers.last_of(&ops_of(doc, &obj)),
                    ..EarlierItem::default()
                };
                read_fields(doc, &mut item, "", &obj, 0);
                held.push(item);
            }
            // A document's own fields are read as they are: a record among them is an item.
            read_field_at(doc, &mut own, &key, &value, &obj, RECORD_DEPTH);
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
    // A record at the layout's first version says none, and an app's own record (its package's
    // primary document) is not stamped with what names it: it is one by holding a field of the
    // layout's own shapes, a collection or a map by key (a record kept as keyed values alone, as
    // a native watch history keeps when each video was watched).
    stated
        .or_else(|| record.then_some(0))
        .or_else(|| holds_layout_field(doc).then_some(1))
}

/// Whether `doc` holds, at its root, a field the resource layout keeps by key: a collection, a
/// set, or a map of values. A document from before the layout keeps its fields as bare words.
fn holds_layout_field(doc: &AutoCommit) -> bool {
    doc.keys(&automerge::ROOT).any(|key| {
        !internal(&key)
            && matches!(
                doc.get(&automerge::ROOT, key.as_str()),
                Ok(Some((AmValue::Object(ObjType::Map), _)))
            )
    })
}

/// Whether `key` is the platform's own, not the record's: its version, its address, how its
/// storage is cut.
fn internal(key: &str) -> bool {
    key.starts_with("tilo:") || key.starts_with('_')
}

/// The items of the collection `key` holds, when it is one: a map whose every entry is an order
/// key and the item's fields, each by who wrote it where `writers` says, or a map of items kept
/// by a key of their own, each entry the item's fields. `None` for any other field.
fn collection(
    doc: &AutoCommit,
    key: &str,
    value: &AmValue<'_>,
    obj: &ObjId,
    version: i64,
    writers: Option<&Writers>,
) -> Option<Vec<EarlierItem>> {
    // `tl` holds the workspace's own references to a package's resources, not a collection.
    if key == "tl" || !matches!(value, AmValue::Object(ObjType::Map)) {
        return None;
    }
    let ids: Vec<String> = doc.keys(obj).collect();
    if ids.is_empty() {
        return None;
    }
    let mut entries = Vec::with_capacity(ids.len());
    for id in &ids {
        let (AmValue::Object(ObjType::Map), entry) = doc.get(obj, id.as_str()).ok()?? else {
            return None;
        };
        entries.push((id.clone(), entry));
    }
    // An ordered collection keeps each item under an order key (`k`) and its fields (`v`).
    let ordered = entries.iter().all(|(_, entry)| {
        words(doc, entry, "k").is_some()
            && matches!(
                doc.get(entry, "v"),
                Ok(Some((AmValue::Object(ObjType::Map), _)))
            )
    });
    let mut out = Vec::with_capacity(entries.len());
    for (id, entry) in entries {
        // A keyed collection keeps the item's fields at its key, named by the item's own key
        // when it has one and by that key otherwise.
        let (order, fields) = if ordered {
            let (AmValue::Object(ObjType::Map), fields) = doc.get(&entry, "v").ok()?? else {
                return None;
            };
            (words(doc, &entry, "k")?, fields)
        } else {
            let order = words(doc, &entry, "key").unwrap_or_else(|| id.clone());
            (order, entry.clone())
        };
        let mut ops = vec![entry.clone()];
        ops.extend(ops_of(doc, &fields));
        let by = writers.and_then(|writers| writers.last_of(&ops));
        let mut item = EarlierItem {
            id,
            of: key.to_owned(),
            order,
            version,
            by,
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
    read_field_at(doc, item, name, value, obj, 0);
}

/// How deep a record inside a record is read: deeper than any native resource keeps one.
const RECORD_DEPTH: usize = 4;

/// [`read_field`] of a field `depth` records down.
fn read_field_at(
    doc: &AutoCommit,
    item: &mut EarlierItem,
    name: &str,
    value: &AmValue<'_>,
    obj: &ObjId,
    depth: usize,
) {
    match value {
        AmValue::Scalar(s) => {
            if let Some(said) = scalar_words(s) {
                // A record a native app kept as JSON in one field (an effect entry's message)
                // reads as that record's fields too, each under the field's name and its own
                // (`value.draft.to`).
                if said.starts_with('{')
                    && let Ok(serde_json::Value::Object(record)) =
                        serde_json::from_str::<serde_json::Value>(&said)
                {
                    for (key, held) in &record {
                        read_json(item, &format!("{name}.{key}"), held);
                    }
                }
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
                if let Some(said) = words_by_key(doc, obj) {
                    item.entries.insert(name.to_owned(), said);
                }
                item.lists.insert(name.to_owned(), doc.keys(obj).collect());
                if let Some(records) = collection(doc, name, value, obj, 0, None) {
                    // A collection kept in a field (a skill's examples): its records, in the
                    // order they stood, each record's words by its field's name.
                    item.records.insert(
                        name.to_owned(),
                        records.into_iter().map(|record| record.texts).collect(),
                    );
                } else if depth < RECORD_DEPTH && record(doc, value, obj) {
                    read_fields(doc, item, name, obj, depth.saturating_add(1));
                }
            }
        }
        AmValue::Object(ObjType::List) => {
            let mut held = Vec::new();
            let mut records = Vec::new();
            for at in 0..doc.length(obj) {
                match doc.get(obj, at).ok().flatten() {
                    // A record a native app kept as JSON in one entry (a draft's attachment)
                    // reads as the records below do (`attachments.filename`), and as its words.
                    Some((AmValue::Scalar(s), _)) => {
                        let Some(said) = scalar_words(&s) else {
                            continue;
                        };
                        if said.starts_with('{')
                            && let Ok(serde_json::Value::Object(record)) =
                                serde_json::from_str::<serde_json::Value>(&said)
                        {
                            let mut fields = BTreeMap::new();
                            for (key, inner) in record {
                                let words = match &inner {
                                    serde_json::Value::String(text) => text.clone(),
                                    serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
                                        inner.to_string()
                                    }
                                    _ => continue,
                                };
                                item.lists
                                    .entry(format!("{name}.{key}"))
                                    .or_default()
                                    .push(words.clone());
                                fields.insert(key, words);
                            }
                            records.push(fields);
                        }
                        held.push(said);
                    }
                    Some((AmValue::Object(ObjType::Text), text)) => {
                        held.extend(doc.text(&text).ok());
                    }
                    // A list of records (a skill's examples) reads as its records, in the order
                    // they stand, and as a list of each of their fields (`examples.text`), an
                    // entry for each record that has it.
                    Some((AmValue::Object(ObjType::Map), record)) => {
                        let mut fields = BTreeMap::new();
                        for key in doc.keys(&record) {
                            if let Some(said) = words(doc, &record, &key) {
                                item.lists
                                    .entry(format!("{name}.{key}"))
                                    .or_default()
                                    .push(said.clone());
                                fields.insert(key, said);
                            }
                        }
                        records.push(fields);
                    }
                    _ => {}
                }
            }
            item.lists.insert(name.to_owned(), held);
            if !records.is_empty() {
                item.records.insert(name.to_owned(), records);
            }
        }
    }
}

/// The field `name` of `item` as the JSON `held` a native app kept it in says it, read as
/// [`read_field`] reads the same shapes: words as text, a record as its fields under the field's
/// name, a list as its entries and a list of records as a list of each of their fields.
fn read_json(item: &mut EarlierItem, name: &str, held: &serde_json::Value) {
    use serde_json::Value as Json;
    match held {
        Json::Null => {}
        Json::String(said) => {
            item.texts.insert(name.to_owned(), said.clone());
        }
        Json::Bool(_) | Json::Number(_) => {
            item.texts.insert(name.to_owned(), held.to_string());
        }
        Json::Object(record) => {
            for (key, inner) in record {
                read_json(item, &format!("{name}.{key}"), inner);
            }
            item.lists
                .insert(name.to_owned(), record.keys().cloned().collect());
        }
        Json::Array(entries) => {
            let mut said = Vec::new();
            for entry in entries {
                match entry {
                    Json::String(text) => said.push(text.clone()),
                    Json::Bool(_) | Json::Number(_) => said.push(entry.to_string()),
                    Json::Object(record) => {
                        for (key, inner) in record {
                            let words = match inner {
                                Json::String(text) => text.clone(),
                                Json::Bool(_) | Json::Number(_) => inner.to_string(),
                                Json::Null | Json::Array(_) | Json::Object(_) => continue,
                            };
                            item.lists
                                .entry(format!("{name}.{key}"))
                                .or_default()
                                .push(words);
                        }
                    }
                    Json::Null | Json::Array(_) => {}
                }
            }
            item.lists.insert(name.to_owned(), said);
        }
    }
}

/// Each field of the record `obj` into `item`, under `prefix` and its own name (`sender.email`),
/// or under its own name alone where `prefix` is empty.
fn read_fields(doc: &AutoCommit, item: &mut EarlierItem, prefix: &str, obj: &ObjId, depth: usize) {
    for key in doc.keys(obj) {
        if internal(&key) {
            continue;
        }
        if let Ok(Some((value, at))) = doc.get(obj, key.as_str()) {
            let name = if prefix.is_empty() {
                key
            } else {
                format!("{prefix}.{key}")
            };
            read_field_at(doc, item, &name, &value, &at, depth);
        }
    }
}

/// Whether `value` (at `obj`) is a record of its own: a map that is neither rich text nor bytes
/// by key, and says something in words of its own (a set keeps its members as flags, and a map
/// of other records, as a package's own resources are kept, says nothing itself).
fn record(doc: &AutoCommit, value: &AmValue<'_>, obj: &ObjId) -> bool {
    if !matches!(value, AmValue::Object(ObjType::Map | ObjType::Table))
        || words(doc, obj, "schema").as_deref() == Some(RICH_SCHEMA)
        || bytes_by_key(doc, obj).is_some()
    {
        return false;
    }
    doc.keys(obj)
        .filter(|key| !internal(key))
        .any(|key| match doc.get(obj, key.as_str()) {
            Ok(Some((AmValue::Scalar(s), _))) => {
                !matches!(s.as_ref(), ScalarValue::Boolean(_) | ScalarValue::Null)
            }
            Ok(Some((AmValue::Object(ObjType::Text), _))) => true,
            _ => false,
        })
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

/// What the map `obj` holds under each of its keys as words, when it holds some and nothing but
/// words: a set's `true`s, or a native app's times by video.
fn words_by_key(doc: &AutoCommit, obj: &ObjId) -> Option<BTreeMap<String, String>> {
    let mut held = BTreeMap::new();
    for key in doc.keys(obj) {
        let said = match doc.get(obj, key.as_str()).ok()?? {
            (AmValue::Scalar(s), _) => scalar_words(&s)?,
            (AmValue::Object(ObjType::Text), text) => doc.text(&text).ok()?,
            (AmValue::Object(_), _) => return None,
        };
        held.insert(key, said);
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

#[cfg(test)]
mod tests {
    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT};

    /// A record held in a field reads as its own fields: at the document's root as an item of
    /// its own, inside an item under the field's name. A set and the map of a package's own
    /// resources say nothing of their own, and are no records.
    #[test]
    fn a_record_in_a_field_reads_as_its_fields() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "tilo:version", 3_i64).unwrap();
        let draft = doc.put_object(ROOT, "draft", ObjType::Map).unwrap();
        doc.put(&draft, "to", "grace@hopper.example").unwrap();
        let sender = doc.put_object(&draft, "sender", ObjType::Map).unwrap();
        doc.put(&sender, "email", "sam@rivera.example").unwrap();
        let drafts = doc.put_object(ROOT, "drafts", ObjType::Map).unwrap();
        let entry = doc.put_object(&drafts, "d1", ObjType::Map).unwrap();
        doc.put(&entry, "k", "a0").unwrap();
        let fields = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        doc.put(&fields, "to", "bo@chen.example").unwrap();
        let sender = doc.put_object(&fields, "sender", ObjType::Map).unwrap();
        doc.put(&sender, "email", "work@rivera.example").unwrap();
        let set = doc.put_object(ROOT, "imported", ObjType::Map).unwrap();
        doc.put(&set, "one", true).unwrap();
        let tl = doc.put_object(ROOT, "tl", ObjType::Map).unwrap();
        let app = doc.put_object(&tl, "so.tilo.mail", ObjType::Map).unwrap();
        doc.put(&app, "x", "y").unwrap();

        let items = super::items("mailbox", &doc);
        let of = |name: &str| items.iter().find(|item| item.of == name).unwrap();
        assert_eq!(of("draft").texts["to"], "grace@hopper.example");
        assert_eq!(of("draft").texts["sender.email"], "sam@rivera.example");
        assert_eq!(of("drafts").texts["to"], "bo@chen.example");
        assert_eq!(of("drafts").texts["sender.email"], "work@rivera.example");
        assert!(
            items
                .iter()
                .all(|item| item.of != "imported" && item.of != "tl")
        );
        let own = of("");
        assert_eq!(own.lists["imported"], ["one"]);
        assert!(
            own.texts.keys().all(|key| !key.contains('.')),
            "{:?}",
            own.texts
        );
    }

    /// A collection a record keeps in a field (a skill's examples) reads as its records, oldest
    /// first by the order they stood, each record's words by its field's name; its ids stay the
    /// field's `lists` entries.
    #[test]
    fn a_collection_in_a_field_reads_as_its_records() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "tilo:version", 1_i64).unwrap();
        let skills = doc.put_object(ROOT, "skills", ObjType::Map).unwrap();
        let skill = doc.put_object(&skills, "weekly", ObjType::Map).unwrap();
        doc.put(&skill, "k", "a").unwrap();
        let fields = doc.put_object(&skill, "v", ObjType::Map).unwrap();
        let name = doc.put_object(&fields, "name", ObjType::Text).unwrap();
        doc.splice_text(&name, 0, 0, "Weekly report").unwrap();
        let examples = doc.put_object(&fields, "examples", ObjType::Map).unwrap();
        for (id, order, text, accepted, note) in [
            ("e1", "a", "Revenue rose 4 %.", true, "Exactly this"),
            ("e2", "b", "It was a week of things.", false, ""),
        ] {
            let entry = doc.put_object(&examples, id, ObjType::Map).unwrap();
            doc.put(&entry, "k", order).unwrap();
            let held = doc.put_object(&entry, "v", ObjType::Map).unwrap();
            for (field, said) in [("text", text), ("note", note)] {
                let at = doc.put_object(&held, field, ObjType::Text).unwrap();
                doc.splice_text(&at, 0, 0, said).unwrap();
            }
            doc.put(&held, "accepted", accepted).unwrap();
            doc.put(&held, "at", 1_700_000_000_000_i64).unwrap();
        }

        let items = super::items("chat.skills", &doc);
        let skill = items
            .iter()
            .find(|item| item.of == "skills")
            .expect("the skill");
        assert_eq!(skill.texts["name"], "Weekly report");
        assert_eq!(skill.lists["examples"], ["e1", "e2"]);
        let records = &skill.records["examples"];
        assert_eq!(records.len(), 2, "{records:?}");
        assert_eq!(records[0]["text"], "Revenue rose 4 %.");
        assert_eq!(records[0]["accepted"], "true");
        assert_eq!(records[0]["note"], "Exactly this");
        assert_eq!(records[0]["at"], "1700000000000");
        assert_eq!(records[1]["text"], "It was a week of things.");
        assert_eq!(records[1]["accepted"], "false");
        assert_eq!(records[1]["note"], "", "an empty field reads as no words");
        assert!(
            items.iter().all(|item| item.of != "examples"),
            "the records are the skill's, not items of their own"
        );
    }

    /// A collection a native model keys by an id of its own (`Keyed<T>`) keeps each item at its
    /// key, the item's fields there, with no order key of its own.
    #[test]
    fn a_keyed_collection_reads_as_its_items() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "tilo:version", 4_i64).unwrap();
        let channels = doc.put_object(ROOT, "channels", ObjType::Map).unwrap();
        let general = doc.put_object(&channels, "456", ObjType::Map).unwrap();
        doc.put(&general, "name", "general").unwrap();
        let lounge = doc.put_object(&channels, "789", ObjType::Map).unwrap();
        doc.put(&lounge, "name", "Lounge").unwrap();
        let links = doc.put_object(ROOT, "links", ObjType::Map).unwrap();
        let link = doc.put_object(&links, "3001", ObjType::Map).unwrap();
        doc.put(&link, "person", "ada").unwrap();

        let items = super::items("guild", &doc);
        let of = |name: &str| {
            items
                .iter()
                .filter(|item| item.of == name)
                .collect::<Vec<_>>()
        };
        let channels = of("channels");
        assert_eq!(channels.len(), 2, "each channel is an item: {channels:?}");
        assert_eq!(channels[0].id, "456");
        assert_eq!(channels[0].texts["name"], "general");
        assert_eq!(channels[1].id, "789");
        let links = of("links");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].id, "3001");
        assert_eq!(links[0].texts["person"], "ada");
    }
}
