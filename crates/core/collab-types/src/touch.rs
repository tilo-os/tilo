//! What one change does to the resources of a store document, read from its operations against
//! the document as it stands, without applying it ([`Touched`]): which resources it makes,
//! removes, places and writes the fields or content of, and the reader set it writes.
//!
//! A store document keeps each resource as the key `r:<id>` of its root, holding its kind (`k`),
//! parent (`p`), place (`o`) and fields (`f`); the root's [`READERS`] names who reads it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use automerge::legacy::{Key, OpType};
use automerge::{Automerge, Change as AmChange, ObjId as ExId, Prop, ReadDoc, ScalarValue};

/// The key at a document's root that names who reads it: fixed when the document is placed.
pub const READERS: &str = "w";

/// What one change does to one resource.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is one thing a change may do to a resource, and any of them at once"
)]
pub struct Touch {
    /// Its kind, as the change sets it or the document holds it.
    pub kind: Option<String>,
    /// The kind it had before the change gave it another, as its earlier changes left it.
    pub retyped_from: Option<String>,
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
pub fn touched(
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
    if content_of.is_none() {
        held_kinds(doc, change.deps(), &mut touched);
    }
    Some((touched, writes))
}

/// The kind of each resource of `touched` the change does not name its kind of is the one it has
/// in `doc`; one it names another kind of than its earlier changes, `deps`, left it was retyped.
fn held_kinds(doc: &Automerge, deps: &[automerge::ChangeHash], touched: &mut Touched) {
    for (id, touch) in &mut touched.resources {
        if !touch.made {
            touch.retyped_from = kind_at(doc, id, deps)
                .filter(|was| touch.kind.as_ref().is_some_and(|kind| kind != was));
        }
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

/// The kind the resource `id` has in `doc` as it stood at `heads`.
fn kind_at(doc: &Automerge, id: &str, heads: &[automerge::ChangeHash]) -> Option<String> {
    let (_, resource) = doc
        .get_at(automerge::ROOT, format!("r:{id}"), heads)
        .ok()??;
    match doc.get_at(&resource, "k", heads).ok()?? {
        (automerge::Value::Scalar(kind), _) => kind.as_str().map(str::to_owned),
        _ => None,
    }
}

/// The resource whose content `doc_id` holds, by kind and id; `None` for a document of resources.
pub fn content_resource(doc_id: &str) -> Option<(String, String)> {
    let (placed, number) = doc_id.rsplit_once('~')?;
    number.parse::<u32>().ok()?;
    let mut parts = placed.splitn(3, '/');
    let _readers = parts.next()?;
    let kind = parts.next()?;
    let id = parts.next()?;
    Some((kind.to_owned(), id.to_owned()))
}
