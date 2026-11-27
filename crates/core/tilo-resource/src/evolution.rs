//! How a resource's storage changes between versions of its app.
//!
//! A record stores the schema version it was written at under one key
//! ([`VERSION_KEY`], read by [`stored_version`]); `#[resource(version = N,
//! migrate = path)]` declares the current one and how to bring an older
//! document up to it ([`rewrite`], after its derived steps). A document that does not read as the
//! model is [`Unreadable`]: views show why and actions report it, and stale
//! state is never shown as current.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ObjId, ROOT, ReadDoc, ScalarValue, Value};
use tilo_automerge::ApplyIncoming;
use tilo_sort::CompactSort as _;

use crate::model::{Doc, ModelError};
use crate::resource::Resource;

/// The root key a document's schema version is stored under, when it is past
/// the first. Every document keeps it here: an app's record, a settings
/// document, a `tilo-doc` page.
pub const VERSION_KEY: &str = "tilo:version";

/// The `u32` a stored version scalar names.
fn version_of(value: &Value<'_>) -> Option<u32> {
    let Value::Scalar(scalar) = value else {
        return None;
    };
    match scalar.as_ref() {
        ScalarValue::Uint(version) => Some(u32::try_from(*version).unwrap_or(u32::MAX)),
        ScalarValue::Int(version) => Some(u32::try_from(*version).unwrap_or(u32::MAX)),
        _ => None,
    }
}

/// The schema version `doc` was written at: 1 when it names none.
#[must_use]
pub fn stored_version(doc: &Doc) -> u32 {
    doc.get(&ROOT, VERSION_KEY)
        .ok()
        .flatten()
        .and_then(|(value, _)| version_of(&value))
        .unwrap_or(1)
}

/// Record that `doc` is at `version` (nothing for the first version, so
/// documents of apps that never changed their storage stay as they are).
pub fn write_version(doc: &mut Doc, version: u32) -> Result<(), ModelError> {
    write_version_at(doc, &ROOT, version)
}

/// [`write_version`] of the document whose root is `at`.
pub fn write_version_at(doc: &mut Doc, at: &ObjId, version: u32) -> Result<(), ModelError> {
    if version > 1 || doc.get(at, VERSION_KEY)?.is_some() {
        doc.put(at, VERSION_KEY, ScalarValue::Uint(u64::from(version)))?;
    }
    Ok(())
}

/// The step that brings the record `scope` names to `version`: the change every holder makes alike,
/// under an actor derived from the record, its workspace and the version alone, at time zero,
/// writing the version key and nothing else. `before` holds exactly the link it follows, the
/// record's founding for the first version and the step to the version before otherwise, so the
/// chain a record stands on is one every holder derives ([`crate::founding::chain`]), however its
/// content went on meanwhile.
///
/// # Errors
/// When `before` holds no heads to step from, or the step cannot be committed.
pub fn step(
    scope: crate::founding::Scope<'_>,
    version: u32,
    before: &Doc,
) -> Result<automerge::Change, automerge::AutomergeError> {
    let crate::founding::Scope {
        workspace,
        kind,
        id,
    } = scope;
    let mut doc = before.clone().with_actor(crate::genesis::actor(
        id,
        kind,
        &format!("version {version} in {workspace}"),
    ));
    doc.put(ROOT, VERSION_KEY, ScalarValue::Uint(u64::from(version)))?;
    let context = tilo_actions::ActionContext {
        change: tilo_actions::ChangeKind::Housekeeping,
        ..tilo_actions::ActionContext::for_resource(id, kind, kind).with_described(
            tilo_actions::Described::new(tilo_actions::Did::history(
                tilo_actions::HistoryOp::Migrate {
                    to: version,
                    rewrite: false,
                },
                0,
                None,
            )),
        )
    };
    let mut options = CommitOptions::default().with_time(0);
    if let Some(message) = context.entry_in(&doc) {
        options = options.with_message(message);
    }
    let hash = doc
        .commit_with(options)
        .ok_or(automerge::AutomergeError::Fail)?;
    doc.get_change_by_hash(&hash)
        .ok_or(automerge::AutomergeError::MissingHash(hash))
}

/// Whether `doc` holds `R` at a version this build reads: its own or an older one, whose chain
/// of steps stops short and is brought up before it opens. A newer version only adds fields and
/// containers to a type, so this build's fields are all still there and it reads them as far as
/// it knows the record.
///
/// # Errors
/// Returns why not: the record waits for its steps to be brought up.
pub fn check_version<R: Resource>(doc: &Doc) -> Result<(), ModelError> {
    let (stored, version) = (stored_version(doc), R::VERSION);
    if stored < version {
        return Err(ModelError::invalid(format!(
            "stored at version {stored}; it is brought up to version {version} when it opens"
        )));
    }
    Ok(())
}

/// Whether `doc` is older than `R`: its chain of steps stops short of `R`'s version, so it is
/// brought up ([`crate::founding::bring_up`]) before it is read.
#[must_use]
pub fn needs_migrate<R: Resource>(doc: &Doc) -> bool {
    stored_version(doc) < R::VERSION
}

