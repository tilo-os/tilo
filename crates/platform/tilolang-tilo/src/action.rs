//! [`DynAction`]: a tilolang action as a replayable tilo action.
//!
//! At the origin a tilolang action runs in wasm as a planner; its writes are recorded as
//! [`DynEdit`]s. The recorded action is plain data whose `apply` replays those edits without wasm,
//! so a peer that never loaded the module still verifies and applies the change. It is committed
//! through the framework executor, so the change carries the normal `ActionContext`: invoker,
//! group, encoded action. Undo and history work as for any app.

use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ReadDoc, Value as AmValue};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilo_actions::{
    Action, ActionError, ActionFacet, ActionKey, ActionRegistry, Described, Did, Doc,
    EncodedAction, Scalar,
};
use tilo_resource::{ActionCx, Edit, Fields, Model, ModelError, Segment, Slot};

use std::collections::BTreeMap;

use crate::record::DynRecord;
use tilolang_runtime::place::{AT_KEY, ITEMS_KEY, Placement};
use tilolang_runtime::store::VersionKind;

/// The registry type id every [`DynAction`] is encoded under.
pub const ACTION_TYPE: &str = "tl.action";

/// The key of the map in a resource that holds the migrations that ran on it, by action id.
pub const MIGRATED_KEY: &str = "$migrated";

/// The key of the map in the room record that holds every tilolang resource.
pub const RECORDS_KEY: &str = "tl";

/// The facet naming the authorship chain of a change, so history can say "you, via Alex's
/// view" (authority.md).
pub const AUTHORSHIP_FACET: &str = "tl.via";

/// The action id of a bound text input changing its field.
pub const EDIT_TEXT_ACTION: &str = "edit-text";

/// The action id of a bound checkbox changing its field.
pub const EDIT_FLAG_ACTION: &str = "edit-flag";

/// The action id of a resource the host creates on its own, outside any action: a package's
/// per-room root, the first time it is wanted ([`crate::store::TiloStore::create_with_id`]).
/// History reads it as the platform's creation of the resource.
pub const CREATE_ACTION: &str = tilo_actions::CREATE_ACTION;

/// Who is behind the code that planned an action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authorship {
    /// The person, agent task or app the code ran for.
    pub invoker: String,
    /// The content hashes of the code authors, outermost first: the module that wrote the
    /// expression that invoked the action, then the modules it passed through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<String>,
    /// The module hash that declares the action.
    pub module: String,
    /// When the action was invoked, recorded like an argument: what reflection reads as the
    /// time of the versions it made (a scenario's `as who at time` sets it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

impl Authorship {
    /// The facets that carry this chain in the change's `ActionContext`, so history can say
    /// "you, via Alex's view".
    #[must_use]
    pub fn facets(&self) -> Vec<ActionFacet> {
        self.chain
            .iter()
            .map(|module| ActionFacet::new(AUTHORSHIP_FACET, module.as_str()))
            .collect()
    }
}

/// One write of a planned action, addressed to a resource of the action's app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum DynEdit {
    /// A new resource: `record` is its JSON form ([`DynRecord::to_json`]), `$type` included.
    Create { id: String, record: Json },
    /// A field edit below the resource `id`. The edit's path starts at a field of the record,
    /// or at `Item(item)` for an item of it stored by its id ([`crate::items`]); an item of a
    /// record written before items were flat is reached by the path it is nested at.
    Edit { id: String, edit: Edit<DynRecord> },
    /// A new item `item` of the resource `id`, stored by its id: `record` is its JSON form
    /// ([`DynRecord::to_json`]), `at` its placement as the item's `$at` holds it
    /// ([`tilolang_runtime::place::Placement`]).
    Insert {
        id: String,
        item: String,
        record: Json,
        at: String,
    },
    /// Write the placement `at` of the item `item` of the resource `id`: moved (from wherever it
    /// was, keeping its id), or removed with everything in it. An item of a record written before
    /// items were flat is found at `nested`, and its map made (of the type `ty`) when it was
    /// stored as its order key alone. `holders` are the resources whose collections it left or
    /// joined, for history.
    Place {
        id: String,
        item: String,
        at: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        nested: Vec<(String, String)>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        ty: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        holders: Vec<String>,
    },
    /// Remove the resource.
    Remove { id: String },
    /// Queue an outside effect on the resource `id`. The entry waits in the record, under the
    /// key `entry`, for the device `owner` to carry it out. Queued again under the same key it is
    /// the same entry, so what is approved twice (on two devices, or twice on one) is queued once.
    OutboxPush {
        id: String,
        entry: String,
        capability: String,
        payload: Json,
        owner: String,
    },
    /// Record how the owning device settled the entry `entry` of the resource `id`: `done`,
    /// `rejected` (with the service's `reason`) or `unknown`.
    OutboxSettle {
        id: String,
        entry: String,
        outcome: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
    },
    /// Record where the owning device is with the entry `entry` of the resource `id` before it
    /// settles: `sending` (handed over by the process whose token `detail` is) or `waiting` (to
    /// go online, `detail` saying why).
    OutboxMark {
        id: String,
        entry: String,
        state: String,
        detail: String,
    },
    /// An edit of a resource of another app than the action's: the host's own `Invocation`
    /// records, which an action's follow-up changes write beside the package's resources.
    In { app: String, edit: Box<DynEdit> },
    /// Record that the migration `migration` (its action id) ran on the resource `id`, in the
    /// same change as what it did, so it runs once on each resource an earlier version wrote.
    Migrated { id: String, migration: String },
}

