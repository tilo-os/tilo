//! Runtime values: what handles refer to, what actions take and return, what flows journal.

use std::collections::BTreeMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value as Json};

use crate::ids::{ActionId, DocId, FieldId, ItemId, ModuleHash, Principal, TypeId};

/// A reference to a resource: a document, or an item of one, named by its id alone.
///
/// An item is identified by its id wherever it is: which collection holds it is a fact the store
/// keeps of the item (its placement), so a reference stays valid when the item moves to another
/// collection of its document. The document is where the item is stored; moves stay within it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceRef {
    pub doc: DocId,
    /// The item, `None` for the document itself.
    pub item: Option<ItemId>,
    pub ty: TypeId,
}

impl ResourceRef {
    /// Whether this names a package's per-room root (`root-<type id>`), which the host makes
    /// the first time it is opened; until then it reads as it would start.
    #[must_use]
    pub fn is_unmade_root(&self) -> bool {
        self.item.is_none() && self.doc.starts_with("root-")
    }

    pub fn root(doc: impl Into<DocId>, ty: impl Into<TypeId>) -> Self {
        Self {
            doc: doc.into(),
            item: None,
            ty: ty.into(),
        }
    }

    /// The item `item` of this resource's document.
    #[must_use]
    pub fn item(&self, item: &str, ty: impl Into<TypeId>) -> Self {
        Self {
            doc: self.doc.clone(),
            item: Some(item.to_owned()),
            ty: ty.into(),
        }
    }

    /// The reference as text: the document id, then `/item` for an item.
    pub fn id_string(&self) -> String {
        match &self.item {
            Some(item) => format!("{}/{item}", self.doc),
            None => self.doc.clone(),
        }
    }

    /// Parse the text produced by [`ResourceRef::id_string`]. The caller supplies the type
    /// and is responsible for checking it against the stored one.
    ///
    /// A reference written before items were stored by id names the path down to the item
    /// (`doc/field:item/field:item`); it reads as the item its last step names.
    pub fn parse_id(text: &str, ty: impl Into<TypeId>) -> Option<Self> {
        let (doc, item) = split_id(text)?;
        Some(Self {
            doc: doc.to_owned(),
            item: item.map(str::to_owned),
            ty: ty.into(),
        })
    }
}

/// The document and the item (if any) of a resource written as [`ResourceRef::id_string`], or
/// as the path a reference held before items were stored by id (its last step's item).
#[must_use]
pub fn split_id(text: &str) -> Option<(&str, Option<&str>)> {
    let Some((doc, rest)) = text.split_once('/') else {
        return (!text.is_empty()).then_some((text, None));
    };
    if doc.is_empty() || rest.split('/').any(str::is_empty) {
        return None;
    }
    let last = rest.rsplit('/').next().unwrap_or(rest);
    // A path's step is `field:item`; the field is never part of the item's id.
    let item = last.split_once(':').map_or(last, |(_, item)| item);
    (!item.is_empty()).then_some((doc, Some(item)))
}

/// A resource written as [`ResourceRef::id_string`], or as an older path to it, in the one form:
/// the document, then `/item`.
#[must_use]
pub fn flat_id(text: &str) -> String {
    match split_id(text) {
        Some((doc, Some(item))) => format!("{doc}/{item}"),
        Some((doc, None)) => doc.to_owned(),
        None => text.to_owned(),
    }
}

/// How a reference is written: its document, item and type. One written before items were
/// stored by id held the path down to the item (`path`); it reads as its last step's item.
#[derive(Serialize, Deserialize)]
struct RefForm {
    doc: DocId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item: Option<ItemId>,
    #[serde(default, skip_serializing)]
    path: Vec<PathStep>,
    ty: TypeId,
}

/// A step of a path an older reference held: only its item is read.
#[derive(Serialize, Deserialize)]
struct PathStep {
    item: ItemId,
}

impl Serialize for ResourceRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        RefForm {
            doc: self.doc.clone(),
            item: self.item.clone(),
            path: Vec::new(),
            ty: self.ty.clone(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for ResourceRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let form = RefForm::deserialize(d)?;
        let item = form
            .item
            .or_else(|| form.path.into_iter().last().map(|step| step.item));
        Ok(Self {
            doc: form.doc,
            item,
            ty: form.ty,
        })
    }
}

/// An action applied to arguments, with the module that wrote the application expression.
/// `author` is `None` for a call made straight by a principal, with no code in between.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionRef {
    pub action: ActionId,
    pub args: Vec<Value>,
    pub author: Option<ModuleHash>,
}

