//! The `Store` trait: where resources live.
//!
//! The runtime knows field ids and merge kinds, never the representation. A store maps the
//! operations below onto its own documents. So that the host can keep the store while guest
//! code runs, a transaction is an owned token ([`Txn`]), the operations are methods of the
//! store, and the store speaks in mutations and field values, not documents. A store also keeps
//! run journals, so a suspended run survives a restart.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::ids::{AppId, DocId, FieldId, ItemId, RunId, TypeId};
use crate::meta::FieldKind;
use crate::registry::TypeInfo;
use crate::run::Journal;
use crate::value::ResourceRef;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid operation: {0}")]
    Invalid(String),
    #[error("store failure: {0}")]
    Backend(String),
}

/// The content of a register field. Composite runtime values travel as `Json`.
#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Json(Json),
}

/// A field of a record type: its id and how it merges.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FieldSpec {
    pub id: FieldId,
    pub kind: FieldKind,
}

/// A predicate over the items of a collection, which [`Store::listing_where`] answers (see
/// [`tilolang::pred`]).
pub type Pred = tilolang::pred::Pred<FieldRead>;

pub use tilolang::pred::Lit;

pub use crate::query::{ClockField, NextChange};

/// A field a [`Pred`] reads: a register, a counter or a text of the item, and what it holds
/// when nobody wrote it (the declared default).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FieldRead {
    pub spec: FieldSpec,
    pub absent: Lit,
}

impl FieldRead {
    /// What the field holds, `value` being what the store holds of it.
    ///
    /// # Errors
    ///
    /// `Invalid` for a stored value a predicate does not compare.
    pub fn lit(&self, value: FieldValue) -> Result<Lit, StoreError> {
        lit_of(&self.spec.id, value, &self.absent)
    }
}

/// What `value` is, as the read `read` compares it: a rich text field as the text it is written
/// as, which is what search finds it by (search.md §2), and every other field as [`FieldRead::lit`]
/// reads it.
///
/// # Errors
///
/// As [`lit_of`].
pub fn field_lit_of(read: &FieldRead, value: FieldValue) -> Result<Lit, StoreError> {
    if let FieldValue::Rich(rich) = &value {
        return Ok(Lit::Text(rich.written_text()));
    }
    read.lit(value)
}

/// What the field `id` holds, `value` being what the store holds of it and `absent` what an
/// unwritten one reads as, as a predicate compares it: a set as its members, a collection as
/// how many items it has.
///
/// # Errors
///
/// `Invalid` for a stored value a predicate does not compare.
pub fn lit_of(id: &str, value: FieldValue, absent: &Lit) -> Result<Lit, StoreError> {
    let invalid = |what: &dyn std::fmt::Debug| {
        StoreError::Invalid(format!(
            "field {id} holds {what:?}, which a filter does not compare"
        ))
    };
    match value {
        FieldValue::Absent => Ok(absent.clone()),
        FieldValue::Scalar(Scalar::Bool(b)) => Ok(Lit::Bool(b)),
        FieldValue::Scalar(Scalar::Int(i)) | FieldValue::Counter(i) => Ok(Lit::Int(i)),
        FieldValue::Scalar(Scalar::Float(f)) => Ok(Lit::Float(tilolang::pred::Float(f))),
        FieldValue::Scalar(Scalar::Text(t)) | FieldValue::Text(t) => Ok(Lit::Text(t)),
        // What rich text says depends on who reads it (its references): a filter compares none.
        FieldValue::Rich(_) => Err(invalid(&"rich text")),
        FieldValue::Scalar(Scalar::Json(j)) => Lit::of_json(&j).ok_or_else(|| invalid(&j)),
        FieldValue::Keys(keys) => Ok(Lit::Set(keys.into_iter().map(Lit::Text).collect())),
        FieldValue::Items(items) => Ok(Lit::Int(i64::try_from(items.len()).unwrap_or(i64::MAX))),
        FieldValue::Entries(entries) => Err(invalid(&entries)),
    }
}

/// How two values of the field a query orders by compare: an absent value first, text
/// without regard to case.
#[must_use]
pub fn order_cmp(a: &Lit, b: &Lit) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Lit::Absent, Lit::Absent) => Ordering::Equal,
        (Lit::Absent, _) => Ordering::Less,
        (_, Lit::Absent) => Ordering::Greater,
        (Lit::Text(x), Lit::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        _ => {
            if a.compare(tilolang::pred::CmpOp::Lt, b) == Some(true) {
                Ordering::Less
            } else if a.compare(tilolang::pred::CmpOp::Gt, b) == Some(true) {
                Ordering::Greater
            } else {
                a.cmp(b)
            }
        }
    }
}

/// How two values of the field one level of `order` names compare: the direction that level
/// asks for, and its items without a value after those with one.
#[must_use]
pub fn level_cmp(order: &crate::query::Order, a: &Lit, b: &Lit) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if order.absent_last {
        match (a, b) {
            (Lit::Absent, Lit::Absent) => return Ordering::Equal,
            (Lit::Absent, _) => return Ordering::Greater,
            (_, Lit::Absent) => return Ordering::Less,
            _ => {}
        }
    }
    let cmp = order_cmp(a, b);
    if order.descending { cmp.reverse() } else { cmp }
}

/// How the items whose keys are `a` and `b` compare in `order`, level after level, and equal
/// when every level holds them equal: what settles them then is the order they came in, a sort
/// being stable.
#[must_use]
pub fn ordered_cmp(order: &crate::query::Order, a: &[Lit], b: &[Lit]) -> std::cmp::Ordering {
    let (mut level, mut at) = (Some(order), 0usize);
    while let Some(order) = level {
        let (Some(a), Some(b)) = (a.get(at), b.get(at)) else {
            return std::cmp::Ordering::Equal;
        };
        let cmp = level_cmp(order, a, b);
        if cmp.is_ne() {
            return cmp;
        }
        level = order.then.as_deref();
        at = at.saturating_add(1);
    }
    std::cmp::Ordering::Equal
}

impl FieldRead {
    /// The field read as a filter reads it, with what an unwritten one of type `ty` (declared
    /// default `default`) holds: `None` for a field no filter reads (a map).
    #[must_use]
    pub fn of(
        id: &str,
        kind: FieldKind,
        ty: &crate::meta::Ty,
        default: Option<&Json>,
    ) -> Option<Self> {
        use crate::meta::Ty;
        let absent = match kind {
            FieldKind::Counter | FieldKind::Collection => Lit::Int(0),
            FieldKind::Text | FieldKind::RichText => Lit::Text(String::new()),
            FieldKind::Set => Lit::Set(Vec::new()),
            FieldKind::Map => return None,
            FieldKind::Register => match default {
                Some(json) => Lit::of_json(json)?,
                None => match ty {
                    Ty::Bool => Lit::Bool(false),
                    Ty::Int => Lit::Int(0),
                    Ty::Float => Lit::Float(tilolang::pred::Float(0.0)),
                    Ty::Text | Ty::Date | Ty::Time | Ty::Identifier => Lit::Text(String::new()),
                    _ => Lit::Absent,
                },
            },
        };
        Some(Self {
            spec: FieldSpec {
                id: id.to_owned(),
                kind,
            },
            absent,
        })
    }
}

impl From<String> for StoreError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}

/// What a new document or item needs to start out with: every structural field exists
/// from the first change, so concurrent first writes merge instead of conflicting.
#[derive(Clone, Debug)]
pub struct NewRecord {
    pub ty: TypeId,
    pub app: AppId,
    pub fields: Vec<FieldSpec>,
}

/// An open transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Txn(pub u64);

