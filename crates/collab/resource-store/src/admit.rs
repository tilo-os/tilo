//! Admitting a peer's change: what it touches, whether the declared types allow it, and whether
//! its author may make it.
//!
//! Every change names the action that made it ([`Stamp`]): the verb, who invoked it, the
//! invocation, and the membership its author saw. A holder reads, before applying a change,
//! which resources, fields and placements it touches ([`Touched`]), from its operations against
//! the document as it stands, without applying it. The store checks what the declared types can
//! say (a field the type declares, a value of its type, a case of its enum, a kind nested where
//! its parent's type nests it, and what the verb declares it writes); the caller's [`Admit`]
//! decides whether the author may invoke the verb on those resources in that reader set,
//! against the membership the stamp names. Every holder reaches the same
//! answer from the same change, so a change that is refused is kept beside the document but has
//! no effect on it, on any holder; a change built on a refused one is refused too.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use automerge::legacy::{Key, OpType};
use automerge::{Automerge, Change as AmChange, ObjId as ExId, Prop, ReadDoc, ScalarValue};

use crate::schema::{Expected, Schema};

pub use collab_types::stamp::{Group, Origin, Stamp};

/// What one change does to one resource.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is one thing a change may do to a resource, and any of them at once"
)]
pub struct Touch {
    /// Its kind, as the change sets it or the document holds it.
    pub kind: Option<String>,
    /// It is made by the change.
    pub made: bool,
    /// It is removed by the change.
    pub removed: bool,
    /// Its parent or its place among its parent's resources changes.
    pub placed: bool,
    /// The parent the change nests it in, when it sets one.
    pub parent: Option<String>,
    /// Its fields the change writes.
    pub fields: BTreeSet<String>,
    /// Its content (rich text, lists, maps) changes.
    pub content: bool,
}

/// What one change touches in one document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Touched {
    /// Each resource touched, by id.
    pub resources: BTreeMap<String, Touch>,
    /// The reader set it writes into the document: only ever the one the document has.
    pub readers: Option<String>,
}

/// A peer's change as the store asks about it.
#[derive(Debug)]
pub struct Proposal<'a> {
    pub doc: &'a str,
    pub readers: &'a str,
    /// The device that signed the change.
    pub author: &'a [u8; 32],
    /// The action it names; `None` for a change that names none.
    pub stamp: Option<&'a Stamp>,
    pub touched: &'a Touched,
}

/// The one permission check, as a holder asks it of each change it receives: whether the
/// change's author may invoke its verb on the resources it touches in the document's reader set,
/// as of the membership its stamp names. It must answer alike on every holder. That what the
/// change touches lies within what its verb declares it writes the store checks itself, from
/// the declared schema ([`crate::Schema::verbs`]).
pub trait Admit {
    fn admit(&self, proposal: &Proposal<'_>) -> bool;
}

impl<F: Fn(&Proposal<'_>) -> bool> Admit for F {
    fn admit(&self, proposal: &Proposal<'_>) -> bool {
        self(proposal)
    }
}

/// A change received from a peer, with the device that signed it.
#[derive(Clone, Debug)]
pub struct Received {
    pub change: AmChange,
    pub author: [u8; 32],
}

/// Why a change was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It is built on a change that was refused.
    OnRefused,
    /// It writes where the store writes nothing, or into an object it cannot place.
    Unplaced,
    /// The declared types do not allow it.
    Undeclared(String),
    /// Its author may not make it.
    NotPermitted,
}

/// Where each object of `doc` and of the change being read lies, as the keys leading to it.
struct Paths<'a> {
    doc: &'a Automerge,
    made: HashMap<String, Vec<String>>,
}

impl Paths<'_> {
    fn of(&self, object: &str) -> Option<Vec<String>> {
        if object == "_root" {
            return Some(Vec::new());
        }
        if let Some(path) = self.made.get(object) {
            return Some(path.clone());
        }
        let id: ExId = self.doc.import_obj(object).ok()?;
        let parents = self.doc.parents(&id).ok()?;
        let mut path: Vec<String> = parents
            .map(|parent| match parent.prop {
                Prop::Map(key) => key,
                Prop::Seq(_) => SEQ.to_owned(),
            })
            .collect();
        path.reverse();
        Some(path)
    }
}

/// A position in a list or text, in a path.
const SEQ: &str = "#";

/// One write the change makes to a field: the field and the value, `None` for a removal or a
/// value no scalar holds.
type FieldWrite = (String, String, Option<ScalarValue>);

/// The resources `change` touches in the store document `doc_id`, `doc` with it applied: what a
/// reader of the store's changes (a trigger watching a kind) sees each change do. `None` when it
/// writes where the store writes nothing.
#[must_use]
pub fn touches(doc: &Automerge, doc_id: &str, change: &AmChange) -> Option<Touched> {
    touched(doc, doc_id, change).map(|(touched, _)| touched)
}

