//! Reflection, indexed: who made each resource and when, who changed it last and when, and
//! when each of its fields last changed (`created(r)`, `updated(r)`, `changed(r, .field)`;
//! language-changes.md, "Reflection"), kept per resource beside its fields and derived change
//! by change from the workspace record's log.
//!
//! The record holds what each resource is now; the log holds who made each change, when, and
//! which resources it changed, and which fields of each ([`DynAction::made`]). The index
//! keeps, per resource, the first and the last of those versions under the pseudo-fields
//! [`CREATED`] and [`UPDATED`], the last change of each field under [`changed_field`], and per
//! collection a column of the ends, ordered by the time: so a list sorts or filters by when a
//! resource or one of its fields was last changed by reading one page, never the log or every
//! item.
//!
//! Which version is first and last does not depend on the order a replica received the
//! changes in: versions compare by their recorded time, then by the change's place in the
//! record's causal order (its first operation's counter, a Lamport clock: a change comes after
//! every change it was made on top of, whatever their times' precision), then by the change's
//! hash. Two replicas holding the same changes hold the same `created` and `updated`.
//!
//! A resource's versions stay when the resource is derived anew (its fields are read from the
//! record again; its versions are not in the record) and when it is removed: an undone removal
//! brings back the resource it was, made when it was made.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use automerge::ReadDoc as _;
use serde_json::{Value as Json, json};
use tilolang::pred::Lit;
use tilolang_runtime::store::{FieldValue, Reflected, Scalar, TaskRun, Version, VersionKind};

use tilo_resource::Doc;
use tilolang_runtime::ids::{AppId, DocId, TypeId};

use crate::action::{ACTION_TYPE, DynAction, DynEdit};
use crate::schema::{FieldSchema, MergeKind};

/// The pseudo-field holding the first version of a resource: who made it, when, how.
pub const CREATED: &str = Reflected::CREATED;
/// The pseudo-field holding the last version of a resource.
pub const UPDATED: &str = Reflected::UPDATED;
/// What every pseudo-field holding when a field of a resource last changed begins with; the
/// field's id follows, so `$changed.done` holds when `done` was last written.
pub const CHANGED: &str = tilolang_runtime::store::CHANGED;

/// The pseudo-field holding the last version that changed the field `field` of a resource.
#[must_use]
pub fn changed_field(field: &str) -> String {
    tilolang_runtime::store::changed_field(field)
}

/// The field whose last change the pseudo-field `id` holds, when it holds one.
#[must_use]
pub fn field_of_changed(id: &str) -> Option<&str> {
    tilolang_runtime::store::field_of_changed(id)
}

static FIELDS: LazyLock<[FieldSchema; 2]> = LazyLock::new(|| {
    [
        FieldSchema::new(CREATED, MergeKind::Register).composite(),
        FieldSchema::new(UPDATED, MergeKind::Register).composite(),
    ]
});

/// Whether `field` names one of the versions the index keeps of every resource.
#[must_use]
pub fn is_reflected(field: &str) -> bool {
    Reflected::of(field).is_some()
}

/// The schema of the pseudo-field `id`, when it is one of the ends the index keeps of every
/// resource.
#[must_use]
pub fn field(id: &str) -> Option<&'static FieldSchema> {
    FIELDS.iter().find(|f| f.id == id)
}

/// One version of a resource as the index keeps it: the runtime's [`Version`] and the change
/// that made it, which orders two versions recorded at the same time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub version: Version,
    /// The change's hash, as hex.
    pub change: String,
    /// The counter of the change's first operation: a change made on top of another counts
    /// past every operation of it, so of two versions recorded in the same second the later
    /// change is the later version. [`PROVISIONAL`] for an action this device has not
    /// committed yet.
    pub op: u64,
}

/// The causal place of an action this device invoked and has not committed yet: after every
/// change it holds, as the action was invoked on top of them.
pub const PROVISIONAL: u64 = u64::MAX;

impl Stamp {
    fn order(&self) -> (&str, u64, &str) {
        (&self.version.at, self.op, &self.change)
    }

    /// Whether this version comes before `other`.
    #[must_use]
    pub fn before(&self, other: &Self) -> bool {
        self.order() < other.order()
    }

    /// The stamp as the index holds it.
    #[must_use]
    pub fn value(&self) -> FieldValue {
        let mut held = json!({
            "who": self.version.who,
            "at": self.version.at,
            "action": self.version.action,
            "change": self.change,
            "op": self.op,
        });
        if let (Some(task), Some(map)) = (&self.version.task, held.as_object_mut()) {
            map.insert(
                "task".to_owned(),
                serde_json::to_value(task).unwrap_or(Json::Null),
            );
        }
        if let (Some(from), Some(map)) = (&self.version.from, held.as_object_mut()) {
            map.insert(
                "from".to_owned(),
                serde_json::to_value(from).unwrap_or(Json::Null),
            );
        }
        if let (Some(who), Some(map)) = (&self.version.suggested_by, held.as_object_mut()) {
            map.insert("suggested_by".to_owned(), Json::String(who.clone()));
        }
        FieldValue::Scalar(Scalar::Json(held))
    }