impl DynEdit {
    /// The same edit, addressed to the resources of `app`.
    #[must_use]
    pub fn in_app(self, app: &str) -> Self {
        Self::In {
            app: app.to_owned(),
            edit: Box::new(self),
        }
    }

    /// Whether `next`, made by the same person straight after this edit, continues the same
    /// gesture: an edit of the same field of the same resource that the field's kind says goes
    /// on from this one (typing on in a word, erasing backwards, setting one value again).
    #[must_use]
    pub fn continues(&self, next: &Self) -> bool {
        match (self, next) {
            (
                Self::Edit { id, edit },
                Self::Edit {
                    id: next_id,
                    edit: next_edit,
                },
            ) => id == next_id && edit.coalesces(next_edit),
            (
                Self::In { app, edit },
                Self::In {
                    app: next_app,
                    edit: next_edit,
                },
            ) => app == next_app && edit.continues(next_edit),
            _ => false,
        }
    }

    /// Whether the edit queues an outside effect, rather than only changing the record.
    #[must_use]
    pub fn reaches_outside(&self) -> bool {
        match self {
            Self::OutboxPush { .. } => true,
            Self::In { edit, .. } => edit.reaches_outside(),
            Self::Create { .. }
            | Self::Edit { .. }
            | Self::Insert { .. }
            | Self::Place { .. }
            | Self::Remove { .. }
            | Self::OutboxSettle { .. }
            | Self::OutboxMark { .. }
            | Self::Migrated { .. } => false,
        }
    }
}

/// What an action did to one resource ([`DynAction::made`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Made {
    /// The resource, as the runtime writes it (`doc` or `doc/item`).
    pub resource: String,
    pub kind: VersionKind,
    /// The fields it changed, by id, for a resource it changed.
    pub fields: Vec<String>,
}

/// A tilolang action as recorded: what was invoked, by whom, and exactly what it wrote.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DynAction {
    pub app: String,
    /// The resource kind the action is published on (`tl.<app>.<Type>`).
    pub kind: String,
    pub action_id: String,
    /// The invocation arguments, as plain JSON, for history and agents.
    pub args: Json,
    pub authorship: Authorship,
    pub edits: Vec<DynEdit>,
}

fn refused(error: &ModelError) -> ActionError {
    ActionError::Refused(error.to_string())
}

/// The map under `key` of `parent`, made when absent.
fn ensure_map(
    doc: &mut Doc,
    parent: &automerge::ObjId,
    key: &str,
) -> Result<automerge::ObjId, ActionError> {
    if let Some((AmValue::Object(ObjType::Map), id)) = doc.get(parent, key)? {
        return Ok(id);
    }
    Ok(doc.put_object(parent, key, ObjType::Map)?)
}

impl DynAction {
    /// What this action is, by what it writes: one that queues an outside effect reaches outside
    /// the record, as the package action that planned it is declared to; every other one is an
    /// undoable change of the record. What a room's grants must allow to commit it.
    #[must_use]
    pub fn need(&self) -> tilo_actions::ActionNeed {
        tilo_actions::ActionNeed::of(if self.edits.iter().any(DynEdit::reaches_outside) {
            tilo_actions::ActionKind::Outside
        } else {
            tilo_actions::ActionKind::Undoable
        })
    }

    /// Whether `next`, invoked by the same person straight after this action, continues the same
    /// gesture, and so is one undo step with it: the same action (a bound field's edit) making
    /// one edit that goes on from this one's ([`DynEdit::continues`]). The keystrokes of a word
    /// typed into a field are one step, as in every other app.
    #[must_use]
    pub fn continues(&self, next: &Self) -> bool {
        let ([edit], [next_edit]) = (self.edits.as_slice(), next.edits.as_slice()) else {
            return false;
        };
        self.app == next.app
            && self.kind == next.kind
            && self.action_id == next.action_id
            // By the same code for the same person; when each keystroke was made differs.
            && self.authorship.invoker == next.authorship.invoker
            && self.authorship.chain == next.authorship.chain
            && self.authorship.module == next.authorship.module
            && edit.continues(next_edit)
    }

    /// Encode under [`ACTION_TYPE`], the framework's action encoding.
    ///
    /// # Errors
    /// The action does not serialize.
    pub fn encode(&self) -> Result<EncodedAction, ActionError> {
        EncodedAction::encode(ACTION_TYPE, self)
    }

    /// The resources this action changed, as the runtime writes them (`doc` or `doc/item`):
    /// each one it edited, each item it inserted, moved or removed, and the resources whose
    /// collections those joined or left. What reflection reads a resource's versions from. An
    /// edit an older action made of a nested item names it by its id as well.
    #[must_use]
    pub fn changed(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |id: String| {
            if !out.contains(&id) {
                out.push(id);
            }
        };
        for edit in &self.edits {
            let mut edit = edit;
            while let DynEdit::In { edit: inner, .. } = edit {
                edit = inner;
            }
            match edit {
                DynEdit::Create { id, .. } => add(id.clone()),
                DynEdit::Edit { id, edit } => {
                    // The resource is the last item the path goes down to; the field after it
                    // is the one edited.
                    let mut resource = id.clone();
                    for segment in edit.path() {
                        if let Segment::Item(item) = segment {
                            resource = format!("{id}/{item}");
                        }
                    }
                    add(resource);
                    if let tilo_resource::EditOp::InsertItem { id: Some(item), .. } = edit.op() {
                        add(format!("{id}/{item}"));
                    }
                }
                DynEdit::Insert { id, item, at, .. } => {
                    if let Some(place) = Placement::decode(at).as_ref().and_then(Placement::place) {
                        add(holder_of(id, place));
                    }
                    add(format!("{id}/{item}"));
                }
                DynEdit::Place {
                    id, item, holders, ..
                } => {
                    for holder in holders {
                        add(holder.clone());
                    }
                    add(format!("{id}/{item}"));
                }
                DynEdit::Remove { .. }
                | DynEdit::OutboxPush { .. }
                | DynEdit::OutboxSettle { .. }
                | DynEdit::OutboxMark { .. }
                | DynEdit::Migrated { .. }
                | DynEdit::In { .. } => {}
            }
        }
        out
    }

