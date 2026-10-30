//! Reflection, indexed: who made each resource and when, and who changed it last and when
//! (`created(r)`, `updated(r)`; language-changes.md, "Reflection"), kept per resource beside its
//! fields and derived change by change from the room record's log.
//!
//! The record holds what each resource is now; the log holds who made each change, when, and
//! which resources it changed ([`DynAction::changed`]). The index keeps, per resource, the
//! first and the last of those versions under the pseudo-fields [`CREATED`] and [`UPDATED`],
//! and per collection a column of each, ordered by the time: so a list sorts or filters by
//! them by reading one page, never the log or every item.
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

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde_json::{Value as Json, json};
use tilolang::pred::Lit;
use tilolang_runtime::store::{FieldValue, Reflected, Scalar, TaskRun, Version};

use crate::action::{ACTION_TYPE, DynAction};
use crate::schema::{FieldSchema, MergeKind};

/// The pseudo-field holding the first version of a resource: who made it, when, how.
pub const CREATED: &str = Reflected::CREATED;
/// The pseudo-field holding the last version of a resource.
pub const UPDATED: &str = Reflected::UPDATED;

static FIELDS: LazyLock<[FieldSchema; 2]> = LazyLock::new(|| {
    [
        FieldSchema::new(CREATED, MergeKind::Register).composite(),
        FieldSchema::new(UPDATED, MergeKind::Register).composite(),
    ]
});

/// Whether `field` names one of the versions the index keeps of every resource.
#[must_use]
pub fn is_reflected(field: &str) -> bool {
    field == CREATED || field == UPDATED
}

/// The schema of the pseudo-field `id`, when it is one of the versions.
#[must_use]
pub fn field(id: &str) -> Option<&'static FieldSchema> {
    FIELDS.iter().find(|f| f.id == id)
}

/// One version of a resource as the index keeps it: the runtime's [`Version`] and the change
/// that made it, which orders two versions recorded at the same time.
#[derive(Clone, Debug, PartialEq, Eq)]
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
            Some(task) => Some(serde_json::from_value(task.clone()).ok()?),
        };
        let change = text("change").unwrap_or_default();
        Some(Self {
            version: Version {
                who: text("who")?,
                at: text("at")?,
                action: text("action")?,
                task,
                change: change.clone(),
                ..Version::default()
            },
            change,
            op: json.get("op").and_then(Json::as_u64)?,
        })
    }
}

/// The action a step on the record that is no package's action is a version by (an undo, a
/// revert from History).
pub const STEP_ACTION: &str = "$history";

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
    action
        .made()
        .into_iter()
        .map(|made| Version {
            task: task.clone(),
            kind: made.kind,
            fields: made.fields,
            resource: made.resource,
            ..stamp.version.clone()
        })
        .collect()
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
            ..Version::default()
        },
        change: hash,
        op: change.start_op().get(),
    };
    Some((stamp, action, task))
}

/// The first and last versions `changes` made of each resource they changed.
#[must_use]
pub fn ends_of<'c>(
    changes: impl IntoIterator<Item = &'c automerge::Change>,
) -> BTreeMap<String, (Stamp, Stamp)> {
    let mut out: BTreeMap<String, (Stamp, Stamp)> = BTreeMap::new();
    for change in changes {
        let Some((stamp, resources)) = stamp_of(change) else {
            continue;
        };
        for resource in resources {
            match out.get_mut(&resource) {
                Some((first, last)) => {
                    if stamp.before(first) {
                        *first = stamp.clone();
                    }
                    if last.before(&stamp) {
                        *last = stamp.clone();
                    }
                }
                None => {
                    out.insert(resource, (stamp.clone(), stamp.clone()));
                }
            }
        }
    }
    out
}