/// What `change` touches in `doc`, the document it is applied to, and each scalar it writes to a
/// field; `None` when it writes where the store writes nothing.
pub(crate) fn touched(
    doc: &Automerge,
    doc_id: &str,
    change: &AmChange,
) -> Option<(Touched, Vec<FieldWrite>)> {
    let expanded = change.decode();
    let actor = expanded.actor_id.to_hex_string();
    let start = expanded.start_op.get();
    let mut paths = Paths {
        doc,
        made: HashMap::new(),
    };
    let content_of = content_resource(doc_id);
    let mut touched = Touched::default();
    let mut writes = Vec::new();
    for (at, op) in (0_u64..).zip(&expanded.operations) {
        let mut path = paths.of(&op.obj.to_string())?;
        path.push(match &op.key {
            Key::Map(key) => key.to_string(),
            Key::Seq(_) => SEQ.to_owned(),
        });
        if let OpType::Make(_) = op.action {
            paths.made.insert(
                format!("{}@{actor}", start.saturating_add(at)),
                path.clone(),
            );
        }
        // A resource is the key `r:<id>` of the root; read as `r`, `<id>`.
        let mut keys: Vec<&str> = Vec::with_capacity(path.len().saturating_add(1));
        for (at, key) in path.iter().enumerate() {
            match key.strip_prefix("r:") {
                Some(id) if at == 0 => keys.extend(["r", id]),
                _ => keys.push(key),
            }
        }
        match (content_of.as_ref(), keys.as_slice()) {
            (Some((kind, id)), _) => {
                let touch = touched.resources.entry(id.clone()).or_default();
                touch.kind = Some(kind.clone());
                touch.content = true;
            }
            (None, ["w"]) => match &op.action {
                OpType::Put(ScalarValue::Str(readers)) => {
                    touched.readers = Some(readers.to_string());
                }
                _ => return None,
            },
            (None, ["r", id]) => {
                let touch = touched.resources.entry((*id).to_owned()).or_default();
                match op.action {
                    OpType::Make(_) => touch.made = true,
                    OpType::Delete => touch.removed = true,
                    _ => return None,
                }
            }
            (None, ["r", id, "k"]) => {
                let touch = touched.resources.entry((*id).to_owned()).or_default();
                match &op.action {
                    OpType::Put(ScalarValue::Str(kind)) => touch.kind = Some(kind.to_string()),
                    _ => return None,
                }
            }
            (None, ["r", id, place @ ("p" | "o")]) => {
                let touch = touched.resources.entry((*id).to_owned()).or_default();
                touch.placed = true;
                if *place == "p"
                    && let OpType::Put(ScalarValue::Str(parent)) = &op.action
                {
                    touch.parent = Some(parent.to_string());
                }
            }
            (None, ["r", _, "f"]) => {}
            (None, ["r", id, "f", field, rest @ ..]) => {
                touched
                    .resources
                    .entry((*id).to_owned())
                    .or_default()
                    .fields
                    .insert((*field).to_owned());
                let value = match (&op.action, rest) {
                    (OpType::Put(value), []) => Some(value.clone()),
                    // A set's member, kept as a key of its map.
                    (OpType::Put(_) | OpType::Delete, [_member]) => None,
                    // A removal, or a set made as the map of its members.
                    (OpType::Delete | OpType::Make(_), []) => None,
                    _ => return None,
                };
                writes.push(((*id).to_owned(), (*field).to_owned(), value));
            }
            _ => return None,
        }
    }
    // The kind of each resource the change does not name its kind of is the one it has.
    if content_of.is_none() {
        for (id, touch) in &mut touched.resources {
            if touch.kind.is_none()
                && let Ok(Some((automerge::Value::Object(_), resource))) =
                    doc.get(automerge::ROOT, format!("r:{id}"))
                && let Ok(Some((automerge::Value::Scalar(kind), _))) = doc.get(&resource, "k")
                && let ScalarValue::Str(kind) = kind.as_ref()
            {
                touch.kind = Some(kind.to_string());
            }
        }
    }
    Some((touched, writes))
}

/// The resource whose content `doc_id` holds, by kind and id; `None` for a document of resources.
pub(crate) fn content_resource(doc_id: &str) -> Option<(String, String)> {
    let (placed, number) = doc_id.rsplit_once('~')?;
    number.parse::<u32>().ok()?;
    let mut parts = placed.splitn(3, '/');
    let _readers = parts.next()?;
    let kind = parts.next()?;
    let id = parts.next()?;
    Some((kind.to_owned(), id.to_owned()))
}