impl ActionRef {
    pub fn new(action: impl Into<ActionId>, args: Vec<Value>) -> Self {
        Self {
            action: action.into(),
            args,
            author: None,
        }
    }
}

impl From<(&str, Vec<Value>)> for ActionRef {
    fn from((action, args): (&str, Vec<Value>)) -> Self {
        Self::new(action, args)
    }
}

/// A record under construction, the init value of `coll_insert`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordInit {
    pub ty: TypeId,
    pub fields: BTreeMap<FieldId, Value>,
}

/// A value of an enum: the enum's type id, the variant's id and its payload fields by id.
/// Declared enums use their declaration ids; the built-in `Result` uses
/// `tilolang::RESULT_TYPE` and its variant and field ids.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnumValue {
    pub ty: TypeId,
    pub variant: String,
    pub fields: BTreeMap<FieldId, Value>,
}

/// A resource reference together with the module that created the handle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefValue {
    /// Shared: a reference is copied wherever a value goes (an item taken out of a list, an
    /// argument, a list built by a filter), and a view over a list copies tens of thousands.
    #[serde(with = "shared")]
    pub target: Rc<ResourceRef>,
    pub author: Option<ModuleHash>,
}

/// Serde for a shared reference: as the reference itself.
mod shared {
    use std::rc::Rc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::ResourceRef;

    pub fn serialize<S: Serializer>(target: &Rc<ResourceRef>, s: S) -> Result<S::Ok, S::Error> {
        target.as_ref().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Rc<ResourceRef>, D::Error> {
        ResourceRef::deserialize(d).map(Rc::new)
    }
}

/// Serde for a collection's filter: as the predicate itself.
mod shared_filter {
    use std::rc::Rc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::store::Pred;

    #[expect(
        clippy::ref_option,
        reason = "serde's `with` hands the field by reference"
    )]
    pub fn serialize<S: Serializer>(pred: &Option<Rc<Pred>>, s: S) -> Result<S::Ok, S::Error> {
        pred.as_deref().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Rc<Pred>>, D::Error> {
        Option::<Pred>::deserialize(d).map(|p| p.map(Rc::new))
    }
}

/// A collection field read in a view: the field, not its items. A view reads what it uses of it
/// (its length, an item, its first or last) through the store's listing, item by item, and its
/// boundary depends on exactly that; nothing builds a reference per item. Two compare equal when
/// they are the same field of the same resource with the same filter, whatever it holds: what a
/// view read of it is tracked as the view's own reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollRef {
    #[serde(with = "shared")]
    pub target: Rc<ResourceRef>,
    pub field: FieldId,
    /// The type of its items.
    pub item: TypeId,
    /// The module whose code read it: the author of the references to its items.
    pub author: Option<ModuleHash>,
    /// Only the items this holds of (`items.filter(x -> ...)`, answered by the store); all of
    /// them when `None`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "shared_filter"
    )]
    pub filter: Option<Rc<crate::store::Pred>>,
    /// The order its items are listed in (`query(T, in: x, sort: .due_oldest)`); the
    /// collection's own when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<crate::query::Order>,
}

impl CollRef {
    /// The same collection, only the items `pred` holds of (and the ones it held of before).
    #[must_use]
    pub fn filtered(&self, pred: crate::store::Pred) -> Self {
        let pred = match self.filter.as_deref() {
            Some(before) => crate::store::Pred::And(vec![before.clone(), pred]),
            None => pred,
        };
        Self {
            filter: Some(Rc::new(pred)),
            ..self.clone()
        }
    }

    /// The same collection, only the items `pred` holds of (and the ones it held of before),
    /// in the order `order` when there is one.
    #[must_use]
    pub fn queried(&self, pred: crate::store::Pred, order: Option<crate::query::Order>) -> Self {
        let mut out = self.filtered(pred);
        if order.is_some() {
            out.order = order;
        }
        out
    }

    /// Whether it lists other items than its collection's, or in another order: which items it
    /// holds, and where, depend on fields of each item.
    #[must_use]
    pub fn derived(&self) -> bool {
        self.filter.is_some() || self.order.is_some()
    }

    /// Whether a change to the item field `field` may change which items it holds or their
    /// order.
    #[must_use]
    pub fn reads_field(&self, field: &str) -> bool {
        field == crate::store::ANY_FIELD
            || self
                .filter
                .as_deref()
                .is_some_and(|pred| pred.fields().iter().any(|read| read.spec.id == field))
            || self.order.as_ref().is_some_and(|order| {
                // Any change of an item is its last version.
                order.field == field
                    || crate::store::Reflected::of(&order.field)
                        == Some(crate::store::Reflected::Updated)
            })
    }

