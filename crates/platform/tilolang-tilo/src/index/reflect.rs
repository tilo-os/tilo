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
use tilolang_runtime::store::{FieldValue, Left, Reflected, Run, Scalar, Version, VersionKind};
use tilolang_runtime::value::ResourceRef;

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

/// One version of a resource as the index keeps it: the runtime's [`Version`], which says
/// when the change happened, where it stands causally and which change it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub version: Version,
}

/// The causal place of an action this device invoked and has not committed yet: after every
/// change it holds, as the action was invoked on top of them.
pub const PROVISIONAL: u64 = u64::MAX;

impl Stamp {
    pub(crate) fn order(&self) -> (&str, u64, &str) {
        self.version.order()
    }

    /// Whether this is the version of the same invocation as `other`: who made it, when and by
    /// which action, over how many items and from what ([`Ends::same_invocation`]).
    #[must_use]
    pub fn same_invocation(&self, other: &Self) -> bool {
        let (held, other) = (&self.version, &other.version);
        held.who == other.who
            && held.at == other.at
            && held.action == other.action
            && held.run == other.run
            && held.count == other.count
            && held.from.as_ref().map(ResourceRef::id_string)
                == other.from.as_ref().map(ResourceRef::id_string)
    }

    /// Whether this version comes before `other`.
    #[must_use]
    pub fn before(&self, other: &Self) -> bool {
        self.version.before(&other.version)
    }

    /// This stamp as the version of the resource it is kept under, changed in `fields`: what it
    /// did, as reading it back from the index says it without the log.
    #[must_use]
    pub fn of_fields(&self, fields: &[String]) -> Self {
        let mut held = self.clone();
        held.version.fields = fields.to_vec();
        held
    }