    /// The stamp the index holds as `value`.
    #[must_use]
    pub fn of_value(value: &FieldValue) -> Option<Self> {
        let FieldValue::Scalar(Scalar::Json(json)) = value else {
            return None;
        };
        let text = |key: &str| json.get(key).and_then(Json::as_str).map(str::to_owned);
        // An index written before versions named their run holds none.
        let task = match json.get("task") {
            None => None,
            Some(task) => match serde_json::from_value(task.clone()) {
                Ok(task) => Some(task),
                Err(error) => {
                    tracing::warn!(%error, "the index holds a version whose run does not decode");
                    return None;
                }
            },
        };
        // An index written before versions named what they were made from holds none.
        let from = match json.get("from") {
            None | Some(Json::Null) => None,
            Some(from) => match serde_json::from_value(from.clone()) {
                Ok(from) => Some(from),
                Err(error) => {
                    tracing::warn!(%error, "the index holds a version made from something that does not decode");
                    return None;
                }
            },
        };
        let change = text("change").unwrap_or_default();
        Some(Self {
            version: Version {
                who: text("who")?,
                at: text("at")?,
                action: text("action")?,
                task,
                change: change.clone(),
                from,
                suggested_by: text("suggested_by"),
                ..Version::default()
            },
            change,
            op: json.get("op").and_then(Json::as_u64)?,
        })
    }
}

/// The action a step on the record that is no package's action is a version by (an undo, a
/// revert from History; [`tilolang_runtime::store::STEP_ACTION`]).
pub const STEP_ACTION: &str = tilolang_runtime::store::STEP_ACTION;

/// The task run a change's context names: its group, when it is a task's.
pub fn task_of(context: &tilo_actions::ActionContext) -> Option<TaskRun> {
    let group = context.group.as_ref()?;
    (group.kind == tilo_actions::context::TASK_RUN_GROUP).then(|| TaskRun {
        id: group.id.clone(),
        title: group.label.clone(),
    })
}

/// The versions `change` made, one per resource it changed, when it is a tilolang action's
/// change: who, when, by which action, in which task run, and what it did to each.
#[must_use]
pub fn versions_of(change: &automerge::Change) -> Vec<Version> {
    let Some((stamp, action, task)) = read(change) else {
        return Vec::new();
    };
    // The items it put into a collection (`DynEdit::Insert`), in the order of the edits: an
    // action that posts two messages is a version of the channel that names both, so where a
    // run of new items begins is known without reading the collection.
    let mut inserted: Vec<(String, String)> = Vec::new();
    for edit in &action.edits {
        let mut edit = edit;
        while let DynEdit::In { edit: inner, .. } = edit {
            edit = inner;
        }
        if let DynEdit::Insert { id, item, .. } = edit {
            inserted.push((id.clone(), item.clone()));
        }
    }
    action
        .made()
        .into_iter()
        .map(|made| Version {
            task: task.clone(),
            kind: made.kind,
            fields: made.fields,
            placed: items_inserted_into(&inserted, &made.resource),
            resource: made.resource,
            // What it was made from is the creation's fact: an action made from a resource
            // changes others too, and those were not made from it.
            from: (made.kind == VersionKind::Created)
                .then(|| stamp.version.from.clone())
                .flatten(),
            ..stamp.version.clone()
        })
        .collect()
}

/// The ids of the items `inserted` put into the collection of `holder`, in the order they were
/// put in.
fn items_inserted_into(inserted: &[(String, String)], holder: &str) -> Vec<String> {
    inserted
        .iter()
        .filter(|(of, _)| of == holder)
        .map(|(_, item)| item.clone())
        .collect()
}

/// Where `change` placed items, when it is a tilolang action's change
/// ([`DynAction::placed`]).
#[must_use]
pub fn placed_of(change: &automerge::Change) -> Vec<(String, String, String, Option<String>)> {
    read(change).map_or_else(Vec::new, |(_, action, _)| action.placed())
}

/// What a column of a version orders by: the time, as a column of times writes it.
#[must_use]
pub fn at_lit(value: &FieldValue) -> Lit {
    Stamp::of_value(value).map_or(Lit::Absent, |stamp| Lit::Time(stamp.version.at))
}

/// The version `change` made, and the resources it changed (as the runtime writes them), when
/// it is a tilolang action's change.
#[must_use]
pub fn stamp_of(change: &automerge::Change) -> Option<(Stamp, Vec<String>)> {
    let (stamp, action, _) = read(change)?;
    Some((stamp, action.changed()))
}