/// One mutation, addressed to a resource.
#[derive(Clone, Debug)]
pub enum Mutation {
    SetRegister {
        field: FieldId,
        value: Scalar,
    },
    /// Add to a counter (integer units).
    AddCounter {
        field: FieldId,
        delta: i64,
    },
    /// Replace the text; the store diffs and splices.
    SetText {
        field: FieldId,
        text: String,
    },
    /// Make the rich text read as `value`; the store splices and marks the paragraphs that
    /// differ ([`crate::rich::store_under`]).
    SetRich {
        field: FieldId,
        value: crate::rich::RichValue,
    },
    /// Insert a new item; `index` of `None` appends.
    CollInsert {
        field: FieldId,
        item: NewRecord,
        index: Option<usize>,
        /// The id the item takes, or `None` for a fresh one. A run that is run again after it
        /// waited names the id it gave before, so the change it makes is the one it was making.
        id: Option<ItemId>,
    },
    /// Remove the item, with every item in it ([`crate::place`]).
    CollRemove {
        field: FieldId,
        item: ItemId,
    },
    /// Put the item at the place `index` of this collection, among its other items: from where
    /// it is in this collection, or from another collection of the same document. It keeps its
    /// id ([`crate::place`]).
    CollMove {
        field: FieldId,
        item: ItemId,
        index: usize,
    },
    SetAdd {
        field: FieldId,
        key: String,
    },
    SetRemove {
        field: FieldId,
        key: String,
    },
    MapPut {
        field: FieldId,
        key: String,
        value: Scalar,
    },
    MapRemove {
        field: FieldId,
        key: String,
    },
    /// Queue an outside effect in the change, on the document of this resource.
    OutboxPush {
        capability: String,
        payload: Json,
        /// Held back until this moment, ISO-8601 UTC text: the device carries it out only once
        /// the clock reaches it. `None` carries it out as soon as the device reads it.
        held_until: Option<String>,
        /// What the entry is known by: queued twice under one key it is one entry. `None` gives
        /// each push an entry of its own.
        key: Option<String>,
    },
    /// Take back the outside effects queued on the document of this resource that were not
    /// settled yet, each as one change, and settle them as withdrawn: what they would have
    /// done never happens.
    OutboxWithdraw,
}

/// What a mutation produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    Done,
    Item(ItemId),
}

/// A field as stored.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    /// A register nobody has written.
    Absent,
    Scalar(Scalar),
    Counter(i64),
    Text(String),
    /// A rich text's paragraphs.
    Rich(crate::rich::RichValue),
    /// Collection items in order.
    Items(Vec<ItemId>),
    /// Set elements, sorted.
    Keys(Vec<String>),
    /// Map entries, sorted by key.
    Entries(Vec<(String, Scalar)>),
}

/// What a change records about its cause: history shows it as "you, via Alex's view".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangeNote {
    pub action: String,
    pub args: Json,
    pub invoker: String,
    /// The authorship chain, outermost first.
    pub chain: Vec<String>,
    pub module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    /// Who proposed the action, when the person invoking it accepted a suggestion: what makes
    /// the change theirs while it says who proposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_by: Option<String>,
    /// When it was made: the invocation's time, recorded like an argument (convergence.md,
    /// "Actions"); a scenario's `invoke(..., at: time)` sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// The resources it changed in the document it is a change of, as
    /// [`ResourceRef::id_string`] writes them: what reflection (`created(r)`, `history(r)`)
    /// reads a resource's versions from. The store fills it in when it commits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched: Vec<String>,
    /// The run it was made in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Run>,
    /// The resource the invocation was made from, when it was made from one (`from` in a
    /// create action's menu, an agent's program): what the resources it created were made from
    /// ([`Version::from`], [`Store::linked`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ResourceRef>,
    /// What the action read that a replay cannot compute again, in the order it read them: the
    /// moment and zone it ran at, what it fetched and what it drew (convergence.md, "Actions").
    /// Kept with the change, so history says what the change was made of.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<crate::run::JournalEntry>,
    /// How many items the change was made over: one for an action applied to its subject, and
    /// for one action applied to many items at once (`many`), as many as the items.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub count: u32,
    /// The run this change was made in, with every action it called
    /// ([`crate::run::Journal::invocation`]): what makes one run's changes one action of the
    /// workspace, and takes them all back when the run fails anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
}

fn one() -> u32 {
    1
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "`skip_serializing_if` calls it with the field by reference"
)]
fn is_one(count: &u32) -> bool {
    *count == 1
}

/// The action id of the change that made a resource, when the host made it.
pub const CREATE_ACTION: &str = "$create";

/// The action id of a change a bound control makes to the text field it shows.
pub const EDIT_TEXT_ACTION: &str = "edit-text";

/// The action id of a change a bound control makes to the flag field it shows.
pub const EDIT_FLAG_ACTION: &str = "edit-flag";

/// One version of a resource, as history keeps it: who made it, when, by which action, in
/// which run, and what it did to the resource.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub who: String,
    /// ISO-8601 UTC, `YYYY-MM-DDTHH:MM:SSZ`.
    pub at: String,
    /// The action's id (`$create` for a resource the host made).
    pub action: String,
    /// The run that made it; `None` for a change made outside a run.
    #[serde(rename = "task")]
    pub run: Option<Run>,
    /// What it did to the resource.
    pub kind: VersionKind,
    /// The fields it changed, by id; empty for a version that made or removed the resource.
    pub fields: Vec<FieldId>,
    /// The resource it is a version of, as [`ResourceRef::id_string`] writes it.
    pub resource: String,
    /// The change that made it, as its store names changes; empty while provisional. What a
    /// seen mark names ([`crate::seen`]).
    pub change: String,
    /// The counter of the change's first operation: the change's place in the record's causal
    /// order, which every replica agrees on however the change reached it. A change made on
    /// top of another counts past every operation of it, so of two versions recorded in the
    /// same second the later change is the later version; [`u64::MAX`] for an action this
    /// device has not committed yet.
    #[serde(default)]
    pub op: u64,
    /// For an item it removed, where the item stood: what a list that held it shows of it
    /// until the person has seen its removal.
    pub left: Option<Left>,
    /// The items it put into a collection, in the order it put them: an action that inserts
    /// items is a version of the collection that names them, so where a run of new items begins
    /// is known without reading the collection (`first_unseen(r)`). Empty for a version that
    /// put none in, and for a store that does not keep them.
    pub placed: Vec<String>,
    /// The resource this one was made from, for the version that made it: the invocation it
    /// ran in was made from that resource (a message a task was made from). `None` for a change
    /// that made none, and for a resource made from nothing.
    pub from: Option<ResourceRef>,
    /// Who proposed the action, for a change made by accepting a suggestion, so history says
    /// who proposed it; `None` for a change made on its own.
    pub suggested_by: Option<String>,
    /// How many items the change was made over ([`ChangeNote::count`]): more than one for one
    /// action applied to many items at once.
    pub count: u32,
    /// The gesture the change is one of, when several of them are one act of the person's (the
    /// keystrokes of one burst of typing): history lists them as one entry, and undo takes them
    /// back as one step. `None` for a change that is an act of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture: Option<String>,
}

impl Version {
    /// What orders two versions, the same on every replica: when it happened, then the
    /// change's causal place for the versions one second holds, then the change itself for two
    /// that are neither before the other.
    #[must_use]
    pub fn order(&self) -> (&str, u64, &str) {
        // Two people changing a resource apart each hold the version they made last until
        // their changes reach each other, and the one made later in the day is the last
        // version both read once they have. The change's place in the log says nothing there,
        // since a replica holds the changes in the order they reached it.
        (&self.at, self.op, &self.change)
    }

    /// Whether this version comes before `other` ([`Self::order`]).
    #[must_use]
    pub fn before(&self, other: &Self) -> bool {
        self.order() < other.order()
    }
}

/// Where a removed item stood ([`Version::left`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Left {
    /// The resource holding the collection, as [`ResourceRef::id_string`] writes it.
    pub holder: String,
    /// The collection's field.
    pub field: FieldId,
    /// The item's type, when the store knows it.
    pub ty: Option<TypeId>,
}

/// One run, as the changes it made are stamped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Run {
    /// The run's group (`runtime_collab::task_run_group`): `<work>@<run>`.
    pub id: String,
    /// What the run was asked, as it was titled when it ran.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

impl Run {
    /// The id of the work it is a run of: its group up to the `@`.
    #[must_use]
    pub fn of(&self) -> &str {
        self.id
            .split_once('@')
            .map_or(self.id.as_str(), |(work, _)| work)
    }
}

/// What a version did to its resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VersionKind {
    Created,
    #[default]
    Changed,
    Removed,
}

impl VersionKind {
    /// The word code reads it as: `created`, `changed` or `removed`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Changed => "changed",
            Self::Removed => "removed",
        }
    }

    /// The kind `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        match word {
            "created" => Some(Self::Created),
            "changed" => Some(Self::Changed),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }
}