    /// The stamp as the index holds it.
    #[must_use]
    pub fn value(&self) -> FieldValue {
        let mut held = json!({
            "who": self.version.who,
            "at": self.version.at,
            "action": self.version.action,
            "kind": self.version.kind.word(),
            "change": self.version.change,
            "op": self.version.op,
        });
        if let (Some(run), Some(map)) = (&self.version.run, held.as_object_mut()) {
            map.insert(
                "task".to_owned(),
                serde_json::to_value(run).unwrap_or(Json::Null),
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
        if self.version.count > 1
            && let Some(map) = held.as_object_mut()
        {
            map.insert("count".to_owned(), serde_json::json!(self.version.count));
        }
        // The fields it changed: what history says of it reads them, and a version read from
        // the index says what the same version read from the log does.
        if !self.version.fields.is_empty()
            && let Some(map) = held.as_object_mut()
        {
            map.insert("fields".to_owned(), serde_json::json!(self.version.fields));
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
        let run = match json.get("task") {
            None => None,
            Some(run) => match serde_json::from_value(run.clone()) {
                Ok(run) => Some(run),
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
        // An index written before versions named the fields they changed holds none.
        let fields = json
            .get("fields")
            .and_then(Json::as_array)
            .map(|fields| {
                fields
                    .iter()
                    .filter_map(Json::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            version: Version {
                who: text("who")?,
                fields,
                at: text("at")?,
                action: text("action")?,
                run,
                change: text("change").unwrap_or_default(),
                op: json.get("op").and_then(Json::as_u64)?,
                from,
                suggested_by: text("suggested_by"),
                // An index written before versions named what they did holds only changes.
                kind: text("kind")
                    .and_then(|word| VersionKind::of_word(&word))
                    .unwrap_or_default(),
                count: json
                    .get("count")
                    .and_then(Json::as_u64)
                    .and_then(|count| u32::try_from(count).ok())
                    .unwrap_or(1),
                ..Version::default()
            },
        })
    }
}

/// The action a step on the record that is no package's action is a version by (an undo, a
/// revert from History; [`tilolang_runtime::store::STEP_ACTION`]).
pub const STEP_ACTION: &str = tilolang_runtime::store::STEP_ACTION;

/// The run a change's context names: its group, when its kind is a run's (`TASK_RUN_GROUP`).
pub fn run_of(context: &tilo_actions::ActionContext) -> Option<Run> {
    let group = context.group.as_ref()?;
    (group.kind == tilo_actions::context::TASK_RUN_GROUP).then(|| Run {
        id: group.id.clone(),
        title: group.label.clone(),
    })
}

/// The versions `change` made, one per resource it changed, when it is a tilolang action's
/// change: who, when, by which action, in which run, and what it did to each.
#[must_use]
pub fn versions_of(change: &automerge::Change) -> Vec<Version> {
    let Some((stamp, action, run)) = read(change) else {
        return Vec::new();
    };
    versions_of_action(&stamp, &action, run.as_ref())
}

/// [`versions_of`] of a change [`read`] once: what a reader of a chunk's whole log does for each
/// of its changes, which an action's change is read for once rather than once a question.
#[must_use]
pub fn versions_of_action(stamp: &Stamp, action: &DynAction, run: Option<&Run>) -> Vec<Version> {
    let run = run.cloned();
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
            run: run.clone(),
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

/// [`placed_of`] of a change [`read_change`] once: where the items its action placed are.
#[must_use]
pub fn placed_of_action(action: &DynAction) -> Vec<(String, String, String, Option<String>)> {
    action.placed()
}

/// What a change says of itself beside its operations: its hash, when it was made, its first
/// operation and its message. Reading what a change records needs no more, and a document
/// answers these for its whole log without encoding a change's operations
/// ([`automerge::Automerge::get_changes_meta`]): building a chunk's versions read its two
/// thousand changes in some 70 ms as whole changes, and in 2 ms as these.
pub trait ChangeHead {
    /// The change's hash.
    fn hash(&self) -> automerge::ChangeHash;
    /// The message the change carries.
    fn message(&self) -> Option<&str>;
    /// When the change was made, in milliseconds since the epoch.
    fn timestamp(&self) -> i64;
    /// The counter of the change's first operation.
    fn start_op(&self) -> u64;
    /// The change whole, operations and all, as `mirror` (which holds it) has it.
    fn whole<'a>(&'a self, mirror: &Doc) -> Option<std::borrow::Cow<'a, automerge::Change>>;
}

impl ChangeHead for automerge::Change {
    fn hash(&self) -> automerge::ChangeHash {
        automerge::Change::hash(self)
    }

    fn message(&self) -> Option<&str> {
        automerge::Change::message(self)
    }

    fn timestamp(&self) -> i64 {
        automerge::Change::timestamp(self)
    }

    fn start_op(&self) -> u64 {
        automerge::Change::start_op(self).get()
    }

    fn whole<'a>(&'a self, _: &Doc) -> Option<std::borrow::Cow<'a, automerge::Change>> {
        Some(std::borrow::Cow::Borrowed(self))
    }
}

impl ChangeHead for automerge::ChangeMetadata<'_> {
    fn hash(&self) -> automerge::ChangeHash {
        self.hash
    }

    fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    fn timestamp(&self) -> i64 {
        self.timestamp
    }

    fn start_op(&self) -> u64 {
        self.start_op
    }

    fn whole<'a>(&'a self, mirror: &Doc) -> Option<std::borrow::Cow<'a, automerge::Change>> {
        mirror
            .committed_document()
            .get_change_by_hash(&self.hash)
            .map(std::borrow::Cow::Owned)
    }
}

impl<C: ChangeHead + ?Sized> ChangeHead for &C {
    fn hash(&self) -> automerge::ChangeHash {
        (**self).hash()
    }

    fn message(&self) -> Option<&str> {
        (**self).message()
    }

    fn timestamp(&self) -> i64 {
        (**self).timestamp()
    }

    fn start_op(&self) -> u64 {
        (**self).start_op()
    }

    fn whole<'a>(&'a self, mirror: &Doc) -> Option<std::borrow::Cow<'a, automerge::Change>> {
        (**self).whole(mirror)
    }
}

/// What a change of a package action says, read once: its version, its action, and the run it
/// was made in. `None` for a change that is no action of a package (an undo, a merge), which
/// [`step_stamp`] answers for.
#[must_use]
pub fn read_change(change: &(impl ChangeHead + ?Sized)) -> Option<(Stamp, DynAction, Option<Run>)> {
    read(change)
}

/// The edits `change` records, when it is a tilolang action's change: what a write the store
/// made is the record's when the record's change holds the very edit it made.
#[must_use]
pub fn edits_of(change: &automerge::Change) -> Vec<DynEdit> {
    read(change).map_or_else(Vec::new, |(_, action, _)| action.edits)
}

/// Where each item the `changes` placed is: the record holding its collection, the field and
/// its type. What a removal in the same batch left, for an index that never held the item.
pub type Placed = BTreeMap<String, Left>;