    /// What this action did to each resource it changed (as the runtime writes them), in order
    /// of first mention: made it, changed which of its fields, or removed it. A removed
    /// resource has a version too, so who removed it stays answerable; a resource it made and
    /// changed in one go was made.
    #[must_use]
    pub fn made(&self) -> Vec<Made> {
        let mut out: Vec<Made> = Vec::new();
        let mut add = |resource: String, kind: VersionKind, field: Option<String>| {
            if !out.iter().any(|m| m.resource == resource) {
                out.push(Made {
                    resource: resource.clone(),
                    kind,
                    fields: Vec::new(),
                });
            }
            let Some(made) = out.iter_mut().find(|m| m.resource == resource) else {
                return;
            };
            // Made, then removed in one go, is removed; made, then changed, stays made.
            match (made.kind, kind) {
                (_, VersionKind::Removed) => made.kind = VersionKind::Removed,
                (VersionKind::Changed, VersionKind::Created) => made.kind = VersionKind::Created,
                _ => {}
            }
            if let Some(field) = field
                && !made.fields.contains(&field)
            {
                made.fields.push(field);
            }
        };
        for edit in &self.edits {
            let mut edit = edit;
            while let DynEdit::In { edit: inner, .. } = edit {
                edit = inner;
            }
            match edit {
                DynEdit::Create { id, .. } => add(id.clone(), VersionKind::Created, None),
                DynEdit::Edit { id, edit } => {
                    // The resource is the last item the path goes down to; the field after it
                    // is the one edited.
                    let mut resource = id.clone();
                    let mut field = None;
                    for segment in edit.path() {
                        match segment {
                            Segment::Item(item) => {
                                resource = format!("{id}/{item}");
                                field = None;
                            }
                            Segment::Field(name) if field.is_none() => {
                                field = Some(name.clone());
                            }
                            _ => {}
                        }
                    }
                    if let tilo_resource::EditOp::InsertItem { id: Some(item), .. } = edit.op() {
                        add(format!("{id}/{item}"), VersionKind::Created, None);
                    }
                    add(resource, VersionKind::Changed, field);
                }
                DynEdit::Insert { id, item, at, .. } => {
                    if let Some(place) = Placement::decode(at).as_ref().and_then(Placement::place) {
                        add(
                            holder_of(id, place),
                            VersionKind::Changed,
                            Some(place.field.clone()),
                        );
                    }
                    add(format!("{id}/{item}"), VersionKind::Created, None);
                }
                DynEdit::Place {
                    id,
                    item,
                    at,
                    holders,
                    ..
                } => {
                    let placed = Placement::decode(at);
                    let field = placed
                        .as_ref()
                        .and_then(Placement::place)
                        .map(|place| place.field.clone());
                    for holder in holders {
                        add(holder.clone(), VersionKind::Changed, field.clone());
                    }
                    let kind = if placed.as_ref().is_some_and(|p| p.place().is_none()) {
                        VersionKind::Removed
                    } else {
                        VersionKind::Changed
                    };
                    add(format!("{id}/{item}"), kind, None);
                }
                DynEdit::Remove { id } => add(id.clone(), VersionKind::Removed, None),
                DynEdit::OutboxPush { .. }
                | DynEdit::OutboxSettle { .. }
                | DynEdit::OutboxMark { .. }
                | DynEdit::Migrated { .. }
                | DynEdit::In { .. } => {}
            }
        }
        // A version that made or removed a resource names no fields.
        for made in &mut out {
            if made.kind != VersionKind::Changed {
                made.fields.clear();
            }
        }
        out
    }

    /// The resource ids this action writes, in order of first mention.
    #[must_use]
    pub fn resources(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        for edit in &self.edits {
            let mut edit = edit;
            while let DynEdit::In { edit: inner, .. } = edit {
                edit = inner;
            }
            let (DynEdit::Create { id, .. }
            | DynEdit::Edit { id, .. }
            | DynEdit::Insert { id, .. }
            | DynEdit::Place { id, .. }
            | DynEdit::Remove { id }
            | DynEdit::OutboxPush { id, .. }
            | DynEdit::OutboxSettle { id, .. }
            | DynEdit::OutboxMark { id, .. }
            | DynEdit::Migrated { id, .. }) = edit
            else {
                continue;
            };
            if !ids.contains(&id.as_str()) {
                ids.push(id.as_str());
            }
        }
        ids
    }
}