    /// A reference to the item `id` of the collection.
    pub fn item_ref(&self, id: &str) -> Value {
        Value::Ref(RefValue {
            target: Rc::new(self.target.item(id, self.item.clone())),
            author: self.author.clone(),
        })
    }
}

/// A value of the language.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
    /// A `RichText` value: tilo-doc's rich text (see [`crate::rich`]).
    Rich(crate::rich::RichValue),
    List(Vec<Value>),
    /// A collection field read in a view, read item by item (see [`CollRef`]). Wherever its
    /// items are needed as values (stored, sorted, put in a list) it is read whole into a
    /// [`Value::List`] first; it never goes into another value.
    Coll(Rc<CollRef>),
    Tuple(Vec<Value>),
    Opt(Option<Box<Value>>),
    /// A value of an enum (declared, or the built-in `Result`).
    Enum(EnumValue),
    Ref(RefValue),
    /// Who did something. Only the host makes one (`invoker`, or read back from a field it
    /// wrote); the language has no way to build one from text.
    Principal(Principal),
    Action(ActionRef),
    /// A UI node under construction in the current view invocation (host-internal).
    Node(usize),
    /// A record under construction (host-internal).
    Rec(RecordInit),
    /// The error a denied or failed invocation returns to a flow.
    Error(String),
    /// The value an event of a control carries, before the event happens: an action reference
    /// holds its place (`open(x)` on `select(x)`), and the host puts the value there when the
    /// event fires (`ViewInstance::fire_with`).
    Event,
    /// One part of what an event carries when it also says where its value lands (a drop
    /// carries `[card, at]`): 0 the value, 1 the place.
    EventPart(usize),
    /// A `window` or `lens` slot an action reference takes as an argument (`send(c, draft)`):
    /// read when the event fires, so the action gets what the person typed up to the press,
    /// whatever the view last drew. `scalar` slots hold their value boxed.
    State {
        key: crate::view_state::StateKey,
        scalar: bool,
    },
    /// A map field read whole: its entries, each key once, in key order. Only a view reads
    /// one, to hand it to a component or another view.
    Map(Vec<(Value, Value)>),
    /// An action offered in a component's prop (`ui.MenuButton(actions: actions(r))`): how
    /// its control shows it, and the event of the node that runs it (host-internal).
    Offered(Rc<tilolang::toolkit::Offered>),
    /// An argument of an action reference computed from view state (`set(open, not open)`,
    /// `send(c, "{draft}!")`): the function `e_<id>` of `module` over `args`, run when the
    /// event fires, after the view state among `args` is read. A `scalar` result is boxed
    /// where a slot takes it.
    AtEvent {
        module: ModuleHash,
        id: String,
        args: Vec<Value>,
        scalar: bool,
    },
}

impl Value {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// A reference value with no authoring module, as handed in by the host.
    pub fn resource(target: &ResourceRef) -> Self {
        Self::Ref(RefValue {
            target: Rc::new(target.clone()),
            author: None,
        })
    }

    pub fn some(v: Self) -> Self {
        Self::Opt(Some(Box::new(v)))
    }

    pub fn none() -> Self {
        Self::Opt(None)
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// A short name of the variant, for trap messages.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Bool(_) => "bool",
            Self::Text(_) => "text",
            Self::Rich(_) => "rich text",
            Self::List(_) => "list",
            Self::Coll(_) => "collection",
            Self::Tuple(_) => "tuple",
            Self::Opt(_) => "option",
            Self::Enum(_) => "enum value",
            Self::Ref(_) => "resource reference",
            Self::Principal(_) => "principal",
            Self::Action(_) => "action reference",
            Self::Node(_) => "ui node",
            Self::Rec(_) => "record",
            Self::Error(_) => "error",
            Self::Event | Self::EventPart(_) => "event value",
            Self::State { .. } => "view state",
            Self::Map(_) => "map",
            Self::Offered(_) => "offered action",
            Self::AtEvent { .. } => "argument computed at the event",
        }
    }
}

