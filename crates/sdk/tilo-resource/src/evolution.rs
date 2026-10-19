//! How a resource's storage changes between versions of its app.
//!
//! A record stores the schema version it was written at
//! ([`stored_version`]); `#[resource(version = N, migrate = path)]` declares
//! the current one and how to bring an older document up to it. A document
//! that does not read as the model is [`Unreadable`]: views show why and
//! actions report it, and stale state is never shown as current.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ObjId, ROOT, ReadDoc, ScalarValue, Value};
use tilo_automerge::ApplyIncoming;

use crate::model::{Doc, LocatedAt as _, ModelError};
use crate::resource::Resource;

/// The root key a record's schema version is stored under, when it is past
/// the first.
pub const VERSION_KEY: &str = "tilo:version";

/// The schema version `doc` was written at: 1 when it names none.
#[must_use]
pub fn stored_version(doc: &Doc) -> u32 {
    match doc.get(ROOT, VERSION_KEY) {
        Ok(Some((Value::Scalar(scalar), _))) => match scalar.as_ref() {
            ScalarValue::Uint(version) => u32::try_from(*version).unwrap_or(u32::MAX),
            ScalarValue::Int(version) => u32::try_from(*version).unwrap_or(u32::MAX),
            _ => 1,
        },
        _ => 1,
    }
}

/// Record that `doc` is at `version` (nothing for the first version, so
/// documents of apps that never changed their storage stay as they are).
///
/// # Errors
/// Returns the document's write error.
pub fn write_version(doc: &mut Doc, version: u32) -> Result<(), ModelError> {
    if version > 1 || doc.get(ROOT, VERSION_KEY)?.is_some() {
        doc.put(ROOT, VERSION_KEY, ScalarValue::Uint(u64::from(version)))?;
    }
    Ok(())
}

/// Whether `doc` holds `R` at the version the app reads.
///
/// # Errors
/// Returns why not: written by a newer version of the app, or waiting for
/// [`migrate`].
pub fn check_version<R: Resource>(doc: &Doc) -> Result<(), ModelError> {
    check_version_at(R::VERSION, doc)
}

/// [`check_version`] of a resource read at `version`.
fn check_version_at(version: u32, doc: &Doc) -> Result<(), ModelError> {
    let stored = stored_version(doc);
    match stored.cmp(&version) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Greater => Err(ModelError::invalid(format!(
            "written by a newer version of the app (storage version {stored}, this app reads {version})"
        ))),
        std::cmp::Ordering::Less => Err(ModelError::invalid(format!(
            "stored at version {stored}; it is brought up to version {version} when it opens"
        ))),
    }
}

/// Bring `doc` up to `R`'s version with its migration: once, as one
/// housekeeping change made by an actor and at a time derived from the
/// resource kind, the versions and the heads it starts from, so two
/// devices migrating the same document produce the same change. Returns
/// whether it changed anything.
///
/// # Errors
/// Returns the migration's error; `doc` is left as it was.
pub fn migrate<R: Resource>(doc: &mut Doc) -> Result<bool, ModelError> {
    migrate_with(
        (R::KIND, R::LABEL, R::VERSION),
        &|doc: &mut Doc, from: u32| R::migrate(doc, from),
        doc,
    )
}

/// [`migrate`] of the resource of the kind, label and storage version
/// `resource`, whose migration is `migration`.
fn migrate_with(
    (kind, label, to): (&str, &str, u32),
    migration: &dyn Fn(&mut Doc, u32) -> Result<(), ModelError>,
    doc: &mut Doc,
) -> Result<bool, ModelError> {
    let from = stored_version(doc);
    if from >= to {
        return Ok(false);
    }
    let mut heads = doc.get_heads();
    heads.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-resource/migration/v1");
    hasher.update(kind.as_bytes());
    hasher.update(&from.to_le_bytes());
    hasher.update(&to.to_le_bytes());
    for head in &heads {
        hasher.update(&head.0);
    }
    let actor =
        automerge::ActorId::from(hasher.finalize().as_bytes().get(..16).unwrap_or_default());
    let mut migrated = doc.fork_at(&heads)?.with_actor(actor);
    migration(&mut migrated, from)?;
    migrated.put(ROOT, VERSION_KEY, ScalarValue::Uint(u64::from(to)))?;
    let context = tilo_actions::ActionContext {
        change: tilo_actions::ChangeKind::Housekeeping,
        ..tilo_actions::ActionContext::for_resource(kind, kind, label).with_described(
            tilo_actions::Described::new(tilo_actions::Did::history(
                tilo_actions::HistoryOp::Migrate { to },
                0,
                None,
            )),
        )
    };
    let mut options = CommitOptions::default().with_time(0);
    if let Some(message) = context.change_message() {
        options = options.with_message(message);
    }
    migrated.commit_with(options);
    let changes = migrated.get_changes(&heads);
    doc.apply_incoming(changes)?;
    Ok(true)
}