/// Apply `edits` to the resources of `app` in `doc`, creating the maps they live in.
///
/// `seed` pins the ids a minting edit would draw: edits carry their own ids, so it only has to
/// be the same on every replica.
///
/// # Errors
/// An edit names a resource that is not there, or does not fit its field.
pub fn apply_edits(
    doc: &mut Doc,
    app: &str,
    edits: &[DynEdit],
    seed: &[u8],
) -> Result<(), ActionError> {
    if edits.is_empty() {
        return Ok(());
    }
    let mut cx = ActionCx::for_write(doc, seed);
    let records = ensure_map(doc, &ROOT, RECORDS_KEY)?;
    for edit in edits {
        apply_one(doc, &records, app, edit, &mut cx)?;
    }
    Ok(())
}

/// The records map `edit` writes in: where its resource was stored, under the app's id or under
/// an id the app had before (`previously:`); what is new goes under the app's id.
fn target_map(
    doc: &mut Doc,
    records: &automerge::ObjId,
    app: &str,
    edit: &DynEdit,
) -> Result<automerge::ObjId, ActionError> {
    let held = match edit {
        DynEdit::Create { .. } | DynEdit::In { .. } => None,
        DynEdit::Edit { id, .. }
        | DynEdit::Insert { id, .. }
        | DynEdit::Place { id, .. }
        | DynEdit::Remove { id }
        | DynEdit::OutboxPush { id, .. }
        | DynEdit::OutboxSettle { id, .. }
        | DynEdit::OutboxMark { id, .. }
        | DynEdit::Migrated { id, .. } => stored_map(doc, records, app, id),
    };
    match held {
        Some(map) => Ok(map),
        None => ensure_map(doc, records, app),
    }
}

fn apply_one(
    doc: &mut Doc,
    records: &automerge::ObjId,
    app: &str,
    edit: &DynEdit,
    cx: &mut ActionCx,
) -> Result<(), ActionError> {
    if let DynEdit::In { app, edit } = edit {
        return apply_one(doc, records, app, edit, cx);
    }
    let app = target_map(doc, records, app, edit)?;
    match edit {
        DynEdit::Create { id, record } => {
            let record = DynRecord::from_json(record).map_err(|error| refused(&error))?;
            store_flat(doc, &app, id, record)?;
        }
        DynEdit::Edit { id, edit } => {
            let Some((AmValue::Object(ObjType::Map), obj)) = doc.get(&app, id.as_str())? else {
                return Err(ActionError::Refused(format!("there is no resource `{id}`")));
            };
            let (obj, path) = match edit.path().split_first() {
                // An item stored by its id.
                Some((Segment::Item(item), rest)) => (flat_item(doc, &obj, item.as_str())?, rest),
                _ => (obj, edit.path()),
            };
            DynRecord::edit_fields(doc, &obj, path, edit.op(), cx)
                .map_err(|error| refused(&error))?;
        }
        DynEdit::Insert {
            id,
            item,
            record,
            at,
        } => {
            let Some((AmValue::Object(ObjType::Map), obj)) = doc.get(&app, id.as_str())? else {
                return Err(ActionError::Refused(format!("there is no resource `{id}`")));
            };
            let items = ensure_map(doc, &obj, ITEMS_KEY)?;
            let mut taken = Taken::new();
            let record = DynRecord::from_json(record).map_err(|error| refused(&error))?;
            let record = take_items(record, Some(item.as_str()), &mut taken);
            record
                .store(doc, Slot::Key(&items, item))
                .map_err(|error| refused(&error))?;
            let held = flat_item(doc, &obj, item)?;
            doc.put(&held, AT_KEY, at.as_str())?;
            store_taken(doc, &obj, taken)?;
        }
        DynEdit::Place {
            id,
            item,
            at,
            nested,
            ty,
            ..
        } => {
            let Some((AmValue::Object(ObjType::Map), obj)) = doc.get(&app, id.as_str())? else {
                return Err(ActionError::Refused(format!("there is no resource `{id}`")));
            };
            let held = if nested.is_empty() {
                flat_item(doc, &obj, item)?
            } else {
                nested_item(doc, &obj, nested, ty)?
            };
            doc.put(&held, AT_KEY, at.as_str())?;
        }
        DynEdit::Remove { id } => {
            if doc.get(&app, id.as_str())?.is_some() {
                doc.delete(&app, id.as_str())?;
            }
        }
        DynEdit::OutboxPush {
            id,
            entry,
            capability,
            payload,
            owner,
        } => {
            let Some((AmValue::Object(ObjType::Map), obj)) = doc.get(&app, id.as_str())? else {
                return Err(ActionError::Refused(format!("there is no resource `{id}`")));
            };
            let outbox = ensure_map(doc, &obj, OUTBOX_KEY)?;
            // The same entry queued again is the same entry: it keeps what became of it.
            if doc.get(&outbox, entry.as_str())?.is_none() {
                let held = doc.put_object(&outbox, entry.as_str(), ObjType::Map)?;
                doc.put(&held, "capability", capability.as_str())?;
                doc.put(&held, "payload", payload.to_string())?;
                doc.put(&held, "owner", owner.as_str())?;
            }
        }
        DynEdit::OutboxSettle { .. } | DynEdit::OutboxMark { .. } => {
            write_outbox_entry(doc, &app, edit)?;
        }
        DynEdit::Migrated { id, migration } => mark_migrated(doc, &app, id, migration)?,
        DynEdit::In { .. } => {}
    }
    Ok(())
}