/// Whether the declared types allow what a change touches and writes: every resource of a
/// declared kind, every field written one its kind declares, holding a value of its type.
pub(crate) fn declared(
    schema: &Schema,
    touched: &Touched,
    writes: &[FieldWrite],
    kind_of: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    if schema.types.is_empty() {
        return Ok(());
    }
    // A resource is nested only where its parent's type nests its kind.
    for touch in touched.resources.values() {
        if let (Some(kind), Some(parent)) = (&touch.kind, &touch.parent) {
            let parent_kind = touched
                .resources
                .get(parent)
                .and_then(|touch| touch.kind.clone())
                .or_else(|| kind_of(parent));
            if let Some(parent_kind) = parent_kind
                && schema
                    .get(&parent_kind)
                    .is_some_and(|ty| !ty.nested.iter().any(|(_, nested)| nested == kind))
            {
                return Err(format!("`{parent_kind}` nests no `{kind}`"));
            }
        }
    }
    for (id, touch) in &touched.resources {
        if touch.removed {
            continue;
        }
        let Some(kind) = &touch.kind else {
            return Err(format!("`{id}` is of no kind"));
        };
        if schema.get(kind).is_none() {
            return Err(format!("no type `{kind}` is declared"));
        }
    }
    for (id, field, value) in writes {
        // The platform's own fields, which no declared type has: what a resource stands in, and
        // the times and fields a version reflects (`$created`, `$updated`, `$changed.<field>`).
        if field.starts_with('$') {
            continue;
        }
        let Some(kind) = touched
            .resources
            .get(id)
            .and_then(|touch| touch.kind.as_ref())
        else {
            continue;
        };
        let Some(declared) = schema.get(kind).and_then(|ty| ty.field(field)) else {
            return Err(format!("`{kind}` declares no field `{field}`"));
        };
        if let Some(value) = value
            && !declared.value.holds(value, declared.optional)
        {
            return Err(format!("`{kind}.{field}` holds no {value:?}"));
        }
    }
    Ok(())
}

/// Whether what `touched` writes lies within what the verb `stamp` names declares it writes. A
/// schema that declares no verbs checks none; one that does refuses a change that names no verb.
pub(crate) fn within_verb(
    schema: &Schema,
    touched: &Touched,
    stamp: Option<&Stamp>,
) -> Result<(), String> {
    if schema.verbs.is_empty() {
        return Ok(());
    }
    let Some(stamp) = stamp else {
        return Err("the change names no verb".to_owned());
    };
    let Some(verb) = schema.verb(&stamp.verb) else {
        return Err(format!("no verb `{}` is declared", stamp.verb));
    };
    for (id, touch) in &touched.resources {
        let Some(kind) = &touch.kind else {
            continue;
        };
        let Some(writes) = verb.writes.get(kind) else {
            // A record's verb writes the kinds its type nests that another surface declares, and
            // their fields are the ones that surface declares: a tilolang package publishes a
            // surface per type, so a list's items are another surface's kind.
            if nested_kind(schema, &verb.nested, kind) {
                continue;
            }
            // A tilolang action's code writes the records of its own package it reaches, as an
            // answer settles the ask it answers: the package's code bounds what its verb writes
            // there, and a surface lists no write a reference leads to. Another package's
            // records stay out of its reach.
            if let Some(package) = tilolang_package(kind)
                && verb
                    .writes
                    .keys()
                    .any(|own| tilolang_package(own) == Some(package))
            {
                continue;
            }
            return Err(format!("`{}` writes no `{kind}`", verb.name));
        };
        if writes.sets_only && (touch.made || touch.removed || touch.placed || touch.content) {
            return Err(format!("`{}` only sets fields of `{id}`", verb.name));
        }
        if let Some(field) = touch
            .fields
            .iter()
            .filter(|field| !field.starts_with('$'))
            .find(|field| !writes.fields.contains(*field))
        {
            return Err(format!("`{}` writes no `{kind}.{field}`", verb.name));
        }
    }
    Ok(())
}

/// Whether `kind` stands among `nested` or among the kinds the types between them nest.
/// The tilolang package a kind of one (`tl.<app>.<Type>`) is of: none for another kind.
fn tilolang_package(kind: &str) -> Option<&str> {
    let (package, _) = kind.strip_prefix("tl.")?.rsplit_once('.')?;
    Some(package)
}

fn nested_kind(schema: &Schema, nested: &BTreeSet<String>, kind: &str) -> bool {
    let mut reached: Vec<&str> = nested.iter().map(String::as_str).collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    while let Some(at) = reached.pop() {
        if at == kind {
            return true;
        }
        if !seen.insert(at) {
            continue;
        }
        if let Some(ty) = schema.get(at) {
            reached.extend(ty.nested.iter().map(|(_, nested)| nested.as_str()));
        }
    }
    false
}

impl Expected {
    /// Whether `value` is one this field may hold.
    fn holds(&self, value: &ScalarValue, optional: bool) -> bool {
        match (self, value) {
            (_, ScalarValue::Null) => optional,
            (Self::Any, _)
            | (Self::Text, ScalarValue::Str(_))
            | (Self::Int, ScalarValue::Int(_) | ScalarValue::Uint(_))
            | (Self::Float, ScalarValue::F64(_) | ScalarValue::Int(_))
            | (Self::Bool, ScalarValue::Boolean(_)) => true,
            (Self::OneOf(cases), ScalarValue::Str(case)) => {
                cases.iter().any(|held| held.as_str() == case.as_str())
            }
            _ => false,
        }
    }
}