/// A resource linked to another ([`Store::linked`]): made from it, mentioning it in its rich
/// text, or holding a reference to it in one of its fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The linked resource.
    pub of: ResourceRef,
    /// How it is linked to the one the query named.
    pub how: LinkHow,
}

impl Link {
    /// Its place in a list of links: by how, then by the resource's id.
    #[must_use]
    pub fn order(&self) -> (u8, String) {
        (self.how.rank(), self.of.id_string())
    }
}

/// How a resource is linked to another: what [`Store::linked`] answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LinkHow {
    /// An action made it from the resource.
    CreatedFrom,
    /// A reference field of it names the resource.
    Field,
    /// Its rich text mentions the resource.
    Mention,
}

impl LinkHow {
    /// The word code reads it as: `created_from`, `mention` or `field`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::CreatedFrom => "created_from",
            Self::Mention => "mention",
            Self::Field => "field",
        }
    }

    /// The how `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        match word {
            "created_from" => Some(Self::CreatedFrom),
            "mention" => Some(Self::Mention),
            "field" => Some(Self::Field),
            _ => None,
        }
    }

    /// How it orders against the others: made from, then a field, then a mention.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::CreatedFrom => 0,
            Self::Field => 1,
            Self::Mention => 2,
        }
    }
}

/// The pseudo-field of a resource holding its links ([`Store::linked`]): what a view that read
/// them is told of, and what a store with an index keeps to answer them from.
pub const LINKS: &str = "$links";

/// The pseudo-field of a reading of another app's resource that says what the person it was
/// read for has not seen of the items it holds: `{"first": <the first unseen item's id, or
/// null>, "count": <how many>}`. The seen marks and versions that answer it are kept where
/// that app runs, so they come in the reading, as [`LINKS`] do.
pub const UNSEEN: &str = "$unseen";

/// What the pseudo-field holding when a field of a resource last changed begins with; the
/// field's id follows, so `$changed.done` holds the version that last changed `done`
/// ([`Store::field_changed`]).
pub const CHANGED: &str = "$changed.";

/// The pseudo-field holding when the field `field` of a resource last changed.
#[must_use]
pub fn changed_field(field: &str) -> String {
    format!("{CHANGED}{field}")
}

/// The action a change is a version by when it is a step on the record rather than a package
/// action's: an undo, or a revert from History. A step takes changes back instead of making
/// one, so it names no field's last change.
pub const STEP_ACTION: &str = "$history";

/// The version of a resource an order or a key reads, beside its fields: its first
/// (`created(r)`), its last (`updated(r)`), or the one that last changed one of its fields
/// (`changed(r, .field)`), ordered by their time. Named in an [`Order`] by the pseudo-field
/// ids `$created`, `$updated` and `$changed.<field>`, which no declared field has.
///
/// [`Order`]: crate::query::Order
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reflected {
    Created,
    Updated,
    /// The last change of the field it names.
    Changed(String),
}

impl Reflected {
    pub const CREATED: &'static str = "$created";
    pub const UPDATED: &'static str = "$updated";

    /// The version the pseudo-field `field` names.
    #[must_use]
    pub fn of(field: &str) -> Option<Self> {
        match field {
            Self::CREATED => Some(Self::Created),
            Self::UPDATED => Some(Self::Updated),
            _ => field_of_changed(field).map(|field| Self::Changed(field.to_owned())),
        }
    }
}

/// The field whose last change the pseudo-field `id` names, when it names one.
#[must_use]
pub fn field_of_changed(id: &str) -> Option<&str> {
    id.strip_prefix(CHANGED).filter(|field| !field.is_empty())
}

/// The versions of `at` among a document's changes, oldest first ([`Version::order`]): the
/// changes whose resources include it (named by id, or by the path an older change named it
/// by), whatever order the changes reached this replica in.
#[must_use]
pub fn versions_in<'c>(
    changes: impl IntoIterator<Item = &'c automerge::Change>,
    at: &ResourceRef,
) -> Vec<Version> {
    let id = at.id_string();
    // A change is named by its place in the document's log.
    let mut making = true;
    let mut versions: Vec<Version> = changes
        .into_iter()
        .enumerate()
        .filter_map(|(n, change)| {
            let note: ChangeNote = serde_json::from_str(change.message()?).ok()?;
            note.touched
                .iter()
                .any(|touched| *touched == id || crate::value::flat_id(touched) == id)
                .then_some((n, note, change))
        })
        .map(|(n, note, change)| {
            let kind = if note.action == CREATE_ACTION {
                VersionKind::Created
            } else {
                VersionKind::Changed
            };
            // The first change of a resource in the log is the one that made it.
            let made = std::mem::take(&mut making);
            Version {
                who: note.invoker.clone(),
                at: note
                    .at
                    .clone()
                    .unwrap_or_else(|| crate::core::EPOCH.to_owned()),
                action: note.action.clone(),
                run: note.group.clone(),
                kind,
                fields: Vec::new(),
                resource: id.clone(),
                change: n.to_string(),
                op: change.start_op().get(),
                left: None,
                placed: Vec::new(),
                // What it was made from is a fact of the change that made it.
                from: made.then(|| note.from.clone()).flatten(),
                suggested_by: note.suggested_by.clone(),
                count: note.count.max(1),
                gesture: None,
            }
        })
        .collect();
    versions.sort_by(|one, other| one.order().cmp(&other.order()));
    versions
}

/// The versions of a resource after the one a person's seen mark names: the newest page of
/// them, and how many there are ([`Store::versions_since`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SinceVersions {
    /// The newest `page` of them, oldest first ([`Version::order`]): what a catch-up reads to
    /// say who changed the resource and what they did.
    pub versions: Vec<Version>,
    /// How many the resource has after the mark, those read included: what a catch-up says it
    /// folds, counted without copying the log.
    pub total: usize,
}

impl SinceVersions {
    /// The versions of `all` (oldest first) from `from` on, the newest `page` of them.
    #[must_use]
    pub fn page(all: &[Version], from: usize, page: usize) -> Self {
        let total = all.len().saturating_sub(from);
        let start = all.len().saturating_sub(total.min(page));
        Self {
            versions: all.get(start..).unwrap_or_default().to_vec(),
            total,
        }
    }
}

/// The last version of `versions` (oldest first) that changed the field `field`
/// ([`Store::field_changed`]): the one that made the resource when none after it names the
/// field, since making it set them. `None` for a resource the log holds no version of.
#[must_use]
pub fn last_change_of(versions: Vec<Version>, field: &str) -> Option<Version> {
    let made = versions.first().cloned();
    versions
        .into_iter()
        .rev()
        .find(|version| {
            version.action != STEP_ACTION && version.fields.iter().any(|held| held == field)
        })
        .or(made)
}

/// One field of one resource: the resource as [`ResourceRef::id_string`] writes it (its
/// document, then `/item` for an item), and the field id, or [`ANY_FIELD`] for every field
/// of that resource. Views record what they read as these, and stores report changes as them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldKey {
    pub resource: String,
    pub field: FieldId,
}

/// The field of a [`FieldKey`] that stands for every field of its resource: the resource was
/// created, replaced or removed as a whole.
pub const ANY_FIELD: &str = "*";

impl FieldKey {
    pub fn new(at: &ResourceRef, field: impl Into<FieldId>) -> Self {
        Self {
            resource: at.id_string(),
            field: field.into(),
        }
    }

    /// The document the resource lives in.
    pub fn doc(&self) -> &str {
        doc_of(&self.resource)
    }
}

/// The document of a resource written as [`ResourceRef::id_string`].
pub fn doc_of(resource: &str) -> &str {
    resource.split_once('/').map_or(resource, |(doc, _)| doc)
}

/// The items of a collection, in order: what [`Store::listing`] hands out, shared, so a view
/// reads a collection's length or one item without copying its items.
///
/// Item ids are shared too: what a view read of a listing (the item at an index) is kept to
/// compare with the listing after a change, which costs a count, not a copy.
///
/// A store may hand out the items it holds ([`Listing::from_items`]) or a listing it reads
/// page by page as items are asked for ([`Listing::paged`]): a view that shows the first rows
/// of 100,000 items then reads those rows' page, not the 100,000.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    /// The items, when held.
    held: Vec<Rc<str>>,
    /// Where the items are read from instead, when paged.
    paged: Option<Rc<dyn PagedItems>>,
}