/// Record in the resource `id` (in the app's records map `app`) that the migration `migration`
/// ran on it.
fn mark_migrated(
    doc: &mut Doc,
    app: &automerge::ObjId,
    id: &str,
    migration: &str,
) -> Result<(), ActionError> {
    let Some((AmValue::Object(ObjType::Map), obj)) = doc.get(app, id)? else {
        return Err(ActionError::Refused(format!("there is no resource `{id}`")));
    };
    // `require not migrated`: where actions are applied in order (the origin), a second run of
    // the same migration on the resource is refused, not applied twice.
    if migrated(doc, &obj, migration) {
        return Err(ActionError::Refused(format!(
            "the migration `{migration}` already ran on `{id}`"
        )));
    }
    let done = ensure_map(doc, &obj, MIGRATED_KEY)?;
    doc.put(&done, migration, true)?;
    Ok(())
}

/// Whether the migration `migration` (its action id) ran on the resource at the map `obj`.
#[must_use]
pub fn migrated(doc: &Doc, obj: &automerge::ObjId, migration: &str) -> bool {
    map_in(doc, obj, MIGRATED_KEY)
        .is_some_and(|done| doc.get(&done, migration).ok().flatten().is_some())
}

/// The items of each collection of a record being stored, in order, by the item holding the
/// collection (`None`: the record) and its field.
type Taken = BTreeMap<(Option<String>, String), Vec<(String, DynRecord)>>;

/// `record` without the items of its collections, which go into `out`, theirs with them.
fn take_items(mut record: DynRecord, holder: Option<&str>, out: &mut Taken) -> DynRecord {
    let fields: Vec<String> = record
        .schema()
        .fields
        .iter()
        .filter(|f| f.kind == crate::schema::MergeKind::Collection)
        .map(|f| f.id.clone())
        .collect();
    for field in fields {
        let Some(crate::record::DynValue::Collection(items)) = record.get_mut(&field) else {
            continue;
        };
        let items = std::mem::take(items);
        for (id, item) in items.iter() {
            let item = take_items(item.clone(), Some(id.as_str()), out);
            out.entry((holder.map(str::to_owned), field.clone()))
                .or_default()
                .push((id.as_str().to_owned(), item));
        }
    }
    record
}

/// The order keys of `count` items placed in a collection in order, as a record created with
/// them places them.
#[must_use]
pub fn keys_in_order(count: usize) -> Vec<String> {
    tilo_resource::order::sequence(None, None, count)
}

/// Store `record` as the resource `id` of the app map `app`, every item of its collections (and
/// of theirs) stored by its id beside it and placed where it is ([`crate::items`]).
fn store_flat(
    doc: &mut Doc,
    app: &automerge::ObjId,
    id: &str,
    record: DynRecord,
) -> Result<(), ActionError> {
    let mut taken = Taken::new();
    let record = take_items(record, None, &mut taken);
    record
        .store(doc, Slot::Key(app, id))
        .map_err(|error| refused(&error))?;
    if taken.is_empty() {
        return Ok(());
    }
    let Some((AmValue::Object(ObjType::Map), obj)) = doc.get(app, id)? else {
        return Err(ActionError::Refused(format!("there is no resource `{id}`")));
    };
    store_taken(doc, &obj, taken)
}

/// Store the items `taken` of the record map `obj` by their ids, each placed in order in the
/// collection it was taken from.
fn store_taken(doc: &mut Doc, obj: &automerge::ObjId, taken: Taken) -> Result<(), ActionError> {
    if taken.is_empty() {
        return Ok(());
    }
    let items = ensure_map(doc, obj, ITEMS_KEY)?;
    for ((parent, field), placed) in taken {
        let keys = keys_in_order(placed.len());
        for ((item, value), key) in placed.into_iter().zip(keys) {
            value
                .store(doc, Slot::Key(&items, &item))
                .map_err(|error| refused(&error))?;
            let held = flat_item(doc, obj, &item)?;
            let at = Placement::At {
                place: tilolang_runtime::place::Place {
                    parent: parent.clone(),
                    field: field.clone(),
                    key,
                },
                was: None,
            };
            doc.put(&held, AT_KEY, at.encode())?;
        }
    }
    Ok(())
}

/// The map of the item `item` of the record map `record`, stored by its id.
fn flat_item(
    doc: &Doc,
    record: &automerge::ObjId,
    item: &str,
) -> Result<automerge::ObjId, ActionError> {
    if let Some((AmValue::Object(ObjType::Map), items)) = doc.get(record, ITEMS_KEY)?
        && let Some((AmValue::Object(ObjType::Map), held)) = doc.get(&items, item)?
    {
        return Ok(held);
    }
    Err(ActionError::Refused(format!("there is no item `{item}`")))
}

/// The map of an item of a record written before items were flat, nested at `nesting` below
/// the record map `record`: made (of the type `ty`) for an item stored as its order key alone.
fn nested_item(
    doc: &mut Doc,
    record: &automerge::ObjId,
    nesting: &[(String, String)],
    ty: &str,
) -> Result<automerge::ObjId, ActionError> {
    let missing = || ActionError::Refused("the item is not where it was nested".into());
    let mut obj = record.clone();
    for (at, (field, item)) in nesting.iter().enumerate() {
        let Some((AmValue::Object(ObjType::Map), coll)) = doc.get(&obj, field.as_str())? else {
            return Err(missing());
        };
        let entry = match doc.get(&coll, item.as_str())? {
            Some((AmValue::Object(ObjType::Map), entry)) => entry,
            // Its order key alone: the entry becomes a map holding it.
            Some((AmValue::Scalar(key), _)) => {
                let key = key.into_owned();
                let entry = doc.put_object(&coll, item.as_str(), ObjType::Map)?;
                doc.put(&entry, ORDER_KEY, key)?;
                entry
            }
            _ => return Err(missing()),
        };
        obj = match doc.get(&entry, VALUE_KEY)? {
            Some((AmValue::Object(ObjType::Map), value)) => value,
            _ if at.saturating_add(1) == nesting.len() => {
                let value = doc.put_object(&entry, VALUE_KEY, ObjType::Map)?;
                doc.put(&value, crate::schema::TYPE_KEY, ty)?;
                value
            }
            _ => return Err(missing()),
        };
    }
    Ok(obj)
}

