//! Admitting a peer's change: what it touches, whether the declared types allow it, and whether
//! its author may make it.
//!
//! Every change names the action that made it ([`Stamp`]): the verb, who invoked it, the
//! invocation, and the membership its author saw. A holder reads, before applying a change,
//! which resources, fields and placements it touches ([`Touched`]), from its operations against
//! the document as it stands, without applying it. The store checks what the declared types can
//! say (a field the type declares, a value of its type, a case of its enum, a kind nested where
//! its parent's type nests it); the caller's [`Admit`] decides whether the author may invoke the
//! verb on those resources in that reader set, and whether what the change touches lies within
//! what the verb writes, against the membership the stamp names. Every holder reaches the same
//! answer from the same change, so a change that is refused is kept beside the document but has
//! no effect on it, on any holder; a change built on a refused one is refused too.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use automerge::legacy::{Key, OpType};
use automerge::{Automerge, Change as AmChange, ObjId as ExId, Prop, ReadDoc, ScalarValue};
use serde::{Deserialize, Serialize};

use crate::schema::{Expected, Schema};

/// The action a change was made by, and the membership its author saw: what a holder judges
/// the change against. It travels in the change itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// The verb invoked.
    pub verb: String,
    /// Who invoked it: a person, or an agent acting for one.
    pub invoker: String,
    /// The invocation: every change it made, in every document, names it, so history and undo
    /// take them as one step.
    pub invocation: String,
    /// The heads of the membership the author saw when it wrote the change, as the membership
    /// names them: who may do what is judged as of there, not as of the receiver's now.
    pub membership: Vec<String>,
}

impl Stamp {
    pub(crate) fn message(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The stamp `change` carries; `None` for a change that carries none.
    #[must_use]
    pub fn of(change: &AmChange) -> Option<Self> {
        serde_json::from_str(change.message()?).ok()
    }
}

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
/// as of the membership its stamp names, and whether what it touches lies within what the verb
/// declares it writes. It must answer alike on every holder.
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