/// Items a store reads as they are asked for: a listing as it was when handed out, which a
/// later change does not alter.
pub trait PagedItems: std::fmt::Debug {
    /// How many items the listing has. A listing that keeps checking its items as it reads
    /// them may know only that this is not the whole of it yet: a count taken before it read
    /// the rest is the most it can have ([`Self::len_is_exact`]).
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether [`Self::len`] is the listing's length, not a count that may yet come down. A
    /// listing that checks its items one by one as it reads does not know its length until it
    /// has read them all: what shows its length before then says it is up to that many.
    fn len_is_exact(&self) -> bool {
        true
    }

    /// The item at `index`, `None` past the end.
    fn get(&self, index: usize) -> Option<Rc<str>>;

    /// Where the item `id` is, found by what orders it (a page or two), not by reading the
    /// items one by one: `Some(None)` when it is not in the listing, `None` when this listing
    /// cannot say so.
    fn position(&self, id: &str) -> Option<Option<usize>> {
        let _ = id;
        None
    }

    /// Whether the listing holds the item `id`: `None` when it cannot say. A listing that
    /// checks its items one by one as it reads them answers from the item's own entries, never
    /// by reading the entries before it, so what it holds costs the item, not the listing
    /// ([`Self::position`], which counts the items before it).
    fn holds(&self, id: &str) -> Option<bool> {
        self.position(id).map(|at| at.is_some())
    }
}

impl PartialEq for Listing {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl Eq for Listing {}

impl Listing {
    pub fn from_ids(ids: impl IntoIterator<Item = ItemId>) -> Self {
        Self::from_items(ids.into_iter().map(Rc::from).collect())
    }

    /// A listing of the items `items`, held.
    pub fn from_items(items: Vec<Rc<str>>) -> Self {
        Self {
            held: items,
            paged: None,
        }
    }

    /// A listing read page by page from `items`.
    pub fn paged(items: Rc<dyn PagedItems>) -> Self {
        Self {
            held: Vec::new(),
            paged: Some(items),
        }
    }

    pub fn len(&self) -> usize {
        self.paged
            .as_ref()
            .map_or(self.held.len(), |paged| paged.len())
    }

    /// Whether [`Self::len`] is the listing's length. A paged listing that checks its items as
    /// it reads them knows only how many it can have at most until it has read them all:
    /// showing that as the length says it is up to that many.
    pub fn len_is_exact(&self) -> bool {
        self.paged.as_ref().is_none_or(|paged| paged.len_is_exact())
    }

    /// The items it holds: [`Self::len`] where that is its length, a listing that checks its
    /// items as it reads them read to its end, which is what saying how many it holds costs.
    /// [`Self::len`] is the length the store knows without reading it; this is the count.
    pub fn count(&self) -> usize {
        if self.len_is_exact() {
            return self.len();
        }
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<Rc<str>> {
        match &self.paged {
            Some(paged) => paged.get(index),
            None => self.held.get(index).cloned(),
        }
    }

    pub fn first(&self) -> Option<Rc<str>> {
        self.get(0)
    }

    pub fn last(&self) -> Option<Rc<str>> {
        self.len().checked_sub(1).and_then(|last| self.get(last))
    }

    /// Where the item `id` is: `Some(None)` when it is not in the listing. A listing read page by
    /// page finds it by what orders it, reading a page or two; one that cannot gives `None`
    /// rather than read its pages one by one.
    pub fn position(&self, id: &str) -> Option<Option<usize>> {
        match &self.paged {
            Some(paged) => paged.position(id),
            None => Some(self.held.iter().position(|held| &**held == id)),
        }
    }

    /// Whether the listing holds the item `id`, `None` when it cannot say: [`Self::position`]
    /// where the listing answers that without reading the items before it, as a listing that
    /// checks its items one by one as it reads them does.
    pub fn holds(&self, id: &str) -> Option<bool> {
        match &self.paged {
            Some(paged) => paged.holds(id),
            None => Some(self.held.iter().any(|held| &**held == id)),
        }
    }

    /// The items in order, each read as it is reached.
    pub fn iter(&self) -> impl Iterator<Item = Rc<str>> + '_ {
        (0..self.len()).map_while(|index| self.get(index))
    }

    /// The items, held: read whole when paged.
    pub fn items(&self) -> Vec<Rc<str>> {
        match &self.paged {
            Some(_) => self.iter().collect(),
            None => self.held.clone(),
        }
    }

    /// The items to change in place: read whole, and held from then on, when paged.
    pub fn items_mut(&mut self) -> &mut Vec<Rc<str>> {
        if self.paged.is_some() {
            self.held = self.iter().collect();
            self.paged = None;
        }
        &mut self.held
    }

    /// The item ids, as owned text.
    pub fn ids(&self) -> Vec<ItemId> {
        self.iter().map(|id| id.to_string()).collect()
    }
}

/// One step of a change to a collection's items, at an index of the listing as the steps
/// before it left it. A move is a removal and an insertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Splice {
    Insert {
        at: usize,
        item: ItemId,
    },
    /// `item` is the one that was at `at`: a reader checks it before applying the step.
    Remove {
        at: usize,
        item: ItemId,
    },
}

impl Splice {
    /// The item the step puts in or takes out.
    #[must_use]
    pub fn item(&self) -> &str {
        match self {
            Self::Insert { item, .. } | Self::Remove { item, .. } => item,
        }
    }
}

/// Where an item stands in a listing, with the item.
pub type Place<'a> = (usize, &'a str);

/// Where an item both listings hold stood and where it stands, with the item.
pub type Stayed<'a> = (usize, usize, &'a str);

/// The steps from `before` to `after`, two listings of one query (a filtered or sorted view of a
/// collection), when only the items `moved` came, went or moved between them and the others
/// kept their order: each is found where it was and where it is ([`Listing::position`]), so the
/// steps cost the items that changed, not the listing's length. The removals come first, from
/// the last, then the insertions in order; an item that stays takes no step of its own — its
/// place follows the items that came and went, and only one that comes to stand among other
/// items than it did is taken out and put back. `None` when a listing cannot say where an item
/// is.
#[must_use]
pub fn moved_steps<'a>(
    before: &Listing,
    after: &Listing,
    moved: impl IntoIterator<Item = &'a str>,
) -> Option<Vec<Splice>> {
    let mut gone: Vec<Place<'_>> = Vec::new();
    let mut come: Vec<Place<'_>> = Vec::new();
    let mut both: Vec<Stayed<'_>> = Vec::new();
    for id in moved {
        match (before.position(id)?, after.position(id)?) {
            (Some(was), Some(now)) => both.push((was, now, id)),
            (was, now) => {
                gone.extend(was.map(|at| (at, id)));
                come.extend(now.map(|at| (at, id)));
            }
        }
    }
    Some(moved_splices(
        before.len(),
        after.len(),
        &gone,
        &come,
        &both,
    ))
}