/// The resource whose collection the place `place` of an item of the record `doc` names.
fn holder_of(doc: &str, place: &tilolang_runtime::place::Place) -> String {
    match &place.parent {
        Some(parent) => format!("{doc}/{parent}"),
        None => doc.to_owned(),
    }
}

/// The key of the order key in a nested collection's entry.
const ORDER_KEY: &str = "k";
/// The key of the item's map in a nested collection's entry.
const VALUE_KEY: &str = "v";

/// The key of the map of outside effects waiting on a resource.
pub const OUTBOX_KEY: &str = "$outbox";

/// An outside effect queued on a resource, as the record holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredEntry {
    /// The key it is queued under.
    pub entry: String,
    pub capability: String,
    pub payload: Json,
    /// The device that carries it out.
    pub owner: String,
    /// `None` while it waits; `done`, `rejected` or `unknown` once settled.
    pub settled: Option<String>,
    /// Why it was rejected, as the service said.
    pub reason: String,
    /// Before it settles: empty (queued), `sending` or `waiting`.
    pub state: String,
    /// The token of the process handing it over, or why it waits.
    pub detail: String,
}

impl StoredEntry {
    /// Where it is on its way out, before it settles.
    #[must_use]
    pub fn outbox_state(&self) -> tilolang_runtime::store::OutboxState {
        use tilolang_runtime::store::OutboxState;
        match self.state.as_str() {
            "sending" => OutboxState::Sending {
                by: self.detail.clone(),
            },
            "waiting" => OutboxState::Waiting {
                reason: self.detail.clone(),
            },
            _ => OutboxState::Queued,
        }
    }

    /// How it stands, as `delivery(r)` reads it.
    #[must_use]
    pub fn delivery(&self) -> tilolang_runtime::store::DeliveryState {
        tilolang_runtime::store::DeliveryState::of(
            &self.outbox_state(),
            self.settled
                .as_deref()
                .map(|outcome| (outcome, self.reason.as_str())),
        )
    }
}

/// Record how an outbox entry was settled, or where it is before it settles.
fn write_outbox_entry(
    doc: &mut Doc,
    app: &automerge::ObjId,
    edit: &DynEdit,
) -> Result<(), automerge::AutomergeError> {
    match edit {
        DynEdit::OutboxSettle {
            id,
            entry,
            outcome,
            reason,
        } => {
            if let Some(held) = outbox_entry(doc, app, id, entry)? {
                doc.put(&held, "settled", outcome.as_str())?;
                if !reason.is_empty() {
                    doc.put(&held, "reason", reason.as_str())?;
                }
            }
        }
        DynEdit::OutboxMark {
            id,
            entry,
            state,
            detail,
        } => {
            if let Some(held) = outbox_entry(doc, app, id, entry)? {
                doc.put(&held, "state", state.as_str())?;
                doc.put(&held, "detail", detail.as_str())?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The map of the entry `entry` queued on the resource `id` under the app's map `app`.
fn outbox_entry(
    doc: &Doc,
    app: &automerge::ObjId,
    id: &str,
    entry: &str,
) -> Result<Option<automerge::ObjId>, automerge::AutomergeError> {
    let Some((AmValue::Object(ObjType::Map), obj)) = doc.get(app, id)? else {
        return Ok(None);
    };
    let Some((AmValue::Object(ObjType::Map), outbox)) = doc.get(&obj, OUTBOX_KEY)? else {
        return Ok(None);
    };
    Ok(match doc.get(&outbox, entry)? {
        Some((AmValue::Object(ObjType::Map), held)) => Some(held),
        _ => None,
    })
}

/// The outside effects queued on the resource `id` of `app`, oldest key first.
#[must_use]
pub fn outbox_entries(doc: &Doc, app: &str, id: &str) -> Vec<StoredEntry> {
    let text = |obj: &automerge::ObjId, key: &str| -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((AmValue::Scalar(scalar), _))) => match scalar.as_ref() {
                automerge::ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        }
    };
    let Some(obj) = crate::reads::resource_obj(doc, app, id) else {
        return Vec::new();
    };
    let Ok(Some((AmValue::Object(ObjType::Map), outbox))) = doc.get(&obj, OUTBOX_KEY) else {
        return Vec::new();
    };
    let mut keys: Vec<String> = doc.keys(&outbox).collect();
    keys.sort();
    keys.into_iter()
        .filter_map(|entry| {
            let Ok(Some((AmValue::Object(ObjType::Map), held))) = doc.get(&outbox, entry.as_str())
            else {
                tracing::warn!(
                    app,
                    id,
                    entry,
                    "an outbox entry is not a map: it is not read"
                );
                return None;
            };
            let read = || -> Result<(String, Json, String), String> {
                let capability = text(&held, "capability").ok_or("it names no capability")?;
                let payload = text(&held, "payload").ok_or("it holds no payload")?;
                let payload = serde_json::from_str(&payload)
                    .map_err(|error| format!("its payload does not decode: {error}"))?;
                let owner = text(&held, "owner").ok_or("it names no device to carry it out")?;
                Ok((capability, payload, owner))
            };
            let settled = text(&held, "settled");
            let mut stored = StoredEntry {
                capability: String::new(),
                payload: Json::Null,
                owner: String::new(),
                settled,
                // Absent until it is settled, and when settled with no reason.
                reason: text(&held, "reason").unwrap_or_default(),
                // Absent while it is queued (`outbox_state`).
                state: text(&held, "state").unwrap_or_default(),
                detail: text(&held, "detail").unwrap_or_default(),
                entry,
            };
            match read() {
                Ok((capability, payload, owner)) => {
                    stored.capability = capability;
                    stored.payload = payload;
                    stored.owner = owner;
                }
                // Nothing can carry out an effect that does not read, so it never would
                // settle: it stands rejected, with why, where `delivery(r)` reads it, and no
                // device takes it up.
                Err(why) if stored.settled.is_none() => {
                    tracing::warn!(
                        app,
                        id,
                        entry = stored.entry,
                        why,
                        "an outbox entry is unreadable"
                    );
                    stored.settled = Some("rejected".to_owned());
                    stored.reason = format!("the queued effect is unreadable: {why}");
                }
                Err(_) => {}
            }
            Some(stored)
        })
        .collect()
}

impl Action for DynAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let payload =
            serde_json::to_vec(self).map_err(|error| ActionError::Refused(error.to_string()))?;
        apply_edits(doc, &self.app, &self.edits, &payload)
    }

    fn describe(&self, doc: &Doc) -> Option<Described> {
        let args = self
            .args
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| Some((name.clone(), Scalar::from_json(value)?)));
        let key = ActionKey::new(
            self.app.as_str(),
            self.kind.as_str(),
            self.action_id.as_str(),
        );
        let described = Described::new(Did::verb(key, args));
        // History names the resource as it reads, by its title and never by its id: the item
        // the action wrote (a note of the notebook), else the record; one without a title yet
        // by its type ("Created Notebook", "Pin: Note").
        let Some((at, schema, title)) = self.subject(doc) else {
            return Some(described);
        };
        Some(described.on(at, title.unwrap_or_else(|| schema.spoken_name())))
    }
}