/// The version `change` made, its action and its task run, when it is a tilolang action's
/// change.
fn read(change: &automerge::Change) -> Option<(Stamp, DynAction, Option<TaskRun>)> {
    let context = change
        .message()
        .and_then(tilo_actions::ActionContext::from_change_message)?;
    let encoded = context.action.as_ref()?;
    if encoded.type_id != ACTION_TYPE {
        return None;
    }
    let action = match serde_json::from_slice::<DynAction>(&encoded.payload) {
        Ok(action) => action,
        Err(error) => {
            // A tilolang action's change whose action does not read: the change is still the
            // record's, but no version stamps it.
            tracing::warn!(
                change = %change.hash(),
                %error,
                "a tilolang action's change carries an action that does not decode"
            );
            return None;
        }
    };
    let at = action
        .authorship
        .at
        .clone()
        .unwrap_or_else(|| tilolang::calendar::time_text(change.timestamp()));
    let task = task_of(&context);
    let hash = change.hash().to_string();
    let stamp = Stamp {
        version: Version {
            who: action.authorship.invoker.clone(),
            at,
            action: action.action_id.clone(),
            task: task.clone(),
            change: hash.clone(),
            from: action.authorship.from.clone(),
            suggested_by: action.authorship.suggested_by.clone(),
            ..Version::default()
        },
        change: hash,
        op: change.start_op().get(),
    };
    Some((stamp, action, task))
}

/// The versions a resource's log holds that a view reads of it without reading the log: the
/// first and the last of them, and the last that changed each of its fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ends {
    /// The version that made it.
    pub first: Stamp,
    /// The version that last changed it.
    pub last: Stamp,
    /// The last version that changed each of its fields, by the field's id: a change of the
    /// record, never a step that takes one back (an undo), which leaves the field's last
    /// change the one still in effect. A field no version here names was last changed by the
    /// one that made the resource, which set it.
    pub fields: BTreeMap<String, Stamp>,
    /// Whether these are the resource's whole log read anew because a step took a change to it
    /// back: the index takes them as they are, even where they stand before what it holds.
    pub restored: bool,
}

impl Ends {
    /// Take in the version `stamp` made of the resource, changing `fields`: the first stays
    /// unless `stamp` came before it, the last is replaced by a later one, and each field's
    /// last change is the later of the two.
    fn take(&mut self, stamp: &Stamp, fields: &[String]) {
        if stamp.before(&self.first) {
            self.first = stamp.clone();
        }
        if self.last.before(stamp) {
            self.last = stamp.clone();
        }
        for field in fields {
            match self.fields.get_mut(field) {
                Some(held) if held.before(stamp) => *held = stamp.clone(),
                Some(_) => {}
                None => {
                    self.fields.insert(field.clone(), stamp.clone());
                }
            }
        }
    }
}

/// The versions `changes` made of each resource they changed, with the last change of each of
/// its fields: a package action's (a field's last change), or a step on the record that is no
/// package's action (an undo, a revert) and changed no field, as it takes a change back
/// rather than making one. A change the record marks undone made no version.
fn versions_of_changes<'c>(
    changes: impl IntoIterator<Item = &'c automerge::Change>,
    mirror: &Doc,
    resources: &BTreeMap<DocId, (AppId, TypeId)>,
    undone: &BTreeSet<String>,
) -> BTreeMap<String, Ends> {
    let mut out: BTreeMap<String, Ends> = BTreeMap::new();
    for change in changes {
        if undone.contains(&change.hash().to_string()) {
            continue;
        }
        let (stamp, wrote) = if let Some((stamp, _)) = stamp_of(change) {
            (
                stamp,
                versions_of(change)
                    .into_iter()
                    .map(|version| (version.resource, version.fields))
                    .collect::<Vec<_>>(),
            )
        } else {
            let Some(stamp) = step_stamp(change) else {
                continue;
            };
            let written = stepped(change, mirror, resources)
                .into_iter()
                .map(|version| (version.resource, Vec::new()))
                .collect();
            (stamp, written)
        };
        for (resource, fields) in wrote {
            match out.get_mut(&resource) {
                Some(end) => end.take(&stamp, &fields),
                None => {
                    out.insert(
                        resource,
                        Ends {
                            first: stamp.clone(),
                            last: stamp.clone(),
                            fields: fields
                                .iter()
                                .map(|field| (field.clone(), stamp.clone()))
                                .collect(),
                            restored: false,
                        },
                    );
                }
            }
        }
    }
    out
}