/// The steps from one listing to another for the items a change touched: `both` holds where
/// each item both listings hold stood and where it stands, `gone` those only the first holds
/// and `come` those only the second. The removals come from the last, then the insertions in
/// order; an item that stays takes no step of its own — its place follows the items that came
/// and went, and only one that comes to stand among others than it did is taken out and put
/// back.
#[must_use]
pub fn moved_splices<'a>(
    before: usize,
    after: usize,
    gone: &[Place<'a>],
    come: &[Place<'a>],
    both: &[Stayed<'a>],
) -> Vec<Splice> {
    // The longest run of the items both listings hold that already stands in its order: those
    // are the items the ones that came and went settle around, and the rest came to stand
    // among others than they did, so each is taken out and put back.
    let kept = in_order(both);
    let moved_one = |at: usize| !kept.get(at).copied().unwrap_or(true);
    let mut moving: Vec<Stayed<'_>> = both
        .iter()
        .enumerate()
        .filter(|&(at, _)| moved_one(at))
        .map(|(_, &(was, now, id))| (was, now, id))
        .collect();
    let (mut removed, mut added) = with_spliced(gone, come, &moving);
    let slots = settling(before, both, &removed, &added);
    let placed = slots.len() == after
        && both
            .iter()
            .enumerate()
            .filter(|&(at, _)| !moved_one(at))
            .all(|(_, &(_, now, id))| slots.iter().position(|held| *held == Some(id)) == Some(now));
    if !placed {
        // The items do not keep an order that settles them: each of the ones both hold is
        // taken out and put back, in its place.
        moving = both.to_vec();
        (removed, added) = with_spliced(gone, come, &moving);
    }
    let mut out = Vec::with_capacity(removed.len().saturating_add(added.len()));
    out.extend(removed.into_iter().map(|(at, id)| Splice::Remove {
        at,
        item: id.to_owned(),
    }));
    out.extend(added.into_iter().map(|(at, id)| Splice::Insert {
        at,
        item: id.to_owned(),
    }));
    out
}

/// The steps of `before` for `gone` and of `after` for `come`, with the items `spliced` taken out
/// of the one and put back into the other: the removals from the last, then the insertions in
/// order, as a reader applies them.
fn with_spliced<'a>(
    gone: &[Place<'a>],
    come: &[Place<'a>],
    spliced: &[Stayed<'a>],
) -> (Vec<Place<'a>>, Vec<Place<'a>>) {
    let mut gone = gone.to_vec();
    let mut come = come.to_vec();
    for &(was, now, id) in spliced {
        gone.push((was, id));
        come.push((now, id));
    }
    gone.sort_unstable_by(|a, b| b.cmp(a));
    come.sort_unstable();
    (gone, come)
}

/// Which of the items both listings hold are already in the order they stand in each, the
/// longest such run: an item of it only shifts with the items that came and went.
fn in_order(both: &[Stayed<'_>]) -> Vec<bool> {
    // The items in the order `before` holds them, by their place in `after`: the longest run of
    // places that grows is the longest run of items that keeps its order.
    let mut places: Vec<(usize, usize, usize)> = both
        .iter()
        .enumerate()
        .map(|(at, &(was, now, _))| (was, now, at))
        .collect();
    places.sort_unstable_by_key(|&(was, ..)| was);
    let now_at = |place: usize| places.get(place).map_or(usize::MAX, |&(_, now, _)| now);
    let mut runs: Vec<usize> = Vec::new();
    let mut from: Vec<Option<usize>> = Vec::with_capacity(places.len());
    for (place, &(_, now, _)) in places.iter().enumerate() {
        let width = runs.partition_point(|&run| now_at(run) < now);
        from.push(
            width
                .checked_sub(1)
                .and_then(|short| runs.get(short).copied()),
        );
        match runs.get_mut(width) {
            Some(run) => *run = place,
            None => runs.push(place),
        }
    }
    let mut kept = vec![false; both.len()];
    let mut walk = runs.last().copied();
    while let Some(place) = walk {
        if let Some(&(.., at)) = places.get(place)
            && let Some(slot) = kept.get_mut(at)
        {
            *slot = true;
        }
        walk = from.get(place).copied().flatten();
    }
    kept
}

/// Where the items of `both` stand once `gone` is taken out and `come` put in, `None` for every
/// item the steps do not name: read over the items that stayed, it says which of them only
/// shifted with the items around them and which came to stand among others.
fn settling<'a>(
    len: usize,
    both: &[Stayed<'a>],
    gone: &[Place<'a>],
    come: &[Place<'a>],
) -> Vec<Option<&'a str>> {
    let mut slots: Vec<Option<&'a str>> = vec![None; len];
    for &(was, _, id) in both {
        if let Some(slot) = slots.get_mut(was) {
            *slot = Some(id);
        }
    }
    for (at, _) in gone {
        if *at < slots.len() {
            slots.remove(*at);
        }
    }
    for (at, id) in come {
        slots.insert((*at).min(slots.len()), Some(*id));
    }
    slots
}

/// How a collection's listing changed: the steps, in order, from the listing as the store held
/// it before the change to the listing after it.
///
/// A reader that keeps something per item (the rows of a list, the published children of a
/// resource) applies the steps instead of comparing every item. It checks them as it goes (a
/// removal names the item it removes, an insertion must not find its item already there) and
/// its length against the listing's at the end, and rebuilds from the listing when they do not
/// fit: what it holds was not the listing the steps start from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListingChange {
    /// The collection: its resource and field.
    pub key: FieldKey,
    pub splices: Vec<Splice>,
}

/// Which fields of which document changed. An empty field list means "unknown: every field of
/// every resource of the document may have changed".
///
/// A collection whose items changed is named in `fields` like any field and in `collections`;
/// when the store could say how, `listings` holds the steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSummary {
    pub doc: DocId,
    pub fields: Vec<FieldKey>,
    /// The fields of `fields` whose items changed: items joined the collection, left it, or
    /// moved in it. When the store cannot say which ([`Self::unsaid`]), a reader that keeps
    /// something per item reads every resource's again, as one it does not know of may have.
    pub collections: Vec<FieldKey>,
    pub listings: Vec<ListingChange>,
    /// Who made the change, when the store knows: the person whose action it was, as the
    /// change's invoker. `None` for a change read from the record, whose author is not told
    /// per document. A reader showing a live query tells the person's own changes from
    /// others' by it ([`crate::live::Live::absorb`]).
    pub by: Option<String>,
}

impl ChangeSummary {
    /// Every field of `doc` may have changed.
    pub fn whole(doc: impl Into<DocId>) -> Self {
        Self {
            doc: doc.into(),
            fields: Vec::new(),
            collections: Vec::new(),
            listings: Vec::new(),
            by: None,
        }
    }

    /// The steps that changed the collection `key`, when the store could say.
    pub fn listing(&self, key: &FieldKey) -> Option<&[Splice]> {
        self.listings
            .iter()
            .find(|change| change.key == *key)
            .map(|change| change.splices.as_slice())
    }

    /// Whether a collection's items changed and the store cannot say how: which items joined,
    /// left or moved is unknown, so a reader that keeps something per item reads every
    /// resource's again.
    pub fn unsaid(&self, key: &FieldKey) -> bool {
        self.collections.contains(key) && self.listing(key).is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubId(pub u64);

/// An outside effect waiting to be settled.
#[derive(Clone, Debug, PartialEq)]
pub struct OutboxEntry {
    pub doc: DocId,
    pub id: String,
    pub capability: String,
    pub payload: Json,
    /// Held back until this moment, ISO-8601 UTC text, when the action said
    /// (`outside(capability, payload, at: ...)`); `None` carries out as soon as it is read.
    pub held_until: Option<String>,
    pub state: OutboxState,
}

/// Where an outside effect that is not settled yet is on its way out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OutboxState {
    /// Queued: nothing was tried yet.
    #[default]
    Queued,
    /// Being handed to the service by the process whose token `by` is. It is marked so before
    /// it is handed over, so one whose process stopped meanwhile reads as outcome unknown and
    /// is never handed over twice.
    Sending { by: String },
    /// The service could not be reached, for `reason`: it waits to go online.
    Waiting { reason: String },
}

/// How an outside effect ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    /// The service took it.
    Done,
    /// The service refused it, for this reason.
    Rejected(String),
    /// The process handing it over stopped before the service answered: whether it went is
    /// not known, and it is not tried again.
    Unknown,
    /// A person took it back before it was carried out: it never went, and its reason says so.
    Withdrawn(String),
}

impl Settled {
    /// The outcome as the record keeps it: `done`, `rejected`, `unknown` or `withdrawn`.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Rejected(_) => "rejected",
            Self::Unknown => "unknown",
            Self::Withdrawn(_) => "withdrawn",
        }
    }

    /// Why, when the service said, or why it was taken back.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Rejected(reason) | Self::Withdrawn(reason) => reason,
            Self::Done | Self::Unknown => "",
        }
    }
}

/// How the outside effect a resource's actions queued last stands, as `delivery(r)` reads it:
/// `waiting`, `held`, `sending`, `sent`, `rejected`, `withdrawn` or `unknown`, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryState {
    pub state: String,
    pub reason: String,
}

impl DeliveryState {
    /// How an entry stands: settled with `settled`'s outcome word and reason, else by `state`.
    /// An entry not settled that is held back until a moment still reads `held`.
    #[must_use]
    pub fn of(state: &OutboxState, settled: Option<(&str, &str)>) -> Self {
        Self::of_entry(state, settled, None)
    }