impl DynAction {
    /// The resource this action writes first, as the record reads before it applies: its
    /// published id (an item of a record by its path), its type, and its title, the first text
    /// among its own fields. A resource the action creates is read from the record it creates.
    fn subject(
        &self,
        doc: &Doc,
    ) -> Option<(String, &'static crate::schema::TypeSchema, Option<String>)> {
        let mut edit = self.edits.first()?;
        while let DynEdit::In { edit: inner, .. } = edit {
            edit = inner;
        }
        let (id, path) = match edit {
            DynEdit::Create { id, record } => {
                let schema = record
                    .get(crate::schema::TYPE_KEY)
                    .and_then(Json::as_str)
                    .and_then(crate::schema::lookup)?;
                let title = own_text(schema, |field| {
                    self.written_text(id, None, field)
                        .or_else(|| record.get(field).and_then(Json::as_str).map(str::to_owned))
                });
                return Some((id.clone(), schema, title));
            }
            DynEdit::Insert {
                id, item, record, ..
            } => {
                let schema = record
                    .get(crate::schema::TYPE_KEY)
                    .and_then(Json::as_str)
                    .and_then(crate::schema::lookup)?;
                let title = own_text(schema, |field| {
                    self.written_text(id, Some(item), field)
                        .or_else(|| record.get(field).and_then(Json::as_str).map(str::to_owned))
                });
                return Some((format!("{id}/{item}"), schema, title));
            }
            DynEdit::Edit { id, edit } => {
                // The last item the path goes down to.
                let item = edit.path().iter().rev().find_map(|segment| match segment {
                    Segment::Item(item) => Some(item.as_str().to_owned()),
                    _ => None,
                });
                (id, item)
            }
            DynEdit::Place { id, item, .. } => (id, Some(item.clone())),
            DynEdit::Remove { id }
            | DynEdit::OutboxPush { id, .. }
            | DynEdit::OutboxSettle { id, .. }
            | DynEdit::OutboxMark { id, .. }
            | DynEdit::Migrated { id, .. } => (id, None),
            DynEdit::In { .. } => return None,
        };
        let root = crate::reads::resource_obj(doc, &self.app, id)?;
        let root_ty = crate::reads::type_at(doc, &root)?;
        let at = tilolang_runtime::value::ResourceRef {
            doc: id.clone(),
            item: path,
            ty: String::new(),
        };
        let located = crate::reads::locate_ref(
            doc,
            (&self.app, &root_ty),
            &at,
            &crate::items::Nested::default(),
        )
        .ok()?;
        let title = located.obj.and_then(|obj| {
            own_text(located.schema, |field| {
                tilo_resource::read_key::<String>(doc, &obj, field).ok()
            })
        });
        Some((at.id_string(), located.schema, title))
    }
}