/// Where the items `changes` placed are, as the changes that placed them recorded it
/// ([`placed_of`]): the last place each was put in.
pub fn placed_by<'c>(changes: impl IntoIterator<Item = &'c automerge::Change>) -> Placed {
    let mut out = Placed::new();
    for change in changes {
        for (item, holder, field, ty) in placed_of(change) {
            let ty = ty.or_else(|| out.get(&item).and_then(|left| left.ty.clone()));
            out.insert(item, Left { holder, field, ty });
        }
    }
    out
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

/// The version `change` made, its action and its run, when it is a tilolang action's
/// change.
fn read(change: &(impl ChangeHead + ?Sized)) -> Option<(Stamp, DynAction, Option<Run>)> {
    parsed(change).read.clone()
}

/// What a change's message says: the context it records and, for a package action's change,
/// [`read`]'s version, action and run.
struct Parsed {
    context: Option<tilo_actions::ActionContext>,
    read: Option<(Stamp, DynAction, Option<Run>)>,
}

/// How many changes' messages [`parsed`] keeps: those of the changes one sync takes in.
const PARSED_KEPT: usize = 32;

thread_local! {
    /// The messages of the changes read last, by hash, newest last.
    static PARSED: std::cell::RefCell<std::collections::VecDeque<(automerge::ChangeHash, std::rc::Rc<Parsed>)>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

/// The message of `change`, parsed once: a change never changes, and taking one in reads it
/// for the edits it holds, the items it placed, the versions it made and whether it is a step,
/// each a parse of its message and its action otherwise.
fn parsed(change: &(impl ChangeHead + ?Sized)) -> std::rc::Rc<Parsed> {
    let hash = change.hash();
    let kept = PARSED.with(|kept| {
        kept.borrow()
            .iter()
            .find(|(held, _)| *held == hash)
            .map(|(_, parsed)| std::rc::Rc::clone(parsed))
    });
    if let Some(parsed) = kept {
        return parsed;
    }
    let context = change
        .message()
        .and_then(tilo_actions::ActionContext::from_change_message);
    let read = context
        .as_ref()
        .and_then(|context| read_context(change, context));
    let parsed = std::rc::Rc::new(Parsed { context, read });
    PARSED.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.len() >= PARSED_KEPT {
            kept.pop_front();
        }
        kept.push_back((hash, std::rc::Rc::clone(&parsed)));
    });
    parsed
}

/// The person `change` names as the one who made it, when its message names one.
pub(crate) fn maker_of(change: &automerge::Change) -> Option<String> {
    let parsed = parsed(change);
    let invoker = parsed.context.as_ref()?.invoker.as_ref()?;
    Some(invoker.actor.clone())
}

/// [`read`] of the context `context` that `change` records.
fn read_context(
    change: &(impl ChangeHead + ?Sized),
    context: &tilo_actions::ActionContext,
) -> Option<(Stamp, DynAction, Option<Run>)> {
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
    let run = run_of(context);
    let hash = change.hash().to_string();
    let stamp = Stamp {
        version: Version {
            who: action.authorship.invoker.clone(),
            at,
            action: action.action_id.clone(),
            run: run.clone(),
            change: hash,
            op: change.start_op(),
            from: action.authorship.from.clone(),
            suggested_by: action.authorship.suggested_by.clone(),
            count: action.authorship.count.max(1),
            gesture: context
                .gesture
                .map(|gesture| gesture.0.simple().to_string()),
            ..Version::default()
        },
    };
    Some((stamp, action, run))
}

/// The versions a resource's log holds that a view reads of it without reading the log: the
/// first and the last of them, and the last that changed each of its fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ends {
    /// The version that made it.
    pub first: Stamp,
    /// The version that last changed it.
    pub last: Stamp,
    /// What that version did to it.
    pub last_kind: VersionKind,
    /// The last version that changed each of its fields, by the field's id: a change of the
    /// record, never a step that takes one back (an undo), which leaves the field's last
    /// change the one still in effect. A field no version here names was last changed by the
    /// one that made the resource, which set it.
    pub fields: BTreeMap<String, Stamp>,
    /// The version that last put it in a collection or took it out of one, and which: a
    /// change of its fields after that leaves it where it is.
    pub moved: Option<(Stamp, VersionKind)>,
    /// Whether these are the resource's whole log read anew because a step took a change to it
    /// back: the index takes them as they are, even where they stand before what it holds.
    pub restored: bool,
}

impl Ends {
    /// The ends of a resource whose log holds no version yet, taking `stamp` as its first and
    /// last: what `fields` it changed, what it did (`kind`), and whether it placed it.
    #[must_use]
    pub(crate) fn of(stamp: &Stamp, fields: &[String], kind: VersionKind, moved: bool) -> Self {
        let version = stamp.of_fields(fields);
        Self {
            first: version.clone(),
            last: version,
            last_kind: kind,
            fields: fields
                .iter()
                .map(|field| (field.clone(), stamp.of_fields(std::slice::from_ref(field))))
                .collect(),
            moved: moved.then(|| (stamp.of_fields(fields), kind)),
            restored: false,
        }
    }