    /// As [`Self::of`], for an entry held back until `held_until`: while the clock has not
    /// reached it, it reads `held`, and the moment says when.
    #[must_use]
    pub fn of_entry(
        state: &OutboxState,
        settled: Option<(&str, &str)>,
        held_until: Option<&str>,
    ) -> Self {
        let (state, reason) = match settled {
            Some(("done", _)) => ("sent", ""),
            Some(("unknown", _)) => ("unknown", ""),
            Some(("withdrawn", reason)) => ("withdrawn", reason),
            Some((_, reason)) => ("rejected", reason),
            None => match (state, held_until) {
                (OutboxState::Queued, Some(at)) => ("held", at),
                (OutboxState::Queued, None) => ("waiting", ""),
                (OutboxState::Sending { .. }, _) => ("sending", ""),
                (OutboxState::Waiting { reason }, _) => ("waiting", reason.as_str()),
            },
        };
        Self {
            state: state.to_owned(),
            reason: reason.to_owned(),
        }
    }
}

pub trait Store {
    /// What the store has read so far, counted: work that does not depend on the machine or
    /// its load, which a profile charges to the unit that read it. A store that counts none
    /// reads 0.
    fn reads(&self) -> crate::cost::Reads {
        crate::cost::Reads::default()
    }

    /// Whether the store has read the whole workspace: until it has, the items it lists are the
    /// part it has read, and a count of them is not the workspace's ([`crate::colls`]'
    /// `counted`). A store given everything it holds is.
    fn settled(&self) -> bool {
        true
    }

    /// A fresh id, unique across replicas and restarts.
    fn fresh_id(&mut self, prefix: &str) -> String;

    /// Create a document and commit it as one change.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the document cannot be created.
    fn create(&mut self, record: &NewRecord) -> Result<ResourceRef, StoreError>;

    /// Create a document under the id `id` and commit it as one change: a package's per-workspace
    /// root, `root-<type id>`, which code names as `root T` or by the root's name.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the document exists or cannot be created.
    fn create_with_id(&mut self, record: &NewRecord, id: &str) -> Result<ResourceRef, StoreError> {
        let _ = (record, id);
        Err(StoreError::Invalid(
            "this store makes documents under ids of its own".into(),
        ))
    }

    /// Create the document `id` inside the open transaction `txn`: what the action makes of it
    /// is the action's own change, committed with the rest of what it wrote and taken back with
    /// it when it fails. It is what `notify` writes ([`crate::notify`]): the notice is part of
    /// the change that told the person, not a change of its own.
    ///
    /// # Errors
    ///
    /// `Invalid` when the document exists or the store makes no document inside a transaction.
    fn create_in(
        &mut self,
        txn: Txn,
        record: &NewRecord,
        id: &str,
    ) -> Result<ResourceRef, StoreError> {
        let _ = (txn, record, id);
        Err(StoreError::Invalid(
            "this store makes no document inside a transaction".into(),
        ))
    }

    /// The type id stored at this resource.
    ///
    /// # Errors
    ///
    /// `NotFound` when the document or item does not exist.
    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError>;

    /// Where the item `at` is: the resource whose collection holds it, and the collection's
    /// field. `None` for a document, which no collection holds.
    ///
    /// # Errors
    ///
    /// `NotFound` when the document or item does not exist.
    fn container(&self, at: &ResourceRef) -> Result<Option<(ResourceRef, FieldId)>, StoreError>;
    /// How a reference in rich text reads for the person this store holds resources for: a
    /// resource they hold by what it is called now, one they cannot read as only being there.
    /// A store that cannot tell what its reader holds reads every resource as not shared: a
    /// reference never grants a read.
    fn reading(&self, reference: &crate::rich::Reference) -> crate::rich::Reading {
        if reference.as_person().is_some() {
            crate::rich::Reading::AsWritten
        } else {
            crate::rich::Reading::Unshared
        }
    }

    /// Open a transaction. Documents join it when first mutated.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when none can be opened.
    fn begin(&mut self) -> Result<Txn, StoreError>;

