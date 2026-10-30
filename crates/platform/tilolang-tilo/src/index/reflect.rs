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
//! changes in: versions compare by their recorded time, then by the change's hash. Two replicas
//! holding the same changes hold the same `created` and `updated`.
//!
//! A resource's versions stay when the resource is derived anew (its fields are read from the
//! record again; its versions are not in the record) and when it is removed: an undone removal
//! brings back the resource it was, made when it was made.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde_json::{Value as Json, json};
use tilolang::pred::Lit;
use tilolang_runtime::store::{FieldValue, Reflected, Scalar, Version};

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
}

impl Stamp {
    fn order(&self) -> (&str, &str) {
        (&self.version.at, &self.change)
    }

    /// Whether this version comes before `other`.
    #[must_use]
    pub fn before(&self, other: &Self) -> bool {
        self.order() < other.order()
    }

    /// The stamp as the index holds it.
    #[must_use]
    pub fn value(&self) -> FieldValue {
        FieldValue::Scalar(Scalar::Json(json!({
            "who": self.version.who,
            "at": self.version.at,
            "action": self.version.action,
            "change": self.change,
        })))
    }

    /// The stamp the index holds as `value`.
    #[must_use]
    pub fn of_value(value: &FieldValue) -> Option<Self> {
        let FieldValue::Scalar(Scalar::Json(json)) = value else {
            return None;
        };
        let text = |key: &str| json.get(key).and_then(Json::as_str).map(str::to_owned);
        Some(Self {
            version: Version {
                who: text("who")?,
                at: text("at")?,
                action: text("action")?,
            },
            change: text("change").unwrap_or_default(),
        })
    }
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
    let stamp = Stamp {
        version: Version {
            who: action.authorship.invoker.clone(),
            at,
            action: action.action_id.clone(),
        },
        change: change.hash().to_string(),
    };
    Some((stamp, action.changed()))
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