/// The changes the record marks undone (an undo, until a redo brings it back), by hash.
fn undone_changes(mirror: &Doc) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let committed = mirror.committed_document();
    for key in committed.keys(automerge::ROOT) {
        let Some(hash) = key.strip_prefix(tilo_actions::UNDONE_KEY) else {
            continue;
        };
        let on = match committed.get(automerge::ROOT, &key) {
            Ok(Some((automerge::Value::Scalar(scalar), _))) => match scalar.as_ref() {
                automerge::ScalarValue::Boolean(on) => *on,
                automerge::ScalarValue::Int(n) => *n != 0,
                automerge::ScalarValue::Uint(n) => *n != 0,
                _ => false,
            },
            _ => false,
        };
        if on {
            out.insert(hash.to_owned());
        }
    }
    out
}

/// The first and last versions `changes` made of each resource they changed, and the last
/// change of each of its fields: a package action's, or a step on the record that is no
/// package's action (an undo, a revert, which `mirror` holds what it wrote).
///
/// A change taken back by a step (an undo) no longer names a field's last change: the
/// resource it changed reads its versions from the whole log again, marked [`Ends::restored`].
#[must_use]
pub fn ends_of<'c>(
    changes: impl IntoIterator<Item = &'c automerge::Change>,
    mirror: &Doc,
    resources: &BTreeMap<DocId, (AppId, TypeId)>,
) -> BTreeMap<String, Ends> {
    let batch: Vec<&automerge::Change> = changes.into_iter().collect();
    let steps: Vec<&automerge::Change> = batch
        .iter()
        .copied()
        .filter(|change| step_stamp(change).is_some())
        .collect();
    let mut out = versions_of_changes(batch.iter().copied(), mirror, resources, &BTreeSet::new());
    if steps.is_empty() {
        return out;
    }
    let undone = undone_changes(mirror);
    // The resources the steps wrote, and those of the changes they took back: a step that
    // undoes a removal writes nothing of the resource that comes back.
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for step in &steps {
        touched.extend(
            stepped(step, mirror, resources)
                .into_iter()
                .map(|version| version.resource),
        );
    }
    for hash in &undone {
        let Ok(hash) = hash.parse::<automerge::ChangeHash>() else {
            continue;
        };
        let Some(change) = mirror.committed_document().get_change_by_hash(&hash) else {
            continue;
        };
        match stamp_of(&change) {
            Some((_, wrote)) => touched.extend(wrote),
            None => touched.extend(
                stepped(&change, mirror, resources)
                    .into_iter()
                    .map(|version| version.resource),
            ),
        }
    }
    let whole = versions_of_changes(
        &mirror.committed_document().get_changes(&[]),
        mirror,
        resources,
        &undone,
    );
    for resource in touched {
        if let Some(end) = whole.get(&resource) {
            let mut end = end.clone();
            end.restored = true;
            out.insert(resource, end);
        }
    }
    out
}

/// The version a step on the record that is no package's action was made by (an undo, a
/// revert from History; [`STEP_ACTION`]), when `change` is one: who took it, when, and which
/// step. `None` for a package action's change, and for one that is nobody's step (an outside
/// effect settling).
#[must_use]
pub fn step_stamp(change: &automerge::Change) -> Option<Stamp> {
    let context = change
        .message()
        .and_then(tilo_actions::ActionContext::from_change_message)?;
    if context
        .action
        .as_ref()
        .is_some_and(|action| action.type_id == ACTION_TYPE)
    {
        return None;
    }
    let invoker = context.invoker.as_ref()?;
    let hash = change.hash().to_string();
    Some(Stamp {
        version: Version {
            who: invoker.actor.clone(),
            at: tilolang::calendar::time_text(change.timestamp()),
            action: STEP_ACTION.to_owned(),
            task: task_of(&context),
            change: hash.clone(),
            ..Version::default()
        },
        change: hash,
        op: change.start_op().get(),
    })
}

/// The versions a step on the record that is no action of a package made (an undo, a revert):
/// one per resource it wrote, by whoever took it, naming the fields it wrote.
#[must_use]
pub fn stepped(
    change: &automerge::Change,
    mirror: &Doc,
    resources: &BTreeMap<DocId, (AppId, TypeId)>,
) -> Vec<Version> {
    let Some(stamp) = step_stamp(change) else {
        return Vec::new();
    };
    let changed = crate::reads::changed_by(std::slice::from_ref(change), mirror, resources);
    let mut by_resource: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for key in changed.docs.into_values().flatten() {
        let fields = by_resource.entry(key.resource).or_default();
        if key.field != tilolang_runtime::store::ANY_FIELD && !fields.contains(&key.field) {
            fields.push(key.field);
        }
    }
    by_resource
        .into_iter()
        .map(|(resource, fields)| Version {
            kind: VersionKind::Changed,
            fields,
            resource,
            ..stamp.version.clone()
        })
        .collect()
}