/// Encode a value as JSON, for registers, journals and change messages.
///
/// # Errors
///
/// Fails for host-internal values and non-finite floats.
pub fn to_json(v: &Value) -> Result<Json, String> {
    Ok(match v {
        Value::Unit => Json::Null,
        Value::Int(i) => Json::from(*i),
        Value::Bool(b) => Json::Bool(*b),
        Value::Float(f) => Json::Number(
            Number::from_f64(*f).ok_or_else(|| "a non-finite float cannot be stored".to_owned())?,
        ),
        Value::Text(s) => Json::String(s.clone()),
        Value::Rich(r) => tagged(
            "rich",
            serde_json::to_value(&r.blocks).map_err(|e| e.to_string())?,
        ),
        Value::List(items) => tagged("list", items_to_json(items)?),
        Value::Tuple(items) => tagged("tuple", items_to_json(items)?),
        Value::Opt(Some(inner)) => tagged("some", to_json(inner)?),
        Value::Opt(None) => tagged("none", Json::Null),
        Value::Enum(e) => {
            let mut fields = Map::new();
            for (id, v) in &e.fields {
                fields.insert(id.clone(), to_json(v)?);
            }
            let mut m = Map::new();
            m.insert("enum".into(), Json::String(e.ty.clone()));
            m.insert("variant".into(), Json::String(e.variant.clone()));
            m.insert("fields".into(), Json::Object(fields));
            Json::Object(m)
        }
        Value::Ref(r) => {
            let mut m = Map::new();
            m.insert("ref".into(), Json::String(r.target.id_string()));
            m.insert("ty".into(), Json::String(r.target.ty.clone()));
            if let Some(a) = &r.author {
                m.insert("author".into(), Json::String(a.0.clone()));
            }
            Json::Object(m)
        }
        Value::Action(a) => {
            let mut m = Map::new();
            m.insert("action".into(), Json::String(a.action.clone()));
            m.insert("args".into(), items_to_json(&a.args)?);
            if let Some(author) = &a.author {
                m.insert("author".into(), Json::String(author.0.clone()));
            }
            Json::Object(m)
        }
        Value::Principal(p) => tagged("principal", Json::String(p.0.clone())),
        Value::Error(msg) => tagged("error", Json::String(msg.clone())),
        Value::Node(_)
        | Value::Rec(_)
        | Value::Coll(_)
        | Value::Event
        | Value::EventPart(_)
        | Value::State { .. }
        | Value::AtEvent { .. }
        | Value::Map(_)
        | Value::Offered(_) => {
            return Err(format!("a {} cannot be stored", v.kind_name()));
        }
    })
}

fn tagged(tag: &str, inner: Json) -> Json {
    let mut m = Map::new();
    m.insert(tag.to_owned(), inner);
    Json::Object(m)
}

fn items_to_json(items: &[Value]) -> Result<Json, String> {
    items
        .iter()
        .map(to_json)
        .collect::<Result<Vec<_>, _>>()
        .map(Json::Array)
}

/// Decode what [`to_json`] produced.
///
/// # Errors
///
/// Fails on JSON that is not an encoded value.
pub fn from_json(j: &Json) -> Result<Value, String> {
    Ok(match j {
        Json::Null => Value::Unit,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) if !n.is_f64() => Value::Int(i),
            _ => Value::Float(n.as_f64().ok_or("number out of range")?),
        },
        Json::String(s) => Value::Text(s.clone()),
        Json::Array(_) => return Err("a bare array is not an encoded value".into()),
        Json::Object(m) => from_json_object(m)?,
    })
}

fn from_json_items(j: &Json) -> Result<Vec<Value>, String> {
    j.as_array()
        .ok_or("expected an array")?
        .iter()
        .map(from_json)
        .collect()
}

fn author_of(m: &Map<String, Json>) -> Option<ModuleHash> {
    m.get("author")
        .and_then(Json::as_str)
        .map(|s| ModuleHash(s.to_owned()))
}