/// The app's own rewrite of the record `scope` names for `version`, from the version `from` it
/// stood at, by `migration` (its `#[resource(migrate = ..)]`): once, as one housekeeping change
/// after the record's step to `version`, made by an actor and at a time derived from the record,
/// its workspace, the version and the heads it starts from, so two holders rewriting the same
/// history make the same change. It is decided as any write of the record's. A rewrite rewrites by
/// key and never inserts list elements, so two holders rewriting apart converge on one item each.
/// Whether it changed anything.
///
/// # Errors
/// Returns the migration's error; `doc` is left as it was.
pub fn rewrite(
    (scope, version): (crate::founding::Scope<'_>, u32),
    from: u32,
    migration: &dyn Fn(&mut Doc, u32) -> Result<(), ModelError>,
    doc: &mut Doc,
) -> Result<bool, ModelError> {
    let crate::founding::Scope {
        workspace,
        kind,
        id,
    } = scope;
    let mut heads = doc.get_heads();
    heads.compact_sort();
    let mut on: Vec<String> = heads.iter().map(ToString::to_string).collect();
    on.sort();
    let actor = crate::genesis::actor(
        id,
        kind,
        &format!(
            "rewrite to version {version} on {} in {workspace}",
            on.join(",")
        ),
    );
    let mut migrated = doc.fork_at(&heads)?.with_actor(actor);
    if from < version {
        migration(&mut migrated, from)?;
    }
    // A record whose version key was written beside its chain (by an initializer, or a migration
    // made before its steps were derived) reads that write against the step's: the rewrite,
    // which follows both, says the version once.
    if stored_version(&migrated) < version {
        migrated.put(ROOT, VERSION_KEY, ScalarValue::Uint(u64::from(version)))?;
    }
    if migrated.pending_ops() == 0 {
        return Ok(false);
    }
    let context = tilo_actions::ActionContext {
        change: tilo_actions::ChangeKind::Housekeeping,
        ..tilo_actions::ActionContext::for_resource(id, kind, kind).with_described(
            tilo_actions::Described::new(tilo_actions::Did::history(
                tilo_actions::HistoryOp::Migrate {
                    to: version,
                    rewrite: true,
                },
                0,
                None,
            )),
        )
    };
    let mut options = CommitOptions::default().with_time(0);
    if let Some(message) = context.entry_in(&migrated) {
        options = options.with_message(message);
    }
    migrated.commit_with(options);
    let changes = migrated.get_changes(&heads);
    doc.apply_incoming(changes)?;
    Ok(true)
}

/// Write what the record `from` holds into the record `to`, another record of
/// the same kind and storage version kept elsewhere: what a record moving
/// from one store to another takes along. Every field is copied as it is
/// stored, scalars with their types, maps, lists and texts; the record's own
/// identity (its `tilo:` keys other than [`VERSION_KEY`]) stays `to`'s. A
/// map or text `to` already holds at a key is written into rather than
/// replaced, so what another writer adds to it concurrently is kept.
///
/// # Errors
/// Returns the document's read or write error.
pub fn copy_into(from: &Doc, to: &mut Doc) -> Result<(), ModelError> {
    for key in from.keys(&ROOT).collect::<Vec<_>>() {
        if key.starts_with("tilo:") && key != VERSION_KEY {
            continue;
        }
        if let Some((value, obj)) = from.get(&ROOT, key.as_str())? {
            copy_value(from, &value, &obj, to, &ROOT, Place::Key(&key))?;
        }
    }
    Ok(())
}

/// Where [`copy_value`] writes: under a key of a map, or at an index of a
/// list.
enum Place<'a> {
    Key(&'a str),
    Insert(usize),
}

fn copy_value(
    from: &Doc,
    value: &Value<'_>,
    obj: &ObjId,
    to: &mut Doc,
    into: &ObjId,
    place: Place<'_>,
) -> Result<(), ModelError> {
    use automerge::ObjType;
    let Value::Object(kind) = value else {
        let Value::Scalar(scalar) = value else {
            return Ok(());
        };
        let scalar = scalar.as_ref().clone();
        match place {
            Place::Key(key) => to.put(into, key, scalar)?,
            Place::Insert(index) => to.insert(into, index, scalar)?,
        }
        return Ok(());
    };
    let kind = match kind {
        ObjType::Table => ObjType::Map,
        other => *other,
    };
    let held = match place {
        Place::Key(key) => match to.get(into, key)? {
            Some((Value::Object(held), id)) if held == kind => Some(id),
            _ => None,
        },
        Place::Insert(_) => None,
    };
    let target = match (held, place) {
        (Some(id), _) => id,
        (None, Place::Key(key)) => to.put_object(into, key, kind)?,
        (None, Place::Insert(index)) => to.insert_object(into, index, kind)?,
    };
    match kind {
        ObjType::Text => {
            let text = from.text(obj)?;
            let length = to.length(&target);
            to.splice_text(
                &target,
                0,
                isize::try_from(length).unwrap_or(isize::MAX),
                &text,
            )?;
        }
        ObjType::List => {
            let start = to.length(&target);
            for index in 0..from.length(obj) {
                if let Some((item, item_obj)) = from.get(obj, index)? {
                    copy_value(
                        from,
                        &item,
                        &item_obj,
                        to,
                        &target,
                        Place::Insert(start.saturating_add(index)),
                    )?;
                }
            }
        }
        ObjType::Map | ObjType::Table => {
            for key in from.keys(obj).collect::<Vec<_>>() {
                if let Some((item, item_obj)) = from.get(obj, key.as_str())? {
                    copy_value(from, &item, &item_obj, to, &target, Place::Key(&key))?;
                }
            }
        }
    }
    Ok(())
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