    /// Whether these are the ends of the same invocation's version as `other`: who made it, when
    /// and by which action, over how many items and from what. A version this device made before
    /// the record held it says the same of itself, though its causal place is
    /// [`PROVISIONAL`] and the change that made it is not known yet.
    #[must_use]
    pub fn same_invocation(&self, other: &Self) -> bool {
        self.last.same_invocation(&other.last)
    }

    /// The newest version of it, as a mark of the resource reads it: what it did to the
    /// resource said of the version itself.
    #[must_use]
    pub fn newest(&self) -> Stamp {
        Stamp {
            version: Version {
                kind: self.last_kind,
                ..self.last.version.clone()
            },
        }
    }

    /// Take in the version `stamp` made of the resource, changing `fields` and, when `places`,
    /// putting it in a collection or taking it out of one: the first stays unless `stamp` came
    /// before it, the last is replaced by a later one, and each field's last change is the
    /// later of the two.
    fn take(&mut self, stamp: &Stamp, fields: &[String], kind: VersionKind, places: bool) {
        let version = stamp.of_fields(fields);
        if version.before(&self.first) {
            self.first = version.clone();
        }
        if self.last.before(&version) {
            self.last = version.clone();
            self.last_kind = kind;
        }
        if places
            && self
                .moved
                .as_ref()
                .is_none_or(|(held, _)| held.before(&version))
        {
            self.moved = Some((version, kind));
        }
        for field in fields {
            let one = stamp.of_fields(std::slice::from_ref(field));
            match self.fields.get_mut(field) {
                Some(held) if held.before(&one) => *held = one,
                Some(_) => {}
                None => {
                    self.fields.insert(field.clone(), one);
                }
            }
        }
    }
}

/// The versions an action's change `change` made, each as the resource it is a version of,
/// what it changed of it and what it did: the resources its action changed, and the resource
/// it was made from, which the change is a version of too though of no field of its own. A
/// comment under a task changed that task, for whoever made or read it; the store's versions
/// note it the same way (`super::super::store::versions`).
fn made_of(change: &automerge::Change) -> Vec<(String, Vec<String>, VersionKind)> {
    let mut out: Vec<(String, Vec<String>, VersionKind)> = Vec::new();
    for version in versions_of(change) {
        if let Some(from) = &version.from {
            out.push((from.id_string(), Vec::new(), VersionKind::Changed));
        }
        out.push((version.resource, version.fields, version.kind));
    }
    out
}

/// The versions `changes` made of each resource they changed, with the last change of each of
/// its fields: a package action's (a field's last change), or a step on the record that is no
/// package's action (an undo, a revert) and changed no field, as it takes a change back
/// rather than making one. A change the record marks undone made no version.
///
/// A resource an action was made from is a version of that change too, of no field of its own:
/// a comment under a task changed that task, for whoever made or read it. The store's versions
/// note it the same way (`super::super::store::versions`).
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
            (stamp, made_of(change))
        } else {
            let Some(stamp) = step_stamp(change) else {
                continue;
            };
            let written = stepped(change, mirror, resources)
                .into_iter()
                .map(|version| (version.resource, Vec::new(), version.kind))
                .collect();
            (stamp, written)
        };
        let placed: BTreeSet<String> = placed_of(change)
            .into_iter()
            .map(|(item, _, _, _)| item)
            .collect();
        for (resource, fields, kind) in wrote {
            let moved = kind == VersionKind::Removed || placed.contains(&resource);
            match out.get_mut(&resource) {
                Some(end) => end.take(&stamp, &fields, kind, moved),
                None => {
                    out.insert(resource, Ends::of(&stamp, &fields, kind, moved));
                }
            }
        }
    }
    out
}

/// The changes `step` marked at the record's root ([`undone_changes`]), whether it marked one
/// undone or took the mark back (a redo): the changes whose resources the record holds as they
/// left them, or no longer does.
fn marked_by(step: &automerge::Change) -> Vec<String> {
    let mut out = Vec::new();
    for op in step.decode().operations {
        if op.obj != automerge::legacy::ObjectId::Root {
            continue;
        }
        if let automerge::legacy::Key::Map(key) = &op.key
            && let Some(hash) = key.strip_prefix(tilo_actions::UNDONE_KEY)
        {
            out.push(hash.to_owned());
        }
    }
    out
}