fn from_json_object(m: &Map<String, Json>) -> Result<Value, String> {
    if let Some(items) = m.get("list") {
        return Ok(Value::List(from_json_items(items)?));
    }
    if let Some(blocks) = m.get("rich") {
        let blocks = serde_json::from_value(blocks.clone()).map_err(|e| e.to_string())?;
        return Ok(Value::Rich(crate::rich::RichValue { blocks }));
    }
    if let Some(items) = m.get("tuple") {
        return Ok(Value::Tuple(from_json_items(items)?));
    }
    if let Some(inner) = m.get("some") {
        return Ok(Value::some(from_json(inner)?));
    }
    if m.contains_key("none") {
        return Ok(Value::none());
    }
    if let (Some(Json::String(ty)), Some(Json::String(variant))) = (m.get("enum"), m.get("variant"))
    {
        let mut fields = BTreeMap::new();
        if let Some(Json::Object(f)) = m.get("fields") {
            for (id, v) in f {
                fields.insert(id.clone(), from_json(v)?);
            }
        }
        return Ok(Value::Enum(EnumValue {
            ty: ty.clone(),
            variant: variant.clone(),
            fields,
        }));
    }
    if let Some(Json::String(id)) = m.get("principal") {
        return Ok(Value::Principal(Principal(id.clone())));
    }
    if let Some(Json::String(msg)) = m.get("error") {
        return Ok(Value::Error(msg.clone()));
    }
    if let (Some(Json::String(id)), Some(Json::String(ty))) = (m.get("ref"), m.get("ty")) {
        let target = ResourceRef::parse_id(id, ty.clone()).ok_or("malformed reference")?;
        return Ok(Value::Ref(RefValue {
            target: Rc::new(target),
            author: author_of(m),
        }));
    }
    if let (Some(Json::String(action)), Some(args)) = (m.get("action"), m.get("args")) {
        return Ok(Value::Action(ActionRef {
            action: action.clone(),
            args: from_json_items(args)?,
            author: author_of(m),
        }));
    }
    Err("an object that is not an encoded value".into())
}

/// Whether two values are equal as the language compares them: by value, payloads included,
/// and references by their target alone (not by the module that made the handle).
pub fn same_value(lhs: &Value, rhs: &Value) -> bool {
    let all = |left: &[Value], right: &[Value]| {
        left.len() == right.len()
            && left
                .iter()
                .zip(right)
                .all(|(one, other)| same_value(one, other))
    };
    match (lhs, rhs) {
        (Value::Ref(left), Value::Ref(right)) => left.target == right.target,
        (Value::List(left), Value::List(right)) | (Value::Tuple(left), Value::Tuple(right)) => {
            all(left, right)
        }
        (Value::Opt(left), Value::Opt(right)) => match (left, right) {
            (Some(one), Some(other)) => same_value(one, other),
            (None, None) => true,
            _ => false,
        },
        (Value::Enum(left), Value::Enum(right)) => {
            left.ty == right.ty
                && left.variant == right.variant
                && left.fields.len() == right.fields.len()
                && left.fields.iter().all(|(id, value)| {
                    right
                        .fields
                        .get(id)
                        .is_some_and(|other| same_value(value, other))
                })
        }
        (Value::Int(int), Value::Bool(flag)) | (Value::Bool(flag), Value::Int(int)) => {
            *int == i64::from(*flag)
        }
        (Value::Action(left), Value::Action(right)) => {
            left.action == right.action && all(&left.args, &right.args)
        }
        _ => lhs == rhs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_through_json() {
        let target = ResourceRef::root("d1", "T").item("i-1", "U");
        let v = Value::List(vec![
            Value::Int(3),
            Value::Float(2.0),
            Value::Tuple(vec![Value::text("x"), Value::none()]),
            Value::some(Value::Bool(true)),
            Value::Ref(RefValue {
                target: Rc::new(target),
                author: Some(ModuleHash("m".into())),
            }),
            Value::Action(ActionRef::new("a", vec![Value::Int(1)])),
            Value::Enum(EnumValue {
                ty: "e1".into(),
                variant: "v1".into(),
                fields: BTreeMap::from([("f1".to_owned(), Value::text("payload"))]),
            }),
        ]);
        let json = to_json(&v).unwrap();
        let text = serde_json::to_string(&json).unwrap();
        let back = from_json(&serde_json::from_str(&text).unwrap()).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn reference_ids_parse_back() {
        let r = ResourceRef::root("d1", "T").item("i-1", "U");
        assert_eq!(r.id_string(), "d1/i-1");
        assert_eq!(ResourceRef::parse_id(&r.id_string(), "U"), Some(r));
    }

    #[test]
    fn a_path_written_before_items_were_flat_reads_as_its_last_item() {
        let r = ResourceRef::root("d1", "T").item("i-2", "U");
        assert_eq!(
            ResourceRef::parse_id("d1/f:i-1/g:i-2", "U"),
            Some(r.clone())
        );
        assert_eq!(flat_id("d1/f:i-1/g:i-2"), "d1/i-2");
        assert_eq!(flat_id("d1"), "d1");
        assert_eq!(ResourceRef::parse_id("/f:i", "U"), None);
        let old = serde_json::json!({
            "doc": "d1",
            "path": [{"field": "f", "item": "i-1"}, {"field": "g", "item": "i-2"}],
            "ty": "U",
        });
        assert_eq!(serde_json::from_value::<ResourceRef>(old).unwrap(), r);
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(serde_json::from_value::<ResourceRef>(json).unwrap(), r);
    }
}