impl DynAction {
    /// The text this action writes into the `field` of the resource `id` (of its item `item`),
    /// the last write winning: what a resource it makes is titled once it applies, since a new
    /// item is stored empty and its fields are written after it.
    fn written_text(&self, id: &str, item: Option<&str>, field: &str) -> Option<String> {
        self.edits.iter().rev().find_map(|edit| {
            let DynEdit::Edit { id: edited, edit } = edit else {
                return None;
            };
            if edited != id {
                return None;
            }
            let (Segment::Field(written), holder) = edit.path().split_last()? else {
                return None;
            };
            if written != field {
                return None;
            }
            let held = match holder.last() {
                Some(Segment::Item(held)) => Some(held.as_str()),
                Some(_) => return None,
                None => None,
            };
            if held != item {
                return None;
            }
            match edit.op() {
                tilo_resource::EditOp::Replace { text } => Some(text.clone()),
                tilo_resource::EditOp::Set { value } => value.as_str().map(str::to_owned),
                _ => None,
            }
        })
    }
}

/// The first non-empty text among the `Text` fields of `schema`, by field id, each read by
/// `read`.
fn own_text(
    schema: &crate::schema::TypeSchema,
    read: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut fields: Vec<_> = schema
        .fields
        .iter()
        .filter(|field| field.kind == crate::schema::MergeKind::Text)
        .collect();
    fields.sort_by(|a, b| a.id.cmp(&b.id));
    fields
        .into_iter()
        .filter_map(|field| read(&field.id))
        .find(|text| !text.trim().is_empty())
}

/// Register the decoder of [`DynAction`], so a replaying peer reconstructs it with no wasm.
pub fn register_actions(registry: &mut ActionRegistry) {
    registry.register::<DynAction>(ACTION_TYPE);
}

fn map_in(doc: &Doc, obj: &automerge::ObjId, key: &str) -> Option<automerge::ObjId> {
    match doc.get(obj, key).ok()?? {
        (AmValue::Object(ObjType::Map), id) => Some(id),
        _ => None,
    }
}

/// The records maps of `app` in `doc` that anything was written to: under its id, then under
/// each id it was published as before (`previously:`), whose records are its own.
#[must_use]
pub fn app_records(doc: &Doc, app: &str) -> Vec<automerge::ObjId> {
    let Some(records) = map_in(doc, &ROOT, RECORDS_KEY) else {
        return Vec::new();
    };
    crate::schema::stored_under(app)
        .iter()
        .filter_map(|key| map_in(doc, &records, key))
        .collect()
}

/// The records map of `app` (in `records`, the room's `tl` map) that holds the resource `id`:
/// its own, or that of an id it was published as before.
fn stored_map(
    doc: &Doc,
    records: &automerge::ObjId,
    app: &str,
    id: &str,
) -> Option<automerge::ObjId> {
    crate::schema::stored_under(app)
        .iter()
        .filter_map(|key| map_in(doc, records, key))
        .find(|map| doc.get(map, id).ok().flatten().is_some())
}

/// Read the resource `id` of `app` from `doc`.
///
/// # Errors
/// The resource is absent, or its type is not registered.
pub fn read_resource(doc: &Doc, app: &str, id: &str) -> Result<DynRecord, ModelError> {
    match crate::reads::resource_obj(doc, app, id) {
        Some(obj) => DynRecord::read_whole(doc, &obj),
        None => Err(ModelError::Missing {
            path: id.to_owned(),
        }),
    }
}

/// The ids of every resource of `app` in `doc`, those under its earlier ids included, in key
/// order.
#[must_use]
pub fn resource_ids(doc: &Doc, app: &str) -> Vec<String> {
    let mut ids: Vec<String> = app_records(doc, app)
        .iter()
        .flat_map(|records| doc.keys(records).collect::<Vec<_>>())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod outbox_tests {
    use super::*;

    /// A record of `app` holding the resource `id` with an outbox of `entries` (key, payload).
    fn holding(app: &str, id: &str, entries: &[(&str, &str)]) -> Doc {
        let mut doc = Doc::new();
        let records = doc
            .put_object(&ROOT, RECORDS_KEY, ObjType::Map)
            .expect("records");
        let held = doc.put_object(&records, app, ObjType::Map).expect("app");
        let resource = doc.put_object(&held, id, ObjType::Map).expect("resource");
        let outbox = doc
            .put_object(&resource, OUTBOX_KEY, ObjType::Map)
            .expect("outbox");
        for (key, payload) in entries {
            let entry = doc.put_object(&outbox, *key, ObjType::Map).expect("entry");
            doc.put(&entry, "capability", "smtp.send")
                .expect("capability");
            doc.put(&entry, "payload", *payload).expect("payload");
            doc.put(&entry, "owner", "device").expect("owner");
        }
        doc
    }

    #[test]
    fn an_outbox_entry_that_does_not_read_stands_rejected_with_why() {
        let doc = holding("app", "r", &[("1", "[\"to\"]"), ("2", "{not json")]);
        let entries = outbox_entries(&doc, "app", "r");
        assert_eq!(entries.len(), 2, "no entry is left out");
        assert_eq!(entries[0].settled, None, "the readable one waits");
        assert_eq!(entries[0].payload, serde_json::json!(["to"]));
        let unreadable = &entries[1];
        assert_eq!(unreadable.settled.as_deref(), Some("rejected"));
        assert!(
            unreadable.reason.contains("payload"),
            "{}",
            unreadable.reason
        );
        let delivery = unreadable.delivery();
        assert_eq!(delivery.state, "rejected");
        assert!(!delivery.reason.is_empty());
    }
}