    /// Apply one mutation inside `txn`.
    ///
    /// # Errors
    ///
    /// `Invalid` for a mutation the field cannot take; `NotFound` for a missing target.
    fn apply(&mut self, txn: Txn, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError>;

    /// Commit one change per touched document, each carrying `note`.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when a document cannot take the change.
    fn commit(&mut self, txn: Txn, note: &ChangeNote) -> Result<(), StoreError>;

    /// Discard everything the transaction did.
    fn rollback(&mut self, txn: Txn);

    /// A run began: from its first change to its end, everything it writes belongs to `run`,
    /// and a run that fails anywhere takes all of it back out ([`Store::abort_run`]). The
    /// changes of one run are kept apart from what is published, so a store that hands what it
    /// commits straight on keeps them until the run ends.
    fn begin_run(&mut self, run: &str) {
        let _ = run;
    }

    /// The run `run` ended: what it wrote stays, and is published as the one action it is.
    fn end_run(&mut self, run: &str) {
        let _ = run;
    }

    /// The run `run` failed somewhere: take everything it wrote back out, in every document it
    /// touched, so nothing of it is published and nothing of it stays.
    fn abort_run(&mut self, run: &str) {
        let _ = run;
    }

    /// Read a field. Sees the transaction's own uncommitted changes.
    ///
    /// # Errors
    ///
    /// `NotFound` for a missing target; `Invalid` when the stored shape does not fit the kind.
    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError>;

    /// The field `read` names of `at`, as a predicate compares it: a rich text field as the
    /// text it is written as, which is what search finds it by (search.md §2), and every other
    /// field as [`lit_of`] reads it.
    ///
    /// # Errors
    ///
    /// As [`Self::read`] and [`lit_of`].
    fn field_lit(&self, at: &ResourceRef, read: &FieldRead) -> Result<Lit, StoreError> {
        field_lit_of(read, self.read(at, &read.spec)?)
    }

    /// The types every loaded package declares, as the runtime loads and reloads them: the
    /// fields a record has, which a store that reads a record's links by its type finds in
    /// them. A store that keeps an index is told them when its schema is registered.
    fn loaded_types(&mut self, types: &[Rc<TypeInfo>]) {
        let _ = types;
    }

    /// The items of the collection `field` of `at`, in order, shared: a view reads the length
    /// or one item of a large collection through this without copying the others. A store that
    /// keeps what it read hands out what it keeps.
    ///
    /// # Errors
    ///
    /// As [`Store::read`], and `Invalid` for a field that is not a collection.
    fn listing(&self, at: &ResourceRef, field: &FieldSpec) -> Result<Rc<Listing>, StoreError> {
        match self.read(at, field)? {
            FieldValue::Items(items) => Ok(Rc::new(Listing::from_ids(items))),
            other => Err(StoreError::Invalid(format!(
                "field {} is not a collection: {other:?}",
                field.id
            ))),
        }
    }

    /// The items of the collection `field` of `at` under the item `node` (above it, for
    /// `down` false) by the reference field `parent`, those under it in the order the
    /// collection keeps them and those above it from the nearest, each with the type it is.
    /// `None` when the store cannot follow that field, and the caller's own walk runs instead.
    ///
    /// # Errors
    ///
    /// As [`Store::read`].
    fn relatives(
        &self,
        _at: &ResourceRef,
        _field: &FieldId,
        _parent: &FieldId,
        _node: &ResourceRef,
        _down: bool,
    ) -> Result<Option<Vec<(ItemId, TypeId)>>, StoreError> {
        Ok(None)
    }

    /// The items of the collection `field` of `at` (whose items are of the type `item`) that
    /// `pred` holds of, in order, shared. A store that keeps what it read keeps this too, and
    /// brings it up to date as the items change rather than evaluating every item again.
    ///
    /// # Errors
    ///
    /// As [`Store::listing`], and as reading a field `pred` reads.
    fn listing_where(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: &Pred,
    ) -> Result<Rc<Listing>, StoreError> {
        let all = self.listing(at, field)?;
        let mut items = Vec::new();
        for id in all.iter() {
            let target = at.item(&id, item.clone());
            let holds = pred.eval(&mut |read: &FieldRead| self.field_lit(&target, read))?;
            if holds {
                items.push(id);
            }
        }
        Ok(Rc::new(Listing::from_items(items)))
    }

    /// The items of the collection `field` of `at` (of the type `item`) that `pred` holds of,
    /// all of them when it is `None`, in the order `order` (the collection's when `None`),
    /// shared. A store with an index answers both from it.
    ///
    /// # Errors
    ///
    /// As [`Store::listing_where`], and as reading the field `order` is by.
    fn listing_query(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: Option<&Pred>,
        order: Option<&crate::query::Order>,
    ) -> Result<Rc<Listing>, StoreError> {
        let base = match pred {
            Some(pred) => self.listing_where(at, field, item, pred)?,
            None => self.listing(at, field)?,
        };
        let Some(order) = order else {
            return Ok(base);
        };
        let levels = order.levels();
        let mut keyed = Vec::with_capacity(base.len());
        for id in base.iter() {
            let target = at.item(&id, item.clone());
            let mut keys = Vec::with_capacity(levels.len());
            for level in &levels {
                let value = match Reflected::of(&level.field) {
                    Some(Reflected::Changed(changed)) => self
                        .field_changed(&target, &changed)?
                        .map_or(Lit::Absent, |version| Lit::Time(version.at)),
                    Some(end) => self
                        .version_end(&target, end == Reflected::Updated)?
                        .map_or(Lit::Absent, |v| Lit::Time(v.at)),
                    None => lit_of(
                        &level.field,
                        self.read(
                            &target,
                            &FieldSpec {
                                id: level.field.clone(),
                                kind: level.kind,
                            },
                        )?,
                        &Lit::Absent,
                    )?,
                };
                keys.push(value);
            }
            keyed.push((keys, id));
        }
        keyed.sort_by(|(a, _), (b, _)| ordered_cmp(order, a, b));
        Ok(Rc::new(Listing::from_items(
            keyed.into_iter().map(|(_, id)| id).collect(),
        )))
    }

    /// The resources of the type `ty` anywhere in the store that `pred` holds of, in the order
    /// `order`: a query that looks inside no one resource (search.md, "A query"). A store
    /// with an index answers it from the index's columns of the type; this one has none.
    ///
    /// # Errors
    ///
    /// A store that cannot answer it.
    fn select(
        &self,
        ty: &TypeId,
        pred: &Pred,
        order: Option<&crate::query::Order>,
    ) -> Result<Vec<ResourceRef>, StoreError> {
        let _ = (pred, order);
        Err(StoreError::Invalid(format!(
            "this store answers no query of every {ty}; look inside a resource (`in:`)"
        )))
    }

    /// The one resource of the type `ty` anywhere in the store that `pred` holds of and comes
    /// first in `order`, or `None` when `pred` holds of none (any of them for no `order`): the
    /// same query as [`Store::select`], asked for its first answer, which a store with an index
    /// finds from its columns without reading the rest.
    ///
    /// # Errors
    ///
    /// As [`Store::select`].
    fn first(
        &self,
        ty: &TypeId,
        pred: &Pred,
        order: Option<&crate::query::Order>,
    ) -> Result<Option<ResourceRef>, StoreError> {
        Ok(self.select(ty, pred, order)?.into_iter().next())
    }

    /// When what a query that read the clock answers next changes, given the fields it compared
    /// with it ([`ClockField`]): the smallest value among them that lies after now, so the view
    /// wakes at the instant the item holding it comes to count and reads again, and does not run
    /// the clock to find it ([`NextChange`]). A store that cannot tell leaves the answer `Unknown`
    /// and the clock's grain wakes the view instead.
    ///
    /// # Errors
    ///
    /// A store that cannot read what it indexed.
    fn next_clock_change(
        &self,
        ty: &TypeId,
        fields: &[ClockField],
    ) -> Result<NextChange, StoreError> {
        let _ = (ty, fields);
        Ok(NextChange::Unknown)
    }

    /// The resources linked to `at`: those an action made from it, those whose rich text
    /// mentions it, and those holding a reference to it in a field, each once per way, ordered
    /// by the way and then by the resource's id. A store with an index answers it from a
    /// reverse list it keeps per resource, so its cost is what is linked to the resource, not
    /// what the workspace holds; a view reading it is told when it changes
    /// ([`LINKS`]).
    ///
    /// # Errors
    ///
    /// A store that cannot answer it.
    fn linked(&self, at: &ResourceRef) -> Result<Vec<Link>, StoreError> {
        let _ = at;
        Err(StoreError::Invalid(
            "this store answers no query of what links to a resource".into(),
        ))
    }

    /// The notes of a document's changes, oldest first.
    ///
    /// # Errors
    ///
    /// `NotFound` for a missing document.
    fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, StoreError>;
    /// The versions of the resource `at`, oldest first ([`Version::order`]): each change that
    /// changed it, with who made it, when, and which action (reflection: `created(r)`,
    /// `updated(r)`, `history(r)`). A store reads them from its log, which says where each
    /// change stands causally ([`Version::op`]).
    ///
    /// # Errors
    ///
    /// As [`Store::history`].
    fn versions(&self, at: &ResourceRef) -> Result<Vec<Version>, StoreError>;

    /// The versions of the resource `at` after the one the seen mark `mark` names, else from
    /// its making, the newest `page` of them, oldest first, with how many there are: what a
    /// window's catch-up reads of a record someone changed since the person last looked. A
    /// store with an index reads them from the place the mark names in the resource's log, so
    /// what opening the window costs is the page, not the length of the record's history.
    ///
    /// # Errors
    ///
    /// As [`Store::versions`].
    fn versions_since(
        &self,
        at: &ResourceRef,
        mark: Option<&crate::seen::SeenMark>,
        page: usize,
    ) -> Result<SinceVersions, StoreError> {
        let all = self.versions(at)?;
        let from = mark.map_or(0, |mark| crate::seen::after_mark(&all, mark));
        Ok(SinceVersions::page(&all, from, page))
    }

    /// The first version of the resource `at` (`created(r)`), or its last (`updated(r)`,
    /// `last`). A store with an index keeps both per resource, so a view showing them, or a
    /// list sorted by them, reads no history.
    ///
    /// # Errors
    ///
    /// As [`Store::versions`].
    fn version_end(&self, at: &ResourceRef, last: bool) -> Result<Option<Version>, StoreError> {
        let mut versions = self.versions(at)?;
        Ok(if last {
            versions.pop()
        } else {
            versions.into_iter().next()
        })
    }

    /// The last version of the resource `at` that changed the field `field` (`changed(r,
    /// .field)`), or the version that made it when none after it changed that field; `None`
    /// while the log of `at` is not at hand, and for a store whose versions name no fields.
    /// A store with an index keeps it per field, so a list groups or sorts by when a field
    /// last changed without reading the history of any row.
    ///
    /// # Errors
    ///
    /// As [`Store::versions`].
    fn field_changed(&self, at: &ResourceRef, field: &str) -> Result<Option<Version>, StoreError> {
        Ok(last_change_of(self.versions(at)?, field))
    }

    /// Every resource of the document `doc` (it and its items) with its versions, oldest
    /// first: what a window's mark counts the unseen changes of.
    ///
    /// # Errors
    ///
    /// As [`Store::versions`].
    fn versions_within(
        &self,
        doc: &DocId,
    ) -> Result<std::collections::BTreeMap<String, Vec<Version>>, StoreError> {
        let root = ResourceRef::root(doc.clone(), String::new());
        Ok(std::iter::once((doc.clone(), self.versions(&root)?)).collect())
    }

    /// The newest version of the resource `at`, else `None`: what a mark of it says. A store
    /// with an index keeps it per resource, so a row's mark reads one field, never the log.
    ///
    /// # Errors
    ///
    /// As [`Store::versions`].
    fn newest(&self, at: &ResourceRef) -> Result<Option<Version>, StoreError> {
        Ok(self.versions(at)?.pop())
    }

    /// The newest version of each resource of the document `doc` (it and its items), of at most
    /// `page` of them, most recent first, and whether it holds more: what a window's mark counts
    /// the unseen changes of, and the order a catch-up lists what changed. A store with an index
    /// answers it a page at a time, so a window of a workspace of any size reads no more than
    /// its page.
    ///
    /// # Errors
    ///
    /// As [`Store::versions_within`].
    fn newest_page(
        &self,
        doc: &DocId,
        page: usize,
    ) -> Result<(Vec<(String, Version)>, bool), StoreError> {
        newest_page_of_log(self, doc, page)
    }

    /// The items of the type `item` that left the collection `field` of `holder`, in the order
    /// they left it, each with the version that removed it: what a list shows struck, until
    /// the person has seen them go.
    ///
    /// A store that keeps only the log answers it from [`Self::versions_within`].
    ///
    /// # Errors
    ///
    /// As [`Store::versions_within`].
    fn removed(
        &self,
        holder: &ResourceRef,
        field: &str,
        item: &TypeId,
    ) -> Result<Vec<(String, Version)>, StoreError> {
        removed_of_log(self, holder, field, item)
    }

    /// Every version the run `run` (its group) made, of every resource, oldest first:
    /// what `changes(of: run)` reads.
    ///
    /// # Errors
    ///
    /// A store that keeps no log says so.
    fn run_changes(&self, run: &str) -> Result<Vec<Version>, StoreError> {
        Err(StoreError::Invalid(format!(
            "this store keeps no log to find the changes of the run {run} in"
        )))
    }

    /// The field `field` of `at` as it was before the change `change` (a [`Version::change`]):
    /// what a bound field changed by someone else shows as its previous value, and what a
    /// removed item's row is named by.
    ///
    /// The cost is the field's, never the record's: one field read in place at the change's
    /// dependencies, whatever the workspace holds beside it. A field of rich text is a document
    /// of its own, read at a fork of those dependencies: the cost of the text.
    ///
    /// # Errors
    ///
    /// `NotFound` for a change the store does not hold; a store that keeps no log says so.
    fn read_before(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        change: &str,
    ) -> Result<FieldValue, StoreError> {
        let _ = (at, field);
        Err(StoreError::Invalid(format!(
            "this store keeps no log to read before the change {change} in"
        )))
    }

    /// What the change `change` (a [`Version::change`]) wrote to each of `fields` of `at`: the
    /// field's value before the change and after it, read from the change itself, for each
    /// field it changed, in the order of `fields`. A field it wrote with what the field held is
    /// left out.
    ///
    /// # Errors
    ///
    /// `NotFound` for a change the store does not hold; a store that keeps no log says so.
    fn written(
        &self,
        at: &ResourceRef,
        fields: &[FieldSpec],
        change: &str,
    ) -> Result<Vec<(FieldId, FieldValue, FieldValue)>, StoreError> {
        let _ = (at, fields);
        Err(StoreError::Invalid(format!(
            "this store keeps no log to read what the change {change} wrote"
        )))
    }

    fn subscribe(&mut self, doc: &DocId, f: Box<dyn FnMut(&ChangeSummary)>) -> SubId;
    fn unsubscribe(&mut self, sub: SubId);

    /// Outside effects queued and not yet settled, each with the moment it is held until when
    /// the action held it back.
    fn outbox_pending(&self) -> Vec<OutboxEntry>;

    /// The earliest moment an unsettled outside effect is held until, ISO-8601 UTC text:
    /// nothing carries it out before then.
    fn outbox_next_held(&self) -> Option<String>;

    /// Take back the outside effects queued on `doc` that were not settled yet, settling each
    /// as withdrawn for `reason`, each as a change of its own.
    ///
    /// # Errors
    ///
    /// `NotFound` when the document is gone.
    fn outbox_withdraw(
        &mut self,
        doc: &DocId,
        reason: &str,
        note: &ChangeNote,
    ) -> Result<usize, StoreError>;

    /// Record where an entry that is not settled is now (handed over, or waiting to go
    /// online), as one change.
    ///
    /// # Errors
    ///
    /// `NotFound` when the entry is gone.
    fn outbox_mark(
        &mut self,
        entry: &OutboxEntry,
        state: &OutboxState,
        note: &ChangeNote,
    ) -> Result<(), StoreError>;

    /// Mark an entry settled with `outcome`, as one change.
    ///
    /// # Errors
    ///
    /// `NotFound` when the entry is gone.
    fn outbox_settle(
        &mut self,
        entry: &OutboxEntry,
        outcome: &Settled,
        note: &ChangeNote,
    ) -> Result<(), StoreError>;

    /// How the outside effect each resource's actions queued last stands, by document: what
    /// `delivery(r)` reads.
    fn outbox_states(&self) -> Vec<(DocId, DeliveryState)>;

    /// # Errors
    ///
    /// A [`StoreError`] when the journal cannot be saved.
    fn save_journal(&mut self, run: &RunId, journal: &Journal) -> Result<(), StoreError>;

    /// # Errors
    ///
    /// A [`StoreError`] when the journal cannot be read.
    fn load_journal(&mut self, run: &RunId) -> Result<Option<Journal>, StoreError>;
}

/// [`Store::newest_page`] as a store that keeps only the log answers it: the last version of
/// each resource of the document `doc`, the first `page` of them, most recent first, and
/// whether there are more.
///
/// # Errors
///
/// As [`Store::versions_within`].
pub fn newest_page_of_log(
    store: &(impl Store + ?Sized),
    doc: &DocId,
    page: usize,
) -> Result<(Vec<(String, Version)>, bool), StoreError> {
    let mut more = false;
    let out = store
        .versions_within(doc)?
        .into_iter()
        .filter_map(|(resource, versions)| versions.last().map(|last| (resource, last.clone())))
        .enumerate()
        .filter_map(|(i, at)| {
            if i < page {
                Some(at)
            } else {
                more = true;
                None
            }
        })
        .collect::<Vec<_>>();
    // Most recent first: a log records one second and no finer, so versions of the same second
    // keep the order the document holds them in.
    let mut out = out;
    out.sort_by(|a, b| b.1.at.cmp(&a.1.at));
    Ok((out, more))
}

/// [`Store::removed`] as a store that keeps only the log answers it: the items of the type
/// `item` that left the collection `field` of `holder`, each with the version that removed it.
///
/// # Errors
///
/// As [`Store::versions_within`].
pub fn removed_of_log(
    store: &(impl Store + ?Sized),
    holder: &ResourceRef,
    field: &str,
    item: &TypeId,
) -> Result<Vec<(String, Version)>, StoreError> {
    let held = holder.id_string();
    let within = store.versions_within(&holder.doc)?;
    Ok(within
        .into_iter()
        .filter_map(|(resource, versions)| {
            let last = versions.last()?;
            let stood_here = last.left.as_ref().is_some_and(|left| {
                last.kind == VersionKind::Removed
                    && left.holder == held
                    && left.field == field
                    && left.ty.as_ref().is_none_or(|ty| ty == item)
            });
            stood_here.then(|| (resource, last.clone()))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ids` as a listing the store holds whole.
    fn listing(ids: &[&str]) -> Listing {
        Listing::from_ids(ids.iter().map(|id| (*id).to_owned()))
    }

    /// The order the steps leave, applied as a reader applies them: each removal names the item
    /// it takes out, each insertion goes in at its index.
    fn replayed(before: &[&str], steps: &[Splice]) -> Vec<String> {
        let mut order: Vec<String> = before.iter().map(|id| (*id).to_owned()).collect();
        for step in steps {
            match step {
                Splice::Remove { at, item } => {
                    assert_eq!(order.get(*at).map(String::as_str), Some(item.as_str()));
                    order.remove(*at);
                }
                Splice::Insert { at, item } => order.insert(*at, item.clone()),
            }
        }
        order
    }

    #[test]
    fn the_items_that_stayed_take_no_step_though_another_one_went() {
        let before = ["a", "b", "c", "d"];
        let after = ["a", "b", "d"];
        let steps = moved_steps(&listing(&before), &listing(&after), before).unwrap();
        assert_eq!(
            steps,
            vec![Splice::Remove {
                at: 2,
                item: "c".to_owned(),
            }],
            "the three that stayed take no step"
        );
        assert_eq!(replayed(&before, &steps), after);
    }

    #[test]
    fn a_moved_item_is_a_removal_and_an_insertion_and_the_others_take_none() {
        let before = ["a", "b", "c", "d"];
        let after = ["c", "a", "b", "d"];
        let steps = moved_steps(&listing(&before), &listing(&after), before).unwrap();
        assert_eq!(replayed(&before, &steps), after);
        assert!(
            !steps
                .iter()
                .any(|step| step.item() == "d" && matches!(step, Splice::Remove { .. })),
            "the row that stayed is not taken out and put back: {steps:?}"
        );
        assert_eq!(
            steps,
            vec![
                Splice::Remove {
                    at: 2,
                    item: "c".to_owned(),
                },
                Splice::Insert {
                    at: 0,
                    item: "c".to_owned(),
                },
            ],
            "only the row that came to stand among others moves, and one step of its own"
        );
    }
}