/// The changes the record marks undone (an undo, until a redo brings it back), by hash: a
/// derivation of the whole log leaves them out.
#[must_use]
pub fn undone_changes(mirror: &Doc) -> BTreeSet<String> {
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
    // undoes a removal writes nothing of the resource that comes back, and a step that makes
    // it again (a redo) only takes the record's marker of it back.
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for step in &steps {
        touched.extend(
            stepped(step, mirror, resources)
                .into_iter()
                .map(|version| version.resource),
        );
    }
    let marked: BTreeSet<String> = steps.iter().flat_map(|step| marked_by(step)).collect();
    for hash in undone.iter().chain(marked.iter()) {
        let Ok(hash) = hash.parse::<automerge::ChangeHash>() else {
            continue;
        };
        let Some(change) = mirror.committed_document().get_change_by_hash(&hash) else {
            continue;
        };
        match stamp_of(&change) {
            Some(_) => touched.extend(
                made_of(&change)
                    .into_iter()
                    .map(|(resource, _, _)| resource),
            ),
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

/// The version a step on the record that is no readable action of a package was made by (an
/// undo, a revert from History; [`STEP_ACTION`]), when `change` is one: who took it, when, and
/// which step. `None` for a change whose recorded action reads, and for one that is nobody's
/// step (an outside effect settling).
///
/// A change whose entry names a package action this device cannot decode is read as a step is:
/// its resources' versions come from what it wrote ([`stepped`]), so history still names the
/// change. An action keeps its large parts as content ([`tilo_actions::CONTENT_PART_THRESHOLD`]),
/// which this device has not held yet, or the package version that wrote it is not installed.
#[must_use]
pub fn step_stamp(change: &(impl ChangeHead + ?Sized)) -> Option<Stamp> {
    let parsed = parsed(change);
    let context = parsed.context.as_ref()?;
    if parsed.read.is_some() {
        return None;
    }
    let invoker = context.invoker.as_ref()?;
    let hash = change.hash().to_string();
    Some(Stamp {
        version: Version {
            who: invoker.actor.clone(),
            at: tilolang::calendar::time_text(change.timestamp()),
            // The entry names the action its change ran, which a change whose action does not
            // read has no other way to say: a verb's own, and a step's [`STEP_ACTION`].
            action: match context.what.as_ref() {
                Some(tilo_actions::Did::Verb(verb)) => verb.action.action.clone(),
                _ => STEP_ACTION.to_owned(),
            },
            run: run_of(context),
            change: hash,
            op: change.start_op(),
            count: 1,
            ..Version::default()
        },
    })
}

/// The versions a step on the record that is no action of a package made (an undo, a revert):
/// one per resource it wrote, by whoever took it, naming the fields it wrote.
#[must_use]
pub fn stepped(
    change: &(impl ChangeHead + ?Sized),
    mirror: &Doc,
    resources: &BTreeMap<DocId, (AppId, TypeId)>,
) -> Vec<Version> {
    let Some(stamp) = step_stamp(change) else {
        return Vec::new();
    };
    // What a step wrote is read off its operations, fetched for the steps alone.
    let Some(whole) = change.whole(mirror) else {
        return Vec::new();
    };
    let changed = crate::reads::changed_by(std::slice::from_ref(whole.as_ref()), mirror, resources);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(at: &str, op: u64, change: &str) -> Stamp {
        Stamp {
            version: Version {
                who: "ada".to_owned(),
                at: at.to_owned(),
                change: change.to_owned(),
                op,
                ..Version::default()
            },
        }
    }

    #[test]
    fn versions_order_by_when_they_happened_then_by_causal_place() {
        // Two people's changes made apart stand at the same place in the log, whichever of
        // them a replica received first: the later one happened is the later version.
        assert!(stamp("2026-10-28T09:30:00Z", 7, "bb").before(&stamp(
            "2026-10-28T10:00:00Z",
            7,
            "aa"
        )));
        assert!(stamp("2026-10-28T09:30:00Z", 9, "bb").before(&stamp(
            "2026-10-28T10:00:00Z",
            7,
            "aa"
        )));
        // One second holds both: the change made on top of the other counts past it.
        assert!(stamp("2026-10-28T09:00:00Z", 7, "zz").before(&stamp(
            "2026-10-28T09:00:00Z",
            9,
            "aa"
        )));
        // Neither is before the other: the hash orders them, the same on every replica.
        assert!(stamp("2026-10-28T09:00:00Z", 7, "aa").before(&stamp(
            "2026-10-28T09:00:00Z",
            7,
            "bb"
        )));
    }
}