/// The value stored under `key` of the map `obj` as one JSON string (an old
/// whole-value field), read with `T`'s serde form: `None` when the key holds
/// no JSON string (absent, or already an object), `Some(None)` for JSON
/// `null`. What a migration reads an old field with when its new type is not
/// the old one's serde form.
///
/// # Errors
/// Returns why the text does not read as `T`, or the document's error.
pub fn read_json<T>(doc: &Doc, obj: &ObjId, key: &str) -> Result<Option<Option<T>>, ModelError>
where
    T: serde::de::DeserializeOwned,
{
    let text = match doc.get(obj, key)? {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Str(text) => text.to_string(),
            ScalarValue::Null => return Ok(Some(None)),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    if text == "null" {
        return Ok(Some(None));
    }
    crate::__private::value_of_bytes(text.as_bytes())
        .and_then(serde_json::from_value)
        .map(|value| Some(Some(value)))
        .map_err(|error| ModelError::invalid(error.to_string()).at(key))
}

/// Rewrite the value stored under `key` of the map `obj` as one JSON string
/// (an old whole-value field, last writer wins) as the model type `T` stores
/// it: the migration of a field whose type became a real model type. `T`
/// reads the old text with its serde form, which is the form the text was
/// written with, so `T` keeps that form (its serde attributes) unchanged.
/// A key holding no JSON string (absent, or already an object) is left as
/// it is; JSON `null` stores `T`'s empty value. Returns whether it rewrote
/// the key.
///
/// # Errors
/// Returns why the text does not read as `T`, or the document's error.
pub fn retype_json<T>(doc: &mut Doc, obj: &ObjId, key: &str) -> Result<bool, ModelError>
where
    T: crate::Model + serde::de::DeserializeOwned,
{
    let Some(read) = read_json::<T>(doc, obj, key)? else {
        return Ok(false);
    };
    let value = match read {
        Some(value) => value,
        None => T::missing().map_err(|error| error.at(key))?,
    };
    // A scalar is replaced in place; the value's own store writes the shape.
    doc.delete(obj, key)?;
    crate::model::write_key(doc, obj, key, &value)?;
    Ok(true)
}

/// [`retype_json`] for every item of the [`Collection`](crate::Collection)
/// stored under `key` of the map `obj`: the migration of a collection whose
/// item type was stored as one JSON string. Returns how many items it
/// rewrote.
///
/// # Errors
/// Returns why an item's text does not read as `T`, or the document's error.
pub fn retype_json_items<T>(doc: &mut Doc, obj: &ObjId, key: &str) -> Result<usize, ModelError>
where
    T: crate::Model + serde::de::DeserializeOwned,
{
    let Some((Value::Object(automerge::ObjType::Map), items)) = doc.get(obj, key)? else {
        return Ok(0);
    };
    let mut rewrote: usize = 0;
    for id in doc.keys(&items).collect::<Vec<_>>() {
        if let Some((Value::Object(automerge::ObjType::Map), entry)) =
            doc.get(&items, id.as_str())?
            && retype_json::<T>(doc, &entry, crate::collection::VALUE).located(&id)?
        {
            rewrote = rewrote.saturating_add(1);
        }
    }
    Ok(rewrote)
}

/// Write the keys of the JSON object `shape` into the map `obj` as an older
/// version of an app stored them: what a migration's test builds the
/// document it migrates from. An object is a map, an array a list, a string,
/// a boolean and a number are scalars (an integer as `Int`), `null` is a
/// null scalar, and `{"$text": "…"}` is a text object holding the string.
///
/// # Errors
/// Returns the document's write error, or that `shape` is no object.
pub fn fixture(doc: &mut Doc, obj: &ObjId, shape: &serde_json::Value) -> Result<(), ModelError> {
    let serde_json::Value::Object(fields) = shape else {
        return Err(ModelError::shape("an object of fields"));
    };
    for (key, value) in fields {
        fixture_value(doc, crate::Slot::Key(obj, key), value)?;
    }
    Ok(())
}

fn fixture_value(
    doc: &mut Doc,
    slot: crate::Slot<'_>,
    value: &serde_json::Value,
) -> Result<(), ModelError> {
    use automerge::ObjType;
    use serde_json::Value as Json;
    match value {
        Json::Object(fields) if fields.len() == 1 && fields.contains_key("$text") => {
            let Some(Json::String(text)) = fields.get("$text") else {
                return Err(ModelError::shape("a text's string"));
            };
            let obj = slot.object(doc, ObjType::Text)?;
            crate::TextEdit::ReplaceText { text: text.clone() }
                .apply_under(doc, &obj)
                .map_err(|error| ModelError::invalid(error.to_string()))
        }
        Json::Object(_) => {
            let obj = slot.object(doc, ObjType::Map)?;
            fixture(doc, &obj, value)
        }
        Json::Array(items) => {
            let obj = slot.object(doc, ObjType::List)?;
            for (index, item) in items.iter().enumerate() {
                fixture_value(doc, crate::Slot::Insert(&obj, index), item)?;
            }
            Ok(())
        }
        Json::String(text) => slot.put_scalar(doc, ScalarValue::Str(text.as_str().into())),
        Json::Bool(flag) => slot.put_scalar(doc, ScalarValue::Boolean(*flag)),
        Json::Number(number) => slot.put_scalar(
            doc,
            match (number.as_i64(), number.as_u64(), number.as_f64()) {
                (Some(int), _, _) => ScalarValue::Int(int),
                (None, Some(uint), _) => ScalarValue::Uint(uint),
                (None, None, Some(float)) => ScalarValue::F64(float),
                (None, None, None) => return Err(ModelError::shape("a number")),
            },
        ),
        Json::Null => slot.put_scalar(doc, ScalarValue::Null),
    }
}

/// The map stored under `key` of `obj`, when it holds one: a nested record
/// or a present `Option` of one, which a migration rewrites inside.
///
/// # Errors
/// Returns the document's read error.
pub fn map_at(doc: &Doc, obj: &ObjId, key: &str) -> Result<Option<ObjId>, ModelError> {
    Ok(match doc.get(obj, key)? {
        Some((Value::Object(automerge::ObjType::Map), map)) => Some(map),
        _ => None,
    })
}

/// The value maps of the items of the [`Collection`](crate::Collection)
/// stored under `key` of the map `obj`, in no particular order: what a
/// migration walks to rewrite a field of every item.
///
/// # Errors
/// Returns the document's read error.
pub fn collection_items(doc: &Doc, obj: &ObjId, key: &str) -> Result<Vec<ObjId>, ModelError> {
    let Some((Value::Object(automerge::ObjType::Map), items)) = doc.get(obj, key)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for id in doc.keys(&items).collect::<Vec<_>>() {
        if let Some((Value::Object(automerge::ObjType::Map), item)) =
            doc.get(&items, id.as_str())?
            && let Some((Value::Object(automerge::ObjType::Map), value)) =
                doc.get(&item, crate::collection::VALUE)?
        {
            out.push(value);
        }
    }
    Ok(out)
}

/// The maps stored as the values of the map under `key` of `obj` (a
/// `BTreeMap<String, T>` or [`Map`](crate::Map) of records), by key.
///
/// # Errors
/// Returns the document's read error.
pub fn map_values(doc: &Doc, obj: &ObjId, key: &str) -> Result<Vec<(String, ObjId)>, ModelError> {
    let Some((Value::Object(automerge::ObjType::Map), map)) = doc.get(obj, key)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in doc.keys(&map).collect::<Vec<_>>() {
        if let Some((Value::Object(automerge::ObjType::Map), value)) =
            doc.get(&map, entry.as_str())?
        {
            out.push((entry, value));
        }
    }
    Ok(out)
}

/// A document that does not read as its resource: what a view shows in
/// place of stale state, and why each action is unavailable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable {
    /// Why, in the words of the read: where it failed and what it expected.
    pub reason: String,
    /// The storage version the document names.
    pub stored: u32,
    /// The version the app reads.
    pub expected: u32,
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl Unreadable {
    /// Why `doc` did not read as `R`, from the read's `error`.
    #[must_use]
    pub fn of<R: Resource>(doc: &Doc, error: &ModelError) -> Self {
        Self {
            reason: error.to_string(),
            stored: stored_version(doc),
            expected: R::VERSION,
        }
    }
}

/// Read `R` from `doc`, or say why it does not read.
///
/// # Errors
/// Returns the [`Unreadable`] state.
pub fn read_checked<R: Resource>(doc: &Doc) -> Result<R::State, Unreadable> {
    check_version::<R>(doc)
        .and_then(|()| R::read(doc))
        .map_err(|error| Unreadable::of::<R>(doc, &error))
}
